//! Console formats from synthetic headers built from the documented layouts (UNVERIFIED
//! against real dumps, except the PS5 path: tests/ps5_samples.rs).

mod support;

use sniff::{Detector, GameKind, Outcome, Platform, Range, SparseFile};
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

/// A CNT block of 64 KiB: content ID, a name table (0x200) and one data entry.
fn cnt_block(content_id: &str, entry_id: u32, data: &[u8], names: &[u8]) -> Vec<u8> {
    let mut c = vec![0u8; 0x10000];
    put(&mut c, 0, b"\x7FCNT");
    put(&mut c, 0x10, &2u32.to_be_bytes());
    put(&mut c, 0x18, &0x2000u32.to_be_bytes());
    put(&mut c, 0x20, &0x2000u64.to_be_bytes());
    put(&mut c, 0x28, &0x8000u64.to_be_bytes());
    put(&mut c, 0x40, content_id.as_bytes());
    put(&mut c, 0x70, &0u32.to_be_bytes());
    put(&mut c, 0x74, &0x1Au32.to_be_bytes());
    let entry = |c: &mut Vec<u8>, i: usize, id: u32, name: u32, off: u32, size: u32| {
        let e = 0x2000 + i * 32;
        put(c, e, &id.to_be_bytes());
        put(c, e + 4, &name.to_be_bytes());
        put(c, e + 16, &off.to_be_bytes());
        put(c, e + 20, &size.to_be_bytes());
    };
    entry(&mut c, 0, 0x200, 0, 0x3000, names.len() as u32);
    entry(&mut c, 1, entry_id, 1, 0x4000, data.len() as u32);
    put(&mut c, 0x3000, names);
    put(&mut c, 0x4000, data);
    c
}

const PARAM_JSON: &str = r#"{"contentId":"EP9999-PPSA99999_00-TESTGAME00000001","titleId":"PPSA99999","contentVersion":"01.002.000","requiredSystemSoftwareVersion":"0x0900000000000000","localizedParameters":{"defaultLanguage":"en-US","en-US":{"titleName":"Test Game"},"fr-FR":{"titleName":"Jeu"}}}"#;

/// A PS5 FIH package: header, PFS area (zeros), CNT block at the PFS end.
fn ps5(len_pfs: u64) -> (Vec<u8>, u64) {
    let cnt = 0x10000 + len_pfs;
    let mut h = vec![0u8; 0x100];
    put(&mut h, 0, b"\x7FFIH\x01\x00");
    put(&mut h, 6, &3u16.to_le_bytes());
    put(&mut h, 0x10, &0x10000u64.to_le_bytes());
    put(&mut h, 0x18, &len_pfs.to_le_bytes());
    put(&mut h, 0x20, &0x10000u64.to_le_bytes());
    put(&mut h, 0x28, &0x10000u64.to_le_bytes());
    put(&mut h, 0x60, &0x10000u64.to_le_bytes());
    put(&mut h, 0x58, &cnt.to_le_bytes());
    for at in [0x30, 0x70, 0xD0] {
        put(&mut h, at, &[0xAB; 32]);
    }
    let block = cnt_block(
        "EP9999-PPSA99999_00-TESTGAME00000001",
        0x2000,
        PARAM_JSON.as_bytes(),
        b"\0param.json\0",
    );
    let mut file = h;
    file.resize(cnt as usize, 0);
    file.extend(block);
    (file, cnt)
}

#[test]
fn ps5_fih_asks_for_the_cnt_block_then_fills_the_title() {
    let (file, cnt) = ps5(0x100000);
    let mut f = SparseFile::new(file.len() as u64).with_name("game.pkg");
    f.insert(0, file[..0x100].to_vec());
    let Outcome::NeedMore(r) = Detector::new().detect(&f) else {
        panic!()
    };
    assert_eq!(r, vec![Range::new(cnt, 0x10000)]);
    f.insert(cnt, file[cnt as usize..].to_vec());
    let Outcome::Detected(d) = Detector::new().detect(&f) else {
        panic!()
    };
    assert_eq!((d.label.as_str(), d.confidence), ("game.ps5.pkg", 99));
    let g = d.game.unwrap();
    assert_eq!(
        (g.platform, g.kind, g.format, g.debug),
        (Platform::Ps5, GameKind::Package, "PS5 FIH", Some(true))
    );
    assert_eq!(
        g.content_id.as_deref(),
        Some("EP9999-PPSA99999_00-TESTGAME00000001")
    );
    assert_eq!(g.title_id.as_deref(), Some("PPSA99999"));
    assert_eq!(g.title.as_deref(), Some("Test Game"));
    assert_eq!(g.version.as_deref(), Some("01.002.000"));
    assert_eq!(g.required_fw.as_deref(), Some("9.00"));
    assert_eq!(g.region, Some("Europe/PAL"));
    assert_eq!(g.languages, Some(2));
}

