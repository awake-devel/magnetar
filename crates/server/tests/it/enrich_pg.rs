//! Enrichment (D40) against Postgres: dataset import from tiny gzip fixtures, strong IDs,
//! offline title matching, ownership rules, search tokens and the media API.

use crate::common;

use std::io::Write;
use std::path::PathBuf;

use common::*;
use magnetar_core::api::ApiClient;
use magnetar_core::model::*;
use magnetar_core::query::SearchQuery;
use magnetar_core::store::Store;
use magnetar_server::enrich::{self, EnrichConfig, imdb};

const BASICS: &str = "tconst\ttitleType\tprimaryTitle\toriginalTitle\tisAdult\tstartYear\tendYear\truntimeMinutes\tgenres
tt0133093\tmovie\tThe Matrix\tThe Matrix\t0\t1999\t\\N\t136\tAction,Sci-Fi
tt0234215\tmovie\tThe Matrix Reloaded\tThe Matrix Reloaded\t0\t2003\t\\N\t138\tAction,Sci-Fi
tt0116282\tmovie\tFargo\tFargo\t0\t1996\t\\N\t98\tCrime,Drama,Thriller
tt2802850\ttvSeries\tFargo\tFargo\t0\t2014\t\\N\t53\tCrime,Drama,Thriller
tt3593662\ttvEpisode\tThe Crocodile's Dilemma\tThe Crocodile's Dilemma\t0\t2014\t\\N\t68\tCrime
tt0087182\tmovie\tDune\tDune\t0\t1984\t\\N\t137\tAction,Adventure
tt1160419\tmovie\tDune: Part One\tDune\t0\t2021\t\\N\t155\tAction,Adventure
tt0211915\tmovie\tAmélie\tLe fabuleux destin d'Amélie Poulain\t0\t2001\t\\N\t122\tComedy,Romance
tt9999991\tmovie\tAdult Thing\tAdult Thing\t1\t2020\t\\N\t80\tAdult
tt9999992\tshort\tShort Thing\tShort Thing\t0\t2020\t\\N\t5\tShort
tt0000001\tvideoGame\tThe Matrix\tThe Matrix\t0\t1999\t\\N\t\\N\t\\N
";
const RATINGS: &str = "tconst\taverageRating\tnumVotes
tt0133093\t8.7\t2100000
tt0234215\t7.2\t650000
tt0116282\t8.1\t720000
tt2802850\t8.9\t420000
tt0087182\t6.3\t190000
tt1160419\t8.0\t900000
tt0211915\t8.3\t800000
";
const EPISODES: &str = "tconst\tparentTconst\tseasonNumber\tepisodeNumber
tt3593662\ttt2802850\t1\t1
tt0000002\ttt0116282\t1\t1
";

fn gz(dir: &std::path::Path, name: &str, body: &str) {
    let f = std::fs::File::create(dir.join(format!("{name}.tsv.gz"))).unwrap();
    let mut e = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
    e.write_all(body.as_bytes()).unwrap();
    e.finish().unwrap();
}

fn fixture_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("magnetar-imdb-{:016x}", rand::random::<u64>()));
    std::fs::create_dir_all(&dir).unwrap();
    gz(&dir, imdb::BASICS, BASICS);
    gz(&dir, imdb::RATINGS, RATINGS);
    gz(&dir, imdb::EPISODE, EPISODES);
    dir
}

fn fetched(name: &str) -> imdb::Fetched {
    imdb::Fetched {
        name: name.into(),
        version: "Sun, 25 Oct 2026 00:36:16 GMT".into(),
        bytes: 1,
        downloaded: false,
    }
}

fn itag(mut t: T, k: &str, v: &str) -> T {
    t.p.external_ids.push((k.into(), v.into()));
    t
}

