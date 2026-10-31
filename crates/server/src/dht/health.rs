//! Router health probe (D66, D109, docs/research/router-bench.md): the home router, not the
//! laptop, is what breaks first under crawling (its NAT/CPU), and it shows as rising latency and
//! loss on **every** flow. Public ICMP is blocked on the user's line and the gateway rate-limits
//! ICMP, so the probe is a plain TCP connect (SYN → SYN-ACK, then closed) to public anycast
//! targets (`MAGNETAR_DHT_PROBE`, default `1.1.1.1:443,9.9.9.9:443`), all of them every 2 s;
//! a sample is the fastest answer, a failure only when every target failed.
//!
//! Decision ([`Health::push`], pure): baseline = 10th percentile of the successful samples of
//! the last 10 min taken while no brake was on (a saturated router must not raise its own
//! baseline); the router is **saturated** when, over the last 6 samples (12 s), at least 3
//! failed or their median is above `max(3 × baseline, baseline + 40 ms)`. Failures count from
//! the third sample: a router that fills its NAT table in the first 20 s is caught then, not
//! after a 30 s warm-up (D109). The benchmark's saturations were 122 ms against a 17 ms
//! baseline (KRPC 400 q/s) and 100 % loss (400 TCP attempts/s); its safe rates stayed at
//! 17–19 ms.
//!
//! Reaction ([`Escalation`], D109): each saturation raises the brake level by one, at most once
//! per 12 s and up to 3, and every DHT sender (the shared pacer: crawler and seed-check walks)
//! and the metadata fetcher's TCP attempts run at 1/2^level of their rate. After 5 min without
//! a saturation the level drops by one, then by one more per calm minute. A fixed halving could
//! not hold a router whose knee is below half the configured rate (2026-10-30: the LAN went
//! down at 75 q/s plus unpaced walks).

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use magnetar_core::dht::Dht;
use magnetar_core::dht::limiter::{MAX_BRAKE_LEVEL, Pacer};
use magnetar_core::metainfo::Fetcher;
use serde::Serialize;
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::daemons::stop_requested;

pub const PROBE_EVERY: Duration = Duration::from_secs(2);
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
/// Calm time before the first step down of the brake level.
pub const BRAKE_FOR: Duration = Duration::from_secs(300);
/// Calm time between further steps down.
pub const RELAX_STEP: Duration = Duration::from_secs(60);
pub const DEFAULT_TARGETS: &str = "1.1.1.1:443,9.9.9.9:443";
/// Samples kept for the baseline (10 min at one per 2 s).
const LONG: usize = 300;
/// Samples judged (12 s).
const SHORT: usize = 6;
const MIN_FAILED: usize = 3;
/// Calm successful samples needed before a baseline is trusted.
const MIN_BASELINE: usize = 12;
/// A raised level gets this long to show its effect before the next raise (one judged window).
const RAISE_EVERY: Duration = Duration::from_secs(PROBE_EVERY.as_secs() * SHORT as u64);

/// One probe result: connect time, None = failed or timed out.
pub type Sample = Option<Duration>;

#[derive(Debug, Default)]
pub struct Health {
    /// The last [`SHORT`] samples, judged.
    recent: VecDeque<Sample>,
    /// The last [`LONG`] successful samples taken while no brake was on: the baseline. Kept
    /// apart so a long braked stretch cannot evict them (D109 review).
    calm: VecDeque<Duration>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct HealthStats {
    pub probes: u64,
    pub failed: u64,
    pub baseline_ms: Option<f64>,
    pub last_median_ms: Option<f64>,
    /// Times the probe found the router saturated (and raised or held the brake).
    pub brakes: u64,
    /// Brake level now: rates at 1/2^level (D109).
    pub level: u8,
}

fn median(mut v: Vec<Duration>) -> Option<Duration> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    Some(v[v.len() / 2])
}

