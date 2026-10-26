//! BEP 9 metadata fetch for the DHT crawler (docs/DHT.md §4, D56; feature `dht-crawl`): BEP 3
//! handshake, BEP 10 extended handshake, `ut_metadata` blocks over TCP, verification of the raw
//! bytes against the infohash (v1, hybrid, v2-only) and the info dict summary.
//!
//! Written from the BEP texts; no code from bitmagnet/magnetico (AGPL) or anacrolix (MPL).

pub mod fetch;
pub mod info;
pub mod wire;

pub use fetch::{
    FetchConfig, FetchFailure, FetchStats, Fetched, Fetcher, Identity, Outcome, PeerError,
    verify_and_parse,
};
pub use info::{InfoError, InfoFile, InfoSummary, parse_info};
