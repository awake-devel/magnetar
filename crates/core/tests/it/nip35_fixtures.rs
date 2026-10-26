//! NIP-35 parsing and ingest classification against real relay events (docs/research/fixtures).

use crate::support;

use magnetar_core::ingest::{self, IngestOutcome, MAX_EVENT_BYTES, MAX_FUTURE_SECS};
use magnetar_core::model::*;
use magnetar_core::nip35::{self, parse_comment, parse_torrent};
use nostr::prelude::Keys;
use support::store::MemStore;
use support::*;

const NOW: i64 = 1_793_000_000;

fn parse(prefix: &str) -> ParsedTorrent {
    parse_torrent(&fixture(prefix)).unwrap()
}

fn files(t: &ParsedTorrent) -> Vec<(&str, Option<u64>)> {
    t.files.iter().map(|f| (f.path.as_str(), f.size)).collect()
}

#[test]
fn every_torrent_fixture_verifies_and_parses() {
    let events = torrents_fixture();
    assert_eq!(events.len(), 52);
    let mut rejected = Vec::new();
    for ev in events {
        ev.verify().unwrap_or_else(|e| panic!("{}: {e}", ev.id));
        let id = ev.id.clone();
        match ingest::classify(ev, NOW) {
            Ok(IngestItem::Torrent(t, raw)) => {
                assert_eq!(t.event_id, raw.id);
                assert_eq!(t.infohash.len(), 40);
                assert!(
                    t.infohash
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                );
                assert!(!t.title.is_empty());
                assert!(t.files.len() <= nip35::MAX_FILES);
                assert!(t.trackers.len() <= nip35::MAX_TRACKERS);
                assert!(t.description.len() <= nip35::MAX_DESCRIPTION_BYTES);
                assert!(
                    t.files
                        .iter()
                        .all(|f| f.path.len() <= nip35::MAX_FILE_PATH_BYTES)
                );
            }
            Ok(other) => panic!("{id}: unexpected {other:?}"),
            Err(r) => rejected.push((id, r)),
        }
    }
    // Only the 64-hex-x event (no v1 hash anywhere) is rejected.
    assert_eq!(rejected.len(), 1, "{rejected:?}");
    assert!(rejected[0].0.starts_with("00eb1974"));
    assert_eq!(rejected[0].1, Rejection::NoInfohash);
}

#[test]
fn tampered_events_fail_verification() {
    let ev = fixture("b29b854d");
    let mut content = ev.clone();
    content.content.push('!');
    assert_eq!(
        ingest::classify(content, NOW).unwrap_err(),
        Rejection::BadId
    );

    let mut tags = ev.clone();
    tags.tags[0][1] = "Another title".into();
    assert_eq!(ingest::classify(tags, NOW).unwrap_err(), Rejection::BadId);

    let mut sig = ev.clone();
    let flipped = if sig.sig.starts_with('0') { "1" } else { "0" };
    sig.sig.replace_range(0..1, flipped);
    assert_eq!(ingest::classify(sig, NOW).unwrap_err(), Rejection::BadSig);

    let mut upper = ev;
    upper.id = upper.id.to_uppercase();
    assert_eq!(ingest::classify(upper, NOW).unwrap_err(), Rejection::BadId);
}

#[test]
fn btih_alias_and_uppercase() {
    let t = parse("01a14e7f");
    assert_eq!(t.infohash, "59cf973b7d05df2362c556c950f45a80f1148c5c");
    assert_eq!(t.title, "SOFTWAR Audiobook - SW002 - Chapter 1 (MP3)");
    assert_eq!(t.category, Category::Audio);
    assert_eq!(t.category_path, vec!["audio", "audio-book"]);
    assert!(t.trackers.is_empty());
    assert_eq!(t.total_size, Some(61013535 + 1301));

    let t = parse("b29b854d");
    assert_eq!(t.infohash, "77dcbd4597cb43f549fd1433d516a696db30044c");
    assert_eq!(t.category, Category::Other);
}

#[test]
fn junk_after_hash() {
    let t = parse("d55f6029");
    assert_eq!(t.infohash, "620032fcaf435a21c2a07d177807e4870ba5a94c");
    // No file tags and no size tag: unknown size, title from the title tag.
    assert!(t.files.is_empty());
    assert_eq!(t.total_size, None);
    // First t is "torrent" (unmapped), the next one is "movie".
    assert_eq!(t.category, Category::Video);
}

