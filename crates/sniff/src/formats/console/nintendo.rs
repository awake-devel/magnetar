//! Nintendo: Switch NSP/NSZ/HFS0/XCI/XCZ, 3DS CCI/CXI/CIA, Wii/GameCube discs and their
//! compressed forms (WBFS, WIA/RVZ, GCZ, CISO), Wii WAD, Wii U WUD/WUX and `meta.xml`, NDS.
//! Layouts from format documentation (switchbrew, 3dbrew, wiibrew, Dolphin's docs, GBATEK).
//! Checked on real homebrew: NRO, ExeFS PFS0, CIA, NDS, GameCube ISO and DOL; the rest is
//! UNVERIFIED (see the crate README). Ids are checked against the GameTDB formats
//! ([`super::db_id_valid`]).
//!
//! Switch ids are mostly inside encrypted NCAs: an NSP gives the title ID through its ticket
//! name (rights ID) and, for scene dumps that ship them, the plaintext `.cnmt.xml` /
//! `.nacp.xml`; an XCI gives no id at all without console keys (`encrypted = true`).

use std::io::Read;

use crate::formats::console::{
    Content, GameInfo, Kind, Platform, nintendo_content, nintendo_region, retail_code, with_game,
};
use crate::types::{Hit, WEAK};
use crate::util::*;
use crate::view::View;

const WII_MAGIC: u32 = 0x5D1C_9EA3;
const GC_MAGIC: u32 = 0xC233_9F3D;

// ---------------------------------------------------------------- Switch

/// A PFS0 / HFS0 entry: name, data offset from the file start, size.
type HfsEntry = (String, u64, u64);

/// A PFS0 / HFS0 directory and its header length.
fn hfs_entries(b: &[u8], base: u64, hfs: bool) -> Option<(Vec<HfsEntry>, usize)> {
    let n = us(u64::from(u32le(b, 4)?));
    let st = us(u64::from(u32le(b, 8)?));
    if n == 0 || n > 4096 || st > 1 << 20 {
        return None;
    }
    let esz = if hfs { 0x40 } else { 0x18 };
    let so = 16 + n * esz;
    let header = so.checked_add(st)?;
    let tbl = b.get(so..header)?;
    let mut out = Vec::new();
    for i in 0..n {
        let e = 16 + i * esz;
        let (Some(off), Some(size), Some(no)) = (u64le(b, e), u64le(b, e + 8), u32le(b, e + 16))
        else {
            break;
        };
        let name = cstr(from(tbl, us(u64::from(no))));
        let at = base.saturating_add(header as u64).saturating_add(off);
        out.push((name, at, size));
    }
    Some((out, header))
}

/// The title ID in a rights ID (ticket / certificate name): 32 hex digits, the first 16.
fn rights_title(name: &str) -> Option<String> {
    let stem = name
        .strip_suffix(".tik")
        .or_else(|| name.strip_suffix(".cert"))?;
    (stem.len() == 32 && stem.bytes().all(|c| c.is_ascii_hexdigit()))
        .then(|| stem.get(..16).map(str::to_ascii_uppercase))
        .flatten()
}

/// NSP / NSZ (PFS0) and HFS0: file count at 4, string table size at 8, entries of 0x18 (PFS0)
/// or 0x40 (HFS0) from 0x10, then the names. NCA names are content IDs (hashes), not title
/// IDs: only ticket names (rights ID = title ID + key generation) and plaintext XML give ids.
pub(crate) fn pfs0(v: &View) -> Option<Hit> {
    let h = v.head();
    let hfs = at(h, 0, b"HFS0");
    if !at(h, 0, b"PFS0") && !hfs {
        return None;
    }
    let n = us(u64::from(u32le(h, 4)?));
    let st = us(u64::from(u32le(h, 8)?));
    if n == 0 || n > 4096 || st > 1 << 20 {
        return None;
    }
    let esz = if hfs { 0x40 } else { 0x18 };
    let header = 16 + n * esz + st;
    let mut g = GameInfo::new(
        Platform::Switch,
        Kind::Package,
        if hfs { "HFS0" } else { "NSP" },
    );
    let Some((ents, _)) = hfs_entries(h, 0, hfs) else {
        return Some(with_game(
            Hit::new("game.switch.nsp", 70).need(0, header as u64),
            g,
        ));
    };
    let names: Vec<&str> = ents.iter().map(|e| e.0.as_str()).collect();
    let in_bounds = ents
        .iter()
        .all(|e| e.1.checked_add(e.2).is_some_and(|end| end <= v.size()));
    let ncz = names.iter().any(|x| x.ends_with(".ncz"));
    let nca = names.iter().any(|x| x.ends_with(".nca"));
    let label = if hfs {
        "game.switch.hfs0"
    } else if ncz {
        g.format = "NSZ";
        g.kind = Kind::CompressedImage;
        "game.switch.nsz"
    } else {
        "game.switch.nsp"
    };
    let mut conf = if nca || ncz { 97 } else { 80 };
    if !in_bounds {
        conf = conf.min(85);
    }
    let mut hit = Hit::new(label, conf).d(
        "files",
        names
            .iter()
            .take(64)
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>(),
    );
    if !in_bounds {
        hit.push("note", "an entry extends past the file end");
    }
    // NCAs (and their tickets) are encrypted; an ExeFS PFS0 (main + main.npdm, rtld, sdk,
    // subsdk*) holds plaintext NSOs: an Atmosphere title override or an extracted ExeFS, not
    // an installable package.
    let tik = names.iter().any(|x| x.ends_with(".tik"));
    let exefs = !hfs
        && !(nca || ncz || tik)
        && names.contains(&"main.npdm")
        && names.iter().all(|x| {
            matches!(*x, "main" | "main.npdm" | "rtld" | "sdk") || x.starts_with("subsdk")
        });
    if exefs {
        g.format = "ExeFS";
        g.encrypted = Some(false);
        hit.push("exefs", true);
    } else if nca || ncz || tik {
        g.encrypted = Some(true);
    }
    if let Some(t) = names.iter().find_map(|n| rights_title(n)) {
        g.content = u64::from_str_radix(&t, 16)
            .ok()
            .and_then(|t| nintendo_content(Platform::Switch, t));
        g.title_id = Some(t);
    }
    // Plaintext metadata some dumps include (`<content id>.cnmt.xml`, `.nacp.xml`).
    for (name, off, size) in &ents {
        let xml = name.ends_with(".cnmt.xml") || name.ends_with(".nacp.xml");
        if !xml || *size == 0 || *size > 256 * 1024 || off.saturating_add(*size) > v.size() {
            continue;
        }
        match v.at(*off, us(*size)) {
            Some(b) => switch_xml(&String::from_utf8_lossy(b), &mut g, &mut hit),
            None => hit = hit.need(*off, *size),
        }
    }
    Some(with_game(hit, g))
}