impl Health {
    pub fn baseline(&self) -> Option<Duration> {
        if self.calm.len() < MIN_BASELINE {
            return None;
        }
        let mut ok: Vec<Duration> = self.calm.iter().copied().collect();
        ok.sort_unstable();
        Some(ok[ok.len() / 10])
    }

    pub fn recent_median(&self) -> Option<Duration> {
        median(self.recent.iter().flatten().copied().collect())
    }

    /// Record a sample (`calm`: no brake was on while it was taken); true when the router looks
    /// saturated.
    pub fn push(&mut self, s: Sample, calm: bool) -> bool {
        if self.recent.len() == SHORT {
            self.recent.pop_front();
        }
        self.recent.push_back(s);
        if let (Some(d), true) = (s, calm) {
            if self.calm.len() == LONG {
                self.calm.pop_front();
            }
            self.calm.push_back(d);
        }
        let failed = self.recent.iter().filter(|s| s.is_none()).count();
        if failed >= MIN_FAILED {
            return true;
        }
        if self.recent.len() < SHORT {
            return false;
        }
        let Some(base) = self.baseline() else {
            // No baseline yet: only a run of failures counts.
            return false;
        };
        let limit = (base * 3).max(base + Duration::from_millis(40));
        self.recent_median().is_some_and(|m| m > limit)
    }
}

/// The brake level over time (pure): raised by one per saturation, at most once per
/// [`RAISE_EVERY`], lowered by one after [`BRAKE_FOR`] without a saturation and then once per
/// [`RELAX_STEP`].
#[derive(Debug, Default)]
pub struct Escalation {
    level: u8,
    last_raise: Option<Instant>,
    /// Last saturation, or the last step down (the next step waits from there).
    last_event: Option<Instant>,
    /// Whether the last event was a step down (then [`RELAX_STEP`] applies, not [`BRAKE_FOR`]).
    relaxing: bool,
}

impl Escalation {
    pub fn level(&self) -> u8 {
        self.level
    }

    /// Feed one probe verdict; returns the new level when it changed.
    pub fn update(&mut self, saturated: bool, now: Instant) -> Option<u8> {
        if saturated {
            self.last_event = Some(now);
            self.relaxing = false;
            let may_raise = self
                .last_raise
                .is_none_or(|t| now.saturating_duration_since(t) >= RAISE_EVERY);
            if may_raise && self.level < MAX_BRAKE_LEVEL {
                self.level += 1;
                self.last_raise = Some(now);
                return Some(self.level);
            }
            return None;
        }
        let wait = if self.relaxing { RELAX_STEP } else { BRAKE_FOR };
        if self.level > 0
            && self
                .last_event
                .is_some_and(|t| now.saturating_duration_since(t) >= wait)
        {
            self.level -= 1;
            self.last_event = Some(now);
            self.relaxing = true;
            return Some(self.level);
        }
        None
    }
}

/// Parse `MAGNETAR_DHT_PROBE`: `off` (or empty) disables the probe.
pub fn parse_targets(s: &str) -> Vec<SocketAddr> {
    if s.trim().eq_ignore_ascii_case("off") {
        return Vec::new();
    }
    s.split(',').filter_map(|t| t.trim().parse().ok()).collect()
}

async fn probe_one(target: SocketAddr) -> Sample {
    let t0 = Instant::now();
    match tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect(target)).await {
        Ok(Ok(_)) => Some(t0.elapsed()),
        _ => None,
    }
}

/// One sample from every target at once: the fastest answer, None only when all failed. One
/// dead target (or a slow one) then reads as neither loss nor latency (D109 review).
async fn probe(targets: &[SocketAddr]) -> Sample {
    futures::future::join_all(targets.iter().map(|t| probe_one(*t)))
        .await
        .into_iter()
        .flatten()
        .min()
}

/// What the probe slows down, process-wide (D109): the shared DHT pacer (crawler queries and
/// seed-check walks) for as long as the process runs, and the crawler's engine (its own brake
/// state and stats) and metadata fetcher while a crawler is attached. The probe outlives
/// crawler restarts, so walks stay braked while the crawler starts or backs off.
#[derive(Default)]
pub struct HealthHub {
    pacer: Option<Arc<Pacer>>,
    attached: std::sync::Mutex<Option<(Dht, Fetcher)>>,
    level: std::sync::atomic::AtomicU8,
    pub stats: Arc<std::sync::Mutex<HealthStats>>,
}

