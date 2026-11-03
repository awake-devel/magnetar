//! Video recon on hand-built containers around real first frames (see `video_fixtures/`).
//!
//! The E-AC-3 / TrueHD frames are ffmpeg output patched to carry the Atmos signalling
//! (ffprobe confirms the profiles); the DTS-HD extension substreams and the E-AC-3 dependent
//! substream are built from the specifications here, against the same reading of them as the
//! parsers, so those tests are partly circular (UNVERIFIED on real streams).

#[path = "video_fixtures/build.rs"]
mod build;

use build::*;
use sniff::video::{AudioFeature, AudioTrack, HdrFormat, VideoInfo};
use sniff::{Detector, Outcome, SparseFile};

fn info(d: &Drive) -> &VideoInfo {
    d.recon.info.as_ref().expect("video info")
}

fn audio(i: &VideoInfo, id: u64) -> &AudioTrack {
    i.audio.iter().find(|a| a.id == id).expect("audio track")
}

fn lang(l: &str) -> Vec<u8> {
    el(&[0x22, 0xB5, 0x9C], l.as_bytes())
}

/// HEVC-less MKV: AVC video with a 300 KB first frame, then TrueHD / E-AC-3 / DTS blocks.
/// Track numbers are not the track order (TrueHD is entry 0 but track 9).
fn premium_mkv() -> Vec<u8> {
    let mut video = el(&[0xE0], &[uel(&[0xB0], 1920), uel(&[0xBA], 1080)].concat());
    video.extend(uel(&[0x23, 0xE3, 0x83], 41_708_333));
    let t = tracks(&[
        track(9, 2, "A_TRUEHD", &lang("eng")),
        track(2, 1, "V_MPEG4/ISO/AVC", &video),
        track(5, 2, "A_EAC3", &lang("fre")),
        track(7, 2, "A_DTS", &lang("ger")),
    ]);
    let c = cluster(&[
        simple_block(2, &vec![0x55; 300_000]),
        simple_block(5, EAC3_JOC),
        simple_block(9, TRUEHD_ATMOS),
        simple_block(7, DTS_CORE),
    ]);
    let mut f = ebml_header("matroska");
    f.extend(segment(&[mkv_info(10_000.0), t, c]));
    f
}

#[test]
fn mkv_first_audio_frames_behind_a_large_keyframe() {
    let bytes = premium_mkv();
    let d = drive(&bytes, 16 * 1024, 8);
    let i = info(&d);
    assert!(d.recon.need.is_empty(), "{:?}", d.recon.need);
    assert_eq!(i.container, "mkv");
    assert_eq!(i.duration_ms, Some(10_000));
    assert_eq!(i.video.len(), 1);
    assert_eq!(i.video[0].width, Some(1920));
    let thd = audio(i, 9);
    assert_eq!(thd.codec, "truehd");
    assert_eq!(thd.atmos, Some(true));
    assert_eq!(thd.profile.as_deref(), Some("Dolby TrueHD + Dolby Atmos"));
    assert_eq!(thd.channels, Some(6));
    assert_eq!(thd.language.as_deref(), Some("eng"));
    assert!(thd.features.contains(&AudioFeature::Atmos));
    let ddp = audio(i, 5);
    assert_eq!(ddp.codec, "eac3");
    assert_eq!(ddp.atmos, Some(true));
    assert!(ddp.features.contains(&AudioFeature::Joc));
    assert_eq!(ddp.language.as_deref(), Some("fre"));
    let dts = audio(i, 7);
    assert_eq!((dts.codec.as_str(), dts.channels), ("dts", Some(6)));
    assert_eq!(dts.profile.as_deref(), Some("DTS"));
    assert_eq!((dts.atmos, dts.dts_x), (Some(false), Some(false)));
    // The 300 KB keyframe is skipped by its header: only blocks around it are fetched.
    assert!(d.fetched < 100_000, "fetched {}", d.fetched);
    assert!(d.rounds <= 3, "rounds {}", d.rounds);
}

#[test]
fn atmos_stays_unknown_when_the_frame_is_not_reached() {
    let bytes = premium_mkv();
    let d = drive(&bytes, 16 * 1024, 0);
    let i = info(&d);
    assert!(!d.recon.need.is_empty());
    assert_eq!(audio(i, 9).atmos, None, "TrueHD frame not fetched");
    assert_eq!(audio(i, 5).atmos, None, "E-AC-3 frame not fetched");
    assert_eq!(audio(i, 7).atmos, Some(false), "DTS cannot carry Atmos");
    assert!(
        i.notes
            .iter()
            .any(|n| n.contains("first frame of audio track 9")),
        "{:?}",
        i.notes
    );
}

#[test]
fn plain_truehd_and_eac3_are_not_atmos() {
    let t = tracks(&[track(1, 2, "A_TRUEHD", &[]), track(2, 2, "A_EAC3", &[])]);
    let c = cluster(&[simple_block(1, TRUEHD_51), simple_block(2, EAC3_PLAIN)]);
    let mut f = ebml_header("matroska");
    f.extend(segment(&[mkv_info(1000.0), t, c]));
    let d = drive(&f, 64 * 1024, 2);
    let i = info(&d);
    assert_eq!(audio(i, 1).atmos, Some(false));
    assert_eq!(audio(i, 1).profile, None);
    assert_eq!(audio(i, 2).atmos, Some(false));
    assert_eq!(audio(i, 2).channels, Some(6));
}