#[test]
fn a_damaged_fih_keeps_the_label_at_low_confidence() {
    let (file, _) = ps5(0x100000);
    for (off, val) in [
        (0x18usize, 0xffu8),
        (0x5d, 0x7f),
        (0x05, 0x01),
        (0x12, 0x00),
    ] {
        let mut h = file[..0x100].to_vec();
        h[off] = val;
        let mut f = SparseFile::new(file.len() as u64);
        f.insert(0, h);
        let d = Detector::new().analyze(&f).detection;
        assert_eq!(d.label, "game.ps5.pkg");
        assert!(d.confidence < 80, "byte {off:#x}: {}", d.confidence);
    }
}

#[test]
fn ps4_cnt_with_param_sfo() {
    let s = sfo(&[
        ("TITLE", "Bloodborne"),
        ("TITLE_ID", "CUSA00207"),
        ("APP_VER", "01.09"),
        ("CATEGORY", "gd"),
    ]);
    let block = cnt_block(
        "EP9000-CUSA00207_00-BLOODBORNE0000EU",
        0x1000,
        &s,
        b"\0param.sfo\0",
    );
    let d = det(&block, "CUSA00207.pkg");
    assert_eq!((d.label.as_str(), d.confidence), ("game.ps4.pkg", 97));
    let g = d.game.clone().unwrap();
    assert_eq!(
        (g.platform, g.title.as_deref(), g.version.as_deref()),
        (Platform::Ps4, Some("Bloodborne"), Some("01.09"))
    );
    assert_eq!(d.field_str("category"), Some("gd"));
    // A bare CNT with a PS5 title ID is a PS5 package.
    let block = cnt_block("EP9000-PPSA00001_00-SOMETHING0000000", 0x1000, &s, b"\0x\0");
    assert_eq!(label(&block, "x.pkg"), "game.ps5.pkg");
}

fn ps3pkg(cid: &str, ty: u16, len: usize) -> Vec<u8> {
    let mut b = vec![0u8; len];
    put(&mut b, 0, b"\x7FPKG");
    put(&mut b, 4, &0x8000u16.to_be_bytes());
    put(&mut b, 6, &ty.to_be_bytes());
    put(&mut b, 0x18, &(len as u64).to_be_bytes());
    put(&mut b, 0x30, cid.as_bytes());
    b
}

#[test]
fn ps3_vita_psp_packages() {
    let d = det(
        &ps3pkg("UP0001-BLUS30443_00-DEMONSSOULS00000", 1, 0x400),
        "a.pkg",
    );
    assert_eq!((d.label.as_str(), d.confidence), ("game.ps3.pkg", 96));
    let g = d.game.unwrap();
    assert_eq!(
        (g.title_id.as_deref(), g.region, g.debug),
        (Some("BLUS30443"), Some("Americas"), Some(false))
    );
    assert_eq!(
        label(
            &ps3pkg("EP0001-PCSB00001_00-TESTGAME00000000", 2, 0x400),
            "a.pkg"
        ),
        "game.vita.pkg"
    );
    assert_eq!(
        label(
            &ps3pkg("EP0001-ULES00001_00-TESTGAME00000000", 2, 0x400),
            "a.pkg"
        ),
        "game.psp.pkg"
    );
    // Header size not matching the file: same label, not a fact.
    let mut b = ps3pkg("UP0001-BLUS30443_00-DEMONSSOULS00000", 1, 0x400);
    b.push(0);
    assert!(det(&b, "a.pkg").confidence <= 80);
}

