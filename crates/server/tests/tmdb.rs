//! TMDB decoration (D42) against an in-process fake TMDB (needs MAGNETAR_TEST_DATABASE_URL).
//! The `media` / `torrent_media` rows are written directly: they belong to the matcher.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use common::*;
use magnetar_core::model::{MediaKind, TorrentDetail};
use magnetar_server::tmdb::{self, Job, Tmdb, TmdbConfig};
use magnetar_server::{AppState, PgStore};
use serde_json::json;

const FAKE_TOKEN: &str = "eyJfake.read.token";
/// 1x1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0, 0, 0,
    1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0x0a, 0x49, 0x44, 0x41, 0x54,
    0x78, 0x9c, 0x63, 0, 1, 0, 0, 5, 0, 1, 0x0d, 0x0a, 0x2d, 0xb4, 0, 0, 0, 0, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

#[derive(Clone, Default)]
struct Fake {
    log: Arc<Mutex<Vec<String>>>,
    /// Answer the next N movie-details calls with 429 + Retry-After: 1.
    throttle: Arc<Mutex<u32>>,
}

impl Fake {
    fn calls(&self, prefix: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with(prefix))
            .count()
    }
}

fn authed(h: &HeaderMap) -> bool {
    h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())
        == Some(&format!("Bearer {FAKE_TOKEN}"))
}

async fn find(State(f): State<Fake>, Path(id): Path<String>, h: HeaderMap) -> Response {
    f.log.lock().unwrap().push(format!("find/{id}"));
    if !authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let body = match id.as_str() {
        "tt1375666" => {
            json!({"movie_results": [{"id": 27205}], "tv_results": [], "tv_episode_results": []})
        }
        "tt0944947" => {
            json!({"movie_results": [], "tv_results": [{"id": 1399}], "tv_episode_results": []})
        }
        _ => json!({"movie_results": [], "tv_results": [], "tv_episode_results": []}),
    };
    axum::Json(body).into_response()
}

async fn movie(State(f): State<Fake>, Path(id): Path<u32>, h: HeaderMap) -> Response {
    f.log.lock().unwrap().push(format!("movie/{id}"));
    if !authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    {
        let mut t = f.throttle.lock().unwrap();
        if *t > 0 {
            *t -= 1;
            return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "1")]).into_response();
        }
    }
    axum::Json(json!({
        "id": id, "title": "Inception", "overview": "Cobb steals secrets.",
        "poster_path": "/inception.png", "backdrop_path": "/bd.jpg",
        "translations": {"translations": [
            {"iso_639_1": "fr", "iso_3166_1": "FR", "data": {"title": "Inception", "overview": "Dom Cobb est un voleur."}}
        ]}
    }))
    .into_response()
}

async fn tv(State(f): State<Fake>, Path(id): Path<u32>) -> Response {
    f.log.lock().unwrap().push(format!("tv/{id}"));
    axum::Json(json!({
        "id": id, "name": "Game of Thrones", "overview": "Seven noble families.",
        "poster_path": "/got.png",
        "translations": {"translations": []}
    }))
    .into_response()
}

async fn season(State(f): State<Fake>, Path((id, n)): Path<(u32, u32)>, h: HeaderMap) -> Response {
    let _ = h;
    f.log.lock().unwrap().push(format!("season/{id}/{n}"));
    axum::Json(json!({"episodes": [
        {"episode_number": 1, "name": "Winter Is Coming"},
        {"episode_number": 2, "name": "The Kingsroad"}
    ]}))
    .into_response()
}

async fn image(State(f): State<Fake>, Path(file): Path<String>) -> Response {
    f.log.lock().unwrap().push(format!("image/{file}"));
    ([(header::CONTENT_TYPE, "image/png")], PNG).into_response()
}

async fn spawn_fake() -> (String, Fake) {
    let fake = Fake::default();
    let app = axum::Router::new()
        .route("/3/find/{id}", get(find))
        .route("/3/movie/{id}", get(movie))
        .route("/3/tv/{id}", get(tv))
        .route("/3/tv/{id}/season/{n}", get(season))
        .route("/t/p/w342/{file}", get(image))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), fake)
}

fn config(base: &str, dir: &std::path::Path) -> TmdbConfig {
    TmdbConfig {
        token: FAKE_TOKEN.into(),
        langs: vec!["en-US".into(), "fr-FR".into()],
        rps: 50,
        image_dir: dir.to_path_buf(),
        prefetch_per_hour: 60,
        api_base: format!("{base}/3"),
        image_base: format!("{base}/t/p"),
    }
}

