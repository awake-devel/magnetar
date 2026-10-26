//! BEP 48 HTTP scrape.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_encode};

use super::bencode::{self, Value};
use super::udp::ScrapeStats;
use crate::error::{Error, Result};

/// Hashes per request: 50 × 60 chars keeps the URL around 3 KB, well inside common limits.
pub const HTTP_SCRAPE_BATCH: usize = 50;

/// RFC 3986 unreserved characters stay literal; every other byte is `%XX`.
const INFO_HASH_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'.')
    .remove(b'-')
    .remove(b'_')
    .remove(b'~');

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
    client: &reqwest::Client,
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
    client: &reqwest::Client,
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
    let resp = client.get(&url).send().await?;
    let status = resp.status();
    let body = resp.bytes().await?;
    if !status.is_success() {
        return Err(Error::Server {
            status: status.as_u16(),
            msg: format!(
                "{tracker}: {}",
                String::from_utf8_lossy(&body[..body.len().min(200)])
            ),
        });
    }
    parse_response(tracker, &body, chunk)
}

/// `files` maps raw hash → stats. A hash missing from a successful answer is unknown to the
/// tracker, which is the same thing UDP reports as 0/0/0 (no swarm registered).
fn parse_response(
    tracker: &str,
    body: &[u8],
    chunk: &[[u8; 20]],
) -> Result<Vec<Option<ScrapeStats>>> {
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
