//! BEP 15 UDP tracker scrape.
//!
//! A [`UdpTracker`] owns one socket connected to the tracker (so foreign datagrams are filtered
//! by the kernel), keeps the resolved address for [`DNS_MAX_AGE`] and reuses the connection id
//! for at most 60 s, also across calls: a long-lived checker (the server, D45) sends one connect
//! per minute per tracker instead of one per batch. Responses are positional; trackers silently
//! truncate at 75 entries, so a missing tail entry is `None`.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;

use crate::error::{Error, Result};

pub const UDP_SCRAPE_BATCH: usize = 74;

const PROTOCOL_ID: u64 = 0x0417_2710_1980;
const ACT_CONNECT: u32 = 0;
const ACT_SCRAPE: u32 = 2;
const ACT_ERROR: u32 = 3;
/// BEP 15 lets a client use a connection id for one minute.
const CONN_ID_MAX_AGE: Duration = Duration::from_secs(60);
/// A tracker's address is looked up again after this (or after any failure).
pub const DNS_MAX_AGE: Duration = Duration::from_secs(3600);
/// Attempts per request: the first send plus one retry.
const ATTEMPTS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrapeStats {
    pub seeders: u32,
    pub leechers: u32,
    pub completed: u32,
}

/// Scrape `hashes` (raw 20-byte) on a udp:// tracker URL. Positional; None = not answered (truncated).
pub async fn scrape(
    tracker: &str,
    hashes: &[[u8; 20]],
    timeout: std::time::Duration,
) -> crate::Result<Vec<Option<ScrapeStats>>> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }
    let mut t = UdpTracker::new(tracker);
    let mut out = Vec::with_capacity(hashes.len());
    for chunk in hashes.chunks(UDP_SCRAPE_BATCH) {
        out.extend(t.scrape_chunk(chunk, timeout).await?);
    }
    Ok(out)
}

/// One udp:// tracker: socket, address and connection id kept between scrapes.
pub struct UdpTracker {
    url: String,
    sock: Option<(UdpSocket, Instant)>,
    conn: Option<(u64, Instant)>,
}

impl UdpTracker {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            sock: None,
            conn: None,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// One scrape request for at most [`UDP_SCRAPE_BATCH`] hashes (a longer chunk is cut by
    /// the tracker: its tail comes back `None`). On any error the socket and connection id are
    /// dropped, so the next call starts over with a fresh lookup and connect.
    pub async fn scrape_chunk(
        &mut self,
        chunk: &[[u8; 20]],
        timeout: Duration,
    ) -> Result<Vec<Option<ScrapeStats>>> {
        if chunk.is_empty() {
            return Ok(Vec::new());
        }
        let res = self.try_scrape(chunk, timeout).await;
        if res.is_err() {
            self.sock = None;
            self.conn = None;
        }
        res
    }

    async fn try_scrape(
        &mut self,
        chunk: &[[u8; 20]],
        timeout: Duration,
    ) -> Result<Vec<Option<ScrapeStats>>> {
        if self
            .sock
            .as_ref()
            .is_none_or(|(_, at)| at.elapsed() >= DNS_MAX_AGE)
        {
            self.sock = Some((open(&self.url, timeout).await?, Instant::now()));
            self.conn = None;
        }
        let sock = &self.sock.as_ref().expect("just opened").0;
        let conn_id = match self.conn {
            Some((id, at)) if at.elapsed() < CONN_ID_MAX_AGE => id,
            _ => {
                let at = Instant::now();
                let id = connect(sock, &self.url, timeout).await?;
                self.conn = Some((id, at));
                id
            }
        };
        let tid: u32 = rand::random();
        let mut req = Vec::with_capacity(16 + 20 * chunk.len());
        req.extend_from_slice(&conn_id.to_be_bytes());
        req.extend_from_slice(&ACT_SCRAPE.to_be_bytes());
        req.extend_from_slice(&tid.to_be_bytes());
        for h in chunk {
            req.extend_from_slice(h);
        }
        let resp = request(sock, &self.url, &req, tid, ACT_SCRAPE, timeout).await?;
        let answered = (resp.len() - 8) / 12;
        Ok((0..chunk.len())
            .map(|i| {
                (i < answered).then(|| {
                    let o = 8 + 12 * i;
                    ScrapeStats {
                        seeders: be32(&resp[o..]),
                        completed: be32(&resp[o + 4..]),
                        leechers: be32(&resp[o + 8..]),
                    }
                })
            })
            .collect())
    }
}

