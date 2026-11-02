//! One synthetic fixture (or a few) per format: media, images, fonts, documents, archives,
//! packages, executables, disc and disk images, text. Console formats: tests/console.rs.

mod support;

use sniff::{Detector, Outcome, Range, SparseFile, Value};
use support::fixtures::*;

// ---------------------------------------------------------------- media

#[test]
fn riff_forms() {
    let mut fmt = b"fmt ".to_vec();
    fmt.extend_from_slice(&16u32.to_le_bytes());
    fmt.extend_from_slice(&[1, 0, 2, 0, 0x44, 0xAC, 0, 0, 0x10, 0xB1, 2, 0, 4, 0, 16, 0]);
    let wav = riff(b"WAVE", &fmt);
    let d = det(&wav, "a.wav");
    assert_eq!((d.label.as_str(), d.confidence), ("audio.wav", 95));
    assert_eq!(d.field_str("format_tag"), Some("0x0001"));
    assert_eq!(label(&riff(b"AVI ", &[0; 64]), "a.avi"), "video.avi");
    assert_eq!(label(&riff(b"WEBP", &[0; 64]), "a.webp"), "image.webp");
    // Size field larger than the file: still the label, lower confidence.
    let mut bad = riff(b"WAVE", &fmt);
    bad[4..8].copy_from_slice(&1_000_000u32.to_le_bytes());
    assert_eq!(det(&bad, "a.wav").confidence, 80);
}

#[test]
fn ogg_first_codec() {
    let page = |payload: &[u8]| {
        let mut v = b"OggS\0".to_vec();
        v.extend_from_slice(&[0u8; 21]);
        v.push(1);
        v.push(payload.len() as u8);
        v.extend_from_slice(payload);
        v
    };
    assert_eq!(
        label(&page(b"\x01vorbis\0\0\0\0"), "a.ogg"),
        "audio.ogg.vorbis"
    );
    assert_eq!(
        label(&page(b"OpusHead\x01\x02"), "a.opus"),
        "audio.ogg.opus"
    );
    assert_eq!(label(&page(b"\x80theora\x03\x02"), "a.ogv"), "video.ogv");
}

