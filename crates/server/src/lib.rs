//! magnetar-server library: PgStore, API, relay endpoint, daemons.

pub mod api;
pub mod attrs;
pub mod config;
pub mod crawlmap;
pub mod daemons;
pub mod dht;
pub mod discovery;
pub mod enrich;
pub mod enrich_anime;
pub mod filter;
pub mod fleet;
pub mod geo;
pub mod graphs;
pub mod liveness;
pub mod media_lock;
pub mod relay;
pub mod search;
pub mod search_index;
pub mod selftest;
pub mod server;
pub mod stats;
pub mod store;
pub mod tmdb;
pub mod worker;

pub use api::{AppState, router};
pub use store::PgStore;
