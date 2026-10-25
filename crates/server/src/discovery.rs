//! Relay discovery loop run by `serve` (D30): NIP-66 monitor events at startup, then every
//! 6 h. New relays land in the relay table with source "nip66"; the crawler's exploration slots
//! pick them up from there.

use std::sync::Arc;
use std::time::Duration;

use magnetar_core::nip66::{self, Nip66Config};
use tokio::sync::watch;

use crate::config::Config;
use crate::daemons::stop_requested;
use crate::store::PgStore;

/// Monitors refresh every 10 min to 24 h, but the relay population moves slowly.
pub const NIP66_INTERVAL: Duration = Duration::from_secs(6 * 3600);

/// Monitor relays from env, else the built-in list.
pub fn nip66_config(cfg: &Config) -> Nip66Config {
    let mut c = Nip66Config::default();
    let monitors: Vec<String> = cfg
        .nip66_monitors
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !monitors.is_empty() {
        c.monitor_relays = monitors;
    }
    c
}

pub async fn discovery_loop(
    store: Arc<PgStore>,
    cfg: Arc<Config>,
    mut shutdown: watch::Receiver<bool>,
) {
    if !cfg.nip66 {
        return;
    }
    let ncfg = nip66_config(&cfg);
    loop {
        tokio::select! {
            res = nip66::discover(&*store, &ncfg) => match res {
                Ok(r) => {
                    tracing::info!(
                        monitors = r.monitors,
                        events = r.events,
                        candidates = r.candidates,
                        inserted = r.inserted,
                        "nip66 discovery done in {} ms",
                        r.duration_ms
                    );
                    for e in &r.errors {
                        tracing::debug!("nip66: {e}");
                    }
                }
                Err(e) => tracing::warn!("nip66 discovery failed: {e}"),
            },
            _ = stop_requested(&mut shutdown) => return,
        }
        tokio::select! {
            _ = tokio::time::sleep(NIP66_INTERVAL) => {}
            _ = stop_requested(&mut shutdown) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::config::Cli;

    #[test]
    fn monitors_from_env_or_defaults() {
        let cli = Cli::try_parse_from(["magnetar-server", "serve"]).unwrap();
        assert!(cli.config.nip66);
        assert_eq!(
            nip66_config(&cli.config).monitor_relays,
            nip66::DEFAULT_MONITOR_RELAYS
        );
        let cli = Cli::try_parse_from([
            "magnetar-server",
            "--nip66",
            "false",
            "--nip66-monitors",
            "wss://a.example.com, ,wss://b.example.com",
            "serve",
        ])
        .unwrap();
        assert!(!cli.config.nip66);
        assert_eq!(
            nip66_config(&cli.config).monitor_relays,
            vec!["wss://a.example.com", "wss://b.example.com"]
        );
    }
}