#[test]
fn mkv_header_stripping_is_undone() {
    // ContentEncoding / ContentCompression algo 3 with the sync word stripped from every frame.
    let comp = el(
        &[0x50, 0x34],
        &[uel(&[0x42, 0x54], 3), el(&[0x42, 0x55], &EAC3_JOC[..2])].concat(),
    );
    let enc = el(&[0x6D, 0x80], &el(&[0x62, 0x40], &comp));
    let t = tracks(&[track(1, 2, "A_EAC3", &enc)]);
    let c = cluster(&[simple_block(1, &EAC3_JOC[2..])]);
    let mut f = ebml_header("matroska");
    f.extend(segment(&[mkv_info(1000.0), t, c]));
    let d = drive(&f, 64 * 1024, 2);
    assert_eq!(audio(info(&d), 1).atmos, Some(true));
}

#[test]
fn mkv_xiph_laced_block() {
    // Two E-AC-3 frames in one block with Xiph lacing.
    let (a, b) = EAC3_JOC.split_at(1536);
    let mut payload = vec![0x80 | 1, 0, 0, 0x80 | 0x02, 1];
    let mut n = a.len();
    while n >= 255 {
        payload.push(255);
        n -= 255;
    }
    payload.push(n as u8);
    payload.extend_from_slice(a);
    payload.extend_from_slice(b);
    let t = tracks(&[track(1, 2, "A_EAC3", &[])]);
    let c = el(
        &[0x1F, 0x43, 0xB6, 0x75],
        &[uel(&[0xE7], 0), el(&[0xA3], &payload)].concat(),
    );
    let mut f = ebml_header("matroska");
    f.extend(segment(&[mkv_info(1000.0), t, c]));
    let d = drive(&f, 64 * 1024, 2);
    assert_eq!(audio(info(&d), 1).atmos, Some(true));
}

#[test]
fn mkv_tracks_after_the_clusters_via_seekhead() {
    let t = tracks(&[track(1, 2, "A_EAC3", &lang("spa"))]);
    let c = cluster(&[
        simple_block(1, EAC3_JOC),
        simple_block(1, &vec![0u8; 200_000]),
    ]);
    let inf = mkv_info(5000.0);
    let seek = |pos: u64| {
        let s = el(
            &[0x4D, 0xBB],
            &[
                el(&[0x53, 0xAB], &[0x16, 0x54, 0xAE, 0x6B]),
                uel(&[0x53, 0xAC], pos),
            ]
            .concat(),
        );
        el(&[0x11, 0x4D, 0x9B, 0x74], &s)
    };
    let head_len = seek(0).len() + inf.len() + c.len();
    let sh = seek(head_len as u64);
    let mut f = ebml_header("matroska");
    f.extend(segment(&[sh, inf, c, t]));
    let d = drive(&f, 16 * 1024, 4);
    let i = info(&d);
    assert!(d.recon.need.is_empty());
    let a = audio(i, 1);
    assert_eq!(a.language.as_deref(), Some("spa"));
    assert_eq!(a.atmos, Some(true));
    assert!(d.fetched < 64 * 1024, "fetched {}", d.fetched);
}

/// Dependent E-AC-3 substream (ETSI TS 102 366 E.1.2.2) adding Lrs/Rrs to a 5.1 independent
/// frame: 7.1. Spec-derived.
#[test]
fn eac3_dependent_substream_makes_7_1() {
    let mut w = Bw::default();
    w.put(16, 0x0b77)
        .put(2, 1) // dependent
        .put(3, 0)
        .put(11, 63) // 128 bytes
        .put(2, 0)
        .put(2, 3)
        .put(3, 2) // 2/0
        .put(1, 0)
        .put(5, 16)
        .put(5, 31)
        .put(1, 0)
        .put(1, 1) // chanmape
        .put(16, 1 << (15 - 6)) // Lrs/Rrs pair
        .put(1, 0)
        .put(1, 0)
        .put(1, 0);
    let dep = w.pad_to(128);
    let t = tracks(&[track(1, 2, "A_EAC3", &[])]);
    let mut block = EAC3_PLAIN[..1536].to_vec();
    block.extend_from_slice(&dep);
    block.extend_from_slice(&EAC3_PLAIN[1536..]);
    let c = cluster(&[simple_block(1, &block)]);
    let mut f = ebml_header("matroska");
    f.extend(segment(&[mkv_info(1000.0), t, c]));
    let d = drive(&f, 64 * 1024, 2);
    let a = audio(info(&d), 1);
    assert_eq!(a.channels, Some(8));
    assert_eq!(a.layout.as_deref(), Some("7.1"));
}

