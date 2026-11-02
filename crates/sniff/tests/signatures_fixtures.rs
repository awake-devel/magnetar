//! Every built-in TOML rule has at least one positive fixture: a minimal header built by hand
//! from the format's documentation (so partly circular for the formats without a real sample
//! on this machine, as the fixtures elsewhere). A new rule without a fixture fails
//! `every_rule_has_a_positive_fixture`.

use sniff::{Detector, Signatures, SparseFile, label};

/// `len` zero bytes with `parts` written at their offsets (negative = from the end).
fn fx(len: usize, parts: &[(i64, &[u8])]) -> Vec<u8> {
    let mut b = vec![0u8; len];
    for (off, bytes) in parts {
        let at = if *off < 0 {
            len - off.unsigned_abs() as usize
        } else {
            *off as usize
        };
        b[at..at + bytes.len()].copy_from_slice(bytes);
    }
    b
}

fn h(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

const SYNC: &[u8] = b"\x00\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\x00";

#[allow(clippy::too_many_lines)]
fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    let le16 = |v: u16| v.to_le_bytes().to_vec();
    let le32 = |v: u32| v.to_le_bytes().to_vec();
    let be16 = |v: u16| v.to_be_bytes().to_vec();
    let be32 = |v: u32| v.to_be_bytes().to_vec();
    vec![
        // ---------------------------------------------------------------- disc
        ("qcow2", fx(512, &[(0, b"QFI\xfb\0\0\0\x03")])),
        ("vhdx", fx(512, &[(0, b"vhdxfile")])),
        ("vmdk", fx(512, &[(0, b"KDMV\x01\0\0\0")])),
        (
            "vdi",
            fx(
                512,
                &[
                    (0, b"<<< Oracle VM VirtualBox Disk Image >>>\n"),
                    (0x40, &le32(0xbeda_107f)),
                ],
            ),
        ),
        ("vhd_dynamic", fx(1024, &[(0, b"conectix")])),
        ("vhd_fixed", fx(4096, &[(-512, b"conectix")])),
        ("dmg", fx(4096, &[(-512, b"koly\0\0\0\x04")])),
        ("squashfs", fx(512, &[(0, b"hsqs")])),
        (
            "ext",
            fx(2048, &[(0x438, &le16(0xEF53)), (0x418, &le32(1))]),
        ),
        (
            "ntfs",
            fx(
                512,
                &[(0, b"\xEB\x52\x90"), (3, b"NTFS    "), (510, b"\x55\xAA")],
            ),
        ),
        (
            "exfat",
            fx(
                512,
                &[(0, b"\xEB\x76\x90"), (3, b"EXFAT   "), (510, b"\x55\xAA")],
            ),
        ),
        (
            "fat12_16",
            fx(
                512,
                &[
                    (0, b"\xEB\x3C\x90MSDOS5.0"),
                    (54, b"FAT16   "),
                    (510, b"\x55\xAA"),
                ],
            ),
        ),
        (
            "fat32",
            fx(
                512,
                &[
                    (0, b"\xEB\x58\x90MSDOS5.0"),
                    (82, b"FAT32   "),
                    (510, b"\x55\xAA"),
                ],
            ),
        ),
        ("mds", fx(256, &[(0, b"MEDIA DESCRIPTOR\x01\x05")])),
        ("nrg_v2", fx(4096, &[(-12, b"NER5")])),
        (
            "nrg_v1",
            fx(4096, &[(-8, b"NERO"), (-4, &be32(0x800)), (0x800, b"CUES")]),
        ),
        ("daa", fx(512, &[(0, b"DAA\0\0\0\0\0")])),
        ("apfs", fx(4096, &[(32, b"NXSB"), (36, &le32(4096))])),
        ("hfsplus", fx(4096, &[(0x400, b"H+\0\x04")])),
        ("btrfs", fx(0x11000, &[(0x10040, b"_BHRfS_M")])),
        ("xfs", fx(4096, &[(0, b"XFSB"), (4, &be32(4096))])),
        ("luks", fx(4096, &[(0, b"LUKS\xba\xbe"), (6, &be16(2))])),
        (
            "chd",
            fx(4096, &[(0, b"MComprHD"), (8, &be32(124)), (12, &be32(5))]),
        ),
        ("ecm", fx(4096, &[(0, b"ECM\0")])),
        ("isz", fx(4096, &[(0, b"IsZ!\x40\x01\x01")])),
        // ---------------------------------------------------------------- doc
        ("postscript", b"%!PS-Adobe-3.0\n%%Title: x\n".to_vec()),
        ("eps_dos", fx(64, &[(0, b"\xC5\xD0\xD3\xC6")])),
        ("djvu", fx(64, &[(0, b"AT&TFORM"), (12, b"DJVU")])),
        ("chm", fx(128, &[(0, b"ITSF"), (4, &le32(3))])),
        ("lit", fx(128, &[(0, b"ITOLITLS\x01\0\0\0")])),
        ("topaz", fx(128, &[(0, b"TPZ0")])),
        ("rtf", b"{\\rtf1\\ansi\\deff0 {\\fonttbl}}".to_vec()),
        // ---------------------------------------------------------------- font
        (
            "ttf",
            fx(256, &[(0, &h("00010000")), (4, &be16(10)), (12, b"cmap")]),
        ),
        (
            "otf",
            fx(256, &[(0, b"OTTO"), (4, &be16(10)), (12, b"CFF ")]),
        ),
        ("ttc", fx(256, &[(0, b"ttcf"), (4, &be32(0x10000))])),
        ("woff", fx(256, &[(0, b"wOFF\0\x01\0\0")])),
        ("woff2", fx(256, &[(0, b"wOF2\0\x01\0\0")])),
        (
            "type1",
            fx(256, &[(0, b"\x80\x01"), (6, b"%!PS-AdobeFont-1.0: X")]),
        ),
        // ---------------------------------------------------------------- image
        ("jpeg", fx(256, &[(0, b"\xFF\xD8\xFF\xE0\0\x10JFIF\0")])),
        (
            "gif",
            fx(256, &[(0, b"GIF89a"), (6, &le16(16)), (8, &le16(16))]),
        ),
        ("tiff", fx(256, &[(0, b"II*\0"), (4, &le32(8))])),
        ("bigtiff", fx(256, &[(0, b"II+\0\x08\0\0\0")])),
        ("psd", fx(256, &[(0, b"8BPS"), (4, &be16(1))])),
        ("xpm", b"/* XPM */\nstatic char *x[] = {\n".to_vec()),
        ("jxl", fx(256, &[(0, b"\xFF\x0A\xFA\x7F")])),
        ("jp2", fx(256, &[(0, &h("0000000C6A5020200D0A870A"))])),
        ("j2k", fx(256, &[(0, &h("FF4FFF51"))])),
        ("jp2_weak", fx(256, &[(0, &h("0000000C6A50FFFF"))])),
        ("qoi", fx(256, &[(0, b"qoif\0\0\0\x10\0\0\0\x10\x04\x00")])),
        ("exr", fx(256, &[(0, &h("762F3101"))])),
        ("dds", fx(256, &[(0, b"DDS "), (4, &le32(124))])),
        ("pnm", b"P6\n16 16\n255\n".to_vec()),
        ("raf", fx(256, &[(0, b"FUJIFILMCCD-RAW 0201")])),
        ("tga_footer", fx(1024, &[(-18, b"TRUEVISION-XFILE.\0")])),
        ("icns", fx(256, &[(0, b"icns"), (4, &be32(256))])),
        (
            "radiance_hdr",
            b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n".to_vec(),
        ),
        ("xcf", b"gimp xcf v011\0".to_vec()),
        ("ktx", fx(256, &[(0, &h("AB4B5458203230BB0D0A1A0A"))])),
        (
            "fits",
            fx(2880, &[(0, b"SIMPLE  =                    T / conforms")]),
        ),
        // ---------------------------------------------------------------- game
        (
            "psp_pbp",
            fx(64, &[(0, b"\0PBP\0\0\x01\0"), (8, &le32(40))]),
        ),
        ("nes", fx(0x4010, &[(0, b"NES\x1a\x02\x01")])),
        ("n64", fx(0x1000, &[(0, &h("80371240"))])),
        ("gb", fx(0x8000, &[(0x104, &h("CEED6666CC0D"))])),
        ("switch_ncz", fx(0x4100, &[(0x4000, b"NCZSECTN")])),
        ("3ds_cxi", fx(0x400, &[(0x100, b"NCCH")])),
        (
            "snes_lorom",
            fx(
                0x80000,
                &[
                    (0x7FC0, b"SUPER MARIOWORLD     "),
                    (0x7FD5, b"\x20\x02\x09\x01\x01\x01\x00"),
                ],
            ),
        ),
        (
            "snes_hirom",
            fx(
                0x80000,
                &[
                    (0xFFC0, b"SECRET OF MANA       "),
                    (0xFFD5, b"\x31\x02\x0B\x03\x01\x01\x00"),
                ],
            ),
        ),
        (
            "megadrive",
            fx(
                0x400,
                &[
                    (0x100, b"SEGA MEGA DRIVE (C)SEGA 1991.APR"),
                    (0x150, b"SONIC THE HEDGEHOG"),
                    (0x180, b"GM 00001009-00"),
                    (0x1F0, b"JUE"),
                ],
            ),
        ),
        (
            "megadrive_sega",
            fx(
                0x400,
                &[
                    (0x100, b"SEGA MEGAWIFI   (C)SGDK 2023"),
                    (0x150, b"SAMPLE PROGRAM"),
                    (0x180, b"GM 00000000-00"),
                    (0x1F0, b"JUE"),
                ],
            ),
        ),
        (
            "sega32x",
            fx(
                0x400,
                &[(0x100, b"SEGA 32X        "), (0x150, b"KNUCKLES CHAOTIX")],
            ),
        ),
        (
            "sms",
            fx(0x8000, &[(0x7FF0, b"TMR SEGA\0\0\x12\x34\x56\x70\x00\x4C")]),
        ),
        (
            "segacd",
            fx(
                0x800,
                &[
                    (0, b"SEGADISCSYSTEM  "),
                    (0x100, b"SEGA MEGA DRIVE "),
                    (0x150, b"SONIC CD"),
                ],
            ),
        ),
        (
            "segacd_raw",
            fx(0x930, &[(0, SYNC), (0x10, b"SEGADISCSYSTEM  ")]),
        ),
        (
            "saturn",
            fx(
                0x800,
                &[
                    (0, b"SEGA SEGASATURN SEGA ENTERPRISES"),
                    (0x20, b"MK-81009  V1.000"),
                    (0x40, b"JTUE            "),
                    (0x60, b"NIGHTS"),
                ],
            ),
        ),
        (
            "saturn_raw",
            fx(0x930, &[(0, SYNC), (0x10, b"SEGA SEGASATURN ")]),
        ),
        (
            "dreamcast",
            fx(
                0x800,
                &[
                    (0, b"SEGA SEGAKATANA SEGA ENTERPRISES"),
                    (0x40, b"MK-51000  V1.004"),
                    (0x80, b"SONIC ADVENTURE"),
                ],
            ),
        ),
        (
            "dreamcast_raw",
            fx(0x930, &[(0, SYNC), (0x10, b"SEGA SEGAKATANA ")]),
        ),
        (
            "lynx",
            fx(0x100, &[(0, b"LYNX\0\x02"), (0x0A, b"California Games\0")]),
        ),
        (
            "a7800",
            fx(0x100, &[(0, b"\x01ATARI7800"), (0x11, b"Asteroids\0")]),
        ),
        (
            "ngp",
            fx(
                0x100,
                &[
                    (0, b"COPYRIGHT BY SNK CORPORATION"),
                    (0x24, b"SONIC POCKET"),
                ],
            ),
        ),
        ("fds", fx(0x100, &[(0, b"FDS\x1a\x02")])),
        // ---------------------------------------------------------------- archive
        ("bzip2", b"BZh91AY&SY\x00\x00".to_vec()),
        ("lz4", fx(64, &[(0, &h("04224D18"))])),
        ("lz4_legacy", fx(64, &[(0, &h("02214C18"))])),
        ("lzip", fx(64, &[(0, b"LZIP\x01")])),
        ("compress", fx(64, &[(0, &h("1F9D90"))])),
        (
            "zstd_skippable",
            fx(64, &[(0, &le32(0x184D_2A50)), (4, &le32(4))]),
        ),
        ("cpio_ascii", fx(512, &[(0, b"070701000000")])),
        ("cpio_binary", fx(512, &[(0, &h("C771"))])),
        ("warc", b"WARC/1.0\r\nWARC-Type: warcinfo\r\n".to_vec()),
        ("ar", fx(256, &[(0, b"!<arch>\ndebian-binary   ")])),
        (
            "rpm",
            fx(256, &[(0, &h("EDABEEDB0300")), (10, b"hello-1.0")]),
        ),
        ("cab", fx(256, &[(0, b"MSCF"), (24, b"\x03\x01")])),
        ("ace", fx(256, &[(7, b"**ACE**")])),
        (
            "arj",
            fx(256, &[(0, &h("60EA")), (2, &le16(40)), (10, b"\x02")]),
        ),
        ("lzh", fx(256, &[(0, b"\x25\x9a"), (2, b"-lh5-")])),
        (
            "zpaq",
            fx(256, &[(0, b"7kSt\xa0\x31\x83\xd3\x8c\xb2\x28\xb0\xd3zPQ")]),
        ),
        ("zpaq_stream", fx(256, &[(0, b"zPQ\x01\x01")])),
        ("wim", fx(256, &[(0, b"MSWIM\0\0\0")])),
        ("lzop", fx(256, &[(0, &h("894C5A4F000D0A1A0A"))])),
        ("stuffit", fx(256, &[(0, b"SIT!"), (10, b"rLau")])),
        ("stuffit5", fx(256, &[(0, b"StuffIt (c)1997-2002 Aladdin")])),
        (
            "zoo",
            fx(
                256,
                &[(0, b"ZOO 2.10 Archive.\x1a"), (20, &le32(0xFDC4_A7DC))],
            ),
        ),
        // ---------------------------------------------------------------- media
        ("w64", fx(256, &[(0, b"riff\x2e\x91\xcf\x11")])),
        ("flac", fx(256, &[(0, b"fLaC\0\0\0\x22")])),
        ("dts", fx(256, &[(0, &h("7FFE8001"))])),
        ("dts_hd", fx(256, &[(0, b"DTSHDHDR")])),
        ("truehd", fx(256, &[(4, &h("F8726FBA"))])),
        ("mlp", fx(256, &[(4, &h("F8726FBB"))])),
        ("wavpack", fx(256, &[(0, b"wvpk")])),
        ("tta", fx(256, &[(0, b"TTA1")])),
        ("ape", fx(256, &[(0, b"MAC "), (4, &le16(3990))])),
        ("aiff", fx(256, &[(0, b"FORM"), (8, b"AIFF")])),
        ("caf", fx(256, &[(0, b"caff"), (4, &be16(1))])),
        ("au", fx(256, &[(0, b".snd"), (4, &be32(24))])),
        ("amr", b"#!AMR\n\x3c".to_vec()),
        ("voc", fx(256, &[(0, b"Creative Voice File\x1a")])),
        ("midi", fx(256, &[(0, b"MThd"), (4, &be32(6))])),
        ("optimfrog", fx(256, &[(0, b"OFR ")])),
        ("dsf", fx(256, &[(0, b"DSD ")])),
        ("dff", fx(256, &[(0, b"FRM8"), (12, b"DSD ")])),
        ("flv", fx(256, &[(0, b"FLV\x01\x05"), (5, &be32(9))])),
        ("y4m", b"YUV4MPEG2 W16 H16 F25:1\n".to_vec()),
        ("nut", fx(256, &[(0, b"nut/multimedia container\0")])),
        (
            "ivf",
            fx(256, &[(0, b"DKIF"), (6, &le16(32)), (8, b"VP80")]),
        ),
        ("realmedia", fx(256, &[(0, b".RMF")])),
        ("bink", fx(256, &[(0, b"BIKi")])),
        ("smacker", fx(256, &[(0, b"SMK2")])),
        ("mxf", fx(256, &[(0, &h("060E2B34020501010D01020101"))])),
        (
            "mxf_run_in",
            fx(1024, &[(100, &h("060E2B34020501010D01020101"))]),
        ),
        ("mod", fx(2048, &[(0, b"song"), (1080, b"M.K.")])),
        (
            "s3m",
            fx(256, &[(0, b"song"), (29, b"\x10"), (44, b"SCRM")]),
        ),
        ("xm", b"Extended Module: song\0".to_vec()),
        (
            "it",
            fx(
                256,
                &[
                    (0, b"IMPM"),
                    (0x28, &le16(0x0214)),
                    (0x2A, &le16(0x0200)),
                    (0x30, b"\x80"),
                ],
            ),
        ),
        ("musepack", fx(256, &[(0, b"MPCK")])),
        ("musepack_sv7", fx(256, &[(0, b"MP+\x07")])),
        ("nsf", fx(256, &[(0, b"NESM\x1a\x01")])),
        ("spc", fx(256, &[(0, b"SNES-SPC700 Sound File Data v0.30")])),
        ("vgm", fx(256, &[(0, b"Vgm "), (8, &le32(0x151))])),
        ("gbs", fx(256, &[(0, b"GBS\x01")])),
        ("psf", fx(256, &[(0, b"PSF\x01")])),
        ("sid", fx(256, &[(0, b"PSID"), (4, &be16(2))])),
        // ---------------------------------------------------------------- exe
        ("wasm", fx(64, &[(0, b"\0asm"), (4, &le32(1))])),
        (
            "lnk",
            fx(128, &[(0, &h("4C0000000114020000000000C000000000000046"))]),
        ),
        ("dex", fx(128, &[(0, b"dex\n035\0")])),
        (
            "pyc",
            fx(64, &[(0, &le16(3495)), (2, b"\r\n"), (4, &le32(0))]),
        ),
        ("pgp", b"-----BEGIN PGP MESSAGE-----\n".to_vec()),
        ("lua", fx(64, &[(0, b"\x1bLua\x54")])),
        ("psx_exe", fx(0x800, &[(0, b"PS-X EXE")])),
        (
            "ps3_self",
            fx(256, &[(0, b"SCE\0"), (4, &be32(2)), (10, &be16(1))]),
        ),
        (
            "vita_self",
            fx(
                256,
                &[
                    (0, b"SCE\0"),
                    (4, &le32(3)),
                    (10, &le16(1)),
                    (0x10, &le32(0x1000)),
                ],
            ),
        ),
        ("ps4_self", fx(256, &[(0, &h("4F153D1D00010112"))])),
        (
            "switch_nro",
            fx(256, &[(0x10, b"NRO0"), (0x18, &le32(256))]),
        ),
        ("switch_nso", fx(256, &[(0, b"NSO0")])),
        ("3dsx", fx(256, &[(0, b"3DSX"), (4, &le16(0x2C))])),
        ("wuhb", fx(256, &[(0, b"WUHB")])),
        // ---------------------------------------------------------------- misc
        ("sqlite", fx(512, &[(0, b"SQLite format 3\0")])),
        ("par2", fx(128, &[(0, b"PAR2\0PKT")])),
        (
            "torrent",
            b"d8:announce35:udp://tracker.example:1337/announce4:infod".to_vec(),
        ),
        ("kdbx", fx(128, &[(0, &h("03D9A29A67FB4BB5"))])),
        ("ds_store", fx(128, &[(0, &h("000000014275643100"))])),
        (
            "git_pack",
            fx(128, &[(0, b"PACK"), (4, &be32(2)), (8, &be32(10))]),
        ),
        ("blend", fx(128, &[(0, b"BLENDER-v279")])),
        ("fbx", fx(128, &[(0, b"Kaydara FBX Binary  \0\x1a\0")])),
        (
            "glb",
            fx(128, &[(0, b"glTF"), (4, &le32(2)), (8, &le32(128))]),
        ),
        (
            "unityfs",
            fx(128, &[(0, b"UnityFS\0"), (8, &be32(6)), (12, b"5.x.x\0")]),
        ),
        ("godot_pck", fx(128, &[(0, b"GDPC"), (4, &le32(1))])),
        (
            "valve_vpk",
            fx(128, &[(0, &le32(0x55AA_1234)), (4, &le32(2))]),
        ),
        (
            "quake_pak",
            fx(1024, &[(0, b"PACK"), (4, &le32(64)), (8, &le32(128))]),
        ),
        (
            "doom_wad",
            fx(1024, &[(0, b"IWAD"), (4, &le32(10)), (8, &le32(100))]),
        ),
        ("bethesda_bsa", fx(128, &[(0, b"BSA\0"), (4, &le32(0x68))])),
        (
            "bethesda_ba2",
            fx(128, &[(0, b"BTDX"), (4, &le32(1)), (8, b"GNRL")]),
        ),
        ("renpy_rpa", b"RPA-3.0 0000000000123456 42424242\n".to_vec()),
        ("rgss", fx(128, &[(0, b"RGSSAD\0\x01")])),
    ]
}

