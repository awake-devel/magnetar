//! Release/media filters (D44): attributes parsed at ingest and by the backfill, the same
//! filter semantics on the Tantivy index and the Postgres fallback, disjunctive facet counts,
//! the per-title release view, the saved-search notifier, the API forms.
//! Needs MAGNETAR_TEST_DATABASE_URL (else SKIPPED).

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::*;
use magnetar_core::api::{FacetSet, FacetsRequest, Health, MediaReleases};
use magnetar_core::model::*;
use magnetar_core::query::{SearchPage, SearchQuery, Sort};
use magnetar_core::release::{PARSER_VERSION, ReleaseAttrs};
use magnetar_core::store::Store;
use magnetar_server::PgStore;
use magnetar_server::search_index::{self, SearchIndex, load};

fn q() -> SearchQuery {
    SearchQuery {
        min_trust: Some(0),
        descending: true,
        limit: 200,
        ..Default::default()
    }
}

fn infohashes(p: &SearchPage) -> Vec<String> {
    let mut v: Vec<String> = p.hits.iter().map(|h| h.infohash.clone()).collect();
    v.sort();
    v
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
    let dir = std::env::temp_dir().join(format!("magnetar-fx-{:016x}", rand::random::<u64>()));
    let id = load::database_id(store.pool()).await.unwrap();
    let idx = SearchIndex::open_async(dir.clone(), id).await.unwrap();
    search_index::build(store.pool(), &idx).await.unwrap();
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

async fn media(
    store: &PgStore,
    kind: &str,
    title: &str,
    year: i32,
    genres: &[&str],
    rating: f32,
) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO media (kind, title, year, genres, rating, updated_at)
         VALUES ($1, $2, $3, $4, $5, 0) RETURNING id",
    )
    .bind(kind)
    .bind(title)
    .bind(year)
    .bind(genres.iter().map(|g| g.to_string()).collect::<Vec<_>>())
    .bind(rating)
    .fetch_one(store.pool())
    .await
    .unwrap()
}

async fn link(store: &PgStore, infohash: u64, media_id: i64) {
    sqlx::query(
        "INSERT INTO torrent_media (infohash, media_id, method, confidence, matcher_version, matched_at)
         VALUES ($1, $2, 'manual', 1.0, 1, 0)",
    )
    .bind(ih(infohash))
    .bind(media_id)
    .execute(store.pool())
    .await
    .unwrap();
}

/// A small mixed corpus: French scene films and series, anime, a DHT-style torrent.
async fn corpus(s: &PgStore) -> (i64, i64) {
    let film = media(
        s,
        "movie",
        "Haunted Mansion",
        2023,
        &["Comedy", "Fantasy"],
        6.1,
    )
    .await;
    let show = media(s, "series", "Fargo", 2014, &["Crime", "Drama"], 8.9).await;
    let rows: &[(u64, &str, Category)] = &[
        (
            1,
            "Haunted.Mansion.2023.MULTi.VF2.1080p.WEB.H264-FW",
            Category::Video,
        ),
        (
            2,
            "Haunted.Mansion.2023.VFQ.720p.WEB-DL.H264-Slay3R",
            Category::Video,
        ),
        (
            3,
            "Haunted.Mansion.2023.TRUEFRENCH.2160p.BluRay.REMUX.HDR.DV.HEVC.TrueHD.Atmos.7.1-GRP",
            Category::Video,
        ),
        (4, "Fargo.S05E09.MULTi.1080p.WEB.x264-FW", Category::Video),
        (5, "Fargo.S05E09.VOSTFR.720p.WEB.H264-TFA", Category::Video),
        (
            6,
            "Fargo.S05.MULTi.1080p.WEB.DDP5.1.x264-FW",
            Category::Video,
        ),
        (
            7,
            "[SubsPlease] Toumei Otoko to Ningen Onna - 07 (1080p) [4874FF4E].mkv",
            Category::Anime,
        ),
        (8, "Old.Film.1999.ENGLISH.DVDRip.XviD-OLD", Category::Video),
    ];
    for (n, title, cat) in rows {
        put(
            s,
            torrent(*n, 1, NOW - *n as i64 * 10, *n, title)
                .cat(*cat)
                .size(1 << 30),
        )
        .await;
    }
    // DHT-style: the name says nothing, the files do.
    put(
        s,
        torrent(9, 1, NOW - 90, 9, "Some Folder")
            .cat(Category::Video)
            .files(&[
                ("Some Folder/Movie.2020.1080p.BluRay.x265-AAA.mkv", 9 << 30),
                ("Some Folder/Movie.2020.fr.srt", 10),
            ]),
    )
    .await;
    for n in [1, 2, 3] {
        link(s, n, film).await;
    }
    for n in [4, 5, 6] {
        link(s, n, show).await;
    }
    (film, show)
}

