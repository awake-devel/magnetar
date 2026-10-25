//! SqliteStore semantics (ARCHITECTURE.md §4–§5, §7).

mod common;

use common::*;
use magnetar_core::model::*;
use magnetar_core::query::Sort;
use magnetar_core::store::{Store, keys};
use magnetar_core::sync::SyncSource;
use magnetar_ffi::SqliteStore;
use magnetar_ffi::store::{backoff_secs, event_trust};

fn store() -> SqliteStore {
    SqliteStore::open_in_memory().unwrap()
}

#[test]
fn fts_matches_release_names() {
    let s = store();
    torrent(&ih(1), "Big.Buck.Bunny.2008.1080p.x264-GRP", &pk(1), 100).put(&s);
    torrent(&ih(2), "Ubuntu 24.04 desktop amd64", &pk(1), 200).put(&s);
    torrent(&ih(3), "Élégie pour piano", &pk(1), 300).put(&s);

    assert_eq!(
        titles(&s, &q("bunny 1080p")),
        ["Big.Buck.Bunny.2008.1080p.x264-GRP"]
    );
    assert_eq!(
        titles(&s, &q("bun")),
        ["Big.Buck.Bunny.2008.1080p.x264-GRP"]
    );
    assert_eq!(
        titles(&s, &q("x264 grp")),
        ["Big.Buck.Bunny.2008.1080p.x264-GRP"]
    );
    assert_eq!(titles(&s, &q("ubuntu")), ["Ubuntu 24.04 desktop amd64"]);
    // remove_diacritics 2
    assert_eq!(titles(&s, &q("elegie")), ["Élégie pour piano"]);
    // Raw user syntax is sanitized, never an FTS5 syntax error.
    assert!(titles(&s, &q("\"unterminated")).is_empty());
    assert!(titles(&s, &q("NOT AND OR (")).is_empty());
    assert!(titles(&s, &q("nothing-here")).is_empty());
}

#[test]
fn fts_indexes_files_and_description_with_title_weighted_highest() {
    let s = store();
    torrent(&ih(1), "Some album", &pk(1), 100)
        .with(|t| {
            t.files = vec![TorrentFile {
                path: "Disc1/rabbit_song.flac".into(),
                size: Some(5),
            }]
        })
        .put(&s);
    torrent(&ih(2), "Another album", &pk(1), 200)
        .with(|t| t.description = "a song about a rabbit".into())
        .put(&s);
    torrent(&ih(3), "Rabbit stories", &pk(1), 50).put(&s);

    let page = s.search_sync(&q("rabbit"), 0).unwrap();
    let got: Vec<_> = page.hits.iter().map(|h| h.title.as_str()).collect();
    assert_eq!(got, ["Rabbit stories", "Some album", "Another album"]);
    assert!(page.hits.windows(2).all(|w| w[0].rank >= w[1].rank));
    assert!(page.hits[0].rank > 0.0, "rank is -bm25: higher is better");
    assert_eq!(page.total, 3);
    assert_eq!(page.source, "local");

    // Ascending relevance flips it.
    let mut asc = q("rabbit");
    asc.descending = false;
    assert_eq!(titles(&s, &asc)[0], "Another album");
}

#[test]
fn groups_by_infohash_with_canonical_event() {
    let s = store();
    let a = torrent(&ih(7), "Early low trust", &pk(1), 100);
    let b = torrent(&ih(7), "Later high trust", &pk(2), 200);
    let c = torrent(&ih(7), "Latest high trust", &pk(3), 300);
    for t in [&a, &b, &c] {
        assert!(t.put(&s));
    }
    // Same infohash, different publishers: one hit, canonical = earliest when trust ties.
    let page = s.search_sync(&q(""), 0).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.hits[0].title, "Early low trust");
    assert_eq!(page.hits[0].publisher_count, 3);

    for (p, score) in [(pk(1), 10), (pk(2), 60), (pk(3), 60)] {
        s.put_trust_sync(&score_of(&p, score)).unwrap();
    }
    let page = s.search_sync(&q("trust"), 0).unwrap();
    assert_eq!(page.hits.len(), 1);
    assert_eq!(
        page.hits[0].event_id,
        b.id(),
        "highest trust, then earliest"
    );
    assert_eq!(page.hits[0].trust, 60);

    // Events below min_trust do not count, and do not become canonical.
    let mut strict = q("");
    strict.min_trust = Some(50);
    let page = s.search_sync(&strict, 0).unwrap();
    assert_eq!(page.hits[0].publisher_count, 2);

    // Blocked publishers are always excluded.
    s.set_override_sync(&pk(2), Some(OverrideAction::Block))
        .unwrap();
    s.set_override_sync(&pk(3), Some(OverrideAction::Block))
        .unwrap();
    let page = s.search_sync(&q(""), 0).unwrap();
    assert_eq!(page.hits[0].event_id, a.id());
    assert_eq!(page.hits[0].publisher_count, 1);
    s.set_override_sync(&pk(1), Some(OverrideAction::Block))
        .unwrap();
    assert_eq!(s.search_sync(&q(""), 0).unwrap().total, 0);
    s.set_override_sync(&pk(1), None).unwrap();
    assert_eq!(s.search_sync(&q(""), 0).unwrap().total, 1);
}

