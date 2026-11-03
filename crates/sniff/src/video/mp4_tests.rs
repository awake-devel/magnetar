//! `mp4::tests`: track kinds and names (D123).

use super::super::testkit::{audio_entry, bx, entry, full, info, mp4, trak};
use super::super::{TrackKind, VideoInfo};

fn kinds(k: &[TrackKind]) -> Vec<&'static str> {
    k.iter().map(|k| k.as_str()).collect()
}

fn sub_kinds(i: &VideoInfo, id: u64) -> Vec<&'static str> {
    kinds(&i.subtitles.iter().find(|s| s.id == id).expect("sub").kinds)
}

fn audio_kinds(i: &VideoInfo, id: u64) -> Vec<&'static str> {
    kinds(&i.audio.iter().find(|a| a.id == id).expect("audio").kinds)
}

fn tx3g(flags: u32) -> Vec<u8> {
    let mut b = flags.to_be_bytes().to_vec();
    b.extend_from_slice(&[0u8; 30]);
    entry(b"tx3g", &b)
}

fn kind_box(scheme: &str, value: &str) -> Vec<u8> {
    full(b"kind", 0, 0, format!("{scheme}\0{value}\0").as_bytes())
}

fn aac() -> Vec<u8> {
    audio_entry(b"mp4a", &[])
}

#[test]
fn udta_name_tagc_kind_and_tref() {
    let dash = "urn:mpeg:dash:role:2011";
    let f = mp4(&[
        // ffmpeg's `name` layout: the whole body is the text.
        trak(1, b"soun", &aac(), &[], &bx(b"name", b"Commentary")),
        trak(
            2,
            b"sbtl",
            &tx3g(0),
            &[],
            &[
                bx(b"tagc", b"public.accessibility.transcribes-spoken-dialog"),
                bx(b"tagc", b"public.accessibility.describes-music-and-sound"),
            ]
            .concat(),
        ),
        trak(
            3,
            b"soun",
            &aac(),
            &[],
            &bx(b"tagc", b"public.accessibility.describes-video"),
        ),
        trak(
            4,
            b"subt",
            &tx3g(0),
            &[],
            &kind_box(dash, "forced-subtitle"),
        ),
        // Track 6 holds the forced subtitles of track 5.
        trak(5, b"sbtl", &tx3g(0), &bx(b"forc", &6u32.to_be_bytes()), &[]),
        trak(6, b"sbtl", &tx3g(0), &[], &[]),
        // QuickTime international text (length, language), and a `tnam` that `name` wins over.
        trak(
            7,
            b"soun",
            &aac(),
            &[],
            &[
                bx(b"tnam", b"Dubbed"),
                bx(
                    b"name",
                    &[&[0, 12, 0x15, 0xc7][..], b"Audio Descr."].concat(),
                ),
            ]
            .concat(),
        ),
        trak(
            8,
            b"soun",
            &aac(),
            &[],
            &kind_box("about:html-kind", "main-desc"),
        ),
        // An ilst-style `data` atom.
        trak(
            9,
            b"soun",
            &aac(),
            &[],
            &bx(
                b"name",
                &bx(b"data", &[&[0u8; 8][..], b"Isolated Score"].concat()),
            ),
        ),
        // `transcribes-spoken-dialog` alone says nothing.
        trak(
            10,
            b"sbtl",
            &tx3g(0),
            &[],
            &bx(b"tagc", b"public.accessibility.transcribes-spoken-dialog"),
        ),
    ]);
    let i = info(&f);
    assert_eq!(audio_kinds(&i, 1), ["commentary"]);
    let a1 = i.audio.iter().find(|a| a.id == 1).unwrap();
    assert_eq!(a1.name.as_deref(), Some("Commentary"));
    assert_eq!(kinds(&a1.name_kinds), ["commentary"]);
    assert_eq!(sub_kinds(&i, 2), ["sdh"]);
    assert_eq!(audio_kinds(&i, 3), ["description"]);
    assert_eq!(sub_kinds(&i, 4), ["forced"]);
    assert!(sub_kinds(&i, 5).is_empty());
    assert_eq!(sub_kinds(&i, 6), ["forced"]);
    let a7 = i.audio.iter().find(|a| a.id == 7).unwrap();
    assert_eq!(a7.name.as_deref(), Some("Audio Descr."));
    assert_eq!(audio_kinds(&i, 8), ["description"]);
    assert_eq!(audio_kinds(&i, 9), ["music"]);
    assert!(sub_kinds(&i, 10).is_empty());
}

