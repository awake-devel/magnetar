//! `magnetar-server worker` (D96–D100, docs/WORKERS.md): a crawl worker for a main server,
//! without a database. It takes seed-check leases and runs its own DHT crawler from its own
//! line, and reports everything to the server over its HTTPS API with a worker token. The
//! server stays the single source of truth (Postgres, search index, API).

pub mod client;
pub mod crawl;
pub mod governor;
pub mod seeds;
pub mod state;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use magnetar_core::dht::EngineConfig;
use magnetar_core::liveness::LivenessConfig;
use serde::Serialize;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::config::{Config, DhtMode};
use crate::daemons::stop_requested;
use crate::dht::DhtOptions;
use crate::fleet::proto::*;
use client::{Client, ClientError};
use governor::{Governor, Limits};
use seeds::{SeedCounters, SeedReport};

pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(30);
/// Worker defaults where the server's would not fit a small machine (docs/WORKERS.md §4).
pub const WORKER_POOL: usize = 200_000;
pub const WORKER_FETCHES: usize = 64;
pub const WORKER_QUEUE: usize = 20_000;
/// Seen filter: 32 Mbit per generation (2 × 4 MiB).
pub const WORKER_SEEN_BITS: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct WorkerOptions {
    pub server: String,
    pub token: String,
    pub state_dir: PathBuf,
    /// Seed checks: None = off.
    pub liveness: Option<LivenessConfig>,
    pub liveness_batch: u32,
    /// DHT crawler: None = off.
    pub dht: Option<DhtOptions>,
    pub limits: Limits,
    pub upload_buffer: usize,
    pub heartbeat_every: Duration,
    /// Crawl map (D105): resolve DHT nodes to country / network here and send the codes
    /// with triage requests. None = off (no database is downloaded).
    pub geo: Option<crate::geo::GeoConfig>,
}

impl WorkerOptions {
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        let server = cfg
            .worker_server
            .clone()
            .filter(|s| !s.trim().is_empty())
            .context("MAGNETAR_WORKER_SERVER (the main server's URL) is required for worker")?;
        let token = cfg
            .worker_token
            .clone()
            .filter(|s| !s.trim().is_empty())
            .context("MAGNETAR_WORKER_TOKEN is required for worker")?;
        let dht = DhtOptions::from_config(cfg, cfg.dht).map(|mut o| {
            // A worker runs on small machines and has no database behind its queue: smaller
            // defaults unless set explicitly (docs/WORKERS.md §4).
            if cfg.dht_pool.is_none() {
                o.pool_cap = WORKER_POOL;
            }
            if cfg.dht_fetch_concurrency.is_none() {
                o.fetch_concurrency = WORKER_FETCHES.min(o.fetch_concurrency);
            }
            o.queue_cap = WORKER_QUEUE;
            o.bloom_bits = WORKER_SEEN_BITS;
            // Another person's router: ports are mapped only when asked explicitly.
            if cfg.dht_upnp != Some(true) {
                o.upnp = None;
            }
            o
        });
        let liveness = cfg
            .worker_liveness
            .then(|| crate::liveness::liveness_config(cfg));
        anyhow::ensure!(
            liveness.is_some() || dht.is_some(),
            "worker: nothing to do (MAGNETAR_WORKER_LIVENESS=false and MAGNETAR_DHT=off)"
        );
        Ok(Self {
            server,
            token,
            state_dir: cfg
                .worker_state_dir
                .clone()
                .unwrap_or_else(|| cfg.state_dir().join("worker")),
            liveness,
            liveness_batch: cfg.worker_liveness_batch,
            dht,
            limits: Limits {
                min_free_mb: cfg.worker_min_free_mb,
                rss_max_mb: cfg.worker_rss_max_mb,
            },
            upload_buffer: (cfg.worker_upload_buffer_mb as usize) << 20,
            heartbeat_every: HEARTBEAT_EVERY,
            geo: (cfg.geoip != crate::geo::GeoMode::Off).then(|| {
                cfg.geo_config(
                    cfg.worker_state_dir
                        .clone()
                        .unwrap_or_else(|| cfg.state_dir().join("worker"))
                        .join("geoip"),
                )
            }),
        })
    }
}

/// A refusal by the server that a restart cannot fix (bad token, incompatible version,
/// workers disabled): `magnetar-server worker` exits with [`FATAL_EXIT`] so a supervisor
/// (systemd `RestartPreventExitStatus`) does not restart it in a loop.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Fatal(pub String);

pub const FATAL_EXIT: i32 = 2;

/// Lets any task stop the whole worker with a fatal error.
#[derive(Clone)]
pub struct Abort {
    tx: Arc<watch::Sender<bool>>,
    err: Arc<Mutex<Option<String>>>,
}

impl Abort {
    fn new() -> (Self, watch::Receiver<bool>) {
        let (tx, rx) = watch::channel(false);
        (
            Self {
                tx: Arc::new(tx),
                err: Arc::default(),
            },
            rx,
        )
    }

