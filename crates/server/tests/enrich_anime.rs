//! Anime enrichment against Postgres (need MAGNETAR_TEST_DATABASE_URL, else SKIPPED).
//!
//! The shared `media` / `torrent_media` tables belong to the IMDb enrichment migration. Until
//! that branch is merged they are created here by [`contract_tables`], which mirrors the agreed
//! contract column for column (`IF NOT EXISTS`, so it is a no-op once the real migration runs).

mod common;

use std::path::PathBuf;

use clap::Parser;
use common::*;
use magnetar_core::model::Category;
use magnetar_server::config::{Cli, Config};
use magnetar_server::enrich_anime;
use sqlx::{AssertSqlSafe, Executor, PgPool};

/// The contract's media / torrent_media tables (owned by the IMDb migration).
pub async fn contract_tables(pool: &PgPool) {
    pool.execute(
        "CREATE TABLE IF NOT EXISTS media (
            id BIGSERIAL PRIMARY KEY,
            kind TEXT NOT NULL CHECK (kind IN ('movie','series','anime')),
            imdb TEXT UNIQUE,
            tmdb_movie INT, tmdb_tv INT, tvdb INT, mal INT, anilist INT, anidb INT,
            title TEXT NOT NULL, original_title TEXT, year INT, end_year INT,
            genres TEXT[] NOT NULL DEFAULT '{}', rating REAL, votes INT, runtime_min INT,
            updated_at BIGINT NOT NULL);
         CREATE UNIQUE INDEX IF NOT EXISTS media_tmdb_movie ON media (tmdb_movie) WHERE tmdb_movie IS NOT NULL;
         CREATE UNIQUE INDEX IF NOT EXISTS media_tmdb_tv ON media (tmdb_tv) WHERE tmdb_tv IS NOT NULL;
         CREATE UNIQUE INDEX IF NOT EXISTS media_tvdb ON media (tvdb) WHERE tvdb IS NOT NULL;
         CREATE UNIQUE INDEX IF NOT EXISTS media_mal ON media (mal) WHERE mal IS NOT NULL;
         CREATE UNIQUE INDEX IF NOT EXISTS media_anilist ON media (anilist) WHERE anilist IS NOT NULL;
         CREATE UNIQUE INDEX IF NOT EXISTS media_anidb ON media (anidb) WHERE anidb IS NOT NULL;
         CREATE TABLE IF NOT EXISTS torrent_media (
            infohash TEXT PRIMARY KEY,
            media_id BIGINT NOT NULL REFERENCES media ON DELETE CASCADE,
            season INT, episode INT,
            method TEXT NOT NULL CHECK (method IN ('itag','desc_link','offline_title','api_search','manual')),
            confidence REAL NOT NULL, matcher_version INT NOT NULL, matched_at BIGINT NOT NULL);
         CREATE INDEX IF NOT EXISTS torrent_media_media ON torrent_media (media_id);",
    )
    .await
    .unwrap();
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "magnetar-anime-{name}-{:016x}",
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn cfg(aod: &str, fribb: &str) -> Config {
    Cli::try_parse_from([
        "magnetar-server",
        "--anime-aod-url",
        aod,
        "--anime-fribb-url",
        fribb,
        "serve",
    ])
    .unwrap()
    .config
}

fn aod_line(mal: u32, title: &str, syn: &[&str], kind: &str, eps: u32, year: i32) -> String {
    serde_json::json!({
        "sources": [format!("https://myanimelist.net/anime/{mal}"), format!("https://anilist.co/anime/{}", mal + 500_000)],
        "title": title, "type": kind, "episodes": eps, "status": "FINISHED",
        "animeSeason": {"season": "FALL", "year": year},
        "duration": {"value": 1440, "unit": "SECONDS"},
        "score": {"median": 8.1},
        "synonyms": syn, "tags": ["action", "shounen", "school"],
    })
    .to_string()
}

