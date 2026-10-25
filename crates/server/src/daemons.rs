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

/// One crawl with status bookkeeping (shared by the loop and `crawl-once`).
pub async fn crawl_pass(
    store: &PgStore,
    cfg: &Config,
    status: &Mutex<CrawlerStatus>,
    budget: Duration,
) -> magnetar_core::Result<CrawlReport> {
    status.lock().unwrap().running = true;
    let ccfg = crawl_config(store, cfg, budget).await;
    let res = crawler::crawl_once(store, &ccfg).await;
    let mut st = status.lock().unwrap();
    st.running = false;
    st.last_run = Some(magnetar_core::now());
    if let Ok(r) = &res {
        st.last_report = Some(r.clone());
    }
    res
}

pub async fn crawler_loop(
    store: Arc<PgStore>,
    cfg: Arc<Config>,
    status: Arc<Mutex<CrawlerStatus>>,
    mut shutdown: watch::Receiver<bool>,
) {
    let budget = CrawlConfig::server_default().budget;
    loop {
        let pass = crawl_pass(&store, &cfg, &status, budget);
        tokio::select! {
            res = pass => match res {
                Ok(r) => {
                    tracing::info!(
                        inserted = r.inserted,
                        duplicates = r.duplicates,
                        relays_ok = r.relays_ok,
                        "crawl done in {} ms",
                        r.duration_ms
                    );
                    notify_saved_searches(&store, &cfg).await;
                }
                Err(e) => tracing::warn!("crawl failed: {e}"),
            },
            _ = stop_requested(&mut shutdown) => return,
        }
        if stopped_after(cfg.crawl_interval(), &mut shutdown).await {
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
