//! Archives and packages needing structure: ZIP (central directory at the tail, then the
//! container type from the file list), RAR4/RAR5 (header CRCs), 7z (start header CRC), tar
//! (checksum), gzip/zstd (inner peek), xz (stream flags CRC), LZMA-alone, xar (TOC inflate) and
//! OLE compound files (MSI, legacy Office). Simple magics live in `data/signatures/archive.toml`.

use std::io::Read;

use crate::name::RISKY_INSIDE;
use crate::types::Hit;
use crate::util::*;
use crate::view::View;

pub(crate) static CHECKS: &[(&str, crate::formats::Check)] = &[
    ("zip", zip),
    ("rar", rar),
    ("7z", sevenz),
    ("tar", tar),
    ("compressors", compressors),
    ("xar", xar),
    ("cfb", cfb),
];

// ---------------------------------------------------------------- ZIP

fn zip(v: &View) -> Option<Hit> {
    let h = v.head();
    let empty = at(h, 0, b"PK\x05\x06");
    let spanned = at(h, 0, b"PK\x07\x08") || at(h, 0, b"PK00");
    if !at(h, 0, b"PK\x03\x04") && !empty && !spanned {
        return None;
    }
    if empty {
        return Some(Hit::new("archive.zip", 90).d("entries", 0u64));
    }
    let size = v.size();
    let mut names: Vec<String> = Vec::new();
    let mut encrypted = false;
    let mut first_stored: Option<(String, Vec<u8>)> = None;
    // Vita VPK: the local entry of sce_sys/param.sfo (method, data offset, compressed size).
    let mut vpk_sfo: Option<(u16, usize, usize)> = None;
    // Walk the local headers within the head.
    let mut o = if spanned { 4 } else { 0 };
    while at(h, o, b"PK\x03\x04") && names.len() < 4096 {
        let flags = u16le(h, o + 6)?;
        let method = u16le(h, o + 8)?;
        let csz = us(u64::from(u32le(h, o + 18)?));
        let nl = usize::from(u16le(h, o + 26)?);
        let el = usize::from(u16le(h, o + 28)?);
        let Some(n) = h.get(o + 30..o + 30 + nl) else {
            break;
        };
        let name = String::from_utf8_lossy(n).to_string();
        if flags & 1 != 0 {
            encrypted = true;
        }
        let data = o + 30 + nl + el;
        if names.is_empty() && method == 0 {
            first_stored = Some((name.clone(), sln(h, data, csz.min(128)).to_vec()));
        }
        if name == "sce_sys/param.sfo" {
            vpk_sfo = Some((method, data, csz));
        }
        names.push(name);
        if flags & 8 != 0 || csz == 0xFFFF_FFFF {
            break;
        }
        o = data.saturating_add(csz);
    }
    let mut from_cd = false;
    // Central directory via the end record in the tail.
    let tail_len = (65536 + 22).min(size);
    let mut need = None;
    match v.tail(us(tail_len)).or_else(|| v.tail(us(22.min(size)))) {
        Some(t) => {
            if let Some(e) = rfind(t, b"PK\x05\x06") {
                let ecd_abs = size - t.len() as u64 + e as u64;
                let mut cd_sz = u64::from(u32le(t, e + 12).unwrap_or(0));
                let mut cd_off = u64::from(u32le(t, e + 16).unwrap_or(0));
                if cd_off == 0xFFFF_FFFF && e >= 20 && at(t, e - 20, b"PK\x06\x07") {
                    let z64 = u64le(t, e - 20 + 8).unwrap_or(0);
                    match v.at(z64, 56) {
                        Some(z) => {
                            cd_sz = u64le(z, 40).unwrap_or(0);
                            cd_off = u64le(z, 48).unwrap_or(0);
                        }
                        None if z64 < size => need = Some((z64, 56)),
                        None => {}
                    }
                }
                if need.is_none() && cd_off < ecd_abs {
                    let want = cd_sz.min(4 << 20);
                    match v.at(cd_off, us(want)) {
                        Some(cd) => {
                            names.clear();
                            let mut i = 0usize;
                            while at(cd, i, b"PK\x01\x02") && names.len() < 100_000 {
                                if u16le(cd, i + 8).unwrap_or(0) & 1 != 0 {
                                    encrypted = true;
                                }
                                let nl = usize::from(u16le(cd, i + 28).unwrap_or(0));
                                let el = usize::from(u16le(cd, i + 30).unwrap_or(0));
                                let cl = usize::from(u16le(cd, i + 32).unwrap_or(0));
                                names
                                    .push(String::from_utf8_lossy(sln(cd, i + 46, nl)).to_string());
                                i += 46 + nl + el + cl;
                            }
                            from_cd = true;
                        }
                        None => need = Some((cd_off, want)),
                    }
                }
            } else if !spanned {
                // No end record: first part of a split archive, or a truncated file.
                return Some(
                    Hit::new("archive.zip", 85)
                        .d("note", "no end-of-central-directory: split/truncated")
                        .d(
                            "first_names",
                            names.iter().take(8).cloned().collect::<Vec<_>>(),
                        ),
                );
            }
        }
        None => need = Some((size - tail_len, tail_len)),
    }
    let (label, c) = classify_zip(&names, first_stored.as_ref());
    let mut hit =
        Hit::new(label, if from_cd { c } else { c.saturating_sub(10) }).d("entries", names.len());
    if label == "game.vita.vpk" {
        hit = vpk(hit, h, vpk_sfo);
    }
    if encrypted {
        hit.push("encrypted", true);
    }
    if spanned {
        hit.push("note", "spanned zip marker");
    }
    let risky: Vec<String> = names
        .iter()
        .filter(|n| RISKY_INSIDE.contains(&ext(n).as_str()))
        .take(4)
        .cloned()
        .collect();
    if !risky.is_empty() {
        hit.push("executables_inside", risky);
    }
    hit.push(
        "first_names",
        names.iter().take(6).cloned().collect::<Vec<_>>(),
    );
    if let Some((o, l)) = need {
        hit = hit.need(o, l);
    }
    Some(hit)
}

