//! Rate limits (docs/DHT.md §3.4 rules 3–5, D54):
//!
//! - a global outbound pace over every KRPC query (`MAGNETAR_DHT_QPS`): a FIFO of senders that
//!   get a permit at the configured rate, cancel-safe ([`Pacer`], D106);
//! - ≤ 1 query/s per remote IP (bucket burst 2) across all virtual nodes — libtorrent bans
//!   senders above 5 packets/s per IP;
//! - responses we send: 300/s globally and 5/s per IP, excess dropped silently (this bounds what a
//!   spoofed-source flood can reflect);
//! - the adaptive brake: when the answered share of the last 60 s falls below half of its 10-min
//!   average while sending more than 50 q/s, the pace halves for 5 minutes.
//!
//! Every map is capped (65 536 IPs, over [`IP_SHARDS`] locks); idle buckets are full and can be
//! forgotten.

use std::collections::{HashMap, VecDeque};
use std::net::Ipv4Addr;
use std::time::Duration;

use tokio::time::Instant;

pub const IP_BUCKETS_CAP: usize = 65_536;

/// Token bucket per IPv4 address, capped; an IP whose bucket is full again is forgotten first.
#[derive(Debug)]
pub struct PerIp {
    rate: f64,
    burst: f64,
    buckets: HashMap<Ipv4Addr, (f64, Instant)>,
    cap: usize,
}

impl PerIp {
    pub fn new(rate: f64, burst: f64, cap: usize) -> Self {
        Self {
            rate,
            burst,
            buckets: HashMap::new(),
            cap,
        }
    }

    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    /// Whether [`try_take`](Self::try_take) would succeed for `ip` now (nothing changes; a full
    /// map is not considered).
    pub fn available(&self, ip: &Ipv4Addr, now: Instant) -> bool {
        self.buckets.get(ip).is_none_or(|(tokens, at)| {
            tokens + now.saturating_duration_since(*at).as_secs_f64() * self.rate >= 1.0
        })
    }

    /// Take one token for `ip` if available.
    pub fn try_take(&mut self, ip: Ipv4Addr, now: Instant) -> bool {
        if !self.buckets.contains_key(&ip) && self.buckets.len() >= self.cap {
            let refill = Duration::from_secs_f64(self.burst / self.rate);
            self.buckets
                .retain(|_, (_, at)| now.saturating_duration_since(*at) < refill);
            if self.buckets.len() >= self.cap {
                return false; // drop on full
            }
        }
        let (tokens, at) = self.buckets.entry(ip).or_insert((self.burst, now));
        let elapsed = now.saturating_duration_since(*at).as_secs_f64();
        *tokens = (*tokens + elapsed * self.rate).min(self.burst);
        *at = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// A plain token bucket (global response cap).
#[derive(Debug)]
pub struct Bucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    at: Instant,
}

impl Bucket {
    pub fn new(rate: f64, burst: f64, now: Instant) -> Self {
        Self {
            rate,
            burst,
            tokens: burst,
            at: now,
        }
    }

    pub fn try_take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

const WINDOW_SHORT: usize = 60;
const WINDOW_LONG: usize = 600;
const BRAKE_MIN_RATE: f64 = 50.0;
const BRAKE_FOR: Duration = Duration::from_secs(300);

/// Answered share over the last 10 minutes, per second, and the brake state.
#[derive(Debug)]
pub struct Brake {
    /// (sent, answered) per second, newest last.
    seconds: VecDeque<(u32, u32)>,
    current: (u32, u32),
    until: Option<Instant>,
    pub engaged: u64,
}

impl Default for Brake {
    fn default() -> Self {
        Self {
            seconds: VecDeque::with_capacity(WINDOW_LONG),
            current: (0, 0),
            until: None,
            engaged: 0,
        }
    }
}

impl Brake {
    pub fn sent(&mut self) {
        self.current.0 = self.current.0.saturating_add(1);
    }

    pub fn answered(&mut self) {
        self.current.1 = self.current.1.saturating_add(1);
    }

    /// Add `sent` / `answered` to the current second (the engine counts them in atomics, D106).
    pub fn record(&mut self, sent: u32, answered: u32) {
        self.current.0 = self.current.0.saturating_add(sent);
        self.current.1 = self.current.1.saturating_add(answered);
    }

    pub fn active(&self, now: Instant) -> bool {
        self.until.is_some_and(|u| now < u)
    }