fn q(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        min_trust: Some(0),
        descending: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn import_link_search() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pool = s.pool();
    let dir = fixture_dir();
    let icfg = imdb::ImdbConfig {
        dir: dir.clone(),
        akas: false,
        base_url: "http://127.0.0.1:9".into(),
        min_titles: 1,
    };
    let ecfg = EnrichConfig {
        imdb: Some(icfg.clone()),
        refresh: std::time::Duration::from_secs(3600),
        fuzzy: true,
    };

    // Strong IDs work before any dataset exists; title matching waits (pending).
    put(
        s,
        itag(
            torrent(1, 100, NOW, 1, "The.Matrix.1999.1080p.BluRay"),
            "imdb",
            "tt0133093",
        )
        .cat(Category::Video),
    )
    .await;
    put(
        s,
        torrent(2, 100, NOW, 2, "Fargo.S05E01.1080p.WEB.h264-ETHEL").cat(Category::Video),
    )
    .await;
    let r = enrich::drain(s, &ecfg).await.unwrap();
    assert_eq!((r.checked, r.strong, r.unmatched), (2, 1, 1));
    let m = s
        .torrent_detail(&ih(1), NOW)
        .await
        .unwrap()
        .unwrap()
        .media
        .unwrap();
    assert_eq!(
        (m.title.as_str(), m.kind, m.confidence),
        ("The Matrix", MediaKind::Movie, 1.0)
    );
    assert_eq!(m.imdb.as_deref(), Some("tt0133093"));
    assert!(
        s.torrent_detail(&ih(2), NOW)
            .await
            .unwrap()
            .unwrap()
            .media
            .is_none()
    );
    assert_eq!(
        enrich::drain(s, &ecfg).await.unwrap().checked,
        0,
        "nothing due"
    );

    // Import: adult rows, shorts and games are dropped; episodes are packed per bucket.
    let rep = imdb::import(
        pool,
        &icfg,
        &[
            fetched(imdb::BASICS),
            fetched(imdb::RATINGS),
            fetched(imdb::EPISODE),
        ],
    )
    .await
    .unwrap();
    assert_eq!((rep.titles, rep.episodes), (7, 1));
    assert_eq!(
        rep.media_refreshed, 1,
        "torrent-derived Matrix row gets IMDb fields"
    );
    assert!(imdb::imported_at(pool).await.unwrap().is_some());
    let m = s
        .torrent_detail(&ih(1), NOW)
        .await
        .unwrap()
        .unwrap()
        .media
        .unwrap();
    assert_eq!(
        (m.title.as_str(), m.year, m.votes),
        ("The Matrix", Some(1999), Some(2_100_000))
    );
    assert_eq!(m.genres, vec!["Action", "Sci-Fi"]);

    // More torrents: every path.
    put(
        s,
        torrent(3, 100, NOW, 3, "Dune.2021.2160p.WEB-DL.DDP5.1").cat(Category::Video),
    )
    .await;
    put(
        s,
        torrent(4, 101, NOW, 4, "Matrix remux")
            .cat(Category::Video)
            .desc("https://www.imdb.com/title/tt0133093/ https://www.themoviedb.org/movie/603-the-matrix"),
    )
    .await;
    put(
        s,
        torrent(5, 101, NOW, 5, "Some.Movie.2020.1080p")
            .cat(Category::Video)
            .desc("TMDB https://www.themoviedb.org/movie/777"),
    )
    .await;
    put(
        s,
        torrent(6, 102, NOW, 6, "[Group] Frieren - 03 [1080p]").cat(Category::Anime),
    )
    .await;
    put(
        s,
        torrent(7, 102, NOW, 7, "[Group] Frieren - 04 [1080p]")
            .cat(Category::Anime)
            .desc("https://myanimelist.net/anime/52991"),
    )
    .await;
    put(
        s,
        itag(
            torrent(8, 103, NOW, 8, "Fargo.S01E01.720p"),
            "imdb",
            "tt3593662",
        )
        .cat(Category::Video),
    )
    .await;
    // A DHT-style torrent: name is the infohash, the largest video file names it.
    put(
        s,
        torrent(9, 104, NOW, 9, &ih(9))
            .cat(Category::Video)
            .files(&[
                ("x/sample.mkv", 5),
                (
                    "x/Le.Fabuleux.Destin.d.Amelie.Poulain.2001.FRENCH.1080p.mkv",
                    900,
                ),
            ]),
    )
    .await;
    put(
        s,
        torrent(10, 104, NOW, 10, "Some.Book.Title.epub").cat(Category::Book),
    )
    .await;
    // Deliberate i tags: the TMDB id is attached to the IMDb row.
    put(
        s,
        itag(
            itag(
                torrent(15, 101, NOW, 15, "Matrix tagged"),
                "imdb",
                "tt0133093",
            ),
            "tmdb",
            "movie:603",
        )
        .cat(Category::Video),
    )
    .await;
    // A manual link is never overwritten.
    put(
        s,
        torrent(11, 104, NOW, 11, "Dune.1984.1080p").cat(Category::Video),
    )
    .await;
    let matrix: i64 = sqlx::query_scalar("SELECT id FROM media WHERE imdb = 'tt0133093'")
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO torrent_media (infohash, media_id, method, confidence, matcher_version, matched_at)
         VALUES ($1, $2, 'manual', 1, 1, 0)",
    )
    .bind(ih(11))
    .bind(matrix)
    .execute(pool)
    .await
    .unwrap();

    // Another enricher's link survives a less confident title match, not a more confident one.
    put(
        s,
        torrent(13, 104, NOW, 13, "Fargo.1996.1080p.BluRay").cat(Category::Video),
    )
    .await;
    put(
        s,
        torrent(14, 104, NOW, 14, "Fargo.1996.720p.BluRay").cat(Category::Video),
    )
    .await;
    for (n, conf) in [(13u64, 0.99f32), (14, 0.3)] {
        sqlx::query(
            "INSERT INTO torrent_media (infohash, media_id, method, confidence, matcher_version, matched_at)
             VALUES ($1, $2, 'api_search', $3, 1, 0)",
        )
        .bind(ih(n))
        .bind(matrix)
        .bind(conf)
        .execute(pool)
        .await
        .unwrap();
    }

    let r = enrich::drain(s, &ecfg).await.unwrap();
    assert_eq!(r.checked, 13, "{r:?}");
    assert_eq!(r.anime_skipped, 1);

    let media = |n: u64| async move { s.torrent_detail(&ih(n), NOW).await.unwrap().unwrap().media };
    let fargo = media(2).await.unwrap();
    assert_eq!(
        (fargo.imdb.as_deref(), fargo.kind),
        (Some("tt2802850"), MediaKind::Series)
    );
    assert_eq!(
        (fargo.season, fargo.episode, fargo.method.as_str()),
        (Some(5), Some(1), "offline_title")
    );
    assert!(fargo.confidence >= 0.7, "{fargo:?}");
    let dune = media(3).await.unwrap();
    assert_eq!(dune.imdb.as_deref(), Some("tt1160419"));
    assert!(dune.confidence >= 0.9);
    let m4 = media(4).await.unwrap();
    assert_eq!(
        (m4.media_id, m4.method.as_str(), m4.confidence),
        (matrix, "desc_link", 1.0)
    );
    let m15 = media(15).await.unwrap();
    assert_eq!((m15.media_id, m15.method.as_str()), (matrix, "itag"));
    assert_eq!(
        m15.tmdb,
        Some((MediaKind::Movie, 603)),
        "tagged tmdb id attached to the IMDb row"
    );
    let m5 = media(5).await.unwrap();
    assert_eq!(
        (m5.title.as_str(), m5.year, m5.kind, m5.imdb),
        ("Some Movie", Some(2020), MediaKind::Movie, None)
    );
    assert_eq!(m5.tmdb, Some((MediaKind::Movie, 777)));
    assert!(
        media(6).await.is_none(),
        "anime without strong id is left to D41"
    );
    let m7 = media(7).await.unwrap();
    assert_eq!((m7.kind, m7.mal), (MediaKind::Anime, Some(52991)));
    let m8 = media(8).await.unwrap();
    assert_eq!(
        (m8.imdb.as_deref(), m8.season, m8.episode),
        (Some("tt2802850"), Some(1), Some(1)),
        "episode → series"
    );
    assert_eq!(m8.media_id, fargo.media_id);
    let m9 = media(9).await.unwrap();
    assert_eq!(
        m9.imdb.as_deref(),
        Some("tt0211915"),
        "originalTitle via the largest file"
    );
    assert!(media(10).await.is_none());
    let m11 = media(11).await.unwrap();
    assert_eq!((m11.media_id, m11.method.as_str()), (matrix, "manual"));
    assert_eq!(media(13).await.unwrap().method, "api_search");
    let m14 = media(14).await.unwrap();
    assert_eq!(
        (m14.imdb.as_deref(), m14.method.as_str()),
        (Some("tt0116282"), "offline_title")
    );

    // Summaries carry the media title; search tokens select one title's releases.
    let page = s.search(&q("imdb:tt0133093"), NOW).await.unwrap();
    let mut got: Vec<String> = page.hits.iter().map(|h| h.infohash.clone()).collect();
    got.sort();
    assert_eq!(got, vec![ih(1), ih(4), ih(11), ih(13), ih(15)]);
    assert!(
        page.hits
            .iter()
            .all(|h| h.media_title.as_deref() == Some("The Matrix"))
    );
    assert_eq!(page.hits[0].media_year, Some(1999));
    assert_eq!(page.total, 5);
    assert_eq!(
        s.search(&q("imdb:tt0133093 remux"), NOW)
            .await
            .unwrap()
            .total,
        1
    );
    assert_eq!(
        s.search(&q("tmdb:movie:777"), NOW).await.unwrap().hits[0].infohash,
        ih(5)
    );
    assert_eq!(s.search(&q("mal:52991"), NOW).await.unwrap().total, 1);
    assert_eq!(
        s.search(&q(&format!("media:{}", fargo.media_id)), NOW)
            .await
            .unwrap()
            .total,
        2
    );
    assert_eq!(s.search(&q("imdb:tt7777777"), NOW).await.unwrap().total, 0);
    let plain = s.search(&q("dune"), NOW).await.unwrap();
    assert!(
        plain
            .hits
            .iter()
            .any(|h| h.media_title.as_deref() == Some("Dune: Part One"))
    );

    // HTTP: other releases of a title (the trust threshold applies; test publishers have 0).
    s.set_setting(magnetar_core::store::keys::TRUST_THRESHOLD, "0")
        .await
        .unwrap();
    let base = spawn_server(s).await;
    let api = ApiClient::new(&base, Some(TOKEN.into())).unwrap();
    let page = api.media_torrents(matrix, 10, 0).await.unwrap();
    assert_eq!(page.total, 5);
    let detail = api.torrent(&ih(15)).await.unwrap().unwrap();
    assert_eq!(detail.media.unwrap().tmdb, Some((MediaKind::Movie, 603)));

    // A later event for torrent 5 links IMDb: the infohash is due again. Its first event's main
    // link is the TMDB id, so the pair is deliberate: the tmdb-only row is merged into the IMDb
    // row (links move, the duplicate goes).
    put(
        s,
        torrent(12, 105, NOW + 1, 5, "Some.Movie.2020.1080p")
            .cat(Category::Video)
            .desc("https://www.imdb.com/title/tt0234215/ https://www.themoviedb.org/movie/777"),
    )
    .await;
    let r = enrich::drain(s, &ecfg).await.unwrap();
    assert_eq!(r.checked, 1);
    let m5b = media(5).await.unwrap();
    assert_eq!(
        (m5b.imdb.as_deref(), m5b.tmdb, m5b.confidence),
        (Some("tt0234215"), Some((MediaKind::Movie, 777)), 1.0)
    );
    let dupes: i64 = sqlx::query_scalar("SELECT count(*) FROM media WHERE tmdb_movie = 777")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(dupes, 1);
    assert_ne!(m5b.media_id, m5.media_id);

    // A description's other link is not attached on its own, however often the pair is seen
    // (an uploader repeats the same wrong link): no TMDB-only row with a matching title.
    let fargo96 = "https://www.imdb.com/title/tt0116282/ https://www.themoviedb.org/movie/275";
    put(
        s,
        torrent(16, 105, NOW, 16, "Fargo 1996 remux")
            .cat(Category::Video)
            .desc(fargo96),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    assert_eq!(media(16).await.unwrap().tmdb, None, "seen once");
    put(
        s,
        torrent(17, 105, NOW, 17, "Fargo 1996 x265")
            .cat(Category::Video)
            .desc(fargo96),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    assert_eq!(media(17).await.unwrap().tmdb, None, "seen twice");

    // Pairs are recorded against the series: two episodes' IMDb ids next to one TMDB series
    // link agree (not a template).
    put(
        s,
        itag(
            torrent(18, 105, NOW, 18, "Fargo.S01E01.1080p"),
            "imdb",
            "tt3593662",
        )
        .cat(Category::Video)
        .desc("https://www.themoviedb.org/tv/60622"),
    )
    .await;
    put(
        s,
        torrent(19, 105, NOW, 19, "Fargo.S01.COMPLETE.1080p")
            .cat(Category::Video)
            .desc("https://www.imdb.com/title/tt2802850/ https://www.themoviedb.org/tv/60622"),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    let m19 = media(19).await.unwrap();
    assert_eq!(
        (m19.media_id, m19.tmdb),
        (fargo.media_id, Some((MediaKind::Series, 60622)))
    );

    // Rows D41 owns (kind anime + an anime id) keep their fields on import; an anime-kind row
    // without anime ids is still refreshed (kind kept).
    sqlx::query(
        "UPDATE media SET kind = 'anime', mal = 42, title = 'Anime Reloaded', rating = 9.9
         WHERE imdb = 'tt0234215'",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE media SET kind = 'anime', title = 'Stale' WHERE imdb = 'tt0116282'")
        .execute(pool)
        .await
        .unwrap();

    // Re-import (unchanged files): media rows untouched, unmatched title lookups retried.
    let rep = imdb::import(
        pool,
        &icfg,
        &[
            fetched(imdb::BASICS),
            fetched(imdb::RATINGS),
            fetched(imdb::EPISODE),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        rep.media_refreshed, 1,
        "only the anime-kind row without anime ids"
    );
    let rows: Vec<(String, String, Option<f32>)> = sqlx::query_as(
        "SELECT imdb, title, rating FROM media WHERE imdb IN ('tt0234215', 'tt0116282')
         ORDER BY imdb",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("tt0116282".into(), "Fargo".into(), Some(8.1)),
            ("tt0234215".into(), "Anime Reloaded".into(), Some(9.9)),
        ]
    );
    let cov = enrich::coverage(pool).await.unwrap();
    assert!(
        cov.iter()
            .any(|c| c.category == "video" && c.method.as_deref() == Some("offline_title"))
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn strong_ids_without_imdb_dataset() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let ecfg = EnrichConfig {
        imdb: None,
        refresh: std::time::Duration::from_secs(3600),
        fuzzy: false,
    };
    put(
        s,
        torrent(1, 100, NOW, 1, "Show.S02E03.1080p")
            .cat(Category::Video)
            .desc("https://www.themoviedb.org/tv/1399/season/2/episode/3"),
    )
    .await;
    put(
        s,
        itag(torrent(2, 100, NOW, 2, "Thing.2020"), "imdb", "tt7654321").cat(Category::Other),
    )
    .await;
    let r = enrich::drain(s, &ecfg).await.unwrap();
    assert_eq!((r.checked, r.strong), (2, 2));
    let m = s
        .torrent_detail(&ih(1), NOW)
        .await
        .unwrap()
        .unwrap()
        .media
        .unwrap();
    assert_eq!(
        (m.kind, m.tmdb, m.season, m.episode),
        (
            MediaKind::Series,
            Some((MediaKind::Series, 1399)),
            Some(2),
            Some(3)
        )
    );
    assert_eq!(m.title, "Show");
    let m = s
        .torrent_detail(&ih(2), NOW)
        .await
        .unwrap()
        .unwrap()
        .media
        .unwrap();
    assert_eq!(
        (m.imdb.as_deref(), m.title.as_str(), m.year),
        (Some("tt7654321"), "Thing", Some(2020))
    );
}

/// Template links of another kind (review of D40): YGG's `themoviedb.org/movie/324544` (In the
/// Lost Lands) under Holy Ghost releases next to their own IMDb link, in both orders.
#[tokio::test]
async fn template_links_never_relink_other_titles() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pool = s.pool();
    let ecfg = EnrichConfig {
        imdb: None,
        refresh: std::time::Duration::from_secs(3600),
        fuzzy: false,
    };
    const LOST: &str = "https://www.themoviedb.org/movie/324544-in-the-lost-lands";
    let holy = |tt: &str| format!("<a href=\"https://www.imdb.com/title/{tt}/\">IMDb</a> {LOST}");
    let media = |n: u64| async move { s.torrent_detail(&ih(n), NOW).await.unwrap().unwrap().media };

    // Lost Lands first (its only id is the TMDB link), then Holy Ghost.
    put(
        s,
        torrent(1, 100, NOW, 1, "In.the.Lost.Lands.2025.MULTi.1080p.WEB")
            .cat(Category::Video)
            .desc(LOST),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    put(
        s,
        torrent(2, 100, NOW, 2, "Holy.Ghost.2025.FRENCH.1080p.WEB")
            .cat(Category::Video)
            .desc(&holy("tt36364273")),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    let lost = media(1).await.unwrap();
    let ghost = media(2).await.unwrap();
    assert_eq!(
        (lost.title.as_str(), lost.tmdb, lost.imdb.as_deref()),
        ("In the Lost Lands", Some((MediaKind::Movie, 324544)), None)
    );
    assert_eq!(
        (ghost.title.as_str(), ghost.imdb.as_deref(), ghost.tmdb),
        ("Holy Ghost", Some("tt36364273"), None)
    );
    assert_ne!(lost.media_id, ghost.media_id);
    assert_eq!(
        ghost.confidence, 1.0,
        "a row without IMDb id is no contradiction"
    );

    // A second Holy Ghost release: the pair is seen twice, still no merge (titles differ).
    put(
        s,
        torrent(6, 100, NOW, 6, "Holy.Ghost.2025.MULTi.1080p.WEB")
            .cat(Category::Video)
            .desc(&holy("tt36364273")),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    assert_eq!(media(1).await.unwrap().media_id, lost.media_id);
    assert_eq!(media(6).await.unwrap().media_id, ghost.media_id);

    // Deliberate tags adopt at once; the same id next to another IMDb id later is a template:
    // it comes off the first row and that row's torrents are relinked without it.
    put(
        s,
        itag(
            itag(
                torrent(8, 100, NOW, 8, "Tagged.Film.2025.1080p"),
                "imdb",
                "tt4444444",
            ),
            "tmdb",
            "movie:999",
        )
        .cat(Category::Video),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    let tagged = media(8).await.unwrap();
    assert_eq!(tagged.tmdb, Some((MediaKind::Movie, 999)));
    put(
        s,
        torrent(7, 100, NOW, 7, "Third.Film.2025.1080p")
            .cat(Category::Video)
            .desc("https://www.imdb.com/title/tt3333333/ https://www.themoviedb.org/movie/999"),
    )
    .await;
    let r = enrich::drain(s, &ecfg).await.unwrap();
    assert_eq!(
        r.checked, 2,
        "torrent 7, then torrent 8 requeued by the heal"
    );
    let tagged = media(8).await.unwrap();
    assert_eq!(
        (tagged.imdb.as_deref(), tagged.tmdb),
        (Some("tt4444444"), None)
    );
    assert_eq!(media(7).await.unwrap().tmdb, None);

    // Another template pair seen once, then a release with only the TMDB link.
    put(
        s,
        torrent(3, 100, NOW, 3, "Other.Film.2025.1080p")
            .cat(Category::Video)
            .desc("https://www.imdb.com/title/tt1111111/ https://www.themoviedb.org/movie/1530875"),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    put(
        s,
        torrent(4, 100, NOW, 4, "Template.Film.2024.1080p")
            .cat(Category::Video)
            .desc("https://www.themoviedb.org/movie/1530875"),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    let other = media(3).await.unwrap();
    let tmpl = media(4).await.unwrap();
    assert_eq!(
        other.tmdb, None,
        "description extra link seen once: not attached"
    );
    assert_ne!(other.media_id, tmdb_media(pool, 1530875).await);
    assert_eq!(
        (tmpl.title.as_str(), tmpl.imdb.as_deref()),
        ("Template Film", None)
    );

    // Once the TMDB id is known to belong to another IMDb title (D42's external ids), a release
    // carrying it next to a different IMDb link is linked at 0.9.
    sqlx::query("UPDATE media SET imdb = 'tt2222222' WHERE tmdb_movie = 324544")
        .execute(pool)
        .await
        .unwrap();
    put(
        s,
        torrent(5, 100, NOW, 5, "Holy.Ghost.2025.MULTi.2160p.WEB")
            .cat(Category::Video)
            .desc(&holy("tt36364273")),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    let ghost5 = media(5).await.unwrap();
    assert_eq!((ghost5.media_id, ghost5.confidence), (ghost.media_id, 0.9));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM media")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        rows, 6,
        "Lost Lands, Holy Ghost, Tagged Film, Third Film, Other Film, Template Film"
    );
}

async fn tmdb_media(pool: &sqlx::PgPool, tmdb: i32) -> i64 {
    sqlx::query_scalar("SELECT id FROM media WHERE tmdb_movie = $1")
        .bind(tmdb)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// D41's rows are linked to as they are, never merged, deleted or rewritten (review of D40).
#[tokio::test]
async fn anime_rows_belong_to_d41() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pool = s.pool();
    let ecfg = EnrichConfig {
        imdb: None,
        refresh: std::time::Duration::from_secs(3600),
        fuzzy: false,
    };
    let anime: i64 = sqlx::query_scalar(
        "INSERT INTO media (kind, mal, title, updated_at)
         VALUES ('anime', 5114, 'Hagane no Renkinjutsushi: Fullmetal Alchemist', 0) RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let media = |n: u64| async move { s.torrent_detail(&ih(n), NOW).await.unwrap().unwrap().media };
    // An IMDb row for the same work already exists (a video release).
    put(
        s,
        itag(
            torrent(
                1,
                100,
                NOW,
                1,
                "Fullmetal.Alchemist.Brotherhood.S01E01.1080p",
            ),
            "imdb",
            "tt1355642",
        )
        .cat(Category::Video),
    )
    .await;
    // Anime release with both tags.
    put(
        s,
        itag(
            itag(
                torrent(
                    2,
                    100,
                    NOW,
                    2,
                    "[Group] Fullmetal Alchemist Brotherhood - 01",
                ),
                "imdb",
                "tt1355642",
            ),
            "mal",
            "anime:5114",
        )
        .cat(Category::Anime),
    )
    .await;
    // An anime release with an IMDb id unknown so far: its new row is kind anime.
    put(
        s,
        itag(
            torrent(3, 100, NOW, 3, "[Group] Some Show - 02"),
            "imdb",
            "tt9876543",
        )
        .cat(Category::Anime),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    let m1 = media(1).await.unwrap();
    let m2 = media(2).await.unwrap();
    let m3 = media(3).await.unwrap();
    assert_eq!(
        (m1.kind, m1.imdb.as_deref(), m1.mal),
        (MediaKind::Series, Some("tt1355642"), None),
        "MAL id not moved onto the IMDb row"
    );
    assert_eq!(
        (m2.media_id, m2.kind, m2.mal, m2.confidence),
        (anime, MediaKind::Anime, Some(5114), 1.0)
    );
    assert_eq!(
        m2.title, "Hagane no Renkinjutsushi: Fullmetal Alchemist",
        "D41's row untouched"
    );
    assert_eq!(m2.imdb, None);
    assert_eq!(
        (m3.kind, m3.imdb.as_deref()),
        (MediaKind::Anime, Some("tt9876543"))
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM media")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(rows, 3);
    assert_eq!(s.search(&q("mal:5114"), NOW).await.unwrap().total, 1);
}

/// A description's other link is adopted when the TMDB-only row's title is one of the IMDb
/// title's names, whichever torrent is linked first (review of D40).
#[tokio::test]
async fn pairs_agree_by_title_in_any_order() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pool = s.pool();
    let ecfg = EnrichConfig {
        imdb: None,
        refresh: std::time::Duration::from_secs(3600),
        fuzzy: false,
    };
    let media = |n: u64| async move { s.torrent_detail(&ih(n), NOW).await.unwrap().unwrap().media };
    let both = |tt: &str, tv: u32| {
        format!("https://www.imdb.com/title/{tt}/ https://www.themoviedb.org/tv/{tv}")
    };
    // TMDB-only releases first, then one release with both links.
    for n in 1..=2u64 {
        put(
            s,
            torrent(n, 100, NOW, n, &format!("Acapulco.S01E0{n}.1080p.WEB"))
                .cat(Category::Video)
                .desc("https://www.themoviedb.org/tv/133727"),
        )
        .await;
    }
    enrich::drain(s, &ecfg).await.unwrap();
    put(
        s,
        torrent(3, 100, NOW, 3, "Acapulco.S01E03.1080p.WEB")
            .cat(Category::Video)
            .desc(&both("tt13567344", 133727)),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    let m3 = media(3).await.unwrap();
    assert_eq!(
        (m3.imdb.as_deref(), m3.tmdb),
        (Some("tt13567344"), Some((MediaKind::Series, 133727)))
    );
    for n in 1..=2 {
        assert_eq!(media(n).await.unwrap().media_id, m3.media_id, "merged");
    }

    // The release with both links first, then a TMDB-only release.
    put(
        s,
        torrent(4, 100, NOW, 4, "Palm.Royale.S01E01.1080p.WEB")
            .cat(Category::Video)
            .desc(&both("tt8888540", 157367)),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    assert_eq!(
        media(4).await.unwrap().tmdb,
        None,
        "seen once, no row to compare"
    );
    put(
        s,
        torrent(5, 100, NOW, 5, "Palm.Royale.S01E02.1080p.WEB")
            .cat(Category::Video)
            .desc("https://www.themoviedb.org/tv/157367"),
    )
    .await;
    // A TMDB-only release of another title with an id paired once elsewhere stays apart.
    put(
        s,
        torrent(6, 100, NOW, 6, "Holy.Ghost.2025.1080p")
            .cat(Category::Video)
            .desc(&both("tt36364273", 324544)),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    put(
        s,
        torrent(7, 100, NOW, 7, "In.the.Lost.Lands.2025.1080p")
            .cat(Category::Video)
            .desc("https://www.themoviedb.org/movie/324544"),
    )
    .await;
    enrich::drain(s, &ecfg).await.unwrap();
    let m4 = media(4).await.unwrap();
    let m5 = media(5).await.unwrap();
    assert_eq!(
        (m5.media_id, m5.tmdb),
        (m4.media_id, Some((MediaKind::Series, 157367)))
    );
    let m7 = media(7).await.unwrap();
    assert_eq!((m7.title.as_str(), m7.imdb), ("In the Lost Lands", None));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM media")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(rows, 4);
}

/// D68: a DHT-only torrent (no Nostr event) is matched by its verified name, and its release
/// attributes come from the backfill (`torrent_rows`).
#[tokio::test]
async fn dht_only_torrents_are_enriched_and_parsed() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pool = s.pool();
    let dir = fixture_dir();
    let icfg = imdb::ImdbConfig {
        dir: dir.clone(),
        akas: false,
        base_url: "http://127.0.0.1:9".into(),
        min_titles: 1,
    };
    let ecfg = EnrichConfig {
        imdb: Some(icfg.clone()),
        refresh: std::time::Duration::from_secs(3600),
        fuzzy: true,
    };
    imdb::import(
        pool,
        &icfg,
        &[
            fetched(imdb::BASICS),
            fetched(imdb::RATINGS),
            fetched(imdb::EPISODE),
        ],
    )
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO dht_torrents (infohash, title, files, file_count, files_text, total_size,
             category, metadata_size, first_seen, fetched_at, discovered_via, classifier_version)
         VALUES ($1, 'Fargo.1996.1080p.BluRay.x264-GRP',
                 '[{\"path\":\"Fargo.1996.1080p.BluRay.x264-GRP.mkv\",\"size\":8000000000}]',
                 1, 'Fargo.1996.1080p.BluRay.x264-GRP.mkv', 8000000000, 'video', 1000, $2, $2,
                 'sample', 1)",
    )
    .bind(ih(77))
    .bind(NOW)
    .execute(pool)
    .await
    .unwrap();
    let r = enrich::drain(s, &ecfg).await.unwrap();
    assert_eq!(r.checked, 1, "{r:?}");
    let (imdb_id, method): (Option<String>, String) = sqlx::query_as(
        "SELECT m.imdb, tm.method FROM torrent_media tm JOIN media m ON m.id = tm.media_id
         WHERE tm.infohash = $1",
    )
    .bind(ih(77))
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(
        imdb_id.as_deref(),
        Some("tt0116282"),
        "Fargo (1996), the film"
    );
    assert_eq!(method, "offline_title");
    // Release attributes: the DHT writer stores them; the backfill re-parses DHT rows too.
    sqlx::query("DELETE FROM torrent_attrs WHERE infohash = $1")
        .bind(ih(77))
        .execute(pool)
        .await
        .unwrap();
    magnetar_server::attrs::backfill(pool, false, None)
        .await
        .unwrap();
    let res: Option<String> =
        sqlx::query_scalar("SELECT attrs->>'resolution' FROM torrent_attrs WHERE infohash = $1")
            .bind(ih(77))
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(res.as_deref(), Some("1080p"));
    let _ = std::fs::remove_dir_all(dir);
}
