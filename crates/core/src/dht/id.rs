//! Node ids: XOR metric helpers and BEP 42 ("DHT Security extension") id restriction.
//!
//! BEP 42 (bittorrent.org/beps/bep_0042.rst, read 2026-10-26): the id prefix is
//! `crc32c((ip & 0x030f3fff) | (r << 29))`, `r` in [0, 7]; the first 21 bits of the id must match
//! the CRC and the last id byte is the random byte whose low 3 bits are `r`. The BEP's prose says
//! to hash 8 bytes (a big-endian 64-bit integer), but its reference code hashes `num_octets` = 4
//! for IPv4 and its five test vectors only match 4 bytes (docs/DHT.md §3.3, D54): we hash the 4
//! big-endian bytes. CRC32C (Castagnoli) is implemented here (a 256-entry table, checked against
//! the standard check value and the BEP vectors) instead of adding the `crc32c` crate (D59).

use std::net::Ipv4Addr;

pub type NodeId = [u8; 20];

/// Reflected Castagnoli polynomial (CRC-32C, iSCSI).
const CRC32C_POLY: u32 = 0x82F6_3B78;

const fn crc32c_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ CRC32C_POLY
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static CRC32C_TABLE: [u32; 256] = crc32c_table();

/// CRC-32C of `data` (init and final xor 0xFFFFFFFF, as in iSCSI / SSE 4.2 `crc32`).
pub fn crc32c(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc = CRC32C_TABLE[((crc ^ u32::from(b)) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

/// The CRC32C a BEP 42 id for `ip` with random value `r` (low 3 bits used) must start with.
pub fn bep42_crc(ip: Ipv4Addr, r: u8) -> u32 {
    let masked = (u32::from(ip) & 0x030f_3fff) | (u32::from(r & 7) << 29);
    crc32c(&masked.to_be_bytes())
}

/// A BEP 42 id for `ip`: 21-bit CRC prefix, random middle, last byte `rand` (its low 3 bits are
/// `r`). `fill` provides the 17 free random bytes (byte 2's low 3 bits and bytes 3–18).
pub fn bep42_id(ip: Ipv4Addr, rand: u8, fill: [u8; 17]) -> NodeId {
    let crc = bep42_crc(ip, rand & 7);
    let mut id = [0u8; 20];
    id[0] = (crc >> 24) as u8;
    id[1] = (crc >> 16) as u8;
    id[2] = ((crc >> 8) as u8 & 0xf8) | (fill[0] & 0x07);
    id[3..19].copy_from_slice(&fill[1..17]);
    id[19] = rand;
    id
}

/// Whether `id` satisfies BEP 42 for `ip` (21-bit prefix of the CRC with `r = id[19] & 7`).
pub fn bep42_valid(ip: Ipv4Addr, id: &NodeId) -> bool {
    let crc = bep42_crc(ip, id[19] & 7);
    id[0] == (crc >> 24) as u8
        && id[1] == (crc >> 16) as u8
        && id[2] & 0xf8 == (crc >> 8) as u8 & 0xf8
}

/// The 21-bit prefix (as the top bits of a u32) BEP 42 gives `ip` for each `r` in 0..8.
fn prefixes(ip: Ipv4Addr) -> [u32; 8] {
    std::array::from_fn(|r| bep42_crc(ip, r as u8) & 0x00ff_fff800)
}

/// The `n` values of `r` (≤ 8) whose prefixes are farthest apart (largest minimum pairwise XOR),
/// so `n` virtual nodes on one IP cover distant parts of the keyspace (D54). Ties go to the
/// lexicographically smallest set, so the choice is stable.
pub fn spread_r_values(ip: Ipv4Addr, n: usize) -> Vec<u8> {
    let n = n.clamp(1, 8);
    let p = prefixes(ip);
    let mut best: (u32, Vec<u8>) = (0, (0..n as u8).collect());
    let mut found = false;
    for mask in 0u16..256 {
        if mask.count_ones() as usize != n {
            continue;
        }
        let set: Vec<u8> = (0..8u8).filter(|r| mask & (1 << r) != 0).collect();
        let mut min = u32::MAX;
        for (i, a) in set.iter().enumerate() {
            for b in &set[i + 1..] {
                min = min.min(p[*a as usize] ^ p[*b as usize]);
            }
        }
        if !found || min > best.0 || (min == best.0 && set < best.1) {
            best = (min, set);
            found = true;
        }
    }
    best.1
}

/// XOR distance.
pub fn distance(a: &NodeId, b: &NodeId) -> NodeId {
    std::array::from_fn(|i| a[i] ^ b[i])
}

/// Number of leading bits `a` and `b` share (160 when equal).
pub fn common_prefix_bits(a: &NodeId, b: &NodeId) -> u32 {
    let mut bits = 0;
    for (x, y) in a.iter().zip(b) {
        let d = x ^ y;
        bits += d.leading_zeros();
        if d != 0 {
            break;
        }
    }
    bits
}

/// A random id sharing exactly `bits` leading bits with `own` (bit `bits` flipped), i.e. a
/// random point in routing bucket `bits` (BEP 5 bucket refresh). `bits` ≥ 160 returns `own`.
pub fn random_id_in_bucket(own: &NodeId, bits: u32, random: NodeId) -> NodeId {
    if bits >= 160 {
        return *own;
    }
    let mut id = random;
    let full = (bits / 8) as usize;
    id[..full].copy_from_slice(&own[..full]);
    let rem = bits % 8;
    // Keep the top `rem` bits of own's byte, flip the next one, keep random below.
    let keep = if rem == 0 { 0u8 } else { 0xffu8 << (8 - rem) };
    let flip = 0x80u8 >> rem;
    let below = !(keep | flip);
    id[full] = (own[full] & keep) | (!own[full] & flip) | (random[full] & below);
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex20(s: &str) -> NodeId {
        hex::decode(s).unwrap().try_into().unwrap()
    }

    #[test]
    fn crc32c_check_value() {
        // The standard CRC-32C check value (RFC 3720 B.4 / "123456789").
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(&[0u8; 32]), 0x8A91_36AA); // RFC 3720 B.4: 32 bytes of zeros
    }

    /// The five test vectors of BEP 42, hashed over 4 bytes (docs/DHT.md §3.3).
    const VECTORS: [(&str, u8, &str); 5] = [
        (
            "124.31.75.21",
            1,
            "5fbfbff10c5d6a4ec8a88e4c6ab4c28b95eee401",
        ),
        (
            "21.75.31.124",
            86,
            "5a3ce9c14e7a08645677bbd1cfe7d8f956d53256",
        ),
        (
            "65.23.51.170",
            22,
            "a5d43220bc8f112a3d426c84764f8c2a1150e616",
        ),
        (
            "84.124.73.14",
            65,
            "1b0321dd1bb1fe518101ceef99462b947a01ff41",
        ),
        (
            "43.213.53.83",
            90,
            "e56f6cbf5b7c4be0237986d5243b87aa6d51305a",
        ),
    ];

    #[test]
    fn bep42_vectors_match_over_four_bytes() {
        for (ip, rand, id) in VECTORS {
            let ip: Ipv4Addr = ip.parse().unwrap();
            let id = hex20(id);
            assert!(bep42_valid(ip, &id), "{ip}");
            let crc = bep42_crc(ip, rand & 7);
            assert_eq!(
                crc >> 11,
                u32::from_be_bytes([0, id[0], id[1], id[2]]) >> 3,
                "{ip}"
            );
            // Regenerating with the vector's free bytes gives the vector back.
            let mut fill = [0u8; 17];
            fill[0] = id[2] & 7;
            fill[1..].copy_from_slice(&id[3..19]);
            assert_eq!(bep42_id(ip, rand, fill), id, "{ip}");
        }
    }

    #[test]
    fn bep42_prose_eight_bytes_does_not_match() {
        // The BEP's prose (a big-endian 64-bit integer) gives a different prefix for the first
        // vector: 9bf040 instead of 5fbfb8 (docs/DHT.md §3.3).
        let ip: Ipv4Addr = "124.31.75.21".parse().unwrap();
        let masked = u64::from((u32::from(ip) & 0x030f_3fff) | (1 << 29));
        let crc8 = crc32c(&masked.to_be_bytes());
        assert_eq!(crc8 >> 8 & 0x00ff_fff8, 0x9bf040);
        assert_eq!(bep42_crc(ip, 1) >> 8 & 0x00ff_fff8, 0x5fbfb8);
    }

    #[test]
    fn bep42_rejects_wrong_ip_or_r() {
        let ip: Ipv4Addr = "124.31.75.21".parse().unwrap();
        let mut id = hex20(VECTORS[0].2);
        // The mask 0x030f3fff keeps the low byte, so another last octet changes the CRC.
        assert!(!bep42_valid("124.31.75.99".parse().unwrap(), &id));
        // Bits outside the mask (top 6 bits of the first octet) do not matter.
        assert!(bep42_valid("252.31.75.21".parse().unwrap(), &id));
        id[19] = 2; // r = 2 instead of 1
        assert!(!bep42_valid(ip, &id));
    }

    #[test]
    fn spread_picks_distinct_distant_prefixes() {
        let ip: Ipv4Addr = "203.0.113.7".parse().unwrap();
        for n in 1..=8 {
            let rs = spread_r_values(ip, n);
            assert_eq!(rs.len(), n);
            let mut dedup = rs.clone();
            dedup.dedup();
            assert_eq!(dedup.len(), n);
        }
        // 4 of 8: the minimum pairwise distance is at least that of 0..4.
        let p = prefixes(ip);
        let min_of = |s: &[u8]| {
            let mut m = u32::MAX;
            for (i, a) in s.iter().enumerate() {
                for b in &s[i + 1..] {
                    m = m.min(p[*a as usize] ^ p[*b as usize]);
                }
            }
            m
        };
        assert!(min_of(&spread_r_values(ip, 4)) >= min_of(&[0, 1, 2, 3]));
        assert_eq!(spread_r_values(ip, 20).len(), 8);
    }

    #[test]
    fn prefix_and_bucket_ids() {
        let own = [0b1010_1010u8; 20];
        assert_eq!(common_prefix_bits(&own, &own), 160);
        for bits in [0u32, 1, 7, 8, 9, 63, 100, 159] {
            for _ in 0..20 {
                let id = random_id_in_bucket(&own, bits, rand::random());
                assert_eq!(common_prefix_bits(&own, &id), bits, "bits {bits}");
            }
        }
        assert_eq!(random_id_in_bucket(&own, 160, [0; 20]), own);
        assert_eq!(distance(&own, &own), [0; 20]);
    }
}
