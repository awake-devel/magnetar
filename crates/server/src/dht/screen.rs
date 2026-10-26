//! The content screen every fetched torrent passes before anything with its name is written
//! (docs/DHT.md §7, D53): the hard illegal-content blocklist. Stage 3 builds the real matcher
//! (`core::blocklist`); stage 2 only defines the seam and **fails closed** without one: with no
//! screen installed, verified metadata is dropped from memory and the hash is put back in the
//! queue (`dht_seen` `queued`, `last_error = 'unscreened'`, due again after
//! [`UNSCREENED_RETRY_SECS`]), so no name reaches the database before the blocklist exists.

use std::fmt::Debug;

/// When an unscreened hash is due again (the blocklist may be installed by then).
pub const UNSCREENED_RETRY_SECS: i64 = 6 * 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Illegal content: only the infohash is kept (`blocked_infohashes`), never a name.
    Block,
}

/// A content screen over every name and path variant of an info dict
/// ([`magnetar_core::metainfo::InfoSummary::all_names`], padding paths included).
///
/// Implementations must not log, store or return the names they read.
pub trait Screen: Send + Sync + Debug {
    fn verdict(&self, names: &[String]) -> Verdict;
    /// Recorded in `blocked_infohashes.rule_version`.
    fn rule_version(&self) -> i16;
}
