//! Every integration test of `magnetar-core` in one binary (D78): one module per former
//! `tests/*.rs` file, so the workspace links one test executable per crate instead of one per
//! file. Run one module with `cargo test -p magnetar-core --test it <module>::`.

mod api_support;
mod support;
mod trust_support;

mod api_client;
mod crawler;
mod dht_bench;
mod dht_fake_network;
mod dht_lookup;
mod dht_shards;
mod liveness;
mod liveness_live;
mod metainfo_fetch;
mod metainfo_librqbit;
mod nip35_fixtures;
mod nip66;
mod notify_ntfy;
mod relay;
mod release_sample;
mod sync_negentropy;
mod sync_up;
mod trust_compute;
mod trust_store;
mod wot_parsers;
