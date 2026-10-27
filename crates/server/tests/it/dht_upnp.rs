//! UPnP port mapping (D57, D72) against an in-process fake Internet Gateway Device on
//! 127.0.0.1: SSDP answer, device and service descriptions, SOAP control. No router is
//! contacted (unicast discovery to the fake only, multicast off).

#![cfg(feature = "upnp")]

use crate::common;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use magnetar_server::dht::upnp::{self, Protocol, UpnpConfig};
use tokio::net::UdpSocket;

#[derive(Default)]
struct Igd {
    /// (protocol, external port) → (internal client, internal port, lease, description).
    mappings: BTreeMap<(String, u16), (String, u16, u32, String)>,
    deleted: Vec<(String, u16)>,
    searches: u32,
    only_permanent: bool,
    in_use: Option<u16>,
    /// Every AddPortMapping fails (501): a router that went bad between renewals.
    fail_add: bool,
}

type Shared = Arc<Mutex<Igd>>;

fn tag<'a>(body: &'a str, name: &str) -> &'a str {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    body.find(&open)
        .and_then(|i| {
            let rest = &body[i + open.len()..];
            rest.find(&close).map(|j| &rest[..j])
        })
        .unwrap_or("")
}

fn envelope(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body>{inner}</s:Body></s:Envelope>"#
    )
}

fn fault(code: u16, desc: &str) -> String {
    envelope(&format!(
        r#"<s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0"><errorCode>{code}</errorCode><errorDescription>{desc}</errorDescription></UPnPError></detail></s:Fault>"#
    ))
}

const SERVICE: &str = "urn:schemas-upnp-org:service:WANIPConnection:1";

async fn root_desc() -> &'static str {
    r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0"><specVersion><major>1</major><minor>0</minor></specVersion>
<device><deviceType>urn:schemas-upnp-org:device:InternetGatewayDevice:1</deviceType>
<friendlyName>fake IGD</friendlyName>
<deviceList><device><deviceType>urn:schemas-upnp-org:device:WANDevice:1</deviceType>
<deviceList><device><deviceType>urn:schemas-upnp-org:device:WANConnectionDevice:1</deviceType>
<serviceList><service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
<serviceId>urn:upnp-org:serviceId:WANIPConn1</serviceId>
<controlURL>/ctl/IPConn</controlURL><eventSubURL>/evt/IPConn</eventSubURL><SCPDURL>/WANIPCn.xml</SCPDURL>
</service></serviceList></device></deviceList></device></deviceList></device></root>"#
}

fn args(dir: &str, names: &[&str]) -> String {
    names
        .iter()
        .map(|n| format!("<argument><name>{n}</name><direction>{dir}</direction></argument>"))
        .collect()
}

async fn scpd() -> String {
    format!(
        r#"<?xml version="1.0"?><scpd xmlns="urn:schemas-upnp-org:service-1-0"><actionList>
<action><name>AddPortMapping</name><argumentList>{}</argumentList></action>
<action><name>DeletePortMapping</name><argumentList>{}</argumentList></action>
<action><name>GetExternalIPAddress</name><argumentList>{}</argumentList></action>
</actionList></scpd>"#,
        args(
            "in",
            &[
                "NewRemoteHost",
                "NewExternalPort",
                "NewProtocol",
                "NewInternalPort",
                "NewInternalClient",
                "NewEnabled",
                "NewPortMappingDescription",
                "NewLeaseDuration",
            ]
        ),
        args("in", &["NewRemoteHost", "NewExternalPort", "NewProtocol"]),
        args("out", &["NewExternalIPAddress"]),
    )
}