/// `.cnmt.xml` (`<Type>`, `<Id>`, `<Version>`) and `.nacp.xml` (`<Name>`, `<DisplayVersion>`).
fn switch_xml(doc: &str, g: &mut GameInfo, hit: &mut Hit) {
    let tag = |t: &str| -> Option<String> {
        let open = format!("<{t}>");
        let a = doc.find(&open)? + open.len();
        let b = doc.get(a..)?.find('<')? + a;
        let s = doc.get(a..b)?.trim();
        (!s.is_empty() && s.len() <= 512).then(|| s.to_owned())
    };
    if let Some(t) = tag("Type") {
        g.content = match t.as_str() {
            "Application" => Some(Content::Game),
            "Patch" => Some(Content::Patch),
            "AddOnContent" => Some(Content::Dlc),
            "SystemUpdate" | "SystemProgram" | "SystemData" => Some(Content::System),
            _ => g.content,
        };
        hit.push("cnmt_type", t);
    }
    if let Some(id) = tag("Id")
        .map(|i| i.trim_start_matches("0x").to_ascii_uppercase())
        .filter(|i| i.len() == 16 && i.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        g.title_id = Some(id);
    }
    if let Some(dv) = tag("DisplayVersion") {
        g.version = Some(dv);
    } else if let Some(ver) = tag("Version").filter(|_| g.version.is_none()) {
        hit.push("title_version", ver);
    }
    if let Some(n) = tag("Name") {
        g.title = Some(n);
    }
}

/// XCI: "HEAD" at 0x100 (0x1100 with a 0x1000 key area in front); 0x10D card size code, 0x130
/// le64 root HFS0 offset, 0x138 its header size. The root HFS0 (plaintext) names the
/// partitions; the `secure` partition lists the NCAs (`.ncz` = XCZ). Title ID and name are in
/// encrypted NCAs: not reachable.
pub(crate) fn xci(v: &View) -> Option<Hit> {
    let h = v.head();
    let base = [0x100usize, 0x1100]
        .into_iter()
        .find(|&b| at(h, b, b"HEAD"))?
        - 0x100;
    if h.len() <= base + 0x140 {
        return None;
    }
    let card = u8at(h, base + 0x10D);
    let hfs0 = u64le(h, base + 0x130)
        .unwrap_or(0)
        .saturating_add(base as u64);
    let hfs0_size = u64le(h, base + 0x138).unwrap_or(0);
    let valid = card.is_some_and(|c| [0xFA, 0xF8, 0xF0, 0xE0, 0xE1, 0xE2].contains(&c))
        && hfs0 != 0
        && hfs0 < v.size();
    let mut hit = Hit::new("game.switch.xci", if valid { 95 } else { 92 });
    let mut g = GameInfo::new(Platform::Switch, Kind::Image, "XCI");
    g.encrypted = Some(true);
    if let Some(c) = card {
        hit.push("card_size_code", format!("{c:#x}"));
    }
    if base != 0 {
        hit.push("key_area", true);
    }
    if !valid {
        return Some(with_game(hit, g));
    }
    let want = hfs0_size.clamp(0x200, 0x8000);
    let root = match v.at(hfs0, us(want)).or_else(|| {
        let b = v.from(hfs0);
        (b.len() >= 0x200).then_some(b)
    }) {
        Some(b) if at(b, 0, b"HFS0") => b,
        Some(_) => return Some(with_game(hit, g)),
        None => return Some(with_game(hit.need(hfs0, want), g)),
    };
    let Some((parts, _)) = hfs_entries(root, hfs0, true) else {
        return Some(with_game(hit, g));
    };
    hit.push(
        "partitions",
        parts.iter().map(|p| p.0.clone()).collect::<Vec<_>>(),
    );
    if let Some(&(_, sec, _)) = parts.iter().find(|p| p.0 == "secure") {
        match v.at(sec, 0x10).filter(|b| at(b, 0, b"HFS0")) {
            Some(_) => {
                let b = v.from(sec);
                match hfs_entries(b, sec, true) {
                    Some((files, _)) => {
                        let ncz = files.iter().any(|f| f.0.ends_with(".ncz"));
                        if ncz {
                            hit.relabel("game.switch.xcz");
                            g.format = "XCZ";
                            g.kind = Kind::CompressedImage;
                        }
                        hit.push("secure_files", files.len());
                    }
                    None => hit = hit.need(sec, 0x8000),
                }
            }
            None if !v.has(sec, 0x10) => hit = hit.need(sec, 0x8000),
            None => {}
        }
    }
    Some(with_game(hit, g))
}

