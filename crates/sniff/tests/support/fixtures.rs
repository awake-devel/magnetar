//! Small synthetic files built in code, shared by the integration tests. They follow each
//! format's documented layout; for the console formats that makes the tests partly circular
//! (UNVERIFIED against real dumps).

#![allow(dead_code)]

use sniff::{Detection, Detector, Outcome, SparseFile};

pub fn put(b: &mut [u8], at: usize, v: &[u8]) {
    b[at..at + v.len()].copy_from_slice(v);
}

/// Whole file known: the final verdict (panics on Unknown).
pub fn det(bytes: &[u8], name: &str) -> Detection {
    let f = SparseFile::from_bytes(bytes.to_vec()).with_name(name);
    match Detector::new().finish(&f) {
        Outcome::Detected(d) => d,
        o => panic!("{name}: {o:?}"),
    }
}

pub fn label(bytes: &[u8], name: &str) -> String {
    det(bytes, name).label
}

/// Only `[0, head)` known of a file of `len` bytes.
pub fn sparse(bytes: &[u8], head: usize, name: &str) -> SparseFile {
    let mut f = SparseFile::new(bytes.len() as u64).with_name(name);
    f.insert(0, bytes[..head.min(bytes.len())].to_vec());
    f
}

// ---------------------------------------------------------------- containers

pub fn riff(form: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
    v.extend_from_slice(form);
    v.extend_from_slice(body);
    v
}

