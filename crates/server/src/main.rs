use std::time::Duration;

use clap::Parser;
use magnetar_server::config::{Cli, Command, DhtMode};
use magnetar_server::{attrs, daemons, dht, enrich, enrich_anime, liveness, selftest, server};
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