    /// Engage (or extend) the brake from outside, e.g. the router health probe (D66).
    pub fn engage_until(&mut self, until: Instant) {
        if self.until.is_none_or(|u| u < until) {
            if !self.until.is_some_and(|u| Instant::now() < u) {
                self.engaged += 1;
            }
            self.until = Some(until);
        }
    }

    fn share(&self, last: usize) -> Option<(f64, f64)> {
        let n = self.seconds.len().min(last);
        let (s, a) = self
            .seconds
            .iter()
            .rev()
            .take(n)
            .fold((0u64, 0u64), |(s, a), (x, y)| {
                (s + u64::from(*x), a + u64::from(*y))
            });
        (s > 0 && n > 0).then(|| (a as f64 / s as f64, s as f64 / n as f64))
    }

    /// Close the current second (called once per second). Returns true when the brake engages.
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.seconds.len() == WINDOW_LONG {
            self.seconds.pop_front();
        }
        self.seconds.push_back(self.current);
        self.current = (0, 0);
        if self.active(now) || self.seconds.len() < WINDOW_SHORT * 2 {
            return false;
        }
        let (Some((short, rate)), Some((long, _))) =
            (self.share(WINDOW_SHORT), self.share(WINDOW_LONG))
        else {
            return false;
        };
        if rate > BRAKE_MIN_RATE && short < long / 2.0 {
            self.until = Some(now + BRAKE_FOR);
            self.engaged += 1;
            return true;
        }
        false
    }
}

/// Shards of the per-IP limit: an address always maps to the same shard, so the 1 q/s rule holds
/// exactly while queries from different virtual nodes and lookups take different locks (D106).
pub const IP_SHARDS: usize = 16;

/// [`PerIp`] split by address over [`IP_SHARDS`] locks, the same total cap.
#[derive(Debug)]
pub struct IpLimits {
    shards: Vec<std::sync::Mutex<PerIp>>,
}

impl IpLimits {
    pub fn new(rate: f64, burst: f64, cap: usize) -> Self {
        let per = cap.div_ceil(IP_SHARDS).max(1);
        Self {
            shards: (0..IP_SHARDS)
                .map(|_| std::sync::Mutex::new(PerIp::new(rate, burst, per)))
                .collect(),
        }
    }

    fn shard(&self, ip: &Ipv4Addr) -> std::sync::MutexGuard<'_, PerIp> {
        // Fibonacci hashing of the address: neighbouring addresses spread over the shards.
        let h = u32::from(*ip).wrapping_mul(0x9E37_79B1) >> 28;
        self.shards[h as usize % IP_SHARDS].lock().unwrap()
    }

    pub fn try_take(&self, ip: Ipv4Addr, now: Instant) -> bool {
        self.shard(&ip).try_take(ip, now)
    }

