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
    /// Refresh the IMDb datasets if due, link every pending torrent, print a JSON report.
    Enrich {
        /// Re-import the datasets even if unchanged.
        #[arg(long)]
        reimport: bool,
        /// Forget every non-manual link check first (full rematch).
        #[arg(long)]
        rematch: bool,
    },
    /// Import the anime datasets when due (or `--force`), match anime torrents, print a report.
    EnrichAnime {
        /// Re-download and re-import even if the datasets were checked recently.
        #[arg(long)]
        force: bool,
    },
    /// Check up to `limit` due infohashes now (seed checks, D45), batch by batch with the
    /// configured rates, and print a JSON report per batch.
    Liveness {
        #[arg(long, default_value_t = 2000)]
        limit: usize,
    },
    /// Run the DHT crawler for a bounded time and print a JSON report (docs/DHT.md §8, D58).
    Dht {
        /// Wall-clock budget in seconds.
        #[arg(long, default_value_t = 300)]
        seconds: u64,
        /// Mode for this run (default: `MAGNETAR_DHT`, or `polite` when that is `off`).
        #[arg(long, value_enum)]
        mode: Option<DhtMode>,
        /// No database at all: dedupe in memory only, nothing written (live tests).
        #[arg(long)]
        dry_run: bool,
    },
    /// Parse the release attributes (D44) of every torrent without current ones, print a report.
    Attrs {
        /// Re-parse every torrent, also those already parsed by this version.
        #[arg(long)]
        force: bool,
    },
}

/// `MAGNETAR_DHT` (D49): off by default, crawling exposes the host's IP in swarms.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DhtMode {
    #[default]
    Off,
    /// BEP 51 sampling only, read-only node (BEP 43), one virtual node.
    Polite,
    /// Sampling + passive capture as a full BEP 5 node on open UDP ports.
    Aggressive,
}

