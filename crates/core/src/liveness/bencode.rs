//! Lenient bencode decoder/encoder (accepts unsorted dict keys).
//!
//! Real trackers emit dicts with unsorted keys (nyaa: `complete, incomplete, downloaded`), so the
//! decoder keeps keys in wire order and lookups are linear. The encoder sorts keys (canonical form).

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(Vec<(Vec<u8>, Value)>),
}

/// Nesting limit: real messages are 3–4 levels deep; this keeps hostile input off the stack.
const MAX_DEPTH: usize = 64;

impl Value {
    /// First value under `key` in a dict (None for non-dicts).
    pub fn get(&self, key: &[u8]) -> Option<&Value> {
        match self {
            Value::Dict(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&[(Vec<u8>, Value)]> {
        match self {
            Value::Dict(d) => Some(d),
            _ => None,
        }
    }
}

/// Decode one value. Trailing ASCII whitespace is tolerated (some HTTP trackers append `\n`),
/// any other trailing byte is an error.
pub fn decode(input: &[u8]) -> Result<Value, String> {
    let mut p = Parser { buf: input, pos: 0 };
    let v = p.value(0)?;
    if input[p.pos..].iter().any(|b| !b.is_ascii_whitespace()) {
        return Err(format!("trailing data at offset {}", p.pos));
    }
    Ok(v)
}

/// Decode one value at the start of `input` and return it with the number of bytes it took;
/// whatever follows is left to the caller (BEP 9 `data` messages append the metadata block
/// after the bencoded dict).
pub fn decode_prefix(input: &[u8]) -> Result<(Value, usize), String> {
    let mut p = Parser { buf: input, pos: 0 };
    let v = p.value(0)?;
    Ok((v, p.pos))
}

/// Number of values (dict keys included) a decode of `input` would build, counted without
/// allocating; stops once it passes `max` (returns `max + 1`). Used to price a decode before
/// running it: the `Value` tree costs tens of bytes per value, so 2-byte values (`le`, `0:`)
/// amplify memory ~30×.
///
/// Anything the counter cannot read as a value (a stray byte, a signed or junk string length,
/// an unterminated integer) also returns `max + 1` (D110): stopping early instead would let a
/// prefix the decoder still accepts (`-0:` keys, before D110) hide the rest of the input from
/// the price. Trailing ASCII whitespace, which [`decode`] tolerates, is skipped.
pub fn count_values(input: &[u8], max: usize) -> usize {
    let over = max.saturating_add(1);
    let mut pos = 0usize;
    let mut n = 0usize;
    while pos < input.len() && n <= max {
        match input[pos] {
            b'i' => {
                n += 1;
                match input[pos..].iter().position(|&b| b == b'e') {
                    Some(r) => pos += r + 1,
                    None => return over,
                }
            }
            b'l' | b'd' => {
                n += 1;
                pos += 1;
            }
            b'e' => pos += 1,
            b'0'..=b'9' => {
                n += 1;
                let Some(len) = string_length(&input[pos..]) else {
                    return over;
                };
                match (pos + len.header).checked_add(len.body) {
                    Some(end) => pos = end,
                    None => return over,
                }
            }
            b if b.is_ascii_whitespace() => pos += 1,
            _ => return over,
        }
    }
    n.min(over)
}

/// Longest string-length prefix we read (`usize::MAX` has 20 digits): a longer run of digits
/// is junk, and the bound keeps the colon search short.
const MAX_LENGTH_DIGITS: usize = 20;

/// A byte-string header (`<digits>:`), as the decoder reads it.
struct StringLength {
    /// Bytes of the header, colon included.
    header: usize,
    /// The string's length.
    body: usize,
}

/// Parse the `<digits>:` header at the start of `input`. Digits only (leading zeros tolerated,
/// as real trackers send them); a sign, a space or any other byte is rejected (D110: `-0:`
/// used to decode as an empty string while [`count_values`] stopped counting there). The
/// decoder and the counter share this, so they cannot disagree on what a string is.
fn string_length(input: &[u8]) -> Option<StringLength> {
    let colon = input
        .iter()
        .take(MAX_LENGTH_DIGITS + 1)
        .position(|&b| b == b':')?;
    let digits = &input[..colon];
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let body = std::str::from_utf8(digits).ok()?.parse::<usize>().ok()?;
    Some(StringLength {
        header: colon + 1,
        body,
    })
}

pub fn encode(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(v, &mut out);
    out
}

fn encode_into(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Int(i) => {
            out.push(b'i');
            out.extend_from_slice(i.to_string().as_bytes());
            out.push(b'e');
        }
        Value::Bytes(b) => encode_bytes(b, out),
        Value::List(l) => {
            out.push(b'l');
            for item in l {
                encode_into(item, out);
            }
            out.push(b'e');
        }
        Value::Dict(d) => {
            let mut entries: Vec<_> = d.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            out.push(b'd');
            for (k, v) in entries {
                encode_bytes(k, out);
                encode_into(v, out);
            }
            out.push(b'e');
        }
    }
}

fn encode_bytes(b: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(b.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(b);
}

struct Parser<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Result<u8, String> {
        self.buf
            .get(self.pos)
            .copied()
            .ok_or_else(|| "unexpected end of input".to_string())
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err("nesting too deep".into());
        }
        match self.peek()? {
            b'i' => {
                self.pos += 1;
                Ok(Value::Int(self.int_until(b'e')?))
            }
            b'l' => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.peek()? != b'e' {
                    items.push(self.value(depth + 1)?);
                }
                self.pos += 1;
                Ok(Value::List(items))
            }
            b'd' => {
                self.pos += 1;
                let mut entries = Vec::new();
                while self.peek()? != b'e' {
                    let k = self.byte_string()?;
                    let v = self.value(depth + 1)?;
                    entries.push((k, v));
                }
                self.pos += 1;
                Ok(Value::Dict(entries))
            }
            b'0'..=b'9' => Ok(Value::Bytes(self.byte_string()?)),
            c => Err(format!("unexpected byte {c:#04x} at offset {}", self.pos)),
        }
    }

