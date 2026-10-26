//! Release/media filters on the phone cache (D44): attributes parsed at ingest and backfilled,
//! the same filter semantics as the server, the offline title page, server-only facets.

mod common;

use common::*;
use futures::executor::block_on;
use magnetar_core::model::{Category, MediaInfo, MediaKind};
use magnetar_core::query::{SearchQuery, Sort};
use magnetar_ffi::*;

fn ihs(s: &SqliteStore, q: &SearchQuery) -> Vec<String> {
    let mut v: Vec<String> = s
        .search_sync(q, 1_000)
        .unwrap()
        .hits
        .into_iter()
        .map(|h| h.infohash)
        .collect();
    v.sort();
    v
}

fn media(id: i64, kind: MediaKind, year: i32, genres: &[&str], rating: f32) -> MediaInfo {
    MediaInfo {
        media_id: id,
        kind,
        title: format!("Media {id}"),
        original_title: None,
        year: Some(year),
        end_year: None,
        genres: genres.iter().map(|g| g.to_string()).collect(),
        rating: Some(rating),
        votes: None,
        runtime_min: None,
        season: None,
        episode: None,
        imdb: None,
        tmdb: None,
        mal: None,
        anilist: None,
        method: "itag".into(),
        confidence: 1.0,
        overview: None,
        poster: None,
        episode_title: None,
    }
}

fn corpus(s: &SqliteStore) {
    for (n, title) in [
        (1, "Haunted.Mansion.2023.MULTi.VF2.1080p.WEB.H264-FW"),
        (2, "Haunted.Mansion.2023.VFQ.720p.WEB-DL.H264-Slay3R"),
        (3, "Fargo.S05E09.VOSTFR.720p.WEB.H264-TFA"),
        (4, "Fargo.S05.MULTi.1080p.WEB.DDP5.1.x264-FW"),
        (5, "Old.Film.1999.ENGLISH.DVDRip.XviD-OLD"),
    ] {
        torrent(&ih(n), title, &pk(1), 100 + n as i64).put(s);
    }
    let now = magnetar_core::now();
    for n in [1, 2] {
        let m = media(7, MediaKind::Movie, 2023, &["Comedy"], 6.1);
        s.put_media(&ih(n), Some(&m), now).unwrap();
    }
    for n in [3, 4] {
        let m = media(8, MediaKind::Series, 2014, &["Drama"], 8.9);
        s.put_media(&ih(n), Some(&m), now).unwrap();
    }
}