    pub fn available(&self, ip: &Ipv4Addr, now: Instant) -> bool {
        self.shard(ip).available(ip, now)
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lock().unwrap().len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A GCRA schedule: `reserve` hands out instants `1/rate` apart, waiters served in order. Used by
/// the metadata fetcher's TCP connection rate; KRPC queries use [`Pacer`] (D106), because a
/// reserved instant whose caller is cancelled is lost.
#[derive(Debug)]
pub struct Pace {
    interval: Duration,
    next: Instant,
}

impl Pace {
    pub fn new(qps: u32, now: Instant) -> Self {
        Self {
            interval: Duration::from_secs_f64(1.0 / f64::from(qps.max(1))),
            next: now,
        }
    }

    /// The instant the caller may send at; twice as far apart while `braked`.
    pub fn reserve(&mut self, now: Instant, braked: bool) -> Instant {
        self.reserve_slowed(now, if braked { 2 } else { 1 })
    }

    /// The instant the caller may send at, `divisor` times as far apart (a brake level, D109).
    pub fn reserve_slowed(&mut self, now: Instant, divisor: u32) -> Instant {
        let at = self.next.max(now);
        self.next = at + self.interval * divisor.max(1);
        at
    }
}

/// How often the pacer hands out send permits.
pub const PACER_TICK: Duration = Duration::from_millis(2);

/// Deepest router health brake (D109): 2^3 = an eighth of the configured rate.
pub const MAX_BRAKE_LEVEL: u8 = 3;

/// Rate divisor for a brake state: 2 while `braked` (the answered-share brake, or a level-1
/// health brake), 2^level for the health brake, the larger cut wins.
pub fn brake_divisor(braked: bool, level: u8) -> u32 {
    let by_level = 1u32 << level.min(MAX_BRAKE_LEVEL);
    by_level.max(if braked { 2 } else { 1 })
}

/// The global outbound pace over every virtual node (D106): a FIFO of senders served at the
/// configured rate (half while braked).
///
/// The earlier pace (D54) reserved a send instant per query in advance. A lookup that ended
/// dropped its queued queries, and their instants went unused: at 4 096 lookups and a cap of
/// 3 000 q/s only 73–75 % of the cap was sent, on the live line and on loopback alike
/// (docs/DHT.md §20). Here a waiter takes a permit only when its turn comes; a waiter that is
/// dropped leaves the queue, and the permit goes to the next one.
///
/// Permits are added every [`PACER_TICK`] by [`Pacer::refill`], at most [`Pacer::burst`]
/// unused at a time, so an idle pacer does not bank a burst.
///
/// One pacer can be shared by every DHT sender of the process (the crawler's engine and the
/// seed checker's walks, D109): `MAGNETAR_DHT_QPS` is then the total the router sees.
#[derive(Debug)]
pub struct Pacer {
    sem: tokio::sync::Semaphore,
    rate: f64,
    burst: usize,
    braked: std::sync::atomic::AtomicBool,
    /// Router health brake level (D109): the rate is divided by 2^level, [`MAX_BRAKE_LEVEL`]
    /// at most. Combined with `braked` by taking the larger cut.
    level: std::sync::atomic::AtomicU8,
    state: std::sync::Mutex<(f64, Instant)>,
    /// Sends taken ahead of the queue since the last refill ([`Pacer::take_ahead`]).
    ahead: std::sync::atomic::AtomicU64,
}

impl Pacer {
    pub fn new(qps: u32, now: Instant) -> Self {
        let rate = f64::from(qps.max(1));
        Self {
            sem: tokio::sync::Semaphore::new(1),
            rate,
            // Two ticks' worth: a tick that comes late does not lose its permits.
            burst: ((rate * PACER_TICK.as_secs_f64() * 2.0).ceil() as usize).max(1),
            braked: std::sync::atomic::AtomicBool::new(false),
            level: std::sync::atomic::AtomicU8::new(0),
            state: std::sync::Mutex::new((0.0, now)),
            ahead: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn burst(&self) -> usize {
        self.burst
    }

    /// Halve the rate (brake, D66) or restore it.
    pub fn set_braked(&self, braked: bool) {
        self.braked
            .store(braked, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn braked(&self) -> bool {
        self.braked.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Set the router health brake level (0 = none; clamped to [`MAX_BRAKE_LEVEL`]).
    pub fn set_level(&self, level: u8) {
        self.level.store(
            level.min(MAX_BRAKE_LEVEL),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    pub fn level(&self) -> u8 {
        self.level.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// What the rate is divided by now: 2 while `braked`, 2^level from the health brake, the
    /// larger of the two.
    pub fn divisor(&self) -> u32 {
        brake_divisor(self.braked(), self.level())
    }

    /// The rate in effect now (permits per second).
    pub fn effective_rate(&self) -> f64 {
        self.rate / f64::from(self.divisor())
    }

    /// Add the permits earned since the last refill (returns how many); at most
    /// [`burst`](Self::burst) stay unused.
    pub fn refill(&self, now: Instant) -> usize {
        let mut st = self.state.lock().unwrap();
        let (credit, at) = &mut *st;
        let rate = self.effective_rate();
        *credit += now.saturating_duration_since(*at).as_secs_f64() * rate;
        *at = now;
        // Sends taken ahead of the queue came out of this budget too (at most one second of
        // debt is carried, so a burst of them cannot silence the queue for long).
        let ahead = self.ahead.swap(0, std::sync::atomic::Ordering::Relaxed);
        *credit = (*credit - ahead as f64).max(-rate);
        let whole = credit.floor().max(0.0);
        *credit -= whole;
        // Everything earned goes to the waiters (a late tick on a busy runtime must not lose
        // its share: capping before handing out cost ~15 % of the rate, docs/DHT.md §20); only
        // what nobody took is cut back to `burst`.
        let add = whole as usize;
        if add > 0 {
            self.sem.add_permits(add);
            let idle = self.sem.available_permits();
            if idle > self.burst {
                self.sem.forget_permits(idle - self.burst);
            }
        }
        add
    }

    /// Wait for a send permit (FIFO). Cancel-safe: a dropped wait consumes nothing.
    pub async fn acquire(&self) {
        if let Ok(p) = self.sem.acquire().await {
            p.forget();
        }
    }

    /// Send now, ahead of the queue, and charge the send to the shared budget: the next refills
    /// hand out that many permits fewer, so the total stays at the rate. For a sender with a
    /// reserved share that spaces itself (the sampler's priority share, [`super::budget`]).
    pub fn take_ahead(&self) {
        self.ahead
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Take a permit if one is free now.
    pub fn try_acquire(&self) -> bool {
        self.sem.try_acquire().map(|p| p.forget()).is_ok()
    }

    /// Run [`refill`](Self::refill) every [`PACER_TICK`] until `stop` is set.
    pub async fn run(&self, mut stop: tokio::sync::watch::Receiver<bool>) {
        let mut tick = tokio::time::interval(PACER_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => { self.refill(Instant::now()); }
                _ = stop.wait_for(|s| *s) => return,
            }
        }
    }
}

impl crate::liveness::dht::WalkPacer for Pacer {
    fn acquire(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(Pacer::acquire(self))
    }

    fn rate(&self) -> Option<f64> {
        Some(self.effective_rate())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brake_levels_divide_the_rate() {
        assert_eq!(brake_divisor(false, 0), 1);
        assert_eq!(brake_divisor(true, 0), 2);
        assert_eq!(brake_divisor(true, 1), 2, "the larger cut, not both");
        assert_eq!(brake_divisor(false, 2), 4);
        assert_eq!(brake_divisor(true, 3), 8);
        assert_eq!(brake_divisor(false, 9), 8, "clamped to MAX_BRAKE_LEVEL");
        let p = Pacer::new(80, Instant::now());
        p.set_level(2);
        assert_eq!(p.effective_rate(), 20.0);
        p.set_level(7);
        assert_eq!(p.level(), MAX_BRAKE_LEVEL);
        assert_eq!(p.effective_rate(), 10.0);
        p.set_level(0);
        p.set_braked(true);
        assert_eq!(p.effective_rate(), 40.0);
    }

    #[test]
    fn braked_pacer_hands_out_the_divided_rate() {
        let t = Instant::now();
        let p = Pacer::new(1000, t);
        p.set_level(3);
        assert_eq!(p.refill(t + Duration::from_secs(1)), 125);
        p.set_level(0);
        assert_eq!(p.refill(t + Duration::from_secs(2)), 1000);
    }

    #[test]
    fn pace_slowed_spaces_by_the_divisor() {
        let t = Instant::now();
        let mut p = Pace::new(100, t);
        assert_eq!(p.reserve_slowed(t, 4), t);
        assert_eq!(p.reserve_slowed(t, 1), t + Duration::from_millis(40));
    }

    #[test]
    fn per_ip_burst_two_then_one_per_second() {
        let mut l = PerIp::new(1.0, 2.0, 4);
        let ip = Ipv4Addr::new(1, 2, 3, 4);
        let t = Instant::now();
        assert!(l.available(&ip, t));
        assert!(l.try_take(ip, t));
        assert!(l.try_take(ip, t));
        assert!(!l.available(&ip, t));
        assert!(!l.try_take(ip, t));
        assert!(l.available(&ip, t + Duration::from_millis(1000)));
        assert!(!l.try_take(ip, t + Duration::from_millis(900)));
        assert!(l.try_take(ip, t + Duration::from_millis(1000)));
        assert!(l.try_take(Ipv4Addr::new(1, 2, 3, 5), t));
    }

    #[test]
    fn per_ip_map_is_capped() {
        let mut l = PerIp::new(1.0, 2.0, 3);
        let t = Instant::now();
        for i in 0..3 {
            assert!(l.try_take(Ipv4Addr::new(9, 0, 0, i), t));
        }
        // Full of active buckets: a new IP is refused, the map stays at its cap.
        assert!(!l.try_take(Ipv4Addr::new(9, 0, 0, 9), t));
        assert_eq!(l.len(), 3);
        // Once the old buckets have refilled they are forgotten.
        assert!(l.try_take(Ipv4Addr::new(9, 0, 0, 9), t + Duration::from_secs(3)));
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn pace_spaces_reservations() {
        let t = Instant::now();
        let mut p = Pace::new(100, t);
        let a = p.reserve(t, false);
        let b = p.reserve(t, false);
        let c = p.reserve(t, true);
        let d = p.reserve(t, false);
        assert_eq!(a, t);
        assert_eq!(b - a, Duration::from_millis(10));
        assert_eq!(d - c, Duration::from_millis(20));
        let later = t + Duration::from_secs(5);
        assert_eq!(p.reserve(later, false), later);
    }

    #[test]
    fn pacer_rate_burst_and_brake() {
        let t = Instant::now();
        let p = Pacer::new(1000, t);
        // Starts with one permit; 10 ms earn 10 but at most `burst` (4) wait unused.
        assert!(p.try_acquire());
        assert!(!p.try_acquire());
        assert_eq!(p.burst(), 4);
        assert_eq!(p.refill(t + Duration::from_millis(10)), 10);
        for _ in 0..4 {
            assert!(p.try_acquire());
        }
        assert!(!p.try_acquire());
        // Braked: half the rate.
        p.set_braked(true);
        assert_eq!(p.refill(t + Duration::from_millis(14)), 2);
        p.set_braked(false);
        assert_eq!(p.refill(t + Duration::from_millis(16)), 2);
    }

    /// A dropped waiter consumes nothing: the permit goes to the next one (the old pace lost
    /// the slot of every cancelled query, docs/DHT.md §20).
    #[tokio::test]
    async fn pacer_waiter_cancel_keeps_the_permit() {
        let t = Instant::now();
        let p = Pacer::new(1000, t);
        assert!(p.try_acquire());
        {
            let w = p.acquire();
            tokio::pin!(w);
            assert!(futures::poll!(w.as_mut()).is_pending());
        } // dropped while queued
        p.refill(t + Duration::from_millis(1));
        assert!(p.try_acquire());
    }

    /// A late refill hands everything earned to the queued senders (only idle permits are
    /// capped at `burst`): capping first lost ~15 % of the rate on a busy runtime.
    #[tokio::test]
    async fn pacer_late_tick_serves_every_waiter_it_earned() {
        let t = Instant::now();
        let p = std::sync::Arc::new(Pacer::new(1000, t));
        assert!(p.try_acquire());
        let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..20 {
            let (p, done) = (p.clone(), done.clone());
            set.spawn(async move {
                p.acquire().await;
                done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            });
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(p.refill(t + Duration::from_millis(10)), 10);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(done.load(std::sync::atomic::Ordering::Relaxed), 10);
        set.abort_all();
    }

    /// Sends taken ahead of the queue are charged to it: the total stays at the rate.
    #[test]
    fn pacer_sends_ahead_are_charged() {
        let t = Instant::now();
        let p = Pacer::new(1000, t);
        assert!(p.try_acquire());
        for _ in 0..4 {
            p.take_ahead();
        }
        assert_eq!(p.refill(t + Duration::from_millis(10)), 6);
        // More ahead than earned: the debt is carried to the next refill.
        for _ in 0..15 {
            p.take_ahead();
        }
        assert_eq!(p.refill(t + Duration::from_millis(20)), 0);
        assert_eq!(p.refill(t + Duration::from_millis(30)), 5);
    }

    #[test]
    fn ip_limits_hold_per_ip_across_shards() {
        let l = IpLimits::new(1.0, 2.0, 1024);
        let t = Instant::now();
        for i in 0..200u32 {
            let ip = Ipv4Addr::from(0x0a00_0000 + i * 7919);
            assert!(l.try_take(ip, t));
            assert!(l.try_take(ip, t));
            assert!(
                !l.try_take(ip, t),
                "third query to {ip} in the same instant"
            );
            assert!(!l.available(&ip, t));
            assert!(l.try_take(ip, t + Duration::from_secs(1)));
        }
        assert_eq!(l.len(), 200);
    }

    #[test]
    fn brake_engages_when_answers_collapse() {
        let second = |b: &mut Brake, sent: u32, answered: u32, now: &mut Instant| {
            for _ in 0..sent {
                b.sent();
            }
            for _ in 0..answered {
                b.answered();
            }
            *now += Duration::from_secs(1);
            b.tick(*now)
        };
        let mut b = Brake::default();
        let mut now = Instant::now();
        for _ in 0..540 {
            assert!(!second(&mut b, 200, 120, &mut now));
        }
        let mut engaged = false;
        for _ in 0..60 {
            engaged |= second(&mut b, 200, 10, &mut now);
        }
        assert!(engaged);
        assert!(b.active(now));
        assert_eq!(b.engaged, 1);
        assert!(!b.active(now + BRAKE_FOR));
        // Low rates never brake.
        let mut quiet = Brake::default();
        let mut now = Instant::now();
        for i in 0..600 {
            let answered = if i < 540 { 20 } else { 0 };
            assert!(!second(&mut quiet, 40, answered, &mut now));
        }
    }

    #[test]
    fn bucket_caps_bursts() {
        let t = Instant::now();
        let mut b = Bucket::new(5.0, 5.0, t);
        for _ in 0..5 {
            assert!(b.try_take(t));
        }
        assert!(!b.try_take(t));
        assert!(b.try_take(t + Duration::from_millis(200)));
    }
}