#[test]
fn v2_hash_without_v1_is_rejected_but_v2_is_kept_when_v1_exists() {
    assert_eq!(
        parse_torrent(&fixture("00eb1974")).unwrap_err(),
        Rejection::NoInfohash
    );
    let keys = Keys::generate();
    let v2 = "84ccd75d383a24368dd8723d3bd62e564cc06a2ac09dbc7b88b37e4369c1290b";
    let ev = sign(
        &keys,
        2003,
        NOW - 10,
        &[
            &["x", v2],
            &["btih", "59CF973B7D05DF2362C556C950F45A80F1148C5C"],
        ],
        "",
    );
    let t = parse_torrent(&ev).unwrap();
    assert_eq!(t.infohash, "59cf973b7d05df2362c556c950f45a80f1148c5c");
    assert_eq!(t.infohash_v2.as_deref(), Some(v2));
    // No title and no files: the infohash is the title.
    assert_eq!(t.title, t.infohash);
}

#[test]
fn base32_infohash() {
    let keys = Keys::generate();
    let hex = "77dcbd4597cb43f549fd1433d516a696db30044c";
    let b32 = data_encoding::BASE32_NOPAD.encode(&::hex::decode(hex).unwrap());
    let ev = sign(
        &keys,
        2003,
        NOW - 10,
        &[&["x", &b32], &["title", "b32"]],
        "",
    );
    assert_eq!(parse_torrent(&ev).unwrap().infohash, hex);
}

#[test]
fn single_element_name_size_files() {
    let t = parse("000c4fc7");
    assert_eq!(
        files(&t),
        vec![
            (
                "Boule Petanque (v2.2.4.1102) (VRP)/com.KaiKramhoeft.Boulepetanque.apk",
                Some(916821638)
            ),
            (
                "Boule Petanque (v2.2.4.1102) (VRP)/release.manifest",
                Some(319)
            ),
        ]
    );
    assert_eq!(t.total_size, Some(916821638 + 319));
    assert!(t.trackers.is_empty());
    assert_eq!(t.published_at, Some(1771528333));
    assert!(t.external_ids.contains(&("ygg".into(), "1449971".into())));
    // YGG French t tags: "sport", "autre" map to nothing, "jeu-vidéo" does.
    assert_eq!(t.category, Category::Game);

    let t = parse("0112f758");
    assert_eq!(files(&t), vec![("Shine T13 (Inoya).cbz", Some(114655939))]);
    assert_eq!(t.category, Category::Book);
}

#[test]
fn size_tag_and_porn_category() {
    let t = parse("000d4e99");
    assert_eq!(t.category, Category::Porn);
    assert!(t.total_size.is_some());
    // `["ygg"]` with one element is ignored; size from files, which agree with the size tag.
    let t = parse("023ee8fd");
    assert_eq!(t.total_size, Some(3307535096));
    assert_eq!(t.files[0].size, Some(3307535096));
    assert_eq!(t.category, Category::Porn);
}

#[test]
fn size_tag_fallback_when_files_missing_or_zero() {
    let keys = Keys::generate();
    let ih = "59cf973b7d05df2362c556c950f45a80f1148c5c";
    let none = sign(
        &keys,
        2003,
        NOW,
        &[&["x", ih], &["size", " 4844799868 "]],
        "",
    );
    assert_eq!(parse_torrent(&none).unwrap().total_size, Some(4844799868));
    let zero = sign(
        &keys,
        2003,
        NOW,
        &[&["x", ih], &["file", "a", "0"], &["size", "77"]],
        "",
    );
    assert_eq!(parse_torrent(&zero).unwrap().total_size, Some(77));
    let unknown = sign(
        &keys,
        2003,
        NOW,
        &[&["x", ih], &["file", "a", "1.5"], &["file", "b", "10"]],
        "",
    );
    let t = parse_torrent(&unknown).unwrap();
    assert_eq!(files(&t), vec![("a", None), ("b", Some(10))]);
    assert_eq!(t.total_size, None);
    assert_eq!(t.title, "a");
}

