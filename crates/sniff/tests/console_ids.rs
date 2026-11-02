//! Console ids: every parser fills the same schema (platform, title_id, content_id, db_id,
//! region, version, content kind) and the database ids have the GameTDB formats. Fixtures are
//! synthetic headers built from the documented layouts (UNVERIFIED against real dumps; the PS5
//! path is checked on real packages in console_ps5_real.rs).

mod support;

use std::io::{Read, Write};

use sniff::{
    Caps, Content, Detector, GameInfo, GameKind, Outcome, Platform, Range, SparseFile, db_id_valid,
    detect_with,
};
use support::fixtures::{det, put};

fn game(bytes: &[u8], name: &str) -> (String, u8, GameInfo) {
    let d = det(bytes, name);
    let g = d
        .game
        .clone()
        .unwrap_or_else(|| panic!("{name}: no game record ({})", d.label));
    (d.label, d.confidence, g)
}

/// Runs the fetch driver with the default caps against `bytes`.
fn drive(bytes: &[u8], name: &str) -> sniff::Report {
    detect_with(
        |r: Range| -> Result<Vec<u8>, ()> {
            let a = r.offset as usize;
            let e = (a + r.len as usize).min(bytes.len());
            Ok(bytes[a.min(e)..e].to_vec())
        },
        bytes.len() as u64,
        Some(name),
        &Caps::default(),
    )
    .unwrap()
}

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

