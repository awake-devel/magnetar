//! D48: IMDb-less media rows created from TMDB-only releases learn their IMDb id from TMDB's
//! details and are merged into the IMDb row of the same title. In-process fake TMDB; needs
//! MAGNETAR_TEST_DATABASE_URL.

mod common;

use std::sync::{Arc, Mutex};

use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use common::*;
use magnetar_core::model::MediaKind;
use magnetar_core::query::SearchQuery;
use magnetar_core::store::Store;
use magnetar_server::PgStore;
use magnetar_server::enrich::ImdbAdoption;
use magnetar_server::search_index::{self, SearchIndex, load};
use magnetar_server::tmdb::{self, Job, Tmdb, TmdbConfig};
use serde_json::json;

#[derive(Clone, Default)]
struct Fake {
    /// "movie/{id}?{query}" / "tv/{id}?{query}" per request.
    log: Arc<Mutex<Vec<String>>>,
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

/// Movie ids: 438631 Dune (tt1160419), 693134 Dune: Part Two (tt15239678), 222 -> tt0000222,
/// 999 = 404, anything else has no IMDb id.
async fn movie(State(f): State<Fake>, Path(id): Path<u32>, q: RawQuery) -> Response {
    f.log
        .lock()
        .unwrap()
        .push(format!("movie/{id}?{}", q.0.unwrap_or_default()));
    let imdb = match id {
        438631 => json!("tt1160419"),
        693134 => json!("tt15239678"),
        222 => json!("tt0000222"),
        999 => return StatusCode::NOT_FOUND.into_response(),
        11 => json!(""),
        _ => json!(null),
    };
    axum::Json(json!({
        "id": id, "title": format!("Movie {id}"), "overview": "o", "imdb_id": imdb,
        "poster_path": "/p.png", "translations": {"translations": []}
    }))
    .into_response()
}

/// Shows: the IMDb id only in `external_ids`, and only when it was asked for.
async fn tv(State(f): State<Fake>, Path(id): Path<u32>, q: RawQuery) -> Response {
    let q = q.0.unwrap_or_default();
    f.log.lock().unwrap().push(format!("tv/{id}?{q}"));
    let mut body = json!({
        "id": id, "name": "Game of Thrones", "overview": "Seven noble families.",
        "translations": {"translations": []}
    });
    if q.contains("external_ids") {
        body["external_ids"] = json!({"id": id, "imdb_id": "tt0944947", "tvdb_id": 121361});
    }
    axum::Json(body).into_response()
}

async fn spawn_fake() -> (String, Fake) {
    let fake = Fake::default();
    let app = axum::Router::new()
        .route("/3/movie/{id}", get(movie))
        .route("/3/tv/{id}", get(tv))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), fake)
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "magnetar-tmdbimdb-{tag}-{:08x}",
        rand::random::<u32>()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tmdb_for(store: &PgStore, base: &str, dir: &std::path::Path) -> Arc<Tmdb> {
    let cfg = TmdbConfig {
        token: "eyJfake.read.token".into(),
        langs: vec!["en-US".into()],
        rps: 50,
        image_dir: dir.to_path_buf(),
        prefetch_per_hour: 0,
        api_base: format!("{base}/3"),
        image_base: format!("{base}/t/p"),
    };
    Tmdb::new(cfg, store.pool().clone()).unwrap()
}

/// An IMDb row as the matcher creates it.
async fn imdb_row(store: &PgStore, imdb: &str, title: &str, year: i32, rating: f32) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO media (kind, imdb, title, year, genres, rating, votes, updated_at)
         VALUES ('movie', $1, $2, $3, '{Sci-Fi}', $4, 900000, 1) RETURNING id",
    )
    .bind(imdb)
    .bind(title)
    .bind(year)
    .bind(rating)
    .fetch_one(store.pool())
    .await
    .unwrap()
}

/// A TMDB-only row as `media_for_ids` creates it (torrent-derived title, no rating).
async fn tmdb_row(
    store: &PgStore,
    kind: &str,
    movie: Option<i32>,
    tv: Option<i32>,
    title: &str,
) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO media (kind, tmdb_movie, tmdb_tv, title, year, updated_at)
         VALUES ($1, $2, $3, $4, 2021, 1) RETURNING id",
    )
    .bind(kind)
    .bind(movie)
    .bind(tv)
    .bind(title)
    .fetch_one(store.pool())
    .await
    .unwrap()
}

