//! Xbox: 360 STFS packages and XEX executables, One/Series XVD, original XBE and XDVDFS
//! images. UNVERIFIED (no local samples): layouts from the Free60 and xboxdevwiki docs.

use crate::formats::console::{Content, GameInfo, Kind, Platform, with_game};
use crate::types::Hit;
use crate::util::*;
use crate::view::View;

/// Known STFS content types (savegame, marketplace, GoD 0x7000, arcade 0xD0000, installer ...).
const STFS_TYPES: [u32; 31] = [
    0x1, 0x2, 0x3, 0x1000, 0x2000, 0x4000, 0x5000, 0x7000, 0x9000, 0x10000, 0x20000, 0x30000,
    0x40000, 0x50000, 0x60000, 0x70000, 0x80000, 0x90000, 0xA0000, 0xB0000, 0xC0000, 0xD0000,
    0xE0000, 0xF0000, 0x100000, 0x200000, 0x300000, 0x400000, 0x500000, 0x600000, 0x2000000,
];

/// Xbox 360 STFS (CON / LIVE / PIRS): be32 content type at 0x344, be32 title ID at 0x360, display
/// name UTF-16BE at 0x411, title name at 0x1691.
pub(crate) fn stfs(v: &View) -> Option<Hit> {
    let h = v.head();
    let con = at(h, 0, b"CON ");
    if !(con || at(h, 0, b"LIVE") || at(h, 0, b"PIRS")) || h.len() < 0x400 {
        return None;
    }
    let mut hit = Hit::new("game.x360.stfs", if con { 70 } else { 80 });
    let mut g = GameInfo::new(Platform::Xbox360, Kind::Package, "STFS");
    let ty = u32be(h, 0x344);
    let title_id = u32be(h, 0x360).unwrap_or(0);
    if title_id != 0 && ty.is_some_and(|t| STFS_TYPES.contains(&t)) {
        hit.conf = 95;
        g.title_id = Some(format!("{title_id:08X}"));
        g.title = utf16_field(h, 0x1691, 0x80, true).or_else(|| utf16_field(h, 0x411, 0x80, true));
        if let Some(t) = ty {
            hit.push("content_type", format!("{t:#x}"));
            // Free60 content types: GoD 0x7000, installed 0x4000, arcade 0xD0000, title
            // update installer 0xB0000, marketplace (DLC) 0x2, demo 0x80000, theme 0x30000.
            g.content = match t {
                0x4000 | 0x5000 | 0x7000 | 0xA0000 | 0xD0000 | 0x2000000 => Some(Content::Game),
                0x2 => Some(Content::Dlc),
                0xB0000 => Some(Content::Patch),
                0x80000 => Some(Content::Demo),
                0x30000 => Some(Content::Theme),
                0xF0000 => Some(Content::License),
                _ => None,
            };
        }
        if g.title.is_none() && h.len() < 0x1711 && v.size() >= 0x1711 {
            hit = hit.need(0, 0x1711);
        }
    }
    Some(with_game(hit, g))
}

/// Xbox One / Series XVD / XVC: "msft-xvd" at 0x200. Encrypted: no title here.
pub(crate) fn xvd(v: &View) -> Option<Hit> {
    at(v.head(), 0x200, b"msft-xvd").then(|| {
        with_game(Hit::new("game.xbox_one.xvd", 96), {
            let mut g = GameInfo::new(Platform::XboxOne, Kind::Package, "XVD");
            g.encrypted = Some(true);
            g
        })
    })
}

/// Original Xbox XBE certificate (title ID at +8, UTF-16LE title of 40 characters at +0xC,
/// region flags at +0xA0, version at +0xAC).
fn xbe_cert(c: &[u8], g: &mut GameInfo) {
    let tid = u32le(c, 8).filter(|&t| t != 0);
    g.title_id = tid.map(|t| format!("{t:08X}"));
    g.title = utf16_field(c, 0xC, 80, false);
    g.region = u32le(c, 0xA0).and_then(|r| match r & 7 {
        1 => Some("Americas"),
        2 => Some("Japan"),
        4 => Some("Europe/PAL"),
        0 => None,
        _ => Some("World"),
    });
    g.version = u32le(c, 0xAC).map(|x| x.to_string());
    // Publisher: the two ASCII letters of the title ID's high half ("MS" = Microsoft).
    if let Some(t) = tid {
        let hi = ((t >> 24) as u8, (t >> 16) as u8);
        if hi.0.is_ascii_uppercase() && hi.1.is_ascii_uppercase() {
            g.maker = Some(format!("{}{}", char::from(hi.0), char::from(hi.1)));
        }
    }
    g.content = Some(Content::Game);
}