#[test]
fn loose_param_sfo() {
    let s = sfo(&[
        ("TITLE", "Demon's Souls"),
        ("TITLE_ID", "BLUS30443"),
        ("PS3_SYSTEM_VER", "03.5000"),
    ]);
    let d = det(&s, "PARAM.SFO");
    assert_eq!((d.label.as_str(), d.confidence), ("game.ps.param_sfo", 95));
    let g = d.game.unwrap();
    assert_eq!(
        (g.platform, g.title.as_deref(), g.required_fw.as_deref()),
        (Platform::Ps3, Some("Demon's Souls"), Some("03.5000"))
    );
}

#[test]
fn switch_nsp_title_id_from_the_ticket_name() {
    let names = b"0123456789abcdef0123456789abcdef.nca\x000100abcd123400000000000000000005.tik\0";
    let n = 2;
    let mut b = vec![0u8; 0x10 + n * 0x18 + names.len()];
    put(&mut b, 0, b"PFS0");
    put(&mut b, 4, &(n as u32).to_le_bytes());
    put(&mut b, 8, &(names.len() as u32).to_le_bytes());
    put(&mut b, 0x18, &16u64.to_le_bytes());
    put(&mut b, 0x28, &16u64.to_le_bytes());
    put(&mut b, 0x30, &16u64.to_le_bytes());
    put(&mut b, 0x38, &37u32.to_le_bytes());
    put(&mut b, 0x10 + n * 0x18, names);
    b.extend_from_slice(&[0u8; 64]);
    let d = det(&b, "game.nsp");
    assert_eq!((d.label.as_str(), d.confidence), ("game.switch.nsp", 97));
    assert_eq!(
        d.game.unwrap().title_id.as_deref(),
        Some("0100ABCD12340000")
    );
}

#[test]
fn switch_xci_and_3ds() {
    let mut xci = vec![0u8; 0x10000];
    put(&mut xci, 0x100, b"HEAD");
    xci[0x10D] = 0xFA;
    put(&mut xci, 0x130, &0xF000u64.to_le_bytes());
    let d = det(&xci, "a.xci");
    assert_eq!((d.label.as_str(), d.confidence), ("game.switch.xci", 95));
    let mut cci = vec![0u8; 0x8000];
    put(&mut cci, 0x100, b"NCSD");
    put(&mut cci, 0x108, &0x0004_0000_0012_3400u64.to_le_bytes());
    put(&mut cci, 0x120, &0x20u32.to_le_bytes());
    put(&mut cci, 0x4100, b"NCCH");
    put(&mut cci, 0x4150, b"CTR-P-ABCE");
    let d = det(&cci, "a.3ds");
    assert_eq!(d.label, "game.3ds.cci");
    let g = d.game.unwrap();
    assert_eq!(
        (g.title_id.as_deref(), g.region),
        (Some("0004000000123400"), Some("Americas"))
    );
    let mut cia = vec![0u8; 0x3100];
    put(&mut cia, 0, &0x2020u32.to_le_bytes());
    put(&mut cia, 8, &0xA00u32.to_le_bytes());
    put(&mut cia, 12, &0x350u32.to_le_bytes());
    put(&mut cia, 16, &0x208u32.to_le_bytes());
    put(&mut cia, 24, &0x100u64.to_le_bytes());
    put(
        &mut cia,
        0x2A40 + 0x1DC,
        &0x0004_0000_0012_3400u64.to_be_bytes(),
    );
    let d = det(&cia, "a.cia");
    assert_eq!((d.label.as_str(), d.confidence), ("game.3ds.cia", 95));
    assert_eq!(
        d.game.unwrap().title_id.as_deref(),
        Some("0004000000123400")
    );
}