fn score_of(pubkey: &str, score: u8) -> TrustScore {
    TrustScore {
        pubkey: pubkey.into(),
        score,
        factors: vec![TrustFactor {
            code: "FOLLOW_D1".into(),
            delta: score as i32,
            detail: "followed".into(),
            evidence: vec![],
        }],
        root: None,
        computed_at: 1,
        graph_version: 0,
    }
}

#[test]
fn trust_threshold_defaults_to_setting() {
    let s = store();
    torrent(&ih(1), "Low", &pk(1), 100).put(&s);
    torrent(&ih(2), "High", &pk(2), 100).put(&s);
    s.put_trust_sync(&score_of(&pk(1), 10)).unwrap();
    s.put_trust_sync(&score_of(&pk(2), 30)).unwrap();
    let mut query = q("");
    query.min_trust = None;
    // Default threshold 20.
    assert_eq!(titles(&s, &query), ["High"]);
    s.set_setting_sync(keys::TRUST_THRESHOLD, "5").unwrap();
    assert_eq!(titles(&s, &query).len(), 2);
    s.set_setting_sync(keys::TRUST_THRESHOLD, "50").unwrap();
    assert!(titles(&s, &query).is_empty());
}

#[test]
fn denormalised_trust_matches_event_trust() {
    let s = store();
    let mut ids = Vec::new();
    for bits in [0u8, 8, 12, 16, 24, 30] {
        let t = torrent(&ih(100 + bits as u64), "pow", &pk(1), 100).with(|t| t.pow_bits = bits);
        t.put(&s);
        ids.push((ih(100 + bits as u64), bits));
    }
    // Unscored: PoW bonus only.
    for (h, bits) in &ids {
        assert_eq!(trust_of(&s, h), event_trust(None, *bits));
    }
    for score in [0u8, 1, 20, 90, 99, 100] {
        s.put_trust_sync(&score_of(&pk(1), score)).unwrap();
        for (h, bits) in &ids {
            let want = event_trust(Some(score), *bits);
            assert_eq!(trust_of(&s, h), want, "score {score} bits {bits}");
            if score == 0 || score == 100 {
                assert_eq!(want, score, "0/100 ignore PoW");
            }
        }
    }
    assert_eq!(event_trust(Some(90), 24), 100);
    assert_eq!(event_trust(Some(20), 16), 32);
    // A torrent inserted after the score uses it straight away.
    s.put_trust_sync(&score_of(&pk(1), 40)).unwrap();
    torrent(&ih(999), "late", &pk(1), 100)
        .with(|t| t.pow_bits = 24)
        .put(&s);
    assert_eq!(trust_of(&s, &ih(999)), 65);
    assert_eq!(s.get_trust_sync(&pk(1)).unwrap().unwrap().score, 40);
}

fn trust_of(s: &SqliteStore, infohash: &str) -> u8 {
    let page = s.search_sync(&q(""), 0).unwrap();
    page.hits
        .into_iter()
        .find(|h| h.infohash == infohash)
        .unwrap()
        .trust
}

#[test]
fn filters() {
    let s = store();
    let mk = |n: u64, title: &str, cat: Category, size: Option<u64>, at: i64| {
        torrent(&ih(n), title, &pk(1), at)
            .with(|t| {
                t.category = cat;
                t.total_size = size;
            })
            .put(&s);
    };
    mk(1, "small video", Category::Video, Some(100), 1000);
    mk(2, "big video", Category::Video, Some(10_000), 2000);
    mk(3, "audio", Category::Audio, Some(500), 3000);
    mk(4, "unknown size", Category::Book, None, 4000);

    let mut f = q("");
    f.categories = vec![Category::Video, Category::Book];
    assert_eq!(titles(&s, &f), ["unknown size", "big video", "small video"]);

    let mut f = q("");
    f.min_size = Some(400);
    assert_eq!(titles(&s, &f), ["audio", "big video"]);
    f.max_size = Some(1000);
    assert_eq!(titles(&s, &f), ["audio"]);

    let mut f = q("");
    f.since = Some(2000);
    f.until = Some(3000);
    assert_eq!(titles(&s, &f), ["audio", "big video"]);

    let mut f = q("video");
    f.categories = vec![Category::Video];
    f.min_size = Some(1000);
    assert_eq!(titles(&s, &f), ["big video"]);
}

