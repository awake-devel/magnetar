//! `ts::tests`: track kinds from the PMT descriptors and the buffered streams (D123).

use super::super::testkit::{desc, info, pes, ts};
use super::super::{SubtitleTrack, TrackKind, VideoInfo};

fn kinds(k: &[TrackKind]) -> Vec<&'static str> {
    k.iter().map(|k| k.as_str()).collect()
}

fn subs(i: &VideoInfo, pid: u64) -> Vec<&SubtitleTrack> {
    i.subtitles.iter().filter(|s| s.id == pid).collect()
}

fn audio_kinds(i: &VideoInfo, pid: u64) -> Vec<&'static str> {
    kinds(&i.audio.iter().find(|a| a.id == pid).expect("audio").kinds)
}

#[test]
fn pmt_kind_descriptors() {
    let f = ts(
        &[
            // ISO 639 with audio_type 3 (visual impaired commentary).
            (0x0f, 0x101, desc(0x0a, b"eng\x03")),
            // DVB subtitling: English normal, French for the hard of hearing.
            (
                0x06,
                0x102,
                desc(
                    0x59,
                    &[&b"eng\x10\x00\x01\x00\x01"[..], b"fre\x20\x00\x02\x00\x02"].concat(),
                ),
            ),
            // Teletext: French subtitles, French HoH subtitles, English initial page.
            (
                0x06,
                0x103,
                desc(
                    0x56,
                    &[
                        &b"fre"[..],
                        &[(2 << 3) | 1, 0x88],
                        b"fre",
                        &[(5 << 3) | 1, 0x89],
                        b"eng",
                        &[(1 << 3) | 1, 0x00],
                    ]
                    .concat(),
                ),
            ),
            // Supplementary audio: editorial classification 1 (audio description), language.
            (
                0x06,
                0x104,
                [
                    desc(0x6a, &[0x00]),
                    desc(0x7f, &[0x06, (1 << 2) | 1, b's', b'p', b'a']),
                ]
                .concat(),
            ),
            // DVB AC-3 descriptor with component_type service 5 (commentary), 2 channels.
            (0x06, 0x105, desc(0x6a, &[0x80, (5 << 3) | 2])),
            // ATSC caption_service_descriptor on the video PID: one analog (608) English
            // service.
            (
                0x1b,
                0x100 + 0x10,
                desc(0x86, &[0xe1, b'e', b'n', b'g', 0x41, 0x3f, 0xff]),
            ),
            // ISO 639 audio_type 0 (undefined) gives nothing.
            (0x0f, 0x106, desc(0x0a, b"ger\x00")),
        ],
        &[],
    );
    let i = info(&f);
    assert_eq!(audio_kinds(&i, 0x101), ["description"]);
    let dvb = subs(&i, 0x102);
    assert_eq!(dvb.len(), 2, "{dvb:?}");
    assert_eq!(dvb[0].language.as_deref(), Some("eng"));
    assert!(dvb[0].kinds.is_empty());
    assert_eq!(dvb[1].language.as_deref(), Some("fre"));
    assert_eq!(kinds(&dvb[1].kinds), ["sdh"]);
    let ttx = subs(&i, 0x103);
    assert_eq!(ttx.len(), 2, "{ttx:?}");
    assert!(ttx.iter().all(|s| s.codec == "dvb_teletext"));
    assert!(ttx.iter().all(|s| s.language.as_deref() == Some("fre")));
    assert!(ttx[0].kinds.is_empty());
    assert_eq!(kinds(&ttx[1].kinds), ["sdh"]);
    assert_eq!(audio_kinds(&i, 0x104), ["description"]);
    let ad = i.audio.iter().find(|a| a.id == 0x104).unwrap();
    assert_eq!(ad.language.as_deref(), Some("spa"));
    assert_eq!(audio_kinds(&i, 0x105), ["commentary"]);
    let cc = subs(&i, 0x110);
    assert_eq!(cc.len(), 1, "{cc:?}");
    assert_eq!(cc[0].codec, "eia_608");
    assert_eq!(cc[0].language.as_deref(), Some("eng"));
    assert_eq!(kinds(&cc[0].kinds), ["cc"]);
    assert!(audio_kinds(&i, 0x106).is_empty());
}

#[test]
fn teletext_without_subtitle_pages_keeps_one_track() {
    let f = ts(
        &[(
            0x06,
            0x101,
            desc(0x56, &[&b"eng"[..], &[(1 << 3) | 1, 0]].concat()),
        )],
        &[],
    );
    let i = info(&f);
    let t = subs(&i, 0x101);
    assert_eq!(t.len(), 1);
    assert_eq!(
        (t[0].codec.as_str(), t[0].language.as_deref()),
        ("dvb_teletext", None)
    );
}

/// An AC-3 sync frame header: 48 kHz, 64 kb/s, bsid 8, `bsmod`, `acmod`.
fn ac3_frame(bsmod: u8, acmod: u8) -> Vec<u8> {
    let mut f = vec![0x0b, 0x77, 0, 0, 0x08, (8 << 3) | bsmod, acmod << 5, 0];
    f.resize(256, 0);
    f
}

