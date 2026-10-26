//! DHT torrents in search, facets, detail and stats (D51, docs/DHT.md §5): equal mixing with
//! Nostr results on the index and on the SQL fallback. Needs MAGNETAR_TEST_DATABASE_URL (else
//! SKIPPED).

use crate::common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use common::*;
use magnetar_core::model::*;
use magnetar_core::query::{SearchPage, SearchQuery, Sort};
use magnetar_core::store::Store;
use magnetar_server::PgStore;
use magnetar_server::search_index::{self, SearchIndex, load};

fn q(text: &str, min_trust: u8) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        min_trust: Some(min_trust),
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
    store.attach_search_index(idx.clone());
    Ix { idx, dir }
}

async fn dht_row(s: &PgStore, n: u64, title: &str, files: &[&str], category: &str) {
    let files_json: Vec<serde_json::Value> = files
        .iter()
        .map(|p| serde_json::json!({"path": p, "size": 1_000_000}))
        .collect();
    sqlx::query(
        "INSERT INTO dht_torrents (infohash, title, files, file_count, files_text, total_size,
             category, metadata_size, first_seen, fetched_at, discovered_via, peers_seen,
             classifier_version)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 1000, $8, $8 + 30, 'sample', 7, 2)",
    )
    .bind(ih(n))
    .bind(title)
    .bind(serde_json::Value::Array(files_json))
    .bind(files.len() as i32)
    .bind(files.join("\n"))
    .bind(files.len() as i64 * 1_000_000)
    .bind(category)
    .bind(NOW - 1000 + n as i64)
    .execute(s.pool())
    .await
    .unwrap();
}

/// infohash → (title, origin) of a page.
fn by_ih(p: &SearchPage) -> BTreeMap<String, (String, Origin)> {
    p.hits
        .iter()
        .map(|h| (h.infohash.clone(), (h.title.clone(), h.origin)))
        .collect()
}

fn origins(p: &SearchPage) -> BTreeMap<String, Origin> {
    p.hits
        .iter()
        .map(|h| (h.infohash.clone(), h.origin))
        .collect()
}

/// (infohash, title, trust, publisher_count, origin) per hit by infohash (relevance differs
/// between the paths by design), and the total.
type Shape = (u64, BTreeMap<String, (String, u8, u32, Origin)>);

fn shape(p: &SearchPage) -> Shape {
    (
        p.total,
        p.hits
            .iter()
            .map(|h| {
                (
                    h.infohash.clone(),
                    (h.title.clone(), h.trust, h.publisher_count, h.origin),
                )
            })
            .collect(),
    )
}

