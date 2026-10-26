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
        let len = self.int_until(b':')?;
        let len = usize::try_from(len).map_err(|_| format!("negative length {len}"))?;
        let end = self
            .pos
            .checked_add(len)
            .filter(|&e| e <= self.buf.len())
            .ok_or_else(|| format!("string of {len} bytes overruns input"))?;
        let s = self.buf[self.pos..end].to_vec();
        self.pos = end;
        Ok(s)
    }

    /// Decimal integer terminated by `term`. Leading zeros and `-0` are tolerated.
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
