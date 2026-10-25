//! Verify + classify raw events; store them (D5, D7). Owner: nostr agent.

use crate::model::{IngestItem, RawEvent, Rejection};
use crate::store::Store;

/// Reject events dated further than this in the future.
pub const MAX_FUTURE_SECS: i64 = 15 * 60;
/// Reject raw events larger than this (serialized).
pub const MAX_EVENT_BYTES: usize = 1 << 20;
/// Kinds accepted on every ingest path.
pub const ACCEPTED_KINDS: &[u16] = &[0, 3, 1111, 1984, 2003, 2004, 10000, 10002, 30007];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IngestOutcome {
    Inserted,
    Duplicate,
    Rejected(Rejection),
}

/// Size/kind/time checks, id + signature verification, then NIP-35/replaceable classification.
/// Kind 1111 is accepted only with a `K` tag of "2003"; 30007 only with `d` = "2003".
pub fn classify(raw: RawEvent, now: i64) -> Result<IngestItem, Rejection> {
    let _ = (raw, now);
    todo!("ingest::classify")
}

/// classify + store. Comments whose torrent is unknown to the store are rejected (OrphanComment).
pub async fn ingest_into<S: Store + ?Sized>(
    store: &S,
    raw: RawEvent,
    now: i64,
) -> crate::Result<IngestOutcome> {
    let _ = (store, raw, now);
    todo!("ingest::ingest_into")
}
