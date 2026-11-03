//! Search filters on header-probe results (D122): the index and the Postgres fallback agree,
//! facets count disjunctively, probe writes reach the index through the change log, stored rows
//! get the width-aware resolution class. Needs MAGNETAR_TEST_DATABASE_URL (else SKIPPED),
//! except the in-memory scale test. Two ignored benchmarks measure the design at production
//! size (numbers in ARCHITECTURE.md D122):
//!
//!   cargo test --release -p magnetar-server --test it probe_search:: -- --ignored --nocapture

use crate::common;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use magnetar_core::api::FacetSet;
use magnetar_core::model::*;
use magnetar_core::query::{PROBE_FACETS, SearchPage, SearchQuery};
use magnetar_core::store::Store;
use magnetar_server::PgStore;
use magnetar_server::search_index::{self, SearchIndex, index_query, load};

fn q(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        min_trust: Some(0),
        descending: true,
        ..Default::default()
    }
}

fn temp_index_dir() -> PathBuf {
    std::env::temp_dir().join(format!("magnetar-ix-{:016x}", rand::random::<u64>()))
}

struct Ix {
    idx: Arc<SearchIndex>,
    dir: PathBuf,
}

impl Drop for Ix {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn index_for(store: &PgStore) -> Ix {
    let dir = temp_index_dir();
    let id = load::database_id(store.pool()).await.unwrap();
    let idx = SearchIndex::open_async(dir.clone(), id).await.unwrap();
    search_index::build(store.pool(), &idx).await.unwrap();
    assert!(idx.is_ready());
    store.attach_search_index(idx.clone());
    Ix { idx, dir }
}

async fn sync(store: &PgStore, ix: &Ix) {
    while search_index::apply_changes(store.pool(), &ix.idx)
        .await
        .unwrap()
        > 0
    {}
}

fn pg_only(store: &PgStore) -> PgStore {
    PgStore::from_pool(store.pool().clone())
}

async fn dht_row(s: &PgStore, n: u64, title: &str, category: &str) {
    sqlx::query(
        "INSERT INTO dht_torrents (infohash, title, file_count, total_size, category,
             metadata_size, first_seen, fetched_at, discovered_via, classifier_version)
         VALUES ($1, $2, 1, 1000, $3, 100, $4, $4, 'sample', 1)",
    )
    .bind(ih(n))
    .bind(title)
    .bind(category)
    .bind(NOW - n as i64)
    .execute(s.pool())
    .await
    .unwrap();
}

async fn probe(s: &PgStore, n: u64, tags: &[&str]) {
    let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
    sqlx::query(
        "INSERT INTO probe_attrs (infohash, probe_version, probed_at, status, files_probed,
             files_verified, bytes_in, tags) VALUES ($1, 1, $2, 'ok', 1, 1, 1000, $3)",
    )
    .bind(ih(n))
    .bind(NOW)
    .bind(tags)
    .execute(s.pool())
    .await
    .unwrap();
}

fn hashes(p: &SearchPage) -> BTreeSet<String> {
    p.hits.iter().map(|h| h.infohash.clone()).collect()
}

fn set(ns: &[u64]) -> BTreeSet<String> {
    ns.iter().map(|n| ih(*n)).collect()
}

/// The corpus: four probed torrents (DHT-only and Nostr + DHT), one DHT row and one Nostr
/// event never probed.
async fn corpus(s: &PgStore) {
    dht_row(s, 1, "film one 2160p", "video").await;
    probe(
        s,
        1,
        &[
            "k:video",
            "v:hevc",
            "r:2160p",
            "h:dolby_vision",
            "h:hdr10",
            "ac:truehd",
            "x:atmos",
            "a:eng",
            "a:fre",
            "s:eng",
        ],
    )
    .await;
    dht_row(s, 2, "film two", "video").await;
    probe(
        s,
        2,
        &[
            "k:video", "v:h264", "r:1080p", "ac:aac", "a:jpn", "s:eng", "s:fre",
        ],
    )
    .await;
    dht_row(s, 3, "film three", "video").await;
    probe(
        s,
        3,
        &[
            "k:video", "v:hevc", "r:2160p", "h:hdr10", "ac:eac3", "a:eng",
        ],
    )
    .await;
    dht_row(s, 4, "film four never probed", "video").await;
    put(
        s,
        torrent(5, 1, NOW - 50, 5, "film five nostr only").cat(Category::Video),
    )
    .await;
    put(
        s,
        torrent(6, 1, NOW - 60, 6, "film six both 720p").cat(Category::Video),
    )
    .await;
    dht_row(s, 6, "film six both dht", "video").await;
    probe(s, 6, &["k:video", "v:av1", "r:720p", "ac:opus", "a:fre"]).await;
}