/// DTS core + extension substream (ETSI TS 102 114 7.4 / 7.5). Spec-derived.
fn dts_with_exss(coding_mode: u64, mask: u64, x_sync: bool) -> Vec<u8> {
    let header = 32u64;
    let body: Vec<u8> = if x_sync {
        let mut b = vec![0x11; 64];
        b.extend_from_slice(&[0x02, 0x00, 0x08, 0x50]);
        b.extend_from_slice(&[0x22; 60]);
        b
    } else {
        vec![0x11; 128]
    };
    let mut w = Bw::default();
    w.put(32, 0x6458_2025)
        .put(8, 0)
        .put(2, 0)
        .put(1, 0)
        .put(8, header - 1)
        .put(16, header + body.len() as u64 - 1)
        .put(1, 0) // no static fields
        .put(16, body.len() as u64 - 1) // asset size
        .put(9, 20)
        .put(3, 0)
        .put(1, 0) // no DRC
        .put(1, 0) // no dialnorm
        .put(2, coding_mode);
    if coding_mode == 0 {
        w.put(12, mask);
    }
    let mut ex = w.pad_to(header as usize);
    ex.extend(body);
    let mut f = DTS_CORE.to_vec();
    f.extend(ex);
    f
}

fn dts_track(frame: &[u8]) -> AudioTrack {
    let t = tracks(&[track(1, 2, "A_DTS", &[])]);
    let c = cluster(&[simple_block(1, frame)]);
    let mut f = ebml_header("matroska");
    f.extend(segment(&[mkv_info(1000.0), t, c]));
    let d = drive(&f, 64 * 1024, 2);
    audio(info(&d), 1).clone()
}

#[test]
fn dts_hd_master_audio_and_dts_x() {
    let ma = dts_track(&dts_with_exss(1, 0, false));
    assert_eq!(ma.profile.as_deref(), Some("DTS-HD MA"));
    assert_eq!(ma.dts_x, Some(false));
    assert!(ma.features.contains(&AudioFeature::DtsHdMa));
    assert!(ma.features.contains(&AudioFeature::Lossless));
    let x = dts_track(&dts_with_exss(1, 0, true));
    assert_eq!(x.profile.as_deref(), Some("DTS-HD MA + DTS:X"));
    assert_eq!(x.dts_x, Some(true));
    assert!(x.features.contains(&AudioFeature::DtsX));
    let hra = dts_track(&dts_with_exss(0, 0x020 | 0x010, false));
    assert_eq!(hra.profile.as_deref(), Some("DTS-HD HRA"));
    assert!(hra.features.contains(&AudioFeature::DtsHdHra));
    let core = dts_track(DTS_CORE);
    assert_eq!(core.profile.as_deref(), Some("DTS"));
    assert_eq!(core.atmos, Some(false));
}

// ------------------------------------------------------------------ MP4

fn dec3(ext: Option<bool>) -> Vec<u8> {
    let mut w = Bw::default();
    w.put(13, 768)
        .put(3, 0)
        .put(2, 0)
        .put(5, 16)
        .put(1, 0)
        .put(1, 0)
        .put(3, 0)
        .put(3, 7)
        .put(1, 1)
        .put(3, 0)
        .put(4, 0)
        .put(1, 0);
    if let Some(e) = ext {
        w.put(7, 0).put(1, u64::from(e)).put(8, 16);
    }
    bx(b"dec3", &w.bytes)
}

/// ftyp, mdat (`pad` zeros then `sample`), moov at the end with one audio track.
fn mp4_audio(fourcc: &[u8; 4], children: &[u8], sample: &[u8], pad: usize, lang: u16) -> Vec<u8> {
    let ftyp = bx(b"ftyp", b"isom\0\0\x02\0isommp41");
    let mut mdat_body = vec![0u8; pad];
    mdat_body.extend_from_slice(sample);
    let mdat = bx(b"mdat", &mdat_body);
    let sample_off = (ftyp.len() + 8 + pad) as u32;
    let mut entry = vec![0u8; 6];
    entry.extend_from_slice(&1u16.to_be_bytes());
    entry.extend_from_slice(&[0; 8]);
    entry.extend_from_slice(&6u16.to_be_bytes());
    entry.extend_from_slice(&16u16.to_be_bytes());
    entry.extend_from_slice(&[0; 4]);
    entry.extend_from_slice(&(48000u32 << 16).to_be_bytes());
    entry.extend_from_slice(children);
    let stsd = full(
        b"stsd",
        0,
        0,
        &[1u32.to_be_bytes().to_vec(), bx(fourcc, &entry)].concat(),
    );
    let stsz = full(
        b"stsz",
        0,
        0,
        &[
            0u32.to_be_bytes(),
            1u32.to_be_bytes(),
            (sample.len() as u32).to_be_bytes(),
        ]
        .concat(),
    );
    let stco = full(
        b"stco",
        0,
        0,
        &[1u32.to_be_bytes(), sample_off.to_be_bytes()].concat(),
    );
    let stbl = bx(
        b"stbl",
        &[stsd, full(b"stts", 0, 0, &[0; 4]), stsz, stco].concat(),
    );
    let minf = bx(b"minf", &[full(b"smhd", 0, 0, &[0; 4]), stbl].concat());
    let mut mdhd = vec![0u8; 8];
    mdhd.extend_from_slice(&48000u32.to_be_bytes());
    mdhd.extend_from_slice(&480_000u32.to_be_bytes());
    mdhd.extend_from_slice(&lang.to_be_bytes());
    mdhd.extend_from_slice(&[0, 0]);
    let hdlr = full(b"hdlr", 0, 0, b"\0\0\0\0soun\0\0\0\0\0\0\0\0\0\0\0\0\0");
    let mdia = bx(b"mdia", &[full(b"mdhd", 0, 0, &mdhd), hdlr, minf].concat());
    let mut tkhd = vec![0u8; 8];
    tkhd.extend_from_slice(&1u32.to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 68]);
    let trak = bx(b"trak", &[full(b"tkhd", 0, 1, &tkhd), mdia].concat());
    let mut mvhd = vec![0u8; 8];
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    mvhd.extend_from_slice(&10_000u32.to_be_bytes());
    mvhd.extend_from_slice(&[0u8; 80]);
    let moov = bx(b"moov", &[full(b"mvhd", 0, 0, &mvhd), trak].concat());
    [ftyp, mdat, moov].concat()
}

