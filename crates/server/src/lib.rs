//! magnetar-server library: PgStore, API, relay endpoint, daemons.

pub mod api;
pub mod config;
pub mod daemons;
pub mod discovery;
pub mod enrich;
pub mod enrich_anime;
pub mod filter;
pub mod relay;
mod search;
pub mod search_index;
pub mod selftest;
pub mod server;
pub mod store;
pub mod tmdb;

pub use api::{AppState, router};
pub use store::PgStore;
