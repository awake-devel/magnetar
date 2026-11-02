//! Images needing structure: PNG/APNG (chunk order), BMP and ICO (size fields), GIF (the block
//! after the colour table), TIFF (the first IFD), and the weak magic-less TGA and PCX headers
//! (reported at low confidence only). Simple magics live in `data/signatures/image.toml`; for
//! GIF and TIFF the rules there only show the short magic (at most [`WEAK`]).

use crate::types::{Hit, WEAK};
use crate::util::*;
use crate::view::View;

pub(crate) static CHECKS: &[(&str, crate::formats::Check)] = &[
    ("png", png),
    ("bmp", bmp),
    ("ico", ico),
    ("gif", gif),
    ("tiff", tiff),
    ("tga_weak", tga_weak),
    ("pcx_weak", pcx_weak),
];

fn png(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    if !at(h, 12, b"IHDR") {
        return Some(Hit::new("image.png", 70));
    }
    let w = u32be(h, 16)?;
    let ht = u32be(h, 20)?;
    // APNG: an acTL chunk before the first IDAT.
    let win = pre(h, 1 << 20);
    let actl = find(win, b"acTL");
    let idat = find(win, b"IDAT");
    let apng =
        matches!((actl, idat), (Some(a), Some(i)) if a < i) || (actl.is_some() && idat.is_none());
    Some(
        Hit::new(if apng { "image.apng" } else { "image.png" }, 98)
            .d("width", w)
            .d("height", ht),
    )
}

fn bmp(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"BM") {
        return None;
    }
    let fsz = u64::from(u32le(h, 2)?);
    let dib = u32le(h, 14)?;
    let off = u32le(h, 10)?;
    if [12, 16, 40, 52, 56, 64, 108, 124].contains(&dib)
        && u64::from(off) >= 14 + u64::from(dib)
        && (fsz == v.size() || fsz == 0 || v.size() == 0)
    {
        return Some(Hit::new("image.bmp", 95));
    }
    if [12, 40, 108, 124].contains(&dib) {
        return Some(Hit::new("image.bmp", 70));
    }
    None
}

fn ico(v: &View) -> Option<Hit> {
    let h = v.head();
    if !(at(h, 0, &[0, 0, 1, 0]) || at(h, 0, &[0, 0, 2, 0])) || h.len() < 22 {
        return None;
    }
    let n = u64::from(u16le(h, 4)?);
    let sz = u64::from(u32le(h, 14)?);
    let off = u64::from(u32le(h, 18)?);
    let end = off.saturating_add(sz);
    if n > 0
        && n < 256
        && u8at(h, 9) == Some(0)
        && off >= 6 + 16 * n
        && (v.size() == 0 || end <= v.size())
    {
        return Some(Hit::new("image.ico", 90).d("images", n));
    }
    None
}

/// TGA has no magic: only reported when nothing else matched (low confidence). The v2 footer is
/// a signature rule.
fn tga_weak(v: &View) -> Option<Hit> {
    let h = v.head();
    if h.len() < 18 {
        return None;
    }
    let (b1, b2, b16, b17) = (u8at(h, 1)?, u8at(h, 2)?, u8at(h, 16)?, u8at(h, 17)?);
    if b1 <= 1
        && matches!(b2, 1 | 2 | 3 | 9 | 10 | 11)
        && matches!(b16, 8 | 15 | 16 | 24 | 32)
        && b17 & 0xC0 == 0
    {
        let w = u16le(h, 12)?;
        let ht = u16le(h, 14)?;
        if w > 0 && ht > 0 && (b1 == 1 || u16le(h, 5) == Some(0)) {
            return Some(Hit::new("image.tga", 45));
        }
    }
    None
}