/// A small real-shaped dataset padded with filler entries past the import sanity floors.
fn write_datasets(dir: &std::path::Path) -> (String, String) {
    let mut aod = vec![
        r#"{"$schema":"x","license":{"name":"ODbL 1.0"},"repository":"r","lastUpdate":"2026-10-25"}"#
            .to_string(),
        aod_line(40748, "Jujutsu Kaisen", &["JJK"], "TV", 24, 2020),
        aod_line(51009, "Jujutsu Kaisen 2nd Season", &[], "TV", 23, 2023),
        aod_line(57658, "Jujutsu Kaisen: Shimetsu Kaiyuu - Zenpen", &[], "TV", 12, 2026),
        aod_line(36028, "Golden Kamuy", &[], "TV", 12, 2018),
        aod_line(37989, "Golden Kamuy 2nd Season", &[], "TV", 12, 2018),
        aod_line(40059, "Golden Kamuy 3rd Season", &[], "TV", 12, 2020),
        aod_line(50528, "Golden Kamuy 4th Season", &[], "TV", 13, 2023),
        aod_line(52991, "Sousou no Frieren", &["Frieren: Beyond Journey's End"], "TV", 28, 2023),
    ];
    let mut fribb = vec![
        serde_json::json!({"type":"TV","mal_id":40748,"imdb_id":["tt12343534"],"themoviedb_id":{"tv":95479},"tvdb_id":377543,"season":{"tvdb":1,"tmdb":1}}),
        serde_json::json!({"type":"TV","mal_id":51009,"imdb_id":["tt12343534"],"themoviedb_id":{"tv":95479},"tvdb_id":377543,"season":{"tvdb":2,"tmdb":2}}),
        serde_json::json!({"type":"TV","mal_id":57658,"imdb_id":["tt12343534"],"themoviedb_id":{"tv":95479},"tvdb_id":377543,"season":{"tvdb":3,"tmdb":3}}),
        serde_json::json!({"type":"TV","mal_id":36028,"tvdb_id":331753,"season":{"tvdb":1}}),
        serde_json::json!({"type":"TV","mal_id":37989,"tvdb_id":331753,"season":{"tvdb":2}}),
        serde_json::json!({"type":"TV","mal_id":40059,"tvdb_id":331753,"season":{"tvdb":3}}),
        serde_json::json!({"type":"TV","mal_id":50528,"tvdb_id":331753,"season":{"tvdb":4}}),
        serde_json::json!({"type":"TV","mal_id":52991,"anidb_id":17617,"imdb_id":["tt22248376"],"themoviedb_id":{"tv":209867},"tvdb_id":424536,"season":{"tvdb":1,"tmdb":1}}),
    ];
    for i in 0..enrich_anime::MIN_AOD_ENTRIES as u32 {
        aod.push(aod_line(
            1_000_000 + i,
            &format!("Filler Show {i}"),
            &[],
            "TV",
            12,
            2000,
        ));
        fribb.push(serde_json::json!({"type":"TV","mal_id":1_000_000 + i}));
    }
    let aod_path = dir.join("aod.jsonl");
    let fribb_path = dir.join("anime-list-full.json");
    std::fs::write(&aod_path, aod.join("\n")).unwrap();
    std::fs::write(&fribb_path, serde_json::to_string(&fribb).unwrap()).unwrap();
    (
        aod_path.to_string_lossy().into_owned(),
        fribb_path.to_string_lossy().into_owned(),
    )
}

#[derive(sqlx::FromRow, Debug)]
struct Link {
    method: String,
    confidence: f32,
    season: Option<i32>,
    episode: Option<i32>,
    mal: Option<i32>,
    kind: String,
    imdb: Option<String>,
}

async fn link(pool: &PgPool, infohash: u64) -> Option<Link> {
    sqlx::query_as::<_, Link>(
        "SELECT tm.method, tm.confidence, tm.season, tm.episode, m.mal, m.kind, m.imdb
         FROM torrent_media tm JOIN media m ON m.id = tm.media_id WHERE tm.infohash = $1",
    )
    .bind(ih(infohash))
    .fetch_optional(pool)
    .await
    .unwrap()
}

fn anime_torrent(n: u64, title: &str) -> T {
    let mut t = torrent(n, 1, NOW + n as i64, n, title).cat(Category::Anime);
    t.p.category_path = vec!["anime".into(), "video".into()];
    t
}

