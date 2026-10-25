//! ApiClient against a fake HTTP server (ARCHITECTURE.md §6).

mod api_support;

use magnetar_core::Error;
use magnetar_core::api::*;
use magnetar_core::model::*;
use magnetar_core::query::{SearchPage, SearchQuery, Sort};

const IH: &str = "0123456789abcdef0123456789abcdef01234567";

fn pk() -> String {
    "ab".repeat(32)
}

fn summary() -> TorrentSummary {
    TorrentSummary {
        infohash: IH.into(),
        event_id: "cd".repeat(32),
        title: "Big Buck Bunny".into(),
        category: Category::Video,
        total_size: Some(276_134_947),
        file_count: 3,
        created_at: 1_790_000_000,
        pubkey: pk(),
        npub: npub(&pk()),
        seeders: Some(12),
        leechers: None,
        dead: Some(false),
        trust: 41,
        pow_bits: 21,
        publisher_count: 2,
        source_url: Some("https://nyaa.si/view/1".into()),
        rank: 0.5,
        pinned: false,
        hidden: false,
        media_title: Some("Big Buck Bunny".into()),
        media_year: Some(2008),
        media_kind: Some(MediaKind::Movie),
        matched_file: None,
    }
}

fn liveness() -> Liveness {
    Liveness {
        infohash: IH.into(),
        seeders: Some(12),
        leechers: Some(1),
        completed: None,
        dht_peers_est: None,
        source: LivenessSource::Udp,
        trackers_ok: 2,
        trackers_failed: 1,
        checked_at: 1_790_000_000,
        expires_at: 1_790_021_600,
    }
}

fn trust_score() -> TrustScore {
    TrustScore {
        pubkey: pk(),
        score: 21,
        factors: vec![TrustFactor {
            code: "FOLLOW_D2".into(),
            delta: 11,
            detail: "followed by 1 of your follows".into(),
            evidence: vec!["01".repeat(32)],
        }],
        root: Some("aa".repeat(32)),
        computed_at: 1_790_000_000,
        graph_version: 2,
    }
}

fn detail() -> TorrentDetail {
    TorrentDetail {
        summary: summary(),
        description: "desc".into(),
        files: vec![TorrentFile {
            path: "a/b.mkv".into(),
            size: Some(1),
        }],
        trackers: vec!["udp://tracker.opentrackr.org:1337/announce".into()],
        hashtags: vec!["movie".into()],
        category_path: vec!["video".into()],
        external_ids: vec![("imdb".into(), "tt1254207".into())],
        magnet: format!("magnet:?xt=urn:btih:{IH}"),
        liveness: Some(liveness()),
        trust: trust_score(),
        events: vec![EventRef {
            event_id: "cd".repeat(32),
            pubkey: pk(),
            npub: npub(&pk()),
            created_at: 1_790_000_000,
            title: "Big Buck Bunny".into(),
            trust: 41,
        }],
        comments: vec![],
        media: Some(MediaInfo {
            media_id: 7,
            kind: MediaKind::Movie,
            title: "Big Buck Bunny".into(),
            original_title: None,
            year: Some(2008),
            end_year: None,
            genres: vec!["Animation".into(), "Short".into()],
            rating: Some(6.4),
            votes: Some(1234),
            runtime_min: Some(10),
            season: None,
            episode: None,
            imdb: Some("tt1254207".into()),
            tmdb: Some((MediaKind::Movie, 10378)),
            mal: None,
            anilist: None,
            method: "itag".into(),
            confidence: 1.0,
            overview: None,
            poster: Some("/api/v1/media/7/poster".into()),
            episode_title: None,
        }),
    }
}

fn relay_info() -> RelayInfo {
    RelayInfo {
        url: "wss://nos.lol".into(),
        source: "seed".into(),
        enabled: false,
        last_ok: Some(1),
        last_err: None,
        consecutive_failures: 0,
        next_try_at: 0,
        events_found: 500,
        supports_neg: Some(false),
    }
}

