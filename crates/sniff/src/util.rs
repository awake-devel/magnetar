//! Bounds-checked byte readers. Every accessor returns `None` (or an empty slice) instead of
//! panicking, so parsers can be written against hostile input without unchecked indexing.

#[inline]
fn arr<const N: usize>(b: &[u8], o: usize) -> Option<[u8; N]> {
    b.get(o..o.checked_add(N)?)?.try_into().ok()
}

#[inline]
pub(crate) fn u8at(b: &[u8], o: usize) -> Option<u8> {
    b.get(o).copied()
}
#[inline]
pub(crate) fn u16le(b: &[u8], o: usize) -> Option<u16> {
    arr(b, o).map(u16::from_le_bytes)
}
#[inline]
pub(crate) fn u16be(b: &[u8], o: usize) -> Option<u16> {
    arr(b, o).map(u16::from_be_bytes)
}
#[inline]
pub(crate) fn u32le(b: &[u8], o: usize) -> Option<u32> {
    arr(b, o).map(u32::from_le_bytes)
}
#[inline]
pub(crate) fn u32be(b: &[u8], o: usize) -> Option<u32> {
    arr(b, o).map(u32::from_be_bytes)
}
#[inline]
pub(crate) fn u64le(b: &[u8], o: usize) -> Option<u64> {
    arr(b, o).map(u64::from_le_bytes)
}
#[inline]
pub(crate) fn u64be(b: &[u8], o: usize) -> Option<u64> {
    arr(b, o).map(u64::from_be_bytes)
}

/// True when `m` sits at `o` in `b`.
#[inline]
pub(crate) fn at(b: &[u8], o: usize, m: &[u8]) -> bool {
    o.checked_add(m.len()).and_then(|e| b.get(o..e)) == Some(m)
}

/// Clamped slice `[a, e)`: never panics, empty when out of range.
#[inline]
pub(crate) fn sl(b: &[u8], a: usize, e: usize) -> &[u8] {
    let e = e.min(b.len());
    b.get(a..e).unwrap_or(&[])
}

/// Clamped slice `[a, a + n)`.
#[inline]
pub(crate) fn sln(b: &[u8], a: usize, n: usize) -> &[u8] {
    sl(b, a, a.saturating_add(n))
}

/// Clamped suffix from `a`.
#[inline]
pub(crate) fn from(b: &[u8], a: usize) -> &[u8] {
    b.get(a..).unwrap_or(&[])
}

/// First `n` bytes (or fewer).
#[inline]
pub(crate) fn pre(b: &[u8], n: usize) -> &[u8] {
    sl(b, 0, n)
}

/// u64 to usize, saturating (offsets taken from the input).
#[inline]
pub(crate) fn us(v: u64) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

/// NUL-terminated, lossy UTF-8, trimmed.
pub(crate) fn cstr(b: &[u8]) -> String {
    let e = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(pre(b, e)).trim().to_string()
}

/// Lossy UTF-8 of the whole slice, trimmed.
pub(crate) fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).trim().to_string()
}

pub(crate) fn printable(b: &[u8]) -> bool {
    b.iter().all(|&c| (0x20..0x7f).contains(&c))
}

pub(crate) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    memchr::memmem::find(hay, needle)
}

pub(crate) fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    memchr::memmem::rfind(hay, needle)
}

pub(crate) fn contains(hay: &[u8], needle: &[u8]) -> bool {
    find(hay, needle).is_some()
}

/// Shannon entropy in bits per byte.
pub(crate) fn entropy(b: &[u8]) -> f64 {
    if b.is_empty() {
        return 0.0;
    }
    let mut c = [0u32; 256];
    for &x in b {
        if let Some(slot) = c.get_mut(usize::from(x)) {
            *slot = slot.saturating_add(1);
        }
    }
    let n = b.len() as f64;
    c.iter()
        .filter(|&&v| v > 0)
        .map(|&v| {
            let p = f64::from(v) / n;
            -p * p.log2()
        })
        .sum()
}

/// Printable ASCII up to the first NUL, trimmed; `None` if empty or holding anything else.
pub(crate) fn ascii_field(b: &[u8], off: usize, len: usize) -> Option<String> {
    let raw = b.get(off..off.checked_add(len)?)?;
    let raw = pre(raw, raw.iter().position(|&c| c == 0).unwrap_or(raw.len()));
    if raw.is_empty() || !printable(raw) {
        return None;
    }
    let s = String::from_utf8_lossy(raw).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

/// UTF-8 up to the first NUL, trimmed, rejecting control characters.
pub(crate) fn text_field(b: &[u8], off: usize, len: usize) -> Option<String> {
    let raw = b.get(off..off.checked_add(len)?)?;
    let raw = pre(raw, raw.iter().position(|&c| c == 0).unwrap_or(raw.len()));
    let s = std::str::from_utf8(raw).ok()?.trim();
    (!s.is_empty() && !s.chars().any(char::is_control)).then(|| s.to_owned())
}

/// UTF-16 up to the first NUL unit, trimmed, rejecting control characters.
pub(crate) fn utf16_field(b: &[u8], off: usize, len: usize, big_endian: bool) -> Option<String> {
    let raw = b.get(off..off.checked_add(len)?)?;
    let units: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| {
            let pair = [
                c.first().copied().unwrap_or(0),
                c.get(1).copied().unwrap_or(0),
            ];
            if big_endian {
                u16::from_be_bytes(pair)
            } else {
                u16::from_le_bytes(pair)
            }
        })
        .take_while(|&u| u != 0)
        .collect();
    let s = String::from_utf16(&units).ok()?.trim().to_owned();
    (!s.is_empty() && !s.chars().any(char::is_control)).then_some(s)
}

/// Lowercased extension of the last path component (at most 6 characters), or "".
pub(crate) fn ext(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((_, e)) if !e.is_empty() && e.len() <= 6 => e.to_ascii_lowercase(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_never_panic() {
        let b = [1u8, 2, 3];
        assert_eq!(u32le(&b, 0), None);
        assert_eq!(u16be(&b, 1), Some(0x0203));
        assert_eq!(u64le(&b, usize::MAX), None);
        assert!(!at(&b, usize::MAX, b"x"));
        assert_eq!(sl(&b, 2, 1), &[] as &[u8]);
        assert_eq!(sln(&b, 1, usize::MAX), &[2, 3]);
        assert_eq!(cstr(b"ab\0cd"), "ab");
    }

    #[test]
    fn extension() {
        assert_eq!(ext("dir.x/Movie.MKV"), "mkv");
        assert_eq!(ext("noext"), "");
        assert_eq!(ext("a.toolongext"), "");
    }
}