#[tokio::test]
async fn imports_datasets_and_matches_torrents() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool().clone();
    let dir = scratch("e2e");
    let (aod, fribb) = write_datasets(&dir);
    let cfg = cfg(&aod, &fribb);

    // The media tables come from the enrich/imdb migration (D40); the mirror is a no-op.
    contract_tables(&pool).await;
    assert!(enrich_anime::contract_tables_exist(&pool).await.unwrap());

    // Another matcher already knows JJK as a series by its IMDb id.
    pool.execute(
        "INSERT INTO media (kind, imdb, title, year, updated_at)
         VALUES ('series', 'tt12343534', 'Jujutsu Kaisen', 2020, 1)",
    )
    .await
    .unwrap();

    put(
        &db.store,
        anime_torrent(
            1,
            "[ToonsHub] JUJUTSU KAISEN S03E01 1080p CR WEB-DL AAC2.0 H.264",
        ),
    )
    .await;
    put(
        &db.store,
        anime_torrent(2, "[SubsPlease] Golden Kamuy - 38 (1080p) [F1434F11].mkv"),
    )
    .await;
    put(
        &db.store,
        anime_torrent(3, "Some Unparseable Release")
            .desc("MAL: https://myanimelist.net/anime/51009/x"),
    )
    .await;
    // DHT-style: no name, no description, only files.
    put(
        &db.store,
        anime_torrent(4, &ih(4)).files(&[
            ("Kamuy/[SubsPlease] Golden Kamuy - 02 (1080p).mkv", 900),
            ("Kamuy/readme.txt", 5000),
        ]),
    )
    .await;
    let mut la = anime_torrent(5, "Jujutsu Kaisen S01E01");
    la.p.category_path = vec!["anime".into(), "video".into(), "live-action".into()];
    put(&db.store, la).await;
    put(
        &db.store,
        anime_torrent(6, "Totally Unknown Show - 01 (1080p)"),
    )
    .await;
    put(&db.store, anime_torrent(7, "Jujutsu Kaisen - 05 (1080p)")).await;
    put(
        &db.store,
        torrent(8, 1, NOW, 8, "Jujutsu Kaisen - 06").cat(Category::Video),
    )
    .await;
    put(
        &db.store,
        anime_torrent(9, "[Erai-raws] Frieren - Beyond Journey's End - 03 [1080p]"),
    )
    .await;
    // A manual link and a confident foreign link must survive.
    pool.execute(AssertSqlSafe(format!(
        "INSERT INTO media (kind, imdb, title, updated_at) VALUES ('movie', 'tt0000001', 'Manual pick', 1);
         INSERT INTO torrent_media (infohash, media_id, method, confidence, matcher_version, matched_at)
           SELECT '{}', id, 'manual', 1.0, 1, 1 FROM media WHERE imdb = 'tt0000001';
         INSERT INTO torrent_media (infohash, media_id, method, confidence, matcher_version, matched_at)
           SELECT '{}', id, 'desc_link', 0.99, 1, 1 FROM media WHERE imdb = 'tt0000001';",
        ih(7),
        ih(9)
    )))
    .await
    .unwrap();

    let r = enrich_anime::run_once(&pool, &cfg, false).await.unwrap();
    assert!(r.refresh.changed);
    assert_eq!(r.index_entries, 8 + enrich_anime::MIN_AOD_ENTRIES);
    assert_eq!(r.matching.scanned, 7, "{r:?}"); // not the live-action nor the video torrent
    assert_eq!(r.matching.kept, 2, "{r:?}");
    assert_eq!(r.matching.unmatched, 1, "{r:?}");

    let l = link(&pool, 1).await.unwrap();
    assert_eq!((l.mal, l.method.as_str()), (Some(57658), "offline_title"));
    assert_eq!((l.season, l.episode), (Some(3), Some(1)));
    assert_eq!(l.kind, "anime");
    // The sequel does not take the series' IMDb id: the adopted series row keeps it.
    assert_eq!(l.imdb, None);
    let l = link(&pool, 2).await.unwrap();
    assert_eq!(
        (l.mal, l.season, l.episode),
        (Some(50528), Some(4), Some(2))
    );
    let l = link(&pool, 3).await.unwrap();
    assert_eq!((l.mal, l.method.as_str()), (Some(51009), "desc_link"));
    assert!(l.confidence > 0.9);
    let l = link(&pool, 4).await.unwrap();
    assert_eq!((l.mal, l.episode), (Some(36028), Some(2)));
    assert!(link(&pool, 5).await.is_none());
    assert!(link(&pool, 6).await.is_none());
    assert_eq!(link(&pool, 7).await.unwrap().method, "manual");
    assert_eq!(link(&pool, 9).await.unwrap().method, "desc_link");
    assert!(link(&pool, 8).await.is_none());

    // The IMDb-matcher's JJK row was adopted: anime ids added, its kind and title kept.
    let (kind, mal, anilist): (String, Option<i32>, Option<i32>) =
        sqlx::query_as("SELECT kind, mal, anilist FROM media WHERE imdb = 'tt12343534'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((kind.as_str(), mal, anilist), ("series", None, None));
    // (Nothing matched season 1 itself; adoption happens when the season-1 entry is written.)
    put(
        &db.store,
        anime_torrent(10, "[SubsPlease] Jujutsu Kaisen - 01 (1080p)"),
    )
    .await;
    let r = enrich_anime::run_once(&pool, &cfg, false).await.unwrap();
    assert_eq!(r.matching.scanned, 1, "only the new torrent: {r:?}");
    assert!(r.refresh.datasets.iter().all(|d| d.outcome == "fresh"));
    let l = link(&pool, 10).await.unwrap();
    assert_eq!((l.mal, l.kind.as_str()), (Some(40748), "series"));
    assert_eq!(l.imdb.as_deref(), Some("tt12343534"));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM media WHERE imdb = 'tt12343534'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let (title, genres, runtime): (String, Vec<String>, Option<i32>) =
        sqlx::query_as("SELECT title, genres, runtime_min FROM media WHERE mal = 57658")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(title, "Jujutsu Kaisen: Shimetsu Kaiyuu - Zenpen");
    assert_eq!(genres, vec!["action", "shounen"]);
    assert_eq!(runtime, Some(24));

    // Forced refresh of identical files: nothing re-imported, nothing re-matched.
    let r = enrich_anime::run_once(&pool, &cfg, true).await.unwrap();
    assert!(!r.refresh.changed);
    assert!(
        r.refresh.datasets.iter().all(|d| d.outcome == "unchanged"),
        "{r:?}"
    );
    assert_eq!(r.matching.scanned, 0);

    // A truncated download never replaces a good import.
    std::fs::write(dir.join("bad.jsonl"), "{\"$schema\":\"x\"}\n").unwrap();
    let bad = cfg_with_aod(&dir.join("bad.jsonl"), &fribb);
    let r = enrich_anime::run_once(&pool, &bad, true).await.unwrap();
    let aod_r = r.refresh.datasets.iter().find(|d| d.name == "aod").unwrap();
    assert_eq!(aod_r.outcome, "failed");
    assert_eq!(r.index_entries, 8 + enrich_anime::MIN_AOD_ENTRIES);
    std::fs::remove_dir_all(&dir).ok();
}

