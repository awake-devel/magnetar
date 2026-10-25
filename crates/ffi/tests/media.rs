//! Media on the phone (D42): server detail media cached for offline use, posters fetched with
//! the bearer token and cached, `imdb:` search offline, other releases. Fake server in-process.

mod common;

use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use common::*;
use futures::executor::block_on;
use magnetar_core::model::{MediaInfo, MediaKind, TorrentDetail};
use magnetar_core::query::SearchPage;
use magnetar_ffi::*;

const TOKEN: &str = "phone-token";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot-really-but-bytes";

#[derive(Clone)]
struct Fake {
    detail: Arc<Mutex<Option<TorrentDetail>>>,
    log: Arc<Mutex<Vec<String>>>,
}

fn authed(h: &HeaderMap) -> bool {
    h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

async fn detail(State(f): State<Fake>, Path(ih): Path<String>, h: HeaderMap) -> Response {
    f.log.lock().unwrap().push(format!("detail/{ih}"));
    if !authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match f.detail.lock().unwrap().clone() {
        Some(d) if d.summary.infohash == ih => axum::Json(d).into_response(),
        _ => (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error":"not_found","message":"x"})),
        )
            .into_response(),
    }
}

async fn poster(State(f): State<Fake>, Path(id): Path<i64>, h: HeaderMap) -> Response {
    let inm = h
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    f.log
        .lock()
        .unwrap()
        .push(format!("poster/{id}/{}", inm.as_deref().unwrap_or("-")));
    if !authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if id != 7 {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error":"not_found","message":"no poster"})),
        )
            .into_response();
    }
    if inm.as_deref() == Some("\"abc\"") {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, "\"abc\"")]).into_response();
    }
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::ETAG, "\"abc\""),
        ],
        PNG,
    )
        .into_response()
}

async fn others(State(f): State<Fake>, Path(id): Path<i64>) -> Response {
    f.log.lock().unwrap().push(format!("others/{id}"));
    let d = f.detail.lock().unwrap().clone().unwrap();
    axum::Json(SearchPage {
        hits: vec![d.summary.clone(), d.summary],
        total: 2,
        source: "x".into(),
    })
    .into_response()
}

/// Fake server on its own runtime thread; returns its base URL.
fn spawn_fake(f: Fake) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let app = axum::Router::new()
                .route("/api/v1/torrents/{ih}", get(detail))
                .route("/api/v1/media/{id}/poster", get(poster))
                .route("/api/v1/media/{id}/torrents", get(others))
                .with_state(f);
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(l.local_addr().unwrap()).unwrap();
            axum::serve(l, app).await.unwrap();
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

fn config(server: Option<String>) -> FfiConfig {
    FfiConfig {
        api_token: server.as_ref().map(|_| TOKEN.to_string()),
        server_url: server,
        root_pubkey: None,
        trust_threshold: 0,
        cache_cap_mb: 0,
        seed_relays: vec![],
        download_dir: String::new(),
        state_dir: String::new(),
    }
}

fn query(text: &str) -> FfiSearchQuery {
    FfiSearchQuery {
        text: text.into(),
        categories: vec![],
        min_size: None,
        max_size: None,
        since: None,
        until: None,
        min_seeders: None,
        hide_dead: false,
        min_trust: Some(0),
        sort: FfiSort::Relevance,
        descending: true,
        limit: 0,
        offset: 0,
        include_hidden: false,
    }
}

fn media() -> MediaInfo {
    MediaInfo {
        media_id: 7,
        kind: MediaKind::Series,
        title: "Game of Thrones".into(),
        original_title: None,
        year: Some(2011),
        end_year: Some(2019),
        genres: vec!["Drama".into()],
        rating: Some(9.2),
        votes: Some(2_400_000),
        runtime_min: Some(57),
        season: Some(1),
        episode: Some(2),
        imdb: Some("tt0944947".into()),
        tmdb: Some((MediaKind::Series, 1399)),
        mal: None,
        anilist: None,
        method: "itag".into(),
        confidence: 1.0,
        overview: Some("Seven noble families.".into()),
        poster: Some("/api/v1/media/7/poster".into()),
        episode_title: Some("The Kingsroad".into()),
    }
}

