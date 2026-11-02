//! PlayStation: PS5 FIH, PS4/PS5 CNT, PS3/PSP/Vita PKG, PARAM.SFO, param.json, PS3 discs.

use crate::formats::console::{Content, GameInfo, Kind, Platform, with_game};
use crate::types::Hit;
use crate::util::*;
use crate::view::View;

/// Platform from a Sony title ID (four letters, then digits).
pub(crate) fn sony_platform(title_id: &str) -> Option<Platform> {
    let p = title_id.get(..4)?;
    Some(match p {
        "PPSA" | "PPSE" | "ECAS" | "ELAS" | "ELJM" | "ELJS" => Platform::Ps5,
        "CUSA" | "CUSB" | "PCAS" | "PCJS" | "PLAS" | "PCKS" | "PLJS" | "PLJM" | "PCSK" => {
            Platform::Ps4
        }
        "SLES" | "SLUS" | "SCES" | "SCUS" | "SLPS" | "SLPM" | "SCPS" | "SLKA" => Platform::Ps1OrPs2,
        "NPXA" | "NPXB" => Platform::PsVita,
        _ if p.starts_with("PCS") => Platform::PsVita,
        _ if p.starts_with("BL") || p.starts_with("BC") => Platform::Ps3,
        _ if p.starts_with("UL") || p.starts_with("UC") => Platform::Psp,
        // NPxB/NPxA/NPxE/NPxJ PS3 digital, NPxH/NPxG/NPxZ PSP digital.
        _ if p.starts_with("NP") => match p.as_bytes().get(3)? {
            b'A' | b'B' | b'E' | b'J' => Platform::Ps3,
            b'G' | b'H' | b'Z' => Platform::Psp,
            _ => return None,
        },
        _ => return None,
    })
}

/// The service prefix of a Sony content ID.
pub(crate) fn sony_region(content_id: &str) -> Option<&'static str> {
    Some(match content_id.get(..2)? {
        "UP" => "Americas",
        "EP" => "Europe/PAL",
        "JP" => "Japan",
        "HP" => "Asia",
        "KP" => "Korea",
        "IP" => "Internal",
        _ => return None,
    })
}

/// Region from a Sony serial: the third letter of disc and PSN serials (SLES / BLUS / NPJB /
/// ULJM), the fourth of Vita cards (PCSB = Europe). PS4 / PS5 serials (CUSA, PPSA) are
/// region-free: the content ID prefix says it instead.
pub(crate) fn serial_region(serial: &str) -> Option<&'static str> {
    let b = serial.as_bytes();
    if serial.starts_with("CUSA") || serial.starts_with("PPSA") {
        return None;
    }
    let letter = if serial.starts_with("PCS") {
        match b.get(3)? {
            b'A' | b'E' => b'U',
            b'B' | b'F' => b'E',
            b'C' | b'G' => b'J',
            b'D' | b'H' => b'A',
            _ => return None,
        }
    } else {
        *b.get(2)?
    };
    Some(match letter {
        b'E' => "Europe/PAL",
        b'U' => "Americas",
        b'J' | b'P' => "Japan",
        b'K' => "Korea",
        b'A' | b'H' => "Asia",
        _ => return None,
    })
}