/// One ZIP local header + data per entry (stored), central directory and end record.
pub fn zip_store(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for (name, data) in entries {
        let off = out.len() as u32;
        let crc = crc32fast::hash(data);
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        cd.extend_from_slice(b"PK\x01\x02");
        cd.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        cd.extend_from_slice(&crc.to_le_bytes());
        cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
        cd.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        cd.extend_from_slice(&off.to_le_bytes());
        cd.extend_from_slice(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// A ustar header block for `name` with a valid checksum, followed by one data block.
pub fn tar_entry(name: &str) -> Vec<u8> {
    let mut h = vec![0u8; 512];
    put(&mut h, 0, name.as_bytes());
    put(&mut h, 100, b"0000644\0");
    put(&mut h, 108, b"0000000\0");
    put(&mut h, 116, b"0000000\0");
    put(&mut h, 124, b"00000000005\0");
    put(&mut h, 136, b"00000000000\0");
    h[156] = b'0';
    put(&mut h, 257, b"ustar\x0000");
    put(&mut h, 148, b"        ");
    let sum: u32 = h.iter().map(|&c| u32::from(c)).sum();
    put(&mut h, 148, format!("{sum:06o}\0 ").as_bytes());
    h.extend_from_slice(&[b'x'; 5]);
    h.resize(1024, 0);
    h
}

pub fn gzip(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

pub fn zlib(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

pub fn zstd(data: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest)
}

/// EBML element with a 1- or 2-byte size.
pub fn ebml(id: &[u8], data: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    if data.len() < 0x7f {
        v.push(0x80 | data.len() as u8);
    } else {
        v.extend_from_slice(&(0x4000u16 | data.len() as u16).to_be_bytes());
    }
    v.extend_from_slice(data);
    v
}

pub fn mkv(doctype: &str, tracks: &[(u8, &str)]) -> Vec<u8> {
    let mut head = ebml(&[0x42, 0x86], &[1]);
    head.extend(ebml(&[0x42, 0x82], doctype.as_bytes()));
    let mut out = ebml(&[0x1A, 0x45, 0xDF, 0xA3], &head);
    let mut entries = Vec::new();
    for (i, (ty, codec)) in tracks.iter().enumerate() {
        let mut te = ebml(&[0xD7], &[i as u8 + 1]);
        te.extend(ebml(&[0x83], &[*ty]));
        te.extend(ebml(&[0x86], codec.as_bytes()));
        entries.extend(ebml(&[0xAE], &te));
    }
    let mut seg = ebml(
        &[0x15, 0x49, 0xA9, 0x66],
        &ebml(&[0x2A, 0xD7, 0xB1], &[0x0F, 0x42, 0x40]),
    );
    seg.extend(ebml(&[0x16, 0x54, 0xAE, 0x6B], &entries));
    seg.extend(ebml(&[0x1F, 0x43, 0xB6, 0x75], &[0u8; 32]));
    out.extend(ebml(&[0x18, 0x53, 0x80, 0x67], &seg));
    out
}

pub fn mp4_box(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(body);
    v
}

/// ftyp + mdat of `mdat_len` bytes + moov with the given handlers (moov at the end).
pub fn mp4(brand: &[u8; 4], handlers: &[&[u8; 4]], mdat_len: usize) -> Vec<u8> {
    let mut ftyp = brand.to_vec();
    ftyp.extend_from_slice(&[0, 0, 2, 0]);
    ftyp.extend_from_slice(b"isommp41");
    let mut out = mp4_box(b"ftyp", &ftyp);
    out.extend(mp4_box(b"mdat", &vec![0u8; mdat_len]));
    let mut traks = Vec::new();
    for h in handlers {
        let mut hd = vec![0u8; 8];
        hd.extend_from_slice(*h);
        hd.extend_from_slice(&[0u8; 12]);
        let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd.extend(mp4_box(
            if *h == b"vide" { b"hvc1" } else { b"mp4a" },
            &[0u8; 8],
        ));
        let stbl = mp4_box(b"stbl", &mp4_box(b"stsd", &stsd));
        let mut mdia = mp4_box(b"hdlr", &hd);
        mdia.extend(mp4_box(b"minf", &stbl));
        traks.extend(mp4_box(b"trak", &mp4_box(b"mdia", &mdia)));
    }
    out.extend(mp4_box(b"moov", &traks));
    out
}

// ---------------------------------------------------------------- disc images

/// A cooked ISO 9660 image: PVD at sector 16, terminator at 17, root directory at sector 20 with
/// the given entries (name, is_dir, lba, size), and file contents at their sectors.
pub fn iso(
    system: &str,
    volume: &str,
    entries: &[(&str, bool, u32, u32)],
    files: &[(u32, &[u8])],
) -> Vec<u8> {
    let mut img = vec![0u8; 2048 * 40];
    let pvd = 16 * 2048;
    img[pvd] = 1;
    put(&mut img, pvd + 1, b"CD001\x01");
    put(&mut img, pvd + 8, format!("{system:<32}").as_bytes());
    put(&mut img, pvd + 40, format!("{volume:<32}").as_bytes());
    let root = dir_record(&[0], true, 20, 2048);
    put(&mut img, pvd + 156, &root);
    let term = 17 * 2048;
    img[term] = 255;
    put(&mut img, term + 1, b"CD001\x01");
    let mut dir = dir_record(&[0], true, 20, 2048);
    dir.extend(dir_record(&[1], true, 20, 2048));
    for (name, is_dir, lba, size) in entries {
        let n = if *is_dir {
            name.to_string()
        } else {
            format!("{name};1")
        };
        dir.extend(dir_record(n.as_bytes(), *is_dir, *lba, *size));
    }
    put(&mut img, 20 * 2048, &dir);
    for (lba, data) in files {
        put(&mut img, *lba as usize * 2048, data);
    }
    img
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

/// Wraps cooked 2048-byte sectors into raw mode-2 form-1 2352-byte sectors.
pub fn raw_cd(cooked: &[u8]) -> Vec<u8> {
    let mut raw = Vec::new();
    for (s, sec) in cooked.chunks(2048).enumerate() {
        raw.push(0);
        raw.extend_from_slice(&[0xFF; 10]);
        raw.push(0);
        raw.extend_from_slice(&[0, 2, (0x16 + s % 75) as u8, 2]);
        raw.extend_from_slice(&[0, 0, 8, 0, 0, 0, 8, 0]);
        raw.extend_from_slice(sec);
        raw.extend_from_slice(&[0u8; 280]);
    }
    raw
}
