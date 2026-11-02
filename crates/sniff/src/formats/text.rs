//! Text sniffing, run only when no binary check is confident: encoding first (BOM, UTF-16,
//! UTF-8, CP437 art), then content rules (subtitles, playlists, scripts, markup, checksums, NFO).

use crate::types::Hit;
use crate::util::*;
use crate::view::View;

fn decode(h: &[u8]) -> Option<(String, &'static str)> {
    let h = pre(h, 64 * 1024);
    if h.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Some((String::from_utf8_lossy(from(h, 3)).to_string(), "utf-8-bom"));
    }
    let utf16 = |b: &[u8], le: bool| -> String {
        let u: Vec<u16> = b
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| {
                let pair = [
                    c.first().copied().unwrap_or(0),
                    c.get(1).copied().unwrap_or(0),
                ];
                if le {
                    u16::from_le_bytes(pair)
                } else {
                    u16::from_be_bytes(pair)
                }
            })
            .collect();
        String::from_utf16_lossy(&u)
    };
    // A UTF-16 BOM is two bytes: binary data starts with FF FE often enough (a misplaced
    // window, a deflate stream). Accept the decode only when it reads as text.
    if h.starts_with(&[0xFF, 0xFE]) || h.starts_with(&[0xFE, 0xFF]) {
        let le = h.first() == Some(&0xFF);
        let t = utf16(from(h, 2), le);
        return texty(&t).then_some((t, if le { "utf-16le" } else { "utf-16be" }));
    }
    // BOM-less UTF-16LE: every odd byte zero in ASCII text.
    if h.len() >= 16
        && h.iter().skip(1).step_by(2).take(64).all(|&b| b == 0)
        && h.iter().step_by(2).take(64).all(|&b| b != 0)
    {
        let t = utf16(h, true);
        return texty(&t).then_some((t, "utf-16le"));
    }
    if memchr::memchr(0, pre(h, 8192)).is_some() {
        return None; // NUL never appears in 8-bit/UTF-8 text
    }
    let ctrl = h
        .iter()
        .filter(|&&b| b < 0x09 || ((0x0E..0x20).contains(&b) && b != 0x1B))
        .count();
    if ctrl * 100 > h.len().max(1) {
        return None; // binary
    }
    match std::str::from_utf8(h) {
        Ok(s) => Some((s.to_string(), if h.is_ascii() { "ascii" } else { "utf-8" })),
        // A multi-byte sequence cut at the end of the head is fine.
        Err(e) if e.valid_up_to() + 4 >= h.len() => Some((
            String::from_utf8_lossy(pre(h, e.valid_up_to())).to_string(),
            "utf-8",
        )),
        Err(_) => {
            // 8-bit legacy: CP437 block/box art (NFO) or latin-1/cp1252.
            let art = h.iter().filter(|&&b| (0xB0..=0xDF).contains(&b)).count();
            let hi = h.iter().filter(|&&b| b >= 0x80).count();
            let printable_ascii = h
                .iter()
                .filter(|&&b| (0x20..0x7f).contains(&b) || b == b'\n' || b == b'\r' || b == b'\t')
                .count();
            let lines = h.iter().filter(|&&b| b == b'\n').count();
            let cp437 = art * 2 > hi
                && art >= 8
                && printable_ascii * 100 >= h.len() * 60
                && lines * 200 >= h.len();
            if !cp437 && hi * 100 > h.len() * 30 {
                return None; // mostly high bytes: binary
            }
            let s = h
                .iter()
                .map(|&b| if b < 0x80 { char::from(b) } else { '\u{FFFD}' })
                .collect();
            Some((
                s,
                if cp437 {
                    "cp437"
                } else {
                    "8bit (latin-1/cp1252?)"
                },
            ))
        }
    }
}

/// Decoded UTF-16 that reads as text: at most 2% replacement, private-use or control
/// characters (random 16-bit units give about 15%).
fn texty(t: &str) -> bool {
    let (mut n, mut bad) = (0usize, 0usize);
    for c in t.chars() {
        n += 1;
        if c == '\u{FFFD}'
            || ('\u{E000}'..='\u{F8FF}').contains(&c)
            || (c.is_control() && !matches!(c, '\t' | '\n' | '\r' | '\u{c}'))
        {
            bad += 1;
        }
    }
    n > 0 && bad * 100 <= n * 2
}

fn is_srt(t: &str) -> bool {
    let mut lines = t.lines().map(|l| l.trim_start_matches('\u{feff}').trim());
    for _ in 0..6 {
        let Some(l) = lines.next() else { return false };
        if l.contains("-->") {
            let b = l.as_bytes();
            return b.len() >= 29
                && b.get(2) == Some(&b':')
                && b.get(5) == Some(&b':')
                && matches!(b.get(8), Some(b',' | b'.'));
        }
    }
    false
}

fn second_char_is_digit(s: &str) -> bool {
    s.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
}