fn pcx_weak(v: &View) -> Option<Hit> {
    let h = v.head();
    if h.len() >= 128
        && u8at(h, 0) == Some(0x0A)
        && matches!(u8at(h, 1), Some(0 | 2 | 3 | 4 | 5))
        && u8at(h, 2) == Some(1)
        && matches!(u8at(h, 3), Some(1 | 2 | 4 | 8))
        && u8at(h, 64) == Some(0)
    {
        return Some(Hit::new("image.pcx", 50));
    }
    None
}

/// GIF: the logical screen descriptor, then after the global colour table the first block must
/// be an extension with a known label (graphic control, comment, plain text, application), an
/// image descriptor or the trailer.
fn gif(v: &View) -> Option<Hit> {
    let h = v.head();
    if !(at(h, 0, b"GIF87a") || at(h, 0, b"GIF89a")) {
        return None;
    }
    let (w, ht, flags) = (u16le(h, 6)?, u16le(h, 8)?, u8at(h, 10)?);
    let gct = if flags & 0x80 != 0 {
        3usize << ((flags & 7) + 1)
    } else {
        0
    };
    let o = 13 + gct;
    let Some(first) = u8at(h, o) else {
        if (o as u64) < v.size() {
            let mut hit = Hit::new("image.gif", WEAK).need(0, o as u64 + 2);
            // Outranks the bare signature rule at the same confidence so the range is fetched.
            hit.strength = 1000;
            return Some(hit);
        }
        return None;
    };
    let ok = match first {
        0x2C | 0x3B => true,
        0x21 => matches!(u8at(h, o + 1)?, 0x01 | 0xF9 | 0xFE | 0xFF),
        _ => false,
    };
    ok.then(|| {
        Hit::new("image.gif", 97)
            .d("width", u64::from(w))
            .d("height", u64::from(ht))
    })
}

/// Largest IFD entry count read (a real first IFD has a few dozen).
const TIFF_MAX_ENTRIES: u64 = 4096;

/// TIFF (and Canon CR2, "CR\x02" at 8): byte order mark, first IFD offset inside the file and
/// an IFD whose entry count is 1-4096 and whose first entries have field types 1-18 (12 bits of
/// evidence each). An IFD past the known bytes is asked for, at [`WEAK`] until it is read.
fn tiff(v: &View) -> Option<Hit> {
    let h = v.head();
    let le = if at(h, 0, b"II*\0") {
        true
    } else if at(h, 0, b"MM\0*") {
        false
    } else {
        return None;
    };
    let r16 = |b: &[u8], o: usize| if le { u16le(b, o) } else { u16be(b, o) };
    let r32 = |b: &[u8], o: usize| if le { u32le(b, o) } else { u32be(b, o) };
    let label = if at(h, 8, b"CR\x02") {
        "image.raw.cr2"
    } else {
        "image.tiff"
    };
    let ifd = u64::from(r32(h, 4)?);
    if ifd < 8 || ifd.saturating_add(2 + 12) > v.size() {
        return None;
    }
    let Some(n) = v.at(ifd, 2).and_then(|b| r16(b, 0)) else {
        let len = (2 + 12 * 64).min(v.size() - ifd);
        let mut hit = Hit::new(label, WEAK).need(ifd, len);
        // Outranks the bare signature rule at the same confidence so the IFD is fetched.
        hit.strength = 1000;
        return Some(hit);
    };
    let n = u64::from(n);
    if n == 0 || n > TIFF_MAX_ENTRIES || ifd + 2 + 12 * n > v.size() {
        return None;
    }
    let k = n.min(8);
    let Some(e) = v.at(ifd + 2, (12 * k) as usize) else {
        let mut hit = Hit::new(label, WEAK).need(ifd + 2, 12 * k);
        hit.strength = 1000;
        return Some(hit);
    };
    let types_ok =
        (0..k as usize).all(|i| r16(e, i * 12 + 2).is_some_and(|t| (1..=18).contains(&t)));
    types_ok.then(|| {
        Hit::new(label, 97)
            .d("byte_order", if le { "II" } else { "MM" })
            .d("ifd_entries", n)
    })
}
