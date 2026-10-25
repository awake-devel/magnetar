//! Background loops run by `serve` (ARCHITECTURE.md §9). Each one stops when the shutdown
//! watch flips to true; a failing pass is logged and retried on the next tick.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use magnetar_core::api::CrawlerStatus;
use magnetar_core::crawler::{self, CrawlConfig, CrawlReport};
use magnetar_core::liveness::{self, LivenessConfig};
use magnetar_core::notify::{self, NtfyConfig};
use magnetar_core::store::{Store, keys};
use magnetar_core::{model, trust};
use tokio::sync::watch;

use crate::config::Config;
use crate::store::PgStore;

pub const LIVENESS_INTERVAL: Duration = Duration::from_secs(5 * 60);
pub const LIVENESS_BATCH: usize = 500;
pub const TRUST_POLL: Duration = Duration::from_secs(60);
pub const TRUST_MAX_AGE: Duration = Duration::from_secs(6 * 3600);

/// Resolves once shutdown is requested (never, if the sender is gone).
pub async fn stop_requested(shutdown: &mut watch::Receiver<bool>) {
    let requested = shutdown.wait_for(|v| *v).await.is_ok();
    if !requested {
        std::future::pending::<()>().await;
    }
}

/// Sleep, or return true early if shutdown was requested.
async fn stopped_after(d: Duration, shutdown: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(d) => false,
        _ = stop_requested(shutdown) => true,
    }
}

fn setting_string(v: Option<String>) -> Option<String> {
    let v = v?;
    serde_json::from_str::<Option<String>>(&v)
        .unwrap_or(Some(v))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Apply startup env to settings (root npub).
pub async fn apply_env(store: &PgStore, cfg: &Config) -> anyhow::Result<()> {
    if let Some(root) = cfg.root_npub.as_deref().filter(|r| !r.trim().is_empty()) {
        let hex = model::parse_pubkey(root)?;
        store
            .set_setting(keys::TRUST_ROOT, &serde_json::to_string(&hex)?)
            .await?;
    }
    Ok(())
}

/// Server crawl config: seeds from env, else the `crawl.seed_relays` setting, else defaults.
pub async fn crawl_config(store: &PgStore, cfg: &Config, budget: Duration) -> CrawlConfig {
    let mut c = CrawlConfig::server_default();
    c.budget = budget;
    c.max_relays = cfg.crawl_max_relays.max(1);
    c.explore_relays = cfg.crawl_explore_relays;
    let env_seeds = cfg.seed_relays();
    if !env_seeds.is_empty() {
        c.seed_relays = env_seeds;
    } else if let Ok(Some(v)) = store.get_setting(keys::SEED_RELAYS).await
        && let Ok(seeds) = serde_json::from_str::<Vec<String>>(&v)
        && !seeds.is_empty()
    {
        c.seed_relays = seeds;
    }
    c.root = setting_string(store.get_setting(keys::TRUST_ROOT).await.ok().flatten());
    c
}

/// ntfy target: settings first, env as fallback. None if no topic.
pub async fn ntfy_config(store: &PgStore, cfg: &Config) -> Option<NtfyConfig> {
    let url = setting_string(store.get_setting(keys::NTFY_URL).await.ok().flatten())
        .or_else(|| cfg.ntfy_url.clone())
        .unwrap_or_else(|| "https://ntfy.sh".into());
    let topic = setting_string(store.get_setting(keys::NTFY_TOPIC).await.ok().flatten())
        .or_else(|| cfg.ntfy_topic.clone())
        .filter(|t| !t.trim().is_empty())?;
    Some(NtfyConfig {
        base_url: url,
        topic,
        token: None,
    })
}

/// Saved-search notifier: one ntfy message per search with new hits (D25).
pub async fn notify_saved_searches(store: &PgStore, cfg: &Config) {
    let hits = match notify::poll_saved_searches(store, magnetar_core::now()).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("saved searches: {e}");
            return;
        }
    };
    let Some(ntfy) = ntfy_config(store, cfg).await else {
        return;
    };
    for (search, new) in hits.iter().filter(|(_, h)| !h.is_empty()) {
        let n = new.len();
        let msg = format!("{n} new result{}", if n == 1 { "" } else { "s" });
        let title = format!("Magnetar: {}", search.name);
        if let Err(e) = notify::ntfy_publish(&ntfy, &title, &msg, None).await {
            tracing::warn!("ntfy for {:?}: {e}", search.name);
        }
    }
}

