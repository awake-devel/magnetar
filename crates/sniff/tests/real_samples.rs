//! Real samples under `~/sniff-samples` (or `$SNIFF_SAMPLES`): freely licensed demo media,
//! codec test files and console homebrew, listed with their sources and ground truth in
//! `SOURCES.tsv` there. Each test reads only the ranges the detector asks for and skips with a
//! note when its file is missing (any other machine).

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;

use sniff::video::{AudioTrack, HdrFormat, VideoTrack};
use sniff::{Caps, Content, Detection, Outcome, Platform, Range, detect_with};

fn root() -> PathBuf {
    std::env::var_os("SNIFF_SAMPLES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/dude/sniff-samples"))
}

fn run(rel: &str, caps: &Caps) -> Option<Detection> {
    let p = root().join(rel);
    if !p.is_file() {
        eprintln!("note: {} is missing; skipping", p.display());
        return None;
    }
    let f = File::open(&p).ok()?;
    let len = f.metadata().ok()?.len();
    let name = p.file_name().and_then(|n| n.to_str()).map(str::to_owned);
    let read = |r: Range| -> std::io::Result<Vec<u8>> {
        let mut b = vec![0u8; r.len as usize];
        f.read_exact_at(&mut b, r.offset)?;
        Ok(b)
    };
    let r = detect_with(read, len, name.as_deref(), caps).unwrap();
    match r.outcome {
        Outcome::Detected(d) => Some(d),
        o => panic!("{rel}: {o:?}"),
    }
}

fn video(rel: &str) -> Option<Detection> {
    run(rel, &sniff::video::recommended_caps())
}

fn file(rel: &str) -> Option<Detection> {
    run(rel, &Caps::default())
}

fn audio(d: &Detection) -> &[AudioTrack] {
    &d.video.as_ref().expect("recon").audio
}

fn vtrack(d: &Detection) -> &VideoTrack {
    &d.video.as_ref().expect("recon").video[0]
}

// ---------------------------------------------------------------- video

/// Blu-ray style E-AC-3: an AC-3 core frame (bsid 6) followed by an E-AC-3 dependent frame
/// carrying the 7.1 channel map and the JOC flag.
#[test]
fn eac3_with_an_ac3_core_and_a_dependent_substream() {
    if let Some(d) = video("video/demos/DDP_7.1_Channel_Check.mkv") {
        let a = &audio(&d)[0];
        assert_eq!(
            (a.codec.as_str(), a.channels, a.layout.as_deref(), a.atmos),
            ("eac3", Some(8), Some("7.1"), Some(false))
        );
    }
    for rel in [
        "video/demos/Dolby_Amaze_ATMOS.m2ts",
        "video/demos/Dolby_ATMOS_Helicopter.m2ts",
    ] {
        let Some(d) = video(rel) else { continue };
        let a = audio(&d)
            .iter()
            .find(|a| a.id == 0x1101)
            .expect("PID 0x1101");
        assert_eq!(
            (a.codec.as_str(), a.channels, a.atmos, a.profile.as_deref()),
            (
                "eac3",
                Some(8),
                Some(true),
                Some("Dolby Digital Plus + Dolby Atmos")
            ),
            "{rel}"
        );
        let t = audio(&d)
            .iter()
            .find(|a| a.id == 0x1100)
            .expect("PID 0x1100");
        assert_eq!((t.codec.as_str(), t.atmos), ("truehd", Some(true)));
    }
    for rel in [
        "video/local/remux_amaze_eac3joc.mp4",
        "video/local/remux_amaze_truehd_eac3.mkv",
    ] {
        let Some(d) = video(rel) else { continue };
        let a = audio(&d).iter().find(|a| a.codec == "eac3").expect(rel);
        assert_eq!((a.channels, a.atmos), (Some(8), Some(true)), "{rel}");
    }
}