#[test]
fn mpeg_audio_frame_chains() {
    // MPEG-1 layer III, 128 kbit/s, 44.1 kHz: 417-byte frames.
    let mut mp3 = Vec::new();
    for _ in 0..5 {
        let mut f = vec![0u8; 417];
        f[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        mp3.extend(f);
    }
    let d = det(&mp3, "a.mp3");
    assert_eq!(
        (d.label.as_str(), d.field_str("layer")),
        ("audio.mpeg", Some("3"))
    );
    let mut id3 = b"ID3\x04\0\0\0\0\0\x10".to_vec();
    id3.extend_from_slice(&[0; 16]);
    id3.extend_from_slice(&mp3[..417]);
    assert_eq!(label(&id3, "a.mp3"), "audio.mpeg");
    // ADTS: 100-byte frames.
    let mut aac = Vec::new();
    for _ in 0..4 {
        let len = 100usize;
        let mut f = vec![0u8; len];
        f[..7].copy_from_slice(&[
            0xFF,
            0xF1,
            0x50,
            0x80 | (len >> 11) as u8,
            (len >> 3) as u8,
            ((len & 7) << 5) as u8 | 0x1F,
            0xFC,
        ]);
        aac.extend(f);
    }
    assert_eq!(label(&aac, "a.aac"), "audio.aac");
    // AC-3 48 kHz, frmsizecod 0: 128-byte frames, bsid 8.
    let mut ac3 = Vec::new();
    for _ in 0..3 {
        let mut f = vec![0u8; 128];
        f[..6].copy_from_slice(&[0x0B, 0x77, 0, 0, 0x00, 8 << 3]);
        ac3.extend(f);
    }
    assert_eq!(label(&ac3, "a.ac3"), "audio.ac3");
}

#[test]
fn flac_signature_extracts_sample_rate() {
    let mut f = b"fLaC\x00\x00\x00\x22".to_vec();
    let mut si = vec![0u8; 34];
    si[10..14].copy_from_slice(&((44100u32 << 12) | 0x170).to_be_bytes());
    f.extend(si);
    let d = det(&f, "a.flac");
    assert_eq!(d.label, "audio.flac");
    assert_eq!(d.field("sample_rate"), Some(&Value::U64(44100)));
}

#[test]
fn transport_and_program_streams() {
    let mut ts = Vec::new();
    for _ in 0..20 {
        let mut p = vec![0xFFu8; 188];
        p[0] = 0x47;
        ts.extend(p);
    }
    assert_eq!(det(&ts, "a.ts").label, "video.ts");
    let mut m2ts = Vec::new();
    for _ in 0..20 {
        let mut p = vec![0xFFu8; 192];
        p[..4].copy_from_slice(&[0, 0, 0, 0]);
        p[4] = 0x47;
        m2ts.extend(p);
    }
    assert_eq!(det(&m2ts, "a.m2ts").label, "video.m2ts");
    // Starting mid-packet: still found, with the sync offset.
    let d = det(&ts[50..], "cut.ts");
    assert_eq!(d.label, "video.ts");
    let mut ps = vec![0, 0, 1, 0xBA, 0x44, 0, 4, 0, 4, 1, 1, 0x89, 0xC3, 0xF8];
    ps.extend_from_slice(&[0, 0, 1, 0xE0, 0, 8, 0x81, 0x80, 0, 0, 0, 0, 0, 0]);
    ps.extend_from_slice(&[0, 0, 1, 0xC0, 0, 8, 0x81, 0x80, 0, 0, 0, 0, 0, 0]);
    let d = det(&ps, "a.mpg");
    assert_eq!(
        (d.label.as_str(), d.field_str("mpeg")),
        ("video.mpeg_ps", Some("2"))
    );
}

#[test]
fn elementary_video_streams() {
    let mut h264 = vec![0, 0, 0, 1, 0x67, 0x64, 0, 0x28];
    h264.extend_from_slice(&[0, 0, 1, 0x68, 0xEE, 0, 0, 1, 0x65, 0x88, 0, 0, 1, 0x65]);
    assert_eq!(label(&h264, "a.h264"), "video.h264");
    let mut hevc = vec![0, 0, 0, 1, 0x40, 0x01, 0x0C];
    hevc.extend_from_slice(&[
        0, 0, 1, 0x42, 0x01, 0, 0, 1, 0x44, 0x01, 0, 0, 1, 0x26, 0x01,
    ]);
    assert_eq!(label(&hevc, "a.hevc"), "video.hevc");
}

#[test]
fn matroska_tracks_decide_video_audio_webm() {
    let d = det(
        &mkv(
            "matroska",
            &[(1, "V_MPEGH/ISO/HEVC"), (2, "A_EAC3"), (17, "S_TEXT/UTF8")],
        ),
        "a.mkv",
    );
    assert_eq!((d.label.as_str(), d.confidence), ("video.mkv", 97));
    assert_eq!(d.field_str("tracks"), Some("v1 a1 s1"));
    assert_eq!(
        label(&mkv("matroska", &[(2, "A_FLAC")]), "a.mka"),
        "audio.mka"
    );
    assert_eq!(label(&mkv("webm", &[(1, "V_VP9")]), "a.webm"), "video.webm");
}

#[test]
fn mp4_moov_at_end_is_asked_for() {
    let file = mp4(b"isom", &[b"vide", b"soun"], 100_000);
    let d = det(&file, "a.mp4");
    assert_eq!((d.label.as_str(), d.confidence), ("video.mp4", 97));
    assert_eq!(
        d.field("handlers"),
        Some(&Value::List(vec!["vide".into(), "soun".into()]))
    );
    assert_eq!(label(&mp4(b"M4A ", &[b"soun"], 1000), "a.m4a"), "audio.m4a");
    assert_eq!(label(&mp4(b"isom", &[b"soun"], 1000), "a.mp4"), "audio.m4a");
    assert_eq!(label(&mp4(b"qt  ", &[b"vide"], 1000), "a.mov"), "video.mov");
    // Head only: the walker asks for the box header after mdat.
    let f = sparse(&file, 16384, "a.mp4");
    match Detector::new().detect(&f) {
        Outcome::NeedMore(r) => assert_eq!(r.first().map(|r| r.offset), Some(100_032)),
        o => panic!("{o:?}"),
    }
}

// ---------------------------------------------------------------- images and fonts

#[test]
fn images() {
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend_from_slice(&640u32.to_be_bytes());
    png.extend_from_slice(&480u32.to_be_bytes());
    png.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
    let mut apng = png.clone();
    png.extend_from_slice(b"\0\0\0\0IDAT");
    apng.extend_from_slice(b"\0\0\0\x08acTL\0\0\0\x02\0\0\0\0\0\0\0\0IDAT");
    let d = det(&png, "a.png");
    assert_eq!(
        (d.label.as_str(), d.field("width")),
        ("image.png", Some(&Value::U64(640)))
    );
    assert_eq!(label(&apng, "a.png"), "image.apng");
    assert_eq!(label(b"\xFF\xD8\xFF\xE0\0\x10JFIF", "a.jpg"), "image.jpeg");
    let d = det(b"GIF89a\x40\x01\xF0\x00\x80", "a.gif");
    assert_eq!(
        (d.label.as_str(), d.field("width"), d.field("height")),
        ("image.gif", Some(&Value::U64(320)), Some(&Value::U64(240)))
    );
    let mut bmp = vec![0u8; 70];
    put(&mut bmp, 0, b"BM");
    put(&mut bmp, 2, &70u32.to_le_bytes());
    put(&mut bmp, 10, &54u32.to_le_bytes());
    put(&mut bmp, 14, &40u32.to_le_bytes());
    assert_eq!(det(&bmp, "a.bmp").confidence, 95);
    let mut ico = vec![0u8; 22 + 40];
    put(&mut ico, 0, &[0, 0, 1, 0, 1, 0, 16, 16]);
    put(&mut ico, 14, &40u32.to_le_bytes());
    put(&mut ico, 18, &22u32.to_le_bytes());
    assert_eq!(label(&ico, "a.ico"), "image.ico");
    assert_eq!(label(b"II*\0\x08\0\0\0\0\0\0\0", "a.tif"), "image.tiff");
    assert_eq!(label(b"II*\0\x10\0\0\0CR\x02\0", "a.cr2"), "image.raw.cr2");
    assert_eq!(label(b"8BPS\0\x01\0\0\0\0\0\0", "a.psd"), "image.psd");
    assert_eq!(label(b"P6\n640 480\n255\n", "a.ppm"), "image.pnm");
    assert_eq!(
        label(b"qoif\0\0\0\x10\0\0\0\x10\x04\x00", "a.qoi"),
        "image.qoi"
    );
    let mut tga = vec![0u8; 100];
    put(&mut tga, 82, b"TRUEVISION-XFILE.\0");
    assert_eq!(label(&tga, "a.tga"), "image.tga");
}

#[test]
fn fonts() {
    let mut ttf = vec![0u8; 64];
    put(&mut ttf, 0, &[0, 1, 0, 0, 0, 10]);
    put(&mut ttf, 12, b"OS/2");
    let d = det(&ttf, "a.ttf");
    assert_eq!(
        (d.label.as_str(), d.field("tables")),
        ("font.ttf", Some(&Value::U64(10)))
    );
    put(&mut ttf, 0, b"OTTO");
    assert_eq!(label(&ttf, "a.otf"), "font.otf");
    assert_eq!(label(b"wOF2\0\x01\0\0", "a.woff2"), "font.woff2");
    assert_eq!(label(b"ttcf\0\x01\0\0\0\0\0\x02", "a.ttc"), "font.ttc");
}

// ---------------------------------------------------------------- documents

#[test]
fn pdf_header_and_eof_marker() {
    let mut pdf = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n1 0 obj\n<<>>\nendobj\n".to_vec();
    let d = det(&pdf, "a.pdf");
    assert_eq!((d.label.as_str(), d.confidence), ("doc.pdf", 97));
    pdf.extend_from_slice(b"trailer\n%%EOF\n");
    let d = det(&pdf, "a.pdf");
    assert_eq!((d.confidence, d.field_str("version")), (99, Some("1.7")));
}

#[test]
fn palm_and_misc_documents() {
    let mut mobi = vec![0u8; 200];
    put(&mut mobi, 0, b"My Book");
    put(&mut mobi, 60, b"BOOKMOBI");
    assert_eq!(label(&mobi, "a.mobi"), "doc.mobi");
    assert_eq!(label(b"{\\rtf1\\ansi hello}", "a.rtf"), "doc.rtf");
    assert_eq!(label(b"AT&TFORM\0\0\0\x10DJVU", "a.djvu"), "doc.djvu");
    assert_eq!(label(b"%!PS-Adobe-3.0\n", "a.ps"), "doc.postscript");
}

// ---------------------------------------------------------------- ZIP family

#[test]
fn zip_container_types_from_the_file_list() {
    let epub = zip_store(&[
        ("mimetype", b"application/epub+zip"),
        ("META-INF/container.xml", b"<x/>"),
    ]);
    assert_eq!(det(&epub, "a.epub").confidence, 99);
    assert_eq!(label(&epub, "a.epub"), "doc.epub");
    let apk = zip_store(&[
        ("AndroidManifest.xml", b"x"),
        ("classes.dex", b"dex\n035\0"),
    ]);
    assert_eq!(label(&apk, "a.apk"), "package.apk");
    let docx = zip_store(&[("[Content_Types].xml", b"x"), ("word/document.xml", b"x")]);
    assert_eq!(label(&docx, "a.docx"), "doc.docx");
    let jar = zip_store(&[
        ("META-INF/MANIFEST.MF", b"x"),
        ("a/B.class", b"\xCA\xFE\xBA\xBE"),
    ]);
    assert_eq!(label(&jar, "a.jar"), "package.jar");
    let whl = zip_store(&[
        ("pkg/__init__.py", b""),
        ("pkg-1.0.dist-info/METADATA", b"x"),
    ]);
    assert_eq!(label(&whl, "a.whl"), "package.wheel");
    let cbz = zip_store(&[
        ("01.jpg", b"\xFF\xD8\xFF"),
        ("02.jpg", b"\xFF\xD8\xFF"),
        ("03.png", b"x"),
    ]);
    assert_eq!(label(&cbz, "a.cbz"), "doc.cbz");
    let xapk = zip_store(&[("manifest.json", b"{}"), ("base.apk", b"PK")]);
    assert_eq!(label(&xapk, "a.xapk"), "package.xapk");
    let ipa = zip_store(&[("Payload/App.app/Info.plist", b"x")]);
    assert_eq!(label(&ipa, "a.ipa"), "package.ipa");
    assert_eq!(
        det(&zip_store(&[("readme.txt", b"hi")]), "a.zip").label,
        "archive.zip"
    );
}

#[test]
fn zip_with_executables_is_suspicious_behind_a_media_name() {
    let z = zip_store(&[("Movie.mp4.exe", b"MZ")]);
    let d = det(&z, "Movie.zip");
    assert!(d.field("executables_inside").is_some());
    assert!(!d.flags.suspicious_executable);
    let d = det(&z, "Movie.mkv");
    assert!(d.flags.suspicious_executable && d.flags.extension_mismatch);
}

#[test]
fn zip_central_directory_is_asked_for_at_the_tail() {
    let mut big = zip_store(&[
        ("a.txt", &vec![b'a'; 200_000]),
        ("AndroidManifest.xml", b"x"),
        ("classes.dex", b"x"),
    ]);
    let f = sparse(&big, 16384, "x.apk");
    let Outcome::NeedMore(r) = Detector::new().detect(&f) else {
        panic!()
    };
    let last = *r.last().unwrap();
    assert_eq!(last.end(), big.len() as u64);
    // With the tail: APK from the central directory.
    let mut f = f;
    let t = big.split_off(big.len() - 70_000);
    f.insert((big.len()) as u64, t);
    assert_eq!(
        Detector::new()
            .detect(&f)
            .detection()
            .map(|d| d.label.clone())
            .as_deref(),
        Some("package.apk")
    );
}

// ---------------------------------------------------------------- other archives

fn rar5_block(fields: &[u8], data_len: usize) -> Vec<u8> {
    let mut h = vec![fields.len() as u8];
    h.extend_from_slice(fields);
    let mut v = crc32fast::hash(&h).to_le_bytes().to_vec();
    v.extend(h);
    v.extend(vec![0u8; data_len]);
    v
}

fn rar5(names: &[&str]) -> Vec<u8> {
    let mut v = b"Rar!\x1a\x07\x01\x00".to_vec();
    v.extend(rar5_block(&[1, 0, 0], 0));
    for n in names {
        let mut f = vec![2, 2, 3, 0, 3, 0x20, 0, 0, n.len() as u8];
        f.extend_from_slice(n.as_bytes());
        v.extend(rar5_block(&f, 3));
    }
    v.extend(rar5_block(&[5, 0, 0], 0));
    v
}

#[test]
fn rar_headers_and_crcs() {
    let d = det(&rar5(&["a.txt"]), "a.rar");
    assert_eq!((d.label.as_str(), d.confidence), ("archive.rar5", 98));
    assert_eq!(label(&rar5(&["01.jpg", "02.png"]), "a.cbr"), "doc.cbr");
    assert!(
        det(&rar5(&["codec.exe"]), "a.rar")
            .field("executables_inside")
            .is_some()
    );
    // RAR4: main header + one file header.
    let mut v = b"Rar!\x1a\x07\x00".to_vec();
    let mut main = vec![0x73, 0, 0, 13, 0, 0, 0, 0, 0, 0, 0];
    let c = (crc32fast::hash(&main) & 0xffff) as u16;
    v.extend_from_slice(&c.to_le_bytes());
    v.append(&mut main);
    let name = b"Movie.mkv";
    let mut fh = vec![0x74, 0x00, 0x80];
    fh.extend_from_slice(&((32 + name.len()) as u16).to_le_bytes());
    fh.extend_from_slice(&4u32.to_le_bytes());
    fh.extend_from_slice(&4u32.to_le_bytes());
    fh.extend_from_slice(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 29, 0x30]);
    fh.extend_from_slice(&(name.len() as u16).to_le_bytes());
    fh.extend_from_slice(&[0x20, 0, 0, 0]);
    fh.extend_from_slice(name);
    let c = (crc32fast::hash(&fh) & 0xffff) as u16;
    v.extend_from_slice(&c.to_le_bytes());
    v.extend(fh);
    v.extend_from_slice(b"data");
    let d = det(&v, "a.rar");
    assert_eq!((d.label.as_str(), d.confidence), ("archive.rar4", 98));
    assert_eq!(
        d.field("first_names"),
        Some(&Value::List(vec!["Movie.mkv".into()]))
    );
}