/// Strict `XX0000-AAAA00000_00-LABEL16CHARSXXXX` (36 characters).
pub(crate) fn is_content_id(s: &str) -> bool {
    let b = s.as_bytes();
    let up = |c: &u8| c.is_ascii_uppercase();
    let dig = |c: &u8| c.is_ascii_digit();
    b.len() == 36
        && sl(b, 0, 2).iter().all(up)
        && sl(b, 2, 6).iter().all(dig)
        && b.get(6) == Some(&b'-')
        && sl(b, 7, 11).iter().all(up)
        && sl(b, 11, 16).iter().all(dig)
        && b.get(16) == Some(&b'_')
        && sl(b, 17, 19).iter().all(dig)
        && b.get(19) == Some(&b'-')
        && from(b, 20)
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Lenient content ID (printable, dashes and underscore in place): what the detector prototype
/// accepted, kept for labelling.
fn content_id_loose(b: &[u8]) -> Option<String> {
    let s = b.get(..36)?;
    if !printable(s)
        || s.get(6) != Some(&b'-')
        || s.get(16) != Some(&b'_')
        || s.get(19) != Some(&b'-')
    {
        return None;
    }
    Some(String::from_utf8_lossy(s).to_string())
}

fn title_of(cid: &str) -> &str {
    cid.get(7..16).unwrap_or("")
}

// ---------------------------------------------------------------- PS5 FIH

/// PS5 package: `\x7FFIH`, little-endian. Checked on two real packages (UNVERIFIED beyond them):
/// 0x05 u8 0x00 debug / 0x80 retail, 0x06 le16 format, 0x10 le64 PFS offset (0x10000), 0x18 le64
/// PFS size, 0x20 le64 outer PFS superblock offset, 0x58 le64 offset of the embedded `\x7FCNT`
/// block; the 32-byte digest at 0x30 is repeated at 0x70 and 0xD0.
pub(crate) fn ps5_fih(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"\x7FFIH") {
        return None;
    }
    let size = v.size();
    let mut score = 70u8;
    if u64le(h, 0x10) == Some(0x10000) {
        score += 5;
    }
    if u64le(h, 0x28) == Some(0x10000) && u64le(h, 0x60) == Some(0x10000) {
        score += 5;
    }
    let digest = sl(h, 0x30, 0x50);
    if digest.len() == 0x20 && digest == sl(h, 0x70, 0x90) && digest == sl(h, 0xD0, 0xF0) {
        score += 10;
    }
    let cnt = u64le(h, 0x58).unwrap_or(0);
    let cnt_ok =
        cnt > 0 && cnt.is_multiple_of(0x1000) && cnt.checked_add(0x80).is_some_and(|e| e <= size);
    if cnt_ok {
        score += 5;
    }
    // Plausibility from the console prototype: a damaged header keeps the label, at low trust.
    let signed = u8at(h, 5);
    let format = u16le(h, 6);
    let pfs_off = u64le(h, 0x10).unwrap_or(0);
    let pfs_size = u64le(h, 0x18).unwrap_or(0);
    let sb_off = u64le(h, 0x20).unwrap_or(0);
    let pfs_end = pfs_off.checked_add(pfs_size);
    let plausible = matches!(signed, Some(0 | 0x80))
        && format.is_some_and(|f| (1..=16).contains(&f))
        && pfs_off != 0
        && pfs_off.is_multiple_of(0x10000)
        && pfs_size != 0
        && pfs_end.is_some_and(|e| e <= size && sb_off >= pfs_off && sb_off < e && cnt >= e)
        && cnt_ok;
    let mut hit = Hit::new(
        "game.ps5.pkg",
        if plausible { score } else { score.min(60) },
    );
    hit.push("format_version", format.unwrap_or(0));
    hit.push("pfs_offset", pfs_off);
    hit.push("pfs_size", pfs_size);
    hit.push("cnt_offset", cnt);
    if !plausible {
        hit.push("note", "FIH header fields inconsistent with the file size");
    }
    let mut g = GameInfo::new(Platform::Ps5, Kind::Package, "PS5 FIH");
    g.debug = match signed {
        Some(0) => Some(true),
        Some(0x80) => Some(false),
        _ => None,
    };
    // The trailing ZIP (PlayGo metadata) names config/<content ID>/...
    let tail = v.known_tail();
    if let Some(cid) = zip_names(tail)
        .iter()
        .find_map(|n| {
            n.strip_prefix("config/")
                .and_then(|r| r.split('/').next())
                .map(str::to_owned)
        })
        .filter(|c| is_content_id(c))
    {
        g.set_content_id(cid);
    }
    if cnt_ok {
        let had = g.content_id.clone();
        let (hh, from_cnt) = parse_cnt(v, cnt, hit, &mut g);
        hit = hh;
        if from_cnt {
            hit.conf = if plausible { 99 } else { 80 };
            if had.is_some() && had != g.content_id {
                hit.push(
                    "note",
                    "content ID of the trailing ZIP differs from the CNT block",
                );
            }
        }
    }
    Some(with_game(hit, g))
}

/// File names from a ZIP central directory held entirely in `tail` (which ends the file).
fn zip_names(tail: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let Some(last) = tail.len().checked_sub(22) else {
        return names;
    };
    let lo = tail.len().saturating_sub(22 + 0xffff);
    let Some(eocd) = (lo..=last).rev().find(|&i| at(tail, i, b"PK\x05\x06")) else {
        return names;
    };
    let (Some(cd_size), Some(count)) = (u32le(tail, eocd + 12), u16le(tail, eocd + 10)) else {
        return names;
    };
    let Some(mut p) = eocd.checked_sub(us(u64::from(cd_size))) else {
        return names;
    };
    for _ in 0..count.min(4096) {
        if !at(tail, p, b"PK\x01\x02") {
            break;
        }
        let (Some(n), Some(x), Some(c)) = (
            u16le(tail, p + 28),
            u16le(tail, p + 30),
            u16le(tail, p + 32),
        ) else {
            break;
        };
        if let Some(name) = text_field(tail, p + 46, usize::from(n)) {
            names.push(name);
        }
        p = p.saturating_add(46 + usize::from(n) + usize::from(x) + usize::from(c));
    }
    names
}

// ---------------------------------------------------------------- CNT (PS4/PS5)

