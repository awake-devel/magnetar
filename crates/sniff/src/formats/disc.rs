//! ISO 9660 / UDF disc images (cooked 2048-byte and raw 2352-byte sectors): volume descriptors,
//! root directory walk (BDMV, VIDEO_TS, PS1/PS2/PSP/PS3 markers) and SYSTEM.CNF for PlayStation
//! discs. Also the all-zero-head case that asks for the deeper fixed offsets. Virtual disks and
//! file systems with plain magics live in `data/signatures/disc.toml`.
//!
//! Blu-ray: UDF and BDMV directory walking is not implemented; only "UDF >= 2.00 and > 15 GB"
//! is reported as `bluray_likely`.

use crate::formats::console::{GameInfo, Kind, Platform, with_game};
use crate::types::Hit;
use crate::util::*;
use crate::view::View;

pub(crate) static CHECKS: &[(&str, crate::formats::Check)] =
    &[("iso", iso), ("raw_cd", raw_cd), ("zero_head", zero_head)];

#[derive(Clone, Copy)]
struct Sect {
    size: u64,
    data_off: u64,
}

fn iso_like(v: &View, s: Sect, raw: bool) -> Option<Hit> {
    let lba = |n: u64| n.saturating_mul(s.size).saturating_add(s.data_off);
    // Volume descriptors from sector 16.
    let mut pvd: Option<&[u8]> = None;
    let mut udf: Option<&'static str> = None;
    let mut saw_cd001 = false;
    for i in 16..32u64 {
        let Some(vd) = v.at(lba(i), 2048) else {
            if i == 16 {
                return None;
            }
            break;
        };
        match sl(vd, 1, 6) {
            b"CD001" => {
                saw_cd001 = true;
                if u8at(vd, 0) == Some(1) && pvd.is_none() {
                    pvd = Some(vd);
                }
            }
            b"BEA01" | b"TEA01" | b"BOOT2" | b"CDW02" => {}
            b"NSR02" => udf = Some("1.02-1.50"),
            b"NSR03" => udf = Some(">=2.00"),
            _ => {
                if i == 16 {
                    return None;
                }
                break;
            }
        }
    }
    if !saw_cd001 && udf.is_none() {
        return None;
    }
    let label = if raw {
        "disc.cd_raw"
    } else if pvd.is_some() {
        "disc.iso9660"
    } else {
        "disc.udf"
    };
    let mut hit = Hit::new(label, 90);
    if let Some(u) = udf {
        hit.push("udf", u);
    }
    if raw {
        hit.push("sector_size", s.size);
        hit.push("sector_data_offset", s.data_off);
    }
    let Some(pvd) = pvd else {
        hit.push(
            "note",
            "UDF-only image: directory walk needs a UDF parser (BD-ROM is UDF 2.50)",
        );
        if udf == Some(">=2.00") && v.size() > 15 << 30 {
            hit.push("bluray_likely", true);
        }
        return Some(hit);
    };
    let sys = cstr(sl(pvd, 8, 40));
    let vol = cstr(sl(pvd, 40, 72));
    hit.push("system_id", sys.clone());
    hit.push("volume_id", vol.clone());
    hit.conf = 95;
    // Root directory record at 156: extent LBA at +2, data length at +10.
    let root_lba = u64::from(u32le(pvd, 156 + 2)?);
    let root_len = u64::from(u32le(pvd, 156 + 10)?).min(64 * 2048);
    let dir: Option<Vec<u8>> = if s.size == 2048 {
        v.at(lba(root_lba), us(root_len)).map(<[u8]>::to_vec)
    } else {
        // Raw sectors: stitch the user data of each sector.
        let mut out = Vec::new();
        for k in 0..root_len.div_ceil(2048) {
            match v.at(lba(root_lba.saturating_add(k)), 2048) {
                Some(b) => out.extend_from_slice(b),
                None => {
                    out.clear();
                    break;
                }
            }
        }
        (!out.is_empty()).then_some(out)
    };
    let Some(dir) = dir else {
        let start = lba(root_lba);
        if start >= v.size() {
            return Some(hit);
        }
        let span = if s.size == 2048 {
            root_len
        } else {
            root_len.div_ceil(2048).saturating_mul(s.size)
        };
        return Some(hit.need(start, span.max(1)));
    };
    let mut entries: Vec<(String, u64, u64)> = Vec::new();
    let mut o = 0usize;
    while o < dir.len() && entries.len() < 4096 {
        let l = usize::from(u8at(&dir, o).unwrap_or(0));
        if l == 0 {
            o = (o / 2048 + 1) * 2048;
            continue;
        }
        if o + 33 > dir.len() {
            break;
        }
        let extent = u64::from(u32le(&dir, o + 2).unwrap_or(0));
        let sz = u64::from(u32le(&dir, o + 10).unwrap_or(0));
        let nl = usize::from(u8at(&dir, o + 32).unwrap_or(0));
        let name = String::from_utf8_lossy(sln(&dir, o + 33, nl)).to_string();
        let name = name.split(';').next().unwrap_or("").to_string();
        if nl > 1 || (nl == 1 && u8at(&dir, o + 33).unwrap_or(0) > 1) {
            entries.push((name, extent, sz));
        }
        o += l;
    }
    let has = |n: &str| entries.iter().any(|e| e.0.eq_ignore_ascii_case(n));
    hit.push(
        "root",
        entries
            .iter()
            .take(12)
            .map(|e| e.0.clone())
            .collect::<Vec<_>>(),
    );
    let found = if has("BDMV") {
        "disc.bluray"
    } else if has("VIDEO_TS") {
        "disc.dvd_video"
    } else if has("PS3_GAME") || has("PS3_DISC.SFB") {
        "game.ps3.iso"
    } else if has("PSP_GAME") || has("UMD_DATA.BIN") {
        "game.psp.iso"
    } else if has("SYSTEM.CNF") || sys.contains("PLAYSTATION") {
        "ps?"
    } else {
        ""
    };
    let ps1 = if raw { "game.ps1.bin" } else { "game.ps1.iso" };
    if found == "ps?" {
        if let Some(e) = entries
            .iter()
            .find(|e| e.0.eq_ignore_ascii_case("SYSTEM.CNF"))
        {
            match v.at(lba(e.1), us(e.2.min(2048))) {
                Some(c) => {
                    let t = String::from_utf8_lossy(c);
                    let ps2 = t.contains("BOOT2");
                    hit.relabel(if ps2 { "game.ps2.iso" } else { ps1 });
                    hit.conf = 97;
                    let mut g = GameInfo::new(
                        if ps2 { Platform::Ps2 } else { Platform::Ps1 },
                        Kind::Image,
                        match (ps2, raw) {
                            (true, _) => "PS2 ISO",
                            (false, true) => "PS1 BIN",
                            (false, false) => "PS1 ISO",
                        },
                    );
                    if let Some(b) = t.lines().find(|l| l.starts_with("BOOT")) {
                        let b = b.trim().to_string();
                        g.title_id = boot_title_id(&b);
                        hit.push("boot", b);
                    }
                    if !vol.is_empty() {
                        g.title = Some(vol.clone());
                    }
                    hit.game = Some(g);
                }
                None if lba(e.1) < v.size() => return Some(hit.need(lba(e.1), e.2.clamp(1, 2048))),
                None => {}
            }
        } else {
            hit.relabel(ps1);
            hit.conf = 80;
        }
    } else if !found.is_empty() {
        hit.relabel(found);
        hit.conf = 97;
        let platform = match found {
            "game.ps3.iso" => Some((Platform::Ps3, "PS3 ISO")),
            "game.psp.iso" => Some((Platform::Psp, "PSP ISO")),
            _ => None,
        };
        if let Some((p, f)) = platform {
            let mut g = GameInfo::new(p, Kind::Image, f);
            if !vol.is_empty() {
                g.title = Some(vol.clone());
            }
            return Some(with_game(hit, g));
        }
    } else if raw {
        hit.relabel("disc.cd_raw");
    }
    Some(hit)
}