pub(crate) fn check(v: &View) -> Option<Hit> {
    let h = v.head();
    if h.is_empty() || h.iter().all(|&b| b == 0) {
        return None;
    }
    let (t, enc) = decode(h)?;
    let s = t.trim_start_matches('\u{feff}').trim_start();
    let low: String = s
        .chars()
        .take(4096)
        .collect::<String>()
        .to_ascii_lowercase();
    let first = s.lines().next().unwrap_or("").trim();
    let mk = |l: &'static str, c: u8| Some(Hit::new(l, c).d("encoding", enc));
    if s.starts_with("WEBVTT") {
        return mk("subtitle.webvtt", 97);
    }
    if is_srt(s) {
        return mk("subtitle.srt", 95);
    }
    if low.starts_with("[script info]") || low.contains("\n[script info]") {
        return if low.contains("[v4+ styles]") || low.contains("scripttype: v4.00+") {
            mk("subtitle.ass", 97)
        } else {
            mk("subtitle.ssa", 95)
        };
    }
    if first.starts_with('{') && first.contains("}{") && second_char_is_digit(first) {
        return mk("subtitle.microdvd", 92);
    }
    if first.starts_with('[') && first.contains("][") && second_char_is_digit(first) {
        return mk("subtitle.mpl2", 88);
    }
    if low.contains("<sami>") {
        return mk("subtitle.sami", 96);
    }
    if s.starts_with("# VobSub index file") {
        return mk("subtitle.vobsub_idx", 99);
    }
    if (low.contains("<tt ") || low.contains("<tt>")) && low.contains("ttml") {
        return mk("subtitle.ttml", 95);
    }
    if low.contains("<fictionbook") {
        return mk("doc.fb2", 96);
    }
    if low.starts_with("[internetshortcut]") {
        return mk("text.url", 98);
    }
    let ini_like = low.starts_with("[.shellclassinfo]")
        || (first.starts_with('[')
            && first.ends_with(']')
            && s.lines().nth(1).is_some_and(|l| l.contains('=')));
    if ini_like && !low.starts_with("[clonecd]") {
        return mk("text.ini", 80);
    }
    if low.starts_with("[clonecd]") {
        return mk("text.ccd", 97);
    }
    if s.starts_with("#EXTM3U") || low.starts_with("[playlist]") {
        return mk("text.playlist", 96);
    }
    if (low.starts_with("file ") || low.contains("\nfile "))
        && low.contains("track ")
        && (low.contains("binary") || low.contains("wave") || low.contains("mp3"))
    {
        return mk("text.cue", 95);
    }
    if s.starts_with("#!") {
        if first.contains("python") {
            return mk("text.python", 95);
        }
        return mk("text.shell", 95);
    }
    if low.starts_with("@echo off")
        || low.starts_with("@echo on")
        || (low.contains("\r\nstart ") && low.contains(".exe"))
    {
        return mk("text.batch", 92);
    }
    if low.contains("createobject(") || low.contains("wscript.") {
        return mk("text.vbscript", 88);
    }
    if low.contains("invoke-webrequest")
        || low.contains("$erroractionpreference")
        || low.contains("-executionpolicy")
        || low.contains("iex(")
    {
        return mk("text.powershell", 85);
    }
    if low.starts_with("<!doctype html")
        || low.starts_with("<html")
        || (low.contains("<html") && low.contains("<body"))
    {
        return mk("text.html", 95);
    }
    if low.starts_with("<?xml") || low.starts_with("<svg") {
        if low.contains("<svg") {
            return mk("image.svg", 95);
        }
        return mk("text.xml", 90);
    }
    if (s.starts_with('{') || s.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(s.trim_end()).is_ok()
    {
        return mk("text.json", 95);
    }
    if s.starts_with('{') && s.contains("\":") {
        return mk("text.json", 70);
    }
    if s.starts_with("WARC/1.") {
        return mk("archive.warc", 97);
    }
    if s.starts_with("# This is a shell archive") || low.contains("\n# this is a shell archive") {
        return mk("text.shell", 85);
    }
    // Checksum lists.
    let mut lines = s.lines().filter(|l| !l.trim().is_empty());
    let l0 = lines.next().unwrap_or("");
    let sfv_line = |l: &str| {
        l.rsplit(' ')
            .next()
            .is_some_and(|c| c.len() == 8 && c.chars().all(|x| x.is_ascii_hexdigit()))
    };
    if l0.starts_with(';')
        && s.lines()
            .filter(|l| !l.starts_with(';') && !l.trim().is_empty())
            .all(sfv_line)
    {
        return mk("text.sfv", 92);
    }
    let tok = l0.split([' ', '\t']).next().unwrap_or("");
    if [32, 40, 64, 128].contains(&tok.len()) && tok.chars().all(|c| c.is_ascii_hexdigit()) {
        return mk("text.checksum", 92);
    }
    // NFO: CP437 art or Unicode block/box drawing art.
    let boxy = t
        .chars()
        .filter(|&c| ('\u{2500}'..='\u{259F}').contains(&c))
        .count();
    if enc == "cp437" || boxy >= 8 {
        return mk("text.nfo", 85);
    }
    mk("text.plain", 60)
}
