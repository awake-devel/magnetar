//! Regressions found on real homebrew samples (`~/sniff-samples`, see tests/real_samples.rs),
//! rebuilt here from the header values of those files so they run everywhere: Inno Setup's
//! overlay magic, GameCube / Wii DOL, logo-less NDS, homebrew SNES and Mega Drive headers, the
//! Vita SELF and VPK, PSP PBP titles, NRO assets, Switch ExeFS, GameCube bi2 regions, 3DS
//! homebrew product codes and the PSP Memory Stick listing.

mod support;

use sniff::{Content, Platform, SparseFile, listing};
use support::fixtures::*;

fn sfo(entries: &[(&str, &str)]) -> Vec<u8> {
    let n = entries.len();
    let mut keys = Vec::new();
    let mut koffs = Vec::new();
    for (k, _) in entries {
        koffs.push(keys.len() as u16);
        keys.extend_from_slice(k.as_bytes());
        keys.push(0);
    }
    while keys.len() % 4 != 0 {
        keys.push(0);
    }
    let kt = 20 + n * 16;
    let dt = kt + keys.len();
    let mut data = Vec::new();
    let mut b = vec![0u8; dt];
    put(&mut b, 0, b"\0PSF");
    put(&mut b, 4, &0x0101u32.to_le_bytes());
    put(&mut b, 8, &(kt as u32).to_le_bytes());
    put(&mut b, 12, &(dt as u32).to_le_bytes());
    put(&mut b, 16, &(n as u32).to_le_bytes());
    for (i, (_, v)) in entries.iter().enumerate() {
        let e = 20 + i * 16;
        let len = v.len() + 1;
        put(&mut b, e, &koffs[i].to_le_bytes());
        put(&mut b, e + 2, &0x0204u16.to_le_bytes());
        put(&mut b, e + 4, &(len as u32).to_le_bytes());
        put(
            &mut b,
            e + 8,
            &(len.next_multiple_of(4) as u32).to_le_bytes(),
        );
        put(&mut b, e + 12, &(data.len() as u32).to_le_bytes());
        data.extend_from_slice(v.as_bytes());
        data.push(0);
        while data.len() % 4 != 0 {
            data.push(0);
        }
    }
    put(&mut b, kt, &keys);
    b.extend(data);
    b
}

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

