//! Shared test helpers: fixtures, event signing, an in-memory Store and a fake relay.
#![allow(dead_code)]

pub mod fake_relay;
pub mod store;

use magnetar_core::model::RawEvent;
use nostr::prelude::*;

pub fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/research/fixtures")
        .join(name)
}

pub fn load_jsonl(name: &str) -> Vec<RawEvent> {
    std::fs::read_to_string(fixture_path(name))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| RawEvent::from_json(l).unwrap())
        .collect()
}

pub fn torrents_fixture() -> Vec<RawEvent> {
    load_jsonl("kind2003_sample.jsonl")
}

pub fn comments_fixture() -> Vec<RawEvent> {
    load_jsonl("kind2004_sample.jsonl")
}

/// Fixture event whose id starts with `prefix`.
pub fn fixture(prefix: &str) -> RawEvent {
    let mut found: Vec<RawEvent> = torrents_fixture()
        .into_iter()
        .chain(comments_fixture())
        .filter(|e| e.id.starts_with(prefix))
        .collect();
    assert_eq!(found.len(), 1, "fixture prefix {prefix} must be unique");
    found.pop().unwrap()
}

/// Sign an event with arbitrary tags and timestamp.
pub fn sign(keys: &Keys, kind: u16, created_at: i64, tags: &[&[&str]], content: &str) -> RawEvent {
    let tags: Vec<Tag> = tags
        .iter()
        .map(|t| Tag::parse(t.iter().copied()).unwrap())
        .collect();
    let ev = EventBuilder::new(Kind::from_u16(kind), content)
        .tags(tags)
        .custom_created_at(Timestamp::from(created_at as u64))
        .finalize(keys)
        .unwrap();
    RawEvent::from_json(&ev.as_json()).unwrap()
}

/// A valid kind 2003 with a deterministic infohash derived from `n`.
pub fn torrent(keys: &Keys, n: u32, created_at: i64) -> RawEvent {
    let ih = hex::encode(
        sha1_smol::Sha1::from(format!("torrent-{n}"))
            .digest()
            .bytes(),
    );
    let title = format!("Generated torrent {n}");
    sign(
        keys,
        2003,
        created_at,
        &[
            &["title", &title],
            &["x", &ih],
            &["file", &format!("gen/{n}.mkv"), "1000"],
            &["tracker", "udp://tracker.opentrackr.org:1337/announce"],
            &["i", "tcat:video,movie"],
        ],
        "generated",
    )
}

pub fn now() -> i64 {
    magnetar_core::now()
}