/// Parses a PS4/PS5 `\x7FCNT` block at `base` (big-endian; needs ~64 KiB): content ID at 0x40,
/// entry count 0x10, entry table offset 0x18, DRM type 0x70, content type 0x74. Entries are 0x20
/// bytes: id, name offset, flags1 (bit 31 encrypted), flags2, data offset, size (relative to
/// the CNT start). Returns whether the content ID came from the block.
fn parse_cnt(v: &View, base: u64, mut hit: Hit, g: &mut GameInfo) -> (Hit, bool) {
    let c = v.from(base);
    if !at(c, 0, b"\x7FCNT") {
        if c.len() < 4 {
            hit = hit.need(base, 0x10000);
        }
        return (hit, false);
    }
    let mut from_cnt = false;
    if let Some(cid) = content_id_loose(from(c, 0x40)) {
        let title = title_of(&cid).to_owned();
        if g.content_id.as_deref().is_some_and(|x| x != cid) {
            g.title_id = None;
            g.region = None;
        }
        if let Some(p) = sony_platform(&title) {
            hit.push("title_platform", platform_name(Some(p)));
        }
        g.set_content_id(cid);
        from_cnt = true;
    }
    if let (Some(drm), Some(ct)) = (u32be(c, 0x70), u32be(c, 0x74)) {
        hit.push("drm_type", format!("{drm:#x}"));
        hit.push("content_type", format!("{ct:#x}"));
        let kind = match ct {
            0x1A => "game-data",
            0x1B => "additional-content",
            0x1C => "additional-license",
            0x1E => "delta-patch?",
            0x20 => "ps5-app?",
            _ => "unknown",
        };
        hit.push("content_kind", kind);
        // The SFO / param.json category refines this (a PS4 patch is also 0x1A).
        g.content = match ct {
            0x1B => Some(Content::Dlc),
            0x1C => Some(Content::License),
            0x1E => Some(Content::Patch),
            _ => g.content,
        };
    }
    if contains(pre(c, 0x1000), b"PPRPLAIN-NOAUTH!") {
        hit.push("plain_marker", "PPRPLAIN-NOAUTH!");
    }
    let (Some(count), Some(table)) = (u32be(c, 0x10), u32be(c, 0x18)) else {
        return (hit, from_cnt);
    };
    if count == 0 || count > 4096 {
        return (hit, from_cnt);
    }
    let table = us(u64::from(table));
    let mut names_tbl: Option<(u32, u32)> = None;
    let mut ents: Vec<(u32, u32, u32, u32, u32)> = Vec::new();
    for i in 0..usize::try_from(count).unwrap_or(0).min(512) {
        let o = table.saturating_add(i * 32);
        let (Some(id), Some(no), Some(off), Some(sz)) = (
            u32be(c, o),
            u32be(c, o + 4),
            u32be(c, o + 16),
            u32be(c, o + 20),
        ) else {
            // Entry table not in hand: ask for it.
            let start = base.saturating_add(table as u64);
            hit = hit.need(start, u64::from(count) * 0x20);
            break;
        };
        let fl = u32be(c, o + 8).unwrap_or(0);
        if id == 0x200 {
            names_tbl = Some((off, sz));
        }
        ents.push((id, no, off, sz, fl));
    }
    hit.push("entries", ents.len());
    if let Some((no, ns)) = names_tbl
        && let Some(tbl) = v.at(
            base.saturating_add(u64::from(no)),
            us(u64::from(ns).min(1 << 20)),
        )
    {
        let files: Vec<String> = ents
            .iter()
            .filter(|e| e.1 > 0)
            .filter_map(|e| tbl.get(us(u64::from(e.1))..).map(cstr))
            .filter(|s| !s.is_empty())
            .take(64)
            .collect();
        if !files.is_empty() {
            hit.push("sc_files", files);
        }
    }
    for &(id, _, off, sz, fl) in &ents {
        let encrypted = fl & 0x8000_0000 != 0;
        if !(id == 0x2000 || id == 0x1000) || encrypted || sz == 0 || sz > 1 << 20 {
            continue;
        }
        let start = base.saturating_add(u64::from(off));
        if start.saturating_add(u64::from(sz)) > v.size() {
            continue;
        }
        match v.at(start, us(u64::from(sz))) {
            Some(blob) if id == 0x2000 => param_json(blob, &mut hit, g),
            Some(blob) => {
                if let Some(sfo) = parse_sfo(blob) {
                    sfo_fill(g, &sfo, &mut hit);
                }
            }
            None => hit = hit.need(start, u64::from(sz)),
        }
    }
    (hit, from_cnt)
}

fn platform_name(p: Option<Platform>) -> &'static str {
    match p {
        Some(Platform::Ps5) => "ps5",
        Some(Platform::Ps4) => "ps4",
        Some(Platform::Ps3) => "ps3",
        Some(Platform::Psp) => "psp",
        Some(Platform::PsVita) => "psvita",
        Some(Platform::Ps1OrPs2) => "ps1/ps2",
        _ => "",
    }
}

// ---------------------------------------------------------------- param.json

