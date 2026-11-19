//! D150: the process destination budget as the chokepoints see it. A separate test binary,
//! because the budget is process-wide: installing a tiny one here cannot touch other tests.
//! No network traffic: only IP literals and the loopback resolver are used.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use magnetar_core::Error;
use magnetar_core::netaddr::AddrPolicy;
use magnetar_core::netbudget::{self, BudgetConfig, DestBudget};

#[tokio::test]
async fn chokepoints_go_through_the_installed_budget() {
    let mut cfg = BudgetConfig::new(2, 1000.0);
    cfg.max_wait = Duration::from_millis(200);
    let b = Arc::new(DestBudget::new(cfg));
    assert!(netbudget::install(b.clone()));
    assert!(!netbudget::install(b.clone()), "installed only once");

    // AddrPolicy::resolve (Nostr checked path, UDP trackers, tracker HTTP) admits literals.
    let policy = AddrPolicy::default();
    policy
        .resolve("tracker", "8.8.8.8", 6969, Duration::from_secs(1))
        .await
        .unwrap();
    // admit_host (trusted relay URLs) admits too, brackets and all.
    netbudget::admit_host("relay", "[2606:4700::1111]", 443, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(b.stats().dests_now, 2);
    // A known destination is still free.
    policy
        .resolve("tracker", "8.8.8.8", 80, Duration::from_secs(1))
        .await
        .unwrap();
    // A third public destination is refused once the short max_wait is over.
    let e = policy
        .resolve("tracker", "9.9.9.9", 6969, Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Budget(_)), "{e:?}");
    // The name branch: `localhost` resolves, passes the policy, and is not counted.
    AddrPolicy::loopback_for_tests()
        .resolve("local", "localhost", 8787, Duration::from_secs(2))
        .await
        .unwrap();
    // The shared HTTP client's resolver admits too: a public literal through it is counted
    // (8.8.8.8 is known: free) and an unknown one is refused, nothing resolved over the network.
    use reqwest::dns::Resolve;
    let r = netbudget::BudgetResolver;
    let before = b.stats().admitted;
    let got: Vec<_> = r
        .resolve("8.8.8.8".parse().unwrap())
        .await
        .unwrap()
        .collect();
    assert_eq!(got.len(), 1);
    assert_eq!(b.stats().admitted, before + 1);
    assert!(r.resolve("1.0.0.1".parse().unwrap()).await.is_err());
    // Loopback is never counted: the shared HTTP client still reaches local services.
    netbudget::admit_host("local", "localhost", 8787, Duration::from_secs(2))
        .await
        .unwrap();
    let lo: IpAddr = "127.0.0.1".parse().unwrap();
    netbudget::admit("local", lo).await.unwrap();
    // The health brake halves the destinations: level 1 → 1 destination; known ones pass.
    b.set_level(1);
    assert_eq!(b.limits().0, 1);
    assert_eq!(b.stats().refused, 2);
}

#[tokio::test]
async fn the_shared_http_client_admits_on_resolve() {
    // A loopback server: the client's resolver runs (admits, not counted) and the request works.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = s.read(&mut buf).await;
        let _ = s
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
            .await;
    });
    let c = magnetar_core::http::client(Duration::from_secs(5)).unwrap();
    let r = c
        .get(format!("http://localhost:{port}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.text().await.unwrap(), "ok");
}