/// A VPK's PARAM.SFO (stored or deflated, read when its local entry is in the head): title,
/// TITLE_ID, APP_VER, CATEGORY.
fn vpk(hit: Hit, h: &[u8], sfo: Option<(u16, usize, usize)>) -> Hit {
    use crate::formats::console::{GameInfo, Kind, Platform, sony, with_game};
    let mut g = GameInfo::new(Platform::PsVita, Kind::Package, "VPK");
    let mut hit = hit;
    let raw = sfo.and_then(|(method, data, csz)| {
        let b = h.get(data..data.checked_add(csz)?)?;
        match method {
            0 => Some(b.to_vec()),
            8 => {
                let mut out = Vec::new();
                flate2::read::DeflateDecoder::new(b)
                    .take(64 * 1024)
                    .read_to_end(&mut out)
                    .ok()?;
                Some(out)
            }
            _ => None,
        }
    });
    if let Some(kv) = raw.as_deref().and_then(sony::parse_sfo) {
        sony::sfo_fill(&mut g, &kv, &mut hit);
    }
    with_game(hit, g)
}

fn classify_zip(names: &[String], first_stored: Option<&(String, Vec<u8>)>) -> (&'static str, u8) {
    if let Some((n, d)) = first_stored
        && n == "mimetype"
    {
        if d.starts_with(b"application/epub+zip") {
            return ("doc.epub", 99);
        }
        if d.starts_with(b"application/vnd.oasis.opendocument") {
            return ("doc.odf", 99);
        }
        if d.starts_with(b"application/vnd.comicbook+zip") {
            return ("doc.cbz", 99);
        }
    }
    let has = |s: &str| names.iter().any(|n| n == s);
    let any = |f: &dyn Fn(&str) -> bool| names.iter().any(|n| f(n));
    if has("AndroidManifest.xml") && (has("classes.dex") || has("resources.arsc")) {
        return ("package.apk", 98);
    }
    if has("manifest.json") && any(&|n| n.ends_with(".apk")) {
        return ("package.xapk", 96);
    }
    if has("info.json") && any(&|n| n.ends_with(".apk")) {
        return ("package.xapk", 90);
    }
    if has("BundleConfig.pb") {
        return ("package.aab", 97);
    }
    // PS Vita homebrew package (VitaShell installs it): the app's sce_sys and its eboot.
    if has("sce_sys/param.sfo") && has("eboot.bin") {
        return ("game.vita.vpk", 96);
    }
    if any(&|n| n.starts_with("Payload/") && n.contains(".app/")) {
        return ("package.ipa", 96);
    }
    if has("[Content_Types].xml") {
        if any(&|n| n.starts_with("word/")) {
            return ("doc.docx", 98);
        }
        if any(&|n| n.starts_with("xl/")) {
            return ("doc.xlsx", 98);
        }
        if any(&|n| n.starts_with("ppt/")) {
            return ("doc.pptx", 98);
        }
        if any(&|n| n.ends_with(".nuspec")) {
            return ("package.nupkg", 95);
        }
        if has("AppxManifest.xml") || has("AppxBlockMap.xml") {
            return ("package.appx", 96);
        }
    }
    if any(&|n| n.contains(".dist-info/")) {
        return ("package.wheel", 96);
    }
    if has("META-INF/MANIFEST.MF") && any(&|n| n.ends_with(".class")) {
        return ("package.jar", 95);
    }
    if has("META-INF/MANIFEST.MF") && !any(&|n| n.ends_with(".py")) {
        return ("package.jar", 85);
    }
    if has("META-INF/container.xml") {
        return ("doc.epub", 85);
    }
    if has("mimetype") && any(&|n| n.ends_with("content.xml")) {
        return ("doc.odf", 90);
    }
    let files: Vec<&String> = names.iter().filter(|n| !n.ends_with('/')).collect();
    let imgs = files
        .iter()
        .filter(|n| {
            matches!(
                ext(n).as_str(),
                "jpg" | "jpeg" | "png" | "webp" | "gif" | "jxl" | "avif" | "bmp"
            )
        })
        .count();
    if files.len() >= 2 && imgs * 10 >= files.len() * 8 {
        return ("doc.cbz", 90);
    }
    ("archive.zip", 97)
}

