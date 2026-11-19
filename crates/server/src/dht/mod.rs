//! DHT crawl building blocks (docs/DHT.md, D49–D55, D156): the options a crawler is built from,
//! the shared pacer, and the pieces the worker's crawler and the server's fleet ingest share
//! (seen filter, fetch queue, content [`screen`], the `dht_seen` store and the batched writer
//! [`persist`]). The crawl itself runs in `worker` only; `serve` is the index (D156).

pub mod classify;
pub mod health;
pub mod persist;
pub mod queue;
pub mod screen;
pub mod seen;
pub mod store;
pub mod upnp;

use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use magnetar_core::dht::Mode;
use magnetar_core::dht::limiter::Pacer;
use magnetar_core::liveness::dht::BOOTSTRAP;
use tokio::time::Instant;

use crate::config::{Config, DhtMode};
use screen::Screen;

#[derive(Clone, Debug)]
pub struct DhtOptions {
    /// Metadata fetches in flight (`MAGNETAR_DHT_FETCH_CONCURRENCY`).
    pub fetch_concurrency: usize,
    /// Largest info dict accepted, bytes (`MAGNETAR_DHT_METADATA_MAX`).
    pub metadata_max: usize,
    /// The content screen (D53). None: verified metadata is never stored (fail closed).
    pub screen: Option<Arc<dyn Screen>>,
    /// New TCP connection attempts per second (`MAGNETAR_DHT_TCP_RATE`, D66; default from the
    /// query rate, D109).
    pub tcp_rate: u32,
    /// TCP connections open at once (`MAGNETAR_DHT_TCP_MAX`, D66).
    pub tcp_max: usize,
    /// Router health probe targets (`MAGNETAR_DHT_PROBE`; empty = off, D66).
    pub health_targets: Vec<SocketAddr>,
    pub mode: Mode,
    pub ports: Vec<u16>,
    pub qps: u32,
    pub pool_cap: usize,
    pub peer_lookups: usize,
    /// Soft per-query timeout of lookups (`MAGNETAR_DHT_LOOKUP_TIMEOUT_MS`, D93).
    pub lookup_timeout: Duration,
    /// Peers tried per hash, and where a lookup stops (`MAGNETAR_DHT_PEERS_PER_HASH`).
    pub peers_per_hash: usize,
    /// Share of the KRPC budget reserved for sampling ahead of the queue
    /// (`MAGNETAR_DHT_SAMPLE_PRIORITY`, D106; 0 = none).
    pub sample_priority: f64,
    pub bloom_bits: u64,
    pub queue_cap: usize,
    /// Router port mappings for the UDP ports (D57, D72); None = no UPnP.
    pub upnp: Option<upnp::UpnpConfig>,
    /// Crawl map counters (D101): None = no counting by country / network.
    pub geo: Option<Arc<crate::crawlmap::CrawlMap>>,
    /// The process's shared DHT pacer ([`shared_pacer`], D109), run by its owner. None: the
    /// engine paces itself (tests).
    pub pacer: Option<Arc<Pacer>>,
    /// The process's router health probe (D109): the crawler attaches its engine and fetcher
    /// to it while it runs. None: the crawler runs its own probe (tests).
    pub health: Option<Arc<health::HealthHub>>,
}

/// Default metadata TCP attempts per second for a query rate (D109): half of it, 5–150. A SYN
/// to a dead peer holds a router NAT entry until its timeout (120 s on the user's router), so
/// this rate, not the query rate, fills small NAT tables first.
pub fn default_tcp_rate(qps: u32) -> u32 {
    (qps / 2).clamp(5, 150)
}

/// Default peer collections at once in aggressive mode for a query rate (D114): 0.4 per q/s,
/// 8–4096. A collection waits ~8.6 s for its walk, so a fixed 64 capped the real send rate at
/// ~150–180 q/s whatever `MAGNETAR_DHT_QPS` said; measured on the workers 2026-10-31, every
/// collection costs ~2.35 q/s (eee: 256 → 607 q/s, rpi: 128 → 348 q/s), so 0.4 per q/s fills
/// the budget without queueing far past it.
pub fn default_peer_lookups(qps: u32) -> usize {
    (qps as usize * 2 / 5).clamp(8, 4096)
}