// ---------------------------------------------------------------- 3DS

/// Fills product code, maker and program ID from an NCCH header (`n` starts at the NCCH
/// signature; "NCCH" at +0x100).
fn ncch_fill(n: &[u8], g: &mut GameInfo, hit: &mut Hit) -> bool {
    if !at(n, 0x100, b"NCCH") {
        return false;
    }
    if let Some(code) = ascii_field(n, 0x150, 16) {
        g.region = retail_code(&code).and_then(nintendo_region);
        g.product_code = Some(code.clone());
        hit.push("product_code", code);
    }
    g.maker = ascii_field(n, 0x110, 2);
    if g.title_id.is_none() {
        g.title_id = u64le(n, 0x118)
            .filter(|&t| t != 0)
            .map(|t| format!("{t:016X}"));
    }
    // Flags byte 7 bit 2: NoCrypto.
    g.encrypted = u8at(n, 0x188 + 7).map(|f| f & 4 == 0);
    true
}

fn n3ds_content(g: &mut GameInfo) {
    if g.content.is_none() {
        g.content = g
            .title_id
            .as_deref()
            .and_then(|t| u64::from_str_radix(t, 16).ok())
            .and_then(|t| nintendo_content(Platform::N3ds, t));
    }
}

/// CCI (.3ds): "NCSD" at 0x100, le64 media ID (= title ID) at 0x108, partition 0 offset in
/// 0x200-byte units at 0x120; the first NCCH has the product code at +0x150.
pub(crate) fn ncsd(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0x100, b"NCSD") {
        return None;
    }
    let mut g = GameInfo::new(Platform::N3ds, Kind::Image, "CCI");
    g.title_id = u64le(h, 0x108)
        .filter(|&t| t != 0)
        .map(|t| format!("{t:016X}"));
    let p0 = u64::from(u32le(h, 0x120).unwrap_or(0)).saturating_mul(0x200);
    let mut hit = Hit::new(
        "game.3ds.cci",
        if p0 != 0 && p0 < v.size() { 95 } else { 93 },
    );
    if p0 != 0 && p0 < v.size() {
        match v.at(p0, 0x200) {
            Some(n) => {
                ncch_fill(n, &mut g, &mut hit);
            }
            None => hit = hit.need(p0, 0x200),
        }
    }
    n3ds_content(&mut g);
    Some(with_game(hit, g))
}

/// A bare NCCH (.cxi / .cfa / .app): "NCCH" at 0x100, le32 size in 0x200 units at 0x104.
pub(crate) fn ncch(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0x100, b"NCCH") || h.len() < 0x200 {
        return None;
    }
    let units = u64::from(u32le(h, 0x104)?).saturating_mul(0x200);
    let consistent = units != 0 && units <= v.size().saturating_add(0x200);
    let mut g = GameInfo::new(Platform::N3ds, Kind::Image, "NCCH");
    // "NCCH" alone (4 bytes) is a hint: the content size is the structural check.
    let mut hit = Hit::new("game.3ds.cxi", if consistent { 96 } else { WEAK });
    ncch_fill(h, &mut g, &mut hit);
    n3ds_content(&mut g);
    Some(with_game(hit, g))
}