#[test]
fn sevenz_tar_and_compressors() {
    let mut z7 = vec![b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C, 0, 4, 0, 0, 0, 0];
    let mut sh = 0u64.to_le_bytes().to_vec();
    sh.extend_from_slice(&1u64.to_le_bytes());
    sh.extend_from_slice(&0u32.to_le_bytes());
    z7[8..12].copy_from_slice(&crc32fast::hash(&sh).to_le_bytes());
    z7.extend(sh);
    z7.push(0x01);
    let d = det(&z7, "a.7z");
    assert_eq!(
        (d.label.as_str(), d.confidence, d.field_str("header")),
        ("archive.7z", 99, Some("plain"))
    );
    let tar = tar_entry("hello.txt");
    assert_eq!(det(&tar, "a.tar").confidence, 99);
    assert_eq!(label(&tar_entry(".PKGINFO"), "a.tar"), "package.arch");
    let gz = gzip(&tar);
    let d = det(&gz, "a.tar.gz");
    assert_eq!(
        (d.label.as_str(), d.field_str("inner")),
        ("archive.gzip", Some("archive.tar"))
    );
    let zst = zstd(&tar_entry(".BUILDINFO"));
    assert_eq!(label(&zst, "pkg.tar.zst"), "package.arch");
    let mut xz = vec![0xFD, b'7', b'z', b'X', b'Z', 0, 0, 4];
    xz.extend_from_slice(&crc32fast::hash(&[0, 4]).to_le_bytes());
    assert_eq!(det(&xz, "a.xz").confidence, 99);
    assert_eq!(label(b"BZh91AY&SY\0\0\0\0", "a.bz2"), "archive.bzip2");
    assert_eq!(
        label(b"\x04\x22\x4D\x18\x64\x40\xA7", "a.lz4"),
        "archive.lz4"
    );
}