/// dvhe with an empty hvcC: the in-band SPS gives the cropped height, colour and HDR10.
#[test]
fn hevc_in_band_sps_after_a_container_size() {
    let Some(d) = video("video/demos/DV8_TEST.mp4") else {
        return;
    };
    let v = vtrack(&d);
    assert_eq!(
        (v.width, v.height, v.primaries, v.transfer, v.matrix),
        (Some(3840), Some(2014), Some(9), Some(16), Some(9))
    );
    assert_eq!(v.hdr, vec![HdrFormat::Hdr10, HdrFormat::DolbyVision]);
}

#[test]
fn vp9_hdr10_plus_in_block_additions() {
    let Some(d) = video("video/webmproject/HDR10plus_Sample_Video.webm") else {
        return;
    };
    assert_eq!(vtrack(&d).hdr, vec![HdrFormat::Hdr10, HdrFormat::Hdr10Plus]);
}

#[test]
fn mp4_dts_uhd_is_not_dca() {
    let Some(d) = video("video/demos/DTS-UHD_DTSX_P2.mp4") else {
        return;
    };
    let a = &audio(&d)[0];
    assert_eq!(
        (a.codec.as_str(), a.dts_x, a.profile.as_deref(), a.channels),
        ("dtsuhd", Some(true), Some("DTS:X Profile 2"), Some(10))
    );
}

#[test]
fn cenc_sample_entries_give_the_original_format() {
    for (rel, codec) in [
        ("video/chromium-cenc/bear-640x360-v_frag-cenc.mp4", "h264"),
        ("video/chromium-cenc/bear-640x360-v_frag-cbcs.mp4", "h264"),
        (
            "video/chromium-cenc/bear-320x240-v-vp9_profile2_subsample_cenc-v.mp4",
            "vp9",
        ),
        ("video/chromium-cenc/bear-av1-cenc.mp4", "av1"),
    ] {
        let Some(d) = video(rel) else { continue };
        assert_eq!(vtrack(&d).codec, codec, "{rel}");
    }
}

/// test4.mkv as mirrored: 134 bytes of 0x0A after the unknown-size Segment header.
#[test]
fn matroska_resyncs_past_junk_between_top_level_elements() {
    let Some(d) = video("video/matroska-testsuite/test4.mkv") else {
        return;
    };
    let v = d.video.unwrap();
    assert_eq!(v.video.first().map(|t| t.codec.as_str()), Some("theora"));
    assert_eq!(v.audio.first().map(|t| t.codec.as_str()), Some("vorbis"));
}

#[test]
fn mpeg2_video_in_ts() {
    let Some(d) = video("video/chromium/bear.m2ts") else {
        return;
    };
    let v = vtrack(&d);
    assert_eq!(
        (
            v.codec.as_str(),
            v.profile.as_deref(),
            v.width,
            v.height,
            v.bit_depth,
            v.chroma.as_deref()
        ),
        (
            "mpeg2video",
            Some("Main"),
            Some(320),
            Some(192),
            Some(8),
            Some("4:2:0")
        )
    );
    assert!((v.fps.unwrap() - 29.97).abs() < 0.01);
}

#[test]
fn audio_only_containers_and_raw_streams_get_the_audio_recon() {
    for (rel, label, codec, atmos, dts_x) in [
        (
            "video/local/audio_amaze.thd",
            "audio.truehd",
            "truehd",
            Some(true),
            None,
        ),
        (
            "video/local/audio_dtsx.dts",
            "audio.dts",
            "dts",
            Some(false),
            Some(true),
        ),
        (
            "video/local/audio_dtsx.mka",
            "audio.mka",
            "dts",
            Some(false),
            Some(true),
        ),
        (
            "video/chromium/bear-eac3-only-frag.mp4",
            "audio.m4a",
            "eac3",
            Some(false),
            None,
        ),
    ] {
        let Some(d) = file(rel) else { continue };
        assert_eq!(d.label, label, "{rel}");
        assert!(d.confidence >= 80, "{rel}: {}", d.confidence);
        let a = &audio(&d)[0];
        assert_eq!(
            (a.codec.as_str(), a.atmos, a.dts_x),
            (codec, atmos, dts_x),
            "{rel}"
        );
    }
    // AC-4 can carry Atmos (A-JOC): unknown, not false, while dac4 is not parsed.
    if let Some(d) = file("video/chromium/ac4-only-ajoc-frag.mp4") {
        assert_eq!(audio(&d)[0].atmos, None);
    }
}

