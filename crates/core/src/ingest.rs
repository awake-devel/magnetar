//! Verify + classify raw events; store them (D5, D7). Owner: nostr agent.

use crate::blocklist::{self, Verdict};
use crate::classify;
use crate::error::Error;
use crate::model::*;
use crate::nip35;
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

/// Size/kind/time checks, id + signature verification, NIP-35/replaceable classification, then
/// the hard illegal-content blocklist on torrents ([`screen_torrent`], D53).
/// Kind 1111 is accepted only with a `K` tag of "2003"; 30007 only with `d` = "2003".
pub fn classify(raw: RawEvent, now: i64) -> Result<IngestItem, Rejection> {
    let item = classify_unscreened(raw, now)?;
    if let IngestItem::Torrent(t, _) = &item
        && screen_torrent(t).is_block()
    {
        return Err(Rejection::Blocked);
    }
    Ok(item)
}

/// The blocklist verdict on a kind 2003 (D53): title and file names with both tiers,
/// description and hashtags with the hard tier only; the context tier applies when the
/// publisher filed it under `porn` or its name/files carry an adult signal (`core::classify`).
/// No list loaded at all → blocked (fail closed). Never logs anything.
pub fn screen_torrent(t: &ParsedTorrent) -> Verdict {
    let Some(list) = blocklist::global() else {
        return Verdict::Block(blocklist::Tier::Hard);
    };
    let mut texts: Vec<&str> = Vec::with_capacity(t.files.len() + 1);
    texts.push(&t.title);
    texts.extend(t.files.iter().map(|f| f.path.as_str()));
    let mut hard_only: Vec<&str> = vec![&t.description];
    hard_only.extend(t.hashtags.iter().map(String::as_str));
    let adult = t.category == Category::Porn || classify::adult_signal(&t.title, &texts[1..]);
    list.check(&texts, &hard_only, adult)
}

/// [`classify`] without the blocklist (`ingest_into` screens itself so it can record the
/// blocked hash).
pub fn classify_unscreened(raw: RawEvent, now: i64) -> Result<IngestItem, Rejection> {
    // Cheap checks first; verification is the expensive part.
    if !ACCEPTED_KINDS.contains(&raw.kind) {
        return Err(Rejection::UnsupportedKind);
    }
    if raw.created_at > now.saturating_add(MAX_FUTURE_SECS) {
        return Err(Rejection::FutureDated);
    }
    if serialized_len(&raw) > MAX_EVENT_BYTES {
        return Err(Rejection::TooLarge);
    }
    match raw.kind {
        KIND_COMMENT if !has_tag(&raw, "K", "2003") => return Err(Rejection::UnsupportedKind),
        KIND_KIND_MUTE_SET if !has_tag(&raw, "d", "2003") => {
            return Err(Rejection::UnsupportedKind);
        }
        _ => {}
    }
    // Stored ids/pubkeys must be canonical lowercase hex (nostr's parser also takes uppercase).
    if !is_hex32(&raw.id) {
        return Err(Rejection::BadId);
    }
    if !is_hex32(&raw.pubkey) {
        return Err(Rejection::BadJson);
    }
    if raw.sig.len() != 128
        || !raw
            .sig
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(Rejection::BadSig);
    }
    raw.verify().map_err(|e| match e {
        Error::Rejected(r) => r,
        _ => Rejection::BadJson,
    })?;

    Ok(match raw.kind {
        KIND_TORRENT => IngestItem::Torrent(Box::new(nip35::parse_torrent(&raw)?), raw),
        KIND_TORRENT_COMMENT | KIND_COMMENT => match nip35::parse_comment(&raw) {
            Some(c) => IngestItem::Comment(c, raw),
            None => return Err(Rejection::OrphanComment),
        },
        KIND_PROFILE => IngestItem::Profile(raw),
        KIND_CONTACTS => IngestItem::ContactList(raw),
        KIND_MUTE_LIST | KIND_KIND_MUTE_SET => IngestItem::MuteList(raw),
        KIND_REPORT => IngestItem::Report(raw),
        KIND_RELAY_LIST => IngestItem::RelayList(raw),
        _ => return Err(Rejection::UnsupportedKind),
    })
}

/// classify + store. Comments whose torrent is unknown to the store are rejected (OrphanComment).
pub async fn ingest_into<S: Store + ?Sized>(
    store: &S,
    raw: RawEvent,
    now: i64,
) -> crate::Result<IngestOutcome> {
    let item = match classify_unscreened(raw, now) {
        Ok(item) => item,
        Err(r) => return Ok(IngestOutcome::Rejected(r)),
    };
    let inserted = match &item {
        IngestItem::Torrent(t, raw) => {
            if store.is_blocked(&t.infohash, &t.event_id).await? {
                return Ok(IngestOutcome::Rejected(Rejection::Blocked));
            }
            if screen_torrent(t).is_block() {
                let version = blocklist::global().map_or(0, |b| b.version());
                store.put_blocked(&t.infohash, &t.event_id, version).await?;
                return Ok(IngestOutcome::Rejected(Rejection::Blocked));
            }
            store.put_torrent(t, raw).await?
        }
        IngestItem::Comment(c, raw) => {
            if !store.known_torrent_event(&c.torrent_event_id).await? {
                return Ok(IngestOutcome::Rejected(Rejection::OrphanComment));
            }
            store.put_comment(c, raw).await?
        }
        IngestItem::Report(raw) => store.put_report(raw).await?,
        IngestItem::ContactList(raw)
        | IngestItem::MuteList(raw)
        | IngestItem::RelayList(raw)
        | IngestItem::Profile(raw) => store.put_replaceable(raw).await?,
    };
    Ok(if inserted {
        IngestOutcome::Inserted
    } else {
        IngestOutcome::Duplicate
    })
}

fn has_tag(raw: &RawEvent, name: &str, value: &str) -> bool {
    raw.tags.iter().any(|t| {
        t.first().map(String::as_str) == Some(name) && t.get(1).map(|v| v.trim()) == Some(value)
    })
}

/// Byte length of the NIP-01 wire JSON, without allocating it.
fn serialized_len(raw: &RawEvent) -> usize {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut c = Counter(0);
    match serde_json::to_writer(&mut c, raw) {
        Ok(()) => c.0,
        Err(_) => usize::MAX,
    }
}