#[tokio::test]
async fn dht_results_mix_equally_on_both_paths() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    // 1: Nostr, trust 0. 2: Nostr with PoW (high trust). 3: Nostr trust 0 + DHT. 4: DHT only.
    // 5: blocked publisher + DHT. 6: blocked publisher only. 7: Nostr trust 0, other words.
    put(
        s,
        torrent(1, 1, NOW - 50, 1, "ubuntu desktop").cat(Category::Application),
    )
    .await;
    put(s, torrent(2, 2, NOW - 40, 2, "ubuntu server").pow(24)).await;
    put(s, torrent(3, 3, NOW - 30, 3, "ubuntu core")).await;
    put(s, torrent(5, 9, NOW - 20, 5, "ubuntu mate")).await;
    put(s, torrent(6, 9, NOW - 10, 6, "ubuntu budgie")).await;
    put(s, torrent(7, 1, NOW - 5, 7, "fedora workstation")).await;
    dht_row(
        s,
        3,
        "Ubuntu.Core.24.04",
        &["Ubuntu.Core.24.04/core.img"],
        "application",
    )
    .await;
    dht_row(
        s,
        4,
        "ubuntu.studio.iso",
        &[
            "ubuntu-studio/ubuntu-studio-24.04.iso",
            "ubuntu-studio/README",
        ],
        "application",
    )
    .await;
    dht_row(
        s,
        5,
        "ubuntu-mate-dht",
        &["ubuntu-mate-dht/mate.iso"],
        "application",
    )
    .await;
    s.set_override(&hex64(9), Some(OverrideAction::Block))
        .await
        .unwrap();

    let ix = index_for(s).await;
    // 7 infohashes: 1–7 (6's only event is from a blocked publisher: no document).
    assert_eq!(ix.idx.num_docs(), 6);
    let pg = PgStore::from_pool(s.pool().clone());

    // min_trust 0: the canonical row is the same on both paths.
    let a = s.search(&q("ubuntu", 0), NOW).await.unwrap();
    let b = pg.search(&q("ubuntu", 0), NOW).await.unwrap();
    assert_eq!(shape(&a), shape(&b));
    let want: BTreeMap<String, Origin> = [
        (ih(1), Origin::Nostr),
        (ih(2), Origin::Nostr),
        (ih(3), Origin::Both),
        (ih(4), Origin::Dht),
        (ih(5), Origin::Dht),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        origins(&a),
        want,
        "a blocked publisher hides its events, not the DHT row"
    );
    let dht_only = a.hits.iter().find(|h| h.infohash == ih(4)).unwrap();
    assert_eq!(dht_only.event_id, "");
    assert_eq!(dht_only.npub, "");
    assert_eq!(dht_only.publisher_count, 0);
    assert_eq!(dht_only.trust, 0);
    assert_eq!(dht_only.category, Category::Application);
    let both = a.hits.iter().find(|h| h.infohash == ih(3)).unwrap();
    assert_eq!(both.title, "ubuntu core", "a Nostr event stays canonical");
    assert_eq!(both.publisher_count, 1, "the DHT row is not a publisher");

    // min_trust 20 (D51): DHT results pass any threshold, trust-0 Nostr-only ones do not.
    let a = s.search(&q("ubuntu", 20), NOW).await.unwrap();
    let b = pg.search(&q("ubuntu", 20), NOW).await.unwrap();
    let want: Vec<String> = [2, 3, 4, 5].iter().map(|n| ih(*n)).collect();
    assert_eq!(by_ih(&a).keys().cloned().collect::<Vec<_>>(), want);
    assert_eq!(by_ih(&b).keys().cloned().collect::<Vec<_>>(), want);
    assert_eq!(origins(&a), origins(&b));
    assert_eq!(a.total, 4);
    assert_eq!(b.total, 4);
    // Documented difference: the index shows the canonical (below-threshold) Nostr event,
    // the row-level fallback the DHT row that passed.
    assert_eq!(by_ih(&a)[&ih(3)].0, "ubuntu core");
    assert_eq!(by_ih(&b)[&ih(3)].0, "Ubuntu.Core.24.04");
    let mut hundred = q("ubuntu", 100);
    hundred.limit = 10;
    for p in [
        s.search(&hundred, NOW).await.unwrap(),
        pg.search(&hundred, NOW).await.unwrap(),
    ] {
        let mut k: Vec<String> = by_ih(&p).keys().cloned().collect();
        k.sort();
        assert_eq!(k, vec![ih(3), ih(4), ih(5)], "threshold 100: DHT only");
    }

    // The source chip: a torrent known from both matches either value.
    for (vals, want) in [
        (vec!["dht"], vec![3, 4, 5]),
        (vec!["nostr"], vec![1, 2, 3]),
        (vec!["DHT", "nostr"], vec![1, 2, 3, 4, 5]),
        (vec!["bogus"], vec![1, 2, 3, 4, 5]),
    ] {
        let mut f = q("ubuntu", 0);
        f.origins = vals.iter().map(|v| v.to_string()).collect();
        let want: Vec<String> = want.iter().map(|n| ih(*n)).collect();
        for p in [
            s.search(&f, NOW).await.unwrap(),
            pg.search(&f, NOW).await.unwrap(),
        ] {
            assert_eq!(
                by_ih(&p).keys().cloned().collect::<Vec<_>>(),
                want,
                "{vals:?}"
            );
        }
    }
    let parsed = magnetar_server::api::parse_search_query(Some("q=x&origin=dht,nostr")).unwrap();
    assert_eq!(parsed.origins, vec!["dht", "nostr"]);
    assert!(magnetar_server::api::parse_search_query(Some("origin=tracker")).is_err());

    // Facet: disjunctive, so the chip does not shrink its own counts.
    let mut f = q("ubuntu", 0);
    f.origins = vec!["dht".into()];
    let facet = s.facets(&f, "origin", 10, NOW).await.unwrap().unwrap();
    let counts: BTreeMap<String, u64> = facet
        .counts
        .iter()
        .map(|c| (c.value.clone(), c.count))
        .collect();
    assert_eq!(
        counts,
        [("dht".to_string(), 3), ("nostr".to_string(), 3)]
            .into_iter()
            .collect()
    );

    // DHT titles and file names are searchable; a file-only match names the file.
    let p = s.search(&q("studio 24.04", 0), NOW).await.unwrap();
    assert_eq!(by_ih(&p).keys().cloned().collect::<Vec<_>>(), vec![ih(4)]);
    assert_eq!(
        p.hits[0].matched_file.as_deref(),
        Some("ubuntu-studio/ubuntu-studio-24.04.iso")
    );
    let p = pg.search(&q("studio", 0), NOW).await.unwrap();
    assert_eq!(by_ih(&p).keys().cloned().collect::<Vec<_>>(), vec![ih(4)]);
    // The DHT title of a Nostr torrent is searchable too.
    let p = s.search(&q("core.img", 0), NOW).await.unwrap();
    assert_eq!(by_ih(&p).keys().cloned().collect::<Vec<_>>(), vec![ih(3)]);

    // The index follows DHT inserts and deletes through the change log.
    dht_row(
        s,
        8,
        "fedora.silverblue",
        &["silverblue.iso"],
        "application",
    )
    .await;
    while search_index::apply_changes(s.pool(), &ix.idx)
        .await
        .unwrap()
        > 0
    {}
    let p = s.search(&q("silverblue", 50), NOW).await.unwrap();
    assert_eq!(origins(&p), [(ih(8), Origin::Dht)].into_iter().collect());
    sqlx::query("DELETE FROM dht_torrents WHERE infohash = $1")
        .bind(ih(8))
        .execute(s.pool())
        .await
        .unwrap();
    while search_index::apply_changes(s.pool(), &ix.idx)
        .await
        .unwrap()
        > 0
    {}
    assert!(
        s.search(&q("silverblue", 0), NOW)
            .await
            .unwrap()
            .hits
            .is_empty()
    );

    // Detail: DHT only, both, blocked publisher + DHT.
    let d = s.torrent_detail(&ih(4), NOW).await.unwrap().unwrap();
    assert_eq!(d.summary.origin, Origin::Dht);
    assert!(d.events.is_empty() && d.comments.is_empty() && d.trackers.is_empty());
    assert_eq!(d.trust.factors[0].code, "dht");
    assert!(d.magnet.contains("&tr="), "default trackers: {}", d.magnet);
    assert_eq!(d.files.len(), 2);
    let info = d.dht.unwrap();
    assert_eq!(info.title, "ubuntu.studio.iso");
    assert_eq!(info.peers_seen, 7);
    assert_eq!(info.discovered_via, "sample");
    let d = s.torrent_detail(&ih(3), NOW).await.unwrap().unwrap();
    assert_eq!(d.summary.origin, Origin::Both);
    assert_eq!(d.summary.title, "ubuntu core");
    assert_eq!(d.events.len(), 1);
    assert_eq!(d.dht.unwrap().title, "Ubuntu.Core.24.04");
    let d = s.torrent_detail(&ih(5), NOW).await.unwrap().unwrap();
    assert_eq!(d.summary.origin, Origin::Dht);
    assert_eq!(d.summary.title, "ubuntu-mate-dht");
    assert!(s.torrent_detail(&ih(6), NOW).await.unwrap().is_none());
    let d = s.torrent_detail(&ih(1), NOW).await.unwrap().unwrap();
    assert_eq!(d.summary.origin, Origin::Nostr);
    assert!(d.dht.is_none());

    // "Published" per day counts Nostr publications only (the DHT fills the schedule too).
    magnetar_server::stats::recompute_published(s.pool())
        .await
        .unwrap();
    let published: i64 = sqlx::query_scalar(
        "SELECT coalesce(sum(value), 0)::bigint FROM stats_series WHERE metric = 'torrents_published'",
    )
    .fetch_one(s.pool())
    .await
    .unwrap();
    assert_eq!(
        published, 6,
        "infohashes 1, 2, 3, 5, 6, 7; not the DHT-only 4"
    );
    drop(ix);
}