/// The pacer every sender of the process shares (D109, D146): a worker's crawl engine, the seed
/// checker's walks and its tracker contacts (D113). Its rate is `MAGNETAR_DHT_QPS` (or the
/// mode's default) with a crawler (`crawler`: the worker with `MAGNETAR_DHT` on),
/// `MAGNETAR_LIVENESS_DHT_QPS` with only seed checks, even with `MAGNETAR_LIVENESS_DHT=false`:
/// tracker scrapes alone still open a flow per tracker through the router, so they need the
/// pace and the router health brake that comes with it (D146). `serve` never crawls (D156) and
/// always passes `crawler = false`, so `MAGNETAR_DHT` cannot change its pace. None when the
/// process sends neither. The caller runs it ([`Pacer::run`]).
pub fn shared_pacer(cfg: &Config, crawler: bool, seed_checks: bool) -> Option<Arc<Pacer>> {
    let qps = match DhtOptions::from_config(cfg, cfg.dht).filter(|_| crawler) {
        Some(o) => o.qps,
        None if seed_checks => cfg.liveness_dht_qps,
        None => return None,
    };
    Some(Arc::new(Pacer::new(qps, Instant::now())))
}

impl DhtOptions {
    /// Options for `mode` (None for `off`) from the configuration (docs/DHT.md §3.2, §8).
    pub fn from_config(cfg: &Config, mode: DhtMode) -> Option<Self> {
        let mode = match mode {
            DhtMode::Off => return None,
            DhtMode::Polite => Mode::Polite,
            DhtMode::Aggressive => Mode::Aggressive,
        };
        let aggressive = mode == Mode::Aggressive;
        let nodes = cfg
            .dht_nodes
            .unwrap_or(if aggressive { 4 } else { 1 })
            .clamp(1, magnetar_core::dht::engine::MAX_VIRTUAL_NODES as u8);
        let ports = (0..nodes)
            .map(|i| {
                if aggressive {
                    cfg.dht_port + u16::from(i)
                } else {
                    0
                }
            })
            .collect();
        let qps = cfg.dht_qps.unwrap_or(if aggressive { 200 } else { 50 });
        Some(Self {
            mode,
            ports,
            qps,
            pool_cap: cfg.dht_pool.map(|p| p as usize).unwrap_or(if aggressive {
                1_000_000
            } else {
                200_000
            }),
            peer_lookups: cfg
                .dht_peer_lookups
                .map(|n| n as usize)
                .unwrap_or(if aggressive {
                    default_peer_lookups(qps)
                } else {
                    8
                }),
            lookup_timeout: Duration::from_millis(u64::from(cfg.dht_lookup_timeout_ms)),
            peers_per_hash: cfg.dht_peers_per_hash as usize,
            sample_priority: if cfg.dht_sample_priority.is_finite() {
                cfg.dht_sample_priority.clamp(0.0, 1.0)
            } else {
                0.0
            },
            bloom_bits: seen::DEFAULT_BITS,
            queue_cap: queue::DEFAULT_CAP,
            fetch_concurrency: cfg
                .dht_fetch_concurrency
                .map(|n| n as usize)
                .unwrap_or(if aggressive { 128 } else { 16 }),
            metadata_max: cfg.dht_metadata_max as usize,
            // The process-wide blocklist (installed at start); None stores nothing.
            screen: screen::global_screen(),
            tcp_rate: cfg.dht_tcp_rate.unwrap_or_else(|| default_tcp_rate(qps)),
            tcp_max: cfg.dht_tcp_max as usize,
            health_targets: health::parse_targets(&cfg.dht_probe),
            // Polite mode uses an ephemeral port: nothing to map. The worker clears this unless
            // `MAGNETAR_DHT_UPNP=true` is set explicitly (docs/WORKERS.md §4).
            upnp: (aggressive && cfg.dht_upnp.unwrap_or(true)).then(|| {
                upnp::UpnpConfig::from_settings(
                    cfg.dht_upnp_gateway.as_deref(),
                    cfg.dht_upnp_local_ip,
                )
            }),
            geo: None,
            pacer: None,
            health: None,
        })
    }
}