/// PS5 `sce_sys/param.json` (CNT entry 0x2000, plaintext in both samples).
fn param_json(j: &[u8], hit: &mut Hit, g: &mut GameInfo) {
    match serde_json::from_slice::<serde_json::Value>(j) {
        Ok(doc) => {
            let s = |k: &str| doc.get(k).and_then(|x| x.as_str()).map(str::to_owned);
            if let Some(cid) = s("contentId").filter(|c| is_content_id(c))
                && g.content_id.is_none()
            {
                g.set_content_id(cid);
            }
            if let Some(t) = s("titleId") {
                g.title_id = Some(t);
            }
            if let Some(v) = s("contentVersion") {
                g.version = Some(v);
            }
            if let Some(fw) = s("requiredSystemSoftwareVersion") {
                g.required_fw = sony_fw_hex(&fw);
            }
            if let Some(sdk) = s("sdkVersion")
                .and_then(|x| sony_fw_hex(&x))
                .filter(|x| x != "0.00")
            {
                g.sdk = Some(sdk);
            }
            // 0 = game application, 65536 = non-game application (media apps); DLC packages
            // carry an `addcont` entitlement block without `gameIntent`. UNVERIFIED beyond the
            // two local game packages (category 0).
            match doc
                .get("applicationCategoryType")
                .and_then(serde_json::Value::as_u64)
            {
                Some(0) if g.content.is_none() => g.content = Some(Content::Game),
                Some(0x10000) => g.content = Some(Content::App),
                _ => {}
            }
            if let Some(ids) = doc
                .get("savedata")
                .and_then(|x| x.get("titleIdForTransferringPs4"))
                .and_then(serde_json::Value::as_array)
            {
                let ids: Vec<String> = ids
                    .iter()
                    .filter_map(|x| x.as_str())
                    .filter(|x| crate::formats::console::db_id_valid(Platform::Ps4, x))
                    .take(32)
                    .map(str::to_owned)
                    .collect();
                if !ids.is_empty() {
                    hit.push("ps4_title_ids", ids);
                }
            }
            for (k, key) in [
                ("masterVersion", "master_version"),
                ("applicationCategoryType", "category_type"),
                ("applicationDrmType", "application_drm_type"),
                ("sdkVersion", "sdk_version"),
                ("conceptId", "concept_id"),
                ("downloadDataSize", "download_data_size"),
            ] {
                if let Some(x) = doc.get(k) {
                    hit.push(key, x.to_string().trim_matches('"').to_string());
                }
            }
            if let Some(lp) = doc.get("localizedParameters") {
                let def = lp
                    .get("defaultLanguage")
                    .and_then(|x| x.as_str())
                    .unwrap_or("en-US");
                let title = lp
                    .get(def)
                    .and_then(|x| x.get("titleName"))
                    .and_then(|x| x.as_str())
                    .or_else(|| {
                        lp.as_object()?
                            .values()
                            .find_map(|x| x.get("titleName")?.as_str())
                    });
                if let Some(t) = title {
                    g.title = Some(t.to_owned());
                }
                let n = lp.as_object().map_or(0, |o| o.len().saturating_sub(1));
                g.languages = u32::try_from(n).ok();
            }
            if let Some(a) = doc
                .get("pubtools")
                .and_then(|x| x.get("creationDate"))
                .and_then(|x| x.as_str())
                .filter(|a| is_set_date(a))
            {
                hit.push("creation_date", a);
            }
        }
        // Truncated or odd JSON: scan for the keys instead.
        Err(_) => param_json_scan(j, g),
    }
}

/// A `pubtools.creationDate` that was actually set: unset ones are the Unix epoch
/// (`1970-01-01 00:00:00` in HZR.pkg); anything before the PS4 (2013) is not a real date.
fn is_set_date(s: &str) -> bool {
    s.get(..4)
        .and_then(|y| y.parse::<u32>().ok())
        .is_some_and(|y| y >= 2013)
}

fn param_json_scan(blob: &[u8], g: &mut GameInfo) {
    let doc = String::from_utf8_lossy(blob);
    if let Some((cid, _)) = json_str(&doc, "contentId", 0).filter(|(c, _)| is_content_id(c))
        && g.content_id.is_none()
    {
        g.set_content_id(cid);
    }
    if let Some((tid, _)) = json_str(&doc, "titleId", 0) {
        g.title_id = Some(tid);
    }
    if let Some((v, _)) = json_str(&doc, "contentVersion", 0) {
        g.version = Some(v);
    }
    if let Some((fw, _)) = json_str(&doc, "requiredSystemSoftwareVersion", 0) {
        g.required_fw = sony_fw_hex(&fw);
    }
    if let Some((sdk, _)) = json_str(&doc, "sdkVersion", 0) {
        g.sdk = sony_fw_hex(&sdk).filter(|x| x != "0.00");
    }
    let title = json_str(&doc, "defaultLanguage", 0)
        .and_then(|(lang, _)| {
            let k = doc.find(&format!("\"{lang}\""))?;
            json_str(&doc, "titleName", k)
        })
        .or_else(|| json_str(&doc, "titleName", 0));
    if let Some((t, _)) = title {
        g.title = Some(t);
    }
}

/// The value of the first `"key": "..."` at or after byte `from`, unescaped.
pub(crate) fn json_str(doc: &str, key: &str, from: usize) -> Option<(String, usize)> {
    let pat = format!("\"{key}\"");
    let k = doc
        .get(from..)?
        .find(&pat)?
        .checked_add(from)?
        .checked_add(pat.len())?;
    let rest = doc
        .get(k..)?
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let start = doc.len() - rest.len();
    let mut out = String::new();
    let mut chars = rest.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, start + i + 1)),
            '\\' => match chars.next()?.1 {
                'u' => {
                    let hex: String = (0..4).filter_map(|_| chars.next().map(|x| x.1)).collect();
                    out.push(
                        char::from_u32(u32::from_str_radix(&hex, 16).ok()?).unwrap_or('\u{fffd}'),
                    );
                }
                'n' => out.push('\n'),
                't' => out.push('\t'),
                other => out.push(other),
            },
            c => out.push(c),
        }
        if out.len() > 4096 {
            return None;
        }
    }
    None
}

/// "0x0900000000000000" -> "9.00": the top two bytes are BCD major.minor.
pub(crate) fn sony_fw_hex(s: &str) -> Option<String> {
    let v = u64::from_str_radix(s.strip_prefix("0x")?, 16).ok()?;
    let major = (v >> 56) as u8;
    let minor = (v >> 48) as u8;
    let bcd = |b: u8| u32::from(b >> 4) * 10 + u32::from(b & 0xf);
    Some(format!("{}.{:02}", bcd(major), bcd(minor)))
}

// ---------------------------------------------------------------- PS4 CNT at 0