async fn link(store: &PgStore, infohash: &str, media_id: i64) {
    sqlx::query(
        "INSERT INTO torrent_media (infohash, media_id, method, confidence, matcher_version,
                                    matched_at)
         VALUES ($1, $2, 'itag', 1.0, 1, 1)",
    )
    .bind(infohash)
    .bind(media_id)
    .execute(store.pool())
    .await
    .unwrap();
}

async fn media_of(store: &PgStore, infohash: &str) -> i64 {
    sqlx::query_scalar("SELECT media_id FROM torrent_media WHERE infohash = $1")
        .bind(infohash)
        .fetch_one(store.pool())
        .await
        .unwrap()
}

async fn exists(store: &PgStore, id: i64) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM media WHERE id = $1)")
        .bind(id)
        .fetch_one(store.pool())
        .await
        .unwrap()
}

type Row = (String, Option<String>, String, Option<f32>, Option<i32>);

async fn row(store: &PgStore, id: i64) -> Row {
    sqlx::query_as("SELECT kind, imdb, title, rating, tmdb_movie FROM media WHERE id = $1")
        .bind(id)
        .fetch_one(store.pool())
        .await
        .unwrap()
}

fn job(media_id: i64) -> Job {
    Job {
        media_id,
        season: None,
    }
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
async fn tmdb_only_row_merges_into_the_imdb_row_and_the_index_follows() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (base, fake) = spawn_fake().await;
    let dir = temp_dir("merge");
    let t = tmdb_for(s, &base, &dir);
    let now = magnetar_core::now();

    // Production shape: Dune (2021) twice, releases split across both rows.
    let keep = imdb_row(s, "tt1160419", "Dune", 2021, 8.0).await;
    let drop = tmdb_row(s, "movie", Some(438631), None, "Dune").await;
    let names = [
        (1, "Dune.2021.1080p.BluRay"),
        (2, "Dune.2021.2160p.WEB"),
        (3, "Dune.2021.MULTi.1080p"),
        (4, "Dune.2021.720p.x264"),
    ];
    for (n, name) in names {
        let (p, _) = put(s, torrent(n, 1, NOW, 100 + n, name)).await;
        link(s, &p.infohash, if n <= 2 { keep } else { drop }).await;
    }

    // Index built before the merge: documents of 3 and 4 carry the TMDB-only row.
    let ix_dir = std::env::temp_dir().join(format!("magnetar-ix-{:016x}", rand::random::<u64>()));
    let id = load::database_id(s.pool()).await.unwrap();
    let idx = SearchIndex::open_async(ix_dir.clone(), id).await.unwrap();
    search_index::build(s.pool(), &idx).await.unwrap();
    s.attach_search_index(idx.clone());
    while search_index::apply_changes(s.pool(), &idx).await.unwrap() > 0 {}
    assert_eq!(
        s.search(&q(&format!("media:{drop}")), NOW)
            .await
            .unwrap()
            .total,
        2
    );

    let r = t.refresh(job(drop), now).await.unwrap();
    assert_eq!(r.tmdb, Some((MediaKind::Movie, 438631)));
    assert_eq!(r.imdb, Some(ImdbAdoption::Merged(keep)));
    // Movies: `imdb_id` is in the details, no external_ids appended.
    assert_eq!(fake.calls("movie/438631"), 1);
    assert!(!fake.log.lock().unwrap()[0].contains("external_ids"));

    // The TMDB-only row is gone; its releases and TMDB id moved to the IMDb row.
    assert!(!exists(s, drop).await);
    for n in 1..=4 {
        assert_eq!(media_of(s, &ih(100 + n)).await, keep);
    }
    let (_, imdb, title, rating, tmdb_movie) = row(s, keep).await;
    assert_eq!(
        (imdb.as_deref(), title.as_str(), rating, tmdb_movie),
        (Some("tt1160419"), "Dune", Some(8.0), Some(438631))
    );

    // The moved infohashes were queued for the index (torrent_media trigger)...
    let queued: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT infohash FROM search_changes ORDER BY infohash")
            .fetch_all(s.pool())
            .await
            .unwrap();
    assert!(
        queued.contains(&ih(103)) && queued.contains(&ih(104)),
        "{queued:?}"
    );
    // ...and once applied, nothing points at the deleted row any more.
    while search_index::apply_changes(s.pool(), &idx).await.unwrap() > 0 {}
    assert_eq!(
        s.search(&q(&format!("media:{drop}")), NOW)
            .await
            .unwrap()
            .total,
        0
    );
    let page = s.search(&q(&format!("media:{keep}")), NOW).await.unwrap();
    assert_eq!(page.total, 4);
    assert!(
        page.hits
            .iter()
            .all(|h| h.media_id == Some(keep) && h.media_rating == Some(8.0)),
        "{:?}",
        page.hits
    );
    assert_eq!(
        s.search(&q("tmdb:movie:438631"), NOW).await.unwrap().total,
        4
    );
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(ix_dir);
}

