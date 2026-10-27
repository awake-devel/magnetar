//! Resource brake of a worker (D100): small machines (a Raspberry Pi with 905 MB, whose
//! connection-tracking table holds 7680 entries) must slow down before they swap or drop
//! packets. Every 5 s it reads `/proc/meminfo` (MemAvailable), the process RSS and
//! `nf_conntrack_count / nf_conntrack_max`; "pressure" pauses DHT discovery and new seed-check
//! leases and halves KRPC and TCP rates until it is gone (hysteresis: 50 MB above the free
//! floor, 90 % of the RSS ceiling, conntrack under 80 %). Pure decision in [`Governor::next`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const EVERY: Duration = Duration::from_secs(5);
const FREE_HYSTERESIS_MB: f64 = 50.0;
const CONNTRACK_HIGH: f64 = 0.9;
const CONNTRACK_LOW: f64 = 0.8;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reading {
    pub mem_available_mb: Option<f64>,
    pub rss_mb: Option<f64>,
    /// `nf_conntrack_count / nf_conntrack_max` (None without netfilter conntrack).
    pub conntrack_share: Option<f64>,
}

fn kb_field(text: &str, field: &str) -> Option<f64> {
    let line = text.lines().find(|l| l.starts_with(field))?;
    line.split_whitespace().nth(1)?.parse::<f64>().ok()
}

fn read_num(path: &str) -> Option<f64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

pub fn read() -> Reading {
    let mem = std::fs::read_to_string("/proc/meminfo").ok();
    let status = std::fs::read_to_string("/proc/self/status").ok();
    let ct = match (
        read_num("/proc/sys/net/netfilter/nf_conntrack_count"),
        read_num("/proc/sys/net/netfilter/nf_conntrack_max"),
    ) {
        (Some(c), Some(m)) if m > 0.0 => Some(c / m),
        _ => None,
    };
    Reading {
        mem_available_mb: mem
            .as_deref()
            .and_then(|m| kb_field(m, "MemAvailable:"))
            .map(|k| k / 1024.0),
        rss_mb: status
            .as_deref()
            .and_then(|s| kb_field(s, "VmRSS:"))
            .map(|k| k / 1024.0),
        conntrack_share: ct,
    }
}

/// Limits; 0 = that check off.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub min_free_mb: u64,
    pub rss_max_mb: u64,
}

/// Shared state: read by the seed-check loop and the DHT pipeline.
#[derive(Debug, Default)]
pub struct Governor {
    pressure: AtomicBool,
    conntrack: AtomicBool,
    last: std::sync::Mutex<Reading>,
}

impl Governor {
    pub fn pressure(&self) -> bool {
        self.pressure.load(Ordering::Relaxed)
    }

    /// Connection tracking nearly full: brake the rates (memory may be fine).
    pub fn conntrack_full(&self) -> bool {
        self.conntrack.load(Ordering::Relaxed)
    }

    pub fn last(&self) -> Reading {
        *self.last.lock().unwrap()
    }

    /// The next (pressure, conntrack full) from the current state and a reading.
    pub fn next(lim: Limits, r: Reading, pressure: bool, ct_full: bool) -> (bool, bool) {
        let free_low = lim.min_free_mb > 0
            && r.mem_available_mb.is_some_and(|m| {
                let floor = lim.min_free_mb as f64;
                if pressure {
                    m < floor + FREE_HYSTERESIS_MB
                } else {
                    m < floor
                }
            });
        let rss_high = lim.rss_max_mb > 0
            && r.rss_mb.is_some_and(|m| {
                let ceil = lim.rss_max_mb as f64;
                if pressure { m > ceil * 0.9 } else { m > ceil }
            });
        let ct = r.conntrack_share.is_some_and(|s| {
            if ct_full {
                s > CONNTRACK_LOW
            } else {
                s > CONNTRACK_HIGH
            }
        });
        (free_low || rss_high, ct)
    }

    pub fn update(&self, lim: Limits, r: Reading) -> (bool, bool) {
        let (was_p, was_c) = (self.pressure(), self.conntrack_full());
        let (p, c) = Self::next(lim, r, was_p, was_c);
        if p != was_p {
            if p {
                tracing::warn!(
                    "worker: memory pressure (available {:.0} MB, RSS {:.0} MB): pausing discovery and new leases, halving rates",
                    r.mem_available_mb.unwrap_or(-1.0),
                    r.rss_mb.unwrap_or(-1.0)
                );
            } else {
                tracing::info!("worker: memory pressure over");
            }
        }
        if c != was_c {
            if c {
                tracing::warn!(
                    "worker: connection tracking {:.0} % full: halving rates",
                    r.conntrack_share.unwrap_or(0.0) * 100.0
                );
            } else {
                tracing::info!("worker: connection tracking back under 80 %");
            }
        }
        self.pressure.store(p, Ordering::Relaxed);
        self.conntrack.store(c, Ordering::Relaxed);
        *self.last.lock().unwrap() = r;
        (p, c)
    }
}

/// Read every [`EVERY`] until `stop`.
pub async fn run(g: Arc<Governor>, lim: Limits, mut stop: tokio::sync::watch::Receiver<bool>) {
    loop {
        g.update(lim, read());
        tokio::select! {
            _ = tokio::time::sleep(EVERY) => {}
            _ = crate::daemons::stop_requested(&mut stop) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(free: f64, rss: f64, ct: f64) -> Reading {
        Reading {
            mem_available_mb: Some(free),
            rss_mb: Some(rss),
            conntrack_share: Some(ct),
        }
    }

    #[test]
    fn hysteresis() {
        let lim = Limits {
            min_free_mb: 250,
            rss_max_mb: 200,
        };
        assert_eq!(
            Governor::next(lim, r(600.0, 100.0, 0.1), false, false),
            (false, false)
        );
        assert!(Governor::next(lim, r(240.0, 100.0, 0.1), false, false).0);
        assert!(
            Governor::next(lim, r(280.0, 100.0, 0.1), true, false).0,
            "still under 300"
        );
        assert!(!Governor::next(lim, r(310.0, 100.0, 0.1), true, false).0);
        assert!(
            Governor::next(lim, r(600.0, 210.0, 0.1), false, false).0,
            "RSS"
        );
        assert!(
            Governor::next(lim, r(600.0, 185.0, 0.1), true, false).0,
            "above 90 %"
        );
        assert!(!Governor::next(lim, r(600.0, 170.0, 0.1), true, false).0);
        assert!(Governor::next(lim, r(600.0, 100.0, 0.95), false, false).1);
        assert!(Governor::next(lim, r(600.0, 100.0, 0.85), false, true).1);
        assert!(!Governor::next(lim, r(600.0, 100.0, 0.85), false, false).1);
        let off = Limits {
            min_free_mb: 0,
            rss_max_mb: 0,
        };
        assert!(!Governor::next(off, r(1.0, 9999.0, 0.0), false, false).0);
        assert!(
            !Governor::next(lim, Reading::default(), false, false).0,
            "unknown = fine"
        );
    }

    #[test]
    fn reads_proc_here() {
        let r = read();
        assert!(r.rss_mb.unwrap() > 0.0 && r.mem_available_mb.unwrap() > 0.0);
    }
}