async fn attrs_of(s: &PgStore, n: u64) -> ReleaseAttrs {
    let a: sqlx::types::Json<ReleaseAttrs> =
        sqlx::query_scalar("SELECT attrs FROM torrent_attrs WHERE infohash = $1")
            .bind(ih(n))
            .fetch_one(s.pool())
            .await
            .unwrap();
    a.0
}

#[tokio::test]
async fn parsed_at_ingest_and_backfilled() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    corpus(s).await;
    let a = attrs_of(s, 1).await;
    assert_eq!(a.languages, ["fr", "fr-ca", "fr-fr", "original", "multi"]);
    assert_eq!(a.resolution.as_deref(), Some("1080p"));
    let a = attrs_of(s, 9).await;
    assert_eq!(
        a.resolution.as_deref(),
        Some("1080p"),
        "from the largest video file"
    );
    assert_eq!(a.subtitles, ["fr"]);
    // Generated filter columns follow the JSON.
    let (langs, res): (Vec<String>, Option<String>) =
        sqlx::query_as("SELECT languages, resolution FROM torrent_attrs WHERE infohash = $1")
            .bind(ih(2))
            .fetch_one(s.pool())
            .await
            .unwrap();
    assert_eq!(
        (langs, res.as_deref()),
        (vec!["fr".into(), "fr-ca".into()], Some("720p"))
    );
    assert!(!magnetar_server::attrs::pending(s.pool()).await.unwrap());

    // Rows lost or written by an older parser are parsed again.
    sqlx::query("DELETE FROM torrent_attrs WHERE infohash = $1")
        .bind(ih(3))
        .execute(s.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE torrent_attrs SET parser_version = 0, attrs = '{}' WHERE infohash = $1")
        .bind(ih(4))
        .execute(s.pool())
        .await
        .unwrap();
    assert!(magnetar_server::attrs::pending(s.pool()).await.unwrap());
    let r = magnetar_server::attrs::backfill(s.pool(), false, None)
        .await
        .unwrap();
    assert_eq!((r.scanned, r.parsed), (9, 2));
    assert_eq!(attrs_of(s, 3).await.source.as_deref(), Some("remux"));
    assert_eq!(attrs_of(s, 4).await.episode, Some(9));
    let v: i16 = sqlx::query_scalar("SELECT min(parser_version) FROM torrent_attrs")
        .fetch_one(s.pool())
        .await
        .unwrap();
    assert_eq!(v, PARSER_VERSION);
    let r = magnetar_server::attrs::backfill(s.pool(), true, None)
        .await
        .unwrap();
    assert_eq!(r.parsed, 9, "--force re-parses everything");
}