/// PS4 package: `\x7FCNT` at 0, big-endian. Layout from the PS4 developer wiki / LibOrbisPkg,
/// cross-checked by the PS5 CNT, which shares the first 0x80 bytes, and on two real homebrew
/// fpkgs (ezRemote Client, Store); no retail package. A bare CNT with a PPSA title is reported
/// as a PS5 package.
pub(crate) fn ps4_cnt(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"\x7FCNT") {
        return None;
    }
    let cid = content_id_loose(from(h, 0x40))?;
    let plat = sony_platform(title_of(&cid));
    let label = if plat == Some(Platform::Ps5) {
        "game.ps5.pkg"
    } else {
        "game.ps4.pkg"
    };
    // Fake-signed and homebrew packages use custom title IDs (RMTC00001, NPXS39041): with
    // the entry table and the body inside the file the CNT is structurally sound (95).
    let mut conf = if plat == Some(Platform::Ps4) { 97 } else { 85 };
    let size = v.size();
    let count = u32be(h, 0x10).unwrap_or(0);
    let table = u64::from(u32be(h, 0x18).unwrap_or(0));
    let body_end = u64be(h, 0x20)
        .zip(u64be(h, 0x28))
        .and_then(|(o, s)| o.checked_add(s));
    let plausible = (1..=4096).contains(&count)
        && table.saturating_add(u64::from(count) * 0x20) <= size
        && body_end.is_some_and(|e| e <= size);
    if !plausible {
        conf = conf.min(80);
    } else {
        // A sound CNT is 95 whatever the title id says; a recognised PS4 id keeps 97.
        conf = conf.max(95);
    }
    let mut g = GameInfo::new(plat.unwrap_or(Platform::Ps4), Kind::Package, "PS4 PKG");
    if plat == Some(Platform::Ps5) {
        g.format = "CNT (PS5 title)";
    }
    let hit = Hit::new(label, conf);
    let (mut hit, _) = parse_cnt(v, 0, hit, &mut g);
    if !plausible {
        hit.push("note", "CNT entry table or body extends past the file end");
    }
    Some(with_game(hit, g))
}

// ---------------------------------------------------------------- PS3 / PSP / Vita PKG

/// `\x7FPKG`, big-endian. Checked on three homebrew debug packages only; layout from the PS3
/// developer wiki: 0x04 revision (0x8000 retail, 0x0000 debug), 0x06 type (1 PS3, 2 PSP/Vita),
/// 0x08 metadata offset, 0x0C metadata count, 0x18 total size (= file length), 0x30 content ID. The metadata
/// list is plaintext (id, size, data): 0x02 content type, 0x05 package version, 0x0E the
/// offset and size of a plaintext PARAM.SFO copy (Vita). The PS3 PARAM.SFO itself is inside
/// the data area: AES-CTR encrypted with a console key in retail packages (revision 0x8000);
/// debug packages (revision 0x0000, every homebrew package seen) use a keystream derived from
/// the header digest alone (SHA-1, no secret), which is not decoded here yet. So no title for
/// PS3 packages, for retail ones by necessity.
pub(crate) fn ps3_pkg(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"\x7FPKG") {
        return None;
    }
    let rev = u16be(h, 4)?;
    let typ = u16be(h, 6)?;
    let cid = content_id_loose(from(h, 0x30));
    let mut plat = cid.as_deref().and_then(|c| sony_platform(title_of(c)));
    let mut conf = if cid.is_some() { 96 } else { 75 };
    let total = u64be(h, 0x18);
    let size_ok = total == Some(v.size());
    if !size_ok {
        conf = conf.min(80);
    }
    let mut hit = Hit::new("game.ps3.pkg", conf)
        .d(
            "revision",
            if rev & 0x8000 != 0 { "retail" } else { "debug" },
        )
        .d("pkg_type", typ);
    if !size_ok {
        hit.push(
            "note",
            "header total size differs from the file length (truncated or split?)",
        );
    }
    if at(h, 0xC0, b"\x7Fext") {
        hit.push("ext_header", true);
    }
    let mut g = GameInfo::new(Platform::PspOrVita, Kind::Package, "PS3 PKG");
    g.debug = match rev {
        0 => Some(true),
        0x8000 => Some(false),
        _ => None,
    };
    // Metadata entries (plaintext).
    let md_off = u64::from(u32be(h, 8).unwrap_or(0));
    let md_count = u32be(h, 12).unwrap_or(0).min(64);
    let mut sfo_at: Option<(u64, u64)> = None;
    if md_off >= 0x80 && md_off < v.size() && md_count > 0 {
        match v.at(md_off, 0x400) {
            Some(md) => {
                let mut o = 0usize;
                for _ in 0..md_count {
                    let (Some(id), Some(sz)) = (u32be(md, o), u32be(md, o + 4)) else {
                        break;
                    };
                    let d = sln(md, o + 8, us(u64::from(sz)));
                    match id {
                        2 => {
                            if let Some(ct) = u32be(d, 0) {
                                hit.push("content_type", format!("{ct:#x}"));
                                let (p, c) = pkg_content_type(ct);
                                if p.is_some() {
                                    plat = p;
                                }
                                if c.is_some() {
                                    g.content = c;
                                }
                            }
                        }
                        5 => {
                            if let Some(pv) = u16be(d, 2) {
                                hit.push(
                                    "package_version",
                                    format!("{:02x}.{:02x}", pv >> 8, pv & 0xff),
                                );
                            }
                        }
                        0xE => {
                            if let (Some(so), Some(ss)) = (u32be(d, 0), u32be(d, 4)) {
                                sfo_at = Some((u64::from(so), u64::from(ss)));
                            }
                        }
                        _ => {}
                    }
                    o = o.saturating_add(8).saturating_add(us(u64::from(sz)));
                    if o >= md.len() {
                        break;
                    }
                }
            }
            None => hit = hit.need(md_off, 0x400),
        }
    }
    hit.relabel(match (typ, plat) {
        (_, Some(Platform::PsVita)) => "game.vita.pkg",
        (_, Some(Platform::Psp)) => "game.psp.pkg",
        (1, _) | (_, Some(Platform::Ps3)) => "game.ps3.pkg",
        (2, _) => "game.psp.pkg",
        _ => "game.ps3.pkg",
    });
    g.platform = match (typ, plat) {
        (_, Some(p @ (Platform::Psp | Platform::PsVita | Platform::Ps3))) => p,
        (1, _) => Platform::Ps3,
        _ => Platform::PspOrVita,
    };
    if let Some(c) = cid {
        g.set_content_id(c);
    }
    if let Some((so, ss)) = sfo_at.filter(|&(o, s)| (20..=0x10000).contains(&s) && o < v.size()) {
        match v.at(so, us(ss)) {
            Some(b) => {
                if let Some(sfo) = parse_sfo(b) {
                    sfo_fill(&mut g, &sfo, &mut hit);
                }
            }
            None => hit = hit.need(so, ss),
        }
    }
    Some(with_game(hit, g))
}