/// The final label, "" when the detector gives up.
fn verdict(b: &[u8], name: &str) -> String {
    let f = SparseFile::from_bytes(b.to_vec()).with_name(name);
    match sniff::Detector::new().finish(&f) {
        sniff::Outcome::Detected(d) => d.label,
        _ => String::new(),
    }
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn le32(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

/// A PE with one section and an overlay starting with Inno's "zlb\x1A" block magic (Inno
/// 5.1.5+ keeps "Inno Setup Setup Data" and rDlPtS inside the image).
#[test]
fn inno_setup_overlay_block_magic() {
    let mut b = vec![0u8; 0x3000];
    put(&mut b, 0, b"MZ");
    put(&mut b, 0x3C, &le32(0x80));
    put(&mut b, 0x80, b"PE\0\0");
    put(&mut b, 0x84, &0x8664u16.to_le_bytes());
    put(&mut b, 0x86, &1u16.to_le_bytes()); // one section
    put(&mut b, 0x94, &0xF0u16.to_le_bytes()); // optional header size
    put(&mut b, 0x96, &0x22u16.to_le_bytes());
    put(&mut b, 0x98, &0x20Bu16.to_le_bytes()); // PE32+
    put(&mut b, 0x98 + 68, &2u16.to_le_bytes()); // GUI
    let sh = 0x98 + 0xF0;
    put(&mut b, sh, b".text\0\0\0");
    put(&mut b, sh + 8, &le32(0x1000));
    put(&mut b, sh + 12, &le32(0x1000));
    put(&mut b, sh + 16, &le32(0x1000)); // raw size
    put(&mut b, sh + 20, &le32(0x1000)); // raw offset
    // Overlay at 0x2000: Inno's compressed block header, then high-entropy data.
    put(&mut b, 0x2000, b"zlb\x1a");
    let mut x = 0x1234_5678u32;
    for v in b.iter_mut().skip(0x2004) {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *v = x as u8;
    }
    let d = det(&b, "setup.exe");
    assert_eq!(d.label, "exe.pe.inno", "{:?}", d.fields);
}

/// The header of Swiss r2119's swiss_r2119.dol (one text, one data section, BSS).
fn swiss_dol() -> Vec<u8> {
    let mut b = vec![0u8; 0x14_9A40];
    put(&mut b, 0, &be32(0x100)); // text0 offset
    put(&mut b, 0x1C, &be32(0x3800)); // data0 offset
    put(&mut b, 0x48, &be32(0x8039_7940)); // text0 address
    put(&mut b, 0x64, &be32(0x8039_B020)); // data0 address
    put(&mut b, 0x90, &be32(0x36E0)); // text0 size
    put(&mut b, 0xAC, &be32(0x14_6240)); // data0 size
    put(&mut b, 0xD8, &be32(0x804E_1260)); // bss
    put(&mut b, 0xDC, &be32(0x7820));
    put(&mut b, 0xE0, &be32(0x8039_7940)); // entry
    b
}

#[test]
fn gamecube_wii_dol() {
    let b = swiss_dol();
    let d = det(&b, "boot.dol");
    assert_eq!((d.label.as_str(), d.confidence), ("exe.dol", 95));
    // The entry point outside every text section: not a DOL.
    let mut bad = b.clone();
    put(&mut bad, 0xE0, &be32(0x8100_0000));
    assert_ne!(verdict(&bad, "boot.dol"), "exe.dol");
    // A section past the end of the file: not a DOL.
    let mut short = b;
    short.truncate(0x10_0000);
    assert_ne!(verdict(&short, "boot.dol"), "exe.dol");
}

/// hbmenu's argvTest.nds: ndstool build with no Nintendo logo, game code "####", valid
/// header CRC16.
#[test]
fn nds_without_logo_on_header_crc() {
    let mut b = vec![0u8; 0x21E40];
    put(&mut b, 0, b".\0\0\xea");
    put(&mut b, 0xC, b"####");
    put(&mut b, 0x10, b"##");
    put(&mut b, 0x14, &[1]);
    put(&mut b, 0x20, &le32(0x200));
    put(&mut b, 0x24, &le32(0x0200_0000));
    put(&mut b, 0x28, &le32(0x0200_0000));
    put(&mut b, 0x2C, &le32(0x117AC));
    put(&mut b, 0x30, &le32(0x11A00));
    put(&mut b, 0x34, &le32(0x037F_8000));
    put(&mut b, 0x38, &le32(0x037F_8000));
    put(&mut b, 0x3C, &le32(0xF9E8));
    put(&mut b, 0x80, &le32(0x21E40));
    put(&mut b, 0x84, &le32(0x200));
    put(&mut b, 0xC0, &[0xC8, 0x60, 0x4F, 0xE2, 0x01, 0x70]);
    put(&mut b, 0x15C, &0x9E1Au16.to_le_bytes());
    let crc = crc16(&b[..0x15E]);
    put(&mut b, 0x15E, &crc.to_le_bytes());
    let d = det(&b, "argvTest.nds");
    assert_eq!((d.label.as_str(), d.confidence), ("game.nds.rom", 85));
    let g = d.game.unwrap();
    assert_eq!((g.title_id, g.db_id), (None, None));
    // A wrong header CRC: nothing.
    put(&mut b, 0x15E, &(crc ^ 1).to_le_bytes());
    assert_ne!(verdict(&b, "argvTest.nds"), "game.nds.rom");
}

/// The SNES "BANK LoROM SlowROM" test ROM: ROM size byte 2, "CCCS" in the checksum words.
#[test]
fn snes_homebrew_header_without_checksum() {
    let mut b = vec![0u8; 0x10000];
    put(&mut b, 0x7FC0, b"BANK LOROM SLOWROM   ");
    put(&mut b, 0x7FD5, &[0x20, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00]);
    put(&mut b, 0x7FDC, b"CCCS");
    put(&mut b, 0x7FFC, &0x8000u16.to_le_bytes());
    let d = det(&b, "bank.sfc");
    assert_eq!((d.label.as_str(), d.confidence), ("game.snes.rom", 85));
    assert_eq!(d.game.unwrap().title.as_deref(), Some("BANK LOROM SLOWROM"));
    // The HiROM variant starts with 32 KiB of zeros: only the second chance reaches 0xFFC0.
    let mut hi = vec![0u8; 0x20000];
    put(&mut hi, 0xFFC0, b"BANK HIROM FASTROM   ");
    put(&mut hi, 0xFFD5, &[0x21, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00]);
    put(&mut hi, 0xFFDC, b"CCCS");
    put(&mut hi, 0xFFFC, &0x8000u16.to_le_bytes());
    let r = sniff::detect_with(
        |r| Ok::<_, ()>(hi[r.offset as usize..(r.offset + r.len) as usize].to_vec()),
        hi.len() as u64,
        Some("bank.sfc"),
        &sniff::Caps::default(),
    )
    .unwrap();
    let sniff::Outcome::Detected(d) = r.outcome else {
        panic!("{:?}", r.outcome)
    };
    assert_eq!(d.label, "game.snes.rom");
}

#[test]
fn a_zero_head_is_not_a_confident_padding_file() {
    // The driver reads the head, the second-chance offsets and the tail: all zero, but most
    // of the file was never read.
    let zeros = vec![0u8; 1 << 20];
    let r = sniff::detect_with(
        |r| Ok::<_, ()>(zeros[r.offset as usize..(r.offset + r.len) as usize].to_vec()),
        zeros.len() as u64,
        Some("big.bin"),
        &sniff::Caps::default(),
    )
    .unwrap();
    let sniff::Outcome::Detected(d) = r.outcome else {
        panic!("{:?}", r.outcome)
    };
    assert_eq!(d.label, "misc.zeros");
    assert!(d.confidence < 60, "{}", d.confidence);
    // A small file read whole is a padding file.
    assert_eq!(det(&[0u8; 4096], "pad.bin").confidence, 70);
}

/// SGDK's MegaWiFi sample: system name "SEGA MEGAWIFI".
#[test]
fn megadrive_with_a_non_standard_system_name() {
    let mut b = vec![0u8; 0x20000];
    put(
        &mut b,
        0x100,
        b"SEGA MEGAWIFI   (C)SGDK 2023    SAMPLE PROGRAM",
    );
    put(&mut b, 0x150, b"SAMPLE PROGRAM");
    put(&mut b, 0x180, b"GM 00000000-00");
    put(&mut b, 0x1F0, b"JUE");
    let d = det(&b, "rom.bin");
    assert_eq!(d.label, "game.genesis.rom");
    assert!(d.confidence >= 80, "{}", d.confidence);
    assert_eq!(d.game.unwrap().title.as_deref(), Some("SAMPLE PROGRAM"));
}

/// VitaShell 2.02's eboot.bin: little-endian SCE header.
#[test]
fn vita_self() {
    let mut b = vec![0u8; 0x1C_0D65];
    put(&mut b, 0, b"SCE\0");
    put(&mut b, 4, &le32(3));
    put(&mut b, 8, &[0xC0, 0x00, 0x01, 0x00]);
    put(&mut b, 0xC, &le32(0x600));
    put(&mut b, 0x10, &0x1000u64.to_le_bytes());
    put(&mut b, 0x18, &0x4A_7F06u64.to_le_bytes());
    put(&mut b, 0x20, &0x1C_0D65u64.to_le_bytes());
    let d = det(&b, "eboot.bin");
    assert_eq!((d.label.as_str(), d.confidence), ("exe.vita.self", 95));
}

#[test]
fn vita_vpk_with_its_param_sfo() {
    let s = sfo(&[
        ("APP_VER", "02.02"),
        ("CATEGORY", "gd"),
        ("TITLE", "VitaShell"),
        ("TITLE_ID", "VITASHELL"),
    ]);
    let z = zip_store(&[
        ("sce_sys/param.sfo", &s),
        ("eboot.bin", b"SCE\0"),
        ("sce_sys/icon0.png", b"\x89PNG"),
    ]);
    let d = det(&z, "VitaShell.vpk");
    assert_eq!(d.label, "game.vita.vpk");
    assert!(!d.flags.extension_mismatch);
    let g = d.game.unwrap();
    assert_eq!(
        (
            g.platform,
            g.title.as_deref(),
            g.title_id.as_deref(),
            g.version.as_deref()
        ),
        (
            Platform::PsVita,
            Some("VitaShell"),
            Some("VITASHELL"),
            Some("02.02")
        )
    );
}

/// A homebrew EBOOT.PBP: PARAM.SFO with the pspsdk default DISC_ID and a plain ELF DATA.PSP.
#[test]
fn psp_pbp_title_without_the_sdk_disc_id() {
    let s = sfo(&[
        ("APP_VER", "02.32"),
        ("CATEGORY", "MG"),
        ("DISC_ID", "UCJS10041"),
        ("TITLE", "Apollo Save Tool"),
    ]);
    let data_psp = 0x28 + s.len() as u32;
    let mut b = vec![0u8; data_psp as usize + 0x100];
    put(&mut b, 0, b"\0PBP");
    put(&mut b, 4, &le32(0x10000));
    put(&mut b, 8, &le32(0x28));
    for i in 1..7 {
        put(&mut b, 8 + i * 4, &le32(data_psp));
    }
    put(&mut b, 8 + 7 * 4, &le32(data_psp + 0x80));
    put(&mut b, 0x28, &s);
    put(&mut b, data_psp as usize, b"\x7FELF");
    let d = det(&b, "EBOOT.PBP");
    assert_eq!(d.label, "game.psp.pbp");
    let g = d.game.unwrap();
    assert_eq!(
        (g.title.as_deref(), g.version.as_deref()),
        (Some("Apollo Save Tool"), Some("02.32"))
    );
    assert_eq!((g.title_id, g.db_id), (None, None));
    // Encrypted DATA.PSP (PSN / retail): the DISC_ID is the game's serial.
    put(&mut b, data_psp as usize, b"~PSP");
    let g = det(&b, "EBOOT.PBP").game.unwrap();
    assert_eq!(g.title_id.as_deref(), Some("UCJS10041"));
}

#[test]
fn nro_title_and_version_from_the_nacp() {
    let nro_size = 0x1000usize;
    let nacp_off = 0x38usize;
    let mut b = vec![0u8; nro_size + nacp_off + 0x4000];
    put(&mut b, 0x10, b"NRO0");
    put(&mut b, 0x18, &le32(nro_size as u32));
    put(&mut b, nro_size, b"ASET");
    put(&mut b, nro_size + 0x18, &(nacp_off as u64).to_le_bytes());
    put(&mut b, nro_size + 0x20, &0x4000u64.to_le_bytes());
    put(
        &mut b,
        nro_size + nacp_off,
        b"ftpd 11th Anniversary Edition",
    );
    put(&mut b, nro_size + nacp_off + 0x3060, b"3.2.1-2");
    let d = det(&b, "ftpd.nro");
    assert_eq!(d.label, "exe.switch.nro");
    let g = d.game.unwrap();
    assert_eq!(
        (g.title.as_deref(), g.version.as_deref()),
        (Some("ftpd 11th Anniversary Edition"), Some("3.2.1-2"))
    );
}

/// uLaunch's atmosphere/contents/0100000000001000/exefs.nsp: a plaintext ExeFS PFS0.
#[test]
fn switch_exefs_is_not_encrypted() {
    let names = b"main\0main.npdm\0\0\0";
    let n = 2usize;
    let header = 16 + n * 0x18 + names.len();
    let mut b = vec![0u8; header + 0x200];
    put(&mut b, 0, b"PFS0");
    put(&mut b, 4, &le32(n as u32));
    put(&mut b, 8, &le32(names.len() as u32));
    put(&mut b, 16, &0u64.to_le_bytes());
    put(&mut b, 24, &0x100u64.to_le_bytes());
    put(&mut b, 32, &le32(0));
    put(&mut b, 40, &0x100u64.to_le_bytes());
    put(&mut b, 48, &0x100u64.to_le_bytes());
    put(&mut b, 56, &le32(5));
    put(&mut b, 16 + n * 0x18, names);
    let d = det(&b, "exefs.nsp");
    let g = d.game.unwrap();
    assert_eq!((g.encrypted, g.format), (Some(false), "ExeFS"));
}

/// Swiss's NTSC-J disc: game code letter W (Taiwan) but bi2.bin region 0 (NTSC-J).
#[test]
fn gamecube_region_from_bi2() {
    let mut gc = vec![0u8; 0x2440];
    put(&mut gc, 0, b"GSWWGL");
    put(&mut gc, 0x1C, &be32(0xC233_9F3D));
    put(&mut gc, 0x20, b"SWISS FOR THE NINTENDO GAMECUBE");
    put(&mut gc, 0x458, &be32(0));
    let d = det(&gc, "swiss.iso");
    let g = d.game.unwrap();
    assert_eq!(
        (g.title_id.as_deref(), g.region),
        (Some("GSWWGL"), Some("Japan"))
    );
}

#[test]
fn listing_psp_memory_stick_folder() {
    let files = [
        ("PSP/GAME/CMFileManager/EBOOT.PBP", 6_277_186u64),
        ("PSP/GAME/CMFileManager/lang/en.txt", 1000),
    ];
    let l = listing::classify(files.iter().copied());
    let l = l.first().expect("a listing");
    assert_eq!(
        (l.label, l.platform, l.confidence, l.root.as_str()),
        (
            "game.psp.folder",
            Some(Platform::Psp),
            90,
            "PSP/GAME/CMFileManager"
        )
    );
}

#[test]
fn ps4_mini_app_category_is_an_app() {
    // Exercised through the loose PARAM.SFO of a PS4 title (category gde).
    let s = sfo(&[
        ("CATEGORY", "gde"),
        ("TITLE", "Store"),
        ("TITLE_ID", "CUSA00001"),
    ]);
    let d = det(&s, "param.sfo");
    assert_eq!(d.game.unwrap().content, Some(Content::App));
}

/// Checkpoint's CIA carries the homebrew product code CTR-HB-CKPT: no region, no GameTDB key.
/// Built here as a CCI, which reads the product code the same way (ncch_fill).
#[test]
fn n3ds_region_only_from_retail_product_codes() {
    let cci = |code: &[u8]| {
        let mut b = vec![0u8; 0x8000];
        put(&mut b, 0x100, b"NCSD");
        put(&mut b, 0x108, &0x0004_0000_0BCF_FF00u64.to_le_bytes());
        put(&mut b, 0x120, &le32(0x20));
        put(&mut b, 0x4100, b"NCCH");
        put(&mut b, 0x4150, code);
        b
    };
    let g = det(&cci(b"CTR-HB-CKPT"), "a.3ds").game.unwrap();
    assert_eq!((g.region, g.db_id), (None, None));
    let g = det(&cci(b"CTR-P-AREE"), "a.3ds").game.unwrap();
    assert_eq!(
        (g.region, g.db_id.as_deref()),
        (Some("Americas"), Some("AREE"))
    );
}

/// Review: "NRO0" plus a size in range is too little for 97. Random bytes with the magic and
/// a non-zero version word must not be called an NRO at all.
#[test]
fn nro_magic_on_random_bytes_is_not_an_nro() {
    let mut b: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    put(&mut b, 0x10, b"NRO0");
    put(&mut b, 0x14, &le32(0xDEAD_BEEF));
    put(&mut b, 0x18, &le32(1000));
    let f = SparseFile::from_bytes(b).with_name("x.bin");
    if let sniff::Outcome::Detected(d) = sniff::Detector::new().finish(&f) {
        assert!(
            !(d.label == "exe.switch.nro" && d.confidence >= 80),
            "{} {}",
            d.label,
            d.confidence
        );
    }
}