#[test]
fn liveness_filters() {
    let s = store();
    let now = 1_000_000;
    for (n, title) in [
        (1, "alive"),
        (2, "dead fresh"),
        (3, "dead expired"),
        (4, "unknown"),
    ] {
        torrent(&ih(n), title, &pk(1), 100 + n as i64).put(&s);
    }
    s.put_liveness_sync(&liveness(&ih(1), Some(7), now - 10, 3600))
        .unwrap();
    s.put_liveness_sync(&liveness(&ih(2), Some(0), now - 10, 3600))
        .unwrap();
    s.put_liveness_sync(&liveness(&ih(3), Some(0), now - 7200, 3600))
        .unwrap();

    let run = |f: &magnetar_core::query::SearchQuery| -> Vec<String> {
        s.search_sync(f, now)
            .unwrap()
            .hits
            .into_iter()
            .map(|h| h.title)
            .collect()
    };
    let mut f = q("");
    f.hide_dead = true;
    assert_eq!(run(&f), ["unknown", "dead expired", "alive"]);

    // min_seeders: unknown counts as 0 (only when the filter is set).
    let mut f = q("");
    f.min_seeders = Some(1);
    assert_eq!(run(&f), ["alive"]);
    f.min_seeders = Some(0);
    assert_eq!(run(&f).len(), 4);

    let hits = s.search_sync(&q(""), now).unwrap().hits;
    let by = |t: &str| hits.iter().find(|h| h.title == t).unwrap().clone();
    assert_eq!(by("alive").dead, Some(false));
    assert_eq!(by("alive").seeders, Some(7));
    assert_eq!(by("dead fresh").dead, Some(true));
    assert_eq!(by("dead expired").dead, None);
    assert_eq!(by("unknown").dead, None);
    assert_eq!(by("unknown").seeders, None);

    // DHT estimate counts as peers.
    let mut dht = liveness(&ih(4), None, now, 3600);
    dht.dht_peers_est = Some(3);
    dht.source = LivenessSource::Dht;
    s.put_liveness_sync(&dht).unwrap();
    let mut f = q("");
    f.min_seeders = Some(2);
    assert_eq!(run(&f), ["unknown", "alive"]);
    assert_eq!(s.get_liveness_sync(&ih(4)).unwrap().unwrap(), dht);
}

#[test]
fn sorts() {
    let s = store();
    let now = 1_000_000;
    let mk = |n: u64, size: Option<u64>, at: i64, seeders: Option<u32>| {
        torrent(&ih(n), &format!("t{n}"), &pk(1), at)
            .with(|t| t.total_size = size)
            .put(&s);
        if let Some(x) = seeders {
            s.put_liveness_sync(&liveness(&ih(n), Some(x), now, 3600))
                .unwrap();
        }
    };
    mk(1, Some(300), 10, Some(5));
    mk(2, None, 30, None);
    mk(3, Some(100), 20, Some(50));
    mk(4, Some(200), 40, Some(0));

    let run = |sort: Sort, desc: bool| -> Vec<String> {
        let mut f = q("");
        f.sort = sort;
        f.descending = desc;
        s.search_sync(&f, now)
            .unwrap()
            .hits
            .into_iter()
            .map(|h| h.title)
            .collect()
    };
    assert_eq!(run(Sort::Date, true), ["t4", "t2", "t3", "t1"]);
    assert_eq!(run(Sort::Date, false), ["t1", "t3", "t2", "t4"]);
    // Unknown size and unknown seeders are last in both directions.
    assert_eq!(run(Sort::Size, true), ["t1", "t4", "t3", "t2"]);
    assert_eq!(run(Sort::Size, false), ["t3", "t4", "t1", "t2"]);
    assert_eq!(run(Sort::Seeders, true), ["t3", "t1", "t4", "t2"]);
    assert_eq!(run(Sort::Seeders, false), ["t4", "t1", "t3", "t2"]);
    // Relevance without text falls back to date.
    assert_eq!(run(Sort::Relevance, true), ["t4", "t2", "t3", "t1"]);
}

#[test]
fn limit_offset_total() {
    let s = store();
    for n in 0..30 {
        torrent(&ih(n), &format!("item {n}"), &pk(1), n as i64).put(&s);
    }
    let mut f = q("item");
    f.sort = Sort::Date;
    f.limit = 10;
    f.offset = 25;
    let page = s.search_sync(&f, 0).unwrap();
    assert_eq!(page.total, 30);
    assert_eq!(page.hits.len(), 5);
    assert_eq!(page.hits[0].title, "item 4");
    f.offset = 100;
    let page = s.search_sync(&f, 0).unwrap();
    assert_eq!((page.total, page.hits.len()), (30, 0));
    f.limit = 0;
    f.offset = 0;
    assert_eq!(s.search_sync(&f, 0).unwrap().hits.len(), 30);
}