/// CIA: le32 header size 0x2020 at 0, type 0, version 0, certificate chain 0xA00. Sections are
/// 64-byte aligned: header, certificates, ticket, TMD, content, meta. Title ID at ticket + 0x1DC
/// (be64). The first content's NCCH header gives the product code unless the content is
/// encrypted. With a meta section, the SMDH at meta + 0x400 holds the titles (UTF-16LE, English
/// short title at SMDH + 0x208) and the meta section ends the file.
pub(crate) fn cia(v: &View) -> Option<Hit> {
    let h = v.head();
    if u32le(h, 0)? != 0x2020 || u16le(h, 4)? != 0 || u32le(h, 8)? != 0xA00 {
        return None;
    }
    let a = |x: u64| x.div_ceil(64).saturating_mul(64);
    let (cert, tik, tmd, meta) = (
        u64::from(u32le(h, 8)?),
        u64::from(u32le(h, 12).unwrap_or(0)),
        u64::from(u32le(h, 16).unwrap_or(0)),
        u64::from(u32le(h, 20).unwrap_or(0)),
    );
    let content = u64le(h, 24).unwrap_or(0);
    let pre_content = a(0x2020)
        .saturating_add(a(cert))
        .saturating_add(a(tik))
        .saturating_add(a(tmd));
    let total = pre_content
        .saturating_add(a(content))
        .saturating_add(a(meta));
    let size = v.size();
    let consistent = tik != 0 && tmd != 0 && total <= size && size - total <= 64;
    let mut hit = Hit::new("game.3ds.cia", if consistent { 95 } else { 80 });
    if !consistent {
        hit.push("note", "section sizes do not add up to the file length");
    }
    let mut g = GameInfo::new(Platform::N3ds, Kind::Package, "CIA");
    if consistent {
        let tik_at = a(0x2020).saturating_add(a(cert));
        match v.at(tik_at.saturating_add(0x1DC), 8) {
            Some(t) => g.title_id = u64be(t, 0).map(|t| format!("{t:016X}")),
            None => hit = hit.need(tik_at, 0x200),
        }
        if content >= 0x200 {
            match v.at(pre_content, 0x200) {
                Some(n) => {
                    if !ncch_fill(n, &mut g, &mut hit) {
                        g.encrypted = Some(true);
                    }
                }
                None => hit = hit.need(pre_content, 0x200),
            }
        }
        if meta >= 0x400 + 0x36C0 {
            let meta_at = pre_content.saturating_add(a(content));
            let smdh = meta_at.saturating_add(0x400);
            match v.at(smdh, 0x300) {
                Some(s) if at(s, 0, b"SMDH") => g.title = utf16_field(s, 8 + 0x200, 0x80, false),
                Some(_) => {}
                None => hit = hit.need(meta_at, size.saturating_sub(meta_at)),
            }
        }
    }
    n3ds_content(&mut g);
    Some(with_game(hit, g))
}

// ---------------------------------------------------------------- Wii / GameCube

/// A Wii / GameCube disc header: 6-character game ID (= GameTDB id: 4-character game code +
/// 2-character maker), disc number at 6, version at 7, magic at 0x18 (Wii) or 0x1C
/// (GameCube), title at 0x20.
struct DiscHeader {
    platform: Platform,
    id: String,
    title: Option<String>,
    disc: u8,
    version: u8,
}

fn disc_header(b: &[u8], off: usize, title_len: usize) -> Option<DiscHeader> {
    let platform = if u32be(b, off + 0x18) == Some(WII_MAGIC) {
        Platform::Wii
    } else if u32be(b, off + 0x1C) == Some(GC_MAGIC) {
        Platform::GameCube
    } else {
        return None;
    };
    let id = ascii_field(b, off, 6).filter(|s| {
        s.len() == 6
            && s.bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    });
    Some(DiscHeader {
        platform,
        id: id.unwrap_or_default(),
        title: text_field(b, off + 0x20, title_len),
        disc: u8at(b, off + 6).unwrap_or(0),
        version: u8at(b, off + 7).unwrap_or(0),
    })
}

fn disc_game(kind: Kind, format: &'static str, d: DiscHeader, hit: &mut Hit) -> GameInfo {
    let mut g = GameInfo::new(d.platform, kind, format);
    if !d.id.is_empty() {
        g.region = nintendo_region(&d.id);
        g.maker = d.id.get(4..6).map(str::to_owned);
        g.title_id = Some(d.id);
    }
    g.title = d.title;
    g.version = Some(format!("1.{:02}", d.version));
    g.content = Some(Content::Game);
    if d.disc > 0 {
        hit.push("disc_number", u64::from(d.disc) + 1);
    }
    g
}

/// Raw .iso / .gcm: the detector prototype required an alphanumeric 4-character game code.
pub(crate) fn wii_gc_raw(v: &View) -> Option<Hit> {
    let h = v.head();
    let alnum = h
        .get(..4)
        .is_some_and(|g| g.iter().all(u8::is_ascii_alphanumeric));
    if !alnum {
        return None;
    }
    let d = disc_header(h, 0, 0x40)?;
    let label = if d.platform == Platform::Wii {
        "game.wii.iso"
    } else {
        "game.gamecube.iso"
    };
    let mut hit = Hit::new(label, 96).d("game_id", lossy(sl(h, 0, 6)));
    if at(h, 0x200, b"NKIT") {
        hit.push("nkit", true);
    }
    let gc = d.platform == Platform::GameCube;
    let mut g = disc_game(Kind::Image, "ISO", d, &mut hit);
    // GameCube: the console checks the region word of bi2.bin (disc offset 0x458), not the
    // game-code letter; homebrew discs (Swiss) set them differently. The letter stays the
    // GameTDB key (title_id).
    if gc && let Some(r) = u32be(h, 0x458).and_then(gc_bi2_region) {
        if g.region != Some(r) {
            hit.push("code_region", g.region.unwrap_or("unknown"));
        }
        g.region = Some(r);
    }
    Some(with_game(hit, g))
}

/// GameCube bi2.bin region word (YAGCD): 0 NTSC-J, 1 NTSC-U, 2 PAL, 4 NTSC-K.
fn gc_bi2_region(w: u32) -> Option<&'static str> {
    Some(match w {
        0 => "Japan",
        1 => "Americas",
        2 => "Europe/PAL",
        4 => "Korea",
        _ => return None,
    })
}

