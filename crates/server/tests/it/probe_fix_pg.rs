//! Track corrections (D128–D131, docs/PROBES.md §12) against a scratch schema: the API writes
//! them over the stored detection, the detail and the stored tags carry the effective tracks,
//! both search paths and the facet counts follow through the change log, a re-probe (D127's
//! v1 → v2 included) keeps them, and the stats count the detector's accuracy. Needs
//! MAGNETAR_TEST_DATABASE_URL (else SKIPPED). Loopback only.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use magnetar_core::api::ProbeCorrectionRequest;
use magnetar_core::model::{ProbeFile, ProbeInfo};
use magnetar_core::query::SearchQuery;
use magnetar_core::store::Store;
use magnetar_server::PgStore;
use magnetar_server::fleet::proto::ProbeReport;
use magnetar_server::search_index::{self, SearchIndex, load};
use magnetar_server::{probe_fix, probes};
use serde_json::{Value as J, json};

use crate::common::*;

const SIZE: u64 = 6_000_000_000;

fn q(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        min_trust: Some(0),
        descending: true,
        ..Default::default()
    }
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
    let dir = std::env::temp_dir().join(format!("magnetar-ix-{:016x}", rand::random::<u64>()));
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

/// A DHT row of one file, `{title}.mkv`.
async fn dht_row(s: &PgStore, n: u64, title: &str) {
    sqlx::query(
        "INSERT INTO dht_torrents (infohash, title, file_count, total_size, category,
             metadata_size, first_seen, fetched_at, discovered_via, classifier_version, files)
         VALUES ($1, $2, 1, $3, 'video', 100, $4, $4, 'sample', 1, $5)",
    )
    .bind(ih(n))
    .bind(title)
    .bind(SIZE as i64)
    .bind(NOW - n as i64)
    .bind(json!([{"path": format!("{title}.mkv"), "size": SIZE}]))
    .execute(s.pool())
    .await
    .unwrap();
}

fn report(v: u16, video: J) -> ProbeReport {
    ProbeReport {
        v,
        status: "ok".into(),
        bytes_in: 262_144,
        uploaded: 0,
        peers: 1,
        files: vec![ProbeFile {
            i: 0,
            size: SIZE,
            label: "video.mkv".into(),
            confidence: 97,
            verified: true,
            video: Some(serde_json::from_value(video).unwrap()),
            ..Default::default()
        }],
    }
}

/// Store a report as a worker's upload is stored (`probes::store`): (stored, rejected).
async fn store(s: &PgStore, n: u64, r: ProbeReport) -> (u32, u32) {
    let o = probes::store(
        s.pool(),
        "eee",
        vec![probes::Upload {
            infohash: ih(n),
            report: r,
        }],
        NOW,
    )
    .await
    .unwrap();
    (o.stored, o.rejected)
}

fn movie() -> J {
    json!({"container": "matroska",
        "video": [{"codec": "hevc", "width": 3840, "height": 2160}],
        "audio": [
            {"codec": "eac3", "channels": 6, "lang": "fre", "atmos": true},
            {"codec": "ac3", "channels": 2, "lang": "eng", "kinds": ["commentary"],
             "name": "Commentary"},
            {"codec": "aac", "channels": 2, "lang": "eng"}],
        "subs": [
            {"codec": "subrip", "lang": "fre", "kinds": ["forced"]},
            {"codec": "hdmv_pgs_subtitle", "lang": "eng", "kinds": ["sdh"],
             "name": "English SDH"}]})
}

async fn tags(s: &PgStore, n: u64) -> Vec<String> {
    sqlx::query_scalar("SELECT tags FROM probe_attrs WHERE infohash = $1")
        .bind(ih(n))
        .fetch_one(s.pool())
        .await
        .unwrap()
}

fn has(tags: &[String], t: &str) -> bool {
    tags.iter().any(|x| x == t)
}

fn corr(items: J) -> ProbeCorrectionRequest {
    serde_json::from_value(json!({"device": "android", "items": items})).unwrap()
}

