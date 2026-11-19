//! Process-wide destination budget (D150, plan P0 "safety floor").
//!
//! A home router keeps one route-cache entry per destination IP (the neufbox: 4 096 entries,
//! 300 s) and one NAT entry per flow. The DHT has its own pacer (D106, D109), but every other
//! sender (Nostr relays, tracker scrapes, TMDB, IMDb, DB-IP, ntfy, anime lists) used to connect
//! with no shared limit, so a new source could repeat the 2026-10-30 outage through another
//! protocol. Every such connection now asks this budget first:
//!
//! - at most `max_dests` **distinct public destination IPs** in any `window` (default 300 s,
//!   the route cache's lifetime); a destination already in the window is free;
//! - at most `flows_per_sec` **new connections** per second (token bucket, burst of one second).
//!
//! The router health brake (D109) divides both by 2^level. A caller over the budget waits, up to
//! `max_wait` (or its own deadline), then gets [`Error::Budget`]: nothing was sent, and
//! callers retry later without counting it as the target's failure. Loopback, private, link-local, CGNAT (tailnet) and
//! ULA addresses never reach the home router's tables and are not counted.
//!
//! The budget is installed once per process ([`install`]); without it (tests, CLI tools)
//! [`admit`] lets everything through. librqbit (downloads) and the DHT engine are not under it:
//! the first has no connect hook, the second has its own pacer.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::time::Instant;

use crate::error::{Error, Result};

/// Highest brake level honoured (as the DHT pacer's `MAX_BRAKE_LEVEL`).
const MAX_LEVEL: u8 = 3;

#[derive(Clone, Debug)]
pub struct BudgetConfig {
    /// Distinct public destination IPs per `window`.
    pub max_dests: usize,
    /// New connections per second.
    pub flows_per_sec: f64,
    pub window: Duration,
    /// How long a caller may wait for room before it is refused.
    pub max_wait: Duration,
}

impl BudgetConfig {
    pub fn new(max_dests: usize, flows_per_sec: f64) -> Self {
        Self {
            max_dests: max_dests.max(1),
            flows_per_sec: flows_per_sec.max(0.1),
            window: Duration::from_secs(300),
            max_wait: Duration::from_secs(30),
        }
    }
}

/// Counters since start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BudgetStats {
    /// Connections let through (private destinations not counted).
    pub admitted: u64,
    /// Of those, how many had to wait for room.
    pub waited: u64,
    /// Connections refused after `max_wait`.
    pub refused: u64,
    /// Distinct destinations in the window now.
    pub dests_now: u64,
}

struct State {
    /// Destination → when it was last used.
    last: HashMap<IpAddr, Instant>,
    /// Uses in time order (an entry is stale when `last` holds a later instant for its IP).
    order: VecDeque<(Instant, IpAddr)>,
    tokens: f64,
    refilled: Instant,
}

pub struct DestBudget {
    cfg: BudgetConfig,
    state: Mutex<State>,
    level: AtomicU8,
    admitted: AtomicU64,
    waited: AtomicU64,
    refused: AtomicU64,
}

/// What a caller must do now.
enum Verdict {
    Go,
    Wait(Duration),
}

impl DestBudget {
    pub fn new(cfg: BudgetConfig) -> Self {
        Self {
            state: Mutex::new(State {
                last: HashMap::new(),
                order: VecDeque::new(),
                tokens: cfg.flows_per_sec.max(1.0),
                refilled: Instant::now(),
            }),
            cfg,
            level: AtomicU8::new(0),
            admitted: AtomicU64::new(0),
            waited: AtomicU64::new(0),
            refused: AtomicU64::new(0),
        }
    }

    pub fn config(&self) -> &BudgetConfig {
        &self.cfg
    }

    /// The router health brake level (0 = none): both limits are divided by 2^level.
    pub fn set_level(&self, level: u8) {
        self.level.store(level.min(MAX_LEVEL), Ordering::Relaxed);
    }

    pub fn level(&self) -> u8 {
        self.level.load(Ordering::Relaxed)
    }

    /// Limits now: (distinct destinations, flows per second).
    pub fn limits(&self) -> (usize, f64) {
        let div = 1u32 << self.level();
        (
            (self.cfg.max_dests / div as usize).max(1),
            self.cfg.flows_per_sec / f64::from(div),
        )
    }

