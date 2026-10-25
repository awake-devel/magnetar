//! NIP-13 proof of work (D19). Owner: nostr agent.

/// Leading zero bits of a 32-byte id.
pub fn leading_zero_bits(id: &[u8; 32]) -> u8 {
    let mut n = 0u32;
    for b in id {
        if *b == 0 {
            n += 8;
        } else {
            n += b.leading_zeros();
            break;
        }
    }
    // An all-zero id has 256 bits, which does not fit a u8.
    n.min(255) as u8
}

/// (effective_bits, committed_target): effective = min(actual, committed) when a nonce tag commits.
pub fn effective_pow(id_hex: &str, tags: &[Vec<String>]) -> (u8, Option<u8>) {
    let mut id = [0u8; 32];
    if hex::decode_to_slice(id_hex, &mut id).is_err() {
        return (0, None);
    }
    let actual = leading_zero_bits(&id);
    let committed = tags
        .iter()
        .find(|t| t.first().map(String::as_str) == Some("nonce"))
        .and_then(|t| t.get(2))
        .and_then(|s| s.trim().parse::<u8>().ok());
    match committed {
        Some(c) => (actual.min(c), Some(c)),
        None => (actual, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(v: &[&[&str]]) -> Vec<Vec<String>> {
        v.iter()
            .map(|t| t.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    #[test]
    fn leading_zeros() {
        let mut id = [0xffu8; 32];
        assert_eq!(leading_zero_bits(&id), 0);
        id[0] = 0;
        id[1] = 0x2f;
        assert_eq!(leading_zero_bits(&id), 10);
        assert_eq!(leading_zero_bits(&[0u8; 32]), 255);
        // NIP-13 example: 000000000e9d97a1… = 36 bits.
        let (bits, committed) = effective_pow(
            "000000000e9d97a1ab09fc381030b346cdd7a142ad57e6df0b46dc9bef6c7e2d",
            &[],
        );
        assert_eq!((bits, committed), (36, None));
    }

    #[test]
    fn committed_target_caps_bits() {
        let id = "0000000095014be2da27c57d0632d1e6015cba4a0d055a8c6dd2f20d6af6902b";
        assert_eq!(
            effective_pow(id, &tags(&[&["nonce", "7", "21"]])),
            (21, Some(21))
        );
        assert_eq!(
            effective_pow(id, &tags(&[&["nonce", "7", "40"]])),
            (32, Some(40))
        );
        // No or unparsable commitment: actual bits.
        assert_eq!(effective_pow(id, &tags(&[&["nonce", "7"]])), (32, None));
        assert_eq!(
            effective_pow(id, &tags(&[&["nonce", "7", "x"]])),
            (32, None)
        );
        assert_eq!(effective_pow("zz", &[]), (0, None));
    }
}