#[test]
fn nested_compression_is_bounded() {
    let mut x = tar_entry("a");
    for _ in 0..6 {
        x = gzip(&x);
    }
    let d = det(&x, "a.gz");
    assert_eq!(d.label, "archive.gzip");
}

#[test]
fn packages() {
    let mut cab = vec![0u8; 64];
    put(&mut cab, 0, b"MSCF");
    put(&mut cab, 8, &64u32.to_le_bytes());
    put(&mut cab, 24, &[3, 1]);
    assert_eq!(
        (
            det(&cab, "a.cab").label.as_str(),
            det(&cab, "a.cab").confidence
        ),
        ("archive.cab", 99)
    );
    put(&mut cab, 8, &65u32.to_le_bytes());
    assert_eq!(det(&cab, "a.cab").confidence, 92);
    let mut deb =
        b"!<arch>\ndebian-binary   1342943816  0     0     100644  4         `\n2.0\n".to_vec();
    deb.resize(200, 0);
    let d = det(&deb, "a.deb");
    assert_eq!(
        (d.label.as_str(), d.confidence, d.field_str("format")),
        ("package.deb", 99, Some("2.0"))
    );
    let mut ar = b"!<arch>\nfoo.o/          ".to_vec();
    ar.resize(100, b' ');
    assert_eq!(label(&ar, "a.a"), "archive.ar");
    let mut rpm = vec![0xED, 0xAB, 0xEE, 0xDB, 3, 0, 0, 0, 0, 1];
    rpm.extend_from_slice(b"pkg-1.0");
    rpm.resize(120, 0);
    rpm[79] = 5; // signature type
    let d = det(&rpm, "a.rpm");
    assert_eq!(
        (d.label.as_str(), d.confidence, d.field_str("name")),
        ("package.rpm", 98, Some("pkg-1.0"))
    );
    // xar with a Distribution file: macOS installer package.
    let toc = zlib(b"<xar><toc><file><name>Distribution</name></file></toc></xar>");
    let mut xar = b"xar!\0\x1c\0\x01".to_vec();
    xar.extend_from_slice(&(toc.len() as u64).to_be_bytes());
    xar.extend_from_slice(&100u64.to_be_bytes());
    xar.extend_from_slice(&1u32.to_be_bytes());
    xar.extend(toc);
    assert_eq!(label(&xar, "Installer.pkg"), "package.macos_pkg");
}

#[test]
fn ole_compound_files() {
    let mut cfb = vec![0u8; 1024];
    put(
        &mut cfb,
        0,
        &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1],
    );
    put(&mut cfb, 0x1E, &9u16.to_le_bytes());
    put(&mut cfb, 0x30, &0u32.to_le_bytes());
    let name: Vec<u8> = "Root Entry\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    put(&mut cfb, 512, &name);
    put(&mut cfb, 512 + 64, &(name.len() as u16).to_le_bytes());
    put(
        &mut cfb,
        512 + 0x50,
        &[
            0x84, 0x10, 0x0C, 0x00, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46,
        ],
    );
    assert_eq!(label(&cfb, "a.msi"), "package.msi");
    let wd: Vec<u8> = "WordDocument\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    put(&mut cfb, 512 + 0x50, &[0; 16]);
    put(&mut cfb, 640, &wd);
    put(&mut cfb, 640 + 64, &(wd.len() as u16).to_le_bytes());
    assert_eq!(label(&cfb, "a.doc"), "doc.doc");
}

// ---------------------------------------------------------------- executables

fn pe(overlay: &[u8], clr: bool) -> Vec<u8> {
    let mut b = vec![0u8; 0x600];
    put(&mut b, 0, b"MZ");
    put(&mut b, 0x3C, &0x80u32.to_le_bytes());
    put(&mut b, 0x80, b"PE\0\0");
    put(&mut b, 0x84, &0x8664u16.to_le_bytes());
    put(&mut b, 0x86, &1u16.to_le_bytes());
    put(&mut b, 0x94, &240u16.to_le_bytes());
    put(&mut b, 0x96, &0x22u16.to_le_bytes());
    let opt = 0x98;
    put(&mut b, opt, &0x20bu16.to_le_bytes());
    put(&mut b, opt + 68, &2u16.to_le_bytes());
    if clr {
        put(&mut b, opt + 112 + 14 * 8, &0x2000u32.to_le_bytes());
    }
    let sec = opt + 240;
    put(&mut b, sec, b".text\0\0\0");
    put(&mut b, sec + 16, &0x200u32.to_le_bytes());
    put(&mut b, sec + 20, &0x400u32.to_le_bytes());
    b.extend_from_slice(overlay);
    b
}

