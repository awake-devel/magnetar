//! Bounds-checked big-endian byte cursor and MSB-first bit reader. Every read returns `None`
//! past the end instead of panicking.

/// Big-endian byte cursor.
pub(crate) struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    pub fn new(b: &'a [u8]) -> Cur<'a> {
        Cur { b, p: 0 }
    }
    pub fn left(&self) -> usize {
        self.b.len().saturating_sub(self.p)
    }
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let e = self.p.checked_add(n)?;
        let s = self.b.get(self.p..e)?;
        self.p = e;
        Some(s)
    }
    pub fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }
    pub fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }
    pub fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }
    pub fn u24(&mut self) -> Option<u32> {
        let s = self.take(3)?;
        Some(s.iter().fold(0u32, |a, &x| (a << 8) | u32::from(x)))
    }
    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    pub fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
    pub fn le16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    pub fn le32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
}

/// MSB-first bit reader.
pub(crate) struct Bits<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Bits<'a> {
    pub fn new(b: &'a [u8]) -> Bits<'a> {
        Bits { b, p: 0 }
    }
    pub fn left(&self) -> usize {
        (self.b.len().saturating_mul(8)).saturating_sub(self.p)
    }
    pub fn bit(&mut self) -> Option<u32> {
        let byte = *self.b.get(self.p / 8)?;
        let v = (byte >> (7 - (self.p % 8))) & 1;
        self.p += 1;
        Some(u32::from(v))
    }
    pub fn flag(&mut self) -> Option<bool> {
        self.bit().map(|b| b == 1)
    }
    /// Up to 32 bits.
    pub fn u(&mut self, n: u32) -> Option<u32> {
        if n > 32 || self.left() < n as usize {
            return None;
        }
        let mut v = 0u64;
        for _ in 0..n {
            v = (v << 1) | u64::from(self.bit()?);
        }
        u32::try_from(v).ok()
    }
    pub fn skip(&mut self, n: usize) -> Option<()> {
        if self.left() < n {
            return None;
        }
        self.p += n;
        Some(())
    }
    /// Unsigned Exp-Golomb.
    pub fn ue(&mut self) -> Option<u32> {
        let mut z = 0u32;
        while self.bit()? == 0 {
            z += 1;
            if z > 31 {
                return None;
            }
        }
        let rest = self.u(z)?;
        u32::try_from((1u64 << z) - 1 + u64::from(rest)).ok()
    }
    /// Signed Exp-Golomb.
    pub fn se(&mut self) -> Option<i32> {
        let k = i64::from(self.ue()?);
        i32::try_from(if k & 1 == 1 { (k + 1) / 2 } else { -(k / 2) }).ok()
    }
}

/// H.264 / HEVC RBSP: removes emulation-prevention bytes (`00 00 03`), at most `cap` bytes.
pub(crate) fn unescape(nal: &[u8], cap: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len().min(cap));
    let mut zeros = 0u32;
    for &x in nal {
        if out.len() >= cap {
            break;
        }
        if zeros >= 2 && x == 3 {
            zeros = 0;
            continue;
        }
        zeros = if x == 0 { zeros + 1 } else { 0 };
        out.push(x);
    }
    out
}

/// AV1 / MP4 descriptor style little-endian base-128 (`leb128`), at most 8 bytes.
pub(crate) fn leb128(b: &[u8], p: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for i in 0..8 {
        let x = *b.get(*p)?;
        *p += 1;
        v |= u64::from(x & 0x7f) << (7 * i);
        if x & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_and_golomb() {
        let mut b = Bits::new(&[0b1010_0110, 0b0100_0000]);
        assert_eq!(b.u(3), Some(0b101));
        // 0 0 1 | 1 0 -> ue = 3 + 2 = 5 ; then 0 1 | 0 -> ue = 1
        assert_eq!(b.ue(), Some(5));
        assert_eq!(b.ue(), Some(1));
        assert_eq!(b.u(32), None);
    }

    #[test]
    fn unescape_drops_epb() {
        assert_eq!(unescape(&[0, 0, 3, 1, 0, 0, 3], 64), vec![0, 0, 1, 0, 0]);
    }
}
