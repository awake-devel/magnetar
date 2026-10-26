//! `serve`: API + relay endpoint + daemons, with graceful shutdown on SIGINT/SIGTERM.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use magnetar_core::downloads::Downloader;
use tokio::sync::watch;

use crate::api::{self, AppState};
use crate::config::Config;
use crate::liveness::{self, LivenessService};
use crate::search_index::{self, SearchIndex};
use crate::store::PgStore;
use crate::tmdb::{self, Tmdb, TmdbConfig};
use crate::{attrs, daemons, discovery, enrich, enrich_anime, stats};

/// Grace period for daemons after the HTTP server stopped.
const DAEMON_GRACE: Duration = Duration::from_secs(10);

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}

/// Connect and migrate (every entry point runs migrations first).
pub async fn open_store(cfg: &Config) -> anyhow::Result<Arc<PgStore>> {
    let store = PgStore::connect(cfg.database_url()?, cfg.db_pool)
        .await
        .context("connect to Postgres")?;
    store.migrate().await?;
    Ok(Arc::new(store))
}

/// Open (or start building) the search index in the background and attach it to the store, so
/// searches use it once ready (D43); the API listens meanwhile. Disabled or failing to open:
/// searches stay on Postgres.
async fn start_search_index(
    store: &Arc<PgStore>,
    cfg: &Config,
    stop: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let dir = cfg.search_index_dir();
    if !cfg.search_index {
        // Nobody follows the change log now, so an index left there would go stale.
        if search_index::is_index_dir(&dir) {
            tracing::info!("search index off: removing {}", dir.display());
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
        return tokio::spawn(search_index::discard_loop(store.pool().clone(), stop));
    }
    let store = store.clone();
    tokio::spawn(async move {
        match SearchIndex::open_for(store.pool(), dir.clone()).await {
            Ok(idx) => {
                tracing::info!(
                    "search index in {} ({})",
                    dir.display(),
                    if idx.is_ready() { "ready" } else { "to build" }
                );
                store.attach_search_index(idx.clone());
                search_index::index_loop(store.pool().clone(), idx, stop).await;
            }
            Err(e) => tracing::error!("search index disabled: {e}"),
        }
    })
}

pub async fn serve(cfg: Config) -> anyhow::Result<()> {
    let token = cfg
        .api_token
        .clone()
        .filter(|t| !t.trim().is_empty())
        .context("MAGNETAR_API_TOKEN is required for serve")?;
    let store = open_store(&cfg).await?;
    daemons::apply_env(&store, &cfg).await?;

    let downloader = if cfg.downloads {
        match Downloader::new(cfg.download_dir(), cfg.state_dir().join("downloads")).await {
            Ok(d) => {
                tracing::info!("downloads into {}", d.output_dir.display());
                Some(Arc::new(d))
            }
            Err(e) => {
                tracing::error!("download engine disabled: {e}");
                None
            }
        }
    } else {
        None
    };

    let tmdb = match TmdbConfig::from_config(&cfg) {
        Some(c) => {
            tracing::info!(
                "TMDB on: langs {:?}, {} req/s, images in {}",
                c.langs,
                c.rps,
                c.image_dir.display()
            );
            Some(Tmdb::new(c, store.pool().clone())?)
        }
        None => None,
    };

    let (stop_tx, stop_rx) = watch::channel(false);
    let state = AppState::new(store.clone(), &token)
        .with_shutdown(stop_rx.clone())
        .with_downloader(downloader.clone())
        .with_tmdb(tmdb.clone())
        .with_liveness(Arc::new(LivenessService::new(liveness::liveness_config(
            &cfg,
        ))));
    let cfg = Arc::new(cfg);
    let mut daemons = vec![
        tokio::spawn(daemons::crawler_loop(
            store.clone(),
            cfg.clone(),
            state.crawler.clone(),
            stop_rx.clone(),
        )),
        tokio::spawn(liveness::liveness_loop(
            store.clone(),
            state.liveness.clone(),
            cfg.liveness_batch_size(),
            cfg.liveness_interval(),
            stop_rx.clone(),
        )),
        tokio::spawn(daemons::trust_loop(store.clone(), stop_rx.clone())),
        tokio::spawn(discovery::discovery_loop(
            store.clone(),
            cfg.clone(),
            stop_rx.clone(),
        )),
        tokio::spawn(enrich::enrich_loop(
            store.clone(),
            cfg.clone(),
            stop_rx.clone(),
        )),
        tokio::spawn(attrs::backfill_loop(store.pool().clone(), stop_rx.clone())),
        tokio::spawn(stats::stats_loop(store.pool().clone(), stop_rx.clone())),
        tokio::spawn(enrich_anime::anime_loop(
            store.pool().clone(),
            cfg.clone(),
            stop_rx.clone(),
        )),
    ];
    daemons.push(start_search_index(&store, &cfg, stop_rx.clone()).await);
    if let Some(t) = &tmdb {
        daemons.extend(t.spawn_workers(stop_rx.clone()));
        daemons.push(tokio::spawn(tmdb::prefetch_loop(
            t.clone(),
            stop_rx.clone(),
        )));
        daemons.push(tokio::spawn(tmdb::backfill_loop(
            t.clone(),
            stop_rx.clone(),
        )));
    }

    let listener = tokio::net::TcpListener::bind(cfg.bind)
        .await
        .with_context(|| format!("bind {}", cfg.bind))?;
    tracing::info!(
        "magnetar-server {} listening on {}",
        magnetar_core::VERSION,
        cfg.bind
    );
    let mut wait = stop_rx.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("shutting down");
        let _ = stop_tx.send(true);
    });
    axum::serve(listener, api::router(state))
        .with_graceful_shutdown(async move { daemons::stop_requested(&mut wait).await })
        .await?;

    for d in daemons {
        if tokio::time::timeout(DAEMON_GRACE, d).await.is_err() {
            tracing::warn!("a daemon did not stop in time");
        }
    }
    // Downloads are not paused here: librqbit persists them as running and resumes on start.
    drop(downloader);
    store.pool().close().await;
    Ok(())
}