    pub fn fatal(&self, e: &ClientError) {
        let mut g = self.err.lock().unwrap();
        if g.is_none() {
            tracing::error!("worker: {e}");
            *g = Some(e.to_string());
        }
        let _ = self.tx.send(true);
    }

    fn stop(&self) {
        let _ = self.tx.send(true);
    }

    pub fn stopping(&self) -> bool {
        *self.tx.borrow()
    }

    fn error(&self) -> Option<String> {
        self.err.lock().unwrap().clone()
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct WorkerReport {
    pub name: String,
    pub server_version: String,
    pub seconds: f64,
    pub liveness: Option<SeedReport>,
    pub dht: Option<crawl::CrawlReport>,
    pub heartbeats: u64,
    pub heartbeat_errors: u64,
    pub pressure_secs: f64,
    pub rss_peak_mb: Option<f64>,
    pub rss_mb: Option<f64>,
    pub cpu_seconds: Option<f64>,
    /// Set when the worker stopped on a fatal error.
    pub error: Option<String>,
}

fn proc_status_kb(field: &str) -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with(field))?;
    line.split_whitespace().nth(1)?.parse::<f64>().ok()
}

fn proc_cpu_seconds() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rest = s.get(s.rfind(')')? + 2..)?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ut: f64 = f.get(11)?.parse().ok()?;
    let st: f64 = f.get(12)?.parse().ok()?;
    Some((ut + st) / 100.0)
}

fn hello_of(opts: &WorkerOptions) -> Hello {
    Hello {
        version: magnetar_core::VERSION.into(),
        arch: std::env::consts::ARCH.into(),
        liveness: opts.liveness.is_some(),
        dht: opts.dht.as_ref().map(|d| {
            match d.mode {
                magnetar_core::dht::Mode::Polite => "polite",
                magnetar_core::dht::Mode::Aggressive => "aggressive",
            }
            .to_string()
        }),
        dht_ports: opts
            .dht
            .as_ref()
            .map(|d| d.ports.clone())
            .unwrap_or_default(),
        metadata_max: opts.dht.as_ref().map_or(0, |d| d.metadata_max as u64),
        blocklist_version: magnetar_core::blocklist::global().map_or(0, |b| b.version()),
    }
}

/// Say hello until the server answers: network errors are retried (the server may be
/// restarting), a fatal answer (token, version) is returned.
async fn connect(
    client: &Client,
    hello: &Hello,
    stop: &mut watch::Receiver<bool>,
) -> Result<Option<HelloResponse>, ClientError> {
    let mut wait = Duration::from_secs(2);
    loop {
        match client.post::<_, HelloResponse>(HELLO, hello).await {
            Ok(r) => return Ok(Some(r)),
            Err(e) if e.is_fatal() => return Err(e),
            Err(e) => {
                tracing::warn!(
                    "worker: server {} not reachable ({e}); retrying in {} s",
                    client.base(),
                    wait.as_secs()
                );
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = stop_requested(stop) => return Ok(None),
                }
                wait = (wait * 2).min(Duration::from_secs(60));
            }
        }
    }
}