/// WBFS: "WBFS" at 0, byte 8 = log2 of the HD sector size; the first disc's header copy follows
/// the first HD sector.
pub(crate) fn wbfs(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"WBFS") {
        return None;
    }
    let shift = u32::from(u8at(h, 8).unwrap_or(0));
    let mut g = GameInfo::new(Platform::Wii, Kind::CompressedImage, "WBFS");
    let mut hit = Hit::new(
        "game.wii.wbfs",
        if (9..=12).contains(&shift) { 92 } else { 70 },
    );
    if (9..=12).contains(&shift) {
        let off = 1u64 << shift;
        match v.at(off, 0x100) {
            Some(b) => {
                if let Some(d) = disc_header(b, 0, 0x40) {
                    g = disc_game(Kind::CompressedImage, "WBFS", d, &mut hit);
                    hit.conf = 96;
                }
            }
            None => hit = hit.need(off, 0x100),
        }
    }
    Some(with_game(hit, g))
}

/// WIA / RVZ: magic at 0, a 0x48-byte file header, then the disc struct whose copy of the first
/// 0x80 disc bytes starts at 0x58.
pub(crate) fn wia_rvz(v: &View) -> Option<Hit> {
    let h = v.head();
    let format = if at(h, 0, b"RVZ\x01") {
        "RVZ"
    } else if at(h, 0, b"WIA\x01") {
        "WIA"
    } else {
        return None;
    };
    let version_ok = u32be(h, 4).is_some_and(|v| (0x0001_0000..0x0100_0000).contains(&v));
    let mut hit = Hit::new("game.rvz", 75);
    let g = match disc_header(h, 0x58, 0x60) {
        Some(d) => {
            hit.conf = 96;
            disc_game(Kind::CompressedImage, format, d, &mut hit)
        }
        None => {
            hit.conf = if version_ok { 95 } else { 75 };
            GameInfo::new(Platform::WiiOrGameCube, Kind::CompressedImage, format)
        }
    };
    Some(with_game(hit, g))
}

/// GCZ: le32 0xB10BC001 at 0, le32 sub-type at 4 (0 GameCube, 1 Wii), le64 compressed size at
/// 8, le64 data size at 0x10, le32 block size at 0x18, le32 block count at 0x1C, then le64
/// block pointers (bit 63 = stored), le32 hashes, then the data. Block 0 (zlib) holds the
/// disc header.
pub(crate) fn gcz(v: &View) -> Option<Hit> {
    let h = v.head();
    if u32le(h, 0)? != 0xB10B_C001 {
        return None;
    }
    let compressed = u64le(h, 8)?;
    let platform = match u32le(h, 4)? {
        0 => Platform::GameCube,
        1 => Platform::Wii,
        _ => Platform::WiiOrGameCube,
    };
    let conf = if compressed <= v.size() { 92 } else { 75 };
    let mut hit = Hit::new("game.gcz", conf);
    let mut g = GameInfo::new(platform, Kind::CompressedImage, "GCZ");
    let block = u64::from(u32le(h, 0x18).unwrap_or(0));
    let count = u64::from(u32le(h, 0x1C).unwrap_or(0));
    if conf >= 90 && (0x1000..=0x10_0000).contains(&block) && count > 0 && count < 1 << 24 {
        let data = 0x20u64.saturating_add(count.saturating_mul(12));
        let (p0, p1) = (u64le(h, 0x20), u64le(h, 0x28));
        if let Some(p0) = p0 {
            let stored = p0 >> 63 == 1;
            let start = data.saturating_add(p0 & !(1 << 63));
            let end = if count > 1 {
                p1.map(|p| data.saturating_add(p & !(1 << 63)))
            } else {
                Some(data.saturating_add(compressed))
            };
            let len = end
                .and_then(|e| e.checked_sub(start))
                .unwrap_or(0)
                .min(block);
            if len > 0 {
                match v.at(start, us(len)) {
                    Some(b) => {
                        let disc = if stored {
                            Some(pre(b, 0x100).to_vec())
                        } else {
                            let mut out = Vec::new();
                            flate2::read::ZlibDecoder::new(b)
                                .take(0x100)
                                .read_to_end(&mut out)
                                .ok()
                                .map(|_| out)
                        };
                        if let Some(d) = disc.as_deref().and_then(|d| disc_header(d, 0, 0x40)) {
                            g = disc_game(Kind::CompressedImage, "GCZ", d, &mut hit);
                            hit.conf = 96;
                        }
                    }
                    None => hit = hit.need(start, len),
                }
            }
        }
    }
    Some(with_game(hit, g))
}

