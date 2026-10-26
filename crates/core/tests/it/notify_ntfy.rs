//! ntfy publish against a fake server (never ntfy.sh) and saved-search polling.

use crate::api_support;
use crate::trust_support;

use magnetar_core::Error;
use magnetar_core::model::SavedSearch;
use magnetar_core::notify::{
    NtfyConfig, format_digest, format_hits, ntfy_publish, poll_saved_searches,
};
use magnetar_core::query::SearchQuery;
use serde_json::json;
use trust_support::{MemStore, summary};

fn cfg(base: &str, token: Option<&str>) -> NtfyConfig {
    NtfyConfig {
        base_url: base.into(),
        topic: "magnetar-k3j4h5g6f7d8s9a0".into(),
        token: token.map(String::from),
    }
}

#[tokio::test]
async fn publishes_json_to_root_url() {
    let srv = api_support::serve(|_| (200, r#"{"id":"x","event":"message"}"#.into())).await;
    let title = "Ubuntu: 2 new results — Été";
    ntfy_publish(
        &cfg(&format!("{}/", srv.base), Some("tk_abc")),
        title,
        "• one\n• two",
        Some("magnetar://torrent/0123"),
    )
    .await
    .unwrap();
    ntfy_publish(&cfg(&srv.base, None), "t", "m", None)
        .await
        .unwrap();

    let reqs = srv.requests();
    assert_eq!(reqs.len(), 2);
    let r = &reqs[0];
    assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/"));
    assert_eq!(r.header("authorization"), Some("Bearer tk_abc"));
    assert_eq!(r.header("x-firebase"), Some("no"));
    assert!(
        r.header("content-type")
            .unwrap()
            .starts_with("application/json")
    );
    assert_eq!(
        r.json(),
        json!({
            "topic": "magnetar-k3j4h5g6f7d8s9a0",
            "title": title,
            "message": "• one\n• two",
            "click": "magnetar://torrent/0123",
            "tags": ["magnet"],
            "priority": 3,
        })
    );
    assert_eq!(reqs[1].header("authorization"), None);
    assert!(reqs[1].json().get("click").is_none());
}

#[tokio::test]
async fn long_message_truncated_on_char_boundary() {
    let srv = api_support::serve(|_| (200, "{}".into())).await;
    let msg = "é".repeat(3000); // 6000 bytes
    ntfy_publish(&cfg(&srv.base, None), "t", &msg, None)
        .await
        .unwrap();
    let sent = srv.requests()[0].json()["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(sent.len(), 4096);
    assert!(msg.starts_with(&sent));
}

#[tokio::test]
async fn non_2xx_is_error() {
    let srv = api_support::serve(|_| {
        (
            429,
            r#"{"code":42901,"http":429,"error":"limit reached"}"#.into(),
        )
    })
    .await;
    match ntfy_publish(&cfg(&srv.base, None), "t", "m", None).await {
        Err(Error::Server { status, msg }) => {
            assert_eq!(status, 429);
            assert!(msg.contains("limit reached"));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        ntfy_publish(&cfg("", None), "t", "m", None).await,
        Err(Error::Invalid(_))
    ));
}

fn saved(id: i64, text: &str, notify: bool) -> SavedSearch {
    SavedSearch {
        id,
        name: text.into(),
        query: SearchQuery {
            text: text.into(),
            ..Default::default()
        },
        notify,
        last_checked: None,
        last_seen_created_at: None,
    }
}

#[tokio::test]
async fn poll_watermarks() {
    let store = MemStore::default();
    store.with(|i| {
        i.saved = vec![
            saved(1, "ubuntu", true),
            saved(2, "ubuntu", false),
            saved(3, "nothing-yet", true),
        ];
        i.torrents = vec![
            summary(&"a".repeat(40), "Ubuntu 24.04", 1_000),
            summary(&"b".repeat(40), "Ubuntu 25.10", 2_000),
            summary(&"c".repeat(40), "Debian 13", 3_000),
        ];
    });

    // First run: watermark only.
    assert!(
        poll_saved_searches(&store, 10_000)
            .await
            .unwrap()
            .is_empty()
    );
    let s = store.with(|i| i.saved.clone());
    assert_eq!(s[0].last_seen_created_at, Some(2_000));
    assert_eq!(s[0].last_checked, Some(10_000));
    assert_eq!(s[1].last_seen_created_at, None, "notify=false is skipped");
    assert_eq!(s[2].last_seen_created_at, Some(0), "no match yet → 0");

    // Nothing new.
    assert!(
        poll_saved_searches(&store, 11_000)
            .await
            .unwrap()
            .is_empty()
    );

    store.with(|i| {
        i.torrents
            .push(summary(&"d".repeat(40), "Ubuntu 26.04", 4_000));
        i.torrents
            .push(summary(&"e".repeat(40), "Ubuntu old reupload", 2_000));
        i.torrents
            .push(summary(&"f".repeat(40), "nothing-yet arrives", 500));
    });
    let hits = poll_saved_searches(&store, 12_000).await.unwrap();
    assert_eq!(hits.len(), 2);
    let (s1, h1) = &hits[0];
    assert_eq!(s1.id, 1);
    assert_eq!(s1.last_seen_created_at, Some(4_000));
    assert_eq!(
        h1.iter().map(|h| h.title.as_str()).collect::<Vec<_>>(),
        ["Ubuntu 26.04"]
    );
    let (s3, h3) = &hits[1];
    assert_eq!((s3.id, h3.len()), (3, 1));
    assert_eq!(s3.last_seen_created_at, Some(500));
    assert_eq!(store.with(|i| i.saved[0].last_seen_created_at), Some(4_000));
    assert!(
        poll_saved_searches(&store, 13_000)
            .await
            .unwrap()
            .is_empty()
    );
}

#[test]
fn hit_formatting() {
    let s = saved(1, "ubuntu", true);
    let one = vec![summary(&"a".repeat(40), "Ubuntu 24.04", 1)];
    let (title, msg, click) = format_hits(&s, &one);
    assert_eq!(title, "ubuntu: 1 new result");
    assert_eq!(msg, "• Ubuntu 24.04");
    assert_eq!(
        click,
        Some(format!("magnetar://torrent/{}", "a".repeat(40)))
    );

    let many: Vec<_> = (0..12)
        .map(|i| summary(&format!("{i:040x}"), &format!("t{i}"), i))
        .collect();
    let (title, msg, click) = format_hits(&s, &many);
    assert_eq!(title, "ubuntu: 12 new results");
    assert_eq!(msg.lines().count(), 11);
    assert!(msg.ends_with("… and 2 more"));
    assert_eq!(click, None);
}

#[test]
fn digest_is_one_message_per_poll() {
    let a = saved(1, "ubuntu", true);
    let b = saved(2, "debian", true);
    let c = saved(3, "arch", true);
    let hit = |h: &str, t: &str| summary(&h.repeat(40), t, 1);
    assert_eq!(format_digest(&[]), None);
    assert_eq!(format_digest(&[(a.clone(), vec![])]), None);

    // A single matching search reads like `format_hits`, empty ones are left out.
    let one = vec![hit("a", "Ubuntu 24.04")];
    assert_eq!(
        format_digest(&[(a.clone(), one.clone()), (b.clone(), vec![])]),
        Some(format_hits(&a, &one))
    );

    let (title, msg, click) = format_digest(&[
        (a, one),
        (b, vec![hit("b", "Debian 13"), hit("c", "Debian 12")]),
        (c, vec![]),
    ])
    .unwrap();
    assert_eq!(title, "3 new results in 2 saved searches");
    assert_eq!(msg, "• ubuntu: 1\n• debian: 2");
    assert_eq!(click, None);
}