/// One crawl with status bookkeeping (shared by the loop and `crawl-once`). `metadata` false
/// skips the indexer phase (kinds 0/3/10000/10002) for this pass.
pub async fn crawl_pass(
    store: &PgStore,
    cfg: &Config,
    status: &Mutex<CrawlerStatus>,
    budget: Duration,
    metadata: bool,
) -> magnetar_core::Result<CrawlReport> {
    {
        let mut st = status.lock().unwrap();
        st.running = true;
        st.next_run = None;
    }
    let mut ccfg = crawl_config(store, cfg, budget).await;
    ccfg.fetch_metadata &= metadata;
    let res = crawler::crawl_once(store, &ccfg).await;
    let mut st = status.lock().unwrap();
    st.running = false;
    st.last_run = Some(magnetar_core::now());
    st.passes += 1;
    match &res {
        Ok(r) => {
            st.last_report = Some(r.clone());
            st.last_error = None;
        }
        Err(e) => st.last_error = Some(e.to_string()),
    }
    res
}

/// Pause between passes in continuous mode (`MAGNETAR_CRAWL_INTERVAL_SECS=0`, D30).
pub const CONTINUOUS_PAUSE: Duration = Duration::from_secs(30);
/// Continuous-mode pause after a pass that failed or had no relay to try (all backed off).
pub const CONTINUOUS_IDLE_PAUSE: Duration = Duration::from_secs(120);
/// Indexer metadata and the saved-search notifier run at most this often, whatever the mode.
pub const METADATA_EVERY: Duration = Duration::from_secs(15 * 60);
pub const NOTIFY_EVERY: Duration = Duration::from_secs(15 * 60);

/// Pause after a pass: the configured interval, or in continuous mode a short fixed one that
/// never drops to zero (an instant pass must not hot-spin; relay backoff lives in the store).
fn pause_after(interval: Option<Duration>, res: &magnetar_core::Result<CrawlReport>) -> Duration {
    match (interval, res) {
        (Some(d), _) => d,
        (None, Ok(r)) if r.relays_tried > 0 => CONTINUOUS_PAUSE,
        (None, _) => CONTINUOUS_IDLE_PAUSE,
    }
}

fn due(last: Option<Instant>, every: Duration) -> bool {
    last.is_none_or(|t| t.elapsed() >= every)
}

pub async fn crawler_loop(
    store: Arc<PgStore>,
    cfg: Arc<Config>,
    status: Arc<Mutex<CrawlerStatus>>,
    mut shutdown: watch::Receiver<bool>,
) {
    let budget = CrawlConfig::server_default().budget;
    let interval = cfg.crawl_interval();
    match interval {
        Some(d) => tracing::info!("crawler: a pass every {} s", d.as_secs()),
        None => tracing::info!(
            "crawler: continuous, {} s between passes",
            CONTINUOUS_PAUSE.as_secs()
        ),
    }
    let mut last_metadata: Option<Instant> = None;
    let mut last_notify: Option<Instant> = None;
    loop {
        let metadata = due(last_metadata, METADATA_EVERY);
        let started = Instant::now();
        let pass = crawl_pass(&store, &cfg, &status, budget, metadata);
        let res = tokio::select! {
            res = pass => res,
            _ = stop_requested(&mut shutdown) => return,
        };
        match &res {
            Ok(r) => {
                if metadata {
                    last_metadata = Some(started);
                }
                tracing::info!(
                    inserted = r.inserted,
                    duplicates = r.duplicates,
                    relays_ok = r.relays_ok,
                    relays_tried = r.relays_tried,
                    "crawl done in {} ms",
                    r.duration_ms
                );
                // Watermarked per search, so a skipped poll only batches hits into the next one.
                if due(last_notify, NOTIFY_EVERY) {
                    last_notify = Some(Instant::now());
                    notify_saved_searches(&store, &cfg).await;
                }
            }
            Err(e) => tracing::warn!("crawl failed: {e}"),
        }
        let pause = pause_after(interval, &res);
        status.lock().unwrap().next_run = Some(magnetar_core::now() + pause.as_secs() as i64);
        if stopped_after(pause, &mut shutdown).await {
            return;
        }
    }
}