fn cfg_with_aod(aod: &std::path::Path, fribb: &str) -> Config {
    cfg(&aod.to_string_lossy(), fribb)
}

/// End-to-end measurement on a production sample (see docs/ENRICHMENT-ANIME.md §Results):
/// `MAGNETAR_ANIME_E2E=<dir>` with `aod.jsonl.zst`, `anime-list-full.json` and
/// `anime_all.tsv` (infohash, mal, anidb, anilist, category_path, title, largest file, pubkey;
/// exported with a read-only SELECT, ids only, no descriptions).
#[tokio::test]
#[ignore = "needs MAGNETAR_ANIME_E2E datasets"]
async fn e2e_production_sample() {
    let Ok(dir) = std::env::var("MAGNETAR_ANIME_E2E") else {
        eprintln!("SKIPPED (no MAGNETAR_ANIME_E2E)");
        return;
    };
    let dir = PathBuf::from(dir);
    let rows: Vec<Vec<String>> = std::fs::read_to_string(dir.join("anime_all.tsv"))
        .unwrap()
        .lines()
        .map(|l| l.split('\t').map(str::to_string).collect())
        .collect();
    let cfg = cfg(
        &dir.join("aod.jsonl.zst").to_string_lossy(),
        &dir.join("anime-list-full.json").to_string_lossy(),
    );
    for with_links in [false, true] {
        let Some(db) = test_db().await else { return };
        let pool = db.store.pool().clone();
        contract_tables(&pool).await;
        // Accuracy: only torrents with a MAL link, links hidden. Coverage: every anime
        // torrent, with its links put back as a bare URL description.
        let sample: Vec<&Vec<String>> = rows
            .iter()
            .filter(|r| with_links || !r[1].is_empty())
            .collect();
        for chunk in sample.chunks(2000) {
            let mut ids = Vec::new();
            let mut hashes = Vec::new();
            let mut titles = Vec::new();
            let mut descs = Vec::new();
            let mut files = Vec::new();
            let mut paths = Vec::new();
            for r in chunk {
                ids.push(format!("{:0>64}", r[0]));
                hashes.push(r[0].clone());
                titles.push(r[5].clone());
                let mut d = String::new();
                if with_links {
                    if !r[1].is_empty() {
                        d += &format!("https://myanimelist.net/anime/{} ", r[1]);
                    }
                    if !r[2].is_empty() {
                        d += &format!("https://anidb.net/anime/{} ", r[2]);
                    }
                    if !r[3].is_empty() {
                        d += &format!("https://anilist.co/anime/{}", r[3]);
                    }
                }
                descs.push(d);
                let f = r.get(6).cloned().unwrap_or_default();
                files.push(if f.is_empty() {
                    serde_json::json!([])
                } else {
                    serde_json::json!([{ "path": f, "size": null }])
                });
                paths.push(r[4].clone());
            }
            sqlx::query(
                "WITH v AS (SELECT * FROM UNNEST($1::text[], $2::text[], $3::text[], $4::text[],
                                                 $5::jsonb[], $6::text[])
                            AS v(id, infohash, title, description, files, path)),
                 e AS (INSERT INTO events (id, pubkey, kind, created_at, raw, received_at)
                       SELECT id, repeat('0', 64), 2003, 1, '{}', 1 FROM v)
                 INSERT INTO torrents (event_id, pubkey, created_at, infohash, title, description,
                                       files, file_count, category, category_path)
                 SELECT id, repeat('0', 64), 1, infohash, title, description, files,
                        jsonb_array_length(files), 'anime', string_to_array(path, ',') FROM v",
            )
            .bind(&ids)
            .bind(&hashes)
            .bind(&titles)
            .bind(&descs)
            .bind(&files)
            .bind(&paths)
            .execute(&pool)
            .await
            .unwrap();
        }
        let t = std::time::Instant::now();
        let r = enrich_anime::run_once(&pool, &cfg, false).await.unwrap();
        let secs = t.elapsed().as_secs_f64();
        eprintln!(
            "{} rows, links {}: {}",
            sample.len(),
            with_links,
            serde_json::to_string(&r.matching).unwrap()
        );
        eprintln!("refresh {:?}, {secs:.1} s total", r.refresh);
        if with_links {
            let by: Vec<(String, i64)> = sqlx::query_as(
                "SELECT method, count(*) FROM torrent_media GROUP BY 1 ORDER BY 2 DESC",
            )
            .fetch_all(&pool)
            .await
            .unwrap();
            let excluded: i64 = sqlx::query_scalar(
                "SELECT count(DISTINCT infohash) FROM torrents WHERE category_path && $1::text[]",
            )
            .bind(
                enrich_anime::EXCLUDED_PATHS
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            eprintln!("coverage by method {by:?}; excluded (live-action, music, …) {excluded}");
        } else {
            // Accuracy against the hidden MAL link.
            let truth: std::collections::HashMap<&str, &str> = sample
                .iter()
                .map(|r| (r[0].as_str(), r[1].as_str()))
                .collect();
            let got: Vec<(String, Option<i32>, f32, String)> = sqlx::query_as(
                "SELECT tm.infohash, m.mal, tm.confidence, tm.method FROM torrent_media tm
                 JOIN media m ON m.id = tm.media_id",
            )
            .fetch_all(&pool)
            .await
            .unwrap();
            let mut ok = 0;
            let mut buckets: std::collections::BTreeMap<String, (u32, u32)> = Default::default();
            for (h, mal, conf, _) in &got {
                let good = mal.map(|m| m.to_string()).as_deref() == truth.get(h.as_str()).copied();
                ok += usize::from(good);
                let b = buckets
                    .entry(format!("{:.2}", (conf * 20.0).floor() / 20.0))
                    .or_default();
                if good {
                    b.0 += 1;
                } else {
                    b.1 += 1;
                }
            }
            let n = sample.len();
            eprintln!(
                "ACCURACY: correct {ok}/{n} ({:.1}%), wrong {} ({:.1}%), none {} ({:.1}%), precision {:.1}%",
                100.0 * ok as f64 / n as f64,
                got.len() - ok,
                100.0 * (got.len() - ok) as f64 / n as f64,
                n - got.len(),
                100.0 * (n - got.len()) as f64 / n as f64,
                100.0 * ok as f64 / got.len() as f64
            );
            eprintln!("by confidence floor (ok, wrong): {buckets:?}");
        }
    }
}