/// Every probe filter selects the same torrents on the index and on the Postgres fallback;
/// fields combine with AND, values within a field with OR; codes are normalised.
#[tokio::test]
async fn probe_filters_match_on_index_and_postgres() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    corpus(s).await;
    let ix = index_for(s).await;
    let pg = pg_only(s);
    let cases: Vec<(&str, SearchQuery, Vec<u64>)> = vec![
        ("none", q("film"), vec![1, 2, 3, 4, 5, 6]),
        (
            "audio fr (639-1)",
            SearchQuery {
                probe_languages: vec!["fr".into()],
                ..q("film")
            },
            vec![1, 6],
        ),
        (
            "audio fra/jpn (639-2/T and /B)",
            SearchQuery {
                probe_languages: vec!["FRA".into(), "jpn".into()],
                ..q("film")
            },
            vec![1, 2, 6],
        ),
        (
            "subs fre",
            SearchQuery {
                probe_subtitles: vec!["fre".into()],
                ..q("film")
            },
            vec![2],
        ),
        (
            "any hdr",
            SearchQuery {
                probe_hdr: vec!["any".into()],
                ..q("film")
            },
            vec![1, 3],
        ),
        (
            "dolby vision",
            SearchQuery {
                probe_hdr: vec!["dolby_vision".into()],
                ..q("film")
            },
            vec![1],
        ),
        (
            "4k and up",
            SearchQuery {
                probe_resolutions: vec!["2160p".into()],
                ..q("film")
            },
            vec![1, 3],
        ),
        (
            "1080p and up",
            SearchQuery {
                probe_resolutions: vec!["2160p".into(), "1080p".into()],
                ..q("film")
            },
            vec![1, 2, 3],
        ),
        (
            "atmos",
            SearchQuery {
                probe_audio: vec!["atmos".into()],
                ..q("film")
            },
            vec![1],
        ),
        (
            "codec truehd or opus",
            SearchQuery {
                probe_audio: vec!["truehd".into(), "opus".into()],
                ..q("film")
            },
            vec![1, 6],
        ),
        (
            "hevc and english",
            SearchQuery {
                probe_video_codecs: vec!["hevc".into()],
                probe_languages: vec!["eng".into()],
                ..q("film")
            },
            vec![1, 3],
        ),
        (
            "hdr and french",
            SearchQuery {
                probe_hdr: vec!["any".into()],
                probe_languages: vec!["fre".into()],
                ..q("film")
            },
            vec![1],
        ),
        (
            "unknown code matches nothing",
            SearchQuery {
                probe_languages: vec!["xx".into()],
                ..q("film")
            },
            vec![],
        ),
        (
            "with a D44 filter (the name says 720p)",
            SearchQuery {
                resolutions: vec!["720p".into()],
                probe_languages: vec!["fre".into()],
                ..q("film")
            },
            vec![6],
        ),
        (
            // The app's default browse: no text, no source chip. The D76 browse rule would
            // hide these DHT-only torrents (no seed check yet); a probe filter turns it off.
            "browse, no source chip",
            SearchQuery {
                probe_hdr: vec!["any".into()],
                ..q("")
            },
            vec![1, 3],
        ),
        (
            "browse, no source chip, no probe filter: D76 still hides unchecked DHT rows",
            q(""),
            vec![5, 6],
        ),
        (
            "browse, explicit DHT source",
            SearchQuery {
                probe_hdr: vec!["any".into()],
                origins: vec!["dht".into()],
                ..q("")
            },
            vec![1, 3],
        ),
    ];
    for (name, query, want) in cases {
        let a = s.search(&query, NOW).await.unwrap();
        let b = pg.search(&query, NOW).await.unwrap();
        assert_eq!(hashes(&a), set(&want), "index, {name}");
        assert_eq!(hashes(&b), set(&want), "postgres, {name}");
        assert_eq!(a.total, want.len() as u64, "index total, {name}");
    }
    // The query string of GET /api/v1/search.
    let parsed = magnetar_server::api::parse_search_query(Some(
        "q=film&probe_lang=fr,ENG&probe_sub=fre&probe_res=2160p&probe_codec=hevc&probe_hdr=any\
         &probe_audio=atmos",
    ))
    .unwrap();
    assert_eq!(parsed.probe_languages, vec!["fr", "eng"]);
    assert_eq!(parsed.probe_subtitles, vec!["fre"]);
    assert_eq!(parsed.probe_resolutions, vec!["2160p"]);
    assert_eq!(parsed.probe_video_codecs, vec!["hevc"]);
    assert_eq!(parsed.probe_hdr, vec!["any"]);
    assert_eq!(parsed.probe_audio, vec!["atmos"]);
    assert!(
        magnetar_core::api::features::ALL.contains(&magnetar_core::api::features::PROBE_FILTERS)
    );
    drop(ix);
}

/// Facet counts of the probe facets leave out their own filter (D44 rule), so the filter
/// sheet shows what each other value would give.
#[tokio::test]
async fn probe_facets_count_disjunctively() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    corpus(s).await;
    let ix = index_for(s).await;
    let fields: Vec<String> = PROBE_FACETS.iter().map(|f| f.to_string()).collect();
    let query = SearchQuery {
        probe_hdr: vec!["any".into()],
        ..q("film")
    };
    let set = s
        .facet_set(&query, &fields, 20, NOW)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(set.total, 2, "films one and three have HDR");
    let counts = |name: &str| -> Vec<(String, u64)> {
        let f = set.facets.iter().find(|f| f.field == name).unwrap();
        let mut v: Vec<(String, u64)> = f
            .counts
            .iter()
            .map(|c| (c.value.clone(), c.count))
            .collect();
        v.sort();
        v
    };
    let pairs = |v: &[(&str, u64)]| -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = v.iter().map(|(a, n)| (a.to_string(), *n)).collect();
        v.sort();
        v
    };
    // Its own filter left out: every probed film counts (the SDR ones have no value).
    assert_eq!(
        counts("probe_hdr"),
        pairs(&[("any", 2), ("dolby_vision", 1), ("hdr10", 2)])
    );
    // The others apply it.
    assert_eq!(counts("probe_language"), pairs(&[("eng", 2), ("fre", 1)]));
    assert_eq!(counts("probe_resolution"), pairs(&[("2160p", 2)]));
    assert_eq!(counts("probe_codec"), pairs(&[("hevc", 2)]));
    assert_eq!(
        counts("probe_audio"),
        pairs(&[("atmos", 1), ("eac3", 1), ("truehd", 1)])
    );
    assert_eq!(counts("probe_subtitles"), pairs(&[("eng", 1)]));
    // No filter: every probed torrent counts under its values.
    let all = s
        .facet_set(&q("film"), &fields, 20, NOW)
        .await
        .unwrap()
        .unwrap();
    let res = all
        .facets
        .iter()
        .find(|f| f.field == "probe_resolution")
        .unwrap();
    let mut got: Vec<(String, u64)> = res
        .counts
        .iter()
        .map(|c| (c.value.clone(), c.count))
        .collect();
    got.sort();
    assert_eq!(got, pairs(&[("1080p", 1), ("2160p", 2), ("720p", 1)]));
    drop(ix);
}