async fn control(State(igd): State<Shared>, headers: HeaderMap, body: String) -> String {
    let action = headers
        .get("soapaction")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim_matches('"')
        .rsplit('#')
        .next()
        .unwrap_or("")
        .to_string();
    let mut g = igd.lock().unwrap();
    match action.as_str() {
        "AddPortMapping" => {
            let proto = tag(&body, "NewProtocol").to_string();
            let port: u16 = tag(&body, "NewExternalPort").parse().unwrap_or(0);
            let lease: u32 = tag(&body, "NewLeaseDuration").parse().unwrap_or(0);
            if g.fail_add {
                return fault(501, "ActionFailed");
            }
            if g.in_use == Some(port) {
                return fault(718, "ConflictInMappingEntry");
            }
            if g.only_permanent && lease != 0 {
                return fault(725, "OnlyPermanentLeasesSupported");
            }
            let client = tag(&body, "NewInternalClient").to_string();
            let iport: u16 = tag(&body, "NewInternalPort").parse().unwrap_or(0);
            let desc = tag(&body, "NewPortMappingDescription").to_string();
            g.mappings
                .insert((proto, port), (client, iport, lease, desc));
            envelope(&format!(
                r#"<u:AddPortMappingResponse xmlns:u="{SERVICE}"/>"#
            ))
        }
        "DeletePortMapping" => {
            let proto = tag(&body, "NewProtocol").to_string();
            let port: u16 = tag(&body, "NewExternalPort").parse().unwrap_or(0);
            if g.mappings.remove(&(proto.clone(), port)).is_none() {
                return fault(714, "NoSuchEntryInArray");
            }
            g.deleted.push((proto, port));
            envelope(&format!(
                r#"<u:DeletePortMappingResponse xmlns:u="{SERVICE}"/>"#
            ))
        }
        "GetExternalIPAddress" => envelope(&format!(
            r#"<u:GetExternalIPAddressResponse xmlns:u="{SERVICE}"><NewExternalIPAddress>203.0.113.7</NewExternalIPAddress></u:GetExternalIPAddressResponse>"#
        )),
        _ => fault(401, "Invalid Action"),
    }
}

/// Start the fake gateway; returns its SSDP address (what `MAGNETAR_DHT_UPNP_GATEWAY` names).
async fn fake_igd(igd: Shared) -> SocketAddr {
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_port = http.local_addr().unwrap().port();
    let app = Router::new()
        .route("/rootDesc.xml", get(root_desc))
        .route("/WANIPCn.xml", get(scpd))
        .route("/ctl/IPConn", post(control))
        .with_state(igd.clone());
    tokio::spawn(async move { axum::serve(http, app).await.unwrap() });
    let ssdp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = ssdp.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            let Ok((n, from)) = ssdp.recv_from(&mut buf).await else {
                return;
            };
            let req = String::from_utf8_lossy(&buf[..n]);
            if !req.starts_with("M-SEARCH") {
                continue;
            }
            igd.lock().unwrap().searches += 1;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\n\
                 ST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\
                 USN: uuid:fake::urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\
                 EXT:\r\nSERVER: fake/1.0 UPnP/1.1\r\n\
                 LOCATION: http://127.0.0.1:{http_port}/rootDesc.xml\r\n\r\n"
            );
            let _ = ssdp.send_to(resp.as_bytes(), from).await;
        }
    });
    addr
}

fn cfg(gateway: SocketAddr) -> UpnpConfig {
    let mut c = UpnpConfig::from_settings(Some(&gateway.to_string()), None);
    // Never the real network: no multicast M-SEARCH.
    c.multicast = false;
    c.search_timeout = Duration::from_secs(2);
    // igd-next never trusts a loopback gateway on its own (the fake's address).
    c.trusted_ips = vec!["127.0.0.1".parse().unwrap()];
    c
}

const WANTED: [(Protocol, u16); 3] = [
    (Protocol::Udp, 6891),
    (Protocol::Udp, 6892),
    (Protocol::Tcp, 7001),
];