#[test]
fn folder_listing_blob_file_names() {
    let t = parse("000000002801a450");
    let blob = &t.files[0];
    assert!(blob.path.contains('\n'));
    assert_eq!(blob.path, blob.path.trim());
    let file_tags = fixture("000000002801a450")
        .tags
        .iter()
        .filter(|t| t[0] == "file")
        .count();
    assert_eq!(t.files.len(), file_tags);
    assert_eq!(
        t.source_url.as_deref(),
        Some("https://nyaa.si/view/2117324")
    );
    assert!(t.external_ids.contains(&("nyaa".into(), "2117324".into())));

    let big = parse("000000a62bab01cc");
    assert!(
        big.files
            .iter()
            .all(|f| f.path.len() <= nip35::MAX_FILE_PATH_BYTES)
    );
    assert!(big.files.len() > 200);
    assert_eq!(big.category, Category::Anime);

    // Title fallback skips blobs, and uses a blob's first line only if nothing else exists.
    let keys = Keys::generate();
    let ih = "59cf973b7d05df2362c556c950f45a80f1148c5c";
    let long = "x".repeat(5000);
    let ev = sign(
        &keys,
        2003,
        NOW,
        &[
            &["x", ih],
            &["file", "  Folder\n\t\t\tinner.mkv (1 MiB)\n", "5"],
            &["file", &long, "6"],
        ],
        "",
    );
    let t = parse_torrent(&ev).unwrap();
    assert_eq!(t.files[0].path, "Folder\n\t\t\tinner.mkv (1 MiB)");
    assert_eq!(t.files[1].path.len(), nip35::MAX_FILE_PATH_BYTES);
    assert_eq!(t.title.len(), nip35::MAX_FILE_PATH_BYTES);
    let only_blob = sign(
        &keys,
        2003,
        NOW,
        &[&["x", ih], &["file", "\n Folder\n\tinner", "5"]],
        "",
    );
    assert_eq!(parse_torrent(&only_blob).unwrap().title, "Folder");
}

#[test]
fn fourth_file_element_and_external_ids() {
    let t = parse("90230a71");
    assert_eq!(
        files(&t),
        vec![("Screenshot 2026-02-27 at 16.38.26.png", Some(49836))]
    );
    // wss trackers are kept for display.
    assert_eq!(t.trackers, vec!["wss://relay.damus.io/", "wss://nos.lol/"]);
    assert_eq!(
        t.external_ids,
        vec![
            ("tcat".to_string(), "software,tools".to_string()),
            ("imdb".to_string(), "tt0133093".to_string()),
            ("tmdb".to_string(), "movie:603".to_string()),
        ]
    );
    assert_eq!(t.category_path, vec!["software", "tools"]);
    assert_eq!(t.category, Category::Application);

    let t = parse("4fb8887d");
    assert!(
        t.external_ids
            .contains(&("tmdb".into(), "movie:106646".into()))
    );
    assert_eq!(t.category, Category::Video);
    // Glued tracker list salvaged, duplicates removed.
    assert_eq!(
        t.trackers,
        vec![
            "udp://tracker.leechers-paradise.org:6969",
            "udp://tracker.openbittorrent.com:80",
            "udp://open.demonii.com:1337",
            "udp://tracker.coppersurfer.tk:6969",
            "udp://exodus.desync.com:6969",
        ]
    );
}

#[test]
fn file_size_zero_is_kept() {
    let t = parse("000000249e25b06e");
    assert_eq!(t.files[0].size, Some(0));
    assert_eq!(t.total_size, Some(9654772891));
    assert!(
        t.source_url
            .as_deref()
            .unwrap()
            .starts_with("https://thepiratebay.org/")
    );
    assert!(t.external_ids.contains(&("tpb".into(), "82595459".into())));
}

#[test]
fn broken_and_empty_trackers() {
    let t = parse("b24b8290");
    assert!(t.trackers.len() <= nip35::MAX_TRACKERS);
    assert!(
        t.trackers
            .iter()
            .all(|u| !u.contains(' ') && !u.contains("anunciar"))
    );
    assert!(
        t.trackers
            .contains(&"http://www.spanishtracker.com:2710/announce".to_string())
    );
    // :80 is the http default and is dropped by URL normalisation.
    assert!(
        t.trackers
            .contains(&"http://tracker.openbittorrent.com/announce".to_string())
    );
    let mut dedup = t.trackers.clone();
    dedup.dedup();
    assert_eq!(dedup.len(), t.trackers.len());

    let t = parse("10d08d30");
    assert_eq!(t.trackers.len(), nip35::MAX_TRACKERS);
    assert_eq!(
        t.trackers[0],
        "udp://tracker.openbittorrent.com:1337/announce"
    );
    assert!(t.trackers.iter().all(|u| !u.is_empty()));
    assert!(t.files.len() > 40);
}