/// A probe stored, replaced or removed after the index was built changes what the filters
/// find: `probe_attrs` writes go through the change log (fails without the D122 triggers).
#[tokio::test]
async fn probe_changes_reach_the_index() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    corpus(s).await;
    let ix = index_for(s).await;
    // The corpus's own log rows first: re-applied later, they would re-read every row and
    // hide a missing trigger.
    sync(s, &ix).await;
    let dv = SearchQuery {
        probe_hdr: vec!["dolby_vision".into()],
        ..q("film")
    };
    assert_eq!(hashes(&s.search(&dv, NOW).await.unwrap()), set(&[1]));
    // A new probe of a DHT row the index already holds.
    probe(s, 4, &["k:video", "v:hevc", "r:2160p", "h:dolby_vision"]).await;
    sync(s, &ix).await;
    assert_eq!(hashes(&s.search(&dv, NOW).await.unwrap()), set(&[1, 4]));
    // A re-probe replacing the row (as `probes::store`'s upsert does).
    sqlx::query("UPDATE probe_attrs SET tags = ARRAY['k:video', 'r:1080p'] WHERE infohash = $1")
        .bind(ih(1))
        .execute(s.pool())
        .await
        .unwrap();
    sync(s, &ix).await;
    assert_eq!(hashes(&s.search(&dv, NOW).await.unwrap()), set(&[4]));
    // Removed (a token leak's `supplied_by` cleanup deletes the probe row).
    sqlx::query("DELETE FROM probe_attrs WHERE infohash = $1")
        .bind(ih(4))
        .execute(s.pool())
        .await
        .unwrap();
    sync(s, &ix).await;
    assert!(s.search(&dv, NOW).await.unwrap().hits.is_empty());
    assert_eq!(
        hashes(&pg_only(s).search(&dv, NOW).await.unwrap()),
        BTreeSet::new()
    );
    drop(ix);
}

/// The migration recomputes the `r:` tag of stored probes from the main file's width and
/// height (D122: scope UHD films were `r:1080p`); other rows and tags stay as they were.
#[tokio::test]
async fn stored_rows_get_the_width_aware_class() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let video = |i: u32, size: u64, w: u32, h: u32| {
        serde_json::json!({"i": i, "size": size, "label": "video.mkv", "confidence": 100,
            "verified": true, "video": {"container": "matroska",
            "video": [{"codec": "hevc", "width": w, "height": h}]}})
    };
    let rows = [
        // Scope UHD as the main file, a small 1080p sample beside it.
        (
            1u64,
            vec!["k:video", "v:hevc", "r:1080p", "a:eng"],
            serde_json::json!([video(0, 10, 1920, 1080), video(1, 9000, 3840, 1606)]),
        ),
        // 16:9 HD stays 1080p.
        (
            2,
            vec!["k:video", "v:h264", "r:1080p"],
            serde_json::json!([video(0, 5000, 1920, 1080)]),
        ),
        // Scope HD (1920×800) is 1080p, not 720p.
        (
            3,
            vec!["k:video", "r:720p"],
            serde_json::json!([video(0, 5000, 1920, 800)]),
        ),
        // No video track: untouched.
        (
            4,
            vec!["k:game", "p:ps5"],
            serde_json::json!([{"i": 0, "size": 100, "label": "game.ps5.pkg",
                "confidence": 100, "verified": true}]),
        ),
    ];
    for (n, tags, files) in &rows {
        dht_row(s, *n, "x", "video").await;
        probe(s, *n, tags).await;
        sqlx::query("INSERT INTO probe_files (infohash, files) VALUES ($1, $2)")
            .bind(ih(*n))
            .bind(files)
            .execute(s.pool())
            .await
            .unwrap();
    }
    let m = magnetar_server::store::MIGRATOR
        .iter()
        .find(|m| m.version == 20261108220000)
        .expect("the D122 migration");
    sqlx::raw_sql(sqlx::AssertSqlSafe(m.sql.as_str().to_string()))
        .execute(s.pool())
        .await
        .unwrap();
    let tags = async |n: u64| -> BTreeSet<String> {
        let t: Vec<String> = sqlx::query_scalar("SELECT tags FROM probe_attrs WHERE infohash = $1")
            .bind(ih(n))
            .fetch_one(s.pool())
            .await
            .unwrap();
        t.into_iter().collect()
    };
    let want = |v: &[&str]| -> BTreeSet<String> { v.iter().map(|s| s.to_string()).collect() };
    assert_eq!(
        tags(1).await,
        want(&["k:video", "v:hevc", "r:2160p", "a:eng"])
    );
    assert_eq!(tags(2).await, want(&["k:video", "v:h264", "r:1080p"]));
    assert_eq!(tags(3).await, want(&["k:video", "r:1080p"]));
    assert_eq!(tags(4).await, want(&["k:game", "p:ps5"]));
}