async fn post(base: &str, n: u64, body: &J, token: Option<&str>) -> (u16, J) {
    let mut rb = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/torrents/{}/probe/corrections",
            ih(n)
        ))
        .json(body);
    if let Some(t) = token {
        rb = rb.bearer_auth(t);
    }
    let r = rb.send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(J::Null))
}

async fn delete(base: &str, n: u64, id: i64) -> (u16, J) {
    let r = reqwest::Client::new()
        .delete(format!(
            "{base}/api/v1/torrents/{}/probe/corrections/{id}",
            ih(n)
        ))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(J::Null))
}

async fn get(base: &str, path: &str) -> J {
    reqwest::Client::new()
        .get(format!("{base}{path}"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn hashes(p: &magnetar_core::query::SearchPage) -> BTreeSet<String> {
    p.hits.iter().map(|h| h.infohash.clone()).collect()
}

/// Both search paths find exactly `want` for `query`.
async fn finds(s: &PgStore, query: &SearchQuery, want: &[u64], what: &str) {
    let want: BTreeSet<String> = want.iter().map(|n| ih(*n)).collect();
    assert_eq!(
        hashes(&s.search(query, NOW).await.unwrap()),
        want,
        "index: {what}"
    );
    assert_eq!(
        hashes(&pg_only(s).search(query, NOW).await.unwrap()),
        want,
        "postgres: {what}"
    );
}

async fn facet(s: &PgStore, query: &SearchQuery, name: &str) -> Vec<(String, u64)> {
    let set = s
        .facet_set(query, &[name.to_string()], 20, NOW)
        .await
        .unwrap()
        .unwrap();
    let f = set.facets.iter().find(|f| f.field == name).unwrap();
    let mut v: Vec<(String, u64)> = f
        .counts
        .iter()
        .map(|c| (c.value.clone(), c.count))
        .collect();
    v.sort();
    v
}

fn pairs(v: &[(&str, u64)]) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = v.iter().map(|(a, n)| (a.to_string(), *n)).collect();
    v.sort();
    v
}

/// POST, the detail, both search paths, the facet counts, DELETE: the user's VLC check
/// becomes the effective tracks everywhere, and undo goes back to the detection.
#[tokio::test]
async fn corrections_end_to_end() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    dht_row(s, 1, "Film One 2024").await;
    dht_row(s, 2, "Film Two 2024").await;
    assert_eq!(store(s, 1, report(2, movie())).await, (1, 0));
    assert_eq!(store(s, 2, report(2, movie())).await, (1, 0));
    let ix = index_for(s).await;
    sync(s, &ix).await;
    let base = spawn_server(s).await;
    let ger_sdh = SearchQuery {
        probe_subtitles: vec!["ger".into()],
        probe_sub_kinds: vec!["sdh".into()],
        ..q("film")
    };
    let eng_audio = SearchQuery {
        probe_languages: vec!["eng".into()],
        ..q("film")
    };
    finds(s, &ger_sdh, &[], "no German SDH yet").await;
    finds(s, &eng_audio, &[1, 2], "English audio (the AAC track)").await;

    let (status, p) = post(
        &base,
        1,
        &json!({"device": "android", "items": [
            {"file": 0, "track": "sub", "pos": 1, "set": ["lang"], "lang": "de"},
            {"file": 0, "track": "audio", "pos": 0, "set": []},
            {"file": 0, "track": "audio", "pos": 2, "removed": true},
            {"file": 0, "track": "sub", "set": ["lang", "kinds", "name"], "lang": "spa",
             "kinds": ["forced"], "name": "Español (forzados)"}
        ]}),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200, "{p}");
    let f = &p["files"][0];
    assert_eq!(f["dht_index"], 0);
    assert_eq!(f["path"], "Film One 2024.mkv");
    let audio = &f["video"]["audio"];
    let subs = &f["video"]["subs"];
    // The check: no detection shown, fields always sent.
    assert_eq!(audio[0]["pos"], 0);
    assert_eq!(audio[0]["fix"]["fields"], json!([]));
    assert_eq!(audio[0]["fix"]["by"], "android");
    assert!(audio[0]["fix"].get("detected").is_none(), "{p}");
    assert!(audio[1].get("fix").is_none() && audio[1]["pos"] == 1);
    // Removed: kept, marked, with what it was.
    assert_eq!(audio[2]["fix"]["removed"], true);
    assert_eq!(audio[2]["fix"]["detected"], json!({"lang": "eng"}));
    // Corrected: effective value, the detection kept.
    assert_eq!(subs[1]["lang"], "ger");
    assert_eq!(subs[1]["kinds"], json!(["sdh"]));
    assert_eq!(subs[1]["fix"]["fields"], json!(["lang"]));
    assert_eq!(
        subs[1]["fix"]["detected"],
        json!({"lang": "eng", "kinds": ["sdh"], "name": "English SDH"})
    );
    // Added: after the detected tracks, no codec, no position.
    assert_eq!(subs.as_array().unwrap().len(), 3);
    assert_eq!(subs[2]["codec"], "");
    assert!(subs[2].get("pos").is_none());
    assert_eq!(subs[2]["fix"]["added"], true);
    assert_eq!(subs[2]["name"], "Español (forzados)");
    assert!(p.get("unplaced").is_none());
    // The detail says the same.
    let d = get(&base, &format!("/api/v1/torrents/{}", ih(1))).await;
    assert_eq!(d["probe"], p);
    // probe_files keeps the detection; probe_attrs carries the effective tracks.
    let raw: sqlx::types::Json<Vec<ProbeFile>> =
        sqlx::query_scalar("SELECT files FROM probe_files WHERE infohash = $1")
            .bind(ih(1))
            .fetch_one(s.pool())
            .await
            .unwrap();
    let rv = raw.0[0].video.as_ref().unwrap();
    assert_eq!(rv.subs.len(), 2);
    assert_eq!(rv.subs[1].lang.as_deref(), Some("eng"));
    let t = tags(s, 1).await;
    assert!(has(&t, "s:ger") && has(&t, "sp:ger:sdh") && has(&t, "sp:spa:forced"));
    assert!(
        !has(&t, "s:eng") && !has(&t, "a:eng") && !has(&t, "ac:aac"),
        "{t:?}"
    );

    sync(s, &ix).await;
    finds(s, &ger_sdh, &[1], "German SDH after the correction").await;
    finds(s, &eng_audio, &[2], "the English AAC track does not exist").await;
    let spa_forced = SearchQuery {
        probe_subtitles: vec!["spa".into()],
        probe_sub_kinds: vec!["forced".into()],
        ..q("film")
    };
    finds(s, &spa_forced, &[1], "the added Spanish forced track").await;
    // Facets count the corrected values.
    assert_eq!(
        facet(s, &q("film"), "probe_subtitles").await,
        pairs(&[("eng", 1), ("ger", 1)])
    );
    assert_eq!(
        facet(s, &q("film"), "probe_audio").await,
        pairs(&[("aac", 1), ("ac3", 2), ("atmos", 2), ("eac3", 2)])
    );
    assert_eq!(
        facet(s, &q("film"), "probe_language").await,
        pairs(&[("eng", 1), ("fre", 2)])
    );

    // Undo the subtitle correction: back to the detection.
    let id = subs[1]["fix"]["id"].as_i64().unwrap();
    let (status, p) = delete(&base, 1, id).await;
    assert_eq!(status, 200, "{p}");
    assert_eq!(p["files"][0]["video"]["subs"][1]["lang"], "eng");
    assert!(p["files"][0]["video"]["subs"][1].get("fix").is_none());
    sync(s, &ix).await;
    finds(s, &ger_sdh, &[], "undone").await;
    let (status, e) = delete(&base, 1, id).await;
    assert_eq!((status, e["error"].as_str()), (404, Some("not_found")));
    // Edit the added track by id: the language only, the rest kept.
    let added = p["files"][0]["video"]["subs"][2]["fix"]["id"]
        .as_i64()
        .unwrap();
    let (status, p) = post(
        &base,
        1,
        &json!({"items": [{"id": added, "file": 0, "track": "sub", "set": ["lang"],
                           "lang": "cat"}]}),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200, "{p}");
    let a = &p["files"][0]["video"]["subs"][2];
    assert_eq!(
        (a["lang"].as_str(), a["fix"]["by"].as_str()),
        (Some("cat"), Some("api"))
    );
    assert_eq!(a["kinds"], json!(["forced"]));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM probe_corrections")
        .fetch_one(s.pool())
        .await
        .unwrap();
    assert_eq!(rows, 3, "check, removal, added (edited in place)");
    // The DHT row deleted: its corrections go with it.
    sqlx::query("DELETE FROM dht_torrents WHERE infohash = $1")
        .bind(ih(1))
        .execute(s.pool())
        .await
        .unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM probe_corrections")
        .fetch_one(s.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0);
    drop(ix);
}

/// The whole cycle on one torrent, through the API: a language and a kind corrected on one
/// subtitle track and a language on an audio track → the filters (index and Postgres) and the
/// facet counts follow → a re-probe with another track list moves the subtitle correction to
/// its track's new position and flags the audio one for review (the detector changed its mind),
/// the stored tags keeping both → "Still correct" (the same item again) clears the review →
/// undo gives the detection back everywhere.
#[tokio::test]
async fn corrections_full_cycle() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    dht_row(s, 10, "Film Ten 2024").await;
    dht_row(s, 11, "Film Eleven 2024").await;
    assert_eq!(store(s, 10, report(2, movie())).await, (1, 0));
    assert_eq!(store(s, 11, report(2, movie())).await, (1, 0));
    let ix = index_for(s).await;
    sync(s, &ix).await;
    let base = spawn_server(s).await;
    let sub_q = |lang: &str, kind: &str| SearchQuery {
        probe_subtitles: vec![lang.into()],
        probe_sub_kinds: vec![kind.into()],
        ..q("film")
    };
    let audio_q = |lang: &str| SearchQuery {
        probe_languages: vec![lang.into()],
        ..q("film")
    };
    finds(s, &sub_q("ita", "sdh"), &[], "no Italian SDH yet").await;
    finds(
        s,
        &sub_q("fre", "forced"),
        &[10, 11],
        "French forced, detected",
    )
    .await;
    finds(s, &audio_q("spa"), &[], "no Spanish audio yet").await;
    assert_eq!(
        facet(s, &q("film"), "probe_sub_kind").await,
        pairs(&[("forced", 2), ("sdh", 2)])
    );

    // The user's VLC check: subtitle track 1 is Italian SDH, not French forced; audio track 1
    // is Spanish.
    let item_sub = json!({"file": 0, "track": "sub", "pos": 0, "set": ["lang", "kinds"],
                          "lang": "it", "kinds": ["sdh"]});
    let item_audio = json!({"file": 0, "track": "audio", "pos": 0, "set": ["lang"],
                            "lang": "spa"});
    let (status, p) = post(
        &base,
        10,
        &json!({"device": "android", "items": [item_sub, item_audio]}),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200, "{p}");
    let sub = &p["files"][0]["video"]["subs"][0];
    assert_eq!(
        (sub["lang"].as_str(), &sub["kinds"]),
        (Some("ita"), &json!(["sdh"]))
    );
    assert_eq!(sub["fix"]["fields"], json!(["lang", "kinds"]));
    let sub_id = sub["fix"]["id"].as_i64().unwrap();
    let audio_id = p["files"][0]["video"]["audio"][0]["fix"]["id"]
        .as_i64()
        .unwrap();
    sync(s, &ix).await;
    finds(
        s,
        &sub_q("ita", "sdh"),
        &[10],
        "Italian SDH after the correction",
    )
    .await;
    finds(
        s,
        &sub_q("fre", "forced"),
        &[11],
        "torrent 10 lost French forced",
    )
    .await;
    finds(s, &audio_q("spa"), &[10], "Spanish audio").await;
    assert_eq!(
        facet(s, &q("film"), "probe_sub_kind").await,
        pairs(&[("forced", 1), ("sdh", 2)])
    );
    assert_eq!(
        facet(s, &q("film"), "probe_subtitles").await,
        // A forced track is not "subtitles in French" (D124); the SDH one is.
        pairs(&[("eng", 2), ("ita", 1)])
    );
    assert_eq!(
        facet(s, &q("film"), "probe_language").await,
        pairs(&[("eng", 2), ("fre", 1), ("spa", 1)])
    );

    // A re-probe lists a new subtitle track first (the corrected one moves to position 1) and
    // reads the first audio track as German now.
    let mut again = movie();
    again["subs"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!({"codec": "ass", "lang": "eng"}));
    again["audio"][0]["lang"] = json!("ger");
    assert_eq!(store(s, 10, report(2, again.clone())).await, (1, 0));
    let d = get(&base, &format!("/api/v1/torrents/{}", ih(10))).await;
    let v = &d["probe"]["files"][0]["video"];
    assert!(v["subs"][0].get("fix").is_none(), "{v}");
    let moved = &v["subs"][1];
    assert_eq!(
        (
            moved["pos"].as_i64(),
            moved["lang"].as_str(),
            moved["fix"]["id"].as_i64()
        ),
        (Some(1), Some("ita"), Some(sub_id)),
        "{v}"
    );
    assert!(
        moved["fix"].get("review").is_none(),
        "same track, same detection"
    );
    let a0 = &v["audio"][0];
    assert_eq!(a0["lang"], "spa", "the override still applies");
    assert_eq!(a0["fix"]["review"], true, "{a0}");
    assert_eq!(a0["fix"]["detected"]["lang"], "ger");
    assert!(d["probe"].get("unplaced").is_none());
    let t = tags(s, 10).await;
    assert!(
        has(&t, "sp:ita:sdh") && has(&t, "a:spa") && !has(&t, "a:ger"),
        "{t:?}"
    );
    sync(s, &ix).await;
    finds(s, &sub_q("ita", "sdh"), &[10], "after the re-probe").await;
    finds(
        s,
        &audio_q("ger"),
        &[],
        "the override hides the new reading",
    )
    .await;
    finds(s, &audio_q("spa"), &[10], "after the re-probe").await;

    // "Still correct": the same item again refreshes the snapshot and clears the review.
    let (status, p) = post(
        &base,
        10,
        &json!({"items": [{"file": 0, "track": "audio", "pos": 0, "set": ["lang"],
                           "lang": "spa"}]}),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200, "{p}");
    let a0 = &p["files"][0]["video"]["audio"][0];
    assert_eq!(a0["fix"]["id"].as_i64(), Some(audio_id), "edited in place");
    assert!(a0["fix"].get("review").is_none(), "{a0}");

    // Undo both: the detection everywhere.
    assert_eq!(delete(&base, 10, sub_id).await.0, 200);
    let (status, p) = delete(&base, 10, audio_id).await;
    assert_eq!(status, 200, "{p}");
    let v = &p["files"][0]["video"];
    assert_eq!(
        (v["subs"][1]["lang"].as_str(), &v["subs"][1]["kinds"]),
        (Some("fre"), &json!(["forced"]))
    );
    assert_eq!(v["audio"][0]["lang"], "ger");
    assert!(v["subs"][1].get("fix").is_none() && v["audio"][0].get("fix").is_none());
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM probe_corrections WHERE infohash = $1")
            .bind(ih(10))
            .fetch_one(s.pool())
            .await
            .unwrap();
    assert_eq!(rows, 0);
    let t = tags(s, 10).await;
    let want = probes::summary(&report(2, again)).tags;
    assert_eq!(t, want, "the tags of the detection");
    sync(s, &ix).await;
    finds(s, &sub_q("ita", "sdh"), &[], "undone").await;
    finds(s, &sub_q("fre", "forced"), &[10, 11], "undone").await;
    finds(s, &audio_q("ger"), &[10], "the detection").await;
    finds(s, &audio_q("spa"), &[], "undone").await;
    assert_eq!(
        facet(s, &q("film"), "probe_sub_kind").await,
        pairs(&[("forced", 2), ("sdh", 2)])
    );
    drop(ix);
}