// ---------------------------------------------------------------- RAR

fn rar_vint(b: &[u8], o: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for i in 0..10 {
        let c = u8at(b, *o)?;
        *o += 1;
        v |= u64::from(c & 0x7f) << (7 * i);
        if c & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

fn rar(v: &View) -> Option<Hit> {
    let h = v.head();
    // SFX stubs put the signature later; the PE overlay check handles that case.
    if at(h, 0, b"Rar!\x1a\x07\x01\x00") {
        return Some(rar5(h));
    }
    if at(h, 0, b"Rar!\x1a\x07\x00") {
        return Some(rar4(h));
    }
    if at(h, 0, b"RE~^") {
        // "RE~^" alone is a 4-byte hint (no header check here).
        return Some(Hit::new("archive.rar4", crate::types::WEAK).d("variant", "rar 1.4"));
    }
    None
}

fn rar5(h: &[u8]) -> Hit {
    let mut o = 8usize;
    let mut hit = Hit::new("archive.rar5", 90);
    let mut names = Vec::new();
    let mut crc_ok = 0;
    for _ in 0..64 {
        let Some(crc) = u32le(h, o) else { break };
        o += 4;
        let hs_pos = o;
        let Some(hsz) = rar_vint(h, &mut o) else {
            break;
        };
        let body_start = o;
        let body_end = body_start.saturating_add(us(hsz));
        let Some(full) = h.get(hs_pos..body_end) else {
            break;
        };
        if crc32fast::hash(full) != crc {
            break;
        }
        crc_ok += 1;
        let parsed = (|| -> Option<(u64, bool)> {
            let ty = rar_vint(h, &mut o)?;
            let hflags = rar_vint(h, &mut o)?;
            if hflags & 1 != 0 {
                rar_vint(h, &mut o)?;
            }
            let data = if hflags & 2 != 0 {
                rar_vint(h, &mut o)?
            } else {
                0
            };
            match ty {
                1 => {
                    let af = rar_vint(h, &mut o)?;
                    if af & 1 != 0 {
                        hit.push("volume", true);
                    }
                    if af & 2 != 0 {
                        let vn = rar_vint(h, &mut o)?;
                        hit.push("volume_number", vn.saturating_add(1));
                    }
                    if af & 4 != 0 {
                        hit.push("solid", true);
                    }
                }
                2 => {
                    let ff = rar_vint(h, &mut o)?;
                    rar_vint(h, &mut o)?; // unpacked size
                    rar_vint(h, &mut o)?; // attributes
                    if ff & 2 != 0 {
                        o += 4;
                    }
                    if ff & 4 != 0 {
                        o += 4;
                    }
                    rar_vint(h, &mut o)?; // compression
                    rar_vint(h, &mut o)?; // host os
                    let nl = us(rar_vint(h, &mut o)?);
                    if let Some(n) = o.checked_add(nl).and_then(|e| h.get(o..e)) {
                        names.push(String::from_utf8_lossy(n).to_string());
                    }
                    // Encryption record in the extra area (type 1).
                    if hflags & 1 != 0 {
                        let ex = sl(h, o.saturating_add(nl), body_end);
                        let mut q = 0;
                        if let (Some(_), Some(1)) = (rar_vint(ex, &mut q), rar_vint(ex, &mut q)) {
                            hit.push("encrypted", true);
                        }
                    }
                }
                4 => {
                    hit.push("encrypted_headers", true);
                    return Some((data, true));
                }
                5 => return Some((data, true)),
                _ => {}
            }
            Some((data, false))
        })();
        let Some((data, stop)) = parsed else { break };
        if stop {
            break;
        }
        o = body_end.saturating_add(us(data));
        if o >= h.len() {
            break;
        }
    }
    if crc_ok > 0 {
        hit.conf = 98;
    }
    rar_names(hit, names)
}

fn rar4(h: &[u8]) -> Hit {
    let mut hit = Hit::new("archive.rar4", 88);
    let mut o = 7usize;
    let mut names = Vec::new();
    for _ in 0..64 {
        let (Some(crc), Some(ty), Some(fl), Some(hs)) = (
            u16le(h, o),
            u8at(h, o + 2),
            u16le(h, o + 3),
            u16le(h, o + 5),
        ) else {
            break;
        };
        let hs = usize::from(hs);
        if hs < 7 {
            break;
        }
        let Some(body) = h.get(o + 2..o + hs) else {
            break;
        };
        if (crc32fast::hash(body) & 0xffff) as u16 != crc {
            break;
        }
        hit.conf = 98;
        let mut add = 0usize;
        match ty {
            0x73 => {
                if fl & 1 != 0 {
                    hit.push("volume", true);
                }
                if fl & 0x100 != 0 {
                    hit.push("first_volume", true);
                }
                if fl & 8 != 0 {
                    hit.push("solid", true);
                }
                if fl & 0x80 != 0 {
                    hit.push("encrypted_headers", true);
                    break;
                }
            }
            0x74 => {
                add = us(u64::from(u32le(h, o + 7).unwrap_or(0)));
                let nl = usize::from(u16le(h, o + 26).unwrap_or(0));
                if let Some(n) = h.get(o + 32..o + 32 + nl) {
                    names.push(String::from_utf8_lossy(n).to_string());
                }
                if fl & 4 != 0 {
                    hit.push("encrypted", true);
                }
                if fl & 1 != 0 {
                    hit.push("continued_from_previous_volume", true);
                }
            }
            0x7b => break,
            _ => {
                if fl & 0x8000 != 0 {
                    add = us(u64::from(u32le(h, o + 7).unwrap_or(0)));
                }
            }
        }
        o = o.saturating_add(hs).saturating_add(add);
    }
    rar_names(hit, names)
}

fn rar_names(mut hit: Hit, names: Vec<String>) -> Hit {
    let imgs = names
        .iter()
        .filter(|n| matches!(ext(n).as_str(), "jpg" | "jpeg" | "png" | "webp" | "gif"))
        .count();
    if !names.is_empty() && imgs == names.len() {
        hit.relabel("doc.cbr");
    }
    if names
        .iter()
        .any(|n| matches!(ext(n).as_str(), "exe" | "scr" | "bat" | "lnk" | "msi"))
    {
        hit.push("executables_inside", true);
    }
    if !names.is_empty() {
        hit.push("first_names", names.into_iter().take(6).collect::<Vec<_>>());
    }
    hit
}

// ---------------------------------------------------------------- 7z

fn sevenz(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, &[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        return None;
    }
    let crc = u32le(h, 8)?;
    let ok = h.get(12..32).is_some_and(|b| crc32fast::hash(b) == crc);
    let nh_off = u64le(h, 12)?;
    let nh_sz = u64le(h, 20)?;
    let mut hit = Hit::new("archive.7z", if ok { 99 } else { 80 })
        .d("version", format!("{}.{}", u8at(h, 6)?, u8at(h, 7)?));
    if ok {
        let abs = nh_off.saturating_add(32);
        if abs.saturating_add(nh_sz) > v.size() && v.size() > 0 {
            hit.push(
                "note",
                "next header beyond EOF: first part of a split (.7z.001) archive",
            );
        } else if let Some(b) = v.at(abs, 1) {
            let kind = match b.first() {
                Some(0x01) => "plain",
                Some(0x17) => "encoded (LZMA): names need a decoder",
                _ => "unknown",
            };
            hit.push("header", kind);
        } else if nh_sz < 1 << 20 {
            hit = hit.need(abs, nh_sz.max(1));
        }
    }
    Some(hit)
}

// ---------------------------------------------------------------- tar

fn tar_checksum_ok(b: &[u8]) -> bool {
    let Some(blk) = b.get(..512) else {
        return false;
    };
    let stored = std::str::from_utf8(sl(blk, 148, 156))
        .ok()
        .map(|s| s.trim_matches(|c: char| c == '\0' || c == ' '))
        .and_then(|s| u32::from_str_radix(s, 8).ok());
    let Some(stored) = stored else { return false };
    let sum: u32 = blk
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            if (148..156).contains(&i) {
                32
            } else {
                u32::from(c)
            }
        })
        .sum();
    sum == stored
}