/// A probe stored as `probes::store` writes it: the tags of `probes::summary` over one video
/// file whose tracks are `video` (the `ProbeVideo` JSON a worker sends).
async fn probe_video(s: &PgStore, n: u64, video: serde_json::Value) {
    let report = magnetar_server::fleet::proto::ProbeReport {
        v: magnetar_core::probe::PROBE_VERSION,
        status: "ok".into(),
        files: vec![ProbeFile {
            i: 0,
            size: 1000,
            label: "video.mkv".into(),
            confidence: 100,
            verified: true,
            video: Some(serde_json::from_value(video).unwrap()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let summary = magnetar_server::probes::summary(&report);
    let tags: Vec<&str> = summary.tags.iter().map(String::as_str).collect();
    probe(s, n, &tags).await;
}

/// The D125 corpus: T1 English SDH (PGS) + French subtitles, French audio + an English
/// commentary; T2 French SDH, French audio + French audio description; T3 French forced
/// subtitles only, English audio; T4 a version-1 probe without kinds (French subtitles,
/// English audio). Spec §4.6.
async fn kinds_corpus(s: &PgStore) {
    use serde_json::json;
    for (n, title) in [
        (11, "kinds one"),
        (12, "kinds two"),
        (13, "kinds three"),
        (14, "kinds four"),
    ] {
        dht_row(s, n, title, "video").await;
    }
    let v = |audio: serde_json::Value, subs: serde_json::Value| {
        json!({"container": "matroska",
               "video": [{"codec": "h264", "width": 1920, "height": 1080}],
               "audio": audio, "subs": subs})
    };
    probe_video(
        s,
        11,
        v(
            json!([{"codec": "eac3", "channels": 6, "lang": "fre"},
                   {"codec": "ac3", "channels": 2, "lang": "eng", "kinds": ["commentary"],
                    "name": "Director's Commentary"}]),
            json!([{"codec": "hdmv_pgs_subtitle", "lang": "eng", "kinds": ["sdh"]},
                   {"codec": "subrip", "lang": "fre"}]),
        ),
    )
    .await;
    probe_video(
        s,
        12,
        v(
            json!([{"codec": "eac3", "channels": 6, "lang": "fre"},
                   {"codec": "aac", "channels": 2, "lang": "fre", "kinds": ["description"]}]),
            json!([{"codec": "subrip", "lang": "fre", "kinds": ["sdh"]}]),
        ),
    )
    .await;
    probe_video(
        s,
        13,
        v(
            json!([{"codec": "aac", "channels": 2, "lang": "eng"}]),
            json!([{"codec": "subrip", "lang": "fre", "kinds": ["forced"]}]),
        ),
    )
    .await;
    probe_video(
        s,
        14,
        v(
            json!([{"codec": "aac", "channels": 2, "lang": "eng"}]),
            json!([{"codec": "subrip", "lang": "fre"}]),
        ),
    )
    .await;
}

/// D125: the track-kind filters select the same torrents on the index and on the Postgres
/// fallback; a language with a kind of the same family is one track having both (French +
/// SDH finds a French SDH track, not French subtitles beside English SDH); forced subtitles
/// and commentary audio do not count as a language (D124).
#[tokio::test]
async fn probe_kind_filters_match_on_index_and_postgres() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    kinds_corpus(s).await;
    let ix = index_for(s).await;
    let pg = pg_only(s);
    let strs = |v: &[&str]| -> Vec<String> { v.iter().map(|x| x.to_string()).collect() };
    let cases: Vec<(&str, SearchQuery, Vec<u64>)> = vec![
        ("none", q("kinds"), vec![11, 12, 13, 14]),
        (
            "probe_sub=fre",
            SearchQuery {
                probe_subtitles: strs(&["fre"]),
                ..q("kinds")
            },
            vec![11, 12, 14],
        ),
        (
            "probe_sub_kind=sdh",
            SearchQuery {
                probe_sub_kinds: strs(&["sdh"]),
                ..q("kinds")
            },
            vec![11, 12],
        ),
        (
            "probe_sub=fre + probe_sub_kind=sdh (T1's SDH is English)",
            SearchQuery {
                probe_subtitles: strs(&["fre"]),
                probe_sub_kinds: strs(&["sdh"]),
                ..q("kinds")
            },
            vec![12],
        ),
        (
            "probe_sub=fre + probe_sub_kind=forced",
            SearchQuery {
                probe_subtitles: strs(&["fr"]),
                probe_sub_kinds: strs(&["FORCED"]),
                ..q("kinds")
            },
            vec![13],
        ),
        (
            "probe_sub=fre,eng + probe_sub_kind=sdh",
            SearchQuery {
                probe_subtitles: strs(&["fre", "eng"]),
                probe_sub_kinds: strs(&["sdh"]),
                ..q("kinds")
            },
            vec![11, 12],
        ),
        (
            "probe_sub_format=image",
            SearchQuery {
                probe_sub_formats: strs(&["image"]),
                ..q("kinds")
            },
            vec![11],
        ),
        (
            "probe_lang=eng (T1's English track is a commentary)",
            SearchQuery {
                probe_languages: strs(&["eng"]),
                ..q("kinds")
            },
            vec![13, 14],
        ),
        (
            "probe_audio_kind=commentary",
            SearchQuery {
                probe_audio_kinds: strs(&["commentary"]),
                ..q("kinds")
            },
            vec![11],
        ),
        (
            "probe_lang=fre + probe_audio_kind=description",
            SearchQuery {
                probe_languages: strs(&["fre"]),
                probe_audio_kinds: strs(&["description"]),
                ..q("kinds")
            },
            vec![12],
        ),
        (
            "probe_lang=eng + probe_audio_kind=commentary",
            SearchQuery {
                probe_languages: strs(&["eng"]),
                probe_audio_kinds: strs(&["commentary"]),
                ..q("kinds")
            },
            vec![11],
        ),
        (
            "unknown kind matches nothing",
            SearchQuery {
                probe_sub_kinds: strs(&["hardsub"]),
                ..q("kinds")
            },
            vec![],
        ),
        (
            "browse: a kind filter turns the D76 rule off like any probe filter",
            SearchQuery {
                probe_sub_kinds: strs(&["sdh"]),
                ..q("")
            },
            vec![11, 12],
        ),
    ];
    for (name, query, want) in cases {
        let a = s.search(&query, NOW).await.unwrap();
        let b = pg.search(&query, NOW).await.unwrap();
        assert_eq!(hashes(&a), set(&want), "index, {name}");
        assert_eq!(hashes(&b), set(&want), "postgres, {name}");
        assert_eq!(a.total, want.len() as u64, "index total, {name}");
    }
    // The query string of GET /api/v1/search.
    let parsed = magnetar_server::api::parse_search_query(Some(
        "q=kinds&probe_sub_kind=sdh&probe_audio_kind=commentary&probe_sub_format=text\
         &probe_sub_kind=forced,cc",
    ))
    .unwrap();
    assert_eq!(parsed.probe_sub_kinds, vec!["sdh", "forced", "cc"]);
    assert_eq!(parsed.probe_audio_kinds, vec!["commentary"]);
    assert_eq!(parsed.probe_sub_formats, vec!["text"]);
    assert!(parsed.needs_probe_kind_feature());
    drop(ix);
}

/// D125: with a language + kind pair filter, the language and kind facets count tracks:
/// each value's count is what picking it as well would give (French + SDH: the kind facet
/// counts French tracks' kinds, the language facet SDH tracks' languages).
#[tokio::test]
async fn probe_kind_facets_count_per_track() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    kinds_corpus(s).await;
    let ix = index_for(s).await;
    let fields: Vec<String> = PROBE_FACETS.iter().map(|f| f.to_string()).collect();
    let counts = |set: &FacetSet, name: &str| -> Vec<(String, u64)> {
        let f = set.facets.iter().find(|f| f.field == name).unwrap();
        let mut v: Vec<(String, u64)> = f
            .counts
            .iter()
            .map(|c| (c.value.clone(), c.count))
            .collect();
        v.sort();
        v
    };
    let pairs = |v: &[(&str, u64)]| -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = v.iter().map(|(a, n)| (a.to_string(), *n)).collect();
        v.sort();
        v
    };
    let query = SearchQuery {
        probe_subtitles: vec!["fre".into()],
        probe_sub_kinds: vec!["sdh".into()],
        ..q("kinds")
    };
    let set = s
        .facet_set(&query, &fields, 20, NOW)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(set.total, 1, "T2");
    assert_eq!(
        counts(&set, "probe_sub_kind"),
        pairs(&[("sdh", 1), ("forced", 1)])
    );
    assert_eq!(
        counts(&set, "probe_subtitles"),
        pairs(&[("eng", 1), ("fre", 1)])
    );
    // Facets of other fields apply the pair filter.
    assert_eq!(counts(&set, "probe_sub_format"), pairs(&[("text", 1)]));
    assert_eq!(
        counts(&set, "probe_audio_kind"),
        pairs(&[("description", 1)])
    );
    // Picking a counted value gives that count.
    for (subs, kinds, want) in [
        (vec!["fre"], vec!["forced"], 1),
        (vec!["eng"], vec!["sdh"], 1),
        (vec!["fre"], vec!["sdh", "forced"], 2),
        (vec!["fre", "eng"], vec!["sdh"], 2),
    ] {
        let q2 = SearchQuery {
            probe_subtitles: subs.iter().map(|x| x.to_string()).collect(),
            probe_sub_kinds: kinds.iter().map(|x| x.to_string()).collect(),
            ..q("kinds")
        };
        assert_eq!(
            s.search(&q2, NOW).await.unwrap().total,
            want,
            "{subs:?} {kinds:?}"
        );
    }
    // The audio family the same way: English + commentary.
    let query = SearchQuery {
        probe_languages: vec!["eng".into()],
        probe_audio_kinds: vec!["commentary".into()],
        ..q("kinds")
    };
    let set = s
        .facet_set(&query, &fields, 20, NOW)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(set.total, 1, "T1");
    assert_eq!(counts(&set, "probe_language"), pairs(&[("eng", 1)]));
    assert_eq!(
        counts(&set, "probe_audio_kind"),
        pairs(&[("commentary", 1)])
    );
    // Only one side set (review of D125): the other side's chips are counted on the pair
    // column too, because picking one makes a pair filter. A kind alone: SDH matches T1 and
    // T2, and the language chips say what French + SDH or English + SDH return (one each,
    // not fre 2 from T2's French SDH plus T1's unrelated French SRT).
    let only_kind = SearchQuery {
        probe_sub_kinds: vec!["sdh".into()],
        ..q("kinds")
    };
    let set = s
        .facet_set(&only_kind, &fields, 20, NOW)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(set.total, 2);
    assert_eq!(
        counts(&set, "probe_sub_kind"),
        pairs(&[("sdh", 2), ("forced", 1)])
    );
    assert_eq!(
        counts(&set, "probe_subtitles"),
        pairs(&[("eng", 1), ("fre", 1)])
    );
    // A forced kind alone: T3, whose forced French track has no `s:fre` tag (D124); the
    // French chip is still offered, and picking it returns T3.
    let set = s
        .facet_set(
            &SearchQuery {
                probe_sub_kinds: vec!["forced".into()],
                ..q("kinds")
            },
            &fields,
            20,
            NOW,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(set.total, 1);
    assert_eq!(counts(&set, "probe_subtitles"), pairs(&[("fre", 1)]));
    // A language alone: French subtitles match T1, T2 and T4; the kind chips say what
    // French + SDH ({T2}) and French + forced ({T3}) return.
    let only_lang = SearchQuery {
        probe_subtitles: vec!["fre".into()],
        ..q("kinds")
    };
    let set = s
        .facet_set(&only_lang, &fields, 20, NOW)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(set.total, 3);
    assert_eq!(
        counts(&set, "probe_sub_kind"),
        pairs(&[("sdh", 1), ("forced", 1)])
    );
    // The same language facet is counted as before, on `s:` (D122).
    assert_eq!(
        counts(&set, "probe_subtitles"),
        pairs(&[("eng", 1), ("fre", 3)])
    );
    // Audio: English alone; the commentary chip says what English + commentary returns.
    let set = s
        .facet_set(
            &SearchQuery {
                probe_languages: vec!["eng".into()],
                ..q("kinds")
            },
            &fields,
            20,
            NOW,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(set.total, 2, "T3, T4");
    assert_eq!(
        counts(&set, "probe_audio_kind"),
        pairs(&[("commentary", 1)])
    );
    // Every chip above predicts the total of picking it.
    for (subs, kinds, want) in [
        (vec!["eng"], vec!["sdh"], 1),
        (vec!["fre"], vec!["sdh"], 1),
        (vec!["fre"], vec!["forced"], 1),
    ] {
        let q2 = SearchQuery {
            probe_subtitles: subs.iter().map(|x| x.to_string()).collect(),
            probe_sub_kinds: kinds.iter().map(|x| x.to_string()).collect(),
            ..q("kinds")
        };
        assert_eq!(s.search(&q2, NOW).await.unwrap().total, want);
    }
    drop(ix);
}

/// D123–D125 end to end, from real file bytes: the ffmpeg-made MP4 fixture (two audio tracks:
/// "Commentary" by name and a `describes-video` description; two mov_text subtitles: forced
/// by a `kind` box and SDH by `tagc`) goes through sniff, the worker's `report_of`, the
/// server's `store` (validation, summary, tags), the detail API JSON the app decodes, and the
/// kind filters on the index and on the Postgres fallback.
#[tokio::test]
async fn track_kinds_end_to_end_from_file_bytes() {
    use magnetar_core::probe::{FileResult, ProbeResult, ProbeStatus};
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../sniff/tests/video_fixtures/ffmpeg_kinds.mp4"),
    )
    .unwrap();
    let size = bytes.len() as u64;
    let file = sniff::SparseFile::from_bytes(bytes);
    let det = sniff::Detector::new()
        .caps(sniff::video::recommended_caps())
        .detect(&file);
    let det = det.detection().expect("detected").clone();
    let result = ProbeResult {
        status: ProbeStatus::Ok,
        files: vec![FileResult {
            index: 0,
            size,
            detection: det,
            verified: true,
            agreed: 1,
            notes: Vec::new(),
        }],
        bytes_in: size,
        uploaded: 0,
        peers_used: 1,
        unchoked: true,
        incomplete: false,
        surprises: 0,
    };
    // The worker's side: what it uploads.
    let report = magnetar_server::probes::report_of(&result).expect("a storable report");
    assert_eq!(report.v, 2);
    let v = report.files[0].video.as_ref().expect("video");
    let audio: Vec<&[String]> = v.audio.iter().map(|a| a.kinds.as_slice()).collect();
    let subs: Vec<&[String]> = v.subs.iter().map(|s| s.kinds.as_slice()).collect();
    assert_eq!(audio, [&["commentary"][..], &["description"]], "{v:?}");
    assert_eq!(subs, [&["forced"][..], &["sdh"]], "{v:?}");
    assert_eq!(v.audio[0].name.as_deref(), Some("Commentary"));

    // The server's side: validated and stored for a DHT row with that one file.
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let h = ih(31);
    crate::probe_pg::dht_row(s, &h, "Kinds.Fixture.2026", "video", &[("Kinds.mp4", size)]).await;
    let o = magnetar_server::probes::store(
        s.pool(),
        "w1",
        vec![magnetar_server::probes::Upload {
            infohash: h.clone(),
            report: report.clone(),
        }],
        NOW,
    )
    .await
    .unwrap();
    assert_eq!((o.stored, o.rejected), (1, 0));

    // The detail JSON carries kinds and the name (the app decodes `kinds`, never shows `name`).
    let base = spawn_server(s).await;
    let d = crate::probe_pg::detail(&base, &h).await;
    assert_eq!(d["probe"]["version"], 2, "{d}");
    let jv = &d["probe"]["files"][0]["video"];
    assert_eq!(
        jv["audio"][0]["kinds"],
        serde_json::json!(["commentary"]),
        "{d}"
    );
    assert_eq!(jv["audio"][0]["name"], "Commentary", "{d}");
    assert_eq!(
        jv["audio"][1]["kinds"],
        serde_json::json!(["description"]),
        "{d}"
    );
    assert_eq!(jv["subs"][0]["kinds"], serde_json::json!(["forced"]), "{d}");
    assert_eq!(jv["subs"][1]["kinds"], serde_json::json!(["sdh"]), "{d}");
    assert_eq!(jv["subs"][0]["codec"], "mov_text", "{d}");

    // Search: the kind and format filters find it on both paths; a kind it lacks does not.
    let ix = index_for(s).await;
    let pg = pg_only(s);
    let strs = |v: &[&str]| -> Vec<String> { v.iter().map(|x| x.to_string()).collect() };
    for (name, query, found) in [
        (
            "sub kind forced",
            SearchQuery {
                probe_sub_kinds: strs(&["forced"]),
                ..q("")
            },
            true,
        ),
        (
            "sub kind sdh + format text",
            SearchQuery {
                probe_sub_kinds: strs(&["sdh"]),
                probe_sub_formats: strs(&["text"]),
                ..q("")
            },
            true,
        ),
        (
            "audio kinds",
            SearchQuery {
                probe_audio_kinds: strs(&["description", "commentary"]),
                ..q("")
            },
            true,
        ),
        (
            "sub kind cc",
            SearchQuery {
                probe_sub_kinds: strs(&["cc"]),
                ..q("")
            },
            false,
        ),
        (
            "format image",
            SearchQuery {
                probe_sub_formats: strs(&["image"]),
                ..q("")
            },
            false,
        ),
    ] {
        for (path, page) in [
            ("index", s.search(&query, NOW).await.unwrap()),
            ("postgres", pg.search(&query, NOW).await.unwrap()),
        ] {
            let hit = page.hits.iter().any(|x| x.infohash == h);
            assert_eq!(hit, found, "{path}, {name}");
        }
    }
    drop(ix);
}

/// D125: `/health` lists `probe_kinds`, so the app offers the kind chips.
#[tokio::test]
async fn health_lists_probe_kinds() {
    use magnetar_core::api::{Health, features};
    assert!(features::ALL.contains(&features::PROBE_KINDS));
    let Some(db) = test_db().await else { return };
    let base = spawn_server(&db.store).await;
    let h: Health = magnetar_core::http::client(Duration::from_secs(20))
        .unwrap()
        .get(format!("{base}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(h.features.iter().any(|f| f == features::PROBE_KINDS));
    assert!(h.features.iter().any(|f| f == features::PROBE_FILTERS));
}

/// Probe tags like the D117 summary writes them, for a share of synthetic documents:
/// roughly the production mix of containers and tracks (MKV most, MP4 and AVI with few
/// languages), with rare values to test rare filters.
fn probe_tags_for(i: u64, probed_per_mille: u64) -> Vec<String> {
    let h = |salt: u64| {
        let mut x =
            i.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ salt.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x ^= x >> 31;
        x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 29)
    };
    if h(1) % 1000 >= probed_per_mille {
        return Vec::new();
    }
    let mut t = vec!["k:video".to_string()];
    t.push(
        match h(2) % 20 {
            0..=8 => "v:hevc",
            9..=17 => "v:h264",
            _ => "v:av1",
        }
        .into(),
    );
    let res = match h(3) % 20 {
        0..=4 => "2160p",
        5..=14 => "1080p",
        15..=18 => "720p",
        _ => "sd",
    };
    t.push(format!("r:{res}"));
    if res == "2160p" {
        match h(4) % 10 {
            0..=2 => {
                t.push("h:dolby_vision".into());
                t.push("h:hdr10".into());
            }
            3..=8 => t.push("h:hdr10".into()),
            _ => {}
        }
    }
    t.push(
        match h(5) % 10 {
            0..=3 => "ac:eac3",
            4..=6 => "ac:aac",
            7 => "ac:truehd",
            8 => "ac:dts",
            _ => "ac:ac3",
        }
        .into(),
    );
    if h(6) % 20 == 0 {
        t.push("x:atmos".into());
    }
    // About 60 % of video probes carry audio languages (MKV 89 %, MP4 39 %, AVI / TS 0 %).
    if h(7) % 10 < 6 {
        t.push("a:eng".into());
        if h(8) % 5 == 0 {
            t.push("a:fre".into());
        }
        if h(9) % 50 == 0 {
            t.push("a:jpn".into());
        }
        // Rare: Icelandic, about 1 in 2000 language-tagged probes.
        if h(10) % 2000 == 0 {
            t.push("a:ice".into());
        }
        if h(11) % 2 == 0 {
            t.push("s:eng".into());
        }
        if h(12) % 6 == 0 {
            t.push("s:fre".into());
        }
    }
    // Track kinds (D124) on about 30 % of probed documents: SDH and forced subtitles most,
    // commentary and audio description less, as releases carry them.
    if h(13) % 10 < 3 {
        let sub = match h(14) % 10 {
            0..=4 => "sdh",
            5..=7 => "forced",
            8 => "signs",
            _ => "cc",
        };
        let lang = if h(15) % 4 == 0 { "fre" } else { "eng" };
        t.push(format!("sk:{sub}"));
        t.push(format!("sp:{lang}:{sub}"));
        if h(16) % 3 == 0 {
            let audio = if h(17) % 3 == 0 {
                "description"
            } else {
                "commentary"
            };
            t.push(format!("ak:{audio}"));
            t.push(format!("ap:eng:{audio}"));
        }
    }
    // Subtitle format class of probed documents with subtitles.
    if t.iter()
        .any(|x| x.starts_with("s:") || x.starts_with("sk:"))
    {
        t.push(
            if h(18) % 3 == 0 {
                "sf:image"
            } else {
                "sf:text"
            }
            .into(),
        );
    }
    t
}

fn probe_queries() -> Vec<(&'static str, SearchQuery)> {
    let f = |text: &str| SearchQuery {
        // The app's empty query: no source chip. The D76 browse rule is on for the query
        // without a probe filter and off for the others (D122 review).
        ..q(text)
    };
    vec![
        ("browse, no probe filter", f("")),
        (
            "browse, common: audio eng",
            SearchQuery {
                probe_languages: vec!["eng".into()],
                ..f("")
            },
        ),
        (
            "browse, common: 2160p",
            SearchQuery {
                probe_resolutions: vec!["2160p".into()],
                ..f("")
            },
        ),
        (
            "browse, any hdr + fre",
            SearchQuery {
                probe_hdr: vec!["any".into()],
                probe_languages: vec!["fre".into()],
                ..f("")
            },
        ),
        (
            "browse, rare: ice + dolby vision",
            SearchQuery {
                probe_languages: vec!["ice".into()],
                probe_hdr: vec!["dolby_vision".into()],
                ..f("")
            },
        ),
        (
            "browse, none match: atmos + sd + av1 + ice",
            SearchQuery {
                probe_audio: vec!["atmos".into()],
                probe_resolutions: vec!["sd".into()],
                probe_video_codecs: vec!["av1".into()],
                probe_languages: vec!["ice".into()],
                ..f("")
            },
        ),
        (
            "text common word + 2160p hdr",
            SearchQuery {
                probe_resolutions: vec!["2160p".into()],
                probe_hdr: vec!["any".into()],
                ..f("the")
            },
        ),
        (
            "text rare word + audio fre",
            SearchQuery {
                probe_languages: vec!["fre".into()],
                ..f("chernobyl")
            },
        ),
        (
            "browse, kind: sdh",
            SearchQuery {
                probe_sub_kinds: vec!["sdh".into()],
                ..f("")
            },
        ),
        (
            "browse, pair: fre + sdh",
            SearchQuery {
                probe_subtitles: vec!["fre".into()],
                probe_sub_kinds: vec!["sdh".into()],
                ..f("")
            },
        ),
        (
            "browse, pair: eng + audio description",
            SearchQuery {
                probe_languages: vec!["eng".into()],
                probe_audio_kinds: vec!["description".into()],
                ..f("")
            },
        ),
        (
            "seeders sort + atmos",
            SearchQuery {
                probe_audio: vec!["atmos".into()],
                sort: magnetar_core::query::Sort::Seeders,
                ..f("")
            },
        ),
    ]
}

/// D122 measurement, index side, at production size: synthetic documents shaped like
/// releases (`search_index::synthetic_docs`) with probe tags on a share of them, then the
/// probe filters from common to rare, and the app's filter-sheet facet set (20 facets since
/// D125). Ignored (minutes to
/// build); env `MAGNETAR_BENCH_DOCS` (default 1 800 000), `MAGNETAR_BENCH_PROBED` (per mille
/// probed, default 100).
#[test]
#[ignore]
fn probe_filters_at_scale_in_memory() {
    let n: u64 = std::env::var("MAGNETAR_BENCH_DOCS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_800_000);
    let probed: u64 = std::env::var("MAGNETAR_BENCH_PROBED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let dir = temp_index_dir();
    let idx = SearchIndex::open(&dir, "probe-bench").unwrap();
    assert_eq!(idx.begin_build().unwrap(), "");
    let t = Instant::now();
    let mut with_probe = 0u64;
    let mut chunk = Vec::with_capacity(5000);
    for mut d in crate::search_index::synthetic_doc_iter(n) {
        let i = u64::from_str_radix(&d.infohash, 16).unwrap();
        d.probe_tags = probe_tags_for(i, probed);
        if !d.probe_tags.is_empty() {
            with_probe += 1;
            // Probes exist only on DHT rows.
            if d.origin == Origin::Nostr {
                d.origin = Origin::Both;
            }
        }
        chunk.push(d);
        if chunk.len() == 5000 {
            let cursor = chunk.last().unwrap().infohash.clone();
            idx.add_build_chunk(std::mem::take(&mut chunk), &cursor, false)
                .unwrap();
        }
    }
    if let Some(last) = chunk.last() {
        let cursor = last.infohash.clone();
        idx.add_build_chunk(chunk, &cursor, false).unwrap();
    }
    idx.finish_build(0).unwrap();
    eprintln!(
        "built {n} docs ({with_probe} probed) in {:.1} s",
        t.elapsed().as_secs_f64()
    );
    // The app's sheet refresh (SearchViewModel.facets): category, origin, the nine name
    // facets of FACET_FIELDS, then the nine probe facets (six of D122, three of D125): 20.
    let fields: Vec<String> = [
        "category",
        "origin",
        "language",
        "subtitles",
        "resolution",
        "source",
        "codec",
        "hdr",
        "audio",
        "genre",
        "media_kind",
    ]
    .into_iter()
    .map(str::to_string)
    .chain(PROBE_FACETS.iter().map(|f| f.to_string()))
    .collect();
    for (name, sq) in probe_queries() {
        let iq = index_query(&sq, &sq.text, 0, None, NOW);
        let mut times = Vec::new();
        let mut total = 0;
        for _ in 0..7 {
            let t = Instant::now();
            total = idx.search(&iq).unwrap().total;
            times.push(t.elapsed());
        }
        times.sort();
        let mut facet_times = Vec::new();
        for _ in 0..3 {
            let t = Instant::now();
            idx.facet_set(&iq, &fields, 20).unwrap().unwrap();
            facet_times.push(t.elapsed());
        }
        facet_times.sort();
        eprintln!(
            "{name:>44}: {total:>8} hits, search p50 {:>7.2} ms max {:>7.2} ms, {} facets p50 {:>7.2} ms",
            ms(times[3]),
            ms(times[6]),
            fields.len(),
            ms(facet_times[1]),
        );
    }
    drop(idx);
    let _ = std::fs::remove_dir_all(&dir);
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// D122 measurement, Postgres side: the fallback's probe filters (index disabled or still
/// building) on `MAGNETAR_BENCH_DOCS` DHT rows (default 1 800 000) of which
/// `MAGNETAR_BENCH_PROBED` per mille are probed, timed and with `EXPLAIN (ANALYZE, BUFFERS)`
/// printed for the rare and the common filter. Ignored; uses a throwaway schema of
/// MAGNETAR_TEST_DATABASE_URL.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn probe_filters_fallback_explain_at_scale() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pool = s.pool();
    let n: i64 = std::env::var("MAGNETAR_BENCH_DOCS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_800_000);
    let probed: i64 = std::env::var("MAGNETAR_BENCH_PROBED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let t = Instant::now();
    let words = "the,lord,of,rings,game,thrones,night,day,world,war,love,story,city,dark,blue,\
                 film,serie,complete,french,multi,1080p,2160p,x265,hevc,bluray,web,dl";
    sqlx::query(
        "INSERT INTO dht_torrents (infohash, title, file_count, total_size, category,
             metadata_size, first_seen, fetched_at, discovered_via, classifier_version)
         SELECT lpad(to_hex(g), 40, '0'),
                array_to_string(ARRAY(SELECT w[1 + abs(hashtext(g::text || k::text)) % array_length(w, 1)]
                                      FROM generate_series(1, 4 + g % 4) k), '.'),
                1 + g % 30, 1000000 + g, 'video', 1000, $2 - g, $2 - g, 'sample', 2
         FROM generate_series(1, $1) g, (SELECT string_to_array($3, ',') AS w) words",
    )
    .bind(n)
    .bind(NOW)
    .bind(words)
    .execute(pool)
    .await
    .unwrap();
    // Two in three checked with peers, as in the index benchmark (the empty query lists
    // DHT-only torrents once a check found peers, D76).
    sqlx::query(
        "INSERT INTO liveness (infohash, seeders, leechers, dht_peers_est, source, trackers_ok,
             trackers_failed, checked_at, expires_at)
         SELECT lpad(to_hex(g), 40, '0'), g % 500, g % 50, NULL, 'udp', 1, 0, $2, $2 + 86400
         FROM generate_series(1, $1) g WHERE g % 3 <> 0",
    )
    .bind(n)
    .bind(NOW)
    .execute(pool)
    .await
    .unwrap();
    // Probe rows as the summary writes them, the same mix as `probe_tags_for`.
    let mut tx = pool.begin().await.unwrap();
    let mut batch: Vec<(String, Vec<String>)> = Vec::new();
    let mut probed_rows = 0u64;
    for g in 1..=n as u64 {
        let tags = probe_tags_for(g, probed as u64);
        if tags.is_empty() {
            continue;
        }
        batch.push((format!("{g:040x}"), tags));
        if batch.len() == 20_000 || g == n as u64 {
            probed_rows += batch.len() as u64;
            let (ihs, tags): (Vec<String>, Vec<String>) = batch
                .drain(..)
                .map(|(i, t)| (i, format!("{{{}}}", t.join(","))))
                .unzip();
            sqlx::query(
                "INSERT INTO probe_attrs (infohash, probe_version, probed_at, status, files_probed,
                     files_verified, bytes_in, tags)
                 SELECT ih, 1, 0, 'ok', 1, 1, 1000, t::text[] FROM unnest($1::text[], $2::text[]) u(ih, t)",
            )
            .bind(&ihs)
            .bind(&tags)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
    }
    tx.commit().await.unwrap();
    sqlx::query("ANALYZE").execute(pool).await.unwrap();
    eprintln!(
        "seeded {n} DHT rows, {probed_rows} probed, in {:.1} s",
        t.elapsed().as_secs_f64()
    );
    let pg = pg_only(s);
    for (name, sq) in probe_queries() {
        let t = Instant::now();
        let page = pg.search(&sq, NOW).await.unwrap();
        let took = t.elapsed();
        eprintln!(
            "{name:>44}: {:>5}{} hits, fallback {:>9.1} ms",
            page.total,
            if page.total_is_lower_bound { "+" } else { " " },
            ms(took)
        );
        if name.starts_with("browse, common: audio") || name.starts_with("browse, rare") {
            let mut conn = pool.acquire().await.unwrap();
            let plan = magnetar_server::search::explain_hits_analyze(&mut conn, &sq, NOW, 0)
                .await
                .unwrap();
            eprintln!("--- EXPLAIN ANALYZE {name}\n{}", plan.join("\n"));
        }
    }
}