/// A version-1 probe corrected, then probed again by D127 as version 2 with the TS
/// teletext expansion (D123): the correction follows its track to its new position, the
/// stored tags keep it, `probe_files` holds the raw detection. A refused weaker re-probe and a
/// probe deleted then stored again keep them too. Fails without the store path's `apply`.
#[tokio::test]
async fn corrections_survive_reprobes() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    dht_row(s, 3, "Film Three TS").await;
    let v1 = json!({"container": "mpegts",
        "video": [{"codec": "h264", "width": 1920, "height": 1080}],
        "audio": [{"codec": "mp2", "channels": 2, "lang": "dut"}],
        "subs": [{"codec": "dvb_teletext", "lang": "eng"},
                 {"codec": "dvb_subtitle", "lang": "fre"}]});
    assert_eq!(store(s, 3, report(1, v1)).await, (1, 0));
    probe_fix::correct(
        s.pool(),
        &ih(3),
        &corr(json!([
            {"file": 0, "track": "sub", "pos": 0, "set": ["lang"], "lang": "dut"},
            {"file": 0, "track": "sub", "pos": 1, "set": ["kinds"], "kinds": ["forced"]}
        ])),
        NOW,
    )
    .await
    .unwrap();
    let t = tags(s, 3).await;
    assert!(
        has(&t, "s:dut") && has(&t, "sp:fre:forced") && !has(&t, "s:eng"),
        "{t:?}"
    );

    // D127: the v2 re-probe expands the teletext pages; the English one is now second.
    let v2 = json!({"container": "mpegts",
        "video": [{"codec": "h264", "width": 1920, "height": 1080}],
        "audio": [{"codec": "mp2", "channels": 2, "lang": "dut"}],
        "subs": [{"codec": "dvb_teletext", "lang": "fre"},
                 {"codec": "dvb_teletext", "lang": "eng", "kinds": ["sdh"], "name": "888"},
                 {"codec": "dvb_teletext", "lang": "ger"},
                 {"codec": "dvb_subtitle", "lang": "fre"}]});
    assert_eq!(store(s, 3, report(2, v2.clone())).await, (1, 0));
    let t = tags(s, 3).await;
    assert!(has(&t, "s:dut") && has(&t, "sp:dut:sdh"), "{t:?}");
    assert!(
        has(&t, "sp:fre:forced") && has(&t, "s:fre") && has(&t, "s:ger"),
        "{t:?}"
    );
    assert!(!has(&t, "s:eng") && !has(&t, "sp:eng:sdh"), "{t:?}");
    let (version, sub_langs): (i16, Vec<String>) =
        sqlx::query_as("SELECT probe_version, sub_langs FROM probe_attrs WHERE infohash = $1")
            .bind(ih(3))
            .fetch_one(s.pool())
            .await
            .unwrap();
    assert_eq!(version, 2);
    assert_eq!(sub_langs, ["fre", "dut", "ger"]);
    let raw: sqlx::types::Json<Vec<ProbeFile>> =
        sqlx::query_scalar("SELECT files FROM probe_files WHERE infohash = $1")
            .bind(ih(3))
            .fetch_one(s.pool())
            .await
            .unwrap();
    let rv = raw.0[0].video.as_ref().unwrap();
    assert_eq!(
        rv.subs[1].lang.as_deref(),
        Some("eng"),
        "raw detection stored"
    );
    assert!(rv.subs.iter().all(|t| t.fix.is_none() && t.pos.is_none()));
    // The detail: the correction on its new position, the change since the check flagged.
    let detail = s.torrent_detail(&ih(3), NOW).await.unwrap().unwrap();
    let p: ProbeInfo = detail.probe.unwrap();
    let subs = &p.files[0].video.as_ref().unwrap().subs;
    assert_eq!(subs[1].lang.as_deref(), Some("dut"));
    let f = subs[1].fix.as_ref().unwrap();
    assert_eq!(f.detected.as_ref().unwrap().lang.as_deref(), Some("eng"));
    assert!(!f.review, "a version-1 snapshot compares the language only");
    assert_eq!(subs[3].kinds, ["forced"]);
    assert!(p.unplaced.is_empty());

    // A weaker re-probe of the same version is refused and changes nothing.
    let mut weak = report(2, v2.clone());
    weak.files[0].verified = false;
    assert_eq!(store(s, 3, weak).await, (0, 1));
    assert!(has(&tags(s, 3).await, "s:dut"));
    // A probe deleted (worker token leak cleanup) and stored again gets them back.
    sqlx::query("DELETE FROM probe_attrs WHERE infohash = $1")
        .bind(ih(3))
        .execute(s.pool())
        .await
        .unwrap();
    assert_eq!(store(s, 3, report(2, v2)).await, (1, 0));
    let t = tags(s, 3).await;
    assert!(has(&t, "s:dut") && !has(&t, "s:eng"), "{t:?}");
}

