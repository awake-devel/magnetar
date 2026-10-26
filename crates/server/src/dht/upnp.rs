//! Router port mappings for the crawler (D57, D72, docs/DHT.md §9): UPnP IGD through
//! `igd-next` (MIT). The aggressive crawler answers queries on fixed UDP ports; without a
//! mapping a home router drops unsolicited datagrams, so passive capture only sees flows the
//! crawler opened itself.
//!
//! Discovery is a **unicast** SSDP M-SEARCH to the configured or default gateway first (the
//! user's MiniUPnPd answers only that, docs/DHT.md §1), then the standard multicast one.
//! Mappings: external port = internal port, description `magnetar-dht`, lease 3600 s renewed
//! every 30 min; a router answering `OnlyPermanentLeasesSupported` (725) gets lease 0. A port
//! already mapped to someone else (718) is logged and left unmapped. Mappings are removed on a
//! clean stop. Nothing here is required: every failure leaves the crawler running unmapped.
//!
//! The mapper is protocol-generic (UDP and TCP), but the crawler asks for UDP only: it never
//! listens on TCP (it never announces, so no peer has a reason to connect, docs/DHT.md §4), and
//! mapping a port nobody listens on would only open a hole (D72).
//!
//! Test seam: `UpnpConfig::gateway` takes any `host:port`, so tests point the M-SEARCH at an
//! in-process fake gateway on 127.0.0.1 (`tests/dht_upnp.rs`) and no router is touched.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use magnetar_core::api::{UpnpMapping, UpnpStatus};
use tokio::sync::watch;

#[cfg(feature = "upnp")]
use crate::daemons::stop_requested;

pub const DESCRIPTION: &str = "magnetar-dht";
pub const LEASE_SECS: u32 = 3600;
pub const RENEW_EVERY: Duration = Duration::from_secs(30 * 60);
const SSDP_PORT: u16 = 1900;
/// Backoff after a failed discovery or mapping round: 1 min doubling to 30 min.
#[cfg(feature = "upnp")]
const RETRY_MIN: Duration = Duration::from_secs(60);
#[cfg(feature = "upnp")]
const RETRY_MAX: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Protocol {
    Udp,
    Tcp,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Udp => "udp",
            Protocol::Tcp => "tcp",
        }
    }
}

#[derive(Clone, Debug)]
pub struct UpnpConfig {
    /// Unicast M-SEARCH target (`MAGNETAR_DHT_UPNP_GATEWAY`, else the default gateway). None:
    /// multicast only.
    pub gateway: Option<SocketAddr>,
    /// Also try the standard multicast M-SEARCH when the unicast one gets no answer.
    pub multicast: bool,
    /// LAN address to map to (`MAGNETAR_DHT_UPNP_LOCAL_IP`); None: the address the route to
    /// the gateway leaves from.
    pub local_ip: Option<Ipv4Addr>,
    pub lease_secs: u32,
    pub renew_every: Duration,
    /// Whole discovery budget, and per answer.
    pub search_timeout: Duration,
    pub description: String,
    /// Extra addresses trusted for the gateway's description and control URLs. igd-next
    /// only trusts the SSDP responder's own address, and never a loopback one; tests add
    /// 127.0.0.1 here for their fake gateway. Empty in production.
    pub trusted_ips: Vec<IpAddr>,
}

impl UpnpConfig {
    /// From `MAGNETAR_DHT_UPNP_GATEWAY` / `_LOCAL_IP`, falling back to /proc/net/route.
    pub fn from_settings(gateway: Option<&str>, local_ip: Option<Ipv4Addr>) -> Self {
        let gateway = match gateway.map(str::trim).filter(|g| !g.is_empty()) {
            Some(g) => parse_gateway(g),
            None => default_gateway().map(|ip| SocketAddr::V4(SocketAddrV4::new(ip, SSDP_PORT))),
        };
        Self {
            gateway,
            multicast: true,
            local_ip,
            lease_secs: LEASE_SECS,
            renew_every: RENEW_EVERY,
            search_timeout: Duration::from_secs(5),
            description: DESCRIPTION.into(),
            trusted_ips: Vec::new(),
        }
    }
}