#[test]
fn pe_variants_and_installers() {
    let d = det(&pe(&[], false), "a.exe");
    assert_eq!(
        (
            d.label.as_str(),
            d.field_str("arch"),
            d.field_str("subsystem")
        ),
        ("exe.pe", Some("x86-64"), Some("gui"))
    );
    assert_eq!(label(&pe(&[], true), "a.dll"), "exe.pe.dotnet");
    let mut nsis = vec![0u8; 64];
    put(&mut nsis, 4, &[0xEF, 0xBE, 0xAD, 0xDE]);
    put(&mut nsis, 8, b"NullsoftInst");
    let d = det(&pe(&nsis, false), "setup.exe");
    assert_eq!(d.label, "exe.pe.nsis");
    assert!(!d.flags.suspicious_executable);
    let d = det(&pe(&nsis, false), "Movie.2024.mkv");
    assert!(d.flags.suspicious_executable && d.flags.extension_mismatch);
    let mut sfx = b"Rar!\x1a\x07\x01\x00".to_vec();
    sfx.resize(64, 0);
    assert_eq!(label(&pe(&sfx, false), "a.exe"), "exe.pe.sfx_rar");
    // Only the DOS header known: ask for the PE header.
    let f = sparse(&pe(&[], false), 0x40, "a.exe");
    assert!(matches!(Detector::new().detect(&f), Outcome::NeedMore(r) if r[0].offset == 0x80));
}

#[test]
fn elf_macho_java_scripts() {
    let mut elf = vec![0u8; 64];
    put(&mut elf, 0, b"\x7FELF\x02\x01\x01");
    put(&mut elf, 16, &3u16.to_le_bytes());
    put(&mut elf, 18, &62u16.to_le_bytes());
    let d = det(&elf, "a.so");
    assert_eq!(
        (d.label.as_str(), d.field_str("machine")),
        ("exe.elf", Some("x86-64"))
    );
    put(&mut elf, 8, b"AI\x02");
    assert_eq!(label(&elf, "a.AppImage"), "exe.appimage");
    assert_eq!(
        label(b"\xCF\xFA\xED\xFE\x07\0\0\x01\x03\0\0\0\x02\0\0\0", "a"),
        "exe.macho"
    );
    assert_eq!(
        label(b"\xCA\xFE\xBA\xBE\0\0\0\x34\0\x10", "A.class"),
        "exe.java_class"
    );
    assert_eq!(label(b"#!/bin/sh\necho hi\n", "a.sh"), "text.shell");
    assert_eq!(
        label(b"#!/usr/bin/env python3\nprint(1)\n", "a.py"),
        "text.python"
    );
    assert_eq!(label(b"\0asm\x01\0\0\0", "a.wasm"), "exe.wasm");
    assert_eq!(
        label(b"U\r\r\n\0\0\0\0\0\0\0\0\0\0\0\0", "a.pyc"),
        "misc.pyc"
    );
}

// ---------------------------------------------------------------- disc and disk images

#[test]
fn iso_directory_walk() {
    let dvd = iso("", "MOVIE", &[("VIDEO_TS", true, 22, 2048)], &[]);
    assert_eq!(label(&dvd, "a.iso"), "disc.dvd_video");
    let bd = iso("", "BD", &[("BDMV", true, 22, 2048)], &[]);
    assert_eq!(label(&bd, "a.iso"), "disc.bluray");
    let data = iso(
        "LINUX",
        "DATA",
        &[("README.TXT", false, 22, 5)],
        &[(22, b"hello")],
    );
    let d = det(&data, "a.iso");
    assert_eq!(
        (d.label.as_str(), d.field_str("volume_id")),
        ("disc.iso9660", Some("DATA"))
    );
    // Head only: ask for the descriptors at 0x8000.
    let f = sparse(&data, 16384, "a.iso");
    assert!(
        matches!(Detector::new().detect(&f), Outcome::NeedMore(r) if r.iter().any(|r| r.offset <= 0x8000 && r.end() > 0x8000))
    );
}

#[test]
fn playstation_discs_from_system_cnf() {
    let cnf = b"BOOT2 = cdrom0:\\SLES_123.45;1\r\nVER = 1.00\r\n";
    let ps2 = iso(
        "PLAYSTATION",
        "SLES_12345",
        &[("SYSTEM.CNF", false, 22, cnf.len() as u32)],
        &[(22, cnf)],
    );
    let d = det(&ps2, "game.iso");
    assert_eq!(d.label, "game.ps2.iso");
    assert_eq!(
        d.game.as_ref().and_then(|g| g.title_id.as_deref()),
        Some("SLES12345")
    );
    let cnf1 = b"BOOT = cdrom:\\SLUS_000.01;1\r\n";
    let ps1 = iso(
        "PLAYSTATION",
        "SLUS_00001",
        &[("SYSTEM.CNF", false, 22, cnf1.len() as u32)],
        &[(22, cnf1)],
    );
    assert_eq!(label(&ps1, "game.iso"), "game.ps1.iso");
    assert_eq!(label(&raw_cd(&ps1), "game.bin"), "game.ps1.bin");
    let psp = iso(
        "PSP GAME",
        "PSP",
        &[("PSP_GAME", true, 22, 2048), ("UMD_DATA.BIN", false, 23, 4)],
        &[],
    );
    assert_eq!(label(&psp, "a.iso"), "game.psp.iso");
}