/// `BOOT2 = cdrom0:\SLES_123.45;1` -> `SLES12345`.
fn boot_title_id(line: &str) -> Option<String> {
    let path = line.split(['\\', ':']).next_back()?;
    let stem = path.split(';').next()?;
    let id: String = stem.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    (id.len() == 9
        && id
            .get(..4)
            .is_some_and(|p| p.bytes().all(|c| c.is_ascii_uppercase())))
    .then_some(id)
}

fn iso(v: &View) -> Option<Hit> {
    if !v.has(0x8000, 2048) {
        return None;
    }
    iso_like(
        v,
        Sect {
            size: 2048,
            data_off: 0,
        },
        false,
    )
}

fn raw_cd(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(
        h,
        0,
        &[
            0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0,
        ],
    ) {
        return None;
    }
    let mode = u8at(h, 15)?;
    let s = Sect {
        size: 2352,
        data_off: if mode == 2 { 24 } else { 16 },
    };
    if !v.has(16 * 2352, 2352) {
        return Some(Hit::new("disc.cd_raw", 85).need(16 * 2352, 2352 * 2));
    }
    iso_like(v, s, true).or_else(|| Some(Hit::new("disc.cd_raw", 85).d("mode", mode)))
}

/// A head of zeros: ISO 9660/UDF descriptors at 0x8000 or XDVDFS at 0x10000 may follow. Ask
/// for the descriptor area while it is unknown.
fn zero_head(v: &View) -> Option<Hit> {
    let h = v.head();
    let size = v.size();
    if v.has(0x8000, 2048) {
        return None;
    }
    if size >= 0x10800
        && h.len() >= 0x8000
        && pre(h, 0x8000).iter().all(|&b| b == 0)
        && !v.has(0x10000, 20)
    {
        return Some(Hit::new("misc.zeros", 40).need(0x8000, 0x8800));
    }
    if size >= 0x8800 && h.len() < 0x8800 && h.len() >= 2048 && pre(h, 2048).iter().all(|&b| b == 0)
    {
        return Some(Hit::new("misc.zeros", 40).need(0x8000, 0x2000));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_line_title_id() {
        assert_eq!(
            boot_title_id("BOOT2 = cdrom0:\\SLES_123.45;1").as_deref(),
            Some("SLES12345")
        );
        assert_eq!(
            boot_title_id("BOOT = cdrom:\\SLUS_000.01;1").as_deref(),
            Some("SLUS00001")
        );
        assert_eq!(boot_title_id("BOOT = x"), None);
    }
}