#[test]
fn local_filters_match_the_server_semantics() {
    let s = SqliteStore::open_in_memory().unwrap();
    corpus(&s);
    let cases: Vec<(&str, SearchQuery, Vec<u64>)> = vec![
        (
            "fr-ca",
            SearchQuery {
                languages: vec!["fr-ca".into()],
                ..q("")
            },
            vec![1, 2],
        ),
        (
            "fr",
            SearchQuery {
                languages: vec!["fr".into()],
                ..q("")
            },
            vec![1, 2, 4],
        ),
        (
            "en",
            SearchQuery {
                languages: vec!["EN".into()],
                ..q("")
            },
            vec![5],
        ),
        (
            "fr subs",
            SearchQuery {
                subtitles: vec!["fr".into()],
                ..q("")
            },
            vec![3],
        ),
        (
            "720p",
            SearchQuery {
                resolutions: vec!["720p".into()],
                ..q("")
            },
            vec![2, 3],
        ),
        (
            "sd",
            SearchQuery {
                resolutions: vec!["sd".into()],
                ..q("")
            },
            vec![5],
        ),
        (
            "dvd",
            SearchQuery {
                sources: vec!["dvd".into()],
                ..q("")
            },
            vec![5],
        ),
        (
            "xvid",
            SearchQuery {
                video_codecs: vec!["xvid".into()],
                ..q("")
            },
            vec![5],
        ),
        (
            "eac3",
            SearchQuery {
                audio: vec!["eac3".into()],
                ..q("")
            },
            vec![4],
        ),
        (
            "text + fr",
            SearchQuery {
                languages: vec!["fr".into()],
                ..q("fargo")
            },
            vec![4],
        ),
        (
            "genre",
            SearchQuery {
                genres: vec!["drama".into()],
                ..q("")
            },
            vec![3, 4],
        ),
        (
            "kind",
            SearchQuery {
                media_kinds: vec![MediaKind::Movie],
                ..q("")
            },
            vec![1, 2],
        ),
        (
            "rating",
            SearchQuery {
                min_rating: Some(8.0),
                ..q("")
            },
            vec![3, 4],
        ),
        (
            "years",
            SearchQuery {
                min_year: Some(2000),
                max_year: Some(2020),
                ..q("")
            },
            vec![3, 4],
        ),
        (
            "old",
            SearchQuery {
                max_year: Some(2000),
                ..q("")
            },
            vec![5],
        ),
    ];
    for (name, query, want) in cases {
        let want: Vec<String> = want.into_iter().map(ih).collect();
        assert_eq!(ihs(&s, &query), want, "{name}");
    }
    let p = s
        .search_sync(
            &SearchQuery {
                sort: Sort::Rating,
                ..q("")
            },
            1_000,
        )
        .unwrap();
    let ratings: Vec<Option<f32>> = p.hits.iter().map(|h| h.media_rating).collect();
    assert_eq!(ratings, [Some(8.9), Some(8.9), Some(6.1), Some(6.1), None]);
    let first = &p.hits[0];
    assert_eq!(first.media_id, Some(8));
    assert!(first.attrs.as_ref().is_some_and(|a| a.season == Some(5)));
    // Detail carries them too.
    let d = s.torrent_detail_sync(&ih(1), 1_000).unwrap().unwrap();
    assert_eq!(d.summary.attrs.unwrap().language_key(), "multi-vf2");
}

