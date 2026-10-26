//! magnetar-core: NIP-35 parsing, validation, crawler, liveness, trust, sync and the Store trait.
//! See ARCHITECTURE.md for the contracts.

pub mod api;
pub mod blocklist;
pub mod classify;
pub mod crawler;
#[cfg(feature = "dht-crawl")]
pub mod dht;
#[cfg(feature = "downloads")]
pub mod downloads;
pub mod enrich;
pub mod enrich_anime;
pub mod error;
pub mod http;
pub mod ingest;
pub mod liveness;
pub mod magnet;
#[cfg(feature = "dht-crawl")]
pub mod metainfo;
pub mod model;
pub mod nip35;
pub mod nip66;
pub mod notify;
pub mod pow;
pub mod query;
pub mod relay;
pub mod relayurl;
pub mod release;
pub mod store;
pub mod sync;
pub mod trust;
pub mod wot;

pub use error::{Error, Result};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Current unix time in seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