#[test]
fn small_video_fields() {
    // Matroska BitsPerChannel 1 (Chromium's colour test): VP8 is 8-bit.
    if let Some(d) = video("video/chromium/colour.webm") {
        assert_eq!(vtrack(&d).bit_depth, Some(8));
    }
    // Container rate over VUI timing / a single jittery sample duration.
    for (rel, fps) in [
        (
            "video/chromium/glass-blowing2-dolby-vision-profile-5-frag.mp4",
            60.0,
        ),
        ("video/chromium/bear-av1-320x180-10bit.mp4", 29.97),
    ] {
        let Some(d) = video(rel) else { continue };
        let f = vtrack(&d).fps.unwrap();
        assert!((f - fps).abs() < 0.05, "{rel}: {f}");
    }
    // AC-3 channels from the container only: ffmpeg's default layout.
    if let Some(d) = video("video/demos/THX_Deep_Note_Genesis.4K.mkv") {
        let a = audio(&d).iter().find(|a| a.codec == "ac3").unwrap();
        assert_eq!(a.layout.as_deref(), Some("5.1"));
    }
    // VfW FOURCCs and the ProRes frame header.
    if let Some(d) = video("video/local/opendml_2g.avi") {
        let v = vtrack(&d);
        assert_eq!(
            (v.codec.as_str(), v.bit_depth, v.chroma.as_deref()),
            ("rawvideo", Some(8), Some("4:2:0"))
        );
    }
    if let Some(d) = video("video/matroska-testsuite/test1.mkv") {
        assert_eq!(vtrack(&d).codec, "msmpeg4v2");
    }
    if let Some(d) = video("video/partial/strobe_scientist.mkv") {
        let v = vtrack(&d);
        assert_eq!(
            (v.codec.as_str(), v.bit_depth, v.chroma.as_deref()),
            ("prores", Some(10), Some("4:2:2"))
        );
    }
}

// ---------------------------------------------------------------- installers and consoles

#[test]
fn inno_setup_7() {
    if let Some(d) = file("installers/innosetup-7.1.1-x64.exe") {
        assert_eq!(d.label, "exe.pe.inno");
    }
}

#[test]
fn dols() {
    for rel in [
        "gc/swiss.x/swiss_r2119/DOL/swiss_r2119.dol",
        "gc/swiss.x/swiss_r2119/DOL/Legacy/swiss_r2119-lz-viper.dol",
        "gc/swiss.x/swiss_r2119/ActionReplay/AUTOEXEC.DOL",
        "wii/Snes9xGX-Wii.x/apps/snes9xgx/boot.dol",
        "wii/wiiflow_v5.6.2.x/apps/wiiflow/boot.dol",
        "wii/usbloadergx_r1283.x/apps/usbloader_gx/boot.dol",
    ] {
        let Some(d) = file(rel) else { continue };
        assert_eq!((d.label.as_str(), d.confidence), ("exe.dol", 95), "{rel}");
    }
}

#[test]
fn vita_vpk_and_self() {
    for (rel, id, ver) in [
        ("vita/VitaShell.vpk", "VITASHELL", "02.02"),
        ("vita/VITAlbum.vpk", "VITALBUM0", "01.40"),
    ] {
        let Some(d) = file(rel) else { continue };
        assert_eq!(d.label, "game.vita.vpk", "{rel}");
        let g = d.game.unwrap();
        assert_eq!(
            (g.platform, g.title_id.as_deref(), g.version.as_deref()),
            (Platform::PsVita, Some(id), Some(ver))
        );
    }
    if let Some(d) = file("vita/VitaShell.x/eboot.bin") {
        assert_eq!(d.label, "exe.vita.self");
    }
}

