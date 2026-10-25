//! BEP 48 HTTP scrape.

use super::udp::ScrapeStats;

/// announce URL -> scrape URL (last path segment starting with "announce"), None if unsupported.
pub fn scrape_url(announce: &str) -> Option<String> {
    let _ = announce;
    todo!("http::scrape_url")
}

/// Scrape on an http(s) tracker. Result positional to `hashes`.
pub async fn scrape(
    client: &reqwest::Client,
    tracker: &str,
    hashes: &[[u8; 20]],
) -> crate::Result<Vec<Option<ScrapeStats>>> {
    let _ = (client, tracker, hashes);
    todo!("http::scrape")
}