const ENG: u16 = (5 << 10) | (14 << 5) | 7; // packed ISO 639-2 "eng"

#[test]
fn mp4_moov_at_end_first_sample_decides_atmos() {
    // dec3 without the extension: the first E-AC-3 frame decides.
    let f = mp4_audio(b"ec-3", &dec3(None), EAC3_JOC, 3 << 20, ENG);
    let d = drive(&f, 16 * 1024, 6);
    let i = info(&d);
    assert!(d.recon.need.is_empty());
    let a = &i.audio[0];
    assert_eq!(a.codec, "eac3");
    assert_eq!(a.atmos, Some(true));
    assert_eq!(a.language.as_deref(), Some("eng"));
    assert_eq!(i.duration_ms, Some(10_000));
    assert!(d.fetched < 128 * 1024, "fetched {}", d.fetched);
    assert!(d.rounds <= 2, "rounds {}", d.rounds);

    let plain = mp4_audio(b"ec-3", &dec3(None), EAC3_PLAIN, 3 << 20, ENG);
    assert_eq!(
        info(&drive(&plain, 16 * 1024, 6)).audio[0].atmos,
        Some(false)
    );
}

#[test]
fn mp4_dec3_extension_flag() {
    // Atmos flag in dec3, sample bytes never fetched (rounds 1: moov only).
    let f = mp4_audio(b"ec-3", &dec3(Some(true)), EAC3_PLAIN, 3 << 20, ENG);
    let d = drive(&f, 16 * 1024, 1);
    let a = &info(&d).audio[0];
    assert_eq!(a.atmos, Some(true));
    assert_eq!(a.channels, Some(6));
}

#[test]
fn mp4_truehd_mlpa() {
    let f = mp4_audio(b"mlpa", &bx(b"dmlp", &[0; 10]), TRUEHD_ATMOS, 1 << 20, ENG);
    let d = drive(&f, 16 * 1024, 6);
    let a = &info(&d).audio[0];
    assert_eq!((a.codec.as_str(), a.atmos), ("truehd", Some(true)));
    let f = mp4_audio(b"mlpa", &bx(b"dmlp", &[0; 10]), TRUEHD_51, 1 << 20, ENG);
    let a = info(&drive(&f, 16 * 1024, 6)).audio[0].clone();
    assert_eq!(a.atmos, Some(false));
}

#[test]
fn mov_macintosh_language_codes() {
    // Codes written by ffmpeg's mov muxer and read back by ffprobe (fra, ger, heb, kor, wel),
    // plus eng / dut / jpn seen in the corpus.
    for (code, want) in [
        (0u16, "eng"),
        (1, "fra"),
        (2, "ger"),
        (4, "dut"),
        (10, "heb"),
        (11, "jpn"),
        (23, "kor"),
        (128, "wel"),
    ] {
        let f = mp4_audio(b"ac-3", &[], &[0; 16], 0, code);
        let i = drive(&f, 64 * 1024, 2);
        assert_eq!(
            info(&i).audio[0].language.as_deref(),
            Some(want),
            "code {code}"
        );
    }
    let f = mp4_audio(b"ac-3", &[], &[0; 16], 0, 0x7fff);
    assert_eq!(info(&drive(&f, 64 * 1024, 2)).audio[0].language, None);
}

// ------------------------------------------------------------------ TS

