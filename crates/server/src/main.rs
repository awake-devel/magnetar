use std::time::Duration;

use clap::Parser;
use magnetar_server::config::{Cli, Command};
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
    // D112: our own addresses are refused like private ones; a bad entry stops the process.
    magnetar_core::netaddr::OWN
        .configure(magnetar_core::netaddr::parse_nets(&cli.config.own_addrs)?);
    // D150: one destination budget per process, for every subcommand that reaches the network
    // (serve, worker, crawl-once, liveness, enrich, enrich-anime…); serve and worker reuse it.
    magnetar_server::netbudget::install(&cli.config);
    // Settings that do nothing on this subcommand (D156: MAGNETAR_DHT on `serve`; D117: header
    // probes anywhere but on a worker; worker settings on the wrong side): said, never fatal.
    for w in cli.config.misplaced(&cli.command) {
        tracing::warn!("{w}");
    }
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
        Command::Worker { seconds } => match worker::run_from_config(&cli.config, seconds).await {
            Err(e) if e.downcast_ref::<worker::Fatal>().is_some() => {
                eprintln!("Error: {e}");
                std::process::exit(worker::FATAL_EXIT);
            }
            r => r,
        },
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

/// A worker's DHT crawler keeps thousands of TCP connections open (`MAGNETAR_DHT_TCP_MAX`), but most
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