/// `host[:port]` with an IPv4 host (port 1900 by default). Names are not resolved: a gateway is
/// a LAN address.
pub fn parse_gateway(s: &str) -> Option<SocketAddr> {
    if let Ok(a) = s.parse::<SocketAddrV4>() {
        return Some(SocketAddr::V4(a));
    }
    s.parse::<Ipv4Addr>()
        .ok()
        .map(|ip| SocketAddr::V4(SocketAddrV4::new(ip, SSDP_PORT)))
}

/// The IPv4 default gateway from /proc/net/route (the line whose destination is 0).
pub fn default_gateway() -> Option<Ipv4Addr> {
    parse_route_table(&std::fs::read_to_string("/proc/net/route").ok()?)
}

/// /proc/net/route: `Iface Destination Gateway Flags …`, addresses as little-endian hex.
pub fn parse_route_table(text: &str) -> Option<Ipv4Addr> {
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (Some(dest), Some(gw), Some(flags)) = (f.get(1), f.get(2), f.get(3)) else {
            continue;
        };
        let flags = u16::from_str_radix(flags, 16).unwrap_or(0);
        // RTF_UP (1) and RTF_GATEWAY (2).
        if *dest != "00000000" || flags & 0x3 != 0x3 {
            continue;
        }
        let gw = u32::from_str_radix(gw, 16).ok()?;
        let ip = Ipv4Addr::from(gw.swap_bytes());
        if !ip.is_unspecified() {
            return Some(ip);
        }
    }
    None
}

/// Docker's default bridge networks (172.16/12) are where a container's default gateway
/// points: no IGD answers there.
#[cfg(feature = "upnp")]
fn route_is_bridge(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v) if v.octets()[0] == 172 && (v.octets()[1] & 0xf0) == 16)
}

/// The local address a UDP "connection" to `to` would leave from (no packet is sent).
pub fn route_source(to: SocketAddr) -> Option<Ipv4Addr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect(to).ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// Shared mapping state (served by `/api/v1/stats` as `DhtStats.upnp`).
pub type Status = Arc<Mutex<UpnpStatus>>;

fn set(status: &Status, f: impl FnOnce(&mut UpnpStatus)) {
    f(&mut status.lock().unwrap());
}

/// What one mapping round did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Round {
    pub mapped: Vec<UpnpMapping>,
    pub errors: Vec<String>,
}

#[cfg(feature = "upnp")]
mod igd {
    use super::*;
    use igd_next::aio::Gateway;
    use igd_next::aio::tokio::{Tokio, search_gateway};
    use igd_next::{AddPortError, PortMappingProtocol, SearchOptions};

    pub type Gw = Gateway<Tokio>;

    fn proto(p: Protocol) -> PortMappingProtocol {
        match p {
            Protocol::Udp => PortMappingProtocol::UDP,
            Protocol::Tcp => PortMappingProtocol::TCP,
        }
    }

    async fn search(to: SocketAddr, cfg: &UpnpConfig) -> Result<Gw, String> {
        let mut o = SearchOptions::default();
        o.broadcast_address = to;
        o.timeout = Some(cfg.search_timeout);
        o.single_search_timeout = Some(cfg.search_timeout);
        o.allowed_gateway_ips = cfg.trusted_ips.clone();
        search_gateway(o).await.map_err(|e| e.to_string())
    }

    /// Unicast first (the gateway we were told about), then multicast.
    pub async fn discover(cfg: &UpnpConfig) -> Result<Gw, String> {
        let mut last = String::from("no gateway configured or found");
        if let Some(g) = cfg.gateway {
            match search(g, cfg).await {
                Ok(gw) => return Ok(gw),
                Err(e) => last = format!("unicast M-SEARCH to {g}: {e}"),
            }
        }
        if cfg.multicast {
            let m: SocketAddr = "239.255.255.250:1900"
                .parse()
                .expect("SSDP multicast address");
            match search(m, cfg).await {
                Ok(gw) => return Ok(gw),
                Err(e) => last = format!("{last}; multicast M-SEARCH: {e}"),
            }
        }
        Err(last)
    }

    pub fn gateway_addr(gw: &Gw) -> SocketAddr {
        gw.addr
    }