/// Both enrichers on the same works at the same time (D46): the IMDb enricher creates and
/// links rows by `i` tag while the anime enricher adopts the same rows and relinks the same
/// torrents. Before the media-links lock this could end in "deadlock detected".
#[tokio::test]
async fn imdb_and_anime_enrichers_run_concurrently() {
    use magnetar_server::enrich::{self, EnrichConfig};
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool().clone();
    let dir = scratch("concurrent");
    let (aod, fribb) = write_datasets(&dir);
    let cfg = cfg(&aod, &fribb);
    enrich_anime::refresh(&pool, &cfg, true).await.unwrap();
    let (ix, version) = enrich_anime::load_index(&pool, true)
        .await
        .unwrap()
        .unwrap();
    let ix = std::sync::Arc::new(ix);
    let works = [
        ("Jujutsu Kaisen", "tt12343534"),
        ("Golden Kamuy", "tt7999998"),
        ("Sousou no Frieren", "tt22248376"),
    ];
    for n in 1..=300u64 {
        let (title, imdb) = works[n as usize % works.len()];
        let mut t = anime_torrent(n, &format!("[Group] {title} - {:02} (1080p)", n % 24 + 1));
        t.p.external_ids.push(("imdb".into(), imdb.into()));
        put(&db.store, t).await;
    }
    let ecfg = EnrichConfig {
        imdb: None,
        refresh: std::time::Duration::from_secs(3600),
        fuzzy: false,
    };
    for round in 0..4 {
        // Make everything due again for both.
        pool.execute("DELETE FROM enrich_checked; DELETE FROM anime_attempts")
            .await
            .unwrap();
        let (a, b) = tokio::join!(
            enrich::drain(&db.store, &ecfg),
            enrich_anime::match_all(&pool, &ix, &version)
        );
        let a = a.unwrap_or_else(|e| panic!("round {round}: imdb enricher: {e:#}"));
        let b = b.unwrap_or_else(|e| panic!("round {round}: anime enricher: {e}"));
        assert_eq!((a.checked, b.scanned), (300, 300), "round {round}");
    }
    // Both writers queue on the media-links lock: while someone holds it, neither commits.
    pool.execute("DELETE FROM enrich_checked; DELETE FROM anime_attempts")
        .await
        .unwrap();
    let mut holder = pool.begin().await.unwrap();
    magnetar_server::media_lock::lock(&mut holder)
        .await
        .unwrap();
    let both = async {
        tokio::join!(
            enrich::drain(&db.store, &ecfg),
            enrich_anime::match_all(&pool, &ix, &version)
        )
    };
    tokio::pin!(both);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(500), &mut both)
            .await
            .is_err(),
        "a writer went ahead without the lock"
    );
    let committed: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM enrich_checked) + (SELECT count(*) FROM anime_attempts)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(committed, 0, "no batch committed while the lock was held");
    holder.rollback().await.unwrap();
    let (a, b) = both.await;
    assert_eq!((a.unwrap().checked, b.unwrap().scanned), (300, 300));

    let links: i64 = sqlx::query_scalar("SELECT count(*) FROM torrent_media")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(links, 300, "one link per torrent");
    let per_imdb: Vec<(String, i64)> = sqlx::query_as(
        "SELECT imdb, count(*) FROM media WHERE imdb IS NOT NULL GROUP BY imdb ORDER BY imdb",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(per_imdb.iter().all(|(_, n)| *n == 1), "{per_imdb:?}");
}

