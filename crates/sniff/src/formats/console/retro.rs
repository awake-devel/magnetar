//! Cartridge ROM headers: Game Boy / Color, Game Boy Advance, Nintendo 64, Super NES, NES.
//! Header checksums are verified where the format has one, so a valid header is a fact (95+)
//! and a bare logo or short magic is not (at most [`WEAK`], shown but not stored). UNVERIFIED
//! on most real dumps: layouts from Pan Docs, GBATEK, n64brew, the SNES development manual
//! (fullsnes) and the NESdev wiki; checked on the homebrew in `~/sniff-samples` (N64, NES, GB).
//!
//! Signatures for systems without a check worth a parser (Mega Drive, Master System, Saturn,
//! ...) are in `data/signatures/game.toml`.

use crate::formats::console::{Content, GameInfo, Kind, Platform, nintendo_region, with_game};
use crate::types::{Hit, WEAK};
use crate::util::*;
use crate::view::View;

/// The 48-byte Nintendo logo of the Game Boy cartridge header (0x104).
const GB_LOGO: [u8; 48] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
    0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
    0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
];

fn code4(b: &[u8], off: usize) -> Option<String> {
    ascii_field(b, off, 4).filter(|c| {
        c.len() == 4
            && c.bytes()
                .all(|x| x.is_ascii_uppercase() || x.is_ascii_digit())
    })
}

/// Game Boy / Color: logo at 0x104, title at 0x134 (16, or 11 + manufacturer code on later
/// carts), CGB flag 0x143 (0x80 enhanced, 0xC0 Color only), destination 0x14A (0 Japan), version
/// 0x14C, header checksum 0x14D over 0x134..=0x14C.
pub(crate) fn gb(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0x104, &GB_LOGO[..6]) || h.len() < 0x150 {
        return None;
    }
    let full_logo = at(h, 0x104, &GB_LOGO);
    let sum = sl(h, 0x134, 0x14D)
        .iter()
        .fold(0u8, |x, &b| x.wrapping_sub(b).wrapping_sub(1));
    let chk_ok = u8at(h, 0x14D) == Some(sum);
    let cgb = u8at(h, 0x143).unwrap_or(0);
    let color = cgb == 0x80 || cgb == 0xC0;
    let (label, platform, fmt) = if color {
        ("game.gbc.rom", Platform::Gbc, "GBC")
    } else {
        ("game.gb.rom", Platform::Gb, "GB")
    };
    let conf = match (full_logo, chk_ok) {
        (true, true) => 98,
        (true, false) => 93,
        (false, true) => 85,
        // Six logo bytes and nothing that checks out.
        (false, false) => WEAK,
    };
    let mut g = GameInfo::new(platform, Kind::Image, fmt);
    let title_len = if color { 11 } else { 16 };
    g.title = ascii_field(h, 0x134, title_len);
    if color {
        if let Some(c) = code4(h, 0x13F) {
            g.title_id = Some(c);
        } else if let Some(t) = ascii_field(h, 0x134, 15) {
            g.title = Some(t);
        }
    }
    g.version = u8at(h, 0x14C).map(|x| x.to_string());
    g.region = match u8at(h, 0x14A) {
        Some(0) => Some("Japan"),
        Some(1) => Some("World"),
        _ => None,
    };
    g.maker = match u8at(h, 0x14B) {
        Some(0x33) => ascii_field(h, 0x144, 2),
        Some(x) => Some(format!("{x:02X}")),
        None => None,
    };
    g.content = Some(Content::Game);
    let mut hit = Hit::new(label, conf);
    if cgb == 0x80 {
        hit.push("cgb", "enhanced");
    } else if cgb == 0xC0 {
        hit.push("cgb", "only");
    }
    if u8at(h, 0x146) == Some(3) {
        hit.push("sgb", true);
    }
    Some(with_game(hit, g))
}

/// GBA: fixed byte 0x96 at 0xB2 and the logo start at 4; title at 0xA0, game code at 0xAC,
/// maker 0xB0, version 0xBC, header complement 0xBD = -(sum 0xA0..=0xBC) - 0x19.
pub(crate) fn gba(v: &View) -> Option<Hit> {
    let h = v.head();
    if !(at(h, 4, &[0x24, 0xFF, 0xAE, 0x51]) && u8at(h, 0xB2) == Some(0x96)) {
        return None;
    }
    let sum = sl(h, 0xA0, 0xBD)
        .iter()
        .fold(0u8, |x, &b| x.wrapping_sub(b))
        .wrapping_sub(0x19);
    let chk_ok = h.len() > 0xBD && u8at(h, 0xBD) == Some(sum);
    let mut g = GameInfo::new(Platform::Gba, Kind::Image, "GBA");
    g.title = ascii_field(h, 0xA0, 12);
    if let Some(c) = ascii_field(h, 0xAC, 4)
        .filter(|c| c.len() == 4 && c.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        g.region = nintendo_region(&c);
        g.title_id = Some(c);
    }
    g.maker = ascii_field(h, 0xB0, 2);
    g.version = u8at(h, 0xBC).map(|x| x.to_string());
    g.content = Some(Content::Game);
    let mut hit =
        Hit::new("game.gba.rom", if chk_ok { 98 } else { 96 }).d("title", cstr(sl(h, 0xA0, 0xAC)));
    if !chk_ok {
        hit.push(
            "note",
            "header complement check failed (hacked or trimmed header)",
        );
    }
    Some(with_game(hit, g))
}

