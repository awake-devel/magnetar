//! Native Mainline DHT crawler node (feature `dht-crawl`, server only; docs/DHT.md, D49–D55).
//!
//! Our own KRPC implementation (librqbit-dht can neither send `sample_infohashes` nor report
//! incoming queries, D49): BEP 5 node with BEP 42 ids, BEP 43 read-only mode, BEP 51 sampling,
//! passive capture of incoming `get_peers` / `announce_peer`, and iterative `get_peers` lookups
//! for peer collection. The server (`server::dht`) owns triage, the persisted queue and stats.

pub mod addr;
pub mod engine;
pub mod id;
pub mod krpc;
pub mod limiter;
pub mod lookup;
pub mod peerstore;
pub mod pool;
pub mod routing;

pub use engine::{Dht, EngineConfig, EngineStats, Identity, Mode, QueryError, Sighting, Via};
pub use krpc::{CompactNode, Id};
pub use lookup::{Collected, collect_peers};