pub async fn liveness_loop(
    store: Arc<PgStore>,
    lcfg: Arc<LivenessConfig>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let enabled = store
            .get_setting(keys::LIVENESS_ENABLED)
            .await
            .ok()
            .flatten()
            .is_none_or(|v| v.trim().trim_matches('"') != "false");
        if enabled {
            let run = liveness::run_due(&*store, &lcfg, magnetar_core::now(), LIVENESS_BATCH);
            tokio::select! {
                res = run => match res {
                    Ok(n) if n > 0 => tracing::info!("liveness: checked {n}"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("liveness: {e}"),
                },
                _ = stop_requested(&mut shutdown) => return,
            }
        }
        if stopped_after(LIVENESS_INTERVAL, &mut shutdown).await {
            return;
        }
    }
}

/// Recompute when the graph version moves, else every 6 h.
pub async fn trust_loop(store: Arc<PgStore>, mut shutdown: watch::Receiver<bool>) {
    let mut last_version: Option<i64> = None;
    let mut last_full = Instant::now();
    loop {
        match store.graph_version().await {
            Ok(v) => {
                if last_version != Some(v) || last_full.elapsed() >= TRUST_MAX_AGE {
                    let run = trust::recompute(
                        &*store,
                        magnetar_core::now(),
                        TRUST_MAX_AGE.as_secs() as i64,
                    );
                    tokio::select! {
                        res = run => match res {
                            Ok(n) => {
                                tracing::info!("trust: rescored {n} publishers (graph v{v})");
                                last_version = Some(v);
                                last_full = Instant::now();
                            }
                            Err(e) => tracing::warn!("trust: {e}"),
                        },
                        _ = stop_requested(&mut shutdown) => return,
                    }
                }
            }
            Err(e) => tracing::warn!("trust: graph version: {e}"),
        }
        if stopped_after(TRUST_POLL, &mut shutdown).await {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_pause_never_zero() {
        let tried = |n| {
            Ok(CrawlReport {
                relays_tried: n,
                ..Default::default()
            })
        };
        let failed = Err(magnetar_core::Error::Store("down".into()));
        let every = Some(Duration::from_secs(900));
        assert_eq!(pause_after(every, &tried(0)), Duration::from_secs(900));
        assert_eq!(pause_after(every, &failed), Duration::from_secs(900));
        assert_eq!(pause_after(None, &tried(12)), CONTINUOUS_PAUSE);
        // Every relay backed off, or the store failing: the pass returns at once.
        assert_eq!(pause_after(None, &tried(0)), CONTINUOUS_IDLE_PAUSE);
        assert_eq!(pause_after(None, &failed), CONTINUOUS_IDLE_PAUSE);
        assert!(CONTINUOUS_PAUSE >= Duration::from_secs(10));
    }

    #[test]
    fn due_gate() {
        assert!(due(None, METADATA_EVERY));
        assert!(!due(Some(Instant::now()), METADATA_EVERY));
        assert!(due(Some(Instant::now()), Duration::ZERO));
    }
}