#[test]
fn hidden_and_pinned() {
    let s = store();
    torrent(&ih(1), "keep", &pk(1), 1).put(&s);
    torrent(&ih(2), "hide me", &pk(1), 2).put(&s);
    s.set_hidden(&ih(2), true).unwrap();
    s.set_pinned(&ih(1), true).unwrap();
    assert_eq!(titles(&s, &q("")), ["keep"]);
    assert_eq!(s.search_sync(&q(""), 0).unwrap().total, 1);
    let mut f = q("");
    f.include_hidden = true;
    let hits = s.search_sync(&f, 0).unwrap().hits;
    assert_eq!(hits.len(), 2);
    assert!(
        hits.iter()
            .any(|h| h.title == "hide me" && h.hidden && !h.pinned)
    );
    assert!(
        hits.iter()
            .any(|h| h.title == "keep" && h.pinned && !h.hidden)
    );
    s.set_hidden(&ih(2), false).unwrap();
    assert_eq!(titles(&s, &q("")).len(), 2);
    // Flags may be set before the torrent is known.
    s.set_pinned(&ih(77), true).unwrap();
    assert_eq!(s.flags(&[ih(77)]).unwrap()[&ih(77)], (true, false));
}

#[test]
fn ingest_is_idempotent_and_tracked() {
    let s = store();
    let t = torrent(&ih(1), "once", &pk(1), 10);
    assert!(t.put(&s));
    assert!(!t.put(&s));
    assert!(s.has_event_sync(&t.id()).unwrap());
    assert!(s.known_torrent_event_sync(&t.id()).unwrap());
    assert!(!s.known_torrent_event_sync(&hex64(1)).unwrap());
    let (c, raw) = comment(&t.id(), &pk(9), 11, "thanks");
    assert!(s.put_comment_sync(&c, &raw).unwrap());
    assert!(!s.put_comment_sync(&c, &raw).unwrap());
    assert!(!s.known_torrent_event_sync(&c.event_id).unwrap());

    let back = s
        .get_raw_events_sync(&[t.id(), c.event_id.clone(), hex64(5)])
        .unwrap();
    assert_eq!(back, vec![t.raw.clone(), raw.clone()]);
    let stats = s.stats_sync().unwrap();
    assert_eq!(
        (stats.torrents, stats.comments, stats.publishers),
        (1, 1, 1)
    );
    assert_eq!(
        stats.unsynced, 2,
        "2003 + 2004 tracked in sync_ids, reports not"
    );
    assert!(stats.db_bytes > 0);
}

#[test]
fn relays_and_cursors() {
    let s = store();
    let url = "wss://nos.lol";
    s.upsert_relay_sync(url, "seed").unwrap();
    s.set_relay_enabled_sync(url, false).unwrap();
    s.upsert_relay_sync(url, "nip65").unwrap();
    let r = &s.list_relays_sync().unwrap()[0];
    assert_eq!(
        (r.source.as_str(), r.enabled),
        ("seed", false),
        "upsert never overwrites"
    );

    let now = 10_000;
    s.record_relay_result_sync(url, false, Some("timeout"), 0, now)
        .unwrap();
    s.record_relay_result_sync(url, false, Some("timeout"), 0, now)
        .unwrap();
    let r = &s.list_relays_sync().unwrap()[0];
    assert_eq!(r.consecutive_failures, 2);
    assert_eq!(r.next_try_at, now + backoff_secs(2));
    assert_eq!(r.last_err.as_deref(), Some("timeout"));
    s.record_relay_result_sync(url, true, None, 42, now + 5)
        .unwrap();
    let r = &s.list_relays_sync().unwrap()[0];
    assert_eq!(
        (r.consecutive_failures, r.last_ok, r.events_found),
        (0, Some(now + 5), 42)
    );
    assert_eq!(r.last_err, None);
    assert_eq!(backoff_secs(1), 60);
    assert_eq!(backoff_secs(100), 24 * 3600);
    assert!((1..30).all(|f| backoff_secs(f) <= backoff_secs(f + 1)));

    // Unknown relay results are recorded as hints; manual enable creates a manual row.
    s.record_relay_result_sync("wss://new.example", true, None, 1, now)
        .unwrap();
    s.set_relay_enabled_sync("wss://manual.example", true)
        .unwrap();
    s.set_relay_supports_neg_sync(url, true).unwrap();
    let relays = s.list_relays_sync().unwrap();
    assert_eq!(relays.len(), 3);
    assert!(
        relays
            .iter()
            .any(|r| r.url == "wss://new.example" && r.source == "hint")
    );
    assert!(
        relays
            .iter()
            .any(|r| r.url == "wss://manual.example" && r.source == "manual")
    );
    assert_eq!(
        relays.iter().find(|r| r.url == url).unwrap().supports_neg,
        Some(true)
    );

    assert_eq!(s.get_cursor_sync(url, 2003).unwrap(), None);
    let c = Cursor {
        relay: url.into(),
        kind: 2003,
        newest_seen: Some(5),
        backfill_until: Some(3),
        backfill_done: false,
    };
    s.put_cursor_sync(&c).unwrap();
    let c2 = Cursor {
        backfill_done: true,
        backfill_until: None,
        ..c.clone()
    };
    s.put_cursor_sync(&c2).unwrap();
    assert_eq!(s.get_cursor_sync(url, 2003).unwrap(), Some(c2));
    assert_eq!(s.get_cursor_sync(url, 2004).unwrap(), None);
}

