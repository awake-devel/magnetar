//! BEP 15 UDP tracker scrape.

pub const UDP_SCRAPE_BATCH: usize = 74;

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
    let _ = (tracker, hashes, timeout);
    todo!("udp::scrape")
}