#[test]
fn server_media_cached_for_offline_and_posters() {
    let dir = TempDir::new("media");
    let db = dir.0.join("db").join("magnetar.db").display().to_string();
    let fake = Fake {
        detail: Arc::default(),
        log: Arc::default(),
    };
    let base = spawn_fake(fake.clone());
    let m = Magnetar::open(db.clone(), config(Some(base.clone()))).unwrap();
    torrent(&ih(1), "Game.of.Thrones.S01E02.1080p", &pk(1), 100).put(m.store());
    torrent(&ih(2), "Unrelated thing", &pk(1), 100).put(m.store());
    let mut d = m.store().torrent_detail_sync(&ih(1), 1).unwrap().unwrap();
    assert!(d.media.is_none() && d.summary.media_title.is_none());
    d.media = Some(media());
    d.summary.media_title = Some("Game of Thrones".into());
    *fake.detail.lock().unwrap() = Some(d);

    // Online: the server's media, kept locally.
    let fd = block_on(m.detail(ih(1), true)).unwrap().unwrap();
    assert_eq!(fd.source, "server");
    let fm = fd.media.unwrap();
    assert_eq!(
        fm.imdb_url.as_deref(),
        Some("https://www.imdb.com/title/tt0944947/")
    );
    assert_eq!(
        (fm.tmdb_kind, fm.tmdb_id),
        (Some(FfiMediaKind::Series), Some(1399))
    );
    assert_eq!(fm.episode_title.as_deref(), Some("The Kingsroad"));
    assert_eq!(fd.summary.media_title.as_deref(), Some("Game of Thrones"));

    // Poster: fetched once with the token, then served from the phone cache.
    let path = block_on(m.media_poster(7, fm.poster.clone().unwrap()))
        .unwrap()
        .unwrap();
    assert!(path.ends_with("posters/7.png"), "{path}");
    assert_eq!(std::fs::read(&path).unwrap(), PNG);
    assert_eq!(
        block_on(m.media_poster(7, fm.poster.clone().unwrap()))
            .unwrap()
            .as_deref(),
        Some(path.as_str())
    );
    assert_eq!(
        fake.log
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with("poster/"))
            .count(),
        1
    );
    // Stale copy: revalidated with If-None-Match, 304 keeps the file.
    m.store()
        .put_poster(7, &path, Some("\"abc\""), PNG.len() as u64, 1)
        .unwrap();
    assert_eq!(
        block_on(m.media_poster(7, fm.poster.clone().unwrap()))
            .unwrap()
            .as_deref(),
        Some(path.as_str())
    );
    assert!(
        fake.log
            .lock()
            .unwrap()
            .contains(&"poster/7/\"abc\"".to_string())
    );
    // Not a server API path: refused before any request.
    assert!(block_on(m.media_poster(8, "https://image.tmdb.org/t/p/w342/x.jpg".into())).is_err());
    assert_eq!(
        block_on(m.media_poster(9, "/api/v1/media/9/poster".into())).unwrap(),
        None
    );

    // Other releases from the server.
    let page = block_on(m.media_torrents(7)).unwrap();
    assert_eq!((page.source.as_str(), page.hits.len()), ("server", 2));

    // Offline: same media from the cache, imdb: search, other releases from the cache.
    m.set_config(config(None)).unwrap();
    let ld = block_on(m.detail(ih(1), true)).unwrap().unwrap();
    assert_eq!(ld.source, "local");
    let lm = ld.media.unwrap();
    assert_eq!(
        (lm.title.as_str(), lm.overview.as_deref()),
        ("Game of Thrones", Some("Seven noble families."))
    );
    assert_eq!(ld.summary.media_year, Some(2011));
    let hits = block_on(m.search(query("imdb:tt0944947"), false))
        .unwrap()
        .hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].media_title.as_deref(), Some("Game of Thrones"));
    assert_eq!(hits[0].media_kind, Some(FfiMediaKind::Series));
    assert_eq!(
        block_on(m.search(query("1080p imdb:0944947"), false))
            .unwrap()
            .total,
        1
    );
    assert_eq!(
        block_on(m.search(query("unrelated imdb:tt0944947"), false))
            .unwrap()
            .total,
        0
    );
    assert_eq!(
        block_on(m.search(query("tmdb:tv:1399"), false))
            .unwrap()
            .total,
        1
    );
    assert_eq!(
        block_on(m.search(query("tmdb:movie:1399"), false))
            .unwrap()
            .total,
        0
    );
    let all = block_on(m.search(query(""), false)).unwrap();
    assert_eq!(all.total, 2);
    let local = block_on(m.media_torrents(7)).unwrap();
    assert_eq!((local.source.as_str(), local.hits.len()), ("local", 1));
    // Offline poster: the cached file.
    assert_eq!(
        block_on(m.media_poster(7, fm.poster.unwrap()))
            .unwrap()
            .as_deref(),
        Some(path.as_str())
    );
    drop(m);

    // Survives reopening (schema V3 persisted).
    let m = Magnetar::open(db, config(None)).unwrap();
    assert!(
        block_on(m.detail(ih(1), false))
            .unwrap()
            .unwrap()
            .media
            .is_some()
    );
}