/// N64: the PI header word 0x80371240 in one of three byte orders (z64 big-endian, v64
/// byte-swapped, n64 word-swapped); boot address at 0x08 (in KSEG0 RAM, word-aligned), title at
/// 0x20 (20 bytes), game code at 0x3B (category + 2-character id + region, e.g. NSME), version
/// at 0x3F. The header CRCs at 0x10 cover 1 MiB from 0x1000: not a cheap check. Title and game
/// code read as text: 96; a plausible boot address only (homebrew leaves the code blank): 90;
/// the 4-byte magic alone: [`WEAK`].
pub(crate) fn n64(v: &View) -> Option<Hit> {
    let h = v.head();
    let order = match sl(h, 0, 4) {
        [0x80, 0x37, 0x12, 0x40] => "z64",
        [0x37, 0x80, 0x40, 0x12] => "v64",
        [0x40, 0x12, 0x37, 0x80] => "n64",
        _ => return None,
    };
    if h.len() < 0x40 {
        return None;
    }
    let mut b: Vec<u8> = pre(h, 0x40).to_vec();
    match order {
        "v64" => b
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .for_each(|c| c.swap(0, 1)),
        "n64" => b
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .for_each(|c| c.reverse()),
        _ => {}
    }
    let mut g = GameInfo::new(Platform::N64, Kind::Image, "N64");
    g.title = ascii_field(&b, 0x20, 20);
    let code = code4(&b, 0x3B);
    let plausible = g.title.is_some() && code.is_some();
    let boot_ok =
        u32be(&b, 8).is_some_and(|a| (0x8000_0000..0x8080_0000).contains(&a) && a % 4 == 0);
    if let Some(c) = code {
        g.region = nintendo_region(&c);
        g.title_id = Some(c);
    }
    g.version = u8at(&b, 0x3F).map(|x| x.to_string());
    g.content = Some(Content::Game);
    let conf = if plausible {
        96
    } else if boot_ok {
        90
    } else {
        WEAK
    };
    let hit = Hit::new("game.n64.rom", conf).d("byte_order", order);
    Some(with_game(hit, g))
}