#[test]
fn proxy_either_order_and_title_fallbacks() {
    let keys = Keys::generate();
    let ih = "59cf973b7d05df2362c556c950f45a80f1148c5c";
    let nip48 = sign(
        &keys,
        2003,
        NOW,
        &[
            &["x", ih],
            &["proxy", "https://nyaa.si/view/1", "web"],
            &["file", "dir/a.mkv", "1"],
        ],
        "",
    );
    let t = parse_torrent(&nip48).unwrap();
    assert_eq!(t.source_url.as_deref(), Some("https://nyaa.si/view/1"));
    assert_eq!(t.title, "dir/a.mkv");
    let not_url = sign(
        &keys,
        2003,
        NOW,
        &[&["x", ih], &["proxy", "abc", "activitypub"]],
        "",
    );
    assert_eq!(parse_torrent(&not_url).unwrap().source_url, None);
    // Empty title tags are skipped; whitespace trimmed.
    let titled = sign(
        &keys,
        2003,
        NOW,
        &[&["x", ih], &["title", "  "], &["title", " T "]],
        "",
    );
    assert_eq!(parse_torrent(&titled).unwrap().title, "T");
}

#[test]
fn hashtags_tcat_and_i_tag_split() {
    let keys = Keys::generate();
    let ih = "59cf973b7d05df2362c556c950f45a80f1148c5c";
    let ev = sign(
        &keys,
        2003,
        NOW,
        &[
            &["x", ih],
            &["t", "Movie"],
            &["t", "movie"],
            &["t", "HD"],
            &["t", "x"],
            &["t", "y"],
            &["i", "tmdb:movie:693134"],
            &["i", "noprefix"],
            &["i", ":empty"],
            &["imdb", "tt1"],
            &["published_at", "nope"],
        ],
        "",
    );
    let t = parse_torrent(&ev).unwrap();
    assert_eq!(t.hashtags, vec!["movie", "hd", "x", "y"]);
    // No tcat: the first 3 t tags are the path.
    assert_eq!(t.category_path, vec!["movie", "hd", "x"]);
    assert_eq!(t.category, Category::Video);
    assert_eq!(
        t.external_ids,
        vec![
            ("tmdb".to_string(), "movie:693134".to_string()),
            ("imdb".to_string(), "tt1".to_string())
        ]
    );
    assert_eq!(t.published_at, None);
}

#[test]
fn caps_on_huge_events() {
    let keys = Keys::generate();
    let ih = "59cf973b7d05df2362c556c950f45a80f1148c5c";
    let names: Vec<String> = (0..5100).map(|i| format!("f{i}")).collect();
    let trackers: Vec<String> = (0..40)
        .map(|i| format!("udp://t{i}.example:1/announce"))
        .collect();
    let mut tags: Vec<Vec<&str>> = vec![vec!["x", ih]];
    tags.extend(names.iter().map(|n| vec!["file", n.as_str(), "1"]));
    tags.extend(trackers.iter().map(|t| vec!["tracker", t.as_str()]));
    let tag_refs: Vec<&[&str]> = tags.iter().map(Vec::as_slice).collect();
    let content = "é".repeat(40_000);
    let ev = sign(&keys, 2003, NOW, &tag_refs, &content);
    let t = parse_torrent(&ev).unwrap();
    assert_eq!(t.files.len(), nip35::MAX_FILES);
    // Total size counts every file tag, not just the stored ones.
    assert_eq!(t.total_size, Some(5100));
    assert_eq!(t.trackers.len(), nip35::MAX_TRACKERS);
    assert!(t.description.len() <= nip35::MAX_DESCRIPTION_BYTES);
    assert!(t.description.chars().all(|c| c == 'é'));
}

#[test]
fn pow_effective_bits() {
    let t = parse("00000001421411af");
    assert_eq!(t.pow_committed, Some(21));
    assert_eq!(t.pow_bits, 21);
    let t = parse("01a14e7f");
    assert_eq!(t.pow_committed, None);
    assert_eq!(t.pow_bits, 7); // 0x01 -> 7 leading zero bits
}