#[test]
fn publishers_and_stats() {
    let s = store();
    for i in 0..5 {
        torrent(&ih(i), "a", &pk(1), 1000 + i as i64 * 600).put(&s);
    }
    torrent(&ih(10), "b", &pk(2), 5000)
        .with(|t| t.source_url = Some("https://example.org/t/1".into()))
        .put(&s);
    assert_eq!(s.publishers_sync(10).unwrap(), [pk(1), pk(2)]);
    assert_eq!(s.publishers_sync(1).unwrap(), [pk(1)]);
    let st = s.publisher_stats_sync(&pk(1)).unwrap();
    assert_eq!(st.first_seen, Some(1000));
    assert_eq!(st.torrent_count, 5);
    assert_eq!(st.max_per_hour, 5, "events at +0..+2400 s fit one hour");
    assert!(!st.is_bridge);
    assert!(s.publisher_stats_sync(&pk(2)).unwrap().is_bridge);
    let none = s.publisher_stats_sync(&pk(3)).unwrap();
    assert_eq!(
        (none.torrent_count, none.max_per_hour, none.first_seen),
        (0, 0, None)
    );
}

#[test]
fn liveness_due_lists_missing_and_expired_newest_first() {
    let s = store();
    let now = 100_000;
    torrent(&ih(1), "old", &pk(1), 10).put(&s);
    torrent(&ih(2), "new", &pk(1), 20)
        .with(|t| t.trackers = vec!["udp://a:1".into()])
        .put(&s);
    torrent(&ih(2), "new again", &pk(2), 21)
        .with(|t| t.trackers = vec!["udp://b:1".into(), "udp://a:1".into()])
        .put(&s);
    torrent(&ih(3), "fresh", &pk(1), 30).put(&s);
    s.put_liveness_sync(&liveness(&ih(3), Some(1), now, 3600))
        .unwrap();
    s.put_liveness_sync(&liveness(&ih(1), Some(1), now - 7200, 3600))
        .unwrap();
    let due = s.liveness_due_sync(now, 10).unwrap();
    assert_eq!(
        due,
        vec![
            (ih(2), vec!["udp://a:1".to_string(), "udp://b:1".into()]),
            (
                ih(1),
                vec!["udp://tracker.opentrackr.org:1337/announce".to_string()]
            ),
        ]
    );
    assert_eq!(s.liveness_due_sync(now, 1).unwrap().len(), 1);
    assert_eq!(s.trackers_of(&ih(2)).unwrap(), ["udp://a:1", "udp://b:1"]);
}

#[test]
fn settings_and_graph_version() {
    let s = store();
    assert_eq!(s.get_setting_sync("x").unwrap(), None);
    s.set_setting_sync("x", "1").unwrap();
    assert_eq!(s.get_setting_sync("x").unwrap().as_deref(), Some("1"));
    let v0 = s.graph_version_sync().unwrap();
    s.set_setting_sync(keys::TRUST_ROOT, &format!("\"{}\"", pk(1)))
        .unwrap();
    let v1 = s.graph_version_sync().unwrap();
    assert_eq!(v1, v0 + 1, "root change bumps");
    s.set_setting_sync(keys::TRUST_ROOT, &format!("\"{}\"", pk(1)))
        .unwrap();
    assert_eq!(s.graph_version_sync().unwrap(), v1, "same root does not");
    assert_eq!(s.bump_graph_version_sync().unwrap(), v1 + 1);
}

#[test]
fn overrides_roundtrip() {
    let s = store();
    s.set_override_sync(&pk(1), Some(OverrideAction::Block))
        .unwrap();
    s.set_override_sync(&pk(2), Some(OverrideAction::Trust))
        .unwrap();
    s.set_override_sync(&pk(2), Some(OverrideAction::Block))
        .unwrap();
    s.set_override_sync(&pk(3), Some(OverrideAction::Trust))
        .unwrap();
    s.set_override_sync(&pk(3), None).unwrap();
    assert_eq!(
        s.overrides_sync().unwrap(),
        vec![
            (pk(1), OverrideAction::Block),
            (pk(2), OverrideAction::Block)
        ]
    );
}