#[tokio::test]
async fn filters_match_on_index_and_postgres() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    corpus(s).await;
    let sql = PgStore::from_pool(s.pool().clone());
    let ix = index_for(s).await;
    let cases: Vec<(&str, SearchQuery, Vec<u64>)> = vec![
        (
            "fr-ca",
            SearchQuery {
                languages: vec!["fr-ca".into()],
                ..q()
            },
            vec![1, 2],
        ),
        (
            "fr any",
            SearchQuery {
                languages: vec!["fr".into()],
                ..q()
            },
            vec![1, 2, 3, 4, 6],
        ),
        (
            "VO",
            SearchQuery {
                languages: vec!["original".into()],
                ..q()
            },
            vec![1, 4, 5, 6],
        ),
        (
            "en or ja",
            SearchQuery {
                languages: vec!["EN".into(), "ja".into()],
                ..q()
            },
            vec![7, 8],
        ),
        (
            "fr subs",
            SearchQuery {
                subtitles: vec!["fr".into()],
                ..q()
            },
            vec![5, 9],
        ),
        (
            "1080p",
            SearchQuery {
                resolutions: vec!["1080p".into()],
                ..q()
            },
            vec![1, 4, 6, 7, 9],
        ),
        (
            "sd",
            SearchQuery {
                resolutions: vec!["sd".into()],
                ..q()
            },
            vec![8],
        ),
        (
            "remux",
            SearchQuery {
                sources: vec!["remux".into()],
                ..q()
            },
            vec![3],
        ),
        (
            "h265",
            SearchQuery {
                video_codecs: vec!["h265".into()],
                ..q()
            },
            vec![3, 9],
        ),
        (
            "dv",
            SearchQuery {
                hdr: vec!["dv".into()],
                ..q()
            },
            vec![3],
        ),
        (
            "atmos",
            SearchQuery {
                audio: vec!["atmos".into()],
                ..q()
            },
            vec![3],
        ),
        (
            "fr + 1080p",
            SearchQuery {
                languages: vec!["fr".into()],
                resolutions: vec!["1080p".into()],
                ..q()
            },
            vec![1, 4, 6],
        ),
        (
            "genre",
            SearchQuery {
                genres: vec!["drama".into()],
                ..q()
            },
            vec![4, 5, 6],
        ),
        (
            "kind",
            SearchQuery {
                media_kinds: vec![MediaKind::Movie],
                ..q()
            },
            vec![1, 2, 3],
        ),
        (
            "rating",
            SearchQuery {
                min_rating: Some(7.0),
                ..q()
            },
            vec![4, 5, 6],
        ),
        // Media year where linked, else the name's year (1999, 2020; anime: none).
        (
            "years",
            SearchQuery {
                min_year: Some(2015),
                max_year: Some(2025),
                ..q()
            },
            vec![1, 2, 3, 9],
        ),
        (
            "old",
            SearchQuery {
                max_year: Some(2000),
                ..q()
            },
            vec![8],
        ),
        (
            "text + filter",
            SearchQuery {
                text: "fargo".into(),
                sources: vec!["web-dl".into()],
                resolutions: vec!["720p".into()],
                ..q()
            },
            vec![5],
        ),
    ];
    for (name, query, want) in cases {
        let want: Vec<String> = want.into_iter().map(ih).collect();
        let a = s.search(&query, NOW).await.unwrap();
        let b = sql.search(&query, NOW).await.unwrap();
        assert_eq!(infohashes(&a), want, "index: {name}");
        assert_eq!(infohashes(&b), want, "postgres: {name}");
        assert_eq!(a.total, want.len() as u64, "{name}");
    }
    // Rating sort: rated first (best first), unrated last, on both paths.
    let rated = SearchQuery {
        sort: Sort::Rating,
        ..q()
    };
    for store in [s, &sql] {
        let p = store.search(&rated, NOW).await.unwrap();
        let top: Vec<Option<f32>> = p.hits.iter().take(6).map(|h| h.media_rating).collect();
        assert_eq!(top[..3], [Some(8.9); 3]);
        assert_eq!(top[3..6], [Some(6.1); 3]);
        assert!(p.hits[6..].iter().all(|h| h.media_rating.is_none()));
        assert!(p.hits.iter().all(|h| h.attrs.is_some()));
    }
    // A changed attribute reaches the index through the change log.
    sqlx::query(
        "UPDATE torrent_attrs SET attrs = jsonb_set(attrs, '{resolution}', '\"480p\"') WHERE infohash = $1",
    )
    .bind(ih(8))
    .execute(s.pool())
    .await
    .unwrap();
    sync(s, &ix).await;
    let p = s
        .search(
            &SearchQuery {
                resolutions: vec!["480p".into()],
                ..q()
            },
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(infohashes(&p), vec![ih(8)]);
}

