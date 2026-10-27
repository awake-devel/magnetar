use std::time::Duration;

use clap::Parser;
use magnetar_server::config::{Cli, Command, DhtMode};
use magnetar_server::{
    attrs, daemons, dht, enrich, enrich_anime, liveness, selftest, server, worker,
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs go to stderr so `crawl-once` can print its JSON report on stdout.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    // Two rustls providers are linked (ring + librqbit's aws-lc): pin ring process-wide (D28).
    magnetar_core::http::install_crypto_provider();
    raise_open_files_limit();

    let cli = Cli::parse();
    // D53: the hard blocklist is loaded before any command; a list that does not load stops
    // the process (fail closed).
    dht::screen::install_from_config(&cli.config)?;
    match cli.command {
        Command::Serve => server::serve(cli.config).await,
        Command::Migrate => {
            server::open_store(&cli.config).await?;
            println!("migrations applied");
            Ok(())
        }
        Command::CrawlOnce { seconds } => {
            let store = server::open_store(&cli.config).await?;
            daemons::apply_env(&store, &cli.config).await?;
            let status = Default::default();
            let report = daemons::crawl_pass(
                &store,
                &cli.config,
                &status,
                Duration::from_secs(seconds),
                true,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            daemons::notify_saved_searches(&store, &cli.config).await;
            Ok(())
        }
        Command::Dht {
            seconds,
            mode,
            dry_run,
        } => {
            let mode = mode.unwrap_or(match cli.config.dht {
                DhtMode::Off => DhtMode::Polite,
                m => m,
            });
            let Some(mut opts) = dht::DhtOptions::from_config(&cli.config, mode) else {
                anyhow::bail!("--mode off: nothing to run");
            };
            opts.dry_run = dry_run;
            // A bounded run maps router ports only when asked explicitly (D72): measurement
            // runs must not open the router by default.
            if cli.config.dht_upnp != Some(true) {
                opts.upnp = None;
            }
            let pool = if dry_run {
                None
            } else {
                Some(server::open_store(&cli.config).await?.pool().clone())
            };
            let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
            tokio::spawn(async move {
                server::shutdown_signal().await;
                let _ = stop_tx.send(true);
            });
            let report = dht::run(
                pool,
                opts,
                stop_rx,
                Some(Duration::from_secs(seconds)),
                |_| {},
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Command::Worker { seconds } => worker::run_from_config(&cli.config, seconds).await,
        Command::Selftest => selftest::run(&cli.config).await,
        Command::Enrich { reimport, rematch } => {
            let store = server::open_store(&cli.config).await?;
            let report = enrich::run_once(&store, &cli.config, reimport, rematch).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Command::Liveness { limit } => {
            let store = server::open_store(&cli.config).await?;
            liveness::run_once(&store, &cli.config, limit).await
        }
        Command::Attrs { force } => {
            let store = server::open_store(&cli.config).await?;
            let report = attrs::backfill(store.pool(), force, None).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Command::EnrichAnime { force } => {
            let store = server::open_store(&cli.config).await?;
            let report = enrich_anime::run_once(store.pool(), &cli.config, force).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
    }
}

/// The DHT crawler keeps thousands of TCP connections open (`MAGNETAR_DHT_TCP_MAX`), but most
/// systems start a process with a soft limit of 1024 open files (Docker included). Raise the soft
/// limit to the hard one, as far as 1 Mi; a failure only logs, the server still runs.
#[cfg(unix)]
fn raise_open_files_limit() {
    const WANT: libc::rlim_t = 1 << 20;
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit/setrlimit only read or write the struct passed by pointer.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        tracing::warn!(
            "open files limit: getrlimit failed: {}",
            std::io::Error::last_os_error()
        );
        return;
    }
    let target = lim.rlim_max.min(WANT);
    if lim.rlim_cur >= target {
        return;
    }
    let old = lim.rlim_cur;
    lim.rlim_cur = target;
    // SAFETY: as above.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } != 0 {
        tracing::warn!(
            "open files limit: setrlimit failed: {}",
            std::io::Error::last_os_error()
        );
    } else {
        tracing::info!("open files limit raised from {old} to {target}");
    }
}

#[cfg(not(unix))]
fn raise_open_files_limit() {}