#[test]
fn backfill_parses_rows_without_attributes() {
    let dir = TempDir::new("attrs");
    let path = dir.0.join("m.db");
    {
        let s = SqliteStore::open(&path).unwrap();
        corpus(&s);
    }
    // Rows from before D44 (no attributes) and from an older parser.
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute("DELETE FROM torrent_attrs WHERE infohash = ?1", [ih(1)])
        .unwrap();
    c.execute(
        "UPDATE torrent_attrs SET parser_version = 0, attrs = '{}' WHERE infohash = ?1",
        [ih(2)],
    )
    .unwrap();
    drop(c);
    let s = SqliteStore::open(&path).unwrap();
    assert!(
        ihs(
            &s,
            &SearchQuery {
                languages: vec!["fr-ca".into()],
                ..q("")
            }
        )
        .is_empty()
    );
    let mut after = String::new();
    assert_eq!(s.backfill_attrs(&mut after, 1).unwrap(), 1);
    assert_eq!(s.backfill_attrs(&mut after, 10).unwrap(), 1);
    assert_eq!(s.backfill_attrs(&mut after, 10).unwrap(), 0);
    assert_eq!(
        ihs(
            &s,
            &SearchQuery {
                languages: vec!["fr-ca".into()],
                ..q("")
            }
        ),
        vec![ih(1), ih(2)]
    );
    // Opening through the app runs the backfill in the background.
    drop(s);
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute("DELETE FROM torrent_attrs", []).unwrap();
    drop(c);
    let m = Magnetar::open(path.display().to_string(), offline()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while ihs(
        m.store(),
        &SearchQuery {
            resolutions: vec!["1080p".into()],
            ..q("")
        },
    )
    .len()
        < 2
    {
        assert!(std::time::Instant::now() < deadline, "backfill did not run");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// (season, episode, resolution, language) of a title-page group.
type GroupKey = (Option<i32>, Option<i32>, Option<String>, String);

fn offline() -> FfiConfig {
    FfiConfig {
        server_url: None,
        api_token: None,
        root_pubkey: None,
        trust_threshold: 0,
        cache_cap_mb: 0,
        seed_relays: vec![],
        download_dir: String::new(),
        state_dir: String::new(),
        show_adult: false,
    }
}

#[test]
fn offline_title_page_and_server_only_facets() {
    let dir = TempDir::new("title");
    let m = Magnetar::open(dir.0.join("m.db").display().to_string(), offline()).unwrap();
    corpus(m.store());
    torrent(&ih(6), "Fargo.S05E09.MULTi.1080p.WEB.x264-FW", &pk(1), 300)
        .with(|p| p.category = Category::Video)
        .put(m.store());
    let now = magnetar_core::now();
    let fargo = media(8, MediaKind::Series, 2014, &["Drama"], 8.9);
    m.store().put_media(&ih(6), Some(&fargo), now).unwrap();
    let all: FfiSearchQuery = SearchQuery::default().into();
    let r = block_on(m.media_releases(8, all.clone())).unwrap();
    assert_eq!((r.source.as_str(), r.total), ("local", 3));
    assert_eq!(r.media.as_ref().map(|m| m.title.as_str()), Some("Media 8"));
    let keys: Vec<GroupKey> = r
        .groups
        .iter()
        .map(|g| {
            (
                g.season,
                g.episode,
                g.resolution.clone(),
                g.language.clone(),
            )
        })
        .collect();
    assert_eq!(
        keys,
        vec![
            (Some(5), None, Some("1080p".into()), "multi".into()),
            (Some(5), Some(9), Some("1080p".into()), "multi".into()),
            (Some(5), Some(9), Some("720p".into()), "vostfr".into()),
        ]
    );
    assert!(
        r.groups[0].releases[0]
            .attrs
            .as_ref()
            .unwrap()
            .badges
            .contains(&"1080p".to_string())
    );
    let vostfr = SearchQuery {
        subtitles: vec!["fr".into()],
        ..Default::default()
    };
    let r = block_on(m.media_releases(8, vostfr.into())).unwrap();
    assert_eq!(r.total, 1);
    // Offline: no features, facets refuse.
    assert!(block_on(m.server_features()).is_none());
    assert_eq!(
        facet_label("language".into(), "fr-ca".into()),
        "French (Québec)"
    );
    assert!(filter_options("resolution".into()).contains(&"2160p".to_string()));
    assert!(matches!(
        block_on(m.facets(all, vec!["language".into()])),
        Err(FfiError::Invalid { .. })
    ));
}

/// Fake server older than D44: `/health` lists no features, `/api/v1/search` ignores the new
/// filters (answers an empty page). Returns its base URL.
fn spawn_old_server() -> String {
    use axum::routing::get;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let health = || async {
                axum::Json(serde_json::json!({"status":"ok","version":"0.1.0","db":true}))
            };
            let search = || async {
                axum::Json(magnetar_core::query::SearchPage {
                    hits: vec![],
                    total: 0,
                    source: "x".into(),
                    total_is_lower_bound: false,
                })
            };
            let app = axum::Router::new()
                .route("/health", get(health))
                .route("/api/v1/search", get(search).post(search));
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(l.local_addr().unwrap()).unwrap();
            axum::serve(l, app).await.unwrap();
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

#[test]
fn old_server_gets_no_filtered_query() {
    let dir = TempDir::new("oldsrv");
    let mut cfg = offline();
    cfg.server_url = Some(spawn_old_server());
    cfg.api_token = Some("t".into());
    let m = Magnetar::open(dir.0.join("m.db").display().to_string(), cfg).unwrap();
    corpus(m.store());
    // No D44 filter: the server answers.
    let p = block_on(m.search(SearchQuery::default().into(), true)).unwrap();
    assert_eq!(p.source, "server");
    // A release filter it would ignore: the local cache applies it instead.
    let fr_ca = SearchQuery {
        languages: vec!["fr-ca".into()],
        ..Default::default()
    };
    let p = block_on(m.search(fr_ca.into(), true)).unwrap();
    assert_eq!(p.source, "local");
    let mut got: Vec<String> = p.hits.into_iter().map(|h| h.infohash).collect();
    got.sort();
    assert_eq!(got, vec![ih(1), ih(2)]);
    // Rating sort, which it would reject, too.
    let rating = SearchQuery {
        sort: Sort::Rating,
        ..Default::default()
    };
    assert_eq!(
        block_on(m.search(rating.into(), true)).unwrap().source,
        "local"
    );
    assert_eq!(block_on(m.server_features()), Some(vec![]));
}
