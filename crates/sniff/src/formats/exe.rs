//! Executables: PE (sections, .NET, installers and self-extractors from the overlay), ELF
//! (AppImage, PS4/PS5 style), Mach-O / Java class (shared magic), and shebang scripts.

use crate::types::Hit;
use crate::util::*;
use crate::view::View;

pub(crate) static CHECKS: &[(&str, crate::formats::Check)] = &[
    ("pe", pe),
    ("elf", elf),
    ("macho_java", macho_java),
    ("shebang", shebang),
];

fn pe(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"MZ") {
        return None;
    }
    let size = v.size();
    let lfa = u64::from(u32le(h, 0x3C)?);
    let Some(nt) = v.at(lfa, 24) else {
        if lfa < 1 << 20 && lfa < size {
            return Some(
                Hit::new("exe.pe", 60)
                    .d("note", "DOS stub only so far")
                    .need(lfa, 4096),
            );
        }
        return Some(Hit::new("exe.mz_dos", 60));
    };
    if !at(nt, 0, b"PE\0\0") {
        if at(nt, 0, b"NE") {
            return Some(Hit::new("exe.ne", 90));
        }
        if at(nt, 0, b"LE") || at(nt, 0, b"LX") {
            return Some(Hit::new("exe.le", 90));
        }
        return Some(Hit::new("exe.mz_dos", 75));
    }
    let machine = u16le(nt, 4)?;
    let nsec = usize::from(u16le(nt, 6)?);
    let osz = usize::from(u16le(nt, 20)?);
    let chars = u16le(nt, 22)?;
    let opt_off = lfa + 24;
    let need_len = osz + nsec * 40;
    let Some(opt) = v.at(opt_off, need_len) else {
        return Some(Hit::new("exe.pe", 85).need(opt_off, need_len as u64));
    };
    let magic = u16le(opt, 0)?;
    let pe32p = magic == 0x20b;
    let subsys = u16le(opt, 68)?;
    let ddir = if pe32p { 112 } else { 96 };
    let clr = u32le(opt, ddir + 14 * 8).unwrap_or(0) != 0;
    let arch = match machine {
        0x14c => "x86",
        0x8664 => "x86-64",
        0xaa64 => "arm64",
        0x1c0 | 0x1c4 => "arm",
        0x200 => "ia64",
        _ => "other",
    };
    let subsystem = match subsys {
        2 => "gui",
        3 => "console",
        10..=13 => "efi",
        1 => "native",
        _ => "other",
    };
    let mut hit = Hit::new(if clr { "exe.pe.dotnet" } else { "exe.pe" }, 97)
        .d("arch", arch)
        .d("bits", if pe32p { "64" } else { "32" })
        .d("kind", if chars & 0x2000 != 0 { "dll" } else { "exe" })
        .d("subsystem", subsystem);
    let mut end = 0u64;
    let mut sects = Vec::new();
    for s in from(opt, osz).as_chunks::<40>().0.iter().take(nsec) {
        let rs = u64::from(u32le(s, 16)?);
        let rp = u64::from(u32le(s, 20)?);
        if rs > 0 {
            end = end.max(rp.saturating_add(rs));
        }
        sects.push(cstr(pre(s, 8)));
    }
    let has_sec = |n: &str| sects.iter().any(|s| s == n);
    if has_sec(".ndata") {
        hit.relabel("exe.pe.nsis");
        hit.push("installer", "nsis (.ndata section)");
    }
    if has_sec(".wixburn") {
        hit.push("installer", "wix burn bundle");
    }
    if has_sec("UPX0") || has_sec("UPX1") {
        hit.push("packer", "upx");
    }
    if has_sec(".themida") || has_sec(".vmp0") || has_sec(".enigma1") {
        hit.push("packer", "protector");
    }
    if has_sec("_winzip_") {
        hit.relabel("exe.pe.sfx_zip");
    }
    hit.push(
        "sections",
        sects.iter().take(32).cloned().collect::<Vec<_>>(),
    );
    // Overlay = bytes after the last section: installers and SFX payloads live there.
    if end > 0 && size > end.saturating_add(16) {
        hit.push("overlay_bytes", size - end);
        hit.push("overlay_at", end);
        let base = end & !511;
        let want = (64 * 1024u64).min(size - base);
        let ov = v.at(base, us(want)).or_else(|| {
            let b = v.from(base);
            (b.len() as u64 >= 4096u64.min(size - base)).then_some(b)
        });
        match ov {
            Some(ov) => overlay(&mut hit, ov, us(end - base)),
            None => hit = hit.need(base, want),
        }
    }
    Some(hit)
}