#[test]
fn file_systems_and_virtual_disks() {
    let mut ext = vec![0u8; 4096];
    put(&mut ext, 0x438, &0xEF53u16.to_le_bytes());
    put(&mut ext, 0x418, &1u32.to_le_bytes());
    put(&mut ext, 0x460, &0x40u32.to_le_bytes());
    put(&mut ext, 0x478, b"rootfs");
    let d = det(&ext, "a.img");
    assert_eq!(
        (d.label.as_str(), d.field_str("fs"), d.field_str("volume")),
        ("disc.ext", Some("ext4"), Some("rootfs"))
    );
    put(&mut ext, 0x460, &0u32.to_le_bytes());
    put(&mut ext, 0x45C, &4u32.to_le_bytes());
    assert_eq!(det(&ext, "a.img").field_str("fs"), Some("ext3"));
    let mut ntfs = vec![0u8; 512];
    put(&mut ntfs, 3, b"NTFS    ");
    put(&mut ntfs, 510, &[0x55, 0xAA]);
    assert_eq!(label(&ntfs, "a.img"), "disc.ntfs");
    let mut fat = vec![0u8; 512];
    put(&mut fat, 82, b"FAT32   ");
    put(&mut fat, 510, &[0x55, 0xAA]);
    assert_eq!(label(&fat, "a.img"), "disc.fat");
    let d = det(b"QFI\xfb\0\0\0\x03", "a.qcow2");
    assert_eq!(
        (d.label.as_str(), d.field("version")),
        ("disc.qcow2", Some(&Value::U64(3)))
    );
    let mut vhd = vec![0x11u8; 4096];
    put(&mut vhd, 4096 - 512, b"conectix");
    assert_eq!(det(&vhd, "a.vhd").field_str("variant"), Some("fixed"));
    let mut dmg = vec![0x11u8; 4096];
    put(&mut dmg, 4096 - 512, b"koly");
    assert_eq!(label(&dmg, "a.dmg"), "disc.dmg");
    let mut nrg = vec![0x11u8; 4096];
    put(&mut nrg, 4096 - 12, b"NER5");
    put(&mut nrg, 4096 - 8, &0x800u64.to_be_bytes());
    assert_eq!(label(&nrg, "a.nrg"), "disc.nrg");
}

#[test]
fn tail_signatures_are_fetched_for_unidentified_data() {
    let mut dmg = vec![0x11u8; 1 << 20];
    let n = dmg.len();
    put(&mut dmg, n - 512, b"koly");
    let f = sparse(&dmg, 16384, "a.bin");
    let Outcome::NeedMore(r) = Detector::new().detect(&f) else {
        panic!()
    };
    assert!(r.iter().any(|r| r.end() == n as u64), "{r:?}");
}

// ---------------------------------------------------------------- text and fallback

#[test]
fn text_formats() {
    assert_eq!(
        label(b"1\n00:00:01,000 --> 00:00:02,000\nHello\n\n", "a.srt"),
        "subtitle.srt"
    );
    assert_eq!(
        label(b"WEBVTT\n\n00:01.000 --> 00:02.000\nHi\n", "a.vtt"),
        "subtitle.webvtt"
    );
    assert_eq!(
        label(
            b"[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\n",
            "a.ass"
        ),
        "subtitle.ass"
    );
    assert_eq!(
        label(b"{1}{25}Hello\n{26}{50}World\n", "a.sub"),
        "subtitle.microdvd"
    );
    assert_eq!(
        label(
            b"[InternetShortcut]\r\nURL=https://example.org/\r\n",
            "a.url"
        ),
        "text.url"
    );
    assert_eq!(
        label(b"#EXTM3U\n#EXTINF:10,x\nhttp://x/1.ts\n", "a.m3u"),
        "text.playlist"
    );
    assert_eq!(
        label(b"@echo off\r\nstart evil.exe\r\n", "a.bat"),
        "text.batch"
    );
    assert_eq!(label(b"{\"a\": [1, 2, 3]}\n", "a.json"), "text.json");
    assert_eq!(
        label(b"<!DOCTYPE html><html><body>x</body></html>", "a.html"),
        "text.html"
    );
    assert_eq!(
        label(b"<?xml version=\"1.0\"?><svg xmlns=\"x\"/>", "a.svg"),
        "image.svg"
    );
    assert_eq!(
        label(b"; sfv\nfile.r00 1A2B3C4D\nfile.r01 DEADBEEF\n", "a.sfv"),
        "text.sfv"
    );
    assert_eq!(
        label(b"d41d8cd98f00b204e9800998ecf8427e  empty.txt\n", "a.md5"),
        "text.checksum"
    );
    assert_eq!(
        label(b"FILE \"a.bin\" BINARY\n  TRACK 01 MODE2/2352\n", "a.cue"),
        "text.cue"
    );
    assert_eq!(
        label(b"Just some words.\nAnd more.\n", "a.txt"),
        "text.plain"
    );
    let mut nfo = Vec::new();
    for _ in 0..20 {
        nfo.extend_from_slice(b"\xDB\xDB\xB1\xB1  RELEASE  \xB0\xB0\xDB\r\n");
    }
    assert_eq!(label(&nfo, "a.nfo"), "text.nfo");
    let d = det(b"@echo off\r\nformat c:\r\n", "Movie.avi");
    assert!(d.flags.suspicious_executable);
}

#[test]
fn misc_and_fallback() {
    assert_eq!(label(b"SQLite format 3\0\x10\0", "a.db"), "misc.sqlite");
    assert_eq!(
        label(
            b"d8:announce35:udp://tracker.example:80/announce4:infod4:name1:aee",
            "a.torrent"
        ),
        "misc.torrent"
    );
    assert_eq!(label(b"PAR2\0PKT\0\0\0\0", "a.par2"), "misc.par2");
    assert_eq!(label(&[0u8; 4096], "a.pad"), "misc.zeros");
    assert_eq!(label(b"", "empty"), "misc.empty");
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let noise: Vec<u8> = (0..4096)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s as u8
        })
        .collect();
    let f = SparseFile::from_bytes(noise).with_name("x.bin");
    assert_eq!(Detector::new().finish(&f), Outcome::Unknown);
}

#[test]
fn extension_only_breaks_ties_and_flags() {
    // A PNG named .jpg is a PNG, flagged.
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x01\0\0\0\x01\x08\x02\0\0\0";
    let d = det(png, "4.jpg");
    assert_eq!(d.label, "image.png");
    assert!(d.flags.extension_mismatch);
    assert!(d.extensions.contains(&"png".to_string()));
    // A name with no known extension never raises the flag.
    assert!(!det(png, "image.weird").flags.extension_mismatch);
}

#[test]
fn unverified_bytes_are_flagged() {
    let tar = tar_entry("x");
    let mut f = SparseFile::new(tar.len() as u64);
    f.insert_unverified(0, tar);
    let Outcome::Detected(d) = Detector::new().detect(&f) else {
        panic!()
    };
    assert!(d.flags.unverified);
}

#[test]
fn ranges_are_clamped_to_the_file() {
    let mut f = SparseFile::new(10);
    f.insert(5, vec![1; 100]);
    assert_eq!(f.known_bytes(), 5);
    assert_eq!(f.missing(Range::new(0, 1000)), vec![Range::new(0, 5)]);
}

