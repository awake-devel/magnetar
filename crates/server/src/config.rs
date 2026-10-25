//! Process configuration from CLI flags / environment (ARCHITECTURE.md §9).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

/// Shortest periodic crawl interval; 1..59 s are raised to this (0 is continuous mode).
pub const MIN_CRAWL_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Parser, Debug)]
#[command(
    name = "magnetar-server",
    version,
    about = "Magnetar NIP-35 torrent index server"
)]
pub struct Cli {
    #[command(flatten)]
    pub config: Config,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// API + relay endpoint + crawler, liveness, trust and notifier daemons.
    Serve,
    /// Apply database migrations and exit.
    Migrate,
    /// One crawl pass, then exit.
    CrawlOnce {
        /// Wall-clock budget in seconds.
        #[arg(long, default_value_t = 600)]
        seconds: u64,
    },
    /// Check DB, DNS, TLS (NIP-11 of a seed relay) and UDP (tracker connect).
    Selftest,
}

#[derive(Args, Debug, Clone)]
pub struct Config {
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    pub database_url: Option<String>,
    #[arg(long, env = "MAGNETAR_BIND", default_value = "127.0.0.1:8787")]
    pub bind: SocketAddr,
    #[arg(long, env = "MAGNETAR_API_TOKEN", hide_env_values = true)]
    pub api_token: Option<String>,
    /// Trust root (npub or hex); written to the `trust.root` setting at startup.
    #[arg(long, env = "MAGNETAR_ROOT_NPUB")]
    pub root_npub: Option<String>,
    /// ntfy base URL, used when the `ntfy.url` setting is unset.
    #[arg(long, env = "MAGNETAR_NTFY_URL")]
    pub ntfy_url: Option<String>,
    #[arg(long, env = "MAGNETAR_NTFY_TOPIC", hide_env_values = true)]
    pub ntfy_topic: Option<String>,
    /// Download output directory (default ~/Downloads/magnetar).
    #[arg(long, env = "MAGNETAR_DOWNLOAD_DIR")]
    pub download_dir: Option<PathBuf>,
    /// Start the librqbit download engine with `serve`.
    #[arg(long, env = "MAGNETAR_DOWNLOADS", default_value_t = true, action = clap::ArgAction::Set)]
    pub downloads: bool,
    /// Pause between crawl passes; 0 = continuous (back to back, see `crawl_interval`).
    #[arg(long, env = "MAGNETAR_CRAWL_INTERVAL_SECS", default_value_t = 900)]
    pub crawl_interval_secs: u64,
    /// Relays crawled per pass.
    #[arg(long, env = "MAGNETAR_CRAWL_MAX_RELAYS", default_value_t = 30)]
    pub crawl_max_relays: usize,
    /// Of those, slots for relays with no events yet (newly discovered first).
    #[arg(long, env = "MAGNETAR_CRAWL_EXPLORE_RELAYS", default_value_t = 10)]
    pub crawl_explore_relays: usize,
    /// Comma-separated seed relays (overrides the `crawl.seed_relays` setting).
    #[arg(long, env = "MAGNETAR_SEED_RELAYS", value_delimiter = ',')]
    pub seed_relays: Vec<String>,
    /// NIP-66 relay discovery from monitor events, every 6 h with `serve`.
    #[arg(long, env = "MAGNETAR_NIP66", default_value_t = true, action = clap::ArgAction::Set)]
    pub nip66: bool,
    /// Comma-separated relays holding NIP-66 monitor events (default: built-in list).
    #[arg(long, env = "MAGNETAR_NIP66_MONITORS", value_delimiter = ',')]
    pub nip66_monitors: Vec<String>,
    /// Comma-separated hex pubkeys of trusted NIP-66 monitors (default: built-in list, D36).
    #[arg(long, env = "MAGNETAR_NIP66_TRUSTED", value_delimiter = ',')]
    pub nip66_trusted: Vec<String>,
}

impl Config {
    pub fn database_url(&self) -> anyhow::Result<&str> {
        self.database_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("DATABASE_URL is not set"))
    }

    /// Periodic mode: the pause between passes, at least [`MIN_CRAWL_INTERVAL`]. None means
    /// continuous mode (`MAGNETAR_CRAWL_INTERVAL_SECS=0`), where the crawler loop picks its own
    /// short pause (D32).
    pub fn crawl_interval(&self) -> Option<Duration> {
        (self.crawl_interval_secs > 0)
            .then(|| Duration::from_secs(self.crawl_interval_secs).max(MIN_CRAWL_INTERVAL))
    }

    pub fn seed_relays(&self) -> Vec<String> {
        self.seed_relays
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    fn home() -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn download_dir(&self) -> PathBuf {
        self.download_dir
            .clone()
            .unwrap_or_else(|| Self::home().join("Downloads").join("magnetar"))
    }

    /// `$XDG_STATE_HOME/magnetar` or `~/.local/state/magnetar`.
    pub fn state_dir(&self) -> PathBuf {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| Self::home().join(".local").join("state"))
            .join("magnetar")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cli() {
        let cli = Cli::try_parse_from([
            "magnetar-server",
            "--seed-relays",
            "wss://a, wss://b",
            "--downloads",
            "false",
            "crawl-once",
            "--seconds",
            "30",
        ])
        .unwrap();
        assert!(matches!(cli.command, Command::CrawlOnce { seconds: 30 }));
        assert_eq!(cli.config.seed_relays(), vec!["wss://a", "wss://b"]);
        assert!(!cli.config.downloads);
        assert!(Cli::try_parse_from(["magnetar-server", "bogus"]).is_err());
    }

    #[test]
    fn crawl_interval_modes() {
        let at = |secs: &str| {
            Cli::try_parse_from(["magnetar-server", "--crawl-interval-secs", secs, "serve"])
                .unwrap()
                .config
                .crawl_interval()
        };
        let default = Cli::try_parse_from(["magnetar-server", "serve"]).unwrap();
        assert_eq!(
            default.config.crawl_interval(),
            Some(Duration::from_secs(900))
        );
        assert_eq!(at("0"), None);
        assert_eq!(at("5"), Some(MIN_CRAWL_INTERVAL));
        assert_eq!(at("3600"), Some(Duration::from_secs(3600)));
    }
}
