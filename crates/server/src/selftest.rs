//! `magnetar-server selftest`: the checks that tend to break on a phone (termux.md §2).

use std::time::Duration;

use magnetar_core::crawler::DEFAULT_SEED_RELAYS;
use magnetar_core::magnet::DEFAULT_TRACKERS;
use tokio::net::UdpSocket;

use crate::config::Config;
use crate::store::PgStore;

const TIMEOUT: Duration = Duration::from_secs(10);
/// BEP 15 connect magic.
const UDP_PROTOCOL_ID: u64 = 0x0417_2710_1980;

fn report(name: &str, res: &anyhow::Result<String>) -> bool {
    match res {
        Ok(detail) => println!("ok    {name}: {detail}"),
        Err(e) => println!("FAIL  {name}: {e:#}"),
    }
    res.is_ok()
}

async fn check_db(cfg: &Config) -> anyhow::Result<String> {
    let store = PgStore::connect(cfg.database_url()?).await?;
    let version: String = sqlx::query_scalar("SHOW server_version")
        .fetch_one(store.pool())
        .await?;
    let trgm: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_available_extensions WHERE name = 'pg_trgm')",
    )
    .fetch_one(store.pool())
    .await?;
    anyhow::ensure!(trgm, "PostgreSQL {version} has no pg_trgm extension");
    Ok(format!("PostgreSQL {version}, pg_trgm available"))
}

fn host_of(url: &str) -> anyhow::Result<String> {
    let u = url::Url::parse(url)?;
    Ok(u.host_str()
        .ok_or_else(|| anyhow::anyhow!("no host in {url}"))?
        .to_string())
}

async fn check_dns(host: &str) -> anyhow::Result<String> {
    let addrs: Vec<_> = tokio::time::timeout(TIMEOUT, tokio::net::lookup_host((host, 443)))
        .await??
        .collect();
    anyhow::ensure!(!addrs.is_empty(), "no addresses for {host}");
    Ok(format!("{host} -> {}", addrs[0].ip()))
}

/// NIP-11 document over HTTPS (rustls + ring + webpki roots, D21).
async fn check_tls(relay: &str) -> anyhow::Result<String> {
    let https = relay.replacen("wss://", "https://", 1).replacen("ws://", "http://", 1);
    let client = magnetar_core::http::client(TIMEOUT)?;
    let resp = client
        .get(&https)
        .header("Accept", "application/nostr+json")
        .send()
        .await?
        .error_for_status()?;
    let doc: serde_json::Value = resp.json().await?;
    let name = doc.get("name").and_then(|v| v.as_str()).unwrap_or("?");
    let software = doc.get("software").and_then(|v| v.as_str()).unwrap_or("?");
    let nips = doc
        .get("supported_nips")
        .and_then(|v| v.as_array())
        .map_or(0, Vec::len);
    Ok(format!("{https}: name={name:?} software={software:?} nips={nips}"))
}

/// BEP 15 connect round trip.
async fn check_udp(tracker: &str) -> anyhow::Result<String> {
    let u = url::Url::parse(tracker)?;
    let host = u.host_str().ok_or_else(|| anyhow::anyhow!("no host"))?;
    let port = u.port().ok_or_else(|| anyhow::anyhow!("no port"))?;
    let addr = tokio::net::lookup_host((host, port))
        .await?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| anyhow::anyhow!("no IPv4 address for {host}"))?;
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    let tid: u32 = rand::random();
    let mut req = [0u8; 16];
    req[..8].copy_from_slice(&UDP_PROTOCOL_ID.to_be_bytes());
    req[12..].copy_from_slice(&tid.to_be_bytes());
    let mut buf = [0u8; 64];
    for attempt in 1..=2 {
        sock.send_to(&req, addr).await?;
        match tokio::time::timeout(Duration::from_secs(5), sock.recv_from(&mut buf)).await {
            Ok(Ok((n, _))) if n >= 16 && buf[..4] == [0; 4] && buf[4..8] == tid.to_be_bytes() => {
                return Ok(format!("{tracker} ({addr}) answered connect, attempt {attempt}"));
            }
            Ok(Ok((n, _))) => anyhow::bail!("unexpected {n}-byte reply from {addr}"),
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => continue,
        }
    }
    anyhow::bail!("no answer from {tracker} ({addr}) in 10 s")
}

/// Run every check; Err if any failed.
pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let seeds = cfg.seed_relays();
    let relay = seeds
        .first()
        .map(String::as_str)
        .unwrap_or(DEFAULT_SEED_RELAYS[2]);
    let mut ok = report("database", &check_db(cfg).await);
    let host = host_of(relay);
    ok &= match host {
        Ok(h) => report("dns", &check_dns(&h).await),
        Err(e) => report("dns", &Err(e)),
    };
    ok &= report("tls (NIP-11)", &check_tls(relay).await);
    ok &= report("udp tracker", &check_udp(DEFAULT_TRACKERS[0]).await);
    anyhow::ensure!(ok, "selftest failed");
    println!("selftest passed");
    Ok(())
}