/// TS with TrueHD / E-AC-3 / DTS PIDs, 2000 padding packets, then a last PES 10 s later.
fn premium_ts() -> Vec<u8> {
    let mut cc = 0u8;
    let mut f = ts_packets(0, &pat(0x100), &mut cc);
    let eng = vec![0x0a, 4, b'e', b'n', b'g', 0];
    f.extend(ts_packets(
        0x100,
        &pmt(&[
            (0x83, 0x1100, eng),
            (0x84, 0x1101, vec![]),
            (0x82, 0x1102, vec![]),
        ]),
        &mut cc,
    ));
    f.extend(ts_packets(
        0x1100,
        &pes(0xfd, 90_000, TRUEHD_ATMOS),
        &mut cc,
    ));
    f.extend(ts_packets(0x1101, &pes(0xfd, 90_000, EAC3_JOC), &mut cc));
    f.extend(ts_packets(0x1102, &pes(0xfd, 90_000, DTS_CORE), &mut cc));
    f.extend(vec![0xff; 0]);
    // Padding packets, then a last PES 10 s later for the duration.
    for _ in 0..2000 {
        f.extend(ts_packets(0x1fff, &[0xff; 184], &mut cc));
    }
    f.extend(ts_packets(0x1101, &pes(0xfd, 990_000, EAC3_JOC), &mut cc));
    f
}

#[test]
fn m2ts_style_premium_audio() {
    let f = premium_ts();
    let d = drive(&f, 16 * 1024, 4);
    let i = info(&d);
    assert_eq!(i.container, "ts");
    let thd = audio(i, 0x1100);
    assert_eq!((thd.codec.as_str(), thd.atmos), ("truehd", Some(true)));
    assert_eq!(thd.language.as_deref(), Some("eng"));
    assert_eq!(audio(i, 0x1101).atmos, Some(true));
    assert_eq!(audio(i, 0x1102).profile.as_deref(), Some("DTS"));
    let ms = i.duration_ms.unwrap();
    assert!((10_000..10_100).contains(&ms), "duration {ms}");
}

#[test]
fn no_hdr_claims_without_signalling() {
    let bytes = premium_mkv();
    let i = drive(&bytes, 16 * 1024, 8).recon.info.unwrap();
    assert!(i.video[0].hdr.is_empty());
    assert!(!i.video[0].hdr.contains(&HdrFormat::DolbyVision));
}

/// Review finding: the recon read the SparseFile without the unverified bookkeeping, so a
/// duration that came only from an unverified tail left `flags.unverified` false.
#[test]
fn recon_values_from_unverified_bytes_flag_the_detection() {
    // Tracks after a 200 KB cluster, reached through the SeekHead: the track fields come from
    // a range of their own, far from the head the type check reads.
    let t = tracks(&[track(1, 2, "A_EAC3", &lang("spa"))]);
    let c = cluster(&[
        simple_block(1, EAC3_JOC),
        simple_block(1, &vec![0u8; 200_000]),
    ]);
    let inf = mkv_info(5000.0);
    let seek = |pos: u64| {
        let s = el(
            &[0x4D, 0xBB],
            &[
                el(&[0x53, 0xAB], &[0x16, 0x54, 0xAE, 0x6B]),
                uel(&[0x53, 0xAC], pos),
            ]
            .concat(),
        );
        el(&[0x11, 0x4D, 0x9B, 0x74], &s)
    };
    let head_len = seek(0).len() + inf.len() + c.len();
    let sh = seek(head_len as u64);
    let mut bytes = ebml_header("matroska");
    bytes.extend(segment(&[sh, inf, c, t]));
    let len = bytes.len() as u64;
    let run = |verified: bool| {
        // Head verified; what the detector asks for afterwards comes in unverified (or
        // verified, for the control).
        let mut f = SparseFile::new(len);
        f.insert(0, bytes[..16 * 1024].to_vec());
        for _ in 0..6 {
            let a = Detector::new().analyze(&f);
            if a.need.is_empty() {
                break;
            }
            for r in a.need {
                let s = r.offset as usize;
                let e = (r.offset + r.len).min(len) as usize;
                if verified {
                    f.insert(r.offset, bytes[s..e].to_vec());
                } else {
                    f.insert_unverified(r.offset, bytes[s..e].to_vec());
                }
            }
        }
        assert!(f.ranges().count() >= 2, "the fetched ranges stay apart");
        let Outcome::Detected(d) = Detector::new().finish(&f) else {
            panic!("not detected");
        };
        d
    };
    let d = run(false);
    assert_eq!(d.label, "video.mkv");
    let v = d.video.as_ref().expect("video info");
    assert_eq!(
        v.audio.first().and_then(|a| a.language.as_deref()),
        Some("spa")
    );
    assert!(
        d.flags.unverified,
        "track fields came from unverified bytes"
    );
    assert!(!run(true).flags.unverified);
}

