//! BEP 48 HTTP scrape.
//!
//! Tracker URLs come from Nostr events, so every request goes through a [`TrackerClient`]
//! (D112): an IP-literal host is checked against the address policy before the request, a name
//! is checked when it is resolved (the client's DNS resolver drops non-public addresses, so the
//! connect goes to a checked address), each redirect hop is checked the same way (at most
//! [`MAX_SCRAPE_REDIRECTS`]) and no proxy is used.

use std::sync::Arc;
use std::time::Duration;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_encode};

use super::bencode::{self, Value};
use super::udp::ScrapeStats;
use crate::error::{Error, Result};
use crate::netaddr::{self, AddrPolicy, PolicyResolver};

/// Hashes per request: 50 × 60 chars keeps the URL around 3 KB, well inside common limits.
pub const HTTP_SCRAPE_BATCH: usize = 50;

/// Largest scrape answer read (D111). One file entry is about 80 bytes (20-byte key and three
/// counters), so 50 of them take 4 KB; trackers that add `name` or `flags` stay far below this.
/// A larger body is a broken or hostile tracker and fails the slot without being buffered.
pub const MAX_SCRAPE_BODY: usize = 256 << 10;

/// Most bencode values (keys included) a scrape answer may decode to (D111): 50 entries of
/// about 8 values each plus the envelope. Priced with `count_values` before decoding, since
/// 2-byte values (`le`) cost ~30× their size once decoded.
pub const MAX_SCRAPE_VALUES: usize = 4096;

/// RFC 3986 unreserved characters stay literal; every other byte is `%XX`.
const INFO_HASH_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'.')
    .remove(b'-')
    .remove(b'_')
    .remove(b'~');

/// Redirects a scrape follows (an http→https move, a tracker that changed host). More is a
/// failed scrape.
pub const MAX_SCRAPE_REDIRECTS: usize = 3;

/// Whether a scrape follows a redirect to `next` after `hops` earlier ones (D112): within
/// [`MAX_SCRAPE_REDIRECTS`], http(s) only, and an IP-literal host must pass `policy` (a name
/// is checked by the client's resolver when the hop connects). A public tracker therefore
/// cannot bounce the request to `http://192.168.1.1/...`.
pub fn redirect_allowed(policy: AddrPolicy, hops: usize, next: &url::Url) -> Result<()> {
    if hops >= MAX_SCRAPE_REDIRECTS {
        return Err(Error::Network(format!(
            "more than {MAX_SCRAPE_REDIRECTS} redirects"
        )));
    }
    if !matches!(next.scheme(), "http" | "https") {
        return Err(Error::Invalid(format!("redirect to {}", next.scheme())));
    }
    netaddr::check_url_host(policy, "redirect", next)
}

/// HTTP client for tracker scrapes (D112). Every redirect hop goes through
/// [`redirect_allowed`] and the policy resolver. No proxy: a proxy resolves the name itself,
/// past the resolver's check, so `HTTP(S)_PROXY` is ignored for scrapes.
#[derive(Clone, Debug)]
pub struct TrackerClient {
    http: reqwest::Client,
    policy: AddrPolicy,
}

impl TrackerClient {
    /// `timeout` bounds each whole request, DNS lookup included. Production passes
    /// [`AddrPolicy::PUBLIC`]; tests on 127.0.0.1 pass [`AddrPolicy::loopback_for_tests`].
    pub fn new(timeout: Duration, policy: AddrPolicy) -> Result<Self> {
        let http = crate::http::builder(timeout)
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                // `previous` holds the first URL and every earlier hop.
                let hops = attempt.previous().len().saturating_sub(1);
                match redirect_allowed(policy, hops, attempt.url()) {
                    Ok(()) => attempt.follow(),
                    Err(e) => attempt.error(e),
                }
            }))
            .no_proxy()
            .dns_resolver(Arc::new(PolicyResolver { policy, timeout }))
            .build()?;
        Ok(Self { http, policy })
    }

    pub fn policy(&self) -> AddrPolicy {
        self.policy
    }
}

