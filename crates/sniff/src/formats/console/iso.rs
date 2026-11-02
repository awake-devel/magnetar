//! A small ISO 9660 walker over three sector sources (cooked 2048-byte images, raw 2352-byte
//! CD images, PSP CSO blocks) for the PlayStation discs: PSP and PS3 `PARAM.SFO`, PS1 / PS2
//! `SYSTEM.CNF`. UNVERIFIED: no real disc image on this machine; the layouts are ECMA-119 and
//! the PS2 / PSP developer wikis.
//!
//! `disc.rs` labels these discs from their root directory; the checks here run the extra
//! steps (subdirectory, file) at a higher confidence so their typed ids win, and ask for
//! speculative spans (the directories right after the root) so that the PARAM.SFO is reached
//! within the default three rounds.

use std::io::Read;

use crate::formats::console::{Content, GameInfo, Kind, Platform, sony, with_game};
use crate::sparse::Range;
use crate::types::Hit;
use crate::util::*;
use crate::view::View;

const SECTOR: u64 = 2048;
/// Directories are read in full up to this many sectors.
const MAX_DIR_SECTORS: u64 = 32;
/// Sectors asked for after the root directory when it is missing (subdirectories follow it).
const PREFETCH_SECTORS: u64 = 32;

/// Logical 2048-byte sectors.
trait Sectors {
    /// `n` sectors from `lba` when all are known.
    fn read(&self, lba: u64, n: u64) -> Option<Vec<u8>>;
    /// The physical ranges to fetch so that `read(lba, n)` can succeed (empty: past the end).
    fn need(&self, lba: u64, n: u64) -> Vec<Range>;
}

/// Cooked (`size` 2048) or raw (`size` 2352 with the user data at `data_off`) sectors.
struct Plain<'v, 'a> {
    v: &'v View<'a>,
    size: u64,
    data_off: u64,
}

impl Sectors for Plain<'_, '_> {
    fn read(&self, lba: u64, n: u64) -> Option<Vec<u8>> {
        if self.size == SECTOR {
            return self
                .v
                .at(lba.checked_mul(SECTOR)?, us(n.checked_mul(SECTOR)?))
                .map(<[u8]>::to_vec);
        }
        let mut out = Vec::with_capacity(us(n.saturating_mul(SECTOR)).min(1 << 20));
        for k in 0..n {
            let at = lba
                .checked_add(k)?
                .checked_mul(self.size)?
                .checked_add(self.data_off)?;
            out.extend_from_slice(self.v.at(at, us(SECTOR))?);
        }
        Some(out)
    }
    fn need(&self, lba: u64, n: u64) -> Vec<Range> {
        let start = lba.saturating_mul(self.size);
        if start >= self.v.size() {
            return Vec::new();
        }
        let len = n.saturating_mul(self.size).min(self.v.size() - start);
        self.v.file.missing(Range::new(start, len))
    }
}

/// PSP CSO v1/v2: le32 block index at 0x18 (bit 31 = stored uncompressed in v1; in v2 a block
/// as long as the block size is stored), raw deflate blocks shifted by `align`.
struct Cso<'v, 'a> {
    v: &'v View<'a>,
    block: u64,
    align: u32,
    version: u8,
    blocks: u64,
}

impl Cso<'_, '_> {
    fn index(&self, i: u64) -> Option<u32> {
        self.v
            .at(0x18u64.checked_add(i.checked_mul(4)?)?, 4)
            .and_then(|b| u32le(b, 0))
    }
    fn span(&self, i: u64) -> Option<(u64, u64, bool)> {
        let a = self.index(i)?;
        let b = self.index(i.checked_add(1)?)?;
        let off = u64::from(a & 0x7FFF_FFFF) << self.align;
        let end = u64::from(b & 0x7FFF_FFFF) << self.align;
        let len = end.checked_sub(off)?;
        let plain = if self.version >= 2 {
            len >= self.block
        } else {
            a & 0x8000_0000 != 0
        };
        Some((off, len.min(self.block.saturating_mul(2)), plain))
    }
    fn blocks_of(&self, lba: u64, n: u64) -> (u64, u64) {
        let per = (self.block / SECTOR).max(1);
        let first = lba / per;
        let last = lba.saturating_add(n.max(1) - 1) / per;
        (first, last.min(self.blocks.saturating_sub(1)))
    }
}