/// Review finding: the type check read the whole `moov` (up to 4 MiB) only to tell `.m4a`
/// from video, ~20x what the recon needs. A large moov is now left to the recon, which walks
/// it by box headers and makes the audio-only call from the track handlers.
#[test]
fn large_moov_is_walked_not_fetched_whole() {
    let f = mp4_audio(b"ec-3", &dec3(None), EAC3_JOC, 3 << 20, ENG);
    let be = |o: usize| u32::from_be_bytes(f[o..o + 4].try_into().unwrap()) as usize;
    let moov_at = be(0) + be(be(0));
    assert_eq!(&f[moov_at + 4..moov_at + 8], b"moov");
    let body = &f[moov_at + 8..];
    let mvhd_len = u32::from_be_bytes(body[..4].try_into().unwrap()) as usize;
    let pad = bx(b"free", &vec![0u8; 1 << 20]);
    let moov = bx(
        b"moov",
        &[&body[..mvhd_len], &pad[..], &body[mvhd_len..]].concat(),
    );
    let file = [&f[..moov_at], &moov[..]].concat();
    let r = sniff::detect_with(
        |r: sniff::Range| -> Result<Vec<u8>, ()> {
            let s = r.offset as usize;
            Ok(file[s..(s + r.len as usize).min(file.len())].to_vec())
        },
        file.len() as u64,
        Some("a.mp4"),
        &sniff::Caps::default(),
    )
    .unwrap();
    let Outcome::Detected(d) = &r.outcome else {
        panic!("{:?}", r.outcome)
    };
    assert_eq!(d.label, "audio.m4a");
    assert_eq!(d.confidence, 97);
    assert_eq!(d.video.as_ref().map(|v| v.audio.len()), Some(1));
    assert!(
        r.bytes_fetched < 256 * 1024,
        "fetched {} bytes",
        r.bytes_fetched
    );
}

// ------------------------------------------------------------------ MP4 `moov` at the end (D120)

/// One `trak` with `mdhd` language `lang` (packed), `hdlr`, a `stsd` holding `entry`, then an
/// `stsz` and an `stco` of `table` bytes each (one sample per 2000 ticks: 24 fps for video).
fn trak_d120(id: u32, handler: &[u8; 4], lang: u16, entry: Vec<u8>, table: usize) -> Vec<u8> {
    let mut tkhd = vec![0u8; 8];
    tkhd.extend_from_slice(&id.to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 68]);
    let n = (table / 4).saturating_sub(3) as u32;
    let mut mdhd = vec![0u8; 8];
    mdhd.extend_from_slice(&48_000u32.to_be_bytes());
    mdhd.extend_from_slice(&(n * 2000).to_be_bytes());
    mdhd.extend_from_slice(&lang.to_be_bytes());
    mdhd.extend_from_slice(&[0, 0]);
    let hdlr = full(
        b"hdlr",
        0,
        0,
        &[&[0u8; 4][..], handler, &[0u8; 13][..]].concat(),
    );
    let stsd = full(b"stsd", 0, 0, &[&1u32.to_be_bytes()[..], &entry].concat());
    let mut stsz = vec![0u8; 4];
    stsz.extend_from_slice(&n.to_be_bytes());
    stsz.resize(table, 0);
    let mut stco = n.to_be_bytes().to_vec();
    stco.resize(table, 0);
    let stbl = bx(
        b"stbl",
        &[stsd, full(b"stsz", 0, 0, &stsz), full(b"stco", 0, 0, &stco)].concat(),
    );
    let mdia = bx(
        b"mdia",
        &[full(b"mdhd", 0, 0, &mdhd), hdlr, bx(b"minf", &stbl)].concat(),
    );
    bx(b"trak", &[full(b"tkhd", 0, 1, &tkhd), mdia].concat())
}

fn avc1() -> Vec<u8> {
    let mut e = vec![0u8; 6];
    e.extend_from_slice(&1u16.to_be_bytes());
    e.extend_from_slice(&[0u8; 16]);
    e.extend_from_slice(&1280u16.to_be_bytes());
    e.extend_from_slice(&720u16.to_be_bytes());
    e.resize(78, 0);
    bx(b"avc1", &e)
}

fn mp4a() -> Vec<u8> {
    let mut e = vec![0u8; 6];
    e.extend_from_slice(&1u16.to_be_bytes());
    e.extend_from_slice(&[0u8; 8]);
    e.extend_from_slice(&2u16.to_be_bytes());
    e.extend_from_slice(&16u16.to_be_bytes());
    e.extend_from_slice(&[0u8; 4]);
    e.extend_from_slice(&(48_000u32 << 16).to_be_bytes());
    bx(b"mp4a", &e)
}

const FRE: u16 = (6 << 10) | (18 << 5) | 5; // packed "fre"
const SPA: u16 = (19 << 10) | (16 << 5) | 1; // packed "spa"

/// ftyp, a 1 MiB `mdat`, then `moov`: H.264 with 400 KiB tables, two AAC tracks with 300 KiB;
/// with the offset of each `trak`.
fn mp4_long_moov() -> (Vec<u8>, Vec<usize>) {
    let ftyp = bx(b"ftyp", b"isom\0\0\x02\0isomavc1mp41");
    let mdat = bx(b"mdat", &vec![0u8; 1 << 20]);
    let mut mvhd = vec![0u8; 8];
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    mvhd.extend_from_slice(&10_000u32.to_be_bytes());
    mvhd.extend_from_slice(&[0u8; 80]);
    let mvhd = full(b"mvhd", 0, 0, &mvhd);
    let traks = [
        trak_d120(1, b"vide", 0x55c4, avc1(), 400 << 10),
        trak_d120(2, b"soun", FRE, mp4a(), 300 << 10),
        trak_d120(3, b"soun", SPA, mp4a(), 300 << 10),
    ];
    let mut at = ftyp.len() + mdat.len() + 8 + mvhd.len();
    let mut starts = Vec::new();
    for t in &traks {
        starts.push(at);
        at += t.len();
    }
    let moov = bx(b"moov", &[mvhd, traks.concat()].concat());
    ([ftyp, mdat, moov].concat(), starts)
}