#[test]
fn saved_searches_crud() {
    let s = store();
    let mut ss = SavedSearch {
        id: 0,
        name: "linux".into(),
        query: q("ubuntu"),
        notify: true,
        last_checked: None,
        last_seen_created_at: None,
    };
    let id = s.put_saved_search_sync(&ss).unwrap();
    assert!(id > 0);
    ss.id = id;
    ss.last_seen_created_at = Some(5);
    assert_eq!(s.put_saved_search_sync(&ss).unwrap(), id);
    assert_eq!(s.list_saved_searches_sync().unwrap(), vec![ss.clone()]);
    let id2 = s
        .put_saved_search_sync(&SavedSearch {
            id: 0,
            ..ss.clone()
        })
        .unwrap();
    assert_ne!(id, id2);
    s.delete_saved_search_sync(id).unwrap();
    let left = s.list_saved_searches_sync().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, id2);
}

#[tokio::test]
async fn sync_source_tracks_every_event() {
    let s = store();
    let a = torrent(&ih(1), "a", &pk(1), 30);
    let b = torrent(&ih(2), "b", &pk(1), 10);
    a.put(&s);
    b.put(&s);
    let (c, craw) = comment(&a.id(), &pk(2), 20, "hi");
    s.put_comment_sync(&c, &craw).unwrap();
    let mut c1111 = craw.clone();
    c1111.id = next_id();
    c1111.kind = KIND_COMMENT;
    let cc = Comment {
        event_id: c1111.id.clone(),
        kind: KIND_COMMENT,
        ..c.clone()
    };
    s.put_comment_sync(&cc, &c1111).unwrap();

    let items = s.neg_items().await.unwrap();
    assert_eq!(items.len(), 4);
    assert!(
        items.windows(2).all(|w| w[0] <= w[1]),
        "sorted by created_at"
    );
    assert_eq!(hex::encode(items[0].1), b.id());

    let unsynced = s.unsynced(10).await.unwrap();
    assert_eq!(unsynced.len(), 4);
    assert_eq!(unsynced[0], b.raw, "oldest first");
    assert_eq!(s.unsynced(2).await.unwrap().len(), 2);

    s.record_failure(&b.id(), "invalid: bad signature")
        .await
        .unwrap();
    // Failed ones retry after the others.
    assert_eq!(s.unsynced(10).await.unwrap().last().unwrap().id, b.id());

    s.mark_synced(&[a.id(), c.event_id.clone()]).await.unwrap();
    let left: Vec<String> = s
        .unsynced(10)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(left, vec![c1111.id.clone(), b.id()]);
    assert_eq!(s.stats().await.unwrap().unsynced, 2);
    assert_eq!(s.unsynced_count().unwrap(), 2);
    assert_eq!(s.load_events(&[a.id()]).await.unwrap(), vec![a.raw.clone()]);
}