#[test]
fn comment_fixtures_classify() {
    let mut comments = 0;
    let mut orphans = 0;
    for ev in comments_fixture() {
        ev.verify().unwrap();
        match ingest::classify(ev.clone(), NOW) {
            Ok(IngestItem::Comment(c, _)) => {
                comments += 1;
                assert!(is_hex32(&c.torrent_event_id));
                assert_eq!(c.kind, 2004);
            }
            Err(Rejection::OrphanComment) => {
                orphans += 1;
                // The squatting app: only a `t` tag with a hash, encrypted content.
                assert!(ev.tags.iter().all(|t| t[0] != "e" && t[0] != "E"));
            }
            other => panic!("{}: {other:?}", ev.id),
        }
    }
    assert_eq!((comments, orphans), (53, 7));
}

#[test]
fn comment_e_tag_shapes() {
    let root = |prefix: &str| parse_comment(&fixture(prefix)).unwrap().torrent_event_id;
    // ["e",id,"","root"] + ["e",id,"","reply"]
    assert!(root("0116b532").starts_with("36b8ffbd"));
    // ["e",id,relay,"",pubkey,"root"] (marker at index 5)
    assert!(root("040aba32").starts_with("e1ab66dd"));
    // ["e",id] unmarked (status bot)
    assert!(root("25e569c3").starts_with("0000051b"));
    // root + reply to another comment: the root is the torrent
    assert!(root("12ae990d").starts_with("d9509807"));
    assert!(root("48edafa0").starts_with("0fe31353"));

    let keys = Keys::generate();
    let a = "aa".repeat(32);
    let b = "bb".repeat(32);
    let reply_only = sign(&keys, 2004, NOW, &[&["e", &b, "", "reply"]], "x");
    assert_eq!(parse_comment(&reply_only).unwrap().torrent_event_id, b);
    let mention_and_plain = sign(
        &keys,
        2004,
        NOW,
        &[&["e", &a, "", "mention"], &["e", &b.to_uppercase()]],
        "x",
    );
    assert_eq!(
        parse_comment(&mention_and_plain).unwrap().torrent_event_id,
        b
    );
    let upper_e = sign(
        &keys,
        2004,
        NOW,
        &[&["E", &a], &["e", &b, "", "reply"]],
        "x",
    );
    assert_eq!(parse_comment(&upper_e).unwrap().torrent_event_id, a);
    let bad_id = sign(&keys, 2004, NOW, &[&["e", "abc"]], "x");
    assert_eq!(parse_comment(&bad_id), None);
}

#[test]
fn nip22_comments_and_kind_gates() {
    let keys = Keys::generate();
    let t = "cc".repeat(32);
    let pk = keys.public_key().to_hex();
    let ok = sign(
        &keys,
        1111,
        NOW,
        &[
            &["E", &t, "wss://nos.lol", &pk],
            &["K", "2003"],
            &["P", &pk],
            &["e", &t],
            &["k", "2003"],
        ],
        "nice",
    );
    match ingest::classify(ok, NOW).unwrap() {
        IngestItem::Comment(c, _) => {
            assert_eq!(c.torrent_event_id, t);
            assert_eq!(c.kind, 1111);
        }
        other => panic!("{other:?}"),
    }
    let no_upper = sign(
        &keys,
        1111,
        NOW,
        &[&["K", "2003"], &["k", "2003"], &["e", &t]],
        "x",
    );
    assert_eq!(parse_comment(&no_upper).unwrap().torrent_event_id, t);
    let other_scope = sign(&keys, 1111, NOW, &[&["E", &t], &["K", "1"]], "x");
    assert_eq!(
        ingest::classify(other_scope, NOW).unwrap_err(),
        Rejection::UnsupportedKind
    );
    let no_root = sign(&keys, 1111, NOW, &[&["K", "2003"]], "x");
    assert_eq!(
        ingest::classify(no_root, NOW).unwrap_err(),
        Rejection::OrphanComment
    );

    let mute_set = sign(&keys, 30007, NOW, &[&["d", "2003"], &["p", &t]], "");
    assert!(matches!(
        ingest::classify(mute_set, NOW).unwrap(),
        IngestItem::MuteList(_)
    ));
    let other_set = sign(&keys, 30007, NOW, &[&["d", "1"]], "");
    assert_eq!(
        ingest::classify(other_set, NOW).unwrap_err(),
        Rejection::UnsupportedKind
    );
    for (kind, check) in [
        (0u16, "profile"),
        (3, "contacts"),
        (10000, "mute"),
        (10002, "relays"),
        (1984, "report"),
    ] {
        let item = ingest::classify(sign(&keys, kind, NOW, &[], ""), NOW).unwrap();
        let got = match item {
            IngestItem::Profile(_) => "profile",
            IngestItem::ContactList(_) => "contacts",
            IngestItem::MuteList(_) => "mute",
            IngestItem::RelayList(_) => "relays",
            IngestItem::Report(_) => "report",
            _ => "other",
        };
        assert_eq!(got, check);
    }
    assert_eq!(
        ingest::classify(sign(&keys, 1, NOW, &[], ""), NOW).unwrap_err(),
        Rejection::UnsupportedKind
    );
}

