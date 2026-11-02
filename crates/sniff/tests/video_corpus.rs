//! Video recon accuracy against ffprobe on the full file (the oracle), on local corpora made by
//! `tests/video_fixtures/gen_corpus2.sh` and by the prototype's `gen_corpus.sh`.
//!
//! The recon only sees the ranges it asks for: the driver ([`sniff::detect_with`]) reads them
//! from the local file. ffprobe reads everything (plus the first video frame for side data).
//!
//! Skipped when no corpus directory exists or ffprobe is not installed. Directories:
//! `SNIFF_VIDEO_CORPUS` (colon-separated; `off` skips) or the default scratch locations.
//! ffprobe is a test-time oracle only, never a dependency of the crate.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use sniff::video::{HdrFormat, VideoInfo};
use sniff::{Caps, Detector, Outcome, Range, SparseFile};

const DEFAULT_DIRS: &[&str] = &[
    "/tmp/claude-1000/-home-dude-magnetar/ab655fe2-024e-43d8-9df4-03634ee729f5/scratchpad/probes/video-recon/corpus2",
    "/tmp/claude-1000/-home-dude-magnetar/ab655fe2-024e-43d8-9df4-03634ee729f5/scratchpad/probes/video-recon/corpus",
];

fn dirs() -> Vec<PathBuf> {
    match std::env::var("SNIFF_VIDEO_CORPUS") {
        Ok(v) if v == "off" => Vec::new(),
        Ok(v) => v.split(':').map(PathBuf::from).collect(),
        Err(_) => DEFAULT_DIRS.iter().map(PathBuf::from).collect(),
    }
    .into_iter()
    .filter(|d| d.is_dir())
    .collect()
}