/// Resolve and connect a fresh socket to the tracker.
async fn open(tracker: &str, timeout: Duration) -> Result<UdpSocket> {
    let addr = resolve(tracker, timeout).await?;
    let bind: SocketAddr = if addr.is_ipv4() {
        ([0u8; 4], 0).into()
    } else {
        ([0u16; 8], 0).into()
    };
    let sock = UdpSocket::bind(bind).await.map_err(net(tracker))?;
    sock.connect(addr).await.map_err(net(tracker))?;
    Ok(sock)
}

/// `udp://host:port[/path]` → socket address, IPv4 preferred.
async fn resolve(tracker: &str, timeout: Duration) -> Result<SocketAddr> {
    let url = url::Url::parse(tracker.trim())
        .map_err(|e| Error::Invalid(format!("tracker url {tracker}: {e}")))?;
    if url.scheme() != "udp" {
        return Err(Error::Invalid(format!("not a udp tracker: {tracker}")));
    }
    let host = url
        .host_str()
        .ok_or_else(|| Error::Invalid(format!("tracker without host: {tracker}")))?;
    let port = url
        .port()
        .ok_or_else(|| Error::Invalid(format!("udp tracker without port: {tracker}")))?;
    // IPv6 literals come back bracketed from host_str.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<SocketAddr> =
        tokio::time::timeout(timeout, tokio::net::lookup_host((host, port)))
            .await
            .map_err(|_| Error::Timeout(format!("{tracker}: DNS lookup")))?
            .map_err(net(tracker))?
            .collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .copied()
        .ok_or_else(|| Error::Network(format!("{tracker}: no address")))
}

async fn connect(sock: &UdpSocket, tracker: &str, timeout: Duration) -> Result<u64> {
    let tid: u32 = rand::random();
    let mut req = Vec::with_capacity(16);
    req.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
    req.extend_from_slice(&ACT_CONNECT.to_be_bytes());
    req.extend_from_slice(&tid.to_be_bytes());
    let resp = request(sock, tracker, &req, tid, ACT_CONNECT, timeout).await?;
    if resp.len() < 16 {
        return Err(Error::Network(format!("{tracker}: short connect response")));
    }
    Ok(u64::from_be_bytes(resp[8..16].try_into().expect("8 bytes")))
}

/// Send `req` and wait for a datagram carrying `tid`; resend once on timeout. Returns a response
/// with the expected action and at least 8 bytes, or the tracker's error message.
async fn request(
    sock: &UdpSocket,
    tracker: &str,
    req: &[u8],
    tid: u32,
    action: u32,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; 2048];
    for _ in 0..ATTEMPTS {
        sock.send(req).await.map_err(net(tracker))?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let n = match tokio::time::timeout_at(deadline, sock.recv(&mut buf)).await {
                Err(_) => break,
                Ok(r) => r.map_err(net(tracker))?,
            };
            let resp = &buf[..n];
            // Stray or stale datagrams (wrong tid, garbage) are ignored.
            if n < 8 || be32(&resp[4..]) != tid {
                continue;
            }
            return match be32(resp) {
                a if a == action => Ok(resp.to_vec()),
                ACT_ERROR => Err(Error::Network(format!(
                    "{tracker}: tracker error: {}",
                    String::from_utf8_lossy(&resp[8..]).trim_end_matches('\0')
                ))),
                a => Err(Error::Network(format!("{tracker}: unexpected action {a}"))),
            };
        }
    }
    Err(Error::Timeout(format!("{tracker}: no answer")))
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().expect("4 bytes"))
}

fn net(tracker: &str) -> impl Fn(std::io::Error) -> Error + '_ {
    move |e| Error::Network(format!("{tracker}: {e}"))
}