fn overlay(hit: &mut Hit, ov: &[u8], skip: usize) {
    let o = from(ov, skip);
    let o4k = pre(o, 4096);
    let nsis = (0..ov.len().saturating_sub(16))
        .step_by(512)
        .any(|i| at(ov, i + 4, &[0xEF, 0xBE, 0xAD, 0xDE]) && at(ov, i + 8, b"NullsoftInst"));
    if nsis {
        hit.relabel("exe.pe.nsis");
        hit.push("installer", "nsis");
    } else if contains(o, b"Inno Setup Setup Data")
        || contains(o, b"rDlPtS")
        // Inno 5.1.5+ keeps the setup ID and the SetupLdr offset table (rDlPtS) inside the
        // image (an RCDATA resource); the overlay starts with its compressed block magic
        // "zlb\x1A" (Inno 7.1.1, checked on the official installer).
        || at(o, 0, b"zlb\x1a")
    {
        hit.relabel("exe.pe.inno");
        hit.push("installer", "inno setup");
    } else if contains(o4k, b"Rar!\x1a\x07") {
        hit.relabel("exe.pe.sfx_rar");
    } else if contains(o4k, &[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        hit.relabel("exe.pe.sfx_7z");
    } else if at(o, 0, b"PK\x03\x04") {
        hit.relabel("exe.pe.sfx_zip");
    } else if at(o, 0, b"MSCF") {
        hit.relabel("exe.pe.sfx_cab");
    } else if at(o, 0, &[0xD0, 0xCF, 0x11, 0xE0]) {
        hit.push("overlay_kind", "embedded msi/cfb");
    } else if entropy(o4k) > 7.5 {
        hit.push("overlay_kind", "high-entropy (unknown packed payload)");
    }
}

fn elf(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"\x7FELF") {
        return None;
    }
    let class = u8at(h, 4)?;
    let data = u8at(h, 5)?;
    if !(1..=2).contains(&class) || !(1..=2).contains(&data) {
        return Some(Hit::new("exe.elf", 60));
    }
    let rd16 = |o| if data == 1 { u16le(h, o) } else { u16be(h, o) };
    let ty = rd16(16)?;
    let m = rd16(18)?;
    let machine = match m {
        3 => "x86",
        62 => "x86-64",
        40 => "arm",
        183 => "aarch64",
        8 => "mips",
        20 | 21 => "ppc",
        243 => "riscv",
        _ => "other",
    };
    let kind = match ty {
        1 => "relocatable",
        2 => "exec",
        3 => "dyn (pie or .so)",
        4 => "core",
        _ => "other",
    };
    let mut hit = Hit::new("exe.elf", 97)
        .d("bits", if class == 2 { "64" } else { "32" })
        .d("machine", machine)
        .d("type", kind);
    if at(h, 8, b"AI\x02") || at(h, 8, b"AI\x01") {
        hit.relabel("exe.appimage");
    }
    if u8at(h, 7) == Some(9) {
        hit.push("osabi", "freebsd (ps4/ps5 eboot style)");
    }
    Some(hit)
}

fn macho_java(v: &View) -> Option<Hit> {
    let h = v.head();
    let m = u32be(h, 0)?;
    match m {
        0xFEEDFACE | 0xFEEDFACF | 0xCEFAEDFE | 0xCFFAEDFE => {
            let le = m == 0xCEFAEDFE || m == 0xCFFAEDFE;
            let ft = if le { u32le(h, 12)? } else { u32be(h, 12)? };
            let kind = match ft {
                1 => "object",
                2 => "exec",
                6 => "dylib",
                8 => "bundle",
                _ => "other",
            };
            Some(Hit::new("exe.macho", 97).d("filetype", kind))
        }
        0xCAFEBABE | 0xCAFEBABF => {
            let n = u32be(h, 4)?;
            // Java class: u16 minor, u16 major (>= 45). Fat Mach-O: few archs, known cputype.
            let cpu = u32be(h, 8).unwrap_or(0);
            let known_cpu =
                matches!(cpu & 0x00FF_FFFF, 7 | 12 | 18) && (cpu >> 24 == 0 || cpu >> 24 == 1);
            if n > 0 && n < 45 && known_cpu {
                return Some(
                    Hit::new("exe.macho", 96)
                        .d("variant", "universal (fat)")
                        .d("archs", n),
                );
            }
            let major = u16be(h, 6)?;
            if (45..=80).contains(&major) {
                return Some(Hit::new("exe.java_class", 96).d("major", major));
            }
            Some(Hit::new("exe.macho", 50))
        }
        _ => None,
    }
}

fn shebang(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"#!") {
        return None;
    }
    let line = pre(h, 128);
    let line = pre(
        line,
        line.iter().position(|&c| c == b'\n').unwrap_or(line.len()),
    );
    if !printable(line) {
        return None;
    }
    // "#!" is a 2-byte magic: the interpreter's absolute path is the check ("#!AMR" is not
    // a script).
    let path = sl(line, 2, line.len())
        .iter()
        .find(|&&c| c != b' ' && c != b'\t')
        == Some(&b'/');
    let mut hit = Hit::new(
        if contains(line, b"python") {
            "text.python"
        } else {
            "text.shell"
        },
        if path { 85 } else { crate::types::WEAK },
    );
    let w = pre(h, 16384);
    if contains(w, b"Makeself") {
        hit.push("installer", "makeself self-extracting (payload appended)");
    }
    if contains(w, b"shell archive") {
        hit.push("variant", "shar");
    }
    Some(hit)
}
