//! The uniffi-facing `Magnetar` object, exercised the way Kotlin does (no ambient tokio runtime).

mod common;

use std::sync::Arc;

use common::*;
use futures::executor::block_on;
use magnetar_core::store::keys;
use magnetar_ffi::*;

/// fiatjaf's key, a known valid secp256k1 x-only pubkey.
const HEX: &str = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
const NPUB: &str = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6";

fn config() -> FfiConfig {
    FfiConfig {
        server_url: None,
        api_token: None,
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
        min_trust: None,
        sort: FfiSort::Relevance,
        descending: true,
        limit: 0,
        offset: 0,
        include_hidden: false,
    }
}

fn open(dir: &TempDir, cfg: FfiConfig) -> Arc<Magnetar> {
    Magnetar::open(
        dir.0.join("db").join("magnetar.db").display().to_string(),
        cfg,
    )
    .unwrap()
}

#[test]
fn bech32_helpers() {
    assert_eq!(npub_encode(HEX.into()).unwrap(), NPUB);
    assert_eq!(npub_encode(HEX.to_uppercase()).unwrap(), NPUB);
    assert_eq!(pubkey_decode(NPUB.into()).unwrap(), HEX);
    assert_eq!(pubkey_decode(format!("nostr:{NPUB}")).unwrap(), HEX);
    assert_eq!(pubkey_decode(format!("  {HEX} ")).unwrap(), HEX);
    assert!(matches!(
        npub_encode("zz".into()),
        Err(FfiError::Invalid { .. })
    ));
    assert!(matches!(
        pubkey_decode("npub1nope".into()),
        Err(FfiError::Invalid { .. })
    ));
    assert_eq!(version(), magnetar_core::VERSION);
}

#[test]
fn open_validates_and_persists_config() {
    let dir = TempDir::new("cfg");
    let mut cfg = config();
    cfg.root_pubkey = Some(NPUB.into());
    cfg.trust_threshold = 15;
    cfg.seed_relays = vec!["wss://nos.lol".into()];
    let m = open(&dir, cfg.clone());
    let got = m.config();
    assert_eq!(got.root_pubkey.as_deref(), Some(NPUB));
    assert_eq!(got.trust_threshold, 15);
    let st = m.store();
    assert_eq!(
        st.get_setting_sync(keys::TRUST_ROOT).unwrap().unwrap(),
        format!("\"{HEX}\"")
    );
    assert_eq!(
        st.get_setting_sync(keys::TRUST_THRESHOLD)
            .unwrap()
            .as_deref(),
        Some("15")
    );
    assert_eq!(
        st.get_setting_sync(keys::SEED_RELAYS).unwrap().as_deref(),
        Some("[\"wss://nos.lol\"]")
    );
    assert!(st.get_setting_sync("ffi.config").unwrap().is_some());

    let mut bad = cfg.clone();
    bad.root_pubkey = Some("not a key".into());
    assert!(matches!(m.set_config(bad), Err(FfiError::Invalid { .. })));
    assert_eq!(
        m.config().trust_threshold,
        15,
        "rejected config not applied"
    );

    // Clearing the root stores JSON null and bumps the graph version.
    let v = st.graph_version_sync().unwrap();
    let mut none = cfg;
    none.root_pubkey = Some("  ".into());
    m.set_config(none).unwrap();
    assert_eq!(
        st.get_setting_sync(keys::TRUST_ROOT).unwrap().as_deref(),
        Some("null")
    );
    assert_eq!(st.graph_version_sync().unwrap(), v + 1);
}

#[test]
fn search_runs_without_ambient_runtime_and_uses_config_threshold() {
    let dir = TempDir::new("search");
    let m = open(&dir, config());
    torrent(&ih(1), "Big.Buck.Bunny.2008.1080p.x264-GRP", &pk(1), 100).put(m.store());

    // Kotlin polls futures from its own threads: no tokio context here.
    let page = block_on(m.search(query("bunny 1080p"), true)).unwrap();
    assert_eq!(page.source, "local");
    assert_eq!(
        page.server_error, None,
        "no server configured is not an error"
    );
    assert_eq!(page.total, 1);
    let hit = &page.hits[0];
    assert_eq!(hit.category, FfiCategory::Video);
    assert!(hit.npub.starts_with("npub1"));

    // min_trust None -> config threshold (unscored publisher has trust 0).
    let mut cfg = config();
    cfg.trust_threshold = 20;
    m.set_config(cfg).unwrap();
    assert_eq!(block_on(m.search(query("bunny"), false)).unwrap().total, 0);
    let mut q = query("bunny");
    q.min_trust = Some(0);
    q.categories = vec![FfiCategory::Audio];
    assert_eq!(block_on(m.search(q.clone(), false)).unwrap().total, 0);
    q.categories = vec![FfiCategory::Video];
    assert_eq!(block_on(m.search(q, false)).unwrap().total, 1);
}