/// "CISO" is two formats. PSP CSO: le32 header size (0x18 or 0) at 4, le64 uncompressed size at
/// 8, le32 block size 0x800 at 0x10, version at 0x14 (the ISO walk is in iso.rs). Wii CISO:
/// le32 block size at 4, a block map, the disc header at 0x8000. "ZISO" (LZ4), "JISO" and
/// "DAX\0" are PSP only.
pub(crate) fn ciso(v: &View) -> Option<Hit> {
    let h = v.head();
    let psp = || GameInfo::new(Platform::Psp, Kind::CompressedImage, "CSO");
    if at(h, 0, b"ZISO") {
        return Some(with_game(
            Hit::new("game.psp.cso", 90).d("variant", "zso"),
            psp(),
        ));
    }
    if at(h, 0, b"JISO") || at(h, 0, b"DAX\0") {
        return Some(with_game(Hit::new("game.psp.cso", 85), psp()));
    }
    if !at(h, 0, b"CISO") {
        return None;
    }
    let field = u32le(h, 4)?;
    let bs = u32le(h, 16)?;
    if (field == 0x18 || field == 0) && bs == 0x800 {
        let conf = if field == 0x18 { 93 } else { 88 };
        return Some(with_game(Hit::new("game.psp.cso", conf), psp()));
    }
    if field.is_power_of_two() && field >= 0x8000 {
        let mut hit = Hit::new("game.ciso", 80).d("note", "Wii/GameCube CISO");
        let g = match v.at(0x8000, 0x80) {
            Some(b) => match disc_header(b, 0, 0x40) {
                Some(d) => {
                    hit.conf = 92;
                    disc_game(Kind::CompressedImage, "CISO", d, &mut hit)
                }
                None => GameInfo::new(Platform::WiiOrGameCube, Kind::CompressedImage, "CISO"),
            },
            None => {
                hit = hit.need(0x8000, 0x80);
                GameInfo::new(Platform::WiiOrGameCube, Kind::CompressedImage, "CISO")
            }
        };
        return Some(with_game(hit, g));
    }
    Some(Hit::new("game.ciso", 70).d("note", "CISO: PSP CSO or Wii/GC CISO"))
}

// ---------------------------------------------------------------- Wii WAD

/// Wii WAD (WiiWare, Virtual Console, channels): be32 header size 0x20, type "Is" / "ib" /
/// "Bk", be32 certificate chain size, ticket size, TMD size, data size, footer size. Sections
/// are 64-byte aligned; the ticket holds the title ID at +0x1DC, the TMD the title version at
/// +0x1DC. The low half of a WiiWare / VC title ID is the 4-character GameTDB id
/// (00010001-57414145 = "WAAE").
pub(crate) fn wii_wad(v: &View) -> Option<Hit> {
    let h = v.head();
    if u32be(h, 0)? != 0x20 || !(at(h, 4, b"Is\0\0") || at(h, 4, b"ib\0\0") || at(h, 4, b"Bk\0\0"))
    {
        return None;
    }
    let a = |x: u64| x.div_ceil(64).saturating_mul(64);
    let cert = u64::from(u32be(h, 8)?);
    let tik = u64::from(u32be(h, 0x10)?);
    let tmd = u64::from(u32be(h, 0x14)?);
    let data = u64::from(u32be(h, 0x18)?);
    let footer = u64::from(u32be(h, 0x1C)?);
    let total = a(0x20)
        .saturating_add(a(cert))
        .saturating_add(a(tik))
        .saturating_add(a(tmd))
        .saturating_add(a(data))
        .saturating_add(a(footer));
    let plausible = (0x100..0x2000).contains(&cert)
        && tik == 0x2A4
        && (0x1E4..0x10000).contains(&tmd)
        && total <= v.size().saturating_add(64)
        && v.size().saturating_sub(total) <= 64;
    let mut hit = Hit::new("game.wii.wad", if plausible { 96 } else { 75 });
    let mut g = GameInfo::new(Platform::Wii, Kind::Package, "WAD");
    let tik_at = a(0x20).saturating_add(a(cert));
    let tmd_at = tik_at.saturating_add(a(tik));
    match (
        v.at(tik_at.saturating_add(0x1DC), 8),
        v.at(tmd_at.saturating_add(0x1DC), 2),
    ) {
        (Some(t), tv) => {
            if let Some(tid) = u64be(t, 0) {
                g.title_id = Some(format!("{tid:016X}"));
                g.content = nintendo_content(Platform::Wii, tid);
                let low = (tid as u32).to_be_bytes();
                if low
                    .iter()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                {
                    let code = String::from_utf8_lossy(&low).into_owned();
                    g.region = nintendo_region(&code);
                    g.db_id = Some(code);
                }
            }
            if let Some(ver) = tv.and_then(|b| u16be(b, 0)) {
                g.version = Some(ver.to_string());
            }
        }
        (None, _) if plausible => hit = hit.need(tik_at, a(tik).saturating_add(0x200)),
        _ => {}
    }
    Some(with_game(hit, g))
}

// ---------------------------------------------------------------- Wii U

/// Wii U disc images. WUD: the raw disc, starting with the product code `WUP-P-XXXX`. WUX
/// (compressed WUD): "WUX0" + le32 0x1099D02E (its index layout is not parsed here). The
/// disc contents are encrypted: only the product code is readable (GameTDB key = the 4
/// characters plus the maker code, which only meta.xml has).
pub(crate) fn wiiu_wud(v: &View) -> Option<Hit> {
    let h = v.head();
    if at(h, 0, b"WUX0") && u32le(h, 4) == Some(0x1099_D02E) {
        let mut g = GameInfo::new(Platform::WiiU, Kind::CompressedImage, "WUX");
        g.encrypted = Some(true);
        let hit = Hit::new("game.wiiu.wux", 96);
        return Some(with_game(hit, g));
    }
    if !at(h, 0, b"WUP-P-") {
        return None;
    }
    let code = ascii_field(h, 6, 4)?;
    if code.len() != 4
        || !code
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return None;
    }
    let mut g = GameInfo::new(Platform::WiiU, Kind::Image, "WUD");
    g.encrypted = Some(true);
    let mut hit = Hit::new("game.wiiu.wud", if v.size() > 1 << 30 { 95 } else { 85 });
    wud_code(h, &mut g, &mut hit);
    Some(with_game(hit, g))
}