/// SNES internal header at 0x7FC0 (LoROM), 0xFFC0 (HiROM) or 0x40FFC0 (ExHiROM), shifted by
/// 0x200 when a copier header precedes the ROM (file size = 512 mod 1024): title (21), map
/// mode +0x15, ROM type +0x16, ROM size +0x17 (log2 KiB), SRAM +0x18, region +0x19, developer
/// +0x1A (0x33 = extended header with the 4-character game code at -0x0E), version +0x1B,
/// checksum complement +0x1C and checksum +0x1E (complement ^ checksum = 0xFFFF).
pub(crate) fn snes(v: &View) -> Option<Hit> {
    let size = v.size();
    if !(0x8000..=0x80_0400).contains(&size) {
        return None;
    }
    let copier = if size % 1024 == 512 { 0x200 } else { 0 };
    let mut best: Option<(u8, u64, &[u8])> = None;
    for (base, modes) in [
        (0x7FC0u64, &[0x20u8, 0x22, 0x23, 0x30, 0x32, 0x33][..]),
        (0xFFC0, &[0x21, 0x31, 0x35, 0x3A, 0x25][..]),
        (0x40_FFC0, &[0x25, 0x35][..]),
    ] {
        let at_ = base + copier;
        let Some(hd) = v.at(at_, 0x40) else { continue };
        let map = u8at(hd, 0x15).unwrap_or(0);
        let rom = u8at(hd, 0x17).unwrap_or(0);
        let sram = u8at(hd, 0x18).unwrap_or(0xFF);
        let region = u8at(hd, 0x19).unwrap_or(0xFF);
        let comp = u16le(hd, 0x1C).unwrap_or(0);
        let chk = u16le(hd, 0x1E).unwrap_or(0);
        let title = sl(hd, 0, 21);
        let title_ok = title
            .iter()
            .all(|&c| (0x20..0x7F).contains(&c) || (0xA0..0xE0).contains(&c));
        if !modes.contains(&map) || rom > 13 || sram > 8 || region > 0x14 {
            continue;
        }
        let sum_ok = comp ^ chk == 0xFFFF && comp != 0 && chk != 0;
        // Homebrew headers often leave the ROM size wrong and put a placeholder ("CCCS") in
        // the checksum words: a printable, non-blank title, a valid map mode and a reset
        // vector in the upper half of the bank (0x8000-0xFFFF, where the ROM is mapped) still
        // make a header (85).
        let named = title_ok && title.iter().any(u8::is_ascii_alphanumeric);
        let reset_ok = u16le(hd, 0x3C).is_some_and(|r| r >= 0x8000);
        let score = match (sum_ok && (7..=13).contains(&rom), title_ok) {
            (true, true) => 96,
            (true, false) => 88,
            (false, true) if named && reset_ok => 85,
            (false, true) if (7..=13).contains(&rom) => 60,
            _ => continue,
        };
        if best.is_none_or(|b| score > b.0) {
            best = Some((score, at_, hd));
        }
    }
    let (conf, base, hd) = best?;
    let mut g = GameInfo::new(Platform::Snes, Kind::Image, "SNES");
    g.title = ascii_field(hd, 0, 21);
    g.version = u8at(hd, 0x1B).map(|x| format!("1.{x}"));
    g.region = match u8at(hd, 0x19) {
        Some(0) => Some("Japan"),
        Some(1) => Some("Americas"),
        Some(2..=10) => Some("Europe/PAL"),
        Some(13) => Some("Korea"),
        _ => None,
    };
    let mut hit = Hit::new("game.snes.rom", conf).d("header_offset", base);
    if u8at(hd, 0x1A) == Some(0x33) {
        match v.at(base.saturating_sub(0x10), 0x10) {
            Some(ext) => {
                g.maker = ascii_field(ext, 0, 2);
                if let Some(c) = code4(ext, 2) {
                    g.title_id = Some(c);
                }
            }
            None => hit = hit.need(base.saturating_sub(0x10), 0x10),
        }
    } else {
        g.maker = u8at(hd, 0x1A).map(|x| format!("{x:02X}"));
    }
    g.content = Some(Content::Game);
    if copier != 0 {
        hit.push("copier_header", true);
    }
    Some(with_game(hit, g))
}

/// NES (iNES 1.0 / NES 2.0): "NES\x1a", PRG ROM in 16 KiB units at 4, CHR ROM in 8 KiB units
/// at 5, trainer (512 bytes) when bit 2 of byte 6 is set, NES 2.0 when bits 2-3 of byte 7 are
/// 10 (size MSB nibbles in byte 9). The file size matching the header is the structural check
/// (96; PlayChoice-10 and NES 2.0 miscellaneous ROMs may follow). A clean iNES 1.0 header
/// (bytes 11-15 zero; "DiskDude!" junk fails it) whose size does not match: 85. The 4-byte
/// magic alone is left to the signature rule ([`WEAK`]).
pub(crate) fn nes(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"NES\x1a") || h.len() < 16 {
        return None;
    }
    let (f6, f7) = (u8at(h, 6)?, u8at(h, 7)?);
    let nes2 = f7 & 0x0C == 0x08;
    let msb = if nes2 { u8at(h, 9)? } else { 0 };
    // Exponent-multiplier sizes (MSB nibble 0xF) are not checked.
    let exp = msb & 0x0F == 0x0F || msb >> 4 == 0x0F;
    let prg = (u64::from(msb & 0x0F) << 8) | u64::from(u8at(h, 4)?);
    let chr = (u64::from(msb >> 4) << 8) | u64::from(u8at(h, 5)?);
    let trainer = if f6 & 4 != 0 { 512 } else { 0 };
    let expect = 16 + trainer + prg * 16384 + chr * 8192;
    let size = v.size();
    let extra_ok = (f7 & 2 != 0 && !nes2) || (nes2 && u8at(h, 14).is_some_and(|m| m & 3 != 0));
    let size_ok = !exp && prg > 0 && (size == expect || (extra_ok && size > expect));
    let clean = !nes2 && sl(h, 11, 16).iter().all(|&b| b == 0);
    let conf = if size_ok {
        96
    } else if clean && prg > 0 {
        85
    } else {
        return None;
    };
    let mapper = if nes2 || clean {
        u64::from((f6 >> 4) | (f7 & 0xF0))
    } else {
        u64::from(f6 >> 4)
    };
    let mut hit = Hit::new("game.nes.rom", conf)
        .d("header", if nes2 { "NES 2.0" } else { "iNES" })
        .d("prg_kib", prg * 16)
        .d("chr_kib", chr * 8)
        .d("mapper", mapper);
    if !size_ok {
        hit.push("note", "file size does not match the header");
    }
    Some(hit)
}