/// PKG metadata content type -> (platform, content). From the PS3 developer wiki.
fn pkg_content_type(ct: u32) -> (Option<Platform>, Option<Content>) {
    use Platform::*;
    match ct {
        0x05 => (Some(Ps3), Some(Content::Game)),
        // 0x04 "game data" is a patch or add-on data: undecided.
        0x04 => (Some(Ps3), None),
        0x06 | 0x12 => (Some(Ps3), Some(Content::Game)),
        0x07 | 0x0E | 0x0F | 0x10 | 0x14 => (None, Some(Content::Game)),
        0x09 => (Some(Ps3), Some(Content::Theme)),
        0x0B => (None, Some(Content::License)),
        0x0A | 0x0C | 0x0D | 0x11 | 0x19 => (None, Some(Content::App)),
        0x15 => (Some(PsVita), Some(Content::Game)),
        0x16 => (Some(PsVita), Some(Content::Dlc)),
        0x17 | 0x18 => (Some(PsVita), Some(Content::App)),
        0x1F => (Some(PsVita), Some(Content::Theme)),
        _ => (None, None),
    }
}

// ---------------------------------------------------------------- PARAM.SFO

/// PARAM.SFO key/value pairs (strings and integers as text).
pub(crate) fn parse_sfo(b: &[u8]) -> Option<Vec<(String, String)>> {
    if !at(b, 0, b"\0PSF") || u32le(b, 4)? != 0x0101 {
        return None;
    }
    let keys = us(u64::from(u32le(b, 8)?));
    let data = us(u64::from(u32le(b, 12)?));
    let n = us(u64::from(u32le(b, 16)?));
    if n == 0 || n > 256 || keys < 20 + n * 16 || data < keys || data > b.len() {
        return None;
    }
    let mut out = Vec::new();
    for i in 0..n {
        let e = 20 + i * 16;
        let kofs = usize::from(u16le(b, e)?);
        let fmt = u16le(b, e + 2)?;
        let len = us(u64::from(u32le(b, e + 4)?));
        let dofs = us(u64::from(u32le(b, e + 12)?));
        let Some(room) = data.checked_sub(keys).and_then(|r| r.checked_sub(kofs)) else {
            continue;
        };
        let Some(key) = ascii_field(b, keys + kofs, 32.min(room)) else {
            continue;
        };
        let value = match fmt {
            0x0404 => format!("{}", u32le(b, data.saturating_add(dofs))?),
            0x0204 | 0x0004 => {
                text_field(b, data.saturating_add(dofs), len.min(4096)).unwrap_or_default()
            }
            _ => continue,
        };
        out.push((key, value));
    }
    Some(out)
}

pub(crate) fn sfo_fill(g: &mut GameInfo, sfo: &[(String, String)], hit: &mut Hit) {
    let get = |k: &str| sfo.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    if let Some(t) = get("TITLE").filter(|t| !t.is_empty()) {
        g.title = Some(t);
    }
    if let Some(t) = get("TITLE_ID")
        .or_else(|| get("DISC_ID"))
        .map(|t| t.replace('-', ""))
        .filter(|t| !t.is_empty())
    {
        g.title_id = Some(t);
    }
    if let Some(c) = get("CONTENT_ID").filter(|c| is_content_id(c))
        && g.content_id.is_none()
    {
        g.set_content_id(c);
    }
    if let Some(v) = get("APP_VER")
        .or_else(|| get("VERSION"))
        .or_else(|| get("DISC_VERSION"))
        .filter(|v| !v.is_empty())
    {
        g.version = Some(v);
    }
    if let Some(c) = get("CATEGORY").filter(|c| !c.is_empty()) {
        if let Some(k) = sfo_category(&c, g.platform) {
            g.content = Some(k);
        }
        hit.push("category", c);
    }
    // SYSTEM_VER / PS3_SYSTEM_VER are integers like 0x05050000 (decimal from parse_sfo) or
    // strings like "03.5000"; PSP_SYSTEM_VER is "6.60".
    for k in [
        "SYSTEM_VER",
        "PS3_SYSTEM_VER",
        "PSP_SYSTEM_VER",
        "PSP2_SYSTEM_VER",
    ] {
        if let Some(v) = get(k) {
            g.required_fw = Some(match v.parse::<u32>() {
                Ok(n) => {
                    let bcd = |b: u32| (b >> 4) * 10 + (b & 0xf);
                    format!("{}.{:02}", bcd(n >> 24), bcd((n >> 16) & 0xff))
                }
                Err(_) => v,
            });
            break;
        }
    }
}