fn tar(v: &View) -> Option<Hit> {
    let h = v.head();
    let ustar = at(h, 257, b"ustar");
    let ck = tar_checksum_ok(h);
    if !(ustar || ck) {
        return None;
    }
    let name = cstr(pre(h, 100));
    let conf = if ustar && ck {
        99
    } else if ck {
        75
    } else {
        70
    };
    let mut hit = Hit::new("archive.tar", conf).d("first_entry", name.clone());
    if matches!(
        name.as_str(),
        ".BUILDINFO" | ".PKGINFO" | ".MTREE" | ".INSTALL"
    ) {
        hit.relabel("package.arch");
    }
    Some(hit)
}

// ---------------------------------------------------------------- compressed streams

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codec {
    Gzip,
    Zlib,
    Zstd,
}

/// The zstd frame content size when the frame is single-segment (ruzstd sizes its buffer from
/// it): `None` when not single-segment or unreadable.
fn zstd_single_segment_size(b: &[u8]) -> Option<u64> {
    let fhd = u8at(b, 4)?;
    if fhd & 0x20 == 0 {
        return None;
    }
    let did = [0usize, 1, 2, 4]
        .get(usize::from(fhd & 3))
        .copied()
        .unwrap_or(0);
    let pos = 5 + did;
    Some(match fhd >> 6 {
        0 => u64::from(u8at(b, pos)?),
        1 => u64::from(u16le(b, pos)?) + 256,
        2 => u64::from(u32le(b, pos)?),
        _ => u64le(b, pos)?,
    })
}