/// A correction re-indexes its torrent through `probe_attrs` and the D122 triggers (D43); a
/// pure confirmation changes no tag and logs nothing. Fails without the `probe_attrs` UPDATE.
#[tokio::test]
async fn correction_reaches_the_index_through_the_change_log() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    dht_row(s, 4, "Film Four").await;
    assert_eq!(store(s, 4, report(2, movie())).await, (1, 0));
    let ix = index_for(s).await;
    sync(s, &ix).await;
    let ita = SearchQuery {
        probe_subtitles: vec!["ita".into()],
        ..q("film")
    };
    let log = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM search_changes WHERE infohash = $1")
            .bind(ih(4))
            .fetch_one(s.pool())
            .await
            .unwrap()
    };
    let before = log().await;
    probe_fix::correct(
        s.pool(),
        &ih(4),
        &corr(json!([{"file": 0, "track": "sub", "pos": 1, "set": []},
                     {"file": 0, "track": "audio", "pos": 0, "set": ["lang"], "lang": "fre"}])),
        NOW,
    )
    .await
    .unwrap();
    assert_eq!(log().await, before, "checks and equal values log no change");
    probe_fix::correct(
        s.pool(),
        &ih(4),
        &corr(json!([{"file": 0, "track": "sub", "pos": 1, "set": ["lang"], "lang": "it"}])),
        NOW,
    )
    .await
    .unwrap();
    assert!(log().await > before, "the tag change is logged");
    // The Postgres fallback reads the table at once; the index after the change-log pass.
    assert_eq!(
        hashes(&pg_only(s).search(&ita, NOW).await.unwrap()),
        BTreeSet::from([ih(4)])
    );
    sync(s, &ix).await;
    assert_eq!(
        hashes(&s.search(&ita, NOW).await.unwrap()),
        BTreeSet::from([ih(4)])
    );
    let id: i64 = sqlx::query_scalar("SELECT id FROM probe_corrections WHERE set_lang")
        .fetch_one(s.pool())
        .await
        .unwrap();
    probe_fix::remove(s.pool(), &ih(4), id).await.unwrap();
    sync(s, &ix).await;
    assert!(s.search(&ita, NOW).await.unwrap().hits.is_empty());
    drop(ix);
}