#[tokio::test]
async fn object_works_and_drops_inside_a_tokio_context() {
    let dir = TempDir::new("tokio");
    let m = open(&dir, config());
    torrent(&ih(1), "inside tokio", &pk(1), 1).put(m.store());
    assert_eq!(m.search(query("tokio"), false).await.unwrap().total, 1);
    drop(m); // must not panic ("Cannot drop a runtime in a context where blocking is not allowed")
}

#[test]
fn flags_saved_searches_relays_stats() {
    let dir = TempDir::new("misc");
    let m = open(&dir, config());
    let t = torrent(&ih(0xab), "flagged", &pk(1), 5);
    t.put(m.store());

    m.set_hidden(ih(0xab).to_uppercase(), true).unwrap();
    assert_eq!(block_on(m.search(query(""), false)).unwrap().total, 0);
    let mut q = query("");
    q.include_hidden = true;
    let hit = &block_on(m.search(q, false)).unwrap().hits[0];
    assert!(hit.hidden && !hit.pinned);
    m.set_pinned(ih(0xab), true).unwrap();
    assert!(matches!(
        m.set_pinned("nope".into(), true),
        Err(FfiError::Invalid { .. })
    ));

    let id = m
        .save_search(FfiSavedSearch {
            id: 0,
            name: "bunnies".into(),
            query: query("bunny"),
            notify: true,
            last_checked: None,
        })
        .unwrap();
    // The poll watermark is not part of FfiSavedSearch; editing must keep it.
    let mut s = m.store().get_saved_search(id).unwrap().unwrap();
    s.last_seen_created_at = Some(42);
    m.store().put_saved_search_sync(&s).unwrap();
    let mut edit = m.saved_searches().unwrap().remove(0);
    assert_eq!(edit.name, "bunnies");
    edit.name = "rabbits".into();
    assert_eq!(m.save_search(edit).unwrap(), id);
    let s = m.store().get_saved_search(id).unwrap().unwrap();
    assert_eq!(
        (s.name.as_str(), s.last_seen_created_at),
        ("rabbits", Some(42))
    );
    m.delete_saved_search(id).unwrap();
    assert!(m.saved_searches().unwrap().is_empty());

    m.set_relay_enabled("wss://relay.example".into(), false)
        .unwrap();
    let relays = m.relays().unwrap();
    assert_eq!(relays.len(), 1);
    assert!(!relays[0].enabled);
    assert_eq!(relays[0].source, "manual");
    assert!(m.set_relay_enabled(" ".into(), true).is_err());

    let st = m.stats().unwrap();
    assert_eq!((st.torrents, st.unsynced, st.relays), (1, 1, 1));
    assert!(st.db_bytes > 0);
    assert_eq!((st.last_crawl, st.last_sync), (None, None));

    let c = m.cleanup().unwrap();
    assert_eq!((c.evicted, c.unsynced_kept), (0, 1), "cap 0 = no eviction");
}

#[test]
fn offline_server_paths() {
    let dir = TempDir::new("offline");
    let m = open(&dir, config());
    let r = block_on(m.sync_now()).unwrap();
    assert_eq!(r.method, "none");
    assert!(!block_on(m.server_health()).unwrap());
    assert!(matches!(
        block_on(m.download_start("magnet:?xt=urn:btih:00".into(), true)),
        Err(FfiError::Invalid { .. })
    ));
    assert!(matches!(
        block_on(m.downloads(true)),
        Err(FfiError::Invalid { .. })
    ));
    assert!(matches!(
        block_on(m.download_action(1, "explode".into(), false)),
        Err(FfiError::Invalid { .. })
    ));
    // Phone engine without configured dirs reports Invalid (app falls back to ACTION_VIEW).
    assert!(matches!(
        block_on(m.download_start("magnet:?xt=urn:btih:00".into(), false)),
        Err(FfiError::Invalid { .. })
    ));
    assert!(matches!(
        block_on(m.check_liveness("xyz".into())),
        Err(FfiError::Invalid { .. })
    ));
    block_on(m.pause_all_downloads());
}