#[tokio::test]
async fn imdb_set_when_no_row_has_it_and_filled_from_the_dataset() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (base, fake) = spawn_fake().await;
    let dir = temp_dir("set");
    let t = tmdb_for(s, &base, &dir);
    let now = magnetar_core::now();

    // Dune: Part Two is in the imported dataset.
    sqlx::query(
        "INSERT INTO imdb_titles (id, title_type, title, original_title, year, runtime_min,
                                  genres, rating, votes)
         VALUES (15239678, 1, 'Dune: Part Two', 'Dune: Part Two', 2024, 166,
                 '{Action,Adventure,Drama}', 8.4, 650000)",
    )
    .execute(s.pool())
    .await
    .unwrap();
    let m = tmdb_row(s, "movie", Some(693134), None, "Dune Part Two").await;
    let (p, _) = put(s, torrent(1, 1, NOW, 7, "Dune.Part.Two.2024.1080p")).await;
    link(s, &p.infohash, m).await;
    let r = t.refresh(job(m), now).await.unwrap();
    assert_eq!(r.imdb, Some(ImdbAdoption::Set));
    let (kind, imdb, title, rating, tmdb_movie) = row(s, m).await;
    assert_eq!(
        (
            kind.as_str(),
            imdb.as_deref(),
            title.as_str(),
            rating,
            tmdb_movie
        ),
        (
            "movie",
            Some("tt15239678"),
            "Dune: Part Two",
            Some(8.4),
            Some(693134)
        )
    );
    let (year, votes, genres): (Option<i32>, Option<i32>, Vec<String>) =
        sqlx::query_as("SELECT year, votes, genres FROM media WHERE id = $1")
            .bind(m)
            .fetch_one(s.pool())
            .await
            .unwrap();
    assert_eq!((year, votes, genres.len()), (Some(2024), Some(650000), 3));

    // A show: the IMDb id comes from external_ids (asked in the same details call). Not in the
    // dataset: only the id is set; the next import fills the rest.
    let show = tmdb_row(s, "series", None, Some(1399), "Game of Thrones").await;
    let r = t.refresh(job(show), now).await.unwrap();
    assert_eq!(r.imdb, Some(ImdbAdoption::Set));
    assert_eq!(fake.calls("tv/1399"), 1);
    let call = fake.log.lock().unwrap().last().cloned().unwrap();
    assert!(
        call.contains("append_to_response=translations%2Cexternal_ids"),
        "{call}"
    );
    let (kind, imdb, title, rating, _) = row(s, show).await;
    assert_eq!(
        (kind.as_str(), imdb.as_deref(), title.as_str(), rating),
        ("series", Some("tt0944947"), "Game of Thrones", None)
    );

    // Already identified: details fresh and an IMDb id, nothing learned again.
    assert_eq!(t.refresh(job(m), now).await.unwrap().imdb, None);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn anime_rows_are_never_touched() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (base, _fake) = spawn_fake().await;
    let dir = temp_dir("anime");
    let t = tmdb_for(s, &base, &dir);
    let now = magnetar_core::now();

    // An IMDb row exists for tt1160419, but the IMDb-less row is D41's: left alone.
    let imdb = imdb_row(s, "tt1160419", "Dune", 2021, 8.0).await;
    let anime = tmdb_row(s, "anime", Some(438631), None, "Dune anime").await;
    link(s, &ih(1), anime).await;
    let r = t.refresh(job(anime), now).await.unwrap();
    assert_eq!(r.imdb, None);
    assert!(exists(s, anime).await && exists(s, imdb).await);
    assert_eq!(row(s, anime).await.1, None);
    assert_eq!(media_of(s, &ih(1)).await, anime);

    // The IMDb id TMDB gives is held by an anime row: no merge into it, nothing written.
    let held: i64 = sqlx::query_scalar(
        "INSERT INTO media (kind, imdb, mal, title, updated_at)
         VALUES ('anime', 'tt0000222', 5, 'Some Anime', 1) RETURNING id",
    )
    .fetch_one(s.pool())
    .await
    .unwrap();
    let film = tmdb_row(s, "movie", Some(222), None, "Some Anime Movie").await;
    link(s, &ih(2), film).await;
    let r = t.refresh(job(film), now).await.unwrap();
    assert_eq!(r.imdb, Some(ImdbAdoption::Skipped));
    assert!(exists(s, film).await && exists(s, held).await);
    assert_eq!(media_of(s, &ih(2)).await, film);
    assert_eq!(row(s, film).await.1, None);
    let held_row = row(s, held).await;
    assert_eq!((held_row.2.as_str(), held_row.4), ("Some Anime", None));

    // The backfill never picks anime rows.
    let ids = t.backfill_candidates(now, 0, 100).await.unwrap();
    assert!(!ids.contains(&anime), "{ids:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn backfill_resumes_and_does_not_ask_twice() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (base, fake) = spawn_fake().await;
    let dir = temp_dir("backfill");
    let t = tmdb_for(s, &base, &dir);
    let now = magnetar_core::now();

    let keep = imdb_row(s, "tt1160419", "Dune", 2021, 8.0).await;
    let rows = [
        tmdb_row(s, "movie", Some(1), None, "No Imdb").await,
        tmdb_row(s, "movie", Some(438631), None, "Dune").await,
        tmdb_row(s, "movie", Some(693134), None, "Dune Part Two").await,
        tmdb_row(s, "movie", Some(999), None, "Deleted On Tmdb").await,
        tmdb_row(s, "movie", Some(11), None, "Empty Imdb").await,
    ];
    // Details already fresh in the cache: the backfill asks anyway (they carry the IMDb id).
    t.refresh(job(rows[4]), now).await.unwrap();
    assert_eq!(fake.calls("movie/11"), 1);
    sqlx::query("DELETE FROM media_extra WHERE source = $1")
        .bind(tmdb::SRC_IMDB)
        .execute(s.pool())
        .await
        .unwrap();
    let before = fake.log.lock().unwrap().len();

    // Interrupted after two rows...
    let a = t.backfill_imdb(now, 2).await.unwrap();
    assert_eq!((a.checked, a.done), (2, false));
    assert_eq!((a.no_imdb, a.merged), (1, 1));
    assert!(!exists(s, rows[1]).await);
    // ...the next run continues with the rest, never the first two again.
    let b = t.backfill_imdb(now, u64::MAX).await.unwrap();
    assert_eq!((b.checked, b.done, b.failed), (3, true, 0));
    assert_eq!((b.set, b.no_imdb), (1, 2));
    assert_eq!(fake.log.lock().unwrap().len() - before, 5);
    assert_eq!(fake.calls("movie/11"), 2);
    assert_eq!(row(s, rows[2]).await.1.as_deref(), Some("tt15239678"));
    // A finished pass leaves nothing to do until the negative marks expire.
    let c = t.backfill_imdb(now, u64::MAX).await.unwrap();
    assert_eq!((c.checked, c.done), (0, true));
    assert_eq!(fake.log.lock().unwrap().len() - before, 5);
    let later = now + tmdb::NEGATIVE_SECS + 1;
    let mut again = t.backfill_candidates(later, 0, 100).await.unwrap();
    again.sort();
    assert_eq!(again, vec![rows[0], rows[3], rows[4]]);
    assert!(exists(s, keep).await);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn imdb_id_from_details_bodies() {
    assert_eq!(
        tmdb::imdb_from_details(&json!({"imdb_id": "tt1160419"})),
        Some(1160419)
    );
    assert_eq!(
        tmdb::imdb_from_details(&json!({"external_ids": {"imdb_id": "tt0944947"}})),
        Some(944947)
    );
    assert_eq!(tmdb::imdb_from_details(&json!({"imdb_id": ""})), None);
    assert_eq!(tmdb::imdb_from_details(&json!({"imdb_id": null})), None);
    assert_eq!(
        tmdb::imdb_from_details(&json!({"imdb_id": "nm0000206"})),
        None
    );
    assert_eq!(tmdb::imdb_from_details(&json!({})), None);
}