    pub fn stats(&self) -> BudgetStats {
        let now = Instant::now();
        let mut s = self.state.lock().unwrap();
        self.prune(&mut s, now);
        BudgetStats {
            admitted: self.admitted.load(Ordering::Relaxed),
            waited: self.waited.load(Ordering::Relaxed),
            refused: self.refused.load(Ordering::Relaxed),
            dests_now: s.last.len() as u64,
        }
    }

    fn prune(&self, s: &mut State, now: Instant) {
        while let Some(&(at, ip)) = s.order.front() {
            if now.saturating_duration_since(at) < self.cfg.window {
                break;
            }
            s.order.pop_front();
            if s.last.get(&ip) == Some(&at) {
                s.last.remove(&ip);
            }
        }
    }

    fn try_take(&self, ip: IpAddr, now: Instant) -> Verdict {
        let (max_dests, rate) = self.limits();
        let mut s = self.state.lock().unwrap();
        self.prune(&mut s, now);
        let elapsed = now.saturating_duration_since(s.refilled).as_secs_f64();
        s.tokens = (s.tokens + elapsed * rate).min(rate.max(1.0));
        s.refilled = now;
        let known = s.last.contains_key(&ip);
        let dest_wait = if known || s.last.len() < max_dests {
            Duration::ZERO
        } else {
            // Room appears when the oldest live destination leaves the window.
            s.order
                .iter()
                .find(|(at, ip)| s.last.get(ip) == Some(at))
                .map(|(at, _)| (*at + self.cfg.window).saturating_duration_since(now))
                .unwrap_or(Duration::from_millis(100))
                .max(Duration::from_millis(10))
        };
        let flow_wait = if s.tokens >= 1.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - s.tokens) / rate.max(0.01))
        };
        if dest_wait.is_zero() && flow_wait.is_zero() {
            s.tokens -= 1.0;
            s.last.insert(ip, now);
            s.order.push_back((now, ip));
            Verdict::Go
        } else {
            Verdict::Wait(dest_wait.max(flow_wait))
        }
    }

    /// Ask to open a connection to `ip` for `what` (a short label for the error). Private
    /// destinations pass at once and are not counted. Waits at most `max_wait`.
    pub async fn admit(&self, what: &str, ip: IpAddr) -> Result<()> {
        self.admit_until(what, ip, None).await
    }

    /// [`DestBudget::admit`], also giving up at `deadline` (the caller's own connect deadline,
    /// minus the time the connect needs). A caller that cannot be served in time is refused
    /// without taking a token or a destination slot, so no flow is spent on a connect that
    /// would time out anyway.
    pub async fn admit_until(
        &self,
        what: &str,
        ip: IpAddr,
        deadline: Option<Instant>,
    ) -> Result<()> {
        if !counts(ip) {
            return Ok(());
        }
        let own = Instant::now() + self.cfg.max_wait;
        let deadline = deadline.map_or(own, |d| d.min(own));
        let mut waited = false;
        loop {
            let now = Instant::now();
            match self.try_take(ip, now) {
                Verdict::Go => {
                    self.admitted.fetch_add(1, Ordering::Relaxed);
                    if waited {
                        self.waited.fetch_add(1, Ordering::Relaxed);
                    }
                    return Ok(());
                }
                Verdict::Wait(d) => {
                    if now + d > deadline {
                        self.refused.fetch_add(1, Ordering::Relaxed);
                        let (dests, rate) = self.limits();
                        return Err(Error::Budget(format!(
                            "{what}: {ip} ({dests} destinations per {} s, {rate:.1} new \
                             connections/s, brake level {})",
                            self.cfg.window.as_secs(),
                            self.level()
                        )));
                    }
                    waited = true;
                    tokio::time::sleep(d).await;
                }
            }
        }
    }
}

/// Whether a connection to `ip` takes a home router route/NAT entry: public unicast addresses.
pub fn counts(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || (o[0] == 100 && (o[1] & 0xc0) == 64)) // 100.64/10: CGNAT, the tailnet
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return counts(IpAddr::V4(v4));
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // ULA (tailnet IPv6 too)
                || (s[0] & 0xffc0) == 0xfe80) // link-local
        }
    }
}