#[test]
fn homebrew_cartridge_headers() {
    for rel in ["snes/BANKHiROMFastROM.sfc", "snes/BANKLoROMSlowROM.sfc"] {
        let Some(d) = file(rel) else { continue };
        assert_eq!(d.label, "game.snes.rom", "{rel}");
        assert!(d.confidence >= 80, "{rel}");
    }
    if let Some(d) = file("md/sgdk.x/sample/megawifi/basic/out/rom.bin") {
        assert_eq!(d.label, "game.genesis.rom");
    }
    if let Some(d) = file("n64/Julia32BPP320X240.N64") {
        assert_eq!(
            d.game.unwrap().title.as_deref(),
            Some("FRACTAL JULIA ANIM 3")
        );
    }
    for rel in [
        "nds/hbmenu-0.11.0.x/hbmenu/nds/argvTest.nds",
        "nds/hbmenu-0.11.0.x/hbmenu/akmenu4.nds",
    ] {
        let Some(d) = file(rel) else { continue };
        assert_eq!(d.label, "game.nds.rom", "{rel}");
    }
}

#[test]
fn nintendo_ids_and_regions() {
    for (rel, region) in [
        (
            "gc/swiss.x/swiss_r2119/ISO/swiss_r2119(ntsc-j).iso",
            "Japan",
        ),
        (
            "gc/swiss.x/swiss_r2119/WODE/swiss_r2119(wode_extcfg).iso",
            "Americas",
        ),
        (
            "gc/swiss.x/swiss_r2119/ISO/swiss_r2119(ntsc-u).iso",
            "Americas",
        ),
    ] {
        let Some(d) = file(rel) else { continue };
        assert_eq!(d.game.unwrap().region, Some(region), "{rel}");
    }
    if let Some(d) = file("3ds/Checkpoint.cia") {
        let g = d.game.unwrap();
        assert_eq!((g.region, g.db_id), (None, None));
    }
    if let Some(d) = file("switch/uLaunch.x/atmosphere/contents/0100000000001000/exefs.nsp") {
        let g = d.game.unwrap();
        assert_eq!((g.encrypted, g.format), (Some(false), "ExeFS"));
    }
    for (rel, title, ver) in [
        (
            "switch/ftpd.nro",
            "ftpd 11th Anniversary Edition",
            "3.2.1-2",
        ),
        ("switch/Goldleaf.nro", "Goldleaf", "1.2.1"),
    ] {
        let Some(d) = file(rel) else { continue };
        let g = d.game.unwrap();
        assert_eq!(
            (g.title.as_deref(), g.version.as_deref()),
            (Some(title), Some(ver))
        );
    }
}

#[test]
fn sony_homebrew() {
    for (rel, title) in [
        ("psp/apollo-psp.x/APOLLO/EBOOT.PBP", "Apollo Save Tool"),
        (
            "psp/CMFileManager-PSP.x/PSP/GAME/CMFileManager/EBOOT.PBP",
            "CM File Manager PSP v4.12",
        ),
    ] {
        let Some(d) = file(rel) else { continue };
        let g = d.game.unwrap();
        assert_eq!(g.title.as_deref(), Some(title), "{rel}");
        // The pspsdk default DISC_ID (UCJS10041) is no id.
        assert_eq!((g.title_id, g.db_id), (None, None));
    }
    for rel in ["ps4/ezremote_client.pkg", "ps4/Store-R2.pkg"] {
        let Some(d) = file(rel) else { continue };
        assert!(d.confidence >= 95, "{rel}: {}", d.confidence);
        assert!(!d.fields.iter().any(|f| f.key == "title_platform"), "{rel}");
        assert_eq!(d.game.unwrap().content, Some(Content::App), "{rel}");
    }
    let ps1 = "ps1/tonyhax-international-v1.6.3.x/tonyhax-international-v1.6.3/boot-cd/\
               tonyhax-boot-cd-europe.bin";
    if let Some(d) = file(ps1) {
        assert_eq!(
            (d.label.as_str(), d.game.unwrap().format),
            ("game.ps1.bin", "PS1 BIN")
        );
    }
}