fn wud_code(b: &[u8], g: &mut GameInfo, hit: &mut Hit) {
    if !at(b, 0, b"WUP-P-") {
        return;
    }
    if let Some(code) = ascii_field(b, 0, 10) {
        g.region = code.get(6..).and_then(nintendo_region);
        g.product_code = Some(code.clone());
        hit.push("product_code", code);
    }
    g.content = Some(Content::Game);
}

/// Wii U `meta/meta.xml` (folder dumps: loadiine / WUP installables): `<title_id>`,
/// `<product_code>WUP-P-AAFE</product_code>`, `<company_code>0001</company_code>`,
/// `<title_version>`, `<longname_en>`. GameTDB id = AAFE + the last two digits of the company
/// code (AAFE01).
pub(crate) fn wiiu_meta(v: &View) -> Option<Hit> {
    let h = v.head();
    let lead = pre(h, 0x1000);
    if !(contains(lead, b"<menu") && contains(pre(h, 0x8000), b"<product_code")) {
        return None;
    }
    if v.size() > 1 << 20 {
        return None;
    }
    let mut g = GameInfo::new(Platform::WiiU, Kind::Metadata, "meta.xml");
    let mut hit = Hit::new("game.wiiu.meta", 90);
    let Some(all) = v.at(0, us(v.size())) else {
        return Some(with_game(hit.need(0, v.size()), g));
    };
    let doc = String::from_utf8_lossy(all);
    let tag = |t: &str| -> Option<String> {
        let a = doc.find(&format!("<{t}"))?;
        let a = doc.get(a..)?.find('>')? + a + 1;
        let b = doc.get(a..)?.find('<')? + a;
        let s = doc.get(a..b)?.trim();
        (!s.is_empty()).then(|| s.to_owned())
    };
    let tid = tag("title_id").filter(|t| t.len() == 16 && t.bytes().all(|c| c.is_ascii_hexdigit()));
    if let Some(t) = &tid {
        g.content = u64::from_str_radix(t, 16)
            .ok()
            .and_then(|x| nintendo_content(Platform::WiiU, x));
        g.title_id = Some(t.to_ascii_uppercase());
        hit.conf = 95;
    }
    if let Some(pc) = tag("product_code") {
        g.region = pc.get(6..).and_then(nintendo_region);
        g.product_code = Some(pc);
    }
    g.maker =
        tag("company_code").and_then(|c| c.get(c.len().saturating_sub(2)..).map(str::to_owned));
    g.version = tag("title_version");
    g.title = tag("longname_en").map(|t| t.replace('\n', " "));
    if g.title_id.is_none() && g.product_code.is_none() {
        return None;
    }
    Some(with_game(hit, g))
}

// ---------------------------------------------------------------- NDS