/// Original Xbox XBE (default.xbe in folder dumps): "XBEH", le32 base 0x10000 at 0x104, le32
/// certificate address at 0x118.
pub(crate) fn xbe(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"XBEH") || u32le(h, 0x104)? != 0x10000 {
        return None;
    }
    let mut g = GameInfo::new(Platform::Xbox, Kind::Metadata, "XBE");
    let mut hit = Hit::new("game.xbox.xbe", 92);
    if let Some(cert) = u32le(h, 0x118).and_then(|c| c.checked_sub(0x10000)) {
        let cert = u64::from(cert);
        match v.at(cert, 0x1D0).or_else(|| v.at(cert, 0x5C)) {
            Some(c) => xbe_cert(c, &mut g),
            None if cert < v.size() => hit = hit.need(cert, 0x1D0),
            None => {}
        }
    }
    Some(with_game(hit, g))
}

/// Xbox 360 XEX2 (default.xex): be32 module flags at 4, PE offset at 8, optional header count
/// at 0x14, then (key, value) pairs; key 0x00040006 points at the execution info: media ID,
/// version, base version, title ID, platform, type, disc number, disc count.
fn xex_info(b: &[u8], g: &mut GameInfo, hit: &mut Hit) -> Option<u64> {
    if !at(b, 0, b"XEX2") {
        return None;
    }
    let n = u32be(b, 0x14)?.min(64);
    for i in 0..us(u64::from(n)) {
        let o = 0x18 + i * 8;
        let (Some(key), Some(val)) = (u32be(b, o), u32be(b, o + 4)) else {
            break;
        };
        if key != 0x0004_0006 {
            continue;
        }
        let e = us(u64::from(val));
        let Some(ex) = b.get(e..e.checked_add(0x18)?) else {
            return Some(u64::from(val));
        };
        let tid = u32be(ex, 0xC).filter(|&t| t != 0)?;
        g.title_id = Some(format!("{tid:08X}"));
        if let Some(ver) = u32be(ex, 4) {
            g.version = Some(format!(
                "{}.{}.{}.{}",
                ver >> 28,
                (ver >> 24) & 0xF,
                (ver >> 8) & 0xFFFF,
                ver & 0xFF
            ));
        }
        if let Some(m) = u32be(ex, 0) {
            hit.push("media_id", format!("{m:08X}"));
        }
        if let (Some(d), Some(c)) = (u8at(ex, 0x12), u8at(ex, 0x13))
            && c > 1
        {
            hit.push("disc", format!("{d}/{c}"));
        }
        let hi = ((tid >> 24) as u8, (tid >> 16) as u8);
        if hi.0.is_ascii_uppercase() && hi.1.is_ascii_uppercase() {
            g.maker = Some(format!("{}{}", char::from(hi.0), char::from(hi.1)));
        }
        g.content = Some(Content::Game);
        return None;
    }
    None
}

/// A loose XEX2 (Xbox 360 executable, `default.xex` of folder dumps).
pub(crate) fn xex(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"XEX2") {
        return None;
    }
    let n = u32be(h, 0x14)?;
    let pe = u32be(h, 8)?;
    if n == 0 || n > 64 || u64::from(pe) > v.size() {
        return None;
    }
    let mut g = GameInfo::new(Platform::Xbox360, Kind::Metadata, "XEX");
    let mut hit = Hit::new("game.x360.xex", 93);
    if let Some(off) = xex_info(h, &mut g, &mut hit)
        && off < v.size()
    {
        hit = hit.need(off, 0x18);
    }
    if g.title_id.is_some() {
        hit.conf = 96;
    }
    Some(with_game(hit, g))
}