/// D130: `/health` lists `probe_corrections`, so the app shows the edit actions.
#[tokio::test]
async fn health_lists_probe_corrections() {
    use magnetar_core::api::{Health, features};
    assert!(features::ALL.contains(&features::PROBE_CORRECTIONS));
    let Some(db) = test_db().await else { return };
    let base = spawn_server(&db.store).await;
    let h: Health = reqwest::Client::new()
        .get(format!("{base}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(h.features.iter().any(|f| f == features::PROBE_CORRECTIONS));
}

/// D130: `/api/v1/stats` carries the accuracy block, zero counts included, from the
/// snapshots and the kind sources computed at write time.
#[tokio::test]
async fn correction_stats_in_api_stats() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let base = spawn_server(s).await;
    let st = get(&base, "/api/v1/stats").await;
    assert_eq!(st["probe_corrections"]["tracks"], 0, "{st}");
    assert_eq!(st["probe_corrections"]["kinds"], json!([]));
    dht_row(s, 5, "Film Five").await;
    assert_eq!(store(s, 5, report(2, movie())).await, (1, 0));
    let (status, p) = post(
        &base,
        5,
        &json!({"items": [
            {"file": 0, "track": "sub", "pos": 1, "set": ["kinds"]},
            {"file": 0, "track": "sub", "pos": 0, "set": []},
            {"file": 0, "track": "audio", "pos": 1, "set": ["name"], "name": "Commentaire"},
            {"file": 0, "track": "audio", "pos": 2, "removed": true},
            {"file": 0, "track": "sub", "set": ["lang"], "lang": "spa"}
        ]}),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 200, "{p}");
    let st = get(&base, "/api/v1/stats").await;
    let c = &st["probe_corrections"];
    assert_eq!(
        (
            &c["tracks"],
            &c["checked"],
            &c["corrected"],
            &c["removed"],
            &c["added"],
            &c["names_changed"]
        ),
        (
            &json!(4),
            &json!(1),
            &json!(2),
            &json!(1),
            &json!(1),
            &json!(1)
        ),
        "{c}"
    );
    assert_eq!(
        c["languages"],
        json!({"checked": 3, "wrong": 0, "filled": 0})
    );
    assert_eq!(
        c["kinds"],
        json!([
            {"track": "sub", "kind": "sdh", "source": "name", "checked": 1, "wrong": 1},
            {"track": "audio", "kind": "commentary", "source": "name", "checked": 1, "wrong": 0},
            {"track": "sub", "kind": "forced", "source": "flags", "checked": 1, "wrong": 0}
        ])
    );
    assert_eq!(c["missed"], json!([]));
    assert!(c["at"].as_i64().unwrap() > 0);
}

/// The routes need the token, a probed torrent and a valid request; a request with one bad
/// item writes nothing.
#[tokio::test]
async fn corrections_need_the_token_and_a_probe() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let base = spawn_server(s).await;
    dht_row(s, 6, "Film Six").await;
    dht_row(s, 7, "Film Seven never probed").await;
    assert_eq!(store(s, 6, report(2, movie())).await, (1, 0));
    let ok = json!({"items": [{"file": 0, "track": "sub", "pos": 0, "set": []}]});
    assert_eq!(post(&base, 6, &ok, None).await.0, 401);
    assert_eq!(post(&base, 6, &ok, Some("wrong")).await.0, 401);
    let r = reqwest::Client::new()
        .delete(format!(
            "{base}/api/v1/torrents/{}/probe/corrections/1",
            ih(6)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
    let (status, e) = post(&base, 7, &ok, Some(TOKEN)).await;
    assert_eq!(
        (status, e["message"].as_str()),
        (404, Some("no probe for this torrent"))
    );
    let (status, e) = post(&base, 8, &ok, Some(TOKEN)).await;
    assert_eq!(
        (status, e["message"].as_str()),
        (404, Some("unknown infohash"))
    );
    let (status, e) = delete(&base, 7, 1).await;
    assert_eq!((status, e["error"].as_str()), (404, Some("not_found")));
    let r = reqwest::Client::new()
        .post(format!("{base}/api/v1/torrents/xyz/probe/corrections"))
        .bearer_auth(TOKEN)
        .json(&ok)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
    // All or nothing.
    let (status, e) = post(
        &base,
        6,
        &json!({"items": [
            {"file": 0, "track": "sub", "pos": 0, "set": ["lang"], "lang": "ger"},
            {"file": 0, "track": "sub", "pos": 1, "set": ["kinds"], "kinds": ["music"]}
        ]}),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, 400, "{e}");
    assert_eq!(e["error"], "invalid");
    assert_eq!(e["message"], "item 1: \"music\" is not a subtitle kind");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM probe_corrections")
        .fetch_one(s.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0);
    assert!(!has(&tags(s, 6).await, "s:ger"));
    // A body that is not JSON of the right shape.
    let (status, e) = post(&base, 6, &json!({"items": "x"}), Some(TOKEN)).await;
    assert_eq!((status, e["error"].as_str()), (422, Some("invalid_body")));
    let (status, _) = post(&base, 6, &ok, Some(TOKEN)).await;
    assert_eq!(status, 200);
}