    pub async fn external_ip(gw: &Gw) -> Option<IpAddr> {
        gw.get_external_ip().await.ok()
    }

    /// Map one port; a router refusing leases gets a permanent mapping (lease 0, removed at
    /// stop like the others). Ok(lease used).
    pub async fn map(
        gw: &Gw,
        p: Protocol,
        port: u16,
        local: Ipv4Addr,
        lease: u32,
        description: &str,
    ) -> Result<u32, String> {
        let local = SocketAddr::V4(SocketAddrV4::new(local, port));
        match gw.add_port(proto(p), port, local, lease, description).await {
            Ok(()) => Ok(lease),
            Err(AddPortError::OnlyPermanentLeasesSupported) if lease != 0 => gw
                .add_port(proto(p), port, local, 0, description)
                .await
                .map(|()| 0)
                .map_err(|e| e.to_string()),
            Err(AddPortError::PortInUse) => Err(format!(
                "{} {port} is already mapped to another host",
                p.as_str()
            )),
            Err(e) => Err(format!("{} {port}: {e}", p.as_str())),
        }
    }

    pub async fn unmap(gw: &Gw, p: Protocol, port: u16) -> Result<(), String> {
        gw.remove_port(proto(p), port)
            .await
            .map_err(|e| e.to_string())
    }
}

/// One discovery + mapping round. `Err` when no gateway answered.
#[cfg(feature = "upnp")]
pub async fn map_once(
    cfg: &UpnpConfig,
    wanted: &[(Protocol, u16)],
    status: &Status,
) -> Result<(Round, igd::Gw), String> {
    let gw = igd::discover(cfg).await?;
    let gw_addr = igd::gateway_addr(&gw);
    let local = cfg
        .local_ip
        .or_else(|| route_source(gw_addr))
        .ok_or_else(|| format!("no local IPv4 address towards {gw_addr}"))?;
    let mut round = Round::default();
    for &(p, port) in wanted {
        match igd::map(&gw, p, port, local, cfg.lease_secs, &cfg.description).await {
            Ok(lease_secs) => round.mapped.push(UpnpMapping {
                protocol: p.as_str().into(),
                port,
                lease_secs,
            }),
            Err(e) => round.errors.push(e),
        }
    }
    let ext = igd::external_ip(&gw).await;
    set(status, |s| {
        s.gateway = Some(gw_addr.to_string());
        s.state = if round.errors.is_empty() {
            "mapped"
        } else if round.mapped.is_empty() {
            "failed"
        } else {
            "partial"
        }
        .into();
        s.mappings = round.mapped.clone();
        s.last_error = round.errors.first().cloned();
        if !round.mapped.is_empty() {
            s.renewed_at = Some(magnetar_core::now());
        }
    });
    if let Some(ip) = ext {
        tracing::debug!("UPnP: gateway reports a {} external address", ip_class(ip));
    }
    Ok((round, gw))
}

/// The class of an external address, never the address itself (D58).
pub fn ip_class(ip: IpAddr) -> &'static str {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            if o[0] == 100 && (o[1] & 0xc0) == 64 {
                "cgnat"
            } else if v.is_private() || v.is_loopback() || v.is_link_local() || v.is_unspecified() {
                "private"
            } else {
                "public"
            }
        }
        IpAddr::V6(_) => "public",
    }
}

