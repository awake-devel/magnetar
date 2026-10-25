//! BEP 33 DHT scrape (get_peers + scrape=1), client-only.

pub const BOOTSTRAP: &[&str] = &[
    "router.bittorrent.com:6881",
    "dht.transmissionbt.com:6881",
    "dht.libtorrent.org:25401",
    "router.utorrent.com:6881",
];

#[derive(Clone, Debug, PartialEq)]
pub struct Bloom256(pub [u8; 256]);

impl Bloom256 {
    pub fn new() -> Self {
        Bloom256([0; 256])
    }
    pub fn insert_ip(&mut self, ip: &std::net::IpAddr) {
        let _ = ip;
        todo!()
    }
    pub fn union(&mut self, other: &[u8]) {
        for (a, b) in self.0.iter_mut().zip(other) {
            *a |= *b;
        }
    }
    pub fn estimate(&self) -> f64 {
        todo!()
    }
}

impl Default for Bloom256 {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DhtScrape {
    pub seeds_est: u32,
    pub peers_est: u32,
    /// union(BFsd, BFpe) estimate, capped 8000.
    pub total_est: u32,
    pub values_ips: u32,
    pub nodes_queried: u32,
    pub bf_responses: u32,
}

pub async fn scrape(infohash: &[u8; 20], budget: std::time::Duration) -> crate::Result<DhtScrape> {
    let _ = (infohash, budget);
    todo!("dht::scrape")
}
