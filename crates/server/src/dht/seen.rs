//! Seen filter in front of Postgres triage (docs/DHT.md §3.7): two Bloom generations, rotated
//! every 6 h (the BEP 51 maximum interval), so a hash re-sampled after that is triaged again and
//! failed fetches can retry. A false positive only skips one sighting. Bit positions are keyed
//! with a per-process random key, so nobody can pick infohashes that collide on purpose.

use std::time::Duration;

use tokio::time::Instant;

pub const DEFAULT_BITS: u64 = 96 * 1024 * 1024; // 12 MiB per generation
pub const DEFAULT_K: u32 = 7;
pub const ROTATE_EVERY: Duration = Duration::from_secs(6 * 3600);

pub struct SeenFilter {
    gens: [Vec<u64>; 2],
    bits: u64,
    k: u32,
    cur: usize,
    rotated: Instant,
    every: Duration,
    key: (u64, u64),
    pub inserted: u64,
    pub skipped: u64,
}

impl SeenFilter {
    pub fn new(bits: u64, k: u32, every: Duration) -> Self {
        let words = bits.div_ceil(64).max(1) as usize;
        Self {
            gens: [vec![0; words], vec![0; words]],
            bits: words as u64 * 64,
            k: k.max(1),
            cur: 0,
            rotated: Instant::now(),
            every,
            key: (rand::random(), rand::random::<u64>() | 1),
            inserted: 0,
            skipped: 0,
        }
    }

    fn positions(&self, h: &[u8; 20]) -> impl Iterator<Item = u64> + use<> {
        let a = u64::from_le_bytes(h[0..8].try_into().expect("8 bytes")) ^ self.key.0;
        let b = (u64::from_le_bytes(h[8..16].try_into().expect("8 bytes")) ^ self.key.1) | 1;
        let bits = self.bits;
        (0..u64::from(self.k)).map(move |i| a.wrapping_add(i.wrapping_mul(b)) % bits)
    }

    fn contains_in(&self, g: usize, h: &[u8; 20]) -> bool {
        self.positions(h)
            .all(|p| self.gens[g][(p / 64) as usize] & (1 << (p % 64)) != 0)
    }

    /// True when `h` was not seen in either generation (and records it).
    pub fn check_and_insert(&mut self, h: &[u8; 20], now: Instant) -> bool {
        if now.saturating_duration_since(self.rotated) >= self.every {
            self.cur ^= 1;
            self.gens[self.cur].fill(0);
            self.rotated = now;
        }
        if self.contains_in(0, h) || self.contains_in(1, h) {
            self.skipped += 1;
            return false;
        }
        let cur = self.cur;
        for p in self.positions(h).collect::<Vec<_>>() {
            self.gens[cur][(p / 64) as usize] |= 1 << (p % 64);
        }
        self.inserted += 1;
        true
    }

    pub fn bytes(&self) -> usize {
        self.gens.iter().map(|g| g.len() * 8).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupes_and_rotates() {
        let mut f = SeenFilter::new(1 << 20, 7, Duration::from_secs(10));
        let t = Instant::now();
        let a: [u8; 20] = rand::random();
        assert!(f.check_and_insert(&a, t));
        assert!(!f.check_and_insert(&a, t));
        // After one rotation the old generation still holds it.
        let t1 = t + Duration::from_secs(10);
        assert!(!f.check_and_insert(&a, t1));
        // After two rotations it is forgotten.
        let t2 = t1 + Duration::from_secs(10);
        assert!(f.check_and_insert(&a, t2));
    }

    #[test]
    fn false_positive_rate_is_low() {
        // 1 Mbit, 70 k entries ≈ the 96 Mbit / 6.7 M design ratio: ~1 % expected.
        let mut f = SeenFilter::new(1 << 20, 7, ROTATE_EVERY);
        let t = Instant::now();
        for _ in 0..70_000 {
            f.check_and_insert(&rand::random(), t);
        }
        let fp = (0..20_000)
            .filter(|_| !f.check_and_insert(&rand::random(), t))
            .count();
        assert!(fp < 600, "{fp} false positives of 20000");
        assert_eq!(f.bytes(), 2 * (1 << 20) / 8);
    }
}