/// Keep `wanted` mapped until `stop`, then remove the mappings. Runs beside the crawler.
#[cfg(feature = "upnp")]
pub async fn upnp_loop(
    cfg: UpnpConfig,
    wanted: Vec<(Protocol, u16)>,
    status: Status,
    mut stop: watch::Receiver<bool>,
) {
    if wanted.is_empty() {
        return;
    }
    if std::path::Path::new("/.dockerenv").exists()
        && (cfg.local_ip.is_none() || cfg.gateway.is_none_or(|g| route_is_bridge(g.ip())))
    {
        tracing::warn!(
            "UPnP inside a container needs MAGNETAR_DHT_UPNP_GATEWAY (the router) and \
             MAGNETAR_DHT_UPNP_LOCAL_IP (this host's LAN address); see docs/DOCKER.md"
        );
    }
    set(&status, |s| s.state = "searching".into());
    let mut retry = RETRY_MIN;
    let mut gw: Option<igd::Gw> = None;
    let mut mapped: Vec<(Protocol, u16)> = Vec::new();
    loop {
        let wait = match map_once(&cfg, &wanted, &status).await {
            Ok((round, g)) => {
                if mapped.is_empty() || !round.errors.is_empty() {
                    tracing::info!(
                        "UPnP: {} of {} port(s) mapped on {}{}",
                        round.mapped.len(),
                        wanted.len(),
                        igd::gateway_addr(&g),
                        round
                            .errors
                            .first()
                            .map(|e| format!(" ({e})"))
                            .unwrap_or_default()
                    );
                }
                mapped = round
                    .mapped
                    .iter()
                    .map(|m| {
                        let p = if m.protocol == "tcp" {
                            Protocol::Tcp
                        } else {
                            Protocol::Udp
                        };
                        (p, m.port)
                    })
                    .collect();
                gw = Some(g);
                if round.mapped.is_empty() {
                    let w = retry;
                    retry = (retry * 2).min(RETRY_MAX);
                    w
                } else {
                    retry = RETRY_MIN;
                    cfg.renew_every
                }
            }
            Err(e) => {
                set(&status, |s| {
                    s.state = "failed".into();
                    s.last_error = Some(e.clone());
                });
                tracing::warn!("UPnP: {e}; retrying in {} s", retry.as_secs());
                let w = retry;
                retry = (retry * 2).min(RETRY_MAX);
                w
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = stop_requested(&mut stop) => break,
        }
    }
    if let Some(gw) = gw {
        for (p, port) in &mapped {
            match tokio::time::timeout(Duration::from_secs(2), igd::unmap(&gw, *p, *port)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("UPnP: removing {} {port}: {e}", p.as_str()),
                Err(_) => tracing::warn!("UPnP: removing {} {port}: timed out", p.as_str()),
            }
        }
        if !mapped.is_empty() {
            tracing::info!("UPnP: {} mapping(s) removed", mapped.len());
        }
    }
    set(&status, |s| {
        s.state = "off".into();
        s.mappings.clear();
    });
}

/// Built without the `upnp` feature: say so once, map nothing.
#[cfg(not(feature = "upnp"))]
pub async fn upnp_loop(
    _cfg: UpnpConfig,
    wanted: Vec<(Protocol, u16)>,
    status: Status,
    _stop: watch::Receiver<bool>,
) {
    if !wanted.is_empty() {
        tracing::warn!("UPnP: this build has no `upnp` feature; forward the DHT ports by hand");
        set(&status, |s| {
            s.state = "off".into();
            s.last_error = Some("built without the upnp feature".into());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_table_default_gateway() {
        let t = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                 wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
                 wlan0\t0001A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n";
        assert_eq!(parse_route_table(t), Some(Ipv4Addr::new(192, 168, 1, 1)));
        // A default route without RTF_GATEWAY (point-to-point) or no default route: none.
        let p2p = "Iface\tDestination\tGateway\tFlags\nwg0\t00000000\t00000000\t0001\n";
        assert_eq!(parse_route_table(p2p), None);
        assert_eq!(parse_route_table(""), None);
        assert_eq!(parse_route_table("garbage\n\tx\n"), None);
    }

    #[test]
    fn gateway_parsing() {
        assert_eq!(
            parse_gateway("192.168.1.1"),
            Some("192.168.1.1:1900".parse().unwrap())
        );
        assert_eq!(
            parse_gateway("10.0.0.1:5000"),
            Some("10.0.0.1:5000".parse().unwrap())
        );
        assert_eq!(parse_gateway("router.lan"), None);
    }

    #[test]
    fn ip_classes() {
        assert_eq!(ip_class("100.64.0.1".parse().unwrap()), "cgnat");
        assert_eq!(ip_class("100.127.255.1".parse().unwrap()), "cgnat");
        assert_eq!(ip_class("100.128.0.1".parse().unwrap()), "public");
        assert_eq!(ip_class("192.168.1.10".parse().unwrap()), "private");
        assert_eq!(ip_class("93.184.216.34".parse().unwrap()), "public");
    }
}