#[test]
fn size_and_time_limits() {
    let keys = Keys::generate();
    let ih = "59cf973b7d05df2362c556c950f45a80f1148c5c";
    let future = sign(&keys, 2003, NOW + MAX_FUTURE_SECS + 5, &[&["x", ih]], "");
    assert_eq!(
        ingest::classify(future, NOW).unwrap_err(),
        Rejection::FutureDated
    );
    let soon = sign(&keys, 2003, NOW + MAX_FUTURE_SECS - 5, &[&["x", ih]], "");
    assert!(ingest::classify(soon, NOW).is_ok());
    let old = sign(&keys, 2003, 1_680_000_000, &[&["x", ih]], "");
    assert!(ingest::classify(old, NOW).is_ok());
    let huge = sign(
        &keys,
        2003,
        NOW,
        &[&["x", ih]],
        &"a".repeat(MAX_EVENT_BYTES),
    );
    assert_eq!(
        ingest::classify(huge, NOW).unwrap_err(),
        Rejection::TooLarge
    );
    let no_hash = sign(&keys, 2003, NOW, &[&["x", "nothing here"]], "");
    assert_eq!(
        ingest::classify(no_hash, NOW).unwrap_err(),
        Rejection::NoInfohash
    );
}

#[tokio::test]
async fn ingest_into_store() {
    let store = MemStore::default();
    let torrents = torrents_fixture();
    let mut inserted = 0;
    for ev in &torrents {
        if ingest::ingest_into(&store, ev.clone(), NOW).await.unwrap() == IngestOutcome::Inserted {
            inserted += 1;
        }
    }
    assert_eq!(inserted, 51);
    assert_eq!(
        ingest::ingest_into(&store, torrents[0].clone(), NOW)
            .await
            .unwrap(),
        IngestOutcome::Duplicate
    );

    // None of the fixture comments reference a fixture torrent: all orphans.
    for ev in comments_fixture() {
        assert_eq!(
            ingest::ingest_into(&store, ev, NOW).await.unwrap(),
            IngestOutcome::Rejected(Rejection::OrphanComment)
        );
    }

    // A comment on a known torrent is stored once.
    let keys = Keys::generate();
    let target = torrents
        .iter()
        .find(|e| e.id.starts_with("b29b854d"))
        .unwrap();
    let c = sign(
        &keys,
        2004,
        NOW,
        &[&["e", &target.id, "", "root"]],
        "thanks",
    );
    assert_eq!(
        ingest::ingest_into(&store, c.clone(), NOW).await.unwrap(),
        IngestOutcome::Inserted
    );
    assert_eq!(
        ingest::ingest_into(&store, c, NOW).await.unwrap(),
        IngestOutcome::Duplicate
    );
    assert_eq!(store.comment_count(), 1);

    // Replaceable: an older contact list is a duplicate.
    let newer = sign(&keys, 3, NOW - 10, &[&["p", &target.pubkey]], "");
    let older = sign(&keys, 3, NOW - 20, &[], "");
    assert_eq!(
        ingest::ingest_into(&store, newer, NOW).await.unwrap(),
        IngestOutcome::Inserted
    );
    assert_eq!(
        ingest::ingest_into(&store, older, NOW).await.unwrap(),
        IngestOutcome::Duplicate
    );
    let mut bad = sign(&keys, 1984, NOW, &[&["p", &target.pubkey, "spam"]], "");
    bad.content = "tampered".into();
    assert_eq!(
        ingest::ingest_into(&store, bad, NOW).await.unwrap(),
        IngestOutcome::Rejected(Rejection::BadId)
    );
}