#[tokio::test]
async fn maps_udp_and_tcp_through_unicast_discovery() {
    let igd: Shared = Arc::default();
    let gw = fake_igd(igd.clone()).await;
    let status: upnp::Status = Arc::default();
    let (round, _) = upnp::map_once(&cfg(gw), &WANTED, &status).await.unwrap();
    assert!(round.errors.is_empty(), "{:?}", round.errors);
    assert_eq!(round.mapped.len(), 3);
    let g = igd.lock().unwrap();
    assert!(g.searches >= 1);
    let want: BTreeMap<(String, u16), (String, u16, u32, String)> =
        [("UDP", 6891), ("UDP", 6892), ("TCP", 7001)]
            .into_iter()
            .map(|(p, port)| {
                (
                    (p.to_string(), port),
                    (
                        "127.0.0.1".to_string(),
                        port,
                        3600,
                        "magnetar-dht".to_string(),
                    ),
                )
            })
            .collect();
    assert_eq!(
        g.mappings, want,
        "external = internal port, our LAN address, 1 h lease"
    );
    let s = status.lock().unwrap();
    assert_eq!(s.state, "mapped");
    assert_eq!(s.mappings.len(), 3);
    assert!(s.renewed_at.is_some());
    assert!(s.gateway.as_deref().unwrap().starts_with("127.0.0.1:"));
}

#[tokio::test]
async fn permanent_only_routers_get_lease_zero_and_conflicts_are_reported() {
    let igd: Shared = Arc::new(Mutex::new(Igd {
        only_permanent: true,
        in_use: Some(6892),
        ..Default::default()
    }));
    let gw = fake_igd(igd.clone()).await;
    let status: upnp::Status = Arc::default();
    let (round, _) = upnp::map_once(&cfg(gw), &WANTED, &status).await.unwrap();
    assert_eq!(round.mapped.len(), 2);
    assert!(round.mapped.iter().all(|m| m.lease_secs == 0));
    assert_eq!(round.errors.len(), 1);
    assert!(round.errors[0].contains("6892"), "{:?}", round.errors);
    assert!(
        round.errors[0].contains("another host"),
        "{:?}",
        round.errors
    );
    let g = igd.lock().unwrap();
    assert_eq!(g.mappings.len(), 2);
    assert!(g.mappings.values().all(|m| m.2 == 0));
    assert!(!g.mappings.contains_key(&("UDP".to_string(), 6892)));
    assert_eq!(status.lock().unwrap().state, "partial");
}