/// CRC-16/MODBUS (the DS header checksum).
fn crc16(b: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &x in b {
        crc ^= u16::from(x);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// Switch homebrew executable (NRO, switchbrew): "NRO0" at 0x10, version 0 at 0x14, the NRO
/// size at 0x18, then
/// an optional asset section ("ASET": icon, NACP, RomFS as offset / size pairs from 0x08). The
/// NACP gives the title (first language entry, 0x200 bytes) and the display version (0x3060).
/// Checked on four real NROs (ftpd, Goldleaf, nxdumptool, hbmenu).
pub(crate) fn nro(v: &View) -> Option<Hit> {
    let h = v.head();
    // The 4-byte magic alone is weak evidence: also require the version word (always 0).
    if !at(h, 0x10, b"NRO0") || u32le(h, 0x14) != Some(0) {
        return None;
    }
    let size = v.size();
    let nro_size = u64::from(u32le(h, 0x18)?);
    if nro_size < 0x80 || nro_size > size {
        return None;
    }
    let mut g = GameInfo::new(Platform::Switch, Kind::Package, "NRO");
    g.content = Some(Content::App);
    let mut hit = Hit::new("exe.switch.nro", 97);
    if nro_size + 0x38 <= size {
        let Some(a) = v.at(nro_size, 0x38) else {
            return Some(with_game(hit.need(nro_size, 0x38), g));
        };
        if at(a, 0, b"ASET")
            && let (Some(no), Some(ns)) = (u64le(a, 0x18), u64le(a, 0x20))
            && ns >= 0x3070
            && nro_size
                .checked_add(no)
                .and_then(|o| o.checked_add(0x3070))
                .is_some_and(|e| e <= size)
        {
            let at_ = nro_size + no;
            match v.at(at_, 0x3070) {
                Some(n) => {
                    g.title = text_field(n, 0, 0x200).filter(|t| !t.is_empty());
                    g.version = text_field(n, 0x3060, 0x10).filter(|t| !t.is_empty());
                    hit.push("assets", true);
                }
                None => hit = hit.need(at_, 0x3070),
            }
        }
    }
    Some(with_game(hit, g))
}

/// GameCube / Wii executable (DOL, YAGCD / wiibrew): no magic, a 0x100-byte big-endian
/// header of 7 text and 11 data sections (file offsets at 0x00 / 0x1C, load addresses at 0x48
/// / 0x64, sizes at 0x90 / 0xAC), BSS address and size at 0xD8 / 0xDC, the entry point at 0xE0
/// and zero padding to 0x100. Every non-empty section must lie after the header and inside
/// the file and load into MEM1 (0x80000000-0x817FFFFF) or Wii MEM2 (0x90000000-0x93FFFFFF);
/// the entry point must fall in a text section (95). Checked on 8 real homebrew DOLs (Swiss,
/// USB Loader GX, WiiFlow, Snes9x GX).
pub(crate) fn dol(v: &View) -> Option<Hit> {
    let h = v.head();
    if h.len() < 0x100 || !h.get(0xE4..0x100)?.iter().all(|&b| b == 0) {
        return None;
    }
    let size = v.size();
    let mem =
        |a: u32| (0x8000_0000..0x8180_0000).contains(&a) || (0x9000_0000..0x9400_0000).contains(&a);
    let entry = u32be(h, 0xE0)?;
    let (mut text, mut data) = (0u32, 0u32);
    let mut entry_ok = false;
    let mut first_off = u32::MAX;
    for i in 0..18usize {
        let off = u32be(h, i * 4)?;
        let addr = u32be(h, 0x48 + i * 4)?;
        let len = u32be(h, 0x90 + i * 4)?;
        if len == 0 {
            continue;
        }
        let end = u64::from(off) + u64::from(len);
        if off < 0x100 || end > size || !mem(addr) || !mem(addr.checked_add(len - 1)?) {
            return None;
        }
        first_off = first_off.min(off);
        if i < 7 {
            text += 1;
            entry_ok |= (addr..addr.saturating_add(len)).contains(&entry);
        } else {
            data += 1;
        }
    }
    let bss_addr = u32be(h, 0xD8)?;
    let bss_ok = u32be(h, 0xDC)? == 0 || mem(bss_addr);
    if text == 0 || !entry_ok || !bss_ok || first_off != 0x100 {
        return None;
    }
    Some(
        Hit::new("exe.dol", 95)
            .d("text_sections", u64::from(text))
            .d("data_sections", u64::from(data))
            .d("entry", format!("{entry:#010x}")),
    )
}

/// NDS: Nintendo logo at 0xC0 (detector prototype) or its CRC 0xCF56 at 0x15C; title at 0 (12),
/// game code at 0xC (= GameTDB id), maker at 0x10, unit code at 0x12 (2 DSi-enhanced, 3 DSi
/// only), version at 0x1E, header CRC-16 of 0..0x15E at 0x15E.
///
/// NDS cartridge header. The Nintendo logo (or its CRC) is the usual evidence; ndstool builds
/// for flashcarts and some homebrew carry no logo, and are accepted on a valid header CRC16
/// plus ARM9 / ARM7 load addresses in main RAM / ARM7 WRAM and a 0x200 or 0x4000 header (85).
pub(crate) fn nds(v: &View) -> Option<Hit> {
    let h = v.head();
    let logo = at(h, 0xC0, &[0x24, 0xFF, 0xAE, 0x51, 0x69, 0x9A]);
    let crc = u16le(h, 0x15C) == Some(0xCF56);
    let header_ok = h.len() >= 0x160 && u16le(h, 0x15E) == Some(crc16(sl(h, 0, 0x15E)));
    let main_ram = |a: u32| (0x0200_0000..0x0240_0000).contains(&a);
    let layout_ok = header_ok
        && u32le(h, 0x20).is_some_and(|o| o >= 0x200 && u64::from(o) < v.size())
        && u32le(h, 0x28).is_some_and(main_ram)
        && u32le(h, 0x38).is_some_and(|a| main_ram(a) || (0x037F_8000..0x0381_0000).contains(&a))
        && matches!(u32le(h, 0x84), Some(0x200 | 0x4000));
    if !logo && !crc && !layout_ok {
        return None;
    }
    let code = ascii_field(h, 0xC, 4)
        .filter(|c| c.len() == 4 && c.bytes().all(|b| b.is_ascii_alphanumeric()));
    if !logo && code.is_none() && !layout_ok {
        return None;
    }
    let mut g = GameInfo::new(Platform::Nds, Kind::Image, "NDS");
    g.title = ascii_field(h, 0, 12);
    g.maker = ascii_field(h, 0x10, 2);
    g.version = u8at(h, 0x1E).map(|x| x.to_string());
    g.content = Some(Content::Game);
    let has_code = code.is_some();
    if let Some(c) = code {
        g.region = nintendo_region(&c);
        g.title_id = Some(c);
    }
    let conf = match (logo, header_ok) {
        (true, true) => 98,
        (true, false) => 95,
        _ if crc && has_code => 88,
        _ => 85,
    };
    let mut hit = Hit::new("game.nds.rom", conf).d("title", cstr(sl(h, 0, 12)));
    if let Some(u) = u8at(h, 0x12).filter(|&u| u == 2 || u == 3) {
        hit.push("dsi", if u == 3 { "dsi-only" } else { "dsi-enhanced" });
    }
    Some(with_game(hit, g))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_modbus() {
        assert_eq!(crc16(b"123456789"), 0x4B37);
    }

    #[test]
    fn rights_ids() {
        assert_eq!(
            rights_title("0100abcd123400000000000000000005.tik").as_deref(),
            Some("0100ABCD12340000")
        );
        assert_eq!(rights_title("0123456789abcdef0123456789abcdef.nca"), None);
    }
}