/// D120: the hops over a track's sample tables (frame rate, first sample) wait until every
/// track is named. With the `moov` header block known, the next pass asks for the second
/// track's header, not also for the video `stco` head 400 KiB further (before: both at once,
/// which with whole pieces cost a piece each). AAC tables are never hopped over (their first
/// frame adds nothing): the whole walk is the head, the `moov` block, one block per audio
/// track and the video `stco` head.
#[test]
fn mp4_track_skeleton_before_sample_tables() {
    let (f, starts) = mp4_long_moov();
    let block = |o: usize| (o as u64) / 16384 * 16384;
    let d = drive(&f, 64 * 1024, 2);
    let need: Vec<u64> = d.recon.need.iter().map(|r| r.offset).collect();
    assert_eq!(need, vec![block(starts[2])], "need {:?}", d.recon.need);
    let d = drive(&f, 64 * 1024, 10);
    let i = info(&d);
    assert!(d.recon.need.is_empty());
    let langs: Vec<_> = i.audio.iter().map(|a| a.language.as_deref()).collect();
    assert_eq!(langs, vec![Some("fre"), Some("spa")]);
    assert_eq!(
        i.video[0].fps,
        Some(24.0),
        "the video tables are still read"
    );
    assert_eq!(d.fetched, 64 * 1024 + 4 * 16384, "ranges {:?}", d.ranges);
}

/// D120: a muxer wrote a wrong `mdat` size, so the box chain breaks inside the media data: the
/// `moov` is looked for in the last 1 MiB (before: "no moov box", no tracks).
#[test]
fn mp4_moov_found_in_the_tail_when_the_mdat_size_is_wrong() {
    let mut f = mp4_audio(b"ec-3", &dec3(Some(true)), EAC3_PLAIN, 3 << 20, ENG);
    let ftyp = u32::from_be_bytes(f[..4].try_into().unwrap()) as usize;
    assert_eq!(&f[ftyp + 4..ftyp + 8], b"mdat");
    f[ftyp..ftyp + 4].copy_from_slice(&4096u32.to_be_bytes());
    // Media data that is no box header where the wrong size points.
    f[ftyp + 4096..ftyp + 4104].copy_from_slice(&[0xff; 8]);
    let d = drive(&f, 64 * 1024, 4);
    let i = info(&d);
    assert_eq!(i.audio.len(), 1, "notes {:?}", i.notes);
    assert_eq!(i.audio[0].language.as_deref(), Some("eng"));
    assert!(d.fetched <= 64 * 1024 + (1 << 20) + 16384, "{}", d.fetched);
    // A file cut short (its `mdat` runs past the end) has no `moov` anywhere: no tail read.
    let mut cut = mp4_audio(b"ec-3", &dec3(Some(true)), EAC3_PLAIN, 3 << 20, ENG);
    cut.truncate(2 << 20);
    let d = drive(&cut, 64 * 1024, 4);
    assert_eq!(d.fetched, 64 * 1024);
    assert!(info(&d).audio.is_empty());
}

/// D120 coverage: a 64-bit `mdat` (`size` 1, `largesize`) of 5 GiB puts the `moov` past
/// 4 GiB; its offset comes from the head alone, one block, no tail scan.
#[test]
fn mp4_largesize_mdat_puts_moov_past_4_gib() {
    let f = mp4_audio(b"ec-3", &dec3(Some(true)), EAC3_PLAIN, 0, ENG);
    let ftyp = u32::from_be_bytes(f[..4].try_into().unwrap()) as usize;
    let mdat_len = u32::from_be_bytes(f[ftyp..ftyp + 4].try_into().unwrap()) as usize;
    let moov = &f[ftyp + mdat_len..];
    let big: u64 = 5 << 30;
    let moov_at = ftyp as u64 + big;
    let total = moov_at + moov.len() as u64;
    let mut head = f[..ftyp].to_vec();
    head.extend_from_slice(&1u32.to_be_bytes());
    head.extend_from_slice(b"mdat");
    head.extend_from_slice(&big.to_be_bytes());
    head.resize(64 * 1024, 0);
    let mut file = SparseFile::new(total);
    file.insert(0, head);
    let caps = sniff::video::recommended_caps();
    let r = sniff::video::analyze(&file, &caps);
    assert_eq!(r.need.len(), 1, "{:?}", r.need);
    let want = r.need[0];
    assert!(want.offset <= moov_at && want.offset + want.len >= moov_at + 16);
    assert!(want.len <= 32 * 1024);
    file.insert(moov_at, moov.to_vec());
    let r = sniff::video::analyze(&file, &caps);
    let i = r.info.expect("info");
    assert_eq!(i.audio[0].language.as_deref(), Some("eng"));
}