impl HealthHub {
    pub fn new(pacer: Option<Arc<Pacer>>) -> Arc<Self> {
        Arc::new(Self {
            pacer,
            ..Default::default()
        })
    }

    pub fn level(&self) -> u8 {
        self.level.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn set_level(&self, level: u8) {
        self.level
            .store(level, std::sync::atomic::Ordering::Relaxed);
        if let Some(p) = &self.pacer {
            p.set_level(level);
        }
        if let Some((d, f)) = &*self.attached.lock().unwrap() {
            d.set_brake_level(level);
            f.set_brake_level(level);
        }
    }

    fn brake_until(&self, until: Instant) {
        if let Some((d, f)) = &*self.attached.lock().unwrap() {
            d.brake_until(until);
            f.brake_until(until);
        }
    }

    /// Put a crawler's engine and fetcher under the probe (at the current level) until the
    /// returned guard is dropped.
    pub fn attach(self: &Arc<Self>, dht: Dht, fetcher: Fetcher) -> Attached {
        let level = self.level();
        dht.set_brake_level(level);
        fetcher.set_brake_level(level);
        *self.attached.lock().unwrap() = Some((dht, fetcher));
        Attached(self.clone())
    }
}

impl std::fmt::Debug for HealthHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthHub")
            .field("level", &self.level())
            .field("crawler_attached", &self.attached.lock().unwrap().is_some())
            .finish()
    }
}

/// Detaches the crawler from its [`HealthHub`] when dropped.
pub struct Attached(Arc<HealthHub>);

impl Drop for Attached {
    fn drop(&mut self) {
        self.0.attached.lock().unwrap().take();
    }
}