/// ffmpeg 9.0.2 output (`tests/video_fixtures/ffmpeg_kinds.mp4`, made locally): an AAC track
/// titled "Commentary" (written as `udta/name`), an AAC track with the visual_impaired
/// disposition (`kind` description), a mov_text track titled "Français" with the forced
/// disposition (`kind` forced-subtitle) and one with hearing_impaired (`kind` caption).
#[test]
fn ffmpeg_udta_name_and_kind_boxes() {
    let f = include_bytes!("../../tests/video_fixtures/ffmpeg_kinds.mp4");
    let i = info(f);
    assert_eq!(i.audio.len(), 2);
    assert_eq!(i.audio[0].name.as_deref(), Some("Commentary"));
    assert_eq!(kinds(&i.audio[0].kinds), ["commentary"]);
    assert_eq!(kinds(&i.audio[1].kinds), ["description"]);
    assert_eq!(i.subtitles.len(), 2);
    assert_eq!(i.subtitles[0].name.as_deref(), Some("Français"));
    assert_eq!(kinds(&i.subtitles[0].kinds), ["forced"]);
    assert_eq!(kinds(&i.subtitles[1].kinds), ["sdh"]);
    // ffmpeg's hdlr names ("SoundHandler", "SubtitleHandler") are not track names.
    assert_eq!(i.audio[1].name, None);
    assert_eq!(i.subtitles[1].name, None);
}

#[test]
fn tx3g_display_flags() {
    let f = mp4(&[
        trak(1, b"sbtl", &tx3g(0x8000_0000), &[], &[]),
        trak(2, b"sbtl", &tx3g(0x4000_0000), &[], &[]),
    ]);
    let i = info(&f);
    assert_eq!(sub_kinds(&i, 1), ["forced"]);
    assert!(sub_kinds(&i, 2).is_empty());
}

#[test]
fn dac3_dec3_service() {
    // dac3: fscod 0, bsid 8, bsmod 2 (visually impaired), acmod 2, lfe 0, bit_rate_code 10.
    let v: u32 = (8 << 17) | (2 << 14) | (2 << 11) | (10 << 5);
    let dac3 = bx(b"dac3", &v.to_be_bytes()[1..]);
    // dec3: data_rate, num_ind_sub 0; then fscod 0, bsid 16, reserved, asvc 0, bsmod 5
    // (commentary), acmod 7, lfe 1, reserved 3, num_dep_sub 0, reserved 1.
    let mut bits: u64 = 0;
    let mut n = 0;
    for (w, x) in [
        (13, 640),
        (3, 0),
        (2, 0),
        (5, 16),
        (1, 0),
        (1, 0),
        (3, 5),
        (3, 7),
        (1, 1),
        (3, 0),
        (4, 0),
        (1, 0),
    ] {
        bits = (bits << w) | x;
        n += w;
    }
    assert_eq!(n, 40);
    let dec3 = bx(b"dec3", &bits.to_be_bytes()[3..]);
    let f = mp4(&[
        trak(1, b"soun", &audio_entry(b"ac-3", &dac3), &[], &[]),
        trak(2, b"soun", &audio_entry(b"ec-3", &dec3), &[], &[]),
    ]);
    let i = info(&f);
    assert_eq!(audio_kinds(&i, 1), ["description"]);
    assert_eq!(audio_kinds(&i, 2), ["commentary"]);
    assert_eq!(i.audio[1].channels, Some(6));
}

#[test]
fn c708_dfxp_subp_codecs() {
    let f = mp4(&[
        trak(1, b"clcp", &entry(b"c708", &[]), &[], &[]),
        trak(2, b"subt", &entry(b"dfxp", &[]), &[], &[]),
        trak(3, b"subp", &entry(b"mp4s", &[]), &[], &[]),
        trak(4, b"clcp", &entry(b"c608", &[]), &[], &[]),
    ]);
    let i = info(&f);
    let codecs: Vec<&str> = i.subtitles.iter().map(|s| s.codec.as_str()).collect();
    assert_eq!(codecs, ["eia_708", "ttml", "dvd_subtitle", "eia_608"]);
}

#[test]
fn hdlr_name_is_a_fallback_without_boilerplate() {
    let h = |name: &[u8]| super::hdlr_name(&[&[0u8; 24][..], name].concat());
    assert_eq!(h(b"SoundHandler\0"), None);
    assert_eq!(h(b"GPAC ISO Audio Handler\0"), None);
    assert_eq!(h(b"Core Media Audio\0"), None);
    let soun = [&[0u8; 8][..], b"soun", &[0; 12], b"soun\0"].concat();
    assert_eq!(super::hdlr_name(&soun), None);
    // QuickTime Pascal string.
    assert_eq!(h(b"\x0aCommentary").as_deref(), Some("Commentary"));
    assert_eq!(
        h(b"Director Commentary\0").as_deref(),
        Some("Director Commentary")
    );
    // The udta name wins; the hdlr name is the fallback.
    let mut hd = vec![0u8; 4];
    hd.extend_from_slice(b"soun");
    hd.extend_from_slice(&[0; 12]);
    hd.extend_from_slice(b"Commentary\0");
    let f = mp4(&[super::super::testkit::trak_with_hdlr(
        1,
        &aac(),
        &full(b"hdlr", 0, 0, &hd),
    )]);
    let i = info(&f);
    assert_eq!(i.audio[0].name.as_deref(), Some("Commentary"));
    assert_eq!(kinds(&i.audio[0].kinds), ["commentary"]);
}