#[test]
fn wii_and_gamecube_discs() {
    let mut gc = vec![0u8; 0x440];
    put(&mut gc, 0, b"GALE01");
    put(&mut gc, 0x1C, &0xC233_9F3Du32.to_be_bytes());
    put(&mut gc, 0x20, b"Super Smash Bros Melee");
    let d = det(&gc, "gc.iso");
    assert_eq!(d.label, "game.gamecube.iso");
    let g = d.game.unwrap();
    assert_eq!(
        (g.title_id.as_deref(), g.title.as_deref(), g.region),
        (
            Some("GALE01"),
            Some("Super Smash Bros Melee"),
            Some("Americas")
        )
    );
    let mut rvz = vec![0u8; 0x100];
    put(&mut rvz, 0, b"RVZ\x01");
    put(&mut rvz, 0x58, b"RMCP01");
    put(&mut rvz, 0x58 + 0x18, &0x5D1C_9EA3u32.to_be_bytes());
    put(&mut rvz, 0x78, b"Mario Kart Wii");
    let d = det(&rvz, "a.rvz");
    let g = d.game.unwrap();
    assert_eq!(
        (
            d.label.as_str(),
            g.platform,
            g.format,
            g.title.as_deref(),
            g.region
        ),
        (
            "game.rvz",
            Platform::Wii,
            "RVZ",
            Some("Mario Kart Wii"),
            Some("Europe/PAL")
        )
    );
    let mut wbfs = vec![0u8; 0x400];
    put(&mut wbfs, 0, b"WBFS");
    wbfs[8] = 9;
    put(&mut wbfs, 0x200, b"RSBE01");
    put(&mut wbfs, 0x218, &0x5D1C_9EA3u32.to_be_bytes());
    put(&mut wbfs, 0x220, b"Brawl");
    let d = det(&wbfs, "a.wbfs");
    assert_eq!(
        (d.label.as_str(), d.game.unwrap().title.as_deref()),
        ("game.wii.wbfs", Some("Brawl"))
    );
    let mut gcz = vec![0u8; 64];
    put(&mut gcz, 0, &0xB10B_C001u32.to_le_bytes());
    put(&mut gcz, 4, &1u32.to_le_bytes());
    put(&mut gcz, 8, &64u64.to_le_bytes());
    let d = det(&gcz, "a.gcz");
    assert_eq!(
        (d.label.as_str(), d.game.unwrap().platform),
        ("game.gcz", Platform::Wii)
    );
}

#[test]
fn psp_cso() {
    let mut cso = b"CISO".to_vec();
    cso.extend_from_slice(&0x18u32.to_le_bytes());
    cso.extend_from_slice(&0x100000u64.to_le_bytes());
    cso.extend_from_slice(&2048u32.to_le_bytes());
    cso.extend_from_slice(&[1, 0, 0, 0]);
    cso.resize(1024, 0);
    assert_eq!(
        (
            det(&cso, "a.cso").label.as_str(),
            det(&cso, "a.cso").confidence
        ),
        ("game.psp.cso", 93)
    );
}

#[test]
fn xbox_formats() {
    let mut stfs = vec![0u8; 0x1800];
    put(&mut stfs, 0, b"LIVE");
    put(&mut stfs, 0x344, &0x7000u32.to_be_bytes());
    put(&mut stfs, 0x360, &0x4156_07D1u32.to_be_bytes());
    let t: Vec<u8> = "Halo".encode_utf16().flat_map(u16::to_be_bytes).collect();
    put(&mut stfs, 0x1691, &t);
    let d = det(&stfs, "x");
    assert_eq!((d.label.as_str(), d.confidence), ("game.x360.stfs", 95));
    let g = d.game.unwrap();
    assert_eq!(
        (g.title_id.as_deref(), g.title.as_deref()),
        (Some("415607D1"), Some("Halo"))
    );
    let mut xvd = vec![0u8; 0x3000];
    put(&mut xvd, 0x200, b"msft-xvd");
    assert_eq!(det(&xvd, "a.xvd").game.unwrap().platform, Platform::XboxOne);
    let mut xbe = vec![0u8; 0x400];
    put(&mut xbe, 0, b"XBEH");
    put(&mut xbe, 0x104, &0x10000u32.to_le_bytes());
    put(&mut xbe, 0x118, &0x10200u32.to_le_bytes());
    put(&mut xbe, 0x208, &0x4D53_0004u32.to_le_bytes());
    let t: Vec<u8> = "Halo".encode_utf16().flat_map(u16::to_le_bytes).collect();
    put(&mut xbe, 0x20C, &t);
    let d = det(&xbe, "default.xbe");
    assert_eq!(
        (d.label.as_str(), d.game.unwrap().title.as_deref()),
        ("game.xbox.xbe", Some("Halo"))
    );
    let mut xiso = vec![0u8; 0x10800];
    put(&mut xiso, 0x10000, b"MICROSOFT*XBOX*MEDIA");
    assert_eq!(label(&xiso, "a.iso"), "game.xbox.xiso");
    // Head only, all zeros: ask for the deeper fixed offsets.
    let f = sparse(&xiso, 0x8000, "a.iso");
    assert!(
        matches!(Detector::new().detect(&f), Outcome::NeedMore(r) if r.iter().any(|r| r.offset <= 0x10000 && r.end() >= 0x10014))
    );
}