#[test]
fn ac3_bsmod_in_ts() {
    let f = ts(
        &[
            (0x81, 0x101, Vec::new()),
            (0x81, 0x102, Vec::new()),
            (0x81, 0x103, Vec::new()),
            (0x81, 0x104, Vec::new()),
            // ATSC AC-3 audio descriptor: bsmod 3 (dialogue) in the third byte.
            (0x81, 0x105, desc(0x81, &[0x48, 0x00, 3 << 5])),
            // bsmod 7 with num_channels 1 (1/0, mono): a voice-over; with 7 (3/2): karaoke.
            (0x81, 0x106, desc(0x81, &[0x48, 0x00, (7 << 5) | (1 << 1)])),
            (0x81, 0x107, desc(0x81, &[0x48, 0x00, (7 << 5) | (7 << 1)])),
        ],
        &[
            (0x101, pes(0xbd, 90_000, &ac3_frame(2, 2))),
            (0x102, pes(0xbd, 90_000, &ac3_frame(7, 1))),
            (0x103, pes(0xbd, 90_000, &ac3_frame(7, 7))),
            (0x104, pes(0xbd, 90_000, &ac3_frame(0, 7))),
        ],
    );
    let i = info(&f);
    assert_eq!(audio_kinds(&i, 0x101), ["description"]);
    assert_eq!(audio_kinds(&i, 0x102), ["voiceover"]);
    assert_eq!(audio_kinds(&i, 0x103), ["karaoke"]);
    assert!(audio_kinds(&i, 0x104).is_empty());
    assert_eq!(audio_kinds(&i, 0x105), ["dialogue"]);
    assert_eq!(audio_kinds(&i, 0x106), ["voiceover"]);
    assert_eq!(audio_kinds(&i, 0x107), ["karaoke"]);
}

/// VBI teletext (0x46) without a subtitle page is data, not a subtitle track; with one it is
/// (review of D123).
#[test]
fn vbi_teletext_is_a_subtitle_only_with_subtitle_pages() {
    let f = ts(
        &[
            (
                0x06,
                0x101,
                desc(0x46, &[&b"eng"[..], &[(1 << 3) | 1, 0]].concat()),
            ),
            (
                0x06,
                0x102,
                desc(0x46, &[&b"ger"[..], &[(2 << 3) | 1, 0x88]].concat()),
            ),
        ],
        &[],
    );
    let i = info(&f);
    assert!(subs(&i, 0x101).is_empty(), "{:?}", i.subtitles);
    let t = subs(&i, 0x102);
    assert_eq!(t.len(), 1);
    assert_eq!(
        (t[0].codec.as_str(), t[0].language.as_deref()),
        ("dvb_teletext", Some("ger"))
    );
}

/// An H.264 access unit: AUD, SEI with GA94 cc_data (one valid 608 pair), IDR slice.
pub(crate) fn h264_au_with_captions() -> Vec<u8> {
    let mut sei = vec![0x06, 0x04];
    let pl = [
        &[0xb5, 0x00, 0x31][..],
        b"GA94",
        &[0x03, 0x42, 0xff, 0xfc, 0x94, 0x2c, 0xf9, 0x80, 0x80],
    ]
    .concat();
    sei.push(pl.len() as u8);
    sei.extend_from_slice(&pl);
    sei.push(0x80);
    [
        &[0, 0, 0, 1, 0x09, 0xf0][..],
        &[0, 0, 0, 1],
        &sei,
        &[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00],
    ]
    .concat()
}

#[test]
fn h264_captions_become_a_subtitle_track() {
    let f = ts(
        &[(0x1b, 0x100 + 0x11, Vec::new())],
        &[(0x111, pes(0xe0, 90_000, &h264_au_with_captions()))],
    );
    let i = info(&f);
    let cc = subs(&i, 0x111);
    assert_eq!(cc.len(), 1, "{:?}", i.subtitles);
    assert_eq!(cc[0].codec, "eia_608");
    assert_eq!(kinds(&cc[0].kinds), ["cc"]);
    // A caption service the PMT lists is not repeated.
    let f = ts(
        &[(
            0x1b,
            0x111,
            desc(0x86, &[0xe1, b'f', b'r', b'e', 0x41, 0x3f, 0xff]),
        )],
        &[(0x111, pes(0xe0, 90_000, &h264_au_with_captions()))],
    );
    let i = info(&f);
    let cc = subs(&i, 0x111);
    assert_eq!(cc.len(), 1, "{:?}", i.subtitles);
    assert_eq!(cc[0].language.as_deref(), Some("fre"));
}

#[test]
fn mpeg2_user_data_captions() {
    let es = [
        &[0, 0, 1, 0xb3, 0x2d, 0x01, 0xe0, 0x24][..],
        &[0, 0, 1, 0x00, 0, 0],
        &[0, 0, 1, 0xb2],
        b"GA94",
        &[0x03, 0x41, 0xff, 0xff, 0x02, 0x21],
        &[0, 0, 1, 0x01, 0, 0],
    ]
    .concat();
    let f = ts(
        &[(0x02, 0x112, Vec::new())],
        &[(0x112, pes(0xe0, 90_000, &es))],
    );
    let i = info(&f);
    let cc = subs(&i, 0x112);
    assert_eq!(cc.len(), 1, "{:?}", i.subtitles);
    assert_eq!(cc[0].codec, "eia_708");
}