/// PARAM.SFO `CATEGORY`: PS4 / Vita `gd` `gp` `ac`, PS3 `DG` `HG` `GD` `AP`, PSP `UG` `MG` `PG`
/// `ME` (from the PS3 / PS4 developer wikis; UNVERIFIED on real files).
///
/// PS4 / PS5 `gd*` sub-categories (PS4 developer wiki, Param.sfo): `gda` system application,
/// `gdc` non-game big app, `gdd` BG application, `gde` non-game mini app, `gdk` / `gdl` video
/// service and cloud apps; `gpc` / `gpd` / `gpe` / `gpk` / `gpl` their patches. Seen on real
/// homebrew packages: `gde` (ezRemote Client, the homebrew Store).
fn sfo_category(c: &str, platform: Platform) -> Option<Content> {
    if matches!(platform, Platform::Ps4 | Platform::Ps5) {
        match c {
            "gda" => return Some(Content::System),
            "gdc" | "gdd" | "gde" | "gdk" | "gdl" => return Some(Content::App),
            "gpc" | "gpd" | "gpe" | "gpk" | "gpl" => return Some(Content::Patch),
            _ => {}
        }
    }
    Some(match c {
        "gd" | "gdc" | "DG" | "HG" | "UG" | "MG" | "EG" | "ME" | "2P" | "1P" | "PP" => {
            Content::Game
        }
        "gp" | "gpc" | "GD" | "PG" => Content::Patch,
        "ac" | "AT" => Content::Dlc,
        "AP" | "AM" | "AV" | "BV" | "WT" | "HM" | "CB" | "gda" | "gdb" => Content::App,
        "gdd" | "gde" => Content::System,
        _ => return None,
    })
}

/// PSP PBP (`EBOOT.PBP`): `\0PBP`, version, then eight little-endian offsets (PARAM.SFO,
/// ICON0, ICON1, PIC0, PIC1, SND0, DATA.PSP, DATA.PSAR); the PARAM.SFO is plaintext. A
/// DATA.PSP that is a plain ELF is homebrew: its DISC_ID is the pspsdk default (UCJS10041, a
/// real retail serial) and is kept out of the ids. Checked on two real homebrew EBOOTs.
pub(crate) fn psp_pbp(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"\0PBP") || u32le(h, 8)? != 0x28 {
        return None;
    }
    let size = v.size();
    let mut offs = [0u64; 8];
    for (i, o) in offs.iter_mut().enumerate() {
        *o = u64::from(u32le(h, 8 + i * 4)?);
    }
    let [sfo_at, icon0, _, _, _, _, data_psp, data_psar] = offs;
    if offs.windows(2).any(|w| w.first() > w.get(1)) || data_psar > size {
        return None;
    }
    let mut g = GameInfo::new(Platform::Psp, Kind::Package, "PBP");
    let mut hit = Hit::new("game.psp.pbp", 98);
    let sfo_len = icon0 - sfo_at;
    if !(20..=64 * 1024).contains(&sfo_len) {
        return Some(with_game(hit, g));
    }
    let Some(b) = v.at(sfo_at, us(sfo_len)) else {
        return Some(with_game(hit.need(sfo_at, sfo_len), g));
    };
    if let Some(kv) = parse_sfo(b) {
        sfo_fill(&mut g, &kv, &mut hit);
    }
    // DATA.PSP: ~PSP (encrypted, PSN / retail) or a plain ELF (homebrew).
    if data_psp < data_psar {
        match v.at(data_psp, 4) {
            Some(d) if d == b"\x7FELF" => {
                hit.push("homebrew", true);
                if let Some(id) = g.title_id.take() {
                    hit.push("disc_id", id);
                }
                g.content = Some(Content::App);
            }
            Some(_) => {}
            None => hit = hit.need(data_psp, 4),
        }
    }
    Some(with_game(hit, g))
}