#[test]
fn i_tag_imdb_search_without_cached_media() {
    let dir = TempDir::new("itag");
    let m = Magnetar::open(dir.0.join("m.db").display().to_string(), config(None)).unwrap();
    torrent(&ih(3), "Inception 2010", &pk(1), 100)
        .with(|p| p.external_ids = vec![("imdb".into(), "tt1375666".into())])
        .put(m.store());
    assert_eq!(
        block_on(m.search(query("imdb:tt1375666"), false))
            .unwrap()
            .total,
        1
    );
    assert_eq!(
        block_on(m.search(query("imdb:tt137566"), false))
            .unwrap()
            .total,
        0
    );
}

/// Against a running server with TMDB on: MAGNETAR_TEST_SERVER_URL, MAGNETAR_TEST_SERVER_TOKEN,
/// MAGNETAR_TEST_MEDIA_INFOHASH (a matched torrent already decorated).
#[test]
#[ignore = "network"]
fn media_against_server() {
    let (Ok(url), Ok(ih)) = (
        std::env::var("MAGNETAR_TEST_SERVER_URL"),
        std::env::var("MAGNETAR_TEST_MEDIA_INFOHASH"),
    ) else {
        eprintln!("SKIPPED (no MAGNETAR_TEST_SERVER_URL / MAGNETAR_TEST_MEDIA_INFOHASH)");
        return;
    };
    let dir = TempDir::new("live-media");
    let mut cfg = config(Some(url));
    cfg.api_token = std::env::var("MAGNETAR_TEST_SERVER_TOKEN").ok();
    let m = Magnetar::open(dir.0.join("m.db").display().to_string(), cfg).unwrap();
    let d = block_on(m.detail(ih.clone(), true)).unwrap().unwrap();
    let media = d.media.expect("server media");
    eprintln!(
        "{} ({:?}) {:?} {:?}",
        media.title, media.year, media.episode_title, media.imdb_url
    );
    assert!(media.overview.is_some());
    let path = block_on(m.media_poster(media.media_id, media.poster.unwrap()))
        .unwrap()
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        bytes.starts_with(&[0xff, 0xd8]) || bytes.starts_with(b"\x89PNG"),
        "{path}"
    );
    eprintln!("poster {} bytes at {path}", bytes.len());
    let page = block_on(m.media_torrents(media.media_id)).unwrap();
    assert_eq!(page.source, "server");
    assert!(page.hits.iter().any(|h| h.infohash == ih));
    // Offline afterwards: media from the phone cache.
    m.set_config(config(None)).unwrap();
    torrent(&ih, &d.summary.title, &pk(1), 1).put(m.store());
    let local = block_on(m.detail(ih, false)).unwrap().unwrap();
    assert_eq!(local.media.unwrap().title, media.title);
}