/// Decompresses up to `limit` bytes from a partial compressed head (errors end the output).
pub(crate) fn inflate_partial(codec: Codec, b: &[u8], limit: usize) -> Vec<u8> {
    let mut out = vec![0u8; limit];
    let mut n = 0usize;
    fn pump<R: Read>(mut r: R, out: &mut [u8], n: &mut usize) {
        while let Some(dst) = out.get_mut(*n..) {
            if dst.is_empty() {
                break;
            }
            match r.read(dst) {
                Ok(0) | Err(_) => break,
                Ok(k) => *n += k,
            }
        }
    }
    match codec {
        Codec::Gzip => pump(flate2::read::MultiGzDecoder::new(b), &mut out, &mut n),
        Codec::Zlib => pump(flate2::read::ZlibDecoder::new(b), &mut out, &mut n),
        Codec::Zstd => {
            // ruzstd only releases output beyond the frame's window (32 MiB for Arch packages),
            // so a partial frame yields nothing. Declare a 256 KiB window in a copy of the
            // header: back-references further than that fail, but the first few hundred KiB come
            // out, which is all a sniff needs. Single-segment frames size the buffer from the
            // content size: refuse large ones instead of allocating.
            if zstd_single_segment_size(b).is_some_and(|s| s > 8 << 20) {
                return Vec::new();
            }
            let mut copy = b.to_vec();
            let fhd = copy.get(4).copied().unwrap_or(0);
            if let Some(wd) = copy.get_mut(5)
                && fhd & 0x20 == 0
                && *wd > 0x40
            {
                *wd = 0x40;
            }
            if let Ok(d) = ruzstd::decoding::StreamingDecoder::new(copy.as_slice()) {
                pump(d, &mut out, &mut n);
            }
        }
    }
    out.truncate(n);
    out
}