static GLOBAL: OnceLock<Arc<DestBudget>> = OnceLock::new();

/// Install the process budget (once; later calls are ignored and return false).
pub fn install(b: Arc<DestBudget>) -> bool {
    GLOBAL.set(b).is_ok()
}

pub fn global() -> Option<&'static Arc<DestBudget>> {
    GLOBAL.get()
}

/// [`DestBudget::admit`] on the process budget; a no-op when none is installed.
pub async fn admit(what: &str, ip: IpAddr) -> Result<()> {
    admit_until(what, ip, None).await
}

/// [`DestBudget::admit_until`] on the process budget; a no-op when none is installed.
pub async fn admit_until(what: &str, ip: IpAddr, deadline: Option<Instant>) -> Result<()> {
    match GLOBAL.get() {
        Some(b) => b.admit_until(what, ip, deadline).await,
        None => Ok(()),
    }
}

/// The share of a caller's connect timeout the budget may wait: the rest is left for the
/// DNS answer and the connect itself.
pub fn wait_share(timeout: Duration) -> Option<Instant> {
    Some(Instant::now() + timeout / 2)
}

/// Move the address a caller dials first to the front (the first IPv4 one: the UDP tracker
/// client and the home router are IPv4; else the first) and return it, so the admitted
/// address is the one dialled. Later addresses are only tried after a failed connect.
pub fn dial_first(addrs: &mut [std::net::SocketAddr]) -> Option<std::net::SocketAddr> {
    let i = addrs.iter().position(|a| a.is_ipv4()).unwrap_or(0);
    if addrs.is_empty() {
        return None;
    }
    addrs[..=i].rotate_right(1);
    Some(addrs[0])
}

/// Resolve `host:port` and admit the first address, for connections that resolve elsewhere
/// (tungstenite, reqwest). An IP literal is not looked up.
pub async fn admit_host(what: &str, host: &str, port: u16, timeout: Duration) -> Result<()> {
    if GLOBAL.get().is_none() {
        return Ok(());
    }
    let until = wait_share(timeout);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let ip = match host.parse::<IpAddr>() {
        Ok(ip) => ip,
        Err(_) => {
            let mut found: Vec<std::net::SocketAddr> =
                tokio::time::timeout(timeout, tokio::net::lookup_host((host, port)))
                    .await
                    .map_err(|_| Error::Timeout(format!("{what}: DNS lookup")))?
                    .map_err(|e| Error::Network(format!("{what}: {e}")))?
                    .collect();
            match dial_first(&mut found) {
                Some(a) => a.ip(),
                None => return Err(Error::Network(format!("{what}: no address"))),
            }
        }
    };
    admit_until(what, ip, until).await
}

/// How long [`BudgetResolver`] may wait for room: under reqwest's 10 s connect timeout, which
/// also covers resolving, so a wait ends as a counted refusal, not a dropped future.
pub const RESOLVER_WAIT: Duration = Duration::from_secs(5);

/// reqwest DNS resolver for first-party clients ([`crate::http::builder`]): resolves with the
/// system resolver, puts the address it admits first, and admits it. reqwest only resolves
/// when it opens a new connection (pooled ones are reused), so one admit is one new flow;
/// hyper's fallback to a second address after a failed connect is not counted.
#[derive(Clone, Copy, Debug, Default)]
pub struct BudgetResolver;

