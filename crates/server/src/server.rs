//! `serve`: API + relay endpoint + daemons, with graceful shutdown on SIGINT/SIGTERM.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use magnetar_core::downloads::Downloader;
use tokio::sync::watch;

use crate::api::{self, AppState};
use crate::config::Config;
use crate::daemons;
use crate::store::PgStore;

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
    let store = PgStore::connect(cfg.database_url()?)
        .await
        .context("connect to Postgres")?;
    store.migrate().await?;
    Ok(Arc::new(store))
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

    let (stop_tx, stop_rx) = watch::channel(false);
    let state = AppState::new(store.clone(), &token)
        .with_shutdown(stop_rx.clone())
        .with_downloader(downloader.clone());
    let cfg = Arc::new(cfg);
    let daemons = vec![
        tokio::spawn(daemons::crawler_loop(
            store.clone(),
            cfg.clone(),
            state.crawler.clone(),
            stop_rx.clone(),
        )),
        tokio::spawn(daemons::liveness_loop(
            store.clone(),
            state.liveness.clone(),
            stop_rx.clone(),
        )),
        tokio::spawn(daemons::trust_loop(store.clone(), stop_rx.clone())),
    ];

    let listener = tokio::net::TcpListener::bind(cfg.bind)
        .await
        .with_context(|| format!("bind {}", cfg.bind))?;
    tracing::info!("magnetar-server {} listening on {}", magnetar_core::VERSION, cfg.bind);
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