/// ARCHITECTURE.md §7: cleanup evicts only synced, unpinned bodies; never unsynced rows; sync_ids
/// survive; the database actually shrinks.
#[test]
fn cleanup_invariant() {
    let dir = TempDir::new("cleanup");
    let s = SqliteStore::open(dir.0.join("cache.db")).unwrap();
    let blob = "lorem ipsum dolor sit amet ".repeat(400); // ~10 KiB per torrent
    let mut synced = Vec::new();
    let mut unsynced = Vec::new();
    let mut synced_comments = Vec::new();
    let mut unsynced_comments = Vec::new();
    for n in 0..400u64 {
        let t = torrent(&ih(n), &format!("Release.{n}.1080p"), &pk(n % 7), n as i64)
            .with(|t| t.description = format!("{blob} {n}"));
        t.put(&s);
        let (c, raw) = comment(&t.id(), &pk(99), n as i64 + 1, &blob[..2000]);
        s.put_comment_sync(&c, &raw).unwrap();
        if n % 4 == 0 {
            unsynced.push(t);
            unsynced_comments.push(c.event_id);
        } else {
            synced_comments.push(c.event_id.clone());
            synced.push(t);
        }
    }
    let ids: Vec<String> = synced
        .iter()
        .map(|t| t.id())
        .chain(synced_comments.iter().cloned())
        .collect();
    s.mark_synced_sync(&ids).unwrap();
    // A synced but pinned torrent must survive too.
    let pinned = synced[0].parsed.infohash.clone();
    s.set_pinned(&pinned, true).unwrap();
    // Recently accessed bodies go last.
    let recent = synced.last().unwrap().parsed.infohash.clone();
    s.touch(&recent, i64::MAX / 2).unwrap();

    let neg_before = s.neg_items_sync().unwrap();
    let before = s.stats_sync().unwrap();
    let cap = before.db_bytes / 3;
    let r = s.cleanup(cap).unwrap();

    assert!(r.evicted > 0);
    assert_eq!(r.bytes_before, before.db_bytes);
    assert!(r.bytes_after < r.bytes_before, "{r:?}");
    assert!(r.bytes_after <= cap, "under cap: {r:?}");
    assert_eq!(r.unsynced_kept, 200, "100 torrents + 100 comments");
    let file_len = std::fs::metadata(dir.0.join("cache.db")).unwrap().len();
    assert!(file_len <= r.bytes_after, "file shrank too: {file_len}");

    // Unsynced rows untouched, and their bodies are still loadable for upload.
    let after = s.stats_sync().unwrap();
    assert_eq!(after.unsynced, before.unsynced);
    for t in &unsynced {
        assert!(s.known_torrent_event_sync(&t.id()).unwrap());
        assert_eq!(s.get_raw_events_sync(&[t.id()]).unwrap().len(), 1);
    }
    assert_eq!(
        s.get_raw_events_sync(&unsynced_comments).unwrap().len(),
        unsynced_comments.len()
    );
    assert_eq!(s.unsynced_sync(1000).unwrap().len(), 200);
    // Pinned and recently used survive.
    let present = |ih: &str| {
        let mut f = q("");
        f.limit = 200;
        (0..2).any(|page| {
            f.offset = page * 200;
            s.search_sync(&f, 0)
                .unwrap()
                .hits
                .iter()
                .any(|h| h.infohash == ih)
        })
    };
    assert!(present(&pinned));
    assert!(present(&recent));
    // sync_ids never pruned: same negentropy set, evicted ids still "known".
    assert_eq!(s.neg_items_sync().unwrap(), neg_before);
    let evicted: Vec<&T> = synced
        .iter()
        .filter(|t| s.get_raw_events_sync(&[t.id()]).unwrap().is_empty())
        .collect();
    assert_eq!(evicted.len() as u64, r.evicted);
    for t in &evicted {
        assert!(s.has_event_sync(&t.id()).unwrap());
        assert!(s.known_torrent_event_sync(&t.id()).unwrap());
        assert!(!t.put(&s), "an evicted event is not re-ingested");
    }
    // Their FTS rows are gone and the index is consistent.
    let evicted_title = evicted[0].parsed.title.clone();
    assert!(!titles(&s, &q(&evicted_title)).contains(&evicted_title));
    assert_eq!(after.torrents, 400 - r.evicted);
    // Synced comments on evicted torrents are gone; others stay.
    assert!(after.comments >= 100);

    // No cap = no-op; a cap we are already under = no-op.
    assert_eq!(s.cleanup(0).unwrap().evicted, 0);
    assert_eq!(s.cleanup(u64::MAX / 2).unwrap().evicted, 0);
    // Nothing evictable left except pinned/unsynced/recent: tiny cap stops instead of looping.
    let r2 = s.cleanup(1).unwrap();
    assert_eq!(r2.unsynced_kept, 200);
    assert!(present(&pinned));
    assert!(s.get_raw_events_sync(&[unsynced[0].id()]).unwrap().len() == 1);
}

#[test]
fn reopen_keeps_data_and_schema() {
    let dir = TempDir::new("reopen");
    let path = dir.0.join("m.db");
    {
        let s = SqliteStore::open(&path).unwrap();
        torrent(&ih(1), "persisted", &pk(1), 1).put(&s);
        assert!(dir.0.join("m.db-wal").exists(), "WAL journal");
    }
    let s = SqliteStore::open(&path).unwrap();
    assert_eq!(titles(&s, &q("persisted")), ["persisted"]);
}

// ---- post-merge: need nostr agent's nip35/ingest/magnet and trust-sync agent's wot.