impl Sectors for Cso<'_, '_> {
    fn read(&self, lba: u64, n: u64) -> Option<Vec<u8>> {
        let (first, last) = self.blocks_of(lba, n);
        if first > last || last - first > 64 {
            return None;
        }
        let mut out = Vec::new();
        for i in first..=last {
            let (off, len, plain) = self.span(i)?;
            let raw = self.v.at(off, us(len))?;
            if plain {
                out.extend_from_slice(pre(raw, us(self.block)));
            } else {
                let mut d = Vec::new();
                flate2::read::DeflateDecoder::new(raw)
                    .take(self.block)
                    .read_to_end(&mut d)
                    .ok()?;
                d.resize(us(self.block), 0);
                out.extend(d);
            }
        }
        let per = (self.block / SECTOR).max(1);
        let skip = us((lba - first * per).saturating_mul(SECTOR));
        let want = us(n.saturating_mul(SECTOR));
        out.get(skip..skip.checked_add(want)?).map(<[u8]>::to_vec)
    }
    fn need(&self, lba: u64, n: u64) -> Vec<Range> {
        let (first, last) = self.blocks_of(lba, n);
        if first > last {
            return Vec::new();
        }
        let last = last.min(first + 64);
        let idx = Range::new(0x18 + first * 4, (last - first + 2) * 4);
        let missing_idx = self.v.file.missing(idx);
        if !missing_idx.is_empty() {
            return missing_idx;
        }
        let (Some((a, _, _)), Some((b, bl, _))) = (self.span(first), self.span(last)) else {
            return Vec::new();
        };
        let end = b.saturating_add(bl).min(self.v.size());
        if end <= a {
            return Vec::new();
        }
        self.v.file.missing(Range::new(a, end - a))
    }
}

/// One directory record.
#[derive(Clone, Debug)]
struct Entry {
    name: String,
    lba: u64,
    size: u64,
    dir: bool,
}

enum Got<T> {
    Ok(T),
    Need(Vec<Range>),
    No,
}

fn read_or_need(src: &dyn Sectors, lba: u64, n: u64, prefetch: u64) -> Got<Vec<u8>> {
    match src.read(lba, n) {
        Some(b) => Got::Ok(b),
        None => {
            let r = src.need(lba, n.max(prefetch));
            if r.is_empty() { Got::No } else { Got::Need(r) }
        }
    }
}

struct Pvd {
    system: String,
    volume: String,
    root: Entry,
}

fn pvd(src: &dyn Sectors) -> Option<Pvd> {
    let s = src.read(16, 1)?;
    if !at(&s, 0, b"\x01CD001") {
        return None;
    }
    let root = record(sln(&s, 156, 34))?;
    Some(Pvd {
        system: cstr(sl(&s, 8, 40)),
        volume: cstr(sl(&s, 40, 72)),
        root,
    })
}

fn record(r: &[u8]) -> Option<Entry> {
    let len = usize::from(u8at(r, 0)?);
    if len < 34 {
        return None;
    }
    let nl = usize::from(u8at(r, 32)?);
    let raw = sln(r, 33, nl);
    let name = String::from_utf8_lossy(raw);
    let name = name.split(';').next().unwrap_or("").to_owned();
    Some(Entry {
        name,
        lba: u64::from(u32le(r, 2)?),
        size: u64::from(u32le(r, 10)?),
        dir: u8at(r, 25)? & 2 != 0,
    })
}

fn entries(dir: &[u8]) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut o = 0usize;
    while o < dir.len() && out.len() < 2048 {
        let l = usize::from(u8at(dir, o).unwrap_or(0));
        if l == 0 {
            // Records do not cross sectors: skip to the next one.
            o = (o / 2048 + 1).saturating_mul(2048);
            continue;
        }
        if let Some(e) = record(sln(dir, o, l))
            .filter(|e| !e.name.is_empty() && e.name != "\0" && e.name != "\u{1}")
        {
            out.push(e);
        }
        o = o.saturating_add(l);
    }
    out
}

fn list(src: &dyn Sectors, d: &Entry, prefetch: u64) -> Got<Vec<Entry>> {
    let n = d.size.div_ceil(SECTOR).clamp(1, MAX_DIR_SECTORS);
    match read_or_need(src, d.lba, n, prefetch) {
        Got::Ok(b) => Got::Ok(entries(&b)),
        Got::Need(r) => Got::Need(r),
        Got::No => Got::No,
    }
}

fn find<'e>(es: &'e [Entry], name: &str) -> Option<&'e Entry> {
    es.iter().find(|e| e.name.eq_ignore_ascii_case(name))
}

