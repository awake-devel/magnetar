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
    n as u8
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