impl reqwest::dns::Resolve for BudgetResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let mut addrs: Vec<std::net::SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            if let Some(first) = dial_first(&mut addrs) {
                admit_until(&host, first.ip(), Some(Instant::now() + RESOLVER_WAIT))
                    .await
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u32) -> IpAddr {
        // 8.0.0.0/8 + n: public addresses.
        IpAddr::V4(std::net::Ipv4Addr::from(0x0800_0000 + n))
    }

    fn budget(dests: usize, flows: f64) -> DestBudget {
        let mut c = BudgetConfig::new(dests, flows);
        c.max_wait = Duration::from_secs(2);
        DestBudget::new(c)
    }

    #[tokio::test(start_paused = true)]
    async fn distinct_destinations_are_capped_and_known_ones_are_free() {
        let b = budget(3, 1000.0);
        for n in 0..3 {
            b.admit("t", ip(n)).await.unwrap();
        }
        // A known destination does not take a new slot.
        b.admit("t", ip(1)).await.unwrap();
        // A fourth one waits for the window (300 s) and is refused after max_wait (2 s).
        let e = b.admit("t", ip(9)).await.unwrap_err();
        assert!(matches!(e, Error::Budget(_)), "{e:?}");
        assert_eq!(b.stats().refused, 1);
        assert_eq!(b.stats().dests_now, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn destinations_leave_after_the_window() {
        let mut c = BudgetConfig::new(2, 1000.0);
        c.max_wait = Duration::from_secs(400);
        let b = DestBudget::new(c);
        b.admit("t", ip(0)).await.unwrap();
        b.admit("t", ip(1)).await.unwrap();
        let t = Instant::now();
        b.admit("t", ip(2)).await.unwrap();
        let waited = Instant::now() - t;
        assert!(waited >= Duration::from_secs(299), "{waited:?}");
        assert!(waited <= Duration::from_secs(301), "{waited:?}");
        assert_eq!(b.stats().waited, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn new_flows_are_paced() {
        let b = budget(1000, 5.0);
        let t = Instant::now();
        for n in 0..15 {
            b.admit("t", ip(n)).await.unwrap();
        }
        // A burst of 5, then 10 more at 5/s: about 2 s.
        let took = Instant::now() - t;
        assert!(took >= Duration::from_millis(1900), "{took:?}");
        assert!(took <= Duration::from_millis(2200), "{took:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn brake_level_divides_the_limits() {
        let b = budget(8, 8.0);
        b.set_level(2);
        assert_eq!(b.limits(), (2, 2.0));
        b.admit("t", ip(0)).await.unwrap();
        b.admit("t", ip(1)).await.unwrap();
        assert!(b.admit("t", ip(2)).await.is_err());
        b.set_level(9);
        assert_eq!(b.level(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_caller_deadline_refuses_early_without_spending_the_budget() {
        let b = budget(1, 1000.0);
        b.admit("t", ip(0)).await.unwrap();
        let t = Instant::now();
        // The next slot frees in 300 s; the caller can only wait 1 s: refused at once.
        let e = b
            .admit_until("t", ip(1), Some(t + Duration::from_secs(1)))
            .await
            .unwrap_err();
        assert!(matches!(e, Error::Budget(_)), "{e:?}");
        assert_eq!(Instant::now(), t, "no sleep when the wait cannot fit");
        assert_eq!(b.stats().dests_now, 1, "the refused caller took no slot");
    }

    #[test]
    fn the_admitted_address_is_the_one_dialled_first() {
        let a = |s: &str| s.parse::<std::net::SocketAddr>().unwrap();
        let mut v = vec![
            a("[2606:4700::1]:80"),
            a("[2606:4700::2]:80"),
            a("1.1.1.1:80"),
        ];
        assert_eq!(dial_first(&mut v), Some(a("1.1.1.1:80")));
        assert_eq!(v[0], a("1.1.1.1:80"));
        assert_eq!(v.len(), 3);
        let mut v6 = vec![a("[2606:4700::1]:80")];
        assert_eq!(dial_first(&mut v6), Some(a("[2606:4700::1]:80")));
        assert_eq!(dial_first(&mut []), None);
    }

    #[tokio::test(start_paused = true)]
    async fn private_destinations_are_not_counted() {
        let b = budget(1, 1000.0);
        for s in [
            "127.0.0.1",
            "192.168.0.1",
            "10.1.2.3",
            "172.20.0.5",
            "100.90.16.31",
            "169.254.1.1",
            "::1",
            "fd7a:115c:a1e0::1",
            "fe80::1",
            "::ffff:192.168.1.1",
        ] {
            let a: IpAddr = s.parse().unwrap();
            assert!(!counts(a), "{s}");
            b.admit("t", a).await.unwrap();
        }
        assert_eq!(b.stats().admitted, 0);
        for s in [
            "1.1.1.1",
            "100.128.0.1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(counts(s.parse().unwrap()), "{s}");
        }
    }
}