/// Resolve the public bootstrap routers (IPv4, global addresses only).
pub async fn resolve_bootstrap() -> Vec<SocketAddrV4> {
    let lookups = BOOTSTRAP.iter().map(|host| async move {
        match tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host(*host)).await {
            Ok(Ok(addrs)) => addrs
                .filter_map(|a| match a {
                    SocketAddr::V4(v) if magnetar_core::dht::addr::is_global(*v.ip()) => Some(v),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    });
    let mut out: Vec<SocketAddrV4> = futures::future::join_all(lookups)
        .await
        .into_iter()
        .flatten()
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn peer_lookups_follow_the_query_rate() {
        use super::{DhtOptions, default_peer_lookups};
        use crate::config::{Cli, DhtMode};
        use clap::Parser;
        assert_eq!(default_peer_lookups(200), 80, "the aggressive default rate");
        assert_eq!(default_peer_lookups(640), 256, "eee's measured setting");
        assert_eq!(default_peer_lookups(5), 8, "a floor");
        assert_eq!(
            default_peer_lookups(20_000),
            4096,
            "the ONT setting is the cap"
        );
        let opts = |args: &[&str]| {
            let mut v = vec!["magnetar-server"];
            v.extend(args);
            v.push("serve");
            let cli = Cli::try_parse_from(v).unwrap();
            DhtOptions::from_config(&cli.config, DhtMode::Aggressive).unwrap()
        };
        // D114: the old fixed 64 held 640 q/s to ~180 real q/s.
        assert_eq!(opts(&["--dht-qps", "640"]).peer_lookups, 256);
        assert_eq!(opts(&[]).peer_lookups, 80);
        assert_eq!(
            opts(&["--dht-qps", "640", "--dht-peer-lookups", "100"]).peer_lookups,
            100,
            "an explicit setting wins"
        );
        let polite = Cli::try_parse_from(["magnetar-server", "serve"]).unwrap();
        let polite = DhtOptions::from_config(&polite.config, DhtMode::Polite).unwrap();
        assert_eq!(
            polite.peer_lookups, 8,
            "polite mode keeps its small fixed default"
        );
    }

    #[test]
    fn tcp_rate_follows_the_query_rate() {
        use super::default_tcp_rate;
        assert_eq!(default_tcp_rate(75), 37);
        assert_eq!(default_tcp_rate(200), 100);
        assert_eq!(default_tcp_rate(1000), 150, "the measured cap");
        assert_eq!(default_tcp_rate(4), 5, "a floor so fetching still moves");
    }

    /// D146: with the crawler off and `MAGNETAR_LIVENESS_DHT=false` (the main server here),
    /// seed checks still get the shared pacer, so tracker scrapes are paced and the caller
    /// starts the router health probe that brakes them.
    #[test]
    fn seed_checks_without_dht_still_get_the_pacer() {
        use super::shared_pacer;
        use crate::config::Cli;
        use clap::Parser;
        let cfg = |args: &[&str]| {
            let mut v = vec!["magnetar-server"];
            v.extend(args);
            v.push("serve");
            Cli::try_parse_from(v).unwrap().config
        };
        let prod = cfg(&["--dht", "off", "--liveness-dht", "false"]);
        let p = shared_pacer(&prod, false, true).expect("seed checks send tracker traffic");
        assert_eq!(p.rate(), f64::from(prod.liveness_dht_qps));
        assert!(
            shared_pacer(&prod, false, false).is_none(),
            "no crawler, no seed checks: nothing to pace"
        );
        let crawl = cfg(&["--dht", "polite", "--dht-qps", "7"]);
        assert_eq!(shared_pacer(&crawl, true, true).unwrap().rate(), 7.0);
        // D156: `serve` never crawls and passes `crawler = false`; a leftover MAGNETAR_DHT
        // (whatever its rate) must not change the server's pace.
        let leftover = cfg(&["--dht", "aggressive", "--dht-qps", "640"]);
        assert_eq!(
            shared_pacer(&leftover, false, true).unwrap().rate(),
            f64::from(leftover.liveness_dht_qps)
        );
    }
}