/// The fallback's `OR` typo arm is guarded by `origin = 'nostr'`: in the DHT branch of the
/// view it folds away, so `dht_torrents` is reached through its GIN index, never scanned.
#[tokio::test]
async fn fallback_reaches_dht_rows_through_the_index() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    for n in 1..=50 {
        dht_row(s, n, &format!("filler {n}"), &["a.bin"], "other").await;
    }
    put(s, torrent(1, 1, NOW, 100, "ubuntu desktop")).await;
    let mut conn = s.pool().acquire().await.unwrap();
    sqlx::query("SET enable_seqscan = off")
        .execute(&mut *conn)
        .await
        .unwrap();
    for text in ["ubuntu desktop", "ubu", "ubuntu"] {
        let plan = magnetar_server::search::explain_hits(&mut conn, &q(text, 20), NOW, 20)
            .await
            .unwrap()
            .join("\n");
        assert!(plan.contains("dht_torrents_search_tsv"), "{text}: {plan}");
        assert!(!plan.contains("Seq Scan on dht_torrents"), "{text}: {plan}");
    }
}

fn live(n: u64, seeders: Option<u32>, dht: Option<u32>) -> Liveness {
    Liveness {
        infohash: ih(n),
        seeders,
        leechers: None,
        completed: None,
        dht_peers_est: dht,
        source: LivenessSource::Udp,
        trackers_ok: 1,
        trackers_failed: 0,
        checked_at: NOW - 10,
        expires_at: NOW + 3600,
    }
}