#[tokio::test]
async fn facet_counts_leave_out_their_own_filter() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    corpus(s).await;
    let fields: Vec<String> = ["language", "resolution", "source", "genre"]
        .map(String::from)
        .to_vec();
    assert!(
        s.facet_set(&q(), &fields, 20, NOW).await.unwrap().is_none(),
        "503 before the build"
    );
    let _ix = index_for(s).await;
    let query = SearchQuery {
        languages: vec!["fr-ca".into()],
        resolutions: vec!["1080p".into()],
        ..q()
    };
    let set: FacetSet = s
        .facet_set(&query, &fields, 20, NOW)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(set.total, 1, "only #1 is fr-ca and 1080p");
    let count = |f: &str, v: &str| {
        set.facets
            .iter()
            .find(|x| x.field == f)
            .and_then(|x| x.counts.iter().find(|c| c.value == v))
            .map_or(0, |c| c.count)
    };
    // Languages over the 1080p results (#1, #4, #6, #7, #9): fr-ca only #1, ja #7.
    assert_eq!(count("language", "fr-ca"), 1);
    assert_eq!(count("language", "fr"), 3);
    assert_eq!(count("language", "ja"), 1);
    // Resolutions over the fr-ca results (#1, #2).
    assert_eq!(count("resolution", "1080p"), 1);
    assert_eq!(count("resolution", "720p"), 1);
    // Unfiltered facets apply every filter.
    assert_eq!(count("source", "web-dl"), 1);
    assert_eq!(count("genre", "comedy"), 1);
    let bad = s.facet_set(&q(), &["nope".to_string()], 20, NOW).await;
    assert!(matches!(bad, Err(magnetar_core::Error::Invalid(_))));
}

/// (season, episode, resolution, language) of a title-page group.
type GroupKey<'a> = (Option<i32>, Option<i32>, Option<&'a str>, &'a str);

#[tokio::test]
async fn releases_grouped_per_title() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (film, show) = corpus(s).await;
    for (n, seeders) in [(4, 50), (5, 3), (6, 7), (1, 9), (2, 1)] {
        s.put_liveness(&Liveness {
            infohash: ih(n),
            seeders: Some(seeders),
            leechers: Some(0),
            completed: None,
            dht_peers_est: None,
            source: LivenessSource::Udp,
            trackers_ok: 1,
            trackers_failed: 0,
            checked_at: NOW,
            expires_at: NOW + 3600,
        })
        .await
        .unwrap();
    }
    for indexed in [false, true] {
        let _ix = if indexed {
            Some(index_for(s).await)
        } else {
            None
        };
        let r = s.media_releases(show, &q(), NOW).await.unwrap().unwrap();
        assert_eq!(r.total, 3);
        let keys: Vec<GroupKey> = r
            .groups
            .iter()
            .map(|g| {
                (
                    g.season,
                    g.episode,
                    g.resolution.as_deref(),
                    g.language.as_str(),
                )
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                (Some(5), None, Some("1080p"), "multi"),
                (Some(5), Some(9), Some("1080p"), "multi"),
                (Some(5), Some(9), Some("720p"), "vostfr"),
            ],
            "indexed: {indexed}"
        );
        assert_eq!(r.groups[1].language_label, "MULTi");
        assert_eq!(r.media.as_ref().unwrap().title, "Fargo");
        // Films: one group per quality/language, no seasons.
        let r = s.media_releases(film, &q(), NOW).await.unwrap().unwrap();
        let keys: Vec<(Option<&str>, &str)> = r
            .groups
            .iter()
            .map(|g| (g.resolution.as_deref(), g.language.as_str()))
            .collect();
        assert_eq!(
            keys,
            vec![
                (Some("2160p"), "vff"),
                (Some("1080p"), "multi-vf2"),
                (Some("720p"), "vfq")
            ]
        );
        // Filters apply.
        let fr_ca = SearchQuery {
            languages: vec!["fr-ca".into()],
            ..q()
        };
        let r = s.media_releases(film, &fr_ca, NOW).await.unwrap().unwrap();
        assert_eq!(r.total, 2);
    }
    assert!(
        s.media_releases(999_999, &q(), NOW)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn saved_searches_honour_filters() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    for indexed in [false, true] {
        let base = if indexed { 100 } else { 0 };
        let _ix = if indexed {
            Some(index_for(s).await)
        } else {
            None
        };
        let saved = SavedSearch {
            id: 0,
            name: format!("quebec {indexed}"),
            query: SearchQuery {
                text: format!("film{base}"),
                languages: vec!["fr-ca".into()],
                ..q()
            },
            notify: true,
            last_checked: None,
            last_seen_created_at: None,
        };
        let id = s.put_saved_search(&saved).await.unwrap();
        // First poll records the watermark.
        magnetar_core::notify::poll_saved_searches(s, NOW)
            .await
            .unwrap();
        put(
            s,
            torrent(
                base + 1,
                1,
                NOW + 10,
                base + 1,
                &format!("Film{base}.2024.VFQ.1080p.WEB.H264-X"),
            ),
        )
        .await;
        put(
            s,
            torrent(
                base + 2,
                1,
                NOW + 11,
                base + 2,
                &format!("Film{base}.2024.VFF.1080p.WEB.H264-X"),
            ),
        )
        .await;
        if let Some(ix) = &_ix {
            sync(s, ix).await;
        }
        let hits = magnetar_core::notify::poll_saved_searches(s, NOW + 20)
            .await
            .unwrap();
        let mine: Vec<&String> = hits
            .iter()
            .filter(|(x, _)| x.id == id)
            .flat_map(|(_, h)| h.iter().map(|h| &h.infohash))
            .collect();
        assert_eq!(mine, vec![&ih(base + 1)], "indexed: {indexed}");
        s.delete_saved_search(id).await.unwrap();
    }
}