fn ffprobe(args: &[&str], f: &Path) -> Option<Value> {
    let out = Command::new("ffprobe")
        .args(["-v", "quiet", "-of", "json"])
        .args(args)
        .arg(f)
        .output()
        .ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

struct Run {
    info: Option<VideoInfo>,
    rounds: u32,
    bytes: u64,
    incomplete: bool,
}

/// The video module alone (no detector): head, then its own requests, like the driver does.
fn recon_only(path: &Path, caps: &Caps) -> (u32, u64) {
    let f = File::open(path).unwrap();
    let len = f.metadata().unwrap().len();
    let mut file = SparseFile::new(len);
    let mut bytes = 0u64;
    let mut read = |file: &mut SparseFile, r: Range| {
        let mut v = vec![0u8; r.len as usize];
        let n = f.read_at(&mut v, r.offset).unwrap();
        v.truncate(n);
        bytes += n as u64;
        file.insert(r.offset, v);
    };
    read(&mut file, Range::new(0, caps.head.min(len)));
    let mut rounds = 0;
    loop {
        let r = sniff::video::analyze(&file, caps);
        if r.need.is_empty() || rounds >= caps.rounds {
            break;
        }
        rounds += 1;
        for n in r.need.iter().take(caps.max_ranges) {
            for m in file.missing(*n) {
                read(&mut file, m);
            }
        }
    }
    (rounds, bytes)
}

fn recon(path: &Path, caps: Caps) -> Run {
    let f = File::open(path).unwrap();
    let len = f.metadata().unwrap().len();
    let det = Detector::new().caps(caps);
    let rep = det
        .detect_with(
            |r: Range| -> std::io::Result<Vec<u8>> {
                let mut v = vec![0u8; r.len as usize];
                let n = f.read_at(&mut v, r.offset)?;
                v.truncate(n);
                Ok(v)
            },
            len,
            path.file_name().and_then(|n| n.to_str()),
        )
        .unwrap();
    let (info, incomplete) = match &rep.outcome {
        Outcome::Detected(d) => (d.video.clone(), d.flags.incomplete),
        _ => (None, true),
    };
    Run {
        info,
        rounds: rep.rounds,
        bytes: rep.bytes_fetched,
        incomplete,
    }
}

#[derive(Default)]
struct Score {
    pass: usize,
    total: usize,
    fails: Vec<String>,
}

impl Score {
    fn chk<T: PartialEq + std::fmt::Debug>(&mut self, what: String, ours: T, theirs: T) {
        self.total += 1;
        if ours == theirs {
            self.pass += 1;
        } else {
            self.fails
                .push(format!("{what}: sniff={ours:?} ffprobe={theirs:?}"));
        }
    }
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn frac(v: Option<&str>) -> Option<f64> {
    let (a, b) = v?.split_once('/')?;
    let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
    (a > 0.0 && b > 0.0).then(|| a / b)
}

fn transfer_code(t: &str) -> Option<u8> {
    Some(match t {
        "bt709" => 1,
        "smpte2084" => 16,
        "arib-std-b67" => 18,
        "bt2020-10" => 14,
        "smpte170m" => 6,
        _ => return None,
    })
}

fn compare(name: &str, info: &VideoInfo, ff: &Value, frame: &Value, sc: &mut Score) {
    let streams: Vec<&Value> = ff
        .get("streams")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    // FFmpeg splits a Blu-ray TrueHD PID into truehd + its AC-3 companion (0 channels here).
    let mut kept: Vec<&Value> = Vec::new();
    for st in streams {
        let attached = st
            .get("disposition")
            .and_then(|d| d.get("attached_pic"))
            .and_then(Value::as_i64)
            == Some(1);
        let companion = s(st, "codec_name") == Some("ac3")
            && kept.last().is_some_and(|p| {
                s(p, "codec_name") == Some("truehd") && p.get("id") == st.get("id")
            });
        if !attached && !companion {
            kept.push(st);
        }
    }
    let of = |ty: &str| -> Vec<&Value> {
        kept.iter()
            .copied()
            .filter(|x| s(x, "codec_type") == Some(ty))
            .collect()
    };
    let (fv, fa, fs) = (of("video"), of("audio"), of("subtitle"));
    if let Some(d) = ff
        .get("format")
        .and_then(|f| s(f, "duration"))
        .and_then(|d| d.parse::<f64>().ok())
    {
        let ours = info.duration_ms.map_or(0.0, |m| m as f64 / 1000.0);
        sc.chk(
            format!("{name}: duration ±0.5% ({ours:.3} vs {d:.3})"),
            (ours - d).abs() <= 0.005 * d,
            true,
        );
    }
    sc.chk(format!("{name}: video tracks"), info.video.len(), fv.len());
    sc.chk(format!("{name}: audio tracks"), info.audio.len(), fa.len());
    sc.chk(
        format!("{name}: subtitle tracks"),
        info.subtitles.len(),
        fs.len(),
    );
    let frame0 = frame
        .get("frames")
        .and_then(|f| f.get(0))
        .cloned()
        .unwrap_or(Value::Null);
    let side: Vec<String> = frame0
        .get("side_data_list")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| s(x, "side_data_type").map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for (i, (t, st)) in info.video.iter().zip(&fv).enumerate() {
        let p = format!("{name}: v{i}");
        sc.chk(
            format!("{p} codec"),
            t.codec.as_str(),
            s(st, "codec_name").unwrap_or(""),
        );
        sc.chk(
            format!("{p} resolution"),
            (t.width, t.height),
            (
                st.get("width").and_then(Value::as_u64).map(|v| v as u32),
                st.get("height").and_then(Value::as_u64).map(|v| v as u32),
            ),
        );
        if let Some(fps) = frac(s(st, "avg_frame_rate")).or_else(|| frac(s(st, "r_frame_rate"))) {
            let ours = t.fps.unwrap_or(0.0);
            sc.chk(
                format!("{p} fps ±0.1% ({ours:.3} vs {fps:.3})"),
                (ours - fps).abs() <= 0.001 * fps,
                true,
            );
        }
        let pix = s(st, "pix_fmt").unwrap_or("");
        let bd = if pix.contains("10") {
            10
        } else if pix.contains("12") {
            12
        } else {
            8
        };
        if !pix.is_empty() {
            sc.chk(format!("{p} bit depth"), t.bit_depth, Some(bd));
        }
        if let Some(tr) = s(st, "color_transfer")
            .or_else(|| s(&frame0, "color_transfer"))
            .and_then(transfer_code)
        {
            sc.chk(format!("{p} transfer"), t.transfer, Some(tr));
        }
        if let Some(prof) = s(st, "profile").filter(|p| *p != "unknown") {
            sc.chk(format!("{p} profile"), t.profile.as_deref(), Some(prof));
        }
        if i == 0 {
            if side.iter().any(|x| x == "Mastering display metadata") {
                sc.chk(format!("{p} mastering display"), t.mastering_display, true);
            }
            if side.iter().any(|x| x == "Content light level metadata") {
                sc.chk(
                    format!("{p} content light level"),
                    t.max_cll.is_some(),
                    true,
                );
            }
            let plus = side
                .iter()
                .any(|x| x.contains("HDR10+") || x.contains("2094-40"));
            sc.chk(
                format!("{p} HDR10+"),
                t.hdr.contains(&HdrFormat::Hdr10Plus),
                plus,
            );
        }
        let dovi = st
            .get("side_data_list")
            .and_then(Value::as_array)
            .and_then(|a| {
                a.iter()
                    .find(|x| s(x, "side_data_type") == Some("DOVI configuration record"))
            });
        if let Some(d) = dovi {
            sc.chk(
                format!("{p} dolby vision profile"),
                t.dolby_vision.map(|d| u64::from(d.profile)),
                d.get("dv_profile").and_then(Value::as_u64),
            );
        }
        lang_chk(sc, &p, t.language.as_deref(), st);
    }
    for (i, (t, st)) in info.audio.iter().zip(&fa).enumerate() {
        let p = format!("{name}: a{i}");
        sc.chk(
            format!("{p} codec"),
            t.codec.as_str(),
            s(st, "codec_name").unwrap_or(""),
        );
        sc.chk(
            format!("{p} channels"),
            t.channels.map(u64::from),
            st.get("channels").and_then(Value::as_u64),
        );
        if let Some(sr) = s(st, "sample_rate").and_then(|v| v.parse::<u32>().ok()) {
            sc.chk(format!("{p} sample rate"), t.sample_rate, Some(sr));
        }
        let prof = s(st, "profile").filter(|p| *p != "unknown");
        if let Some(prof) = prof {
            sc.chk(format!("{p} profile"), t.profile.as_deref(), Some(prof));
        }
        if matches!(t.codec.as_str(), "eac3" | "truehd") {
            let atmos = prof.is_some_and(|p| p.contains("Atmos"));
            sc.chk(format!("{p} atmos"), t.atmos, Some(atmos));
        }
        lang_chk(sc, &p, t.language.as_deref(), st);
    }
    for (i, (t, st)) in info.subtitles.iter().zip(&fs).enumerate() {
        let p = format!("{name}: s{i}");
        sc.chk(
            format!("{p} codec"),
            t.codec.as_str(),
            s(st, "codec_name").unwrap_or(""),
        );
        let forced = st
            .get("disposition")
            .and_then(|d| d.get("forced"))
            .and_then(Value::as_i64)
            == Some(1);
        sc.chk(format!("{p} forced"), t.forced, forced);
        lang_chk(sc, &p, t.language.as_deref(), st);
    }
}

fn lang_chk(sc: &mut Score, p: &str, ours: Option<&str>, st: &Value) {
    if let Some(l) = st
        .get("tags")
        .and_then(|t| s(t, "language"))
        .filter(|l| *l != "und")
    {
        sc.chk(format!("{p} language"), ours, Some(l));
    }
}

#[test]
fn video_recon_matches_ffprobe() {
    let dirs = dirs();
    if dirs.is_empty() || Command::new("ffprobe").arg("-version").output().is_err() {
        eprintln!("note: no video corpus or no ffprobe; skipping");
        return;
    }
    let mut files: Vec<PathBuf> = dirs
        .iter()
        .flat_map(|d| std::fs::read_dir(d).unwrap().flatten().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            !n.starts_with("a_")
                && !n.starts_with("v_")
                && matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("mkv" | "webm" | "mp4" | "mov" | "avi" | "ts" | "m2ts")
                )
        })
        .collect();
    files.sort();
    let mut total = Score::default();
    let mut dflt = Score::default();
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let Some(ff) = ffprobe(&["-show_format", "-show_streams"], f) else {
            continue;
        };
        let frame = ffprobe(
            &[
                "-select_streams",
                "v:0",
                "-show_frames",
                "-read_intervals",
                "%+#1",
                "-show_entries",
                "frame=color_transfer,side_data_list",
            ],
            f,
        )
        .unwrap_or(Value::Null);
        let caps = sniff::video::recommended_caps();
        let run = recon(f, caps.clone());
        let (vr, vb) = recon_only(f, &caps);
        let mut sc = Score::default();
        match &run.info {
            Some(info) => compare(&name, info, &ff, &frame, &mut sc),
            None => sc.chk(format!("{name}: recon"), "none", "info"),
        }
        // Same file under the detector's default caps (16 KiB head, 3 rounds): reported only.
        let d = recon(f, Caps::default());
        let mut sd = Score::default();
        match &d.info {
            Some(info) => compare(&name, info, &ff, &frame, &mut sd),
            None => sd.chk(format!("{name}: recon"), "none", "info"),
        }
        eprintln!(
            "{name}: {}/{} checks; driver {} rounds {} KiB{}; video module alone {} rounds {} KiB; default caps {}/{}",
            sc.pass,
            sc.total,
            run.rounds,
            run.bytes / 1024,
            if run.incomplete { " (incomplete)" } else { "" },
            vr,
            vb / 1024,
            sd.pass,
            sd.total,
        );
        for m in &sc.fails {
            eprintln!("  FAIL {m}");
        }
        total.pass += sc.pass;
        total.total += sc.total;
        total.fails.extend(sc.fails);
        dflt.pass += sd.pass;
        dflt.total += sd.total;
    }
    eprintln!(
        "default caps: {}/{} field checks agree",
        dflt.pass, dflt.total
    );
    eprintln!(
        "video recon vs ffprobe: {}/{} field checks agree on {} files",
        total.pass,
        total.total,
        files.len()
    );
    // Known, documented misses (see crates/sniff/README.md): keep the agreement at least here.
    let pct = 100.0 * total.pass as f64 / total.total.max(1) as f64;
    assert!(pct >= 97.0, "agreement fell to {pct:.1}%");
}