fn keys_of(p: &SearchPage) -> Vec<String> {
    let mut k: Vec<String> = p.hits.iter().map(|h| h.infohash.clone()).collect();
    k.sort();
    k
}

/// D76: the empty query lists a DHT-only torrent only once a seed check found peers; Nostr
/// results, torrents known from both, text searches and the explicit DHT chip are unchanged.
/// Both search paths, any sort, the facets of the browse sheet, the index following a check.
#[tokio::test]
async fn empty_query_lists_dht_only_torrents_once_seeded() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    // 1: Nostr, never checked. 2: Nostr + DHT, never checked. 3: DHT only, never checked.
    // 4: DHT only, checked dead. 5: DHT only, tracker seeders 4. 6: DHT only, DHT peers 2.
    // 7: DHT only, never checked (checked later in the test).
    put(s, torrent(1, 1, NOW - 50, 1, "debian netinst")).await;
    put(s, torrent(2, 2, NOW - 40, 2, "debian live")).await;
    for (n, t) in [
        (2, "debian.live.dht"),
        (3, "debian.fresh.dht"),
        (4, "debian.dead.dht"),
        (5, "debian.seeded.dht"),
        (6, "debian.dhtpeers.dht"),
        (7, "debian.later.dht"),
    ] {
        dht_row(s, n, t, &[&format!("{t}/disc.iso")], "application").await;
    }
    s.put_liveness(&live(4, Some(0), Some(0))).await.unwrap();
    s.put_liveness(&live(5, Some(4), None)).await.unwrap();
    s.put_liveness(&live(6, None, Some(2))).await.unwrap();
    let ix = index_for(s).await;
    let pg = PgStore::from_pool(s.pool().clone());
    let set = |v: &[u64]| -> Vec<String> {
        let mut k: Vec<String> = v.iter().map(|n| ih(*n)).collect();
        k.sort();
        k
    };
    let browse = q("", 0);
    assert!(browse.browse_hides_unchecked_dht());
    for sort in [Sort::Relevance, Sort::Date, Sort::Size, Sort::Seeders] {
        let mut b = browse.clone();
        b.sort = sort;
        for (path, p) in [
            ("index", s.search(&b, NOW).await.unwrap()),
            ("sql", pg.search(&b, NOW).await.unwrap()),
        ] {
            assert_eq!(keys_of(&p), set(&[1, 2, 5, 6]), "{path} {sort:?}");
            assert_eq!(p.total, 4, "{path} {sort:?}");
        }
    }
    // Other filters keep the rule (still the empty query).
    let mut cat = browse.clone();
    cat.categories = vec![Category::Application];
    for p in [
        s.search(&cat, NOW).await.unwrap(),
        pg.search(&cat, NOW).await.unwrap(),
    ] {
        // (The fallback also matches 2 by its DHT row's category, a D73 difference.)
        let k = keys_of(&p);
        assert!(k.contains(&ih(5)) && k.contains(&ih(6)), "{k:?}");
        assert!([3, 4, 7].iter().all(|n| !k.contains(&ih(*n))), "{k:?}");
    }
    // Any text, and the explicit DHT chip, return every DHT result.
    for p in [
        s.search(&q("debian", 0), NOW).await.unwrap(),
        pg.search(&q("debian", 0), NOW).await.unwrap(),
    ] {
        assert_eq!(keys_of(&p), set(&[1, 2, 3, 4, 5, 6, 7]));
    }
    let mut chip = browse.clone();
    chip.origins = vec!["dht".into()];
    assert!(!chip.browse_hides_unchecked_dht());
    for p in [
        s.search(&chip, NOW).await.unwrap(),
        pg.search(&chip, NOW).await.unwrap(),
    ] {
        assert_eq!(keys_of(&p), set(&[2, 3, 4, 5, 6, 7]));
    }
    let mut both = browse.clone();
    both.origins = vec!["dht".into(), "nostr".into()];
    assert!(
        both.browse_hides_unchecked_dht(),
        "both chips = no source filter"
    );
    // The browse sheet's facets follow the list; Stats breakdowns count the whole catalogue.
    let facet = s.facets(&browse, "origin", 10, NOW).await.unwrap().unwrap();
    assert_eq!(facet.total, 4);
    let counts: BTreeMap<String, u64> = facet
        .counts
        .iter()
        .map(|c| (c.value.clone(), c.count))
        .collect();
    assert_eq!(
        counts,
        [("dht".to_string(), 3), ("nostr".to_string(), 2)]
            .into_iter()
            .collect()
    );
    let (total, _) = s
        .facet_tallies(&browse, &["origin".to_string()], NOW)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(total, 7, "breakdowns are not the browse list");
    // A seed check that finds peers brings a DHT-only torrent into the list.
    s.put_liveness(&live(7, None, Some(1))).await.unwrap();
    while search_index::apply_changes(s.pool(), &ix.idx)
        .await
        .unwrap()
        > 0
    {}
    for p in [
        s.search(&browse, NOW).await.unwrap(),
        pg.search(&browse, NOW).await.unwrap(),
    ] {
        assert_eq!(keys_of(&p), set(&[1, 2, 5, 6, 7]));
    }
    drop(ix);
}
