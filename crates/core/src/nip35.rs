//! NIP-35 parser (lenient, D4). Owner: nostr agent. See nostr-nips.md §2.

use crate::model::{Category, Comment, ParsedTorrent, RawEvent, Rejection};

/// Parse a verified kind-2003 event. Err(NoInfohash) when neither x nor btih yields a v1 hash.
pub fn parse_torrent(raw: &RawEvent) -> Result<ParsedTorrent, Rejection> {
    let _ = raw;
    todo!("nip35::parse_torrent")
}

/// Parse a kind 2004 (NIP-10 e tags, any marker position) or kind 1111 (`K`=2003, `E` root).
/// Returns None if no torrent reference can be found.
pub fn parse_comment(raw: &RawEvent) -> Option<Comment> {
    let _ = raw;
    todo!("nip35::parse_comment")
}

/// Category mapping (ARCHITECTURE.md §3.1) from tcat path / t tags.
pub fn category_of(category_path: &[String], hashtags: &[String]) -> Category {
    let _ = (category_path, hashtags);
    todo!("nip35::category_of")
}
