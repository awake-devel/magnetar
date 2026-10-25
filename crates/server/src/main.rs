use std::time::Duration;

use clap::Parser;
use magnetar_server::config::{Cli, Command};
use magnetar_server::{daemons, selftest, server};
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
        Command::Selftest => selftest::run(&cli.config).await,
    }
}