#[test]
fn cartridges() {
    let mut nes = b"NES\x1a\x02\x01".to_vec();
    nes.resize(0x4010, 0);
    assert_eq!(det(&nes, "a.nes").game.unwrap().platform, Platform::Nes);
    let mut gba = vec![0u8; 0x200];
    put(&mut gba, 4, &[0x24, 0xFF, 0xAE, 0x51]);
    put(&mut gba, 0xA0, b"POKEMON EMER");
    put(&mut gba, 0xAC, b"BPEE");
    gba[0xB2] = 0x96;
    let d = det(&gba, "a.gba");
    let g = d.game.unwrap();
    assert_eq!(
        (
            d.label.as_str(),
            g.title.as_deref(),
            g.title_id.as_deref(),
            g.region
        ),
        (
            "game.gba.rom",
            Some("POKEMON EMER"),
            Some("BPEE"),
            Some("Americas")
        )
    );
    let mut nds = vec![0u8; 0x200];
    put(&mut nds, 0, b"MARIOKARTDS");
    put(&mut nds, 0xC, b"AMCP");
    put(&mut nds, 0xC0, &[0x24, 0xFF, 0xAE, 0x51, 0x69, 0x9A]);
    assert_eq!(det(&nds, "a.nds").game.unwrap().region, Some("Europe/PAL"));
    assert_eq!(
        label(b"\x80\x37\x12\x40\0\0\0\x0f", "a.z64"),
        "game.n64.rom"
    );
    let mut gb = vec![0u8; 0x150];
    put(&mut gb, 0x104, &[0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D]);
    assert_eq!(label(&gb, "a.gb"), "game.gb.rom");
    let mut ncz = vec![0u8; 0x4100];
    put(&mut ncz, 0x4000, b"NCZSECTN");
    assert_eq!(label(&ncz, "a.ncz"), "game.switch.ncz");
    let mut pbp = b"\0PBP\0\0\x01\0".to_vec();
    pbp.extend_from_slice(&40u32.to_le_bytes());
    pbp.resize(64, 0);
    assert_eq!(label(&pbp, "EBOOT.PBP"), "game.psp.pbp");
}

#[test]
fn random_bytes_with_console_magics_are_never_facts() {
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let buf: Vec<u8> = (0..0x20000)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s as u8
        })
        .collect();
    for magic in [
        &b"\x7fFIH"[..],
        b"\x7fCNT",
        b"\x7fPKG",
        b"PFS0",
        b"WBFS",
        b"RVZ\x01",
        b"CISO",
        b"CON ",
        b"XBEH",
        b"\0PSF",
    ] {
        let mut b = buf.clone();
        b[..magic.len()].copy_from_slice(magic);
        let mut f = SparseFile::new(1 << 34);
        f.insert(0, b);
        let d = Detector::new().analyze(&f).detection;
        assert!(
            !d.label.starts_with("game.") || d.confidence < 80,
            "{magic:?}: {} {}",
            d.label,
            d.confidence
        );
    }
}

/// Review: two equally sound CNT blocks must score the same; a recognised PS5 title id used
/// to stay at 85 while an unknown id got 95.
#[test]
fn sound_cnt_scores_95_for_ps5_and_unknown_ids_alike() {
    let ps5 = det(
        &cnt_block(
            "EP9999-PPSA01234_00-TESTGAME00000001",
            0x1000,
            b"x",
            b"\0a\0",
        ),
        "a.pkg",
    );
    let other = det(
        &cnt_block(
            "EP9999-RMTC00001_00-TESTGAME00000001",
            0x1000,
            b"x",
            b"\0a\0",
        ),
        "a.pkg",
    );
    assert_eq!(ps5.label, "game.ps5.pkg");
    assert!(ps5.confidence >= 95, "ps5 {}", ps5.confidence);
    assert!(other.confidence >= 95, "other {}", other.confidence);
}