/// Labels the decompressed head of a gzip/zstd stream with the full detector (nested peeks
/// are bounded by `Caps::max_depth`).
///
/// The peek is the structural check: a stream that decodes keeps the hit's confidence, one
/// that cannot be tried (nesting limit) drops to 90, and one that decodes to nothing is marked
/// `inflate = failed` for the caller to lower (gzip) or to weigh against a short head (zstd).
fn inner(mut hit: Hit, codec: Codec, v: &View, b: &[u8]) -> Hit {
    if v.depth >= v.caps.max_depth {
        hit.conf = hit.conf.min(90);
        return hit;
    }
    let inner = inflate_partial(codec, b, v.caps.inner_bytes);
    if inner.is_empty() {
        hit.push("inflate", "failed");
        return hit;
    }
    let n = inner.len();
    let (label, first_entry) = crate::engine::inner_label(inner, v.caps, v.depth + 1, v.signatures);
    if label == "package.arch" {
        hit.relabel("package.arch");
    }
    hit.push("inner", label);
    hit.push("inner_bytes", n);
    if let Some(e) = first_entry {
        hit.push("inner_first_entry", e);
    }
    hit
}

fn compressors(v: &View) -> Option<Hit> {
    let h = v.head();
    if at(h, 0, &[0x1F, 0x8B, 0x08]) && u8at(h, 3).is_some_and(|f| f & 0xE0 == 0) {
        let flg = u8at(h, 3)?;
        let mut hit = Hit::new("archive.gzip", 95);
        if flg & 8 != 0 {
            let mut o = 10usize;
            if flg & 4 != 0 {
                o += 2 + usize::from(u16le(h, 10).unwrap_or(0));
            }
            if let Some(n) = h.get(o..) {
                hit.push("orig_name", cstr(pre(n, 256)));
            }
        }
        let mut hit = inner(hit, Codec::Gzip, v, h);
        // Deflate emits output block by block: a head of a few hundred bytes that yields
        // nothing is a corrupt stream behind a gzip header, not a gzip file to store as one.
        let out = hit
            .fields
            .iter()
            .find(|f| f.key == "inner_bytes")
            .and_then(|f| f.value.as_u64());
        let whole = v.size() == h.len() as u64;
        if hit.has("inflate") && (h.len() >= 512 || whole) {
            hit.conf = 60;
        } else if let Some(n) = out {
            if whole && n < v.caps.inner_bytes as u64 {
                // All of it decoded: the trailer's ISIZE must agree.
                let isize = u32le(h, h.len().saturating_sub(4)).map(u64::from);
                if isize != Some(n & 0xFFFF_FFFF) {
                    hit.conf = hit.conf.min(85);
                }
            } else if n < 256 && h.len() >= 4096 {
                // A few bytes out of kilobytes in: random data that happened to decode.
                hit.conf = 70;
            }
        }
        return Some(hit);
    }
    if at(h, 0, &[0x28, 0xB5, 0x2F, 0xFD]) {
        let mut hit = inner(Hit::new("archive.zstd", 97), Codec::Zstd, v, h);
        // zstd blocks are up to 128 KiB: a shorter head may decode to nothing yet.
        if hit.has("inflate") {
            if h.len() < 300 * 1024 && v.size() > h.len() as u64 {
                hit.conf = 90;
                return Some(hit.need(0, 300 * 1024));
            }
            hit.conf = 60;
        }
        return Some(hit);
    }
    if u32le(h, 0).is_some_and(|m| (0x184D2A50..=0x184D2A5F).contains(&m)) {
        return Some(Hit::new("archive.zstd", 70).d("note", "skippable frame first"));
    }
    if at(h, 0, &[0xFD, b'7', b'z', b'X', b'Z', 0]) {
        let ok = h
            .get(6..8)
            .zip(u32le(h, 8))
            .is_some_and(|(f, c)| crc32fast::hash(f) == c);
        return Some(Hit::new("archive.xz", if ok { 99 } else { 85 }).d(
            "note",
            "inner sniff needs an LZMA2 decoder (not in the offline registry)",
        ));
    }
    // LZMA-alone: props byte 0x5D, power-of-two dictionary, size -1 or plausible.
    if h.len() >= 13 && u8at(h, 0) == Some(0x5D) {
        let dict = u32le(h, 1)?;
        let us_ = u64le(h, 5)?;
        if dict.is_power_of_two() && dict >= 1 << 12 && (us_ == u64::MAX || us_ < 1 << 40) {
            return Some(Hit::new("archive.lzma", 65));
        }
    }
    None
}

