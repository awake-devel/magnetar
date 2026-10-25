//! The binary itself: CLI, serve lifecycle, and the network-bound commands.

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::*;
use magnetar_core::query::SearchQuery;
use magnetar_core::store::Store;

const BIN: &str = env!("CARGO_BIN_EXE_magnetar-server");

#[test]
fn version_and_help() {
    let out = Command::new(BIN).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("magnetar-server {}", magnetar_core::VERSION)
    );
    let out = Command::new(BIN).arg("--help").output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    for cmd in ["serve", "migrate", "crawl-once", "selftest"] {
        assert!(help.contains(cmd), "{help}");
    }
}

#[test]
fn serve_requires_token() {
    let out = Command::new(BIN)
        .arg("serve")
        .env_remove("MAGNETAR_API_TOKEN")
        .env("DATABASE_URL", "postgres://nobody@127.0.0.1:1/none")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("MAGNETAR_API_TOKEN"));
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// migrate, then serve: /health answers, auth is enforced, SIGTERM exits cleanly.
#[tokio::test]
async fn migrate_serve_and_graceful_shutdown() {
    let Some(db) = test_db().await else { return };
    let url = db.scoped_url();
    let out = Command::new(BIN)
        .arg("migrate")
        .env("DATABASE_URL", &url)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let port = free_port();
    let state = std::env::temp_dir().join(format!("magnetar-serve-{port}"));
    let mut child = Command::new(BIN)
        .arg("serve")
        .env("DATABASE_URL", &url)
        .env("MAGNETAR_API_TOKEN", TOKEN)
        .env("MAGNETAR_BIND", format!("127.0.0.1:{port}"))
        .env("MAGNETAR_DOWNLOADS", "false")
        // Unroutable seed: the crawler loop must not reach the network from tests.
        .env("MAGNETAR_SEED_RELAYS", "ws://127.0.0.1:1")
        .env("MAGNETAR_CRAWL_INTERVAL_SECS", "3600")
        .env("XDG_STATE_HOME", &state)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let http = magnetar_core::http::client(Duration::from_secs(5)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(r) = http.get(format!("{base}/health")).send().await
            && r.status().is_success()
        {
            break;
        }
        assert!(Instant::now() < deadline, "server never became healthy");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let r = http
        .get(format!("{base}/api/v1/stats"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);
    let r = http
        .get(format!("{base}/api/v1/stats"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());

    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(
            Instant::now() < deadline,
            "serve did not exit after SIGTERM"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(status.success(), "exit status {status:?}");
    let _ = std::fs::remove_dir_all(state);
}

/// Real relays. Run with `--ignored` after merge (needs core::crawler + nip35 + ingest).
#[tokio::test]
#[ignore = "post-merge"]
async fn crawl_once_end_to_end() {
    let Some(db) = test_db().await else { return };
    let out = Command::new(BIN)
        .args(["crawl-once", "--seconds", "60"])
        .env("DATABASE_URL", db.scoped_url())
        .env(
            "MAGNETAR_SEED_RELAYS",
            "wss://nos.lol,wss://relay.primal.net",
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: magnetar_core::crawler::CrawlReport = serde_json::from_slice(&out.stdout).unwrap();
    eprintln!("{report:?}");
    assert!(report.inserted > 0);
    let st = db.store.stats().await.unwrap();
    assert!(st.torrents > 0 && st.relays > 0);
    let page = db
        .store
        .search(
            &SearchQuery {
                min_trust: Some(0),
                ..Default::default()
            },
            magnetar_core::now(),
        )
        .await
        .unwrap();
    assert!(page.total > 0);
}

/// DB + DNS + TLS NIP-11 + UDP tracker against the real network.
#[test]
#[ignore = "network"]
fn selftest_against_network() {
    let Ok(url) = std::env::var("MAGNETAR_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED (no MAGNETAR_TEST_DATABASE_URL)");
        return;
    };
    let out = Command::new(BIN)
        .arg("selftest")
        .env("DATABASE_URL", url)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    eprintln!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success());
    assert!(stdout.contains("selftest passed"));
}
