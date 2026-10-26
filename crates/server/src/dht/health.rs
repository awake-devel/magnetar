//! Router health probe (D66, docs/research/router-bench.md): the home router, not the laptop,
//! is what breaks first under crawling (its NAT/CPU), and it shows as rising latency and loss on
//! **every** flow. Public ICMP is blocked on the user's line and the gateway rate-limits ICMP, so
//! the probe is a plain TCP connect (SYN → SYN-ACK, then closed) to public anycast targets
//! (`MAGNETAR_DHT_PROBE`, default `1.1.1.1:443,9.9.9.9:443`), one every 5 s.
//!
//! Decision ([`Health::push`], pure): baseline = 10th percentile of the successful samples of
//! the last 10 min; the router is **saturated** when, over the last 6 samples (30 s), at least 3
//! failed or their median is above `max(3 × baseline, baseline + 40 ms)`. The benchmark's
//! saturations were 122 ms against a 17 ms baseline (KRPC 400 q/s) and 100 % loss (400 TCP
//! attempts/s); its safe rates stayed at 17–19 ms. Saturated → KRPC rate and TCP connection
//! attempts halved for 5 min (extended while it lasts).

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Duration;

use magnetar_core::dht::Dht;
use magnetar_core::metainfo::Fetcher;
use serde::Serialize;
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::daemons::stop_requested;

pub const PROBE_EVERY: Duration = Duration::from_secs(5);
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
pub const BRAKE_FOR: Duration = Duration::from_secs(300);
pub const DEFAULT_TARGETS: &str = "1.1.1.1:443,9.9.9.9:443";
/// Samples kept for the baseline (10 min at one per 5 s).
const LONG: usize = 120;
/// Samples judged (30 s).
const SHORT: usize = 6;
const MIN_FAILED: usize = 3;
/// Successful samples needed before a baseline is trusted.
const MIN_BASELINE: usize = 12;

/// One probe result: connect time, None = failed or timed out.
pub type Sample = Option<Duration>;

#[derive(Debug, Default)]
pub struct Health {
    samples: VecDeque<Sample>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct HealthStats {
    pub probes: u64,
    pub failed: u64,
    pub baseline_ms: Option<f64>,
    pub last_median_ms: Option<f64>,
    /// Times the probe engaged (or extended) the brake.
    pub brakes: u64,
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
        let mut ok: Vec<Duration> = self.samples.iter().flatten().copied().collect();
        if ok.len() < MIN_BASELINE {
            return None;
        }
        ok.sort_unstable();
        Some(ok[ok.len() / 10])
    }

    pub fn recent_median(&self) -> Option<Duration> {
        median(
            self.samples
                .iter()
                .rev()
                .take(SHORT)
                .flatten()
                .copied()
                .collect(),
        )
    }

    /// Record a sample; true when the router looks saturated.
    pub fn push(&mut self, s: Sample) -> bool {
        if self.samples.len() == LONG {
            self.samples.pop_front();
        }
        self.samples.push_back(s);
        if self.samples.len() < SHORT {
            return false;
        }
        let failed = self
            .samples
            .iter()
            .rev()
            .take(SHORT)
            .filter(|s| s.is_none())
            .count();
        let Some(base) = self.baseline() else {
            // No baseline yet: only a run of failures counts.
            return failed >= MIN_FAILED;
        };
        if failed >= MIN_FAILED {
            return true;
        }
        let limit = (base * 3).max(base + Duration::from_millis(40));
        self.recent_median().is_some_and(|m| m > limit)
    }
}

/// Parse `MAGNETAR_DHT_PROBE`: `off` (or empty) disables the probe.
pub fn parse_targets(s: &str) -> Vec<SocketAddr> {
    if s.trim().eq_ignore_ascii_case("off") {
        return Vec::new();
    }
    s.split(',').filter_map(|t| t.trim().parse().ok()).collect()
}

async fn probe(target: SocketAddr) -> Sample {
    let t0 = Instant::now();
    match tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect(target)).await {
        Ok(Ok(_)) => Some(t0.elapsed()),
        _ => None,
    }
}

/// Probe until `stop`, braking the DHT engine and the fetcher while the router is saturated.
pub async fn health_loop(
    targets: Vec<SocketAddr>,
    dht: Dht,
    fetcher: Fetcher,
    stats: std::sync::Arc<std::sync::Mutex<HealthStats>>,
    mut stop: watch::Receiver<bool>,
) {
    if targets.is_empty() {
        return;
    }
    let mut h = Health::default();
    let mut tick = tokio::time::interval(PROBE_EVERY);
    for i in 0usize.. {
        tokio::select! {
            _ = tick.tick() => {}
            _ = stop_requested(&mut stop) => return,
        }
        let s = probe(targets[i % targets.len()]).await;
        let saturated = h.push(s);
        {
            let mut st = stats.lock().unwrap();
            st.probes += 1;
            st.failed += u64::from(s.is_none());
            st.baseline_ms = h.baseline().map(|d| d.as_secs_f64() * 1000.0);
            st.last_median_ms = h.recent_median().map(|d| d.as_secs_f64() * 1000.0);
            if saturated {
                st.brakes += 1;
            }
        }
        if saturated {
            let until = Instant::now() + BRAKE_FOR;
            dht.brake_until(until);
            fetcher.brake_until(until);
            tracing::warn!(
                "DHT: router looks saturated (probe latency/loss), halving query and connection rates for 5 min"
            );
        }
    }
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
        for i in 0..200 {
            assert!(!h.push(ms(17 + (i % 3))), "sample {i}");
        }
        assert_eq!(h.baseline(), Some(Duration::from_millis(17)));
    }

    #[test]
    fn latency_rise_brakes() {
        let mut h = Health::default();
        for _ in 0..30 {
            assert!(!h.push(ms(17)));
        }
        // The benchmark's KRPC-400 saturation: ~122 ms.
        let hits: Vec<bool> = (0..6).map(|_| h.push(ms(122))).collect();
        assert!(!hits[0] && !hits[1], "one or two slow samples are noise");
        assert!(*hits.last().unwrap());
        // 45 ms is below max(3 × 17, 17 + 40) = 57 ms.
        let mut h = Health::default();
        for _ in 0..30 {
            h.push(ms(17));
        }
        assert!((0..6).all(|_| !h.push(ms(45))));
    }

    #[test]
    fn loss_brakes_even_without_a_baseline() {
        let mut h = Health::default();
        for _ in 0..6 {
            assert!(!h.push(ms(20)));
        }
        assert!(!h.push(None) && !h.push(None), "two of the last six failed");
        assert!(h.push(None), "three failed");
    }

    #[test]
    fn targets() {
        assert_eq!(parse_targets("off"), vec![]);
        assert_eq!(parse_targets(""), vec![]);
        assert_eq!(parse_targets(DEFAULT_TARGETS).len(), 2);
    }
}