async fn add_media(store: &PgStore, kind: &str, imdb: &str, title: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO media (kind, imdb, title, year, genres, rating, votes, updated_at)
         VALUES ($1, $2, $3, 2010, '{Action,Sci-Fi}', 8.8, 2500000, 1) RETURNING id",
    )
    .bind(kind)
    .bind(imdb)
    .bind(title)
    .fetch_one(store.pool())
    .await
    .unwrap()
}

async fn link(store: &PgStore, infohash: &str, media_id: i64, se: Option<(i32, i32)>) {
    sqlx::query(
        "INSERT INTO torrent_media (infohash, media_id, season, episode, method, confidence,
                                    matcher_version, matched_at)
         VALUES ($1, $2, $3, $4, 'itag', 1.0, 1, $5)",
    )
    .bind(infohash)
    .bind(media_id)
    .bind(se.map(|s| s.0))
    .bind(se.map(|s| s.1))
    .bind(magnetar_core::now())
    .execute(store.pool())
    .await
    .unwrap();
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("magnetar-tmdb-{tag}-{:08x}", rand::random::<u32>()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn refresh_find_details_season_poster_and_cache() {
    let Some(db) = test_db().await else { return };
    let (base, fake) = spawn_fake().await;
    let dir = temp_dir("refresh");
    let t = Tmdb::new(config(&base, &dir), db.store.pool().clone()).unwrap();
    let now = magnetar_core::now();

    let movie = add_media(&db.store, "movie", "tt1375666", "Inception").await;
    *fake.throttle.lock().unwrap() = 1;
    let started = Instant::now();
    let r = t
        .refresh(
            Job {
                media_id: movie,
                season: None,
            },
            now,
        )
        .await
        .unwrap();
    assert_eq!(r.tmdb, Some((MediaKind::Movie, 27205)));
    assert!(r.details && r.poster && !r.season);
    // One 429 honoured with Retry-After: 1.
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert_eq!(fake.calls("movie/"), 2);
    assert_eq!((fake.calls("find/"), fake.calls("image/")), (1, 1));
    let tmdb_movie: Option<i32> = sqlx::query_scalar("SELECT tmdb_movie FROM media WHERE id = $1")
        .bind(movie)
        .fetch_one(db.store.pool())
        .await
        .unwrap();
    assert_eq!(tmdb_movie, Some(27205));

    // Everything fresh: no request at all.
    let before = t.requests();
    let r = t
        .refresh(
            Job {
                media_id: movie,
                season: None,
            },
            now,
        )
        .await
        .unwrap();
    assert!(!r.details && !r.poster);
    assert_eq!(t.requests(), before);

    let x = tmdb::load_extras(db.store.pool(), movie).await.unwrap();
    assert_eq!(
        x.details["fr-FR"].0.overview.as_deref(),
        Some("Dom Cobb est un voleur.")
    );
    let img = x.image.unwrap().0;
    assert_eq!(img.content_type, "image/png");
    let file = t.image_file(&img);
    assert_eq!(std::fs::read(&file).unwrap(), PNG);

    // Series with an episode: tv details + one season call per language.
    let show = add_media(&db.store, "series", "tt0944947", "Game of Thrones").await;
    let r = t
        .refresh(
            Job {
                media_id: show,
                season: Some(1),
            },
            now,
        )
        .await
        .unwrap();
    assert_eq!(r.tmdb, Some((MediaKind::Series, 1399)));
    assert!(r.details && r.season && r.poster);
    assert_eq!(fake.calls("season/1399/1"), 2);

    // Unknown to TMDB: cached negative, not asked again.
    let none = add_media(&db.store, "movie", "tt0000001", "Nothing").await;
    assert_eq!(
        t.refresh(
            Job {
                media_id: none,
                season: None
            },
            now
        )
        .await
        .unwrap()
        .tmdb,
        None
    );
    assert_eq!(
        t.refresh(
            Job {
                media_id: none,
                season: None
            },
            now
        )
        .await
        .unwrap()
        .tmdb,
        None
    );
    assert_eq!(fake.calls("find/tt0000001"), 1);

    // Older than 6 months is purged, with the now unreferenced poster file.
    let (rows, _) = t.purge(now + tmdb::PURGE_SECS + 10).await.unwrap();
    assert!(rows >= 6, "{rows}");
    assert!(!file.exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn detail_is_decorated_lazily_and_poster_is_served() {
    let Some(db) = test_db().await else { return };
    let (fake_base, fake) = spawn_fake().await;
    let dir = temp_dir("api");
    let t = Tmdb::new(config(&fake_base, &dir), db.store.pool().clone()).unwrap();
    let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    t.spawn_workers(stop_rx);

    let (p, _) = put(
        &db.store,
        torrent(1, 1, NOW, 77, "Game.of.Thrones.S01E02.1080p"),
    )
    .await;
    let show = add_media(&db.store, "series", "tt0944947", "Game of Thrones").await;
    link(&db.store, &p.infohash, show, Some((1, 2))).await;

    let state = AppState::new(Arc::new(db.store.clone()), TOKEN).with_tmdb(Some(t.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, magnetar_server::router(state))
            .await
            .unwrap()
    });
    let http = magnetar_core::http::client(Duration::from_secs(20)).unwrap();
    let detail = || async {
        http.get(format!("{base}/api/v1/torrents/{}", p.infohash))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json::<TorrentDetail>()
            .await
            .unwrap()
    };

    // First view: matcher data only, TMDB fetch queued in the background.
    let started = Instant::now();
    let d = detail().await;
    assert!(started.elapsed() < Duration::from_secs(2));
    let m = d.media.expect("media");
    assert_eq!(
        (m.title.as_str(), m.season, m.episode),
        ("Game of Thrones", Some(1), Some(2))
    );
    assert_eq!(d.summary.media_title.as_deref(), Some("Game of Thrones"));
    let deadline = Instant::now() + Duration::from_secs(10);
    let m = loop {
        let m = detail().await.media.unwrap();
        if m.episode_title.is_some() && m.poster.is_some() {
            break m;
        }
        assert!(Instant::now() < deadline, "never decorated: {m:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(m.overview.as_deref(), Some("Seven noble families."));
    assert_eq!(m.episode_title.as_deref(), Some("The Kingsroad"));
    assert_eq!(m.tmdb, Some((MediaKind::Series, 1399)));
    let poster = m.poster.unwrap();
    assert_eq!(poster, format!("/api/v1/media/{show}/poster"));
    let calls = fake.log.lock().unwrap().len();

    // Poster: auth required, bytes + cache headers, 304 on the ETag.
    let r = http.get(format!("{base}{poster}")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = http
        .get(format!("{base}{poster}"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()[header::CONTENT_TYPE], "image/png");
    assert!(
        r.headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .starts_with("private")
    );
    let etag = r.headers()[header::ETAG].clone();
    assert_eq!(r.bytes().await.unwrap().as_ref(), PNG);
    let r = http
        .get(format!("{base}{poster}"))
        .bearer_auth(TOKEN)
        .header(header::IF_NONE_MATCH, etag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_MODIFIED);
    // Served from the cache: TMDB was not contacted again.
    let _ = detail().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.log.lock().unwrap().len(), calls);

    // Other releases of the same media.
    let (p2, _) = put(
        &db.store,
        torrent(2, 2, NOW, 78, "Game.of.Thrones.S01.Complete"),
    )
    .await;
    link(&db.store, &p2.infohash, show, Some((1, 0))).await;
    let page: magnetar_core::query::SearchPage = http
        .get(format!("{base}/api/v1/media/{show}/torrents"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut ihs: Vec<_> = page.hits.iter().map(|h| h.infohash.clone()).collect();
    ihs.sort();
    assert_eq!(ihs, vec![p.infohash.clone(), p2.infohash.clone()]);
    assert!(
        page.hits
            .iter()
            .all(|h| h.media_title.as_deref() == Some("Game of Thrones"))
    );

    let r = http
        .get(format!("{base}/api/v1/media/999999/poster"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn prefetch_picks_recent_undecorated_media() {
    let Some(db) = test_db().await else { return };
    let (base, _fake) = spawn_fake().await;
    let dir = temp_dir("prefetch");
    let t = Tmdb::new(config(&base, &dir), db.store.pool().clone()).unwrap();
    let a = add_media(&db.store, "movie", "tt1375666", "Inception").await;
    let b = add_media(&db.store, "series", "tt0944947", "Game of Thrones").await;
    for (i, ih) in ["a1", "a2", "a3"].iter().enumerate() {
        link(&db.store, &format!("{ih:0>40}"), a, None).await;
        if i == 0 {
            link(&db.store, &format!("b{ih:0>39}"), b, Some((1, 1))).await;
        }
    }
    let now = magnetar_core::now();
    assert_eq!(t.prefetch_candidates(now, 10).await.unwrap(), vec![a, b]);
    t.refresh(
        Job {
            media_id: a,
            season: None,
        },
        now,
    )
    .await
    .unwrap();
    assert_eq!(t.prefetch_candidates(now, 10).await.unwrap(), vec![b]);
    let _ = std::fs::remove_dir_all(dir);
}