/// Run until `stop` (or `budget`, or a fatal error) and return the report. `engine_tweak`
/// is the test seam for fake DHT networks (as `dht::run`).
pub async fn run(
    opts: WorkerOptions,
    stop: watch::Receiver<bool>,
    budget: Option<Duration>,
    engine_tweak: impl FnOnce(&mut EngineConfig),
) -> anyhow::Result<WorkerReport> {
    let started = Instant::now();
    state::ensure_dir(&opts.state_dir)
        .with_context(|| format!("worker state dir {}", opts.state_dir.display()))?;
    let client = Client::new(&opts.server, &opts.token)?;
    let (abort, int_rx) = Abort::new();
    {
        let (abort, mut stop) = (abort.clone(), stop.clone());
        tokio::spawn(async move {
            stop_requested(&mut stop).await;
            abort.stop();
        });
    }
    if let Some(b) = budget {
        let abort = abort.clone();
        tokio::spawn(async move {
            tokio::time::sleep(b).await;
            abort.stop();
        });
    }
    let hello = hello_of(&opts);
    let mut wait_rx = int_rx.clone();
    let welcome = match connect(&client, &hello, &mut wait_rx).await {
        Ok(Some(w)) => w,
        Ok(None) => return Ok(WorkerReport::default()),
        Err(e) => return Err(Fatal(e.to_string()).into()),
    };
    if welcome.worker_api != WORKER_API {
        return Err(Fatal(format!(
            "incompatible server: it speaks worker API {}, this worker {WORKER_API}",
            welcome.worker_api
        ))
        .into());
    }
    tracing::info!(
        "worker {} connected to {} (magnetar-server {}, worker API {}): seed checks {}, DHT {}",
        welcome.name,
        client.base(),
        welcome.server_version,
        welcome.worker_api,
        if opts.liveness.is_some() { "on" } else { "off" },
        hello.dht.as_deref().unwrap_or("off")
    );

    let governor = Arc::new(Governor::default());
    let gov_task = tokio::spawn(governor::run(governor.clone(), opts.limits, int_rx.clone()));

    let seed_counters = Arc::new(SeedCounters::default());
    let seed_task = opts.liveness.clone().map(|cfg| {
        let batch = opts.liveness_batch.min(welcome.max_lease.max(1));
        let (client, gov, c, rx, abort) = (
            client.clone(),
            governor.clone(),
            seed_counters.clone(),
            int_rx.clone(),
            abort.clone(),
        );
        tokio::spawn(async move {
            if let Err(e) = seeds::run(client, cfg, batch, gov, c, rx).await {
                abort.fatal(&e);
            }
        })
    });

    let crawl = match opts.dht.clone() {
        Some(mut d) => {
            // D105: the worker's own geo database (downloaded into its state dir), loaded in
            // the background; until then its triage items carry no codes.
            if let Some(g) = opts.geo.clone() {
                let map = Arc::new(crate::crawlmap::CrawlMap::new(
                    g.mode,
                    crate::crawlmap::NODE_BITS_WORKER,
                ));
                tokio::spawn(crate::crawlmap::refresh_loop(
                    map.clone(),
                    g,
                    int_rx.clone(),
                ));
                d.geo = Some(map);
            }
            // The server refuses dicts above its own limit: never fetch more than that.
            if welcome.metadata_max > 0 {
                d.metadata_max = d.metadata_max.min(welcome.metadata_max as usize);
            }
            let setup = crawl::CrawlSetup {
                opts: d,
                state_dir: opts.state_dir.clone(),
                upload_buffer: opts.upload_buffer,
                max_results: welcome.max_results as usize,
            };
            match crawl::start(
                setup,
                client.clone(),
                governor.clone(),
                abort.clone(),
                int_rx.clone(),
                engine_tweak,
            )
            .await
            {
                Ok(c) => Some(c),
                Err(e) => {
                    abort.stop();
                    return Err(e);
                }
            }
        }
        None => None,
    };

    // Heartbeats until the stop.
    let mut heartbeats = 0u64;
    let mut heartbeat_errors = 0u64;
    let mut pressure_ms = 0u64;
    let mut stop_rx = int_rx.clone();
    let mut tick = tokio::time::interval(opts.heartbeat_every);
    let mut last_tick = Instant::now();
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = stop_requested(&mut stop_rx) => break,
        }
        if governor.pressure() {
            pressure_ms += last_tick.elapsed().as_millis() as u64;
        }
        last_tick = Instant::now();
        let g = governor.last();
        let hb = Heartbeat {
            uptime_secs: started.elapsed().as_secs(),
            rss_mb: g.rss_mb,
            mem_available_mb: g.mem_available_mb,
            pressure: governor.pressure() || governor.conntrack_full(),
            dht: crawl.as_ref().and_then(|(c, _)| c.state()),
        };
        match client.post::<_, serde_json::Value>(HEARTBEAT, &hb).await {
            Ok(_) => {
                heartbeats += 1;
                state::touch_heartbeat(&opts.state_dir);
            }
            Err(e) if e.is_fatal() => {
                abort.fatal(&e);
                break;
            }
            Err(e) => {
                heartbeat_errors += 1;
                tracing::warn!("worker: heartbeat: {e}");
                // A restarted server forgets who we are: say hello again on the next answer.
                let _ = client.post::<_, HelloResponse>(HELLO, &hello).await;
            }
        }
    }

    let dht_report = match crawl {
        Some((_, h)) => h.await.ok(),
        None => None,
    };
    if let Some(t) = seed_task {
        let _ = tokio::time::timeout(Duration::from_secs(30), t).await;
    }
    gov_task.abort();
    let report = WorkerReport {
        name: welcome.name,
        server_version: welcome.server_version,
        seconds: started.elapsed().as_secs_f64(),
        liveness: opts.liveness.as_ref().map(|_| seed_counters.report()),
        dht: dht_report,
        heartbeats,
        heartbeat_errors,
        pressure_secs: pressure_ms as f64 / 1000.0,
        rss_peak_mb: proc_status_kb("VmHWM:").map(|k| k / 1024.0),
        rss_mb: proc_status_kb("VmRSS:").map(|k| k / 1024.0),
        cpu_seconds: proc_cpu_seconds(),
        error: abort.error(),
    };
    Ok(report)
}

/// `magnetar-server worker [--seconds N]`: run, print the JSON report, fail on a fatal error.
pub async fn run_from_config(cfg: &Config, seconds: Option<u64>) -> anyhow::Result<()> {
    let opts = WorkerOptions::from_config(cfg)?;
    if cfg.dht != DhtMode::Off && opts.dht.is_none() {
        anyhow::bail!("worker: DHT options did not resolve");
    }
    let (stop_tx, stop_rx) = watch::channel(false);
    tokio::spawn(async move {
        crate::server::shutdown_signal().await;
        tracing::info!("worker: stopping");
        let _ = stop_tx.send(true);
    });
    let report = run(opts, stop_rx, seconds.map(Duration::from_secs), |_| {}).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if let Some(e) = report.error {
        return Err(Fatal(e).into());
    }
    Ok(())
}