// ------------------------------------------------------------------ TS PMT (D120)

/// D120: a PMT with 40 audio streams (each with an ISO 639 descriptor) spans three packets;
/// every stream and language is read (before: 16, the streams of the first packet).
#[test]
fn ts_pmt_over_several_packets() {
    let mut cc = 0u8;
    let mut f = ts_packets(0, &pat(0x100), &mut cc);
    let codes: [&[u8; 3]; 4] = [b"fre", b"eng", b"spa", b"ger"];
    let streams: Vec<(u8, u16, Vec<u8>)> = (0..40u16)
        .map(|i| {
            let l = codes[usize::from(i) % 4];
            (0x0f, 0x101 + i, vec![0x0a, 4, l[0], l[1], l[2], 0])
        })
        .collect();
    let table = pmt(&streams);
    assert!(table.len() > 2 * 184);
    f.extend(ts_packets(0x100, &table, &mut cc));
    for _ in 0..50 {
        f.extend(ts_packets(0x1fff, &[0xff; 184], &mut cc));
    }
    let d = drive(&f, 64 * 1024, 2);
    let i = info(&d);
    assert_eq!(i.audio.len(), 40);
    assert!(i.audio.iter().all(|a| a.language.is_some()));
    assert_eq!(audio(i, 0x101 + 39).language.as_deref(), Some("ger"));
}

/// 40 audio streams, each with an ISO 639 descriptor: a PMT of more than two packets.
fn big_pmt() -> Vec<u8> {
    let codes: [&[u8; 3]; 4] = [b"fre", b"eng", b"spa", b"ger"];
    let streams: Vec<(u8, u16, Vec<u8>)> = (0..40u16)
        .map(|i| {
            let l = codes[usize::from(i) % 4];
            (0x0f, 0x101 + i, vec![0x0a, 4, l[0], l[1], l[2], 0])
        })
        .collect();
    pmt(&streams)
}

fn assert_40_languages(i: &VideoInfo) {
    let codes = ["fre", "eng", "spa", "ger"];
    assert_eq!(i.audio.len(), 40, "{:?}", i.audio);
    for n in 0..40u64 {
        let a = audio(i, 0x101 + n);
        assert_eq!(a.language.as_deref(), Some(codes[n as usize % 4]), "{a:?}");
    }
}

/// D120 review: packed PSI. Each repetition of the PMT starts in the packet where the previous
/// one ends (PUSI with a non-zero pointer: the bytes before it end the section in progress),
/// and the head stops inside the last one. Before, every PUSI dropped those tail bytes, no
/// section was ever whole and only the first packet's streams were read ("PMT cut short").
#[test]
fn ts_pmt_packed_back_to_back() {
    let sct = big_pmt()[1..].to_vec(); // without psi()'s pointer field
    let mut stream = Vec::new();
    let mut starts = Vec::new();
    for _ in 0..3 {
        starts.push(stream.len());
        stream.extend_from_slice(&sct);
    }
    stream.truncate(starts[2] + 200);
    let mut cc = 0u8;
    let mut f = ts_packets(0, &pat(0x100), &mut cc);
    let mut pos = 0;
    while pos < stream.len() {
        let start = starts.iter().copied().find(|&s| s >= pos && s < pos + 183);
        assert!(start.is_some() || !starts.contains(&(pos + 183)));
        let mut p = vec![
            0x47,
            0x01 | if start.is_some() { 0x40 } else { 0 },
            0x00,
            0x10 | (cc & 0xf),
        ];
        let take = if let Some(s) = start {
            p.push((s - pos) as u8);
            183
        } else {
            184
        };
        p.extend_from_slice(&stream[pos..(pos + take).min(stream.len())]);
        p.resize(188, 0xff);
        f.extend(p);
        cc = cc.wrapping_add(1);
        pos += take;
    }
    // The first repetition must end inside a PUSI packet for this to test anything.
    assert_ne!((starts[1] + 1) % 184, 0);
    for _ in 0..50 {
        f.extend(ts_packets(0x1fff, &[0xff; 184], &mut cc));
    }
    let d = drive(&f, 64 * 1024, 2);
    assert_40_languages(info(&d));
}

/// D120 review: a duplicated PMT packet (same continuity counter, allowed once by ISO 13818-1)
/// is not appended twice. Before, its bytes went into the section a second time and the
/// streams after it were read from the wrong offsets.
#[test]
fn ts_pmt_duplicate_packet_is_skipped() {
    let mut cc = 0u8;
    let mut f = ts_packets(0, &pat(0x100), &mut cc);
    let pm = ts_packets(0x100, &big_pmt(), &mut cc);
    assert!(pm.len() >= 3 * 188);
    f.extend_from_slice(&pm[..2 * 188]);
    f.extend_from_slice(&pm[188..2 * 188]); // the second packet again
    f.extend_from_slice(&pm[2 * 188..]);
    for _ in 0..50 {
        f.extend(ts_packets(0x1fff, &[0xff; 184], &mut cc));
    }
    let d = drive(&f, 64 * 1024, 2);
    assert_40_languages(info(&d));
}