fn saved() -> SavedSearch {
    SavedSearch {
        id: 7,
        name: "ubuntu".into(),
        query: SearchQuery {
            text: "ubuntu".into(),
            ..Default::default()
        },
        notify: true,
        last_checked: None,
        last_seen_created_at: None,
    }
}

fn download() -> DownloadInfo {
    DownloadInfo {
        id: 3,
        name: "bbb".into(),
        infohash: IH.into(),
        state: "live".into(),
        progress: 0.5,
        downloaded: 10,
        total: 20,
        down_bps: 1,
        up_bps: 2,
        peers: 4,
        error: None,
        output_dir: "/dl".into(),
    }
}

fn j<T: serde::Serialize>(v: &T) -> (u16, String) {
    (200, serde_json::to_string(v).unwrap())
}

/// The whole §6 surface with canned answers.
async fn server() -> api_support::FakeHttp {
    api_support::serve(|req| {
        let path = req.path.as_str();
        match (req.method.as_str(), path) {
            ("GET", "/health") => j(&Health {
                status: "ok".into(),
                version: "0.1.0".into(),
                db: true,
            }),
            ("POST", "/api/v1/search") => {
                let q: SearchQuery = serde_json::from_slice(&req.body).unwrap();
                j(&SearchPage {
                    hits: vec![summary()],
                    total: q.limit as u64,
                    source: "server".into(),
                    total_is_lower_bound: false,
                })
            }
            ("GET", p) if p == format!("/api/v1/torrents/{IH}") => j(&detail()),
            ("GET", p) if p.starts_with("/api/v1/torrents/") => (
                404,
                r#"{"error":"not_found","message":"no such torrent"}"#.into(),
            ),
            ("POST", p) if p == format!("/api/v1/torrents/{IH}/liveness") => j(&liveness()),
            ("GET", "/api/v1/stats") => j(&StatsResponse {
                store: StoreStats {
                    torrents: 5,
                    unsynced: 0,
                    ..Default::default()
                },
                crawler: CrawlerStatus {
                    running: true,
                    last_run: Some(9),
                    last_report: None,
                    ..Default::default()
                },
                search_index: None,
            }),
            ("GET", "/api/v1/relays") => j(&vec![relay_info()]),
            ("PUT", "/api/v1/relays") => {
                let u: RelayUpdate = serde_json::from_slice(&req.body).unwrap();
                let mut r = relay_info();
                r.url = u.url;
                r.enabled = u.enabled;
                j(&vec![r])
            }
            ("GET", p) if p == format!("/api/v1/trust/{}", pk()) => j(&trust_score()),
            ("GET", "/api/v1/settings/trust") => j(&TrustSettings {
                root: None,
                threshold: 20,
            }),
            ("PUT", "/api/v1/settings/trust") => {
                let mut s: TrustSettings = serde_json::from_slice(&req.body).unwrap();
                s.root = s.root.map(|r| parse_pubkey(&r).unwrap());
                j(&s)
            }
            ("PUT", p) if p.starts_with("/api/v1/overrides/") => (204, String::new()),
            ("GET", "/api/v1/saved-searches") => j(&vec![saved()]),
            ("POST", "/api/v1/saved-searches") => {
                let mut s: SavedSearch = serde_json::from_slice(&req.body).unwrap();
                s.id = 42;
                j(&s)
            }
            ("DELETE", "/api/v1/saved-searches/42") => (204, String::new()),
            ("GET", "/api/v1/downloads") => j(&vec![download()]),
            ("POST", "/api/v1/downloads") => j(&download()),
            ("POST", "/api/v1/downloads/3/pause") | ("POST", "/api/v1/downloads/3/resume") => {
                (204, String::new())
            }
            ("DELETE", "/api/v1/downloads/3") => (204, String::new()),
            ("POST", "/api/v1/events/batch") => {
                let b: BatchRequest = serde_json::from_slice(&req.body).unwrap();
                let results = b
                    .events
                    .iter()
                    .map(|e| BatchResult {
                        id: e.id.clone(),
                        accepted: true,
                        duplicate: false,
                        message: String::new(),
                    })
                    .collect();
                j(&BatchResponse { results })
            }
            _ => (
                404,
                format!(
                    r#"{{"error":"no_route","message":"{} {path}"}}"#,
                    req.method
                ),
            ),
        }
    })
    .await
}

