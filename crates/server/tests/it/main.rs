//! Every integration test of `magnetar-server` in one binary (D78): one module per former
//! `tests/*.rs` file, so the workspace links one test executable per crate instead of one per
//! file. Run one module with `cargo test -p magnetar-server --test it <module>::`.

mod common;
/// The core tests' loopback BitTorrent peer (header probes, D117), shared as source.
#[path = "../../../core/tests/it/support/fake_seeder.rs"]
#[allow(dead_code)]
mod fake_seeder;

mod api;
mod cli;
mod crawl_map;
mod dht_classify_pg;
mod dht_fetch_pg;
mod dht_pg;
mod dht_search_pg;
mod dht_upnp;
mod enrich_anime;
mod enrich_pg;
mod filters;
mod liveness_pg;
mod notices;
mod probe_backfill_pg;
mod probe_pg;
mod probe_search;
mod search_bench;
mod search_index;
mod stats;
mod stats_bench;
mod store_pg;
mod tmdb;
mod tmdb_imdb;
mod workers;