#[tokio::test]
async fn api_forms() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (film, _) = corpus(s).await;
    let _ix = index_for(s).await;
    let base = spawn_server(s).await;
    let http = reqwest::Client::new();
    let health: Health = http
        .get(format!("{base}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(health.features.iter().any(|f| f == "filters"));
    let get = |path: String| {
        let http = http.clone();
        async move { http.get(path).bearer_auth(TOKEN).send().await.unwrap() }
    };
    let p: SearchPage = get(format!(
        "{base}/api/v1/search?lang=fr-ca&res=1080p,720p&min_trust=0&sort=rating"
    ))
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(infohashes(&p), vec![ih(1), ih(2)]);
    let p: SearchPage = get(format!(
        "{base}/api/v1/search?kind=series&min_rating=8&min_trust=0"
    ))
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(p.total, 3);
    let resp = get(format!("{base}/api/v1/search?kind=book")).await;
    assert_eq!(resp.status(), 400);
    let set: FacetSet = http
        .post(format!("{base}/api/v1/facets"))
        .bearer_auth(TOKEN)
        .json(&FacetsRequest {
            query: SearchQuery {
                text: "haunted".into(),
                ..q()
            },
            fields: vec!["resolution".into(), "language".into()],
            size: 5,
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(set.total, 3);
    assert_eq!(set.facets[0].counts.len(), 3);
    let one: magnetar_core::api::FacetsResponse = get(format!(
        "{base}/api/v1/facets?field=language&lang=fr-ca&min_trust=0"
    ))
    .await
    .json()
    .await
    .unwrap();
    assert!(
        one.counts.iter().any(|c| c.value == "fr-fr"),
        "own filter left out"
    );
    let r: MediaReleases = get(format!(
        "{base}/api/v1/media/{film}/releases?res=2160p&min_trust=0"
    ))
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(r.groups.len(), 1);
    assert_eq!(r.groups[0].releases[0].infohash, ih(3));
    let api = magnetar_core::api::ApiClient::new(&base, Some(TOKEN.into())).unwrap();
    let r = api
        .media_releases(
            film,
            &SearchQuery {
                min_trust: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(r.total, 3);
    assert_eq!(
        get(format!("{base}/api/v1/media/424242/releases"))
            .await
            .status(),
        404
    );
}