#[tokio::test]
async fn loop_maps_then_removes_its_mappings_on_stop() {
    let igd: Shared = Arc::default();
    let gw = fake_igd(igd.clone()).await;
    let status: upnp::Status = Arc::default();
    let (tx, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(upnp::upnp_loop(
        cfg(gw),
        WANTED[..2].to_vec(),
        status.clone(),
        rx,
    ));
    let t0 = std::time::Instant::now();
    // The status is set after the whole round (mappings, then the external address): wait for
    // it, not only for the gateway's table (a race under a loaded test run).
    while igd.lock().unwrap().mappings.len() < 2 || status.lock().unwrap().state != "mapped" {
        assert!(t0.elapsed() < Duration::from_secs(10), "not mapped");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("stops within 5 s")
        .unwrap();
    let g = igd.lock().unwrap();
    assert!(g.mappings.is_empty(), "{:?}", g.mappings);
    assert_eq!(g.deleted.len(), 2);
    let s = status.lock().unwrap();
    assert_eq!(s.state, "off");
    assert!(s.mappings.is_empty());
}

#[tokio::test]
async fn a_failed_renewal_still_removes_earlier_mappings_at_stop() {
    // Lease-0 mappings (725 router) never expire: forgetting them after a failed renewal
    // round would leave the ports open on the router for good.
    let igd: Shared = Arc::new(Mutex::new(Igd {
        only_permanent: true,
        ..Default::default()
    }));
    let gw = fake_igd(igd.clone()).await;
    let status: upnp::Status = Arc::default();
    let (tx, rx) = tokio::sync::watch::channel(false);
    let mut c = cfg(gw);
    c.renew_every = Duration::from_millis(200);
    let task = tokio::spawn(upnp::upnp_loop(c, WANTED.to_vec(), status.clone(), rx));
    let t0 = std::time::Instant::now();
    while igd.lock().unwrap().mappings.len() < 3 {
        assert!(t0.elapsed() < Duration::from_secs(10), "not mapped");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    igd.lock().unwrap().fail_add = true;
    while status.lock().unwrap().state != "failed" {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "renewal did not run"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("all removals fit one 2 s budget")
        .unwrap();
    let g = igd.lock().unwrap();
    assert!(g.mappings.is_empty(), "{:?}", g.mappings);
    assert_eq!(g.deleted.len(), 3);
}

#[tokio::test]
async fn a_port_lost_to_another_host_is_never_removed() {
    let igd: Shared = Arc::default();
    let gw = fake_igd(igd.clone()).await;
    let status: upnp::Status = Arc::default();
    let (tx, rx) = tokio::sync::watch::channel(false);
    let mut c = cfg(gw);
    c.renew_every = Duration::from_millis(200);
    let task = tokio::spawn(upnp::upnp_loop(c, WANTED[..2].to_vec(), status.clone(), rx));
    let t0 = std::time::Instant::now();
    while igd.lock().unwrap().mappings.len() < 2 {
        assert!(t0.elapsed() < Duration::from_secs(10), "not mapped");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Our lease on 6892 lapsed and another LAN host took it.
    {
        let mut g = igd.lock().unwrap();
        g.in_use = Some(6892);
        g.mappings.insert(
            ("UDP".into(), 6892),
            ("192.168.1.50".into(), 6892, 0, "other".into()),
        );
    }
    while status.lock().unwrap().state != "partial" {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "renewal did not run"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    let g = igd.lock().unwrap();
    assert_eq!(g.deleted, vec![("UDP".to_string(), 6891)]);
    assert_eq!(
        g.mappings
            .get(&("UDP".to_string(), 6892))
            .map(|m| m.0.as_str()),
        Some("192.168.1.50"),
        "the other host's mapping stays"
    );
}

#[tokio::test]
async fn no_gateway_fails_softly() {
    // A UDP port nobody answers on.
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut c = cfg(silent.local_addr().unwrap());
    c.search_timeout = Duration::from_millis(300);
    let status: upnp::Status = Arc::default();
    let err = match upnp::map_once(&c, &WANTED, &status).await {
        Err(e) => e,
        Ok(_) => panic!("mapped without a gateway"),
    };
    assert!(err.contains("unicast M-SEARCH"), "{err}");
    let (tx, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(upnp::upnp_loop(c, WANTED.to_vec(), status.clone(), rx));
    let t0 = std::time::Instant::now();
    while status.lock().unwrap().state != "failed" {
        assert!(t0.elapsed() < Duration::from_secs(5), "never failed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(status.lock().unwrap().last_error.is_some());
    tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    drop(silent);
}

fn fake_cfg(
    ip: std::net::Ipv4Addr,
    bootstrap: Vec<std::net::SocketAddrV4>,
) -> magnetar_core::dht::EngineConfig {
    let mut c = magnetar_core::dht::EngineConfig::new(magnetar_core::dht::Mode::Aggressive);
    c.bind = ip;
    c.ports = vec![0];
    c.bootstrap = bootstrap;
    c.allow_loopback = true;
    c.query_timeout = Duration::from_millis(500);
    c.per_ip_qps = 100.0;
    c.response_per_ip = 1000.0;
    c.response_rate = 10_000.0;
    c.vote_responders = 3;
    c.vote_prefixes = 1;
    c.sample = false;
    c
}

/// `serve`'s crawler: maps its UDP ports through the fake gateway, shows up live in
/// `/api/v1/stats` and `/health` (feature `dht`), removes the mappings when it stops.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crawler_maps_its_ports_and_reports_live_stats() {
    use magnetar_core::dht::{Dht, Mode};
    use magnetar_server::dht::{self, DhtOptions, status::DhtService};
    use std::net::{Ipv4Addr, SocketAddrV4};

    let Some(db) = common::test_db().await else {
        return;
    };
    let pool = db.store.pool().clone();
    let igd: Shared = Arc::default();
    let gw = fake_igd(igd.clone()).await;
    let mut net = Vec::new();
    let mut boot = Vec::new();
    for i in 0..4u8 {
        let ip = Ipv4Addr::new(127, 0, 31, 10 + i);
        let (d, _rx) = Dht::start(fake_cfg(ip, boot.clone())).await.unwrap();
        let a = SocketAddrV4::new(ip, d.local_ports()[0]);
        boot.push(a);
        net.push((d, a));
    }
    let svc = DhtService::new(Mode::Aggressive);
    let opts = DhtOptions {
        mode: Mode::Aggressive,
        ports: vec![0, 0],
        qps: 400,
        pool_cap: 10_000,
        fetch_known: false,
        peer_lookups: 4,
        lookup_timeout: Duration::from_millis(1500),
        peers_per_hash: 12,
        dry_run: false,
        bloom_bits: 1 << 20,
        queue_cap: 1000,
        fetch_concurrency: 4,
        metadata_max: 1 << 20,
        tcp_rate: 1000,
        tcp_max: 350,
        probe: Vec::new(),
        screen: None,
        upnp: Some(cfg(gw)),
        service: Some(svc.clone()),
    };
    let boot0 = net[0].1;
    let (tx, stop) = tokio::sync::watch::channel(false);
    let run = tokio::spawn(dht::run(Some(pool.clone()), opts, stop, None, move |c| {
        c.bind = Ipv4Addr::new(127, 0, 32, 1);
        c.bootstrap = vec![boot0];
        c.allow_loopback = true;
        c.query_timeout = Duration::from_millis(500);
        c.per_ip_qps = 100.0;
        c.response_per_ip = 1000.0;
        c.response_rate = 10_000.0;
        c.vote_responders = 3;
        c.vote_prefixes = 1;
    }));
    let t0 = std::time::Instant::now();
    loop {
        let live = svc.live();
        // The status follows the whole round (see loop_maps_then_removes_its_mappings_on_stop).
        let mapped = live.upnp.as_ref().is_some_and(|u| u.state == "mapped");
        if live.running && mapped && igd.lock().unwrap().mappings.len() == 2 {
            assert_eq!(live.mode, "aggressive");
            assert_eq!(live.virtual_nodes, 2);
            assert_eq!(live.ports.len(), 2);
            let up = live.upnp.unwrap();
            assert_eq!(up.state, "mapped");
            let mut ports: Vec<u16> = up.mappings.iter().map(|m| m.port).collect();
            ports.sort_unstable();
            let mut want = live.ports.clone();
            want.sort_unstable();
            assert_eq!(ports, want, "the bound UDP ports are mapped");
            assert!(up.mappings.iter().all(|m| m.protocol == "udp"));
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "{live:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mappings = igd.lock().unwrap().mappings.clone();
    assert!(
        mappings.keys().all(|(p, _)| p == "UDP"),
        "no TCP mapping: {mappings:?}"
    );

    // Through the API: /health lists `dht`, /api/v1/stats carries the live numbers.
    let state = magnetar_server::AppState::new(Arc::new(db.store.clone()), common::TOKEN)
        .with_dht(Some(svc.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, magnetar_server::router(state))
            .await
            .unwrap();
    });
    let http = reqwest::Client::new();
    let health: magnetar_core::api::Health = http
        .get(format!("http://{addr}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(health.features.iter().any(|f| f == "dht"));
    assert!(health.features.iter().any(|f| f == "origin"));
    let stats: magnetar_core::api::StatsResponse = http
        .get(format!("http://{addr}/api/v1/stats"))
        .bearer_auth(common::TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let d = stats.dht.expect("dht stats");
    assert!(d.running);
    assert_eq!(d.upnp.unwrap().mappings.len(), 2);
    assert_eq!(d.stored_total, 0);
    assert!(["public", "private", "unknown"].contains(&d.external_ip.as_str()));

    tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(12), run)
        .await
        .expect("stops")
        .unwrap()
        .unwrap();
    {
        let g = igd.lock().unwrap();
        assert!(g.mappings.is_empty(), "removed at stop: {:?}", g.mappings);
        assert_eq!(g.deleted.len(), 2);
    }
    let after = svc.live();
    assert!(!after.running);
    assert_eq!(after.upnp.unwrap().state, "off");
    for (d, _) in net {
        d.shutdown().await;
    }
}