/// Probe until `stop`, slowing what `hub` holds while the router is saturated. Leaves the level
/// at 0 when it returns.
pub async fn health_loop(
    targets: Vec<SocketAddr>,
    hub: Arc<HealthHub>,
    mut stop: watch::Receiver<bool>,
) {
    if targets.is_empty() {
        return;
    }
    let mut h = Health::default();
    let mut esc = Escalation::default();
    let mut tick = tokio::time::interval(PROBE_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = stop_requested(&mut stop) => break,
        }
        let calm = esc.level() == 0;
        let s = probe(&targets).await;
        let saturated = h.push(s, calm);
        let changed = esc.update(saturated, Instant::now());
        if let Some(level) = changed {
            hub.set_level(level);
        }
        if saturated {
            // The engine's own brake state (stats, sampler share) follows the probe too.
            hub.brake_until(Instant::now() + BRAKE_FOR);
        }
        {
            let mut st = hub.stats.lock().unwrap();
            st.probes += 1;
            st.failed += u64::from(s.is_none());
            st.baseline_ms = h.baseline().map(|d| d.as_secs_f64() * 1000.0);
            st.last_median_ms = h.recent_median().map(|d| d.as_secs_f64() * 1000.0);
            st.brakes += u64::from(saturated);
            st.level = esc.level();
        }
        match changed {
            Some(level) if saturated => tracing::warn!(
                "DHT: router looks saturated (probe latency/loss), brake level {level}: DHT queries and TCP attempts at 1/{} of their rate",
                1u32 << level
            ),
            Some(0) => tracing::info!("DHT: router calm, brake released"),
            Some(level) => tracing::info!(
                "DHT: router calm, brake level {level} (1/{} of the rate)",
                1u32 << level
            ),
            None => {}
        }
    }
    hub.set_level(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Sample {
        Some(Duration::from_millis(n))
    }

    #[test]
    fn steady_latency_never_brakes() {
        let mut h = Health::default();
        for i in 0..400 {
            assert!(!h.push(ms(17 + (i % 3)), true), "sample {i}");
        }
        assert_eq!(h.baseline(), Some(Duration::from_millis(17)));
    }

    #[test]
    fn latency_rise_brakes() {
        let mut h = Health::default();
        for _ in 0..30 {
            assert!(!h.push(ms(17), true));
        }
        // The benchmark's KRPC-400 saturation: ~122 ms.
        let hits: Vec<bool> = (0..6).map(|_| h.push(ms(122), true)).collect();
        assert!(!hits[0] && !hits[1], "one or two slow samples are noise");
        assert!(*hits.last().unwrap());
        // 45 ms is below max(3 × 17, 17 + 40) = 57 ms.
        let mut h = Health::default();
        for _ in 0..30 {
            h.push(ms(17), true);
        }
        assert!((0..6).all(|_| !h.push(ms(45), true)));
    }

    #[test]
    fn loss_brakes_from_the_third_sample() {
        let mut h = Health::default();
        assert!(!h.push(None, true) && !h.push(None, true));
        assert!(
            h.push(None, true),
            "three failures, no baseline needed (D109)"
        );
        let mut h = Health::default();
        for _ in 0..6 {
            assert!(!h.push(ms(20), true));
        }
        assert!(
            !h.push(None, true) && !h.push(None, true),
            "two of the last six failed"
        );
        assert!(h.push(None, true), "three failed");
    }

    /// D109: samples taken under a brake never move the baseline, so a router that stays slow
    /// while braked keeps being judged against its calm latency.
    #[test]
    fn braked_samples_do_not_raise_the_baseline() {
        let mut h = Health::default();
        for _ in 0..20 {
            h.push(ms(17), true);
        }
        for _ in 0..200 {
            h.push(ms(90), false);
        }
        assert_eq!(h.baseline(), Some(Duration::from_millis(17)));
        assert!(
            h.push(ms(90), false),
            "still saturated against the calm baseline"
        );
    }

    /// D109 review: a braked stretch longer than the 10-min window keeps the calm baseline.
    #[test]
    fn a_long_braked_stretch_keeps_the_calm_baseline() {
        let mut h = Health::default();
        for _ in 0..20 {
            h.push(ms(17), true);
        }
        for _ in 0..(LONG * 2) {
            h.push(ms(90), false);
        }
        assert_eq!(h.baseline(), Some(Duration::from_millis(17)));
    }

    #[test]
    fn escalation_raises_spaced_and_relaxes_slowly() {
        let t = Instant::now();
        let s = |n: u64| t + Duration::from_secs(n);
        let mut e = Escalation::default();
        assert_eq!(e.update(true, s(0)), Some(1));
        assert_eq!(e.update(true, s(2)), None, "a raise gets one window to act");
        assert_eq!(e.update(true, s(12)), Some(2));
        assert_eq!(e.update(true, s(24)), Some(3));
        assert_eq!(e.update(true, s(36)), None, "deepest level");
        assert_eq!(e.level(), MAX_BRAKE_LEVEL);
        assert_eq!(e.update(false, s(36 + 299)), None);
        assert_eq!(e.update(false, s(36 + 300)), Some(2), "5 min calm");
        assert_eq!(e.update(false, s(36 + 330)), None);
        assert_eq!(
            e.update(false, s(36 + 360)),
            Some(1),
            "then one per calm minute"
        );
        assert_eq!(e.update(true, s(36 + 370)), Some(2), "saturated again");
        assert_eq!(
            e.update(false, s(36 + 370 + 60)),
            None,
            "back to the 5 min wait"
        );
        assert_eq!(e.update(false, s(36 + 370 + 300)), Some(1));
        assert_eq!(e.update(false, s(36 + 370 + 360)), Some(0));
        assert_eq!(e.update(false, s(10_000)), None);
    }

    #[test]
    fn targets() {
        assert_eq!(parse_targets("off"), vec![]);
        assert_eq!(parse_targets(""), vec![]);
        assert_eq!(parse_targets(DEFAULT_TARGETS).len(), 2);
    }
}