/// A real deadlock is recognised as retryable (the enrichers retry such a batch once).
#[tokio::test]
async fn deadlocks_are_recognised() {
    use sqlx::Connection;
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool().clone();
    pool.execute(
        "CREATE TABLE dl (id int PRIMARY KEY, v int); INSERT INTO dl VALUES (1, 0), (2, 0)",
    )
    .await
    .unwrap();
    let mut a = pool.acquire().await.unwrap();
    let mut b = pool.acquire().await.unwrap();
    let mut ta = a.begin().await.unwrap();
    let mut tb = b.begin().await.unwrap();
    sqlx::query("UPDATE dl SET v = 1 WHERE id = 1")
        .execute(&mut *ta)
        .await
        .unwrap();
    sqlx::query("UPDATE dl SET v = 1 WHERE id = 2")
        .execute(&mut *tb)
        .await
        .unwrap();
    let (ra, rb) = tokio::join!(
        sqlx::query("UPDATE dl SET v = 2 WHERE id = 2").execute(&mut *ta),
        sqlx::query("UPDATE dl SET v = 2 WHERE id = 1").execute(&mut *tb),
    );
    let err = ra
        .err()
        .or(rb.err())
        .expect("one of them is the deadlock victim");
    assert!(magnetar_server::media_lock::is_conflict(&err), "{err}");
    assert!(magnetar_server::media_lock::is_conflict_any(
        &anyhow::Error::from(err)
    ));
}