fn client(base: &str) -> ApiClient {
    ApiClient::new(base, Some("t0k".into())).unwrap()
}

#[test]
fn relay_ws_url_variants() {
    let c = |base: &str, tok: Option<&str>| {
        ApiClient::new(base, tok.map(String::from))
            .unwrap()
            .relay_ws_url()
    };
    assert_eq!(
        c("http://127.0.0.1:8787", Some("abc")),
        "ws://127.0.0.1:8787/relay?token=abc"
    );
    assert_eq!(
        c("https://phone.tail1234.ts.net/", Some("a+b/c=")),
        "wss://phone.tail1234.ts.net/relay?token=a%2Bb%2Fc%3D"
    );
    assert_eq!(
        c("https://x.example/magnetar", None),
        "wss://x.example/magnetar/relay"
    );
    assert_eq!(
        c("http://localhost:8787", Some("")),
        "ws://localhost:8787/relay"
    );
}

#[tokio::test]
async fn read_endpoints() {
    let srv = server().await;
    let api = client(&format!("{}/", srv.base));
    assert_eq!(api.base_url, srv.base, "trailing slash trimmed");

    let h = api.health().await.unwrap();
    assert_eq!((h.status.as_str(), h.db), ("ok", true));

    let q = SearchQuery {
        text: "bunny".into(),
        sort: Sort::Seeders,
        limit: 25,
        ..Default::default()
    };
    let page = api.search(&q).await.unwrap();
    assert_eq!((page.total, page.hits[0].clone()), (25, summary()));

    assert_eq!(api.torrent(IH).await.unwrap(), Some(detail()));
    assert_eq!(api.torrent(&"f".repeat(40)).await.unwrap(), None);
    assert_eq!(api.refresh_liveness(IH).await.unwrap(), liveness());
    let st = api.stats().await.unwrap();
    assert_eq!((st.store.torrents, st.crawler.running), (5, true));
    assert_eq!(api.relays().await.unwrap(), vec![relay_info()]);
    assert_eq!(api.trust(&pk()).await.unwrap(), trust_score());
    assert_eq!(api.trust_settings().await.unwrap().threshold, 20);
    assert_eq!(api.saved_searches().await.unwrap(), vec![saved()]);
    assert_eq!(api.downloads().await.unwrap(), vec![download()]);

    let reqs = srv.requests();
    assert_eq!(reqs.len(), 11);
    for r in &reqs {
        assert_eq!(r.header("authorization"), Some("Bearer t0k"), "{}", r.path);
    }
    let search = reqs.iter().find(|r| r.path == "/api/v1/search").unwrap();
    assert_eq!(search.method, "POST");
    assert_eq!(search.json()["sort"], "seeders");
    assert_eq!(search.json()["text"], "bunny");
}