/// XDVDFS: "MICROSOFT*XBOX*MEDIA" at 0x10000 (and again at +0x7EC), le32 root directory sector
/// at +0x14 and size at +0x18. The directory is a binary tree of entries (le16 left, le16
/// right in dwords, le32 sector, le32 size, u8 attributes, u8 name length, name);
/// `default.xbe` (Xbox) or `default.xex` (Xbox 360) gives the title ID. Redump Xbox 360
/// images keep the game partition deeper (0xFD90000 / 0x2080000), after a video partition:
/// not handled.
pub(crate) fn xiso(v: &View) -> Option<Hit> {
    let vd = v.at(0x10000, 0x20)?;
    if !at(vd, 0, b"MICROSOFT*XBOX*MEDIA") {
        return None;
    }
    let mut hit = Hit::new("game.xbox.xiso", 96);
    let mut g = GameInfo::new(Platform::Xbox, Kind::Image, "XISO");
    let root = u64::from(u32le(vd, 0x14).unwrap_or(0)).saturating_mul(2048);
    let root_len = u64::from(u32le(vd, 0x18).unwrap_or(0)).min(64 * 1024);
    if root == 0 || root_len == 0 || root >= v.size() {
        return Some(with_game(hit, g));
    }
    let Some(dir) = v.at(root, us(root_len)) else {
        return Some(with_game(hit.need(root, root_len), g));
    };
    let mut stack = vec![0usize];
    let mut seen = 0;
    let mut found: Option<(bool, u64, u64)> = None;
    while let Some(o) = stack.pop() {
        seen += 1;
        if seen > 4096 {
            break;
        }
        let (Some(l), Some(r), Some(sec), Some(size), Some(nl)) = (
            u16le(dir, o),
            u16le(dir, o + 2),
            u32le(dir, o + 4),
            u32le(dir, o + 8),
            u8at(dir, o + 13),
        ) else {
            continue;
        };
        if l == 0xFFFF && r == 0xFFFF {
            continue;
        }
        let name = String::from_utf8_lossy(sln(dir, o + 14, usize::from(nl))).to_ascii_lowercase();
        if name == "default.xbe" || name == "default.xex" {
            found = Some((
                name.ends_with("xex"),
                u64::from(sec) * 2048,
                u64::from(size),
            ));
            break;
        }
        for c in [l, r] {
            if c != 0 && c != 0xFFFF {
                stack.push(usize::from(c) * 4);
            }
        }
    }
    let Some((x360, at_, size)) = found else {
        return Some(with_game(hit, g));
    };
    if x360 {
        g.platform = Platform::Xbox360;
        hit.relabel("game.x360.iso");
    }
    hit.push(
        "executable",
        if x360 { "default.xex" } else { "default.xbe" },
    );
    let want = size.clamp(0x200, 0x3000);
    let Some(exe) = v.at(at_, us(want)) else {
        return Some(with_game(hit.need(at_, want), g));
    };
    if x360 {
        if let Some(off) = xex_info(exe, &mut g, &mut hit) {
            hit = hit.need(at_.saturating_add(off), 0x18);
        }
    } else if at(exe, 0, b"XBEH")
        && let Some(cert) = u32le(exe, 0x118).and_then(|c| c.checked_sub(u32le(exe, 0x104)?))
    {
        let cert = us(u64::from(cert));
        match exe.get(cert..) {
            Some(c) if c.len() >= 0x5C => xbe_cert(c, &mut g),
            _ => hit = hit.need(at_.saturating_add(cert as u64), 0x1D0),
        }
    }
    g.content = Some(Content::Game);
    Some(with_game(hit, g))
}

/// A zero-filled head (extract-xiso images start with 64 KiB of zeros): asks for the XDVDFS
/// volume descriptor at 0x10000 in the same round as the ISO 9660 descriptors that
/// `disc.rs` asks for, so that the title ID (root directory, then default.xbe) is reached
/// within the default three rounds. Ranks just above disc.rs's own zero-head request.
pub(crate) fn zero_head_xdvdfs(v: &View) -> Option<Hit> {
    let h = v.head();
    if v.size() < 0x10800 || h.len() < 2048 || v.has(0x10000, 20) {
        return None;
    }
    if !pre(h, 0x8000).iter().all(|&b| b == 0) {
        return None;
    }
    let mut hit =
        Hit::new("misc.zeros", 41).d("note", "zero-filled head: asking for the disc descriptors");
    if !v.has(0x8000, 0x800) {
        hit = hit.need(0x8000, 0x2000);
    }
    Some(hit.need(0x10000, 0x800))
}