#[test]
fn utf16_text_and_binary_behind_a_bom() {
    let srt: Vec<u8> = "\u{feff}1\r\n00:00:01,000 --> 00:00:02,000\r\nHello\r\n"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    assert_eq!(label(&srt, "a.srt"), "subtitle.srt");
    // FF FE followed by noise (a misplaced window into compressed data) is not NFO text.
    let mut s = 0x2545_F491_4F6C_DD1Du64;
    let mut noise: Vec<u8> = (0..65536)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s as u8
        })
        .collect();
    noise[..2].copy_from_slice(&[0xFF, 0xFE]);
    let mut f = SparseFile::new(1 << 30);
    f.insert(0, noise);
    let d = Detector::new().analyze(&f).detection;
    assert!(d.confidence < 80, "{} {}", d.label, d.confidence);
}

/// Seeded xorshift bytes (no rand dependency).
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// Review finding: a magic followed by random bytes got 95+ (gzip and zstd even though their
/// inflate peek failed, Ogg without a page CRC check, `%PDF-1.4`, JPEG, NES), against the
/// contract that 95+ means signature plus a passing structural check.
#[test]
fn garbage_behind_a_magic_stays_below_95() {
    let magics: [&[u8]; 6] = [
        b"\x1f\x8b\x08\x00",
        b"\x28\xb5\x2f\xfd",
        b"OggS\0",
        b"%PDF-1.4",
        b"\xff\xd8\xff\xe0",
        b"NES\x1a",
    ];
    for m in magics {
        for seed in 1..=8u64 {
            let mut b = noise(1 << 20, seed * 0x9E37_79B9);
            b[..m.len()].copy_from_slice(m);
            let d = det(&b, "x.bin");
            assert!(
                d.confidence < 95,
                "{m:?} seed {seed}: {} {}",
                d.label,
                d.confidence
            );
        }
    }
    // The genuine articles keep 95+.
    let payload = noise(50_000, 7)
        .iter()
        .map(|b| b % 16 + b'a')
        .collect::<Vec<u8>>();
    assert!(det(&gzip(&payload), "a.gz").confidence >= 95);
    assert!(det(&zstd(&payload), "a.zst").confidence >= 95);
    let mut pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
    pdf.extend(vec![b' '; 2000]);
    assert_eq!(det(&pdf, "a.pdf").confidence, 97);
    let mut jpg = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01".to_vec();
    jpg.extend(noise(4000, 3));
    assert_eq!(det(&jpg, "a.jpg").confidence, 95);
}

/// Signature rules: 95+ needs 64 bits of matched evidence, whatever the rule claims; a magic
/// shorter than 8 bytes with nothing else checked is a hint (at most 79, not stored).
#[test]
fn short_magic_rules_are_capped() {
    let user = r#"
[[rule]]
id = "four"
label = "custom.four"
confidence = 99
type = "string"
value = 'QZ4\x01'

[[rule]]
id = "four_checked"
label = "custom.four_checked"
confidence = 99
type = "string"
value = 'QZ5\x01'
  [[rule.then]]
  offset = 4
  type = "u16le"
  test = "7"

[[rule]]
id = "seven"
label = "custom.seven"
confidence = 99
type = "string"
value = 'QZ7\x01\x02\x03\x04'

[[rule]]
id = "eight"
label = "custom.eight"
confidence = 99
type = "string"
value = 'QZ8\x01\x02\x03\x04\x05'
"#;
    let sigs = sniff::Signatures::from_toml("user.toml", user).unwrap();
    let det = sniff::Detector::with_signatures(&sigs);
    let conf = |m: &[u8]| {
        let mut b = m.to_vec();
        b.extend(noise(4000, 5));
        det.analyze(&sniff::SparseFile::from_bytes(b))
            .detection
            .confidence
    };
    assert_eq!(conf(b"QZ4\x01"), 79);
    assert_eq!(conf(b"QZ5\x01\x07\x00"), 94);
    assert_eq!(conf(b"QZ7\x01\x02\x03\x04"), 79);
    assert_eq!(conf(b"QZ8\x01\x02\x03\x04\x05"), 99);
}

/// Short magics (under 8 bytes) of the built-in rules and Rust checks, each put in random
/// bytes: nothing reaches the 80 that callers store. Before the cap these scored 80-94 (the
/// tracker MOD tag 85, DTS 85, N64 90-92, TIFF 92, the GB logo start 85, NCCH 85, MPEG-PS 92,
/// AV1 85, MPEG-2 video 85, RAR 1.4 80, ...).
#[test]
fn short_magic_behind_random_bytes_stays_below_80() {
    let cases: &[(&str, i64, &[u8])] = &[
        ("mod", 1080, b"M.K."),
        ("mod 8ch", 1080, b"8CHN"),
        ("dts", 0, b"\x7F\xFE\x80\x01"),
        ("dts le", 0, b"\xFE\x7F\x01\x80"),
        ("dts 14", 0, b"\x1F\xFF\xE8\x00"),
        ("n64", 0, b"\x80\x37\x12\x40"),
        ("v64", 0, b"\x37\x80\x40\x12"),
        ("tiff", 0, b"II*\0"),
        ("tiff be", 0, b"MM\0*"),
        ("bigtiff", 0, b"II+\0"),
        ("gif", 0, b"GIF89a"),
        ("nes", 0, b"NES\x1a"),
        ("gb", 0x104, b"\xCE\xED\x66\x66\xCC\x0D"),
        ("ncch", 0x100, b"NCCH"),
        ("mpeg-ps", 0, b"\0\0\x01\xBA"),
        ("mpeg-2 video", 0, b"\0\0\x01\xB3"),
        ("av1", 0, b"\x12\x00\x0A"),
        ("rar 1.4", 0, b"RE~^"),
        ("amr", 0, b"#!AMR\n"),
        ("qoi", 0, b"qoif"),
        ("lz4", 0, b"\x04\x22\x4D\x18"),
        ("lzip", 0, b"LZIP"),
        ("squashfs", 0, b"hsqs"),
        ("qcow2", 0, b"QFI\xfb"),
        ("vmdk", 0, b"KDMV"),
        ("dmg", -512, b"koly"),
        ("woff", 0, b"wOFF"),
        ("woff2", 0, b"wOF2"),
        ("wavpack", 0, b"wvpk"),
        ("tta", 0, b"TTA1"),
        ("au", 0, b".snd"),
        ("dsf", 0, b"DSD "),
        ("realmedia", 0, b".RMF"),
        ("exr", 0, b"v/1\x01"),
        ("truehd", 4, b"\xF8\x72\x6F\xBA"),
        ("postscript", 0, b"%!PS"),
        ("rtf", 0, b"{\\rtf"),
        ("lua", 0, b"\x1bLua"),
        ("nsf", 0, b"NESM\x1a"),
        ("psf", 0, b"PSF\x01"),
        ("gbs", 0, b"GBS\x01"),
        ("lynx", 0, b"LYNX\0"),
        ("fds", 0, b"FDS\x1a"),
        ("j2k", 0, b"\xFF\x4F\xFF\x51"),
        ("wuhb", 0, b"WUHB"),
        ("isz", 0, b"IsZ!"),
        ("daa", 0, b"DAA\0"),
        ("hfs+", 0x400, b"H+\0\x04"),
        ("ecm", 0, b"ECM\0"),
        ("cpio", 0, b"070701"),
        ("warc", 0, b"WARC/1."),
        ("rgss", 0, b"RGSSAD\0"),
    ];
    for (name, off, magic) in cases {
        for seed in 1..=8u64 {
            let mut b = noise(1 << 16, seed * 0x9E37_79B9 + 11);
            let at = if *off < 0 {
                b.len() - off.unsigned_abs() as usize
            } else {
                *off as usize
            };
            b[at..at + magic.len()].copy_from_slice(magic);
            let d = Detector::new()
                .analyze(&SparseFile::from_bytes(b).with_name("x.bin"))
                .detection;
            assert!(
                d.confidence < 80,
                "{name} seed {seed}: {} {} ({:?})",
                d.label,
                d.confidence,
                d.candidates
            );
        }
    }
}