// ---------------------------------------------------------------- xar / macOS pkg

fn xar(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"xar!") {
        return None;
    }
    let hs = usize::from(u16be(h, 4)?);
    let ver = u16be(h, 6)?;
    let tc = us(u64be(h, 8)?);
    let mut hit = Hit::new("archive.xar", if hs >= 28 && ver == 1 { 92 } else { 70 });
    if tc == 0 {
        return Some(hit);
    }
    let want = hs.saturating_add(tc.min(1 << 20));
    let Some(toc) = h.get(hs..want) else {
        return Some(hit.need(0, want as u64));
    };
    let xml = inflate_partial(Codec::Zlib, toc, 1 << 20);
    if xml.is_empty() {
        return Some(hit.d("toc", "not inflatable"));
    }
    hit.conf = 98;
    let has = |s: &[u8]| contains(&xml, s);
    if has(b"<name>Distribution</name>")
        || has(b"<name>PackageInfo</name>")
        || (has(b"<name>Bom</name>") && has(b"<name>Payload</name>"))
    {
        hit.relabel("package.macos_pkg");
        let flavour = if has(b"<name>Distribution</name>") {
            "product archive (Distribution)"
        } else {
            "component package"
        };
        hit.push("flavour", flavour);
    }
    Some(hit)
}

// ---------------------------------------------------------------- OLE compound file

const MSI_CLSID: [u8; 16] = [
    0x84, 0x10, 0x0C, 0x00, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46,
];
const MSP_CLSID: [u8; 16] = [
    0x86, 0x10, 0x0C, 0x00, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46,
];

/// OLE compound file (MSI, MSP, legacy Office, Thumbs.db): root entry CLSID and stream names in
/// the first directory sector.
fn cfb(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return None;
    }
    let shift = u32::from(u16le(h, 0x1E)?);
    if shift != 9 && shift != 12 {
        return Some(Hit::new("misc.cfb", 70));
    }
    let first_dir = u64::from(u32le(h, 0x30)?);
    let off = first_dir
        .saturating_add(1)
        .checked_shl(shift)
        .unwrap_or(u64::MAX);
    let ssz = 1usize << shift;
    let Some(d) = v.at(off, ssz) else {
        let hit = Hit::new("misc.cfb", 85);
        return Some(if off < v.size() {
            hit.need(off, ssz as u64)
        } else {
            hit
        });
    };
    let clsid = sl(d, 0x50, 0x60);
    let mut names = Vec::new();
    for e in d.as_chunks::<128>().0 {
        let nl = usize::from(u16le(e, 64).unwrap_or(0)).min(64);
        if nl < 2 {
            continue;
        }
        let u: Vec<u16> = pre(e, nl - 2)
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| {
                u16::from_le_bytes([
                    c.first().copied().unwrap_or(0),
                    c.get(1).copied().unwrap_or(0),
                ])
            })
            .collect();
        names.push(String::from_utf16_lossy(&u));
    }
    let has = |s: &str| names.iter().any(|n| n == s);
    let (label, c) = if clsid == MSI_CLSID {
        ("package.msi", 99)
    } else if clsid == MSP_CLSID {
        ("package.msp", 99)
    } else if has("WordDocument") {
        ("doc.doc", 97)
    } else if has("Workbook") || has("Book") {
        ("doc.xls", 97)
    } else if has("PowerPoint Document") {
        ("doc.ppt", 97)
    } else if has("Catalog") {
        ("misc.thumbs_db", 90)
    } else {
        ("misc.cfb", 85)
    };
    Some(Hit::new(label, c).d("streams", names.into_iter().take(6).collect::<Vec<_>>()))
}