#[test]
fn block_publisher_is_local_first() {
    let dir = TempDir::new("block");
    let m = open(&dir, config());
    torrent(&ih(1), "spam", HEX, 1).put(m.store());
    block_on(m.block_publisher(NPUB.into(), true)).unwrap();
    assert_eq!(block_on(m.search(query(""), false)).unwrap().total, 0);
    block_on(m.block_publisher(HEX.into(), false)).unwrap();
    assert_eq!(block_on(m.search(query(""), false)).unwrap().total, 1);
    assert!(block_on(m.block_publisher("garbage".into(), true)).is_err());
}

// ---- post-merge / network

#[test]
#[ignore = "post-merge"]
fn server_failure_falls_back_to_local() {
    let dir = TempDir::new("fallback");
    let mut cfg = config();
    cfg.server_url = Some("http://127.0.0.1:9".into());
    cfg.api_token = Some("x".into());
    let m = open(&dir, cfg);
    torrent(&ih(1), "local only", &pk(1), 1).put(m.store());
    let page = block_on(m.search(query("local"), true)).unwrap();
    assert_eq!(page.source, "local");
    assert!(page.server_error.is_some());
    assert_eq!(page.total, 1);
    let d = block_on(m.detail(ih(1), true)).unwrap().unwrap();
    assert_eq!(d.source, "local");
    assert!(!block_on(m.server_health()).unwrap());
    // Server unreachable: saved searches poll locally (first run records the watermark).
    m.save_search(FfiSavedSearch {
        id: 0,
        name: "l".into(),
        query: query("local"),
        notify: true,
        last_checked: None,
    })
    .unwrap();
    assert!(block_on(m.poll_saved_searches()).unwrap().is_empty());
    torrent(&ih(2), "local newer", &pk(1), 10).put(m.store());
    let hits = block_on(m.poll_saved_searches()).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].new_hits[0].title, "local newer");
}

/// Real relays, phone budget: run with `cargo test -p magnetar-ffi -- --ignored crawl_once_live`.
#[test]
#[ignore = "post-merge"]
fn crawl_once_live() {
    let dir = TempDir::new("crawl");
    let m = open(&dir, config());
    let r = block_on(m.crawl_once(45)).unwrap();
    eprintln!("{r:?}");
    assert!(r.relays_tried > 0);
    assert!(r.relays_ok > 0, "{:?}", r.errors);
    assert!(r.inserted > 0);
    let st = m.stats().unwrap();
    assert!(st.torrents > 0 && st.last_crawl.is_some());
    assert_eq!(st.unsynced, st.events.min(st.unsynced));
    let page = block_on(m.search(query(""), false)).unwrap();
    assert!(page.total > 0);
}

/// Needs a running magnetar-server: MAGNETAR_TEST_SERVER_URL (+ MAGNETAR_TEST_SERVER_TOKEN).
#[test]
#[ignore = "post-merge"]
fn sync_now_against_server() {
    let Ok(url) = std::env::var("MAGNETAR_TEST_SERVER_URL") else {
        eprintln!("SKIPPED (no MAGNETAR_TEST_SERVER_URL)");
        return;
    };
    let dir = TempDir::new("sync");
    let mut cfg = config();
    cfg.server_url = Some(url);
    cfg.api_token = std::env::var("MAGNETAR_TEST_SERVER_TOKEN").ok();
    let m = open(&dir, cfg);
    assert!(block_on(m.server_health()).unwrap());
    // Fixture events are real signed events the server will accept.
    let fixtures = format!(
        "{}/../../docs/research/fixtures/kind2003_sample.jsonl",
        env!("CARGO_MANIFEST_DIR")
    );
    let rt = tokio::runtime::Runtime::new().unwrap();
    for line in std::fs::read_to_string(fixtures).unwrap().lines().take(10) {
        let raw = magnetar_core::model::RawEvent::from_json(line).unwrap();
        rt.block_on(magnetar_core::ingest::ingest_into(
            &**m.store(),
            raw,
            magnetar_core::now(),
        ))
        .unwrap();
    }
    let before = m.stats().unwrap().unsynced;
    assert!(before > 0);
    let r = block_on(m.sync_now()).unwrap();
    eprintln!("{r:?}");
    assert_eq!(r.error, None);
    assert_eq!(r.uploaded + r.duplicates, before);
    assert_eq!(m.stats().unwrap().unsynced, 0);
    assert!(m.stats().unwrap().last_sync.is_some());
    // Second run has nothing to send.
    let r2 = block_on(m.sync_now()).unwrap();
    assert_eq!(r2.uploaded, 0);
}