#[tokio::test]
async fn write_endpoints() {
    let srv = server().await;
    let api = client(&srv.base);

    let r = api
        .set_relay(&RelayUpdate {
            url: "wss://relay.damus.io".into(),
            enabled: true,
        })
        .await
        .unwrap();
    assert_eq!(
        (r[0].url.as_str(), r[0].enabled),
        ("wss://relay.damus.io", true)
    );

    let s = api
        .set_trust_settings(&TrustSettings {
            root: Some(npub(&pk())),
            threshold: 35,
        })
        .await
        .unwrap();
    assert_eq!((s.root, s.threshold), (Some(pk()), 35));

    api.set_override(&pk(), Some(OverrideAction::Block))
        .await
        .unwrap();
    api.set_override(&pk(), None).await.unwrap();

    let mut new = saved();
    new.id = 0;
    assert_eq!(api.put_saved_search(&new).await.unwrap().id, 42);
    api.delete_saved_search(42).await.unwrap();

    let d = api
        .add_download(&format!("magnet:?xt=urn:btih:{IH}"))
        .await
        .unwrap();
    assert_eq!(d.id, 3);
    for action in ["pause", "resume", "delete"] {
        api.download_action(3, action).await.unwrap();
    }
    assert!(matches!(
        api.download_action(3, "explode").await,
        Err(Error::Invalid(_))
    ));

    let reqs = srv.requests();
    let find = |m: &str, p: &str| {
        reqs.iter()
            .filter(|r| r.method == m && r.path == p)
            .cloned()
            .collect::<Vec<_>>()
    };
    let ov = find("PUT", &format!("/api/v1/overrides/{}", pk()));
    assert_eq!(ov.len(), 2);
    assert_eq!(ov[0].json(), serde_json::json!({"action": "block"}));
    assert_eq!(ov[1].json(), serde_json::json!({"action": null}));
    let dl = find("POST", "/api/v1/downloads");
    assert_eq!(dl[0].json()["magnet"], format!("magnet:?xt=urn:btih:{IH}"));
    assert_eq!(find("POST", "/api/v1/downloads/3/pause").len(), 1);
    assert_eq!(find("POST", "/api/v1/downloads/3/resume").len(), 1);
    assert_eq!(find("DELETE", "/api/v1/downloads/3").len(), 1);
    assert_eq!(find("DELETE", "/api/v1/saved-searches/42").len(), 1);
    assert_eq!(
        find("PUT", "/api/v1/settings/trust")[0].json()["root"],
        npub(&pk())
    );
}

#[tokio::test]
async fn batch_is_chunked() {
    let srv = server().await;
    let api = client(&srv.base);
    let events: Vec<RawEvent> = (0..1_101)
        .map(|i| RawEvent {
            id: format!("{i:064x}"),
            pubkey: pk(),
            created_at: i,
            kind: 2003,
            tags: vec![],
            content: String::new(),
            sig: "00".repeat(64),
        })
        .collect();
    let r = api.post_batch(&events).await.unwrap();
    assert_eq!(r.results.len(), 1_101);
    assert_eq!(r.results[1_100].id, format!("{:064x}", 1_100));
    let sizes: Vec<usize> = srv
        .requests()
        .iter()
        .map(|r| r.json()["events"].as_array().unwrap().len())
        .collect();
    assert_eq!(sizes, vec![500, 500, 101]);
    assert!(api.post_batch(&[]).await.unwrap().results.is_empty());
}

#[tokio::test]
async fn errors_map_to_server_error() {
    let srv = server().await;
    let api = client(&srv.base);
    // JSON error body → its message.
    match api.trust(&"cc".repeat(32)).await {
        Err(Error::Server { status, msg }) => {
            assert_eq!(status, 404);
            assert_eq!(msg, format!("GET /api/v1/trust/{}", "cc".repeat(32)));
        }
        other => panic!("{other:?}"),
    }
    // Plain-text body is passed through.
    let raw = api_support::serve(|_| (500, "plain text failure".into())).await;
    match ApiClient::new(&raw.base, None).unwrap().stats().await {
        Err(Error::Server { status, msg }) => {
            assert_eq!((status, msg.as_str()), (500, "plain text failure"));
        }
        other => panic!("{other:?}"),
    }
    // No token → no Authorization header.
    let _ = ApiClient::new(&raw.base, None).unwrap().health().await;
    assert!(
        raw.requests()
            .iter()
            .all(|r| r.header("authorization").is_none())
    );
    // Unreachable → Network.
    let dead = ApiClient::new("http://127.0.0.1:9", None).unwrap();
    assert!(matches!(dead.health().await, Err(Error::Network(_))));
}