    fn byte_string(&mut self) -> Result<Vec<u8>, String> {
        let start = self.pos;
        let StringLength { header, body: len } = string_length(&self.buf[start..])
            .ok_or_else(|| format!("bad string length at offset {start}"))?;
        self.pos = start + header;
        let end = self
            .pos
            .checked_add(len)
            .filter(|&e| e <= self.buf.len())
            .ok_or_else(|| format!("string of {len} bytes overruns input"))?;
        let s = self.buf[self.pos..end].to_vec();
        self.pos = end;
        Ok(s)
    }

    /// Decimal integer terminated by `term` (`i…e`). Leading zeros and `-0` are tolerated.
    /// String lengths do not come here: [`string_length`] takes digits only.
    fn int_until(&mut self, term: u8) -> Result<i64, String> {
        let start = self.pos;
        let rel = self.buf[start..]
            .iter()
            .take(21)
            .position(|&b| b == term)
            .ok_or_else(|| format!("unterminated integer at offset {start}"))?;
        let digits = &self.buf[start..start + rel];
        let unsigned = digits.strip_prefix(b"-").unwrap_or(digits);
        if unsigned.is_empty() || !unsigned.iter().all(u8::is_ascii_digit) {
            return Err(format!("bad integer at offset {start}"));
        }
        let n = std::str::from_utf8(digits)
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .ok_or_else(|| format!("integer out of range at offset {start}"))?;
        self.pos = start + rel + 1;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_or_junk_string_lengths_are_rejected() {
        // D110: `-0:` was an empty key to the decoder and a stop for the counter.
        for bad in [
            &b"d-0:lee"[..],
            b"d-0:0:e",
            b"-0:",
            b"d+1:ai1ee",
            b"d 1:ai1ee",
            b"d1 :ai1ee",
            b"l:e",
        ] {
            assert!(decode(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        // Leading zeros stay tolerated (lenient decoder), and so does trailing whitespace.
        assert_eq!(decode(b"01:a").unwrap(), Value::Bytes(b"a".to_vec()));
        assert_eq!(decode(b"1:a\n").unwrap(), Value::Bytes(b"a".to_vec()));
        // Signed integers are still integers.
        assert_eq!(decode(b"i-3e").unwrap(), Value::Int(-3));
    }

    #[test]
    fn malformed_input_counts_past_the_max() {
        // The audit's shape: one dict whose `-0:` keys hide 100k entries from the price.
        let mut hostile = b"d".to_vec();
        for _ in 0..100_000 {
            hostile.extend_from_slice(b"-0:le");
        }
        hostile.push(b'e');
        assert_eq!(count_values(&hostile, 1000), 1001);
        assert!(decode(&hostile).is_err());
        // Anything the counter cannot read is priced as too much, never as a short prefix.
        for bad in [
            &b"d1:a?e"[..],
            b"i12",
            b"99999999999999999999999:x",
            b"1x:a",
        ] {
            assert_eq!(
                count_values(bad, 10),
                11,
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
        // Well-formed input still counts exactly; trailing whitespace is not malformed.
        assert_eq!(count_values(b"d1:ai1e1:b2:xye", 100), 5);
        assert_eq!(count_values(b"d1:ai1ee\r\n", 100), 3);
    }
}