#[tokio::test]
#[ignore = "post-merge"]
async fn replaceable_semantics() {
    let s = store();
    let root = pk(1);
    s.set_setting_sync(keys::TRUST_ROOT, &format!("\"{root}\""))
        .unwrap();
    let v0 = s.graph_version_sync().unwrap();
    let older = raw(KIND_CONTACTS, &root, 100, vec![vec!["p", &pk(2)]]);
    let newer = raw(
        KIND_CONTACTS,
        &root,
        200,
        vec![vec!["p", &pk(3)], vec!["p", &pk(4)]],
    );
    assert!(s.put_replaceable(&older).await.unwrap());
    assert!(s.put_replaceable(&newer).await.unwrap());
    assert!(!s.put_replaceable(&older).await.unwrap(), "older loses");
    assert_eq!(s.follows_of(&root).await.unwrap(), vec![pk(3), pk(4)]);
    assert!(
        s.graph_version_sync().unwrap() > v0,
        "root head change bumps"
    );
    assert!(!s.has_event(&older.id).await.unwrap());
    // Tie: lowest id wins.
    let mut tie = raw(KIND_CONTACTS, &root, 200, vec![vec!["p", &pk(5)]]);
    tie.id = "0".repeat(64);
    assert!(s.put_replaceable(&tie).await.unwrap());
    assert_eq!(s.follows_of(&root).await.unwrap(), vec![pk(5)]);
    assert_eq!(
        s.followers_among(&[pk(5), pk(6)], std::slice::from_ref(&root))
            .await
            .unwrap(),
        vec![(pk(5), vec![root.clone()])]
    );

    let mute = raw(KIND_MUTE_LIST, &root, 10, vec![vec!["p", &pk(7)]]);
    let set = raw(
        KIND_KIND_MUTE_SET,
        &root,
        10,
        vec![vec!["d", "2003"], vec!["p", &pk(8)]],
    );
    s.put_replaceable(&mute).await.unwrap();
    s.put_replaceable(&set).await.unwrap();
    let mut mutes = s.mutes_by(std::slice::from_ref(&root)).await.unwrap();
    mutes.sort();
    assert_eq!(mutes, vec![(root.clone(), pk(7)), (root.clone(), pk(8))]);

    let rl = raw(
        KIND_RELAY_LIST,
        &pk(2),
        10,
        vec![vec!["r", "wss://relay.example.com", "write"]],
    );
    s.put_replaceable(&rl).await.unwrap();
    assert_eq!(
        s.relay_lists_for(&[pk(2)]).await.unwrap(),
        vec![(pk(2), "wss://relay.example.com".to_string(), false, true)]
    );
    assert!(!s.relay_list_oversized(&pk(2)).await.unwrap());
    let profile = raw(KIND_PROFILE, &pk(2), 10, vec![]);
    s.put_replaceable(&profile).await.unwrap();
    let st = s.publisher_stats(&pk(2)).await.unwrap();
    assert!(st.has_profile && st.has_relay_list);

    let rep = raw(KIND_REPORT, &root, 10, vec![vec!["p", &pk(9), "spam"]]);
    assert!(s.put_report(&rep).await.unwrap());
    assert_eq!(
        s.reports_by(std::slice::from_ref(&root)).await.unwrap(),
        vec![(root.clone(), pk(9), "spam".to_string())]
    );
}

#[tokio::test]
#[ignore = "post-merge"]
async fn detail_groups_events_and_comments() {
    let s = store();
    let a = torrent(&ih(5), "Big.Buck.Bunny", &pk(1), 100);
    let b = torrent(&ih(5), "Big Buck Bunny (mirror)", &pk(2), 200)
        .with(|t| t.trackers = vec!["udp://other:1".into()]);
    a.put(&s);
    b.put(&s);
    s.put_trust_sync(&score_of(&pk(2), 80)).unwrap();
    let (c, raw) = comment(&a.id(), &pk(3), 300, "works");
    s.put_comment_sync(&c, &raw).unwrap();
    s.put_liveness_sync(&liveness(&ih(5), Some(3), 1000, 3600))
        .unwrap();
    let d = s.torrent_detail(&ih(5), 1000).await.unwrap().unwrap();
    assert_eq!(d.summary.event_id, b.id());
    assert_eq!(d.summary.publisher_count, 2);
    assert_eq!(d.events.len(), 2);
    assert_eq!(d.events[0].event_id, b.id());
    assert_eq!(d.comments, vec![c]);
    assert_eq!(d.trust.score, 80);
    assert_eq!(d.liveness.unwrap().seeders, Some(3));
    assert!(
        d.magnet
            .starts_with(&format!("magnet:?xt=urn:btih:{}", ih(5)))
    );
    assert!(d.trackers.contains(&"udp://other:1".to_string()));
    assert!(s.torrent_detail(&ih(6), 0).await.unwrap().is_none());
}

/// Fixture ingest parity with PgStore (ARCHITECTURE.md §10): every fixture line ingests, and the
/// same queries give the same grouped results as the server store's test.
#[tokio::test]
#[ignore = "post-merge"]
async fn fixture_ingest_parity() {
    use magnetar_core::ingest::{IngestOutcome, ingest_into};
    let s = store();
    let root = env!("CARGO_MANIFEST_DIR");
    let mut inserted = 0;
    for file in ["kind2003_sample.jsonl", "kind2004_sample.jsonl"] {
        let path = format!("{root}/../../docs/research/fixtures/{file}");
        for line in std::fs::read_to_string(&path).unwrap().lines() {
            let raw = RawEvent::from_json(line).unwrap();
            if let IngestOutcome::Inserted =
                ingest_into(&s, raw, magnetar_core::now()).await.unwrap()
            {
                inserted += 1;
            }
        }
    }
    assert!(
        inserted >= 52,
        "every 2003 fixture is ingested ({inserted})"
    );
    let stats = s.stats().await.unwrap();
    assert!(stats.torrents > 0 && stats.torrents <= 52);
    let page = s.search(&q("softwar"), 0).await.unwrap();
    assert!(!page.hits.is_empty());
    for h in s.search(&q(""), 0).await.unwrap().hits {
        let d = s.torrent_detail(&h.infohash, 0).await.unwrap().unwrap();
        assert_eq!(
            d.summary.event_id, h.event_id,
            "search and detail agree on canonical"
        );
    }
}