fn cnt_block(content_id: &str, entry_id: u32, data: &[u8], ct: u32) -> Vec<u8> {
    let names = b"\0param\0";
    let mut c = vec![0u8; 0x10000];
    put(&mut c, 0, b"\x7FCNT");
    put(&mut c, 0x10, &2u32.to_be_bytes());
    put(&mut c, 0x18, &0x2000u32.to_be_bytes());
    put(&mut c, 0x20, &0x2000u64.to_be_bytes());
    put(&mut c, 0x28, &0x8000u64.to_be_bytes());
    put(&mut c, 0x40, content_id.as_bytes());
    put(&mut c, 0x74, &ct.to_be_bytes());
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

// ---------------------------------------------------------------- Sony

#[test]
fn ps4_patch_and_dlc_from_the_sfo_category() {
    for (cat, ct, want) in [
        ("gd", 0x1A, Content::Game),
        ("gp", 0x1A, Content::Patch),
        ("ac", 0x1B, Content::Dlc),
    ] {
        let s = sfo(&[
            ("TITLE", "Bloodborne"),
            ("TITLE_ID", "CUSA00207"),
            ("APP_VER", "01.09"),
            ("CATEGORY", cat),
        ]);
        let block = cnt_block("EP9000-CUSA00207_00-BLOODBORNE0000EU", 0x1000, &s, ct);
        let (label, _, g) = game(&block, "x.pkg");
        assert_eq!(label, "game.ps4.pkg");
        assert_eq!(g.content, Some(want), "{cat}");
        assert_eq!(g.db_id.as_deref(), Some("CUSA00207"));
        assert_eq!(g.region, Some("Europe/PAL"));
    }
}

const PARAM_JSON: &str = r#"{"applicationCategoryType":0,"contentId":"UP9000-PPSA01284_00-RATCHETPS5000000","titleId":"PPSA01284","contentVersion":"01.004.000","sdkVersion":"0x0300000000000000","requiredSystemSoftwareVersion":"0x0250000000000000","savedata":{"titleIdForTransferringPs4":["CUSA01073","bogus"]},"localizedParameters":{"defaultLanguage":"en-US","en-US":{"titleName":"Ratchet & Clank: Rift Apart"}}}"#;

#[test]
fn loose_param_json_from_a_folder_dump() {
    let (label, conf, g) = game(PARAM_JSON.as_bytes(), "sce_sys/param.json");
    assert_eq!((label.as_str(), conf), ("game.ps5.param_json", 96));
    assert_eq!(
        (
            g.platform,
            g.kind,
            g.title_id.as_deref(),
            g.db_id.as_deref()
        ),
        (
            Platform::Ps5,
            GameKind::Metadata,
            Some("PPSA01284"),
            Some("PPSA01284")
        )
    );
    assert_eq!(g.title.as_deref(), Some("Ratchet & Clank: Rift Apart"));
    assert_eq!(
        (
            g.version.as_deref(),
            g.sdk.as_deref(),
            g.required_fw.as_deref()
        ),
        (Some("01.004.000"), Some("3.00"), Some("2.50"))
    );
    assert_eq!(
        (g.content, g.region),
        (Some(Content::Game), Some("Americas"))
    );
    let d = det(PARAM_JSON.as_bytes(), "param.json");
    assert_eq!(
        d.field("ps4_title_ids"),
        Some(&sniff::Value::List(vec!["CUSA01073".into()]))
    );
    // A JSON file that is not a PS5 param.json stays JSON.
    let other = br#"{"titleId": "abc", "contentId": "x", "other": 1}"#;
    assert_ne!(det(other, "x.json").label, "game.ps5.param_json");
}

/// A PS3/Vita `\x7FPKG` with a metadata list (content type) and a plaintext PARAM.SFO copy.
fn pkg_with_metadata(cid: &str, ty: u16, content_type: u32, sfo_bytes: Option<&[u8]>) -> Vec<u8> {
    let len = 0x2000;
    let mut b = vec![0u8; len];
    put(&mut b, 0, b"\x7FPKG");
    put(&mut b, 4, &0x8000u16.to_be_bytes());
    put(&mut b, 6, &ty.to_be_bytes());
    put(&mut b, 8, &0xC0u32.to_be_bytes());
    put(&mut b, 12, &3u32.to_be_bytes());
    put(&mut b, 0x18, &(len as u64).to_be_bytes());
    put(&mut b, 0x30, cid.as_bytes());
    let mut md = Vec::new();
    md.extend_from_slice(&2u32.to_be_bytes());
    md.extend_from_slice(&4u32.to_be_bytes());
    md.extend_from_slice(&content_type.to_be_bytes());
    md.extend_from_slice(&5u32.to_be_bytes());
    md.extend_from_slice(&4u32.to_be_bytes());
    md.extend_from_slice(&[0x10, 0x61, 0x01, 0x00]);
    md.extend_from_slice(&0xEu32.to_be_bytes());
    md.extend_from_slice(&8u32.to_be_bytes());
    let sfo_at = 0x1000u32;
    let sfo_len = sfo_bytes.map_or(0, <[u8]>::len) as u32;
    md.extend_from_slice(&sfo_at.to_be_bytes());
    md.extend_from_slice(&sfo_len.to_be_bytes());
    put(&mut b, 0xC0, &md);
    if let Some(s) = sfo_bytes {
        put(&mut b, 0x1000, s);
    }
    b
}

#[test]
fn ps3_and_vita_packages_read_the_metadata_list() {
    let s = sfo(&[
        ("TITLE", "Gravity Rush"),
        ("TITLE_ID", "PCSB00204"),
        ("APP_VER", "01.01"),
        ("CATEGORY", "gd"),
    ]);
    let pkg = pkg_with_metadata("EP9000-PCSF00024_00-GRAVITYRUSH00000", 2, 0x15, Some(&s));
    let d = det(&pkg, "a.pkg");
    assert_eq!(d.label, "game.vita.pkg");
    let g = d.game.clone().unwrap();
    assert_eq!(
        (
            g.platform,
            g.title.as_deref(),
            g.version.as_deref(),
            g.content
        ),
        (
            Platform::PsVita,
            Some("Gravity Rush"),
            Some("01.01"),
            Some(Content::Game)
        )
    );
    assert_eq!(d.field_str("package_version"), Some("01.00"));
    // The SFO title ID wins over the content ID one (PCSB vs PCSF regional variants).
    assert_eq!(g.title_id.as_deref(), Some("PCSB00204"));
    let dlc = pkg_with_metadata("UP0001-BLUS30443_00-DEMONSSOULSDLC01", 1, 0x0B, None);
    let (label, _, g) = game(&dlc, "a.pkg");
    assert_eq!(
        (label.as_str(), g.platform, g.content, g.db_id.as_deref()),
        (
            "game.ps3.pkg",
            Platform::Ps3,
            Some(Content::License),
            Some("BLUS30443")
        )
    );
}

fn dir_record(name: &[u8], dir: bool, lba: u32, size: u32) -> Vec<u8> {
    let len = 33 + name.len() + (name.len() + 1) % 2;
    let mut r = vec![0u8; len];
    r[0] = len as u8;
    put(&mut r, 2, &lba.to_le_bytes());
    put(&mut r, 6, &lba.to_be_bytes());
    put(&mut r, 10, &size.to_le_bytes());
    put(&mut r, 14, &size.to_be_bytes());
    r[25] = if dir { 2 } else { 0 };
    r[32] = name.len() as u8;
    put(&mut r, 33, name);
    r
}

fn dir(entries: &[(&str, bool, u32, u32)], self_lba: u32) -> Vec<u8> {
    let mut d = dir_record(&[0], true, self_lba, 2048);
    d.extend(dir_record(&[1], true, 20, 2048));
    for (name, is_dir, lba, size) in entries {
        let n = if *is_dir {
            name.to_string()
        } else {
            format!("{name};1")
        };
        d.extend(dir_record(n.as_bytes(), *is_dir, *lba, *size));
    }
    d
}

/// A cooked ISO 9660 image of `sectors` sectors: PVD at 16, root directory at 20, `blobs` at
/// their sectors.
fn iso(
    system: &str,
    volume: &str,
    root: &[(&str, bool, u32, u32)],
    blobs: &[(u32, Vec<u8>)],
    sectors: usize,
) -> Vec<u8> {
    let mut img = vec![0u8; 2048 * sectors];
    let pvd = 16 * 2048;
    img[pvd] = 1;
    put(&mut img, pvd + 1, b"CD001\x01");
    put(&mut img, pvd + 8, format!("{system:<32}").as_bytes());
    put(&mut img, pvd + 40, format!("{volume:<32}").as_bytes());
    put(&mut img, pvd + 156, &dir_record(&[0], true, 20, 2048));
    img[17 * 2048] = 255;
    put(&mut img, 17 * 2048 + 1, b"CD001\x01");
    put(&mut img, 20 * 2048, &dir(root, 20));
    for (lba, data) in blobs {
        put(&mut img, *lba as usize * 2048, data);
    }
    img
}

fn psp_iso() -> Vec<u8> {
    let s = sfo(&[
        ("TITLE", "Lumines"),
        ("DISC_ID", "ULES00043"),
        ("DISC_VERSION", "1.01"),
        ("CATEGORY", "UG"),
        ("PSP_SYSTEM_VER", "1.50"),
    ]);
    iso(
        "PSP GAME",
        "LUMINES",
        &[
            ("PSP_GAME", true, 22, 2048),
            ("UMD_DATA.BIN", false, 23, 32),
        ],
        &[
            (22, dir(&[("PARAM.SFO", false, 24, s.len() as u32)], 22)),
            (23, b"ULES-00043|5F7E1F8C1D4B1A9F|0001|G".to_vec()),
            (24, s),
        ],
        64,
    )
}

#[test]
fn psp_iso_param_sfo_within_the_default_rounds() {
    let img = psp_iso();
    let r = drive(&img, "Lumines.iso");
    let Outcome::Detected(d) = &r.outcome else {
        panic!("{:?}", r.outcome)
    };
    assert!(r.rounds <= 3, "{} rounds", r.rounds);
    assert_eq!((d.label.as_str(), d.confidence), ("game.psp.iso", 98));
    let g = d.game.clone().unwrap();
    assert_eq!(
        (
            g.title.as_deref(),
            g.title_id.as_deref(),
            g.db_id.as_deref()
        ),
        (Some("Lumines"), Some("ULES00043"), Some("ULES00043"))
    );
    assert_eq!(
        (
            g.version.as_deref(),
            g.region,
            g.required_fw.as_deref(),
            g.content
        ),
        (
            Some("1.01"),
            Some("Europe/PAL"),
            Some("1.50"),
            Some(Content::Game)
        )
    );
}

/// PSP CSO v1 of `iso`: deflate blocks of 2048 bytes behind a le32 index.
fn cso(iso: &[u8]) -> Vec<u8> {
    let n = iso.len() / 2048;
    let mut data = Vec::new();
    let mut index = Vec::new();
    let base = 0x18 + (n + 1) * 4;
    for blk in iso.chunks(2048) {
        index.push((base + data.len()) as u32);
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        e.write_all(blk).unwrap();
        data.extend(e.finish().unwrap());
    }
    index.push((base + data.len()) as u32);
    let mut out = b"CISO".to_vec();
    out.extend_from_slice(&0x18u32.to_le_bytes());
    out.extend_from_slice(&(iso.len() as u64).to_le_bytes());
    out.extend_from_slice(&2048u32.to_le_bytes());
    out.extend_from_slice(&[1, 0, 0, 0]);
    for i in index {
        out.extend_from_slice(&i.to_le_bytes());
    }
    out.extend(data);
    out
}

#[test]
fn psp_cso_is_walked_through_its_deflate_blocks() {
    let c = cso(&psp_iso());
    let (label, conf, g) = game(&c, "Lumines.cso");
    assert_eq!((label.as_str(), conf), ("game.psp.cso", 98));
    assert_eq!(
        (g.kind, g.title.as_deref(), g.db_id.as_deref()),
        (
            GameKind::CompressedImage,
            Some("Lumines"),
            Some("ULES00043")
        )
    );
    // From a 512-byte head: the blocks of the volume descriptor area are asked for.
    let mut f = SparseFile::new(c.len() as u64);
    f.insert(0, c[..512].to_vec());
    let a = Detector::new().analyze(&f);
    assert_eq!(a.detection.label, "game.psp.cso");
    assert!(!a.need.is_empty());
    let r = drive(&c, "Lumines.cso");
    let g = r.outcome.detection().unwrap().game.clone().unwrap();
    assert_eq!(g.title.as_deref(), Some("Lumines"));
}

#[test]
fn ps3_disc_title_from_ps3_game_param_sfo() {
    let s = sfo(&[
        ("TITLE", "Red Dead Redemption"),
        ("TITLE_ID", "BLES00680"),
        ("VERSION", "01.00"),
        ("APP_VER", "01.00"),
        ("CATEGORY", "DG"),
        ("PS3_SYSTEM_VER", "03.2100"),
    ]);
    let mut img = iso(
        "PS3VOLUME",
        "PS3VOLUME",
        &[
            ("PS3_GAME", true, 22, 2048),
            ("PS3_DISC.SFB", false, 23, 64),
        ],
        &[
            (22, dir(&[("PARAM.SFO", false, 24, s.len() as u32)], 22)),
            (24, s),
        ],
        64,
    );
    put(&mut img, 0x800, b"PlayStation3\0\0\0\0BLES-00680");
    let r = drive(&img, "RDR.iso");
    let d = r.outcome.detection().unwrap();
    assert_eq!((d.label.as_str(), d.confidence), ("game.ps3.iso", 98));
    let g = d.game.clone().unwrap();
    assert_eq!(
        (g.title.as_deref(), g.db_id.as_deref(), g.region, g.content),
        (
            Some("Red Dead Redemption"),
            Some("BLES00680"),
            Some("Europe/PAL"),
            Some(Content::Game)
        )
    );
    assert_eq!(g.required_fw.as_deref(), Some("03.2100"));
}

#[test]
fn ps1_ps2_serial_region_and_version() {
    let cnf = b"BOOT2 = cdrom0:\\SLES_508.21;1\r\nVER = 1.01\r\nVMODE = PAL\r\n";
    let img = iso(
        "PLAYSTATION",
        "SOTC",
        &[("SYSTEM.CNF", false, 22, cnf.len() as u32)],
        &[(22, cnf.to_vec())],
        40,
    );
    let (label, conf, g) = game(&img, "sotc.iso");
    assert_eq!((label.as_str(), conf), ("game.ps2.iso", 98));
    assert_eq!(
        (
            g.platform,
            g.db_id.as_deref(),
            g.region,
            g.version.as_deref()
        ),
        (
            Platform::Ps2,
            Some("SLES50821"),
            Some("Europe/PAL"),
            Some("1.01")
        )
    );
    let cnf = b"BOOT = cdrom:\\SCUS_944.26;1\r\n";
    let img = iso(
        "PLAYSTATION",
        "",
        &[("SYSTEM.CNF", false, 22, cnf.len() as u32)],
        &[(22, cnf.to_vec())],
        40,
    );
    let (label, _, g) = game(&support::fixtures::raw_cd(&img), "game.bin");
    assert_eq!(label, "game.ps1.bin");
    assert_eq!(
        (g.platform, g.db_id.as_deref(), g.region),
        (Platform::Ps1, Some("SCUS94426"), Some("Americas"))
    );
}

// ---------------------------------------------------------------- Switch

fn pfs0(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut names = Vec::new();
    let mut offs = Vec::new();
    for (n, _) in files {
        offs.push(names.len() as u32);
        names.extend_from_slice(n.as_bytes());
        names.push(0);
    }
    while names.len() % 16 != 0 {
        names.push(0);
    }
    let n = files.len();
    let mut b = vec![0u8; 0x10 + n * 0x18];
    put(&mut b, 0, b"PFS0");
    put(&mut b, 4, &(n as u32).to_le_bytes());
    put(&mut b, 8, &(names.len() as u32).to_le_bytes());
    let mut data_off = 0u64;
    for (i, (_, data)) in files.iter().enumerate() {
        let e = 0x10 + i * 0x18;
        put(&mut b, e, &data_off.to_le_bytes());
        put(&mut b, e + 8, &(data.len() as u64).to_le_bytes());
        put(&mut b, e + 16, &offs[i].to_le_bytes());
        data_off += data.len() as u64;
    }
    b.extend(names);
    for (_, data) in files {
        b.extend_from_slice(data);
    }
    b
}

#[test]
fn switch_nsp_update_with_plaintext_cnmt_and_nacp() {
    let cnmt = b"<?xml version=\"1.0\"?><ContentMeta><Type>Patch</Type><Id>0x0100abcd12340800</Id><Version>65536</Version></ContentMeta>";
    let nacp = b"<?xml version=\"1.0\"?><Application><Title><Language>AmericanEnglish</Language><Name>Some Game</Name></Title><DisplayVersion>1.0.1</DisplayVersion></Application>";
    let nca = vec![0u8; 0x400];
    let b = pfs0(&[
        ("0123456789abcdef0123456789abcdef.nca", &nca),
        ("0123456789abcdef0123456789abcdef.cnmt.xml", cnmt),
        ("fedcba9876543210fedcba9876543210.nacp.xml", nacp),
        ("0100abcd123408000000000000000005.tik", &[0u8; 0x2C0]),
    ]);
    let (label, conf, g) = game(&b, "Some Game [0100ABCD12340800][v65536].nsp");
    assert_eq!((label.as_str(), conf), ("game.switch.nsp", 97));
    assert_eq!(
        (
            g.title_id.as_deref(),
            g.content,
            g.title.as_deref(),
            g.version.as_deref()
        ),
        (
            Some("0100ABCD12340800"),
            Some(Content::Patch),
            Some("Some Game"),
            Some("1.0.1")
        )
    );
    // No readable product code: no GameTDB id, and the NCAs are encrypted.
    assert_eq!((g.db_id, g.encrypted), (None, Some(true)));
    // NCA names are content IDs, never title IDs.
    let b = pfs0(&[("0123456789abcdef0123456789abcdef.nca", &nca)]);
    assert_eq!(game(&b, "x.nsp").2.title_id, None);
}

#[test]
fn switch_xcz_from_the_secure_partition() {
    let secure = {
        let names = b"0123456789abcdef0123456789abcdef.ncz\0\0\0\0\0\0\0\0\0\0\0\0";
        let mut s = vec![0u8; 0x10 + 0x40];
        put(&mut s, 0, b"HFS0");
        put(&mut s, 4, &1u32.to_le_bytes());
        put(&mut s, 8, &(names.len() as u32).to_le_bytes());
        put(&mut s, 0x18, &0x100u64.to_le_bytes());
        s.extend_from_slice(names);
        s
    };
    let names = b"secure\0\0\0\0\0\0\0\0\0\0";
    let mut root = vec![0u8; 0x10 + 0x40];
    put(&mut root, 0, b"HFS0");
    put(&mut root, 4, &1u32.to_le_bytes());
    put(&mut root, 8, &(names.len() as u32).to_le_bytes());
    put(
        &mut root,
        0x18,
        &(secure.len() as u64 + 0x100).to_le_bytes(),
    );
    root.extend_from_slice(names);
    let mut xci = vec![0u8; 0x20000];
    put(&mut xci, 0x100, b"HEAD");
    xci[0x10D] = 0xFA;
    put(&mut xci, 0x130, &0xF000u64.to_le_bytes());
    put(&mut xci, 0x138, &0x200u64.to_le_bytes());
    put(&mut xci, 0xF000, &root);
    put(&mut xci, 0xF000 + root.len(), &secure);
    let r = drive(&xci, "game.xcz");
    let d = r.outcome.detection().unwrap();
    assert_eq!(d.label, "game.switch.xcz");
    let g = d.game.clone().unwrap();
    assert_eq!(
        (g.kind, g.encrypted, g.title_id),
        (GameKind::CompressedImage, Some(true), None)
    );
    assert_eq!(
        d.field("partitions"),
        Some(&sniff::Value::List(vec!["secure".into()]))
    );
}

// ---------------------------------------------------------------- 3DS / DS / Wii / Wii U

#[test]
fn n3ds_cia_product_code_gives_the_gametdb_id() {
    let mut cia = vec![0u8; 0x3200];
    put(&mut cia, 0, &0x2020u32.to_le_bytes());
    put(&mut cia, 8, &0xA00u32.to_le_bytes());
    put(&mut cia, 12, &0x350u32.to_le_bytes());
    put(&mut cia, 16, &0x208u32.to_le_bytes());
    put(&mut cia, 24, &0x200u64.to_le_bytes());
    put(
        &mut cia,
        0x2A40 + 0x1DC,
        &0x0004_000E_0012_3400u64.to_be_bytes(),
    );
    put(&mut cia, 0x3100, b"NCCH");
    put(&mut cia, 0x3110, b"01");
    put(&mut cia, 0x3150, b"CTR-P-ABCE");
    cia[0x3000 + 0x18F] = 0x04;
    let (label, conf, g) = game(&cia, "update.cia");
    assert_eq!((label.as_str(), conf), ("game.3ds.cia", 95));
    assert_eq!(
        (
            g.title_id.as_deref(),
            g.db_id.as_deref(),
            g.product_code.as_deref()
        ),
        (Some("0004000E00123400"), Some("ABCE"), Some("CTR-P-ABCE"))
    );
    assert_eq!(
        (g.content, g.region, g.maker.as_deref(), g.encrypted),
        (
            Some(Content::Patch),
            Some("Americas"),
            Some("01"),
            Some(false)
        )
    );
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

#[test]
fn nds_header_crc_makes_it_a_fact() {
    let mut nds = vec![0u8; 0x200];
    put(&mut nds, 0, b"MARIOKARTDS");
    put(&mut nds, 0xC, b"AMCP");
    put(&mut nds, 0x10, b"01");
    nds[0x1E] = 1;
    put(&mut nds, 0xC0, &[0x24, 0xFF, 0xAE, 0x51, 0x69, 0x9A]);
    put(&mut nds, 0x15C, &0xCF56u16.to_le_bytes());
    let c = crc16(&nds[..0x15E]);
    put(&mut nds, 0x15E, &c.to_le_bytes());
    let (label, conf, g) = game(&nds, "mk.nds");
    assert_eq!((label.as_str(), conf), ("game.nds.rom", 98));
    assert_eq!(
        (
            g.db_id.as_deref(),
            g.maker.as_deref(),
            g.version.as_deref(),
            g.region
        ),
        (Some("AMCP"), Some("01"), Some("1"), Some("Europe/PAL"))
    );
    nds[0x15E] ^= 1;
    assert_eq!(game(&nds, "mk.nds").1, 95);
}

#[test]
fn wii_disc_and_wad_ids() {
    let mut wii = vec![0u8; 0x440];
    put(&mut wii, 0, b"RMCE01");
    wii[7] = 1;
    put(&mut wii, 0x18, &0x5D1C_9EA3u32.to_be_bytes());
    put(&mut wii, 0x20, b"Mario Kart Wii");
    let (_, _, g) = game(&wii, "mkw.iso");
    assert_eq!(
        (
            g.db_id.as_deref(),
            g.maker.as_deref(),
            g.version.as_deref(),
            g.content
        ),
        (
            Some("RMCE01"),
            Some("01"),
            Some("1.01"),
            Some(Content::Game)
        )
    );
    // WAD: header, certificates, ticket (title ID at +0x1DC), TMD (version at +0x1DC).
    let mut wad = vec![0u8; 0xF80];
    put(&mut wad, 0, &0x20u32.to_be_bytes());
    put(&mut wad, 4, b"Is\0\0");
    put(&mut wad, 8, &0xA00u32.to_be_bytes());
    put(&mut wad, 0x10, &0x2A4u32.to_be_bytes());
    put(&mut wad, 0x14, &0x208u32.to_be_bytes());
    put(&mut wad, 0x18, &0x40u32.to_be_bytes());
    put(
        &mut wad,
        0xA40 + 0x1DC,
        &0x0001_0001_5741_4145u64.to_be_bytes(),
    );
    put(&mut wad, 0xD00 + 0x1DC, &2u16.to_be_bytes());
    let (label, conf, g) = game(&wad, "game.wad");
    assert_eq!((label.as_str(), conf), ("game.wii.wad", 96));
    assert_eq!(
        (
            g.title_id.as_deref(),
            g.db_id.as_deref(),
            g.version.as_deref(),
            g.region
        ),
        (
            Some("0001000157414145"),
            Some("WAAE"),
            Some("2"),
            Some("Americas")
        )
    );
}

#[test]
fn wii_u_wud_and_meta_xml() {
    let mut wud = vec![0u8; 0x8000];
    put(&mut wud, 0, b"WUP-P-AAFE\0\0\0\0\0\0");
    let (label, _, g) = game(&wud, "bayonetta.wud");
    assert_eq!(label, "game.wiiu.wud");
    assert_eq!(
        (
            g.product_code.as_deref(),
            g.db_id.as_deref(),
            g.region,
            g.encrypted
        ),
        (
            Some("WUP-P-AAFE"),
            Some("AAFE"),
            Some("Americas"),
            Some(true)
        )
    );
    let meta = br#"<?xml version="1.0" encoding="utf-8"?>
<menu type="complex" access="777">
  <title_id type="hexBinary" length="8">0005000010101D00</title_id>
  <title_version type="unsignedInt" length="4">0</title_version>
  <product_code type="string" length="32">WUP-P-AAFE</product_code>
  <company_code type="string" length="8">0001</company_code>
  <longname_en type="string" length="512">Bayonetta</longname_en>
</menu>"#;
    let (label, conf, g) = game(meta, "meta/meta.xml");
    assert_eq!((label.as_str(), conf), ("game.wiiu.meta", 95));
    assert_eq!(
        (
            g.title_id.as_deref(),
            g.db_id.as_deref(),
            g.title.as_deref(),
            g.content
        ),
        (
            Some("0005000010101D00"),
            Some("AAFE01"),
            Some("Bayonetta"),
            Some(Content::Game)
        )
    );
}

// ---------------------------------------------------------------- cartridges

#[test]
fn cartridge_header_checksums() {
    // Game Boy Color: logo, title, CGB flag, header checksum.
    let logo = [
        0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00,
        0x0D, 0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD,
        0xD9, 0x99, 0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB,
        0xB9, 0x33, 0x3E,
    ];
    let mut gb = vec![0u8; 0x8000];
    put(&mut gb, 0x104, &logo);
    put(&mut gb, 0x134, b"POKEMON_SLV");
    put(&mut gb, 0x13F, b"AAXE");
    gb[0x143] = 0x80;
    gb[0x14A] = 1;
    gb[0x14B] = 0x33;
    put(&mut gb, 0x144, b"01");
    let sum = gb[0x134..0x14D]
        .iter()
        .fold(0u8, |x, &b| x.wrapping_sub(b).wrapping_sub(1));
    gb[0x14D] = sum;
    let (label, conf, g) = game(&gb, "silver.gbc");
    assert_eq!((label.as_str(), conf), ("game.gbc.rom", 98));
    assert_eq!(
        (g.title.as_deref(), g.db_id.as_deref(), g.maker.as_deref()),
        (Some("POKEMON_SLV"), Some("AAXE"), Some("01"))
    );
    // GBA: complement byte.
    let mut gba = vec![0u8; 0x200];
    put(&mut gba, 4, &[0x24, 0xFF, 0xAE, 0x51]);
    put(&mut gba, 0xA0, b"POKEMON EMER");
    put(&mut gba, 0xAC, b"BPEE");
    put(&mut gba, 0xB0, b"01");
    gba[0xB2] = 0x96;
    let c = gba[0xA0..0xBD]
        .iter()
        .fold(0u8, |x, &b| x.wrapping_sub(b))
        .wrapping_sub(0x19);
    gba[0xBD] = c;
    let (_, conf, g) = game(&gba, "emerald.gba");
    assert_eq!(
        (conf, g.db_id.as_deref(), g.maker.as_deref()),
        (98, Some("BPEE"), Some("01"))
    );
    // N64 in v64 (byte-swapped) order.
    let mut z = vec![0u8; 0x1000];
    put(&mut z, 0, &[0x80, 0x37, 0x12, 0x40]);
    put(&mut z, 0x20, b"SUPER MARIO 64      ");
    put(&mut z, 0x3B, b"NSME");
    let v64: Vec<u8> = z.chunks(2).flat_map(|c| [c[1], c[0]]).collect();
    let d = det(&v64, "sm64.v64");
    let g = d.game.clone().unwrap();
    assert_eq!(
        (
            d.confidence,
            d.field_str("byte_order"),
            g.title.as_deref(),
            g.db_id.as_deref(),
            g.region
        ),
        (
            96,
            Some("v64"),
            Some("SUPER MARIO 64"),
            Some("NSME"),
            Some("Americas")
        )
    );
}

#[test]
fn snes_lorom_header_is_fetched_and_checked() {
    let mut rom = vec![0u8; 0x80000];
    for (i, b) in rom.iter_mut().enumerate().take(0x4000) {
        *b = (i * 7 + 3) as u8;
    }
    put(&mut rom, 0x7FB0, b"01MWE\0");
    put(&mut rom, 0x7FB2, b"SMWE");
    put(&mut rom, 0x7FC0, b"SUPER MARIOWORLD     ");
    put(
        &mut rom,
        0x7FD5,
        &[0x20, 0x02, 0x09, 0x01, 0x01, 0x33, 0x00],
    );
    put(&mut rom, 0x7FDC, &0x5F25u16.to_le_bytes());
    put(&mut rom, 0x7FDE, &0xA0DAu16.to_le_bytes());
    let r = drive(&rom, "smw.sfc");
    let d = r.outcome.detection().unwrap();
    assert_eq!((d.label.as_str(), d.confidence), ("game.snes.rom", 96));
    let g = d.game.clone().unwrap();
    assert_eq!(
        (
            g.title.as_deref(),
            g.db_id.as_deref(),
            g.maker.as_deref(),
            g.region
        ),
        (
            Some("SUPER MARIOWORLD"),
            Some("SMWE"),
            Some("01"),
            Some("Americas")
        )
    );
    // A broken checksum pair is not a fact.
    rom[0x7FDE] ^= 0xFF;
    assert!(det(&rom, "smw.sfc").confidence < 80);
}

// ---------------------------------------------------------------- Xbox

#[test]
fn xiso_walks_to_default_xbe() {
    let mut x = vec![0u8; 0x16000];
    put(&mut x, 0x10000, b"MICROSOFT*XBOX*MEDIA");
    put(&mut x, 0x10014, &33u32.to_le_bytes());
    put(&mut x, 0x10018, &0x800u32.to_le_bytes());
    put(&mut x, 0x107EC, b"MICROSOFT*XBOX*MEDIA");
    let mut e = vec![0u8; 14];
    put(&mut e, 4, &40u32.to_le_bytes());
    put(&mut e, 8, &0x1000u32.to_le_bytes());
    e[13] = 11;
    e.extend_from_slice(b"default.xbe");
    put(&mut x, 33 * 2048, &e);
    let xbe = 40 * 2048;
    put(&mut x, xbe, b"XBEH");
    put(&mut x, xbe + 0x104, &0x10000u32.to_le_bytes());
    put(&mut x, xbe + 0x118, &0x10200u32.to_le_bytes());
    put(&mut x, xbe + 0x208, &0x4D53_0004u32.to_le_bytes());
    let t: Vec<u8> = "Halo".encode_utf16().flat_map(u16::to_le_bytes).collect();
    put(&mut x, xbe + 0x20C, &t);
    put(&mut x, xbe + 0x2A0, &1u32.to_le_bytes());
    let r = drive(&x, "halo.iso");
    let d = r.outcome.detection().unwrap();
    assert_eq!(d.label, "game.xbox.xiso");
    let g = d.game.clone().unwrap();
    assert_eq!(
        (
            g.title.as_deref(),
            g.db_id.as_deref(),
            g.maker.as_deref(),
            g.region
        ),
        (Some("Halo"), Some("4D530004"), Some("MS"), Some("Americas"))
    );
}

#[test]
fn xex_execution_info() {
    let mut x = vec![0u8; 0x3000];
    put(&mut x, 0, b"XEX2");
    put(&mut x, 8, &0x2000u32.to_be_bytes());
    put(&mut x, 0x14, &1u32.to_be_bytes());
    put(&mut x, 0x18, &0x0004_0006u32.to_be_bytes());
    put(&mut x, 0x1C, &0x100u32.to_be_bytes());
    put(&mut x, 0x100, &0xABCD_0123u32.to_be_bytes());
    put(&mut x, 0x104, &0x2000_0400u32.to_be_bytes());
    put(&mut x, 0x10C, &0x4D53_07E6u32.to_be_bytes());
    put(&mut x, 0x112, &[1, 2]);
    let d = det(&x, "default.xex");
    let g = d.game.clone().unwrap();
    assert_eq!(
        (
            d.label.as_str(),
            d.confidence,
            g.db_id.as_deref(),
            g.version.as_deref()
        ),
        ("game.x360.xex", 96, Some("4D5307E6"), Some("2.0.4.0"))
    );
    assert_eq!(d.field_str("disc"), Some("1/2"));
}

// ---------------------------------------------------------------- id formats

/// The `<id>` values of a GameTDB XML inside its zip (one deflated member).
fn gametdb_ids(zip: &str) -> Option<Vec<String>> {
    let path = std::path::Path::new(&std::env::var("HOME").ok()?)
        .join("gametdb")
        .join(zip);
    let b = std::fs::read(path).ok()?;
    if b.get(..4)? != b"PK\x03\x04" {
        return None;
    }
    let method = u16::from_le_bytes([b[8], b[9]]);
    let csize = u32::from_le_bytes(b[18..22].try_into().ok()?) as usize;
    let n = u16::from_le_bytes([b[26], b[27]]) as usize;
    let x = u16::from_le_bytes([b[28], b[29]]) as usize;
    let data = b.get(30 + n + x..30 + n + x + csize)?;
    let mut xml = String::new();
    if method == 8 {
        flate2::read::DeflateDecoder::new(data)
            .read_to_string(&mut xml)
            .ok()?;
    } else {
        xml = String::from_utf8_lossy(data).into_owned();
    }
    Some(
        xml.split("<id>")
            .skip(1)
            .filter_map(|s| s.split("</id>").next())
            .map(str::to_owned)
            .collect(),
    )
}

#[test]
fn db_id_formats_match_the_gametdb_databases() {
    for (zip, platform) in [
        ("wiitdb.zip", Platform::Wii),
        ("wiiutdb.zip", Platform::WiiU),
        ("dstdb.zip", Platform::Nds),
        ("3dstdb.zip", Platform::N3ds),
        ("ps3tdb.zip", Platform::Ps3),
        ("switchtdb.zip", Platform::Switch),
    ] {
        let Some(ids) = gametdb_ids(zip) else {
            eprintln!("note: ~/gametdb/{zip} not readable; skipping");
            continue;
        };
        let ok = ids.iter().filter(|i| db_id_valid(platform, i)).count();
        let pct = 100.0 * ok as f64 / ids.len().max(1) as f64;
        eprintln!("{zip}: {ok}/{} ids accepted ({pct:.2}%)", ids.len());
        assert!(pct >= 97.0, "{zip}: {pct:.2}%");
    }
    // Every id the parsers above produce has its platform's format (spot checks).
    for (p, id) in [
        (Platform::Wii, "RMCE01"),
        (Platform::Wii, "WAAE"),
        (Platform::WiiU, "AAFE01"),
        (Platform::Nds, "AMCP"),
        (Platform::N3ds, "ABCE"),
        (Platform::Ps3, "BLES00680"),
        (Platform::Ps5, "PPSA12544"),
    ] {
        assert!(db_id_valid(p, id), "{p:?} {id}");
    }
}
