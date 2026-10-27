//! Rate limits (docs/DHT.md §3.4 rules 3–5, D54):
//!
//! - a global outbound pace over every KRPC query (`MAGNETAR_DHT_QPS`), as a GCRA schedule: each
//!   caller reserves the next free slot and sleeps until it, so waiters are served in order;
//! - ≤ 1 query/s per remote IP (bucket burst 2) across all virtual nodes — libtorrent bans
//!   senders above 5 packets/s per IP;
//! - responses we send: 300/s globally and 5/s per IP, excess dropped silently (this bounds what a
//!   spoofed-source flood can reflect);
//! - the adaptive brake: when the answered share of the last 60 s falls below half of its 10-min
//!   average while sending more than 50 q/s, the pace halves for 5 minutes.
//!
//! Every map is capped (65 536 IPs); idle buckets are full and can be forgotten.

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

/// The global outbound pace (GCRA): `reserve` hands out send instants `1/rate` apart.
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
        let at = self.next.max(now);
        let step = if braked {
            self.interval * 2
        } else {
            self.interval
        };
        self.next = at + step;
        at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // An idle pace does not bank tokens: after a pause the next slot is "now".
        let later = t + Duration::from_secs(5);
        assert_eq!(p.reserve(later, false), later);
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