/// announce URL -> scrape URL (last path segment starting with "announce"), None if unsupported.
pub fn scrape_url(announce: &str) -> Option<String> {
    let mut url = url::Url::parse(announce.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_fragment(None);
    let path = url.path();
    let cut = path.rfind('/').map_or(0, |i| i + 1);
    let rest = path[cut..].strip_prefix("announce")?;
    let new_path = format!("{}scrape{rest}", &path[..cut]);
    url.set_path(&new_path);
    Some(url.into())
}

/// Scrape on an http(s) tracker. Result positional to `hashes`.
pub async fn scrape(
    client: &TrackerClient,
    tracker: &str,
    hashes: &[[u8; 20]],
) -> crate::Result<Vec<Option<ScrapeStats>>> {
    let mut out = Vec::with_capacity(hashes.len());
    for chunk in hashes.chunks(HTTP_SCRAPE_BATCH) {
        out.extend(scrape_chunk(client, tracker, chunk).await?);
    }
    Ok(out)
}

/// One scrape request (callers keep `chunk` within [`HTTP_SCRAPE_BATCH`]).
pub async fn scrape_chunk(
    client: &TrackerClient,
    tracker: &str,
    chunk: &[[u8; 20]],
) -> crate::Result<Vec<Option<ScrapeStats>>> {
    if chunk.is_empty() {
        return Ok(Vec::new());
    }
    let base = scrape_url(tracker)
        .ok_or_else(|| Error::Invalid(format!("tracker does not support scrape: {tracker}")))?;
    let mut url = base.clone();
    let mut sep = if base.contains('?') { '&' } else { '?' };
    for h in chunk {
        url.push(sep);
        url.push_str("info_hash=");
        url.extend(percent_encode(h, INFO_HASH_SET));
        sep = '&';
    }
    let parsed =
        url::Url::parse(&url).map_err(|e| Error::Invalid(format!("tracker {tracker}: {e}")))?;
    // IP literals never reach the resolver: check them here.
    netaddr::check_url_host(client.policy, tracker, &parsed)?;
    let resp = client
        .http
        .get(parsed)
        .send()
        .await
        .map_err(|e| send_error(tracker, e))?;
    let status = resp.status();
    let body = crate::body::read_capped(resp, MAX_SCRAPE_BODY).await;
    if !status.is_success() {
        let body = body.unwrap_or_default();
        return Err(Error::Server {
            status: status.as_u16(),
            msg: format!(
                "{tracker}: {}",
                String::from_utf8_lossy(&body[..body.len().min(200)])
            ),
        });
    }
    let body = body.map_err(|e| Error::Network(format!("{tracker}: scrape answer: {e}")))?;
    parse_response(tracker, &body, chunk)
}

/// A name the resolver refused or a redirect hop [`redirect_allowed`] stopped comes back
/// wrapped in reqwest's error: keep it typed.
fn send_error(tracker: &str, e: reqwest::Error) -> Error {
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        match s.downcast_ref::<Error>() {
            Some(Error::Refused(msg)) => return Error::Refused(format!("{tracker}: {msg}")),
            Some(Error::Network(msg)) => return Error::Network(format!("{tracker}: {msg}")),
            Some(Error::Invalid(msg)) => return Error::Invalid(format!("{tracker}: {msg}")),
            _ => {}
        }
        src = s.source();
    }
    Error::from(e)
}

/// `files` maps raw hash → stats. A hash missing from a successful answer is unknown to the
/// tracker, which is the same thing UDP reports as 0/0/0 (no swarm registered).
fn parse_response(
    tracker: &str,
    body: &[u8],
    chunk: &[[u8; 20]],
) -> Result<Vec<Option<ScrapeStats>>> {
    if bencode::count_values(body, MAX_SCRAPE_VALUES) > MAX_SCRAPE_VALUES {
        return Err(Error::Network(format!(
            "{tracker}: scrape answer has more than {MAX_SCRAPE_VALUES} values"
        )));
    }
    let root = bencode::decode(body)
        .map_err(|e| Error::Network(format!("{tracker}: bad bencode: {e}")))?;
    if let Some(reason) = root.get(b"failure reason") {
        let msg = reason
            .as_bytes()
            .map(String::from_utf8_lossy)
            .unwrap_or_default();
        return Err(Error::Network(format!("{tracker}: tracker failure: {msg}")));
    }
    let files = root
        .get(b"files")
        .and_then(Value::as_dict)
        .ok_or_else(|| Error::Network(format!("{tracker}: scrape answer without files")))?;
    let stat = |entry: &Value, key: &[u8]| {
        entry
            .get(key)
            .and_then(Value::as_int)
            .map_or(0, |n| n.clamp(0, u32::MAX as i64) as u32)
    };
    Ok(chunk
        .iter()
        .map(|h| {
            let entry = files.iter().find(|(k, _)| k == h).map(|(_, v)| v);
            Some(match entry {
                Some(e) => ScrapeStats {
                    seeders: stat(e, b"complete"),
                    leechers: stat(e, b"incomplete"),
                    completed: stat(e, b"downloaded"),
                },
                None => ScrapeStats {
                    seeders: 0,
                    leechers: 0,
                    completed: 0,
                },
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D112: the redirect decision on literals, without any connect.
    #[test]
    fn redirect_hops_are_checked() {
        let p = AddrPolicy::PUBLIC;
        let u = |s: &str| url::Url::parse(s).unwrap();
        assert!(redirect_allowed(p, 0, &u("https://1.1.1.1/scrape")).is_ok());
        assert!(redirect_allowed(p, 2, &u("https://tracker.example/scrape")).is_ok());
        for bad in [
            "http://192.168.1.1/cgi-bin/admin",
            "http://100.100.1.1:8443/",
            "http://[fd00::1]/",
            "http://127.1/",
            "http://[::ffff:10.0.0.1]/",
        ] {
            let e = redirect_allowed(p, 0, &u(bad)).unwrap_err();
            assert!(matches!(e, Error::Refused(_)), "{bad}: {e}");
        }
        assert!(redirect_allowed(p, 0, &u("ftp://1.1.1.1/x")).is_err());
        assert!(redirect_allowed(p, MAX_SCRAPE_REDIRECTS, &u("https://1.1.1.1/")).is_err());
    }
}