#[derive(Args, Debug, Clone)]
pub struct Config {
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    pub database_url: Option<String>,
    /// Maximum pooled Postgres connections (sqlx).
    #[arg(
        long,
        env = "MAGNETAR_DB_POOL",
        default_value_t = crate::store::DEFAULT_MAX_CONNECTIONS,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub db_pool: u32,
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
    /// Offline IMDb identification from the non-commercial datasets (D40).
    #[arg(long, env = "MAGNETAR_IMDB", default_value_t = true, action = clap::ArgAction::Set)]
    pub imdb: bool,
    /// Also import title.akas (~450 MB download) for localised titles.
    #[arg(long, env = "MAGNETAR_IMDB_AKAS", default_value_t = false, action = clap::ArgAction::Set)]
    pub imdb_akas: bool,
    /// Where dataset downloads live (default <state dir>/datasets).
    #[arg(long, env = "MAGNETAR_DATASET_DIR")]
    pub dataset_dir: Option<PathBuf>,
    /// Conditional re-download + re-import of the IMDb datasets every N hours.
    #[arg(long, env = "MAGNETAR_IMDB_REFRESH_HOURS", default_value_t = 168)]
    pub imdb_refresh_hours: u64,
    /// Offline anime identification from anime-offline-database + Fribb anime-lists (D41).
    #[arg(long, env = "MAGNETAR_ANIME_DB", default_value_t = true, action = clap::ArgAction::Set)]
    pub anime_db: bool,
    /// anime-offline-database JSONL source: URL or local file, `.zst` is decompressed
    /// (default: the latest cedya77 weekly release; a `…/download/<tag>/…` URL pins one).
    #[arg(long, env = "MAGNETAR_ANIME_AOD_URL")]
    pub anime_aod_url: Option<String>,
    /// Fribb `anime-list-full.json` source: URL or local file (default: master on GitHub).
    #[arg(long, env = "MAGNETAR_ANIME_FRIBB_URL")]
    pub anime_fribb_url: Option<String>,
    /// TMDB API read access token (or v3 key) for posters/overviews; unset = TMDB off (D42).
    #[arg(long, env = "MAGNETAR_TMDB_TOKEN", hide_env_values = true)]
    pub tmdb_token: Option<String>,
    /// Comma-separated TMDB languages, preferred first.
    #[arg(
        long,
        env = "MAGNETAR_TMDB_LANGS",
        value_delimiter = ',',
        default_value = "en-US,fr-FR"
    )]
    pub tmdb_langs: Vec<String>,
    /// Poster cache (default `$XDG_CACHE_HOME/magnetar/images` or ~/.cache/magnetar/images).
    #[arg(long, env = "MAGNETAR_IMAGE_CACHE_DIR")]
    pub image_cache_dir: Option<PathBuf>,
    /// TMDB requests per second (token bucket, bursts of up to 5).
    #[arg(long, env = "MAGNETAR_TMDB_RPS", default_value_t = 20)]
    pub tmdb_rps: u32,
    /// Background TMDB prefetch of recently matched media per hour; 0 = only on detail views.
    #[arg(long, env = "MAGNETAR_TMDB_PREFETCH_PER_HOUR", default_value_t = 60)]
    pub tmdb_prefetch_per_hour: u32,
    /// Embedded Tantivy search index (D43); false = every search runs in Postgres.
    #[arg(long, env = "MAGNETAR_SEARCH_INDEX", default_value_t = true, action = clap::ArgAction::Set)]
    pub search_index: bool,
    /// Where the search index lives (default <state dir>/search-index).
    #[arg(long, env = "MAGNETAR_SEARCH_INDEX_DIR")]
    pub search_index_dir: Option<PathBuf>,
    /// Infohashes per seed-check batch (D45).
    #[arg(
        long,
        env = "MAGNETAR_LIVENESS_BATCH",
        default_value_t = 2000,
        value_parser = clap::value_parser!(u64).range(1..=50_000)
    )]
    pub liveness_batch: u64,
    /// Pause after a batch that emptied the queue (back to back while a full batch is due).
    #[arg(long, env = "MAGNETAR_LIVENESS_INTERVAL_SECS", default_value_t = 60)]
    pub liveness_interval_secs: u64,
    /// Scrape requests per second per tracker (74 hashes per UDP packet, 50 per HTTP GET).
    #[arg(long, env = "MAGNETAR_LIVENESS_TRACKER_RPS", default_value_t = 1.0)]
    pub liveness_tracker_rps: f64,
    /// Trackers scraped at the same time.
    #[arg(
        long,
        env = "MAGNETAR_LIVENESS_TRACKER_CONCURRENCY",
        default_value_t = 16
    )]
    pub liveness_tracker_concurrency: usize,
    /// DHT fallback for infohashes no tracker answered.
    #[arg(long, env = "MAGNETAR_LIVENESS_DHT", default_value_t = true, action = clap::ArgAction::Set)]
    pub liveness_dht: bool,
    /// DHT walks at the same time.
    #[arg(long, env = "MAGNETAR_LIVENESS_DHT_CONCURRENCY", default_value_t = 32)]
    pub liveness_dht_concurrency: usize,
    /// Native DHT crawler (docs/DHT.md): off, polite or aggressive.
    #[arg(long, env = "MAGNETAR_DHT", value_enum, default_value = "off")]
    pub dht: DhtMode,
    /// First UDP port; virtual node i listens on PORT + i (aggressive; polite is ephemeral).
    #[arg(long, env = "MAGNETAR_DHT_PORT", default_value_t = 6891, value_parser = clap::value_parser!(u16).range(1..=65527))]
    pub dht_port: u16,
    /// Virtual nodes, 1–8 (default 4 aggressive, 1 polite).
    #[arg(long, env = "MAGNETAR_DHT_NODES", value_parser = clap::value_parser!(u8).range(1..=8))]
    pub dht_nodes: Option<u8>,
    /// Outbound KRPC queries per second, all kinds (default 200 aggressive, 50 polite).
    #[arg(long, env = "MAGNETAR_DHT_QPS", value_parser = clap::value_parser!(u32).range(1..=5000))]
    pub dht_qps: Option<u32>,
    /// Crawl pool size in nodes (default 1000000 aggressive, 200000 polite).
    #[arg(long, env = "MAGNETAR_DHT_POOL", value_parser = clap::value_parser!(u64).range(1000..=20_000_000))]
    pub dht_pool: Option<u64>,
    /// Also queue infohashes already known from Nostr (to verify their file lists).
    #[arg(long, env = "MAGNETAR_DHT_FETCH_KNOWN", default_value_t = false, action = clap::ArgAction::Set)]
    pub dht_fetch_known: bool,
    /// Peer collections (get_peers to the sampling node, then a lookup) running at once
    /// (default 64 aggressive, 8 polite).
    #[arg(long, env = "MAGNETAR_DHT_PEER_LOOKUPS", value_parser = clap::value_parser!(u32).range(1..=8192))]
    pub dht_peer_lookups: Option<u32>,
    /// Soft per-query timeout of peer lookups, ms (D93): a query unanswered by then no longer
    /// holds up the lookup (the next node is asked), a late answer is still used until 4 s.
    #[arg(long, env = "MAGNETAR_DHT_LOOKUP_TIMEOUT_MS", default_value_t = 4000, value_parser = clap::value_parser!(u32).range(100..=4000))]
    pub dht_lookup_timeout_ms: u32,
    /// Peers tried per infohash by the metadata fetcher; a lookup stops once it knows this many.
    #[arg(long, env = "MAGNETAR_DHT_PEERS_PER_HASH", default_value_t = 24, value_parser = clap::value_parser!(u32).range(1..=64))]
    pub dht_peers_per_hash: u32,
    /// BEP 9 metadata fetches in flight (default 128 aggressive, 16 polite).
    #[arg(long, env = "MAGNETAR_DHT_FETCH_CONCURRENCY", value_parser = clap::value_parser!(u32).range(1..=2048))]
    pub dht_fetch_concurrency: Option<u32>,
    /// Largest info dict fetched, in bytes (BEP 9 sets no limit).
    #[arg(long, env = "MAGNETAR_DHT_METADATA_MAX", default_value_t = 16 * 1024 * 1024, value_parser = clap::value_parser!(u64).range(16_384..=67_108_864))]
    pub dht_metadata_max: u64,
    /// New TCP connection attempts per second for metadata fetches (router benchmark: 200
    /// safe, 400 breaks the user's router; docs/research/router-bench.md).
    #[arg(long, env = "MAGNETAR_DHT_TCP_RATE", default_value_t = 150, value_parser = clap::value_parser!(u32).range(1..=2000))]
    pub dht_tcp_rate: u32,
    /// TCP connections open at once for metadata fetches (benchmark: ~440 held).
    #[arg(long, env = "MAGNETAR_DHT_TCP_MAX", default_value_t = 350, value_parser = clap::value_parser!(u32).range(1..=10_000))]
    pub dht_tcp_max: u32,
    /// Router health probe: TCP-connect targets, comma-separated, or `off`.
    #[arg(
        long,
        env = "MAGNETAR_DHT_PROBE",
        default_value = "1.1.1.1:443,9.9.9.9:443"
    )]
    pub dht_probe: String,
    /// Map the crawler's UDP ports on the router with UPnP IGD (D57, D72). Default: on in
    /// aggressive mode, off in polite mode (ephemeral port, nothing to map).
    #[arg(long, env = "MAGNETAR_DHT_UPNP", action = clap::ArgAction::Set)]
    pub dht_upnp: Option<bool>,
    /// Gateway for a unicast SSDP M-SEARCH, `host[:port]` (port 1900 by default). Default: the
    /// default gateway of /proc/net/route. Needed in Docker (the container's gateway is the
    /// bridge).
    #[arg(long, env = "MAGNETAR_DHT_UPNP_GATEWAY")]
    pub dht_upnp_gateway: Option<String>,
    /// LAN address the router maps the ports to. Default: the address the connection to the
    /// gateway leaves from. Required in Docker (the container only sees its bridge address).
    #[arg(long, env = "MAGNETAR_DHT_UPNP_LOCAL_IP")]
    pub dht_upnp_local_ip: Option<std::net::Ipv4Addr>,
    /// Extra hard-blocklist terms (D53): plain text, one per line, `context:` prefix for the
    /// context tier. Additive only; a file that does not load stops the server.
    #[arg(long, env = "MAGNETAR_BLOCKLIST_FILE")]
    pub blocklist_file: Option<std::path::PathBuf>,
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

    pub fn liveness_batch_size(&self) -> usize {
        self.liveness_batch as usize
    }

    /// Idle pause of the seed checker, at least 5 s.
    pub fn liveness_interval(&self) -> Duration {
        Duration::from_secs(self.liveness_interval_secs.max(5))
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

    /// `MAGNETAR_DATASET_DIR`, else `<state dir>/datasets` (D40).
    pub fn dataset_dir(&self) -> PathBuf {
        self.dataset_dir
            .clone()
            .unwrap_or_else(|| self.state_dir().join("datasets"))
    }

    /// `MAGNETAR_SEARCH_INDEX_DIR`, else `<state dir>/search-index` (D43).
    pub fn search_index_dir(&self) -> PathBuf {
        self.search_index_dir
            .clone()
            .unwrap_or_else(|| self.state_dir().join("search-index"))
    }

    pub fn image_cache_dir(&self) -> PathBuf {
        self.image_cache_dir.clone().unwrap_or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| Self::home().join(".cache"))
                .join("magnetar")
                .join("images")
        })
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
        assert_eq!(cli.config.db_pool, crate::store::DEFAULT_MAX_CONNECTIONS);
        assert!(Cli::try_parse_from(["magnetar-server", "--db-pool", "0", "serve"]).is_err());
        assert!(Cli::try_parse_from(["magnetar-server", "bogus"]).is_err());
    }

    #[test]
    fn parses_dht_options() {
        let cli = Cli::try_parse_from([
            "magnetar-server",
            "--dht",
            "aggressive",
            "--dht-nodes",
            "2",
            "dht",
            "--seconds",
            "60",
            "--mode",
            "polite",
            "--dry-run",
        ])
        .unwrap();
        assert_eq!(cli.config.dht, DhtMode::Aggressive);
        assert_eq!(cli.config.dht_nodes, Some(2));
        assert!(matches!(
            cli.command,
            Command::Dht {
                seconds: 60,
                mode: Some(DhtMode::Polite),
                dry_run: true
            }
        ));
        let default = Cli::try_parse_from(["magnetar-server", "serve"]).unwrap();
        assert_eq!(default.config.dht, DhtMode::Off);
        for bad in [
            ["--dht-nodes", "9"],
            ["--dht-nodes", "0"],
            ["--dht", "loud"],
        ] {
            let mut args = vec!["magnetar-server"];
            args.extend(bad);
            args.push("serve");
            assert!(Cli::try_parse_from(args).is_err(), "{bad:?}");
        }
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