/// The genuine articles of the short-magic formats keep 80+: their structural checks pass.
#[test]
fn short_magic_formats_with_structure_reach_80() {
    // ProTracker MOD: sane sample table, one pattern, the pattern data present.
    let mut m = vec![0u8; 1084 + 1024];
    put(&mut m, 0, b"song");
    m[950] = 1;
    put(&mut m, 1080, b"M.K.");
    let d = det(&m, "a.mod");
    assert_eq!((d.label.as_str(), d.confidence), ("audio.mod", 95));
    // DTS core frames (a 5.1 frame from ffmpeg, twice: the frame chain).
    let f = include_bytes!("video_fixtures/dts_core51.bin");
    let two = [&f[..], &f[..]].concat();
    let d = det(&two, "a.dts");
    assert_eq!((d.label.as_str(), d.confidence), ("audio.dts", 95));
    // The same with only the first KiB known: the next sync word is asked for.
    let file = sparse(&two, 1024, "a.dts");
    let Outcome::NeedMore(r) = Detector::new().detect(&file) else {
        panic!("dts: no range asked")
    };
    assert!(
        r.iter()
            .any(|r| r.offset <= f.len() as u64 && r.end() > f.len() as u64),
        "{r:?}"
    );
    // N64: boot address in KSEG0 (homebrew, blank game code), then title + code.
    let mut n = vec![0u8; 0x1000];
    put(&mut n, 0, b"\x80\x37\x12\x40");
    put(&mut n, 8, &0x8000_0400u32.to_be_bytes());
    assert_eq!(det(&n, "a.z64").confidence, 90);
    put(&mut n, 0x20, b"SUPER GAME          ");
    put(&mut n, 0x3B, b"NSME");
    assert_eq!(det(&n, "a.z64").confidence, 96);
    // NES: the file size matches PRG + CHR.
    let mut nes = vec![0u8; 16 + 2 * 16384 + 8192];
    put(&mut nes, 0, b"NES\x1a\x02\x01");
    let d = det(&nes, "a.nes");
    assert_eq!((d.label.as_str(), d.confidence), ("game.nes.rom", 96));
    // TIFF: an IFD of two entries with valid field types, at the end of the file.
    let mut t = b"II*\0".to_vec();
    t.extend(&4000u32.to_le_bytes());
    t.extend(noise(4000 - 8, 3));
    t.extend(&2u16.to_le_bytes());
    for (tag, ty) in [(0x100u16, 3u16), (0x101, 3)] {
        t.extend(&tag.to_le_bytes());
        t.extend(&ty.to_le_bytes());
        t.extend(&1u32.to_le_bytes());
        t.extend(&64u32.to_le_bytes());
    }
    t.extend(&0u32.to_le_bytes());
    let d = det(&t, "a.tif");
    assert_eq!((d.label.as_str(), d.confidence), ("image.tiff", 97));
    // Only the head known: the IFD is asked for.
    let file = sparse(&t, 64, "a.tif");
    let Outcome::NeedMore(r) = Detector::new().detect(&file) else {
        panic!("tiff: no range asked")
    };
    assert!(r.iter().any(|r| r.offset == 4000), "{r:?}");
    // GIF: global colour table of 2 entries, then an image descriptor.
    let mut g = b"GIF89a\x02\x00\x02\x00\x80\x00\x00".to_vec();
    g.extend([0, 0, 0, 255, 255, 255, 0x2C]);
    g.extend(noise(64, 4));
    let d = det(&g, "a.gif");
    assert_eq!((d.label.as_str(), d.confidence), ("image.gif", 97));
    // AMR: four 12.2 kbit/s frames.
    let mut a = b"#!AMR\n".to_vec();
    for _ in 0..4 {
        a.push(0x3C);
        a.extend(noise(31, 5));
    }
    let d = det(&a, "a.amr");
    assert_eq!((d.label.as_str(), d.confidence), ("audio.amr", 95));
    // TOML rules with a structural child: QOI channels / colourspace, TrueHD major sync.
    let mut q = b"qoif\0\0\0\x40\0\0\0\x30\x03\x00".to_vec();
    q.extend(noise(64, 6));
    assert!(det(&q, "a.qoi").confidence >= 80);
    let mut thd = noise(64, 7);
    put(&mut thd, 4, b"\xF8\x72\x6F\xBA");
    put(&mut thd, 12, b"\xB7\x52");
    let d = det(&thd, "a.thd");
    assert_eq!(d.label, "audio.truehd");
    assert!(d.confidence >= 80);
}