#[test]
fn every_rule_has_a_positive_fixture() {
    let sigs = Signatures::builtin();
    let fx = fixtures();
    let mut missing = Vec::new();
    for r in sigs.info() {
        if !fx.iter().any(|(id, _)| *id == r.id) {
            missing.push(r.id);
        }
    }
    assert!(missing.is_empty(), "rules without a fixture: {missing:?}");
    assert_eq!(fx.len(), sigs.len(), "fixtures for rules that do not exist");
}

#[test]
fn every_fixture_fires_its_rule() {
    let sigs = Signatures::builtin();
    let info = sigs.info();
    let det = Detector::new();
    let mut bad = Vec::new();
    for (id, bytes) in fixtures() {
        let rule = info
            .iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("{id}"));
        let a = det.analyze(&SparseFile::from_bytes(bytes));
        let hit = a.detection.candidates.iter().find(|c| c.source == id);
        match hit {
            Some(c) => {
                assert!(label::info(&c.label).is_some(), "{id}: {}", c.label);
                assert!(
                    c.confidence >= rule.confidence.saturating_sub(20),
                    "{id}: partial match {} (rule {})",
                    c.confidence,
                    rule.confidence
                );
            }
            None => bad.push(format!(
                "{id}: got {} ({:?})",
                a.detection.label,
                a.detection
                    .candidates
                    .iter()
                    .map(|c| c.source.as_str())
                    .collect::<Vec<_>>()
            )),
        }
    }
    assert!(
        bad.is_empty(),
        "fixtures that do not fire their rule:\n{}",
        bad.join("\n")
    );
}

#[test]
fn typed_fields_from_signature_rules_fill_the_game_record() {
    let (_, md) = fixtures()
        .into_iter()
        .find(|(id, _)| *id == "megadrive")
        .unwrap();
    let d = Detector::new()
        .analyze(&SparseFile::from_bytes(md))
        .detection;
    assert_eq!(d.label, "game.genesis.rom");
    let g = d.game.unwrap();
    assert_eq!(g.platform, sniff::Platform::Genesis);
    assert_eq!(g.title.as_deref(), Some("SONIC THE HEDGEHOG"));
    assert_eq!(g.product_code.as_deref(), Some("GM 00001009-00"));
    assert_eq!(g.title_id.as_deref(), Some("00001009-00"));
    assert_eq!(g.region, Some("World"));
    let (_, sat) = fixtures()
        .into_iter()
        .find(|(id, _)| *id == "saturn")
        .unwrap();
    let g = Detector::new()
        .analyze(&SparseFile::from_bytes(sat))
        .detection
        .game
        .unwrap();
    assert_eq!(
        (
            g.title_id.as_deref(),
            g.version.as_deref(),
            g.title.as_deref()
        ),
        (Some("MK-81009"), Some("V1.000"), Some("NIGHTS"))
    );
}