fn file(src: &dyn Sectors, e: &Entry, max: u64) -> Got<Vec<u8>> {
    let size = e.size.min(max);
    match read_or_need(src, e.lba, size.div_ceil(SECTOR).max(1), 1) {
        Got::Ok(mut b) => {
            b.truncate(us(size));
            Got::Ok(b)
        }
        other => other,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Disc {
    Psp,
    Ps3,
    PsX,
}

/// Walks a PlayStation disc image.
fn walk(v: &View, src: &dyn Sectors, compressed: bool) -> Option<Hit> {
    let ps3_marker = v.at(0x800, 12) == Some(b"PlayStation3") && !compressed;
    let Some(p) = pvd(src) else {
        // A CSO hides the volume descriptor in its blocks: ask for them (cooked and raw images
        // get theirs from disc.rs).
        if compressed {
            let r = src.need(16, PREFETCH_SECTORS);
            if !r.is_empty() {
                let mut hit = pending(Disc::Psp, true);
                hit.conf = 94;
                hit.need = r;
                return Some(hit);
            }
        }
        return None;
    };
    let sys = p.system.to_ascii_uppercase();
    let mut disc = if sys.starts_with("PSP GAME") {
        Some(Disc::Psp)
    } else if ps3_marker || sys.starts_with("PS3") {
        Some(Disc::Ps3)
    } else if sys.starts_with("PLAYSTATION") {
        Some(Disc::PsX)
    } else {
        None
    };
    let root = match list(src, &p.root, PREFETCH_SECTORS) {
        Got::Ok(r) => r,
        // Root directory not in hand: only drive the fetch when the PVD named the platform
        // (PS1 / PS2 discs are left to disc.rs, which asks for SYSTEM.CNF itself).
        Got::Need(r) => {
            return match disc {
                Some(d @ (Disc::Psp | Disc::Ps3)) => {
                    let mut hit = pending(d, compressed);
                    hit.need = r;
                    Some(hit)
                }
                _ => None,
            };
        }
        Got::No => return None,
    };
    if find(&root, "PSP_GAME").is_some_and(|e| e.dir) || find(&root, "UMD_DATA.BIN").is_some() {
        disc = Some(Disc::Psp);
    } else if find(&root, "PS3_GAME").is_some_and(|e| e.dir)
        || find(&root, "PS3_DISC.SFB").is_some()
    {
        disc = Some(Disc::Ps3);
    } else if find(&root, "SYSTEM.CNF").is_some() {
        disc = Some(Disc::PsX);
    }
    match disc? {
        Disc::Psp | Disc::Ps3 => sfo_disc(src, &root, disc?, compressed),
        Disc::PsX => system_cnf(src, &root, &p.volume),
    }
}

fn pending(d: Disc, compressed: bool) -> Hit {
    let (label, plat, fmt) = match (d, compressed) {
        (Disc::Psp, true) => ("game.psp.cso", Platform::Psp, "CSO"),
        (Disc::Psp, false) => ("game.psp.iso", Platform::Psp, "PSP ISO"),
        _ => ("game.ps3.iso", Platform::Ps3, "PS3 ISO"),
    };
    let mut g = GameInfo::new(
        plat,
        if compressed {
            Kind::CompressedImage
        } else {
            Kind::Image
        },
        fmt,
    );
    g.content = Some(Content::Game);
    with_game(Hit::new(label, 96), g)
}

fn sfo_disc(src: &dyn Sectors, root: &[Entry], d: Disc, compressed: bool) -> Option<Hit> {
    let mut hit = pending(d, compressed);
    hit.conf = 98;
    let mut g = hit.game.take()?;
    let dir_name = if d == Disc::Psp {
        "PSP_GAME"
    } else {
        "PS3_GAME"
    };
    // UMD_DATA.BIN starts with the disc ID ("ULES-01234|...").
    if d == Disc::Psp
        && let Some(u) = find(root, "UMD_DATA.BIN")
        && let Got::Ok(b) = file(src, u, 64)
        && let Some(id) = ascii_field(&b, 0, 10).map(|s| s.replace('-', ""))
    {
        g.title_id = Some(id);
    }
    let Some(sub) = find(root, dir_name).filter(|e| e.dir) else {
        return Some(with_game(hit, g));
    };
    let es = match list(src, sub, 8) {
        Got::Ok(es) => es,
        Got::Need(r) => {
            hit.need = r;
            return Some(with_game(hit, g));
        }
        Got::No => return Some(with_game(hit, g)),
    };
    let Some(sfo) = find(&es, "PARAM.SFO") else {
        return Some(with_game(hit, g));
    };
    match file(src, sfo, 0x10000) {
        Got::Ok(b) => {
            if let Some(kv) = sony::parse_sfo(&b) {
                sony::sfo_fill(&mut g, &kv, &mut hit);
                if d == Disc::Psp {
                    // Disc games are UG; an update partition on the disc does not change that.
                    g.content = Some(Content::Game);
                }
            }
        }
        Got::Need(r) => hit.need = r,
        Got::No => {}
    }
    Some(with_game(hit, g))
}

fn system_cnf(src: &dyn Sectors, root: &[Entry], volume: &str) -> Option<Hit> {
    let e = find(root, "SYSTEM.CNF")?;
    let Got::Ok(b) = file(src, e, 2048) else {
        // disc.rs asks for SYSTEM.CNF.
        return None;
    };
    let t = String::from_utf8_lossy(&b);
    let ps2 = t.contains("BOOT2");
    let mut g = GameInfo::new(
        if ps2 { Platform::Ps2 } else { Platform::Ps1 },
        Kind::Image,
        if ps2 { "PS2 ISO" } else { "PS1 ISO" },
    );
    let mut hit = Hit::new(if ps2 { "game.ps2.iso" } else { "game.ps1.iso" }, 98);
    for line in t.lines().take(32) {
        let line = line.trim();
        let (k, val) = line
            .split_once('=')
            .map_or((line, ""), |(a, b)| (a.trim(), b.trim()));
        match k {
            "BOOT" | "BOOT2" => {
                g.title_id = boot_serial(val);
                hit.push("boot", line.to_owned());
            }
            "VER" if !val.is_empty() => g.version = Some(val.to_owned()),
            "VMODE" if !val.is_empty() => hit.push("video_mode", val.to_owned()),
            _ => {}
        }
    }
    if !volume.is_empty() {
        g.title = Some(volume.to_owned());
    }
    g.content = Some(Content::Game);
    Some(with_game(hit, g))
}

/// `cdrom0:\SLES_123.45;1` -> `SLES12345` (four letters, five digits, as GameTDB-style keys).
fn boot_serial(path: &str) -> Option<String> {
    let name = path.rsplit(['\\', ':', '/']).next()?;
    let stem = name.split(';').next()?;
    let id: String = stem.chars().filter(char::is_ascii_alphanumeric).collect();
    let id = id.to_ascii_uppercase();
    (id.len() == 9
        && id
            .get(..4)
            .is_some_and(|p| p.bytes().all(|c| c.is_ascii_uppercase()))
        && id
            .get(4..)
            .is_some_and(|p| p.bytes().all(|c| c.is_ascii_digit())))
    .then_some(id)
}

/// Cooked or raw PlayStation disc images.
pub(crate) fn sony_iso(v: &View) -> Option<Hit> {
    let h = v.head();
    let sync = [
        0u8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0,
    ];
    let mut hit = if at(h, 0, &sync) {
        let mode = u8at(h, 15)?;
        let src = Plain {
            v,
            size: 2352,
            data_off: if mode == 2 { 24 } else { 16 },
        };
        let mut hit = walk(v, &src, false)?;
        if hit.label() == "game.ps1.iso" {
            hit.relabel("game.ps1.bin");
            if let Some(g) = hit.game.as_mut() {
                g.format = "PS1 BIN";
            }
        }
        hit
    } else {
        walk(
            v,
            &Plain {
                v,
                size: SECTOR,
                data_off: 0,
            },
            false,
        )?
    };
    hit.source = "sony_iso".into();
    Some(hit)
}

/// PSP CSO ("CISO", header size 0x18, block size 0x800): the ISO walk through the blocks.
pub(crate) fn psp_cso(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"CISO") {
        return None;
    }
    let hdr = u32le(h, 4)?;
    let total = u64le(h, 8)?;
    let block = u64::from(u32le(h, 16)?);
    let version = u8at(h, 20)?;
    let align = u32::from(u8at(h, 21)?);
    if !(hdr == 0x18 || hdr == 0) || block != 0x800 || align > 8 || total == 0 {
        return None;
    }
    let src = Cso {
        v,
        block,
        align,
        version,
        blocks: total.div_ceil(block),
    };
    let mut hit = walk(v, &src, true)?;
    hit.relabel("game.psp.cso");
    if let Some(g) = hit.game.as_mut() {
        g.kind = Kind::CompressedImage;
        g.format = "CSO";
    }
    hit.push("uncompressed_size", total);
    Some(hit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_from_boot_line() {
        assert_eq!(
            boot_serial("cdrom0:\\SLES_123.45;1").as_deref(),
            Some("SLES12345")
        );
        assert_eq!(
            boot_serial("cdrom:\\slus_000.01;1").as_deref(),
            Some("SLUS00001")
        );
        assert_eq!(boot_serial("cdrom:PSX.EXE;1"), None);
    }
}