/// A loose PARAM.SFO (folder dumps of PS3 / PSP / Vita / PS4 games).
pub(crate) fn param_sfo_file(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"\0PSF") {
        return None;
    }
    let complete = v.size() <= 1 << 20 && h.len() as u64 >= v.size();
    let parsed = if complete { parse_sfo(h) } else { None };
    let Some(sfo) = parsed else {
        let hit = Hit::new("game.ps.param_sfo", 70);
        let hit = if !complete && v.size() <= 1 << 20 {
            hit.need(0, v.size())
        } else {
            hit
        };
        return Some(with_game(
            hit,
            GameInfo::new(Platform::PspOrVita, Kind::Metadata, "PARAM.SFO"),
        ));
    };
    let has = |k: &str| sfo.iter().any(|(key, _)| key == k);
    let title_id = sfo
        .iter()
        .find(|(k, _)| k == "TITLE_ID")
        .map(|(_, v)| v.clone());
    let platform = title_id
        .as_deref()
        .and_then(sony_platform)
        .or(if has("PS3_SYSTEM_VER") {
            Some(Platform::Ps3)
        } else if has("PSP2_SYSTEM_VER") {
            Some(Platform::PsVita)
        } else if has("PSP_SYSTEM_VER") {
            Some(Platform::Psp)
        } else {
            None
        })
        .unwrap_or(Platform::PspOrVita);
    let mut g = GameInfo::new(platform, Kind::Metadata, "PARAM.SFO");
    let mut hit = Hit::new("game.ps.param_sfo", 95);
    sfo_fill(&mut g, &sfo, &mut hit);
    Some(with_game(hit, g))
}

// ---------------------------------------------------------------- loose param.json

/// A loose PS5 `sce_sys/param.json` (folder dumps): JSON with `titleId` and `contentId` or
/// `localizedParameters` near the start. Whole files up to 1 MiB are asked for.
pub(crate) fn param_json_file(v: &View) -> Option<Hit> {
    let h = v.head();
    let start = h
        .iter()
        .take(64)
        .position(|c| !c.is_ascii_whitespace() && !matches!(c, 0xEF | 0xBB | 0xBF))?;
    if u8at(h, start) != Some(b'{') {
        return None;
    }
    let lead = pre(h, 0x4000);
    let tid_key = contains(lead, b"\"titleId\"");
    if !(tid_key
        && (contains(lead, b"\"contentId\"") || contains(lead, b"\"localizedParameters\"")))
    {
        return None;
    }
    let size = v.size();
    let mut g = GameInfo::new(Platform::Ps5, Kind::Metadata, "param.json");
    let mut hit = Hit::new("game.ps5.param_json", 85);
    if size > 1 << 20 {
        return None;
    }
    match v.at(0, us(size)) {
        Some(all) => {
            param_json(all, &mut hit, &mut g);
            if g.title_id.as_deref().and_then(sony_platform) != Some(Platform::Ps5) {
                return None;
            }
            hit.conf = if g.content_id.is_some() { 96 } else { 90 };
        }
        None => hit = hit.need(0, size),
    }
    Some(with_game(hit, g))
}

// ---------------------------------------------------------------- PS3 disc

/// PS3 disc images: "PlayStation3" at 0x800 and the title ID ("BLES-00680") at 0x810.
/// UNVERIFIED. The PARAM.SFO (title, version) is read by the ISO walker ([`super::iso`]).
pub(crate) fn ps3_disc(v: &View) -> Option<Hit> {
    let b = v.at(0x800, 0x20)?;
    if !at(b, 0, b"PlayStation3") {
        return None;
    }
    let mut g = GameInfo::new(Platform::Ps3, Kind::Image, "PS3 ISO");
    g.title_id = ascii_field(b, 0x10, 16).map(|s| s.replace('-', ""));
    g.content = Some(Content::Game);
    let mut hit = Hit::new("game.ps3.iso", 95);
    // The ISO 9660 volume descriptor: the walker in iso.rs takes over once it is known.
    if !v.has(0x8000, 0x800) && v.size() > 0x8800 {
        hit = hit.need(0x8000, 0x10000);
    }
    Some(with_game(hit, g))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_creation_dates_are_dropped() {
        assert!(!is_set_date("1970-01-01 00:00:00"));
        assert!(!is_set_date(""));
        assert!(!is_set_date("garbage"));
        assert!(is_set_date("2026-10-01 06:04:54"));
    }

    #[test]
    fn content_ids() {
        assert!(is_content_id("EP1805-PPSA12544_00-HKSILKSONGPS5000"));
        assert!(!is_content_id("EP1805-PPSA12544_00-HKSILKSONGPS500"));
        assert!(!is_content_id("ep1805-PPSA12544_00-HKSILKSONGPS5000"));
        assert_eq!(
            sony_region("UP0001-CUSA00001_00-0000000000000000"),
            Some("Americas")
        );
        assert_eq!(sony_platform("PPSA12544"), Some(Platform::Ps5));
        assert_eq!(sony_platform("NPUB30001"), Some(Platform::Ps3));
        assert_eq!(sony_platform("PCSB00001"), Some(Platform::PsVita));
    }

    #[test]
    fn firmware_hex_is_bcd() {
        assert_eq!(sony_fw_hex("0x0900000000000000").as_deref(), Some("9.00"));
        assert_eq!(sony_fw_hex("0x1000000000000000").as_deref(), Some("10.00"));
        assert_eq!(sony_fw_hex("0x0450000000000000").as_deref(), Some("4.50"));
    }

    #[test]
    fn json_strings_unescape() {
        let doc = r#"{"a": "x\"y™", "titleName" : "T"}"#;
        assert_eq!(json_str(doc, "a", 0).unwrap().0, "x\"y\u{2122}");
        assert_eq!(json_str(doc, "titleName", 0).unwrap().0, "T");
        assert_eq!(json_str(doc, "titleName", usize::MAX), None);
    }
}
