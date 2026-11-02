//! `sniff <file> [--explain] [--json] [--head N] [--tail N] [--rounds N] [--signatures FILE]`
//!
//! Simulates sparse fetches on a local file: only the ranges the detector asks for are read.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::process::ExitCode;

use sniff::{Caps, Detector, Outcome, Range, Signatures, Stop};

const USAGE: &str = "usage: sniff <file> [--explain] [--json] [--head N] [--tail N] [--rounds N] [--max-bytes N] [--signatures FILE]";

struct Args {
    path: String,
    explain: bool,
    json: bool,
    caps: Caps,
    signatures: Option<String>,
}

fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mul) = match s.char_indices().last() {
        Some((i, 'k' | 'K')) => (s.get(..i)?, 1024),
        Some((i, 'm' | 'M')) => (s.get(..i)?, 1 << 20),
        _ => (s, 1),
    };
    let v = match num.strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16).ok()?,
        None => num.parse::<u64>().ok()?,
    };
    v.checked_mul(mul)
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut a = Args {
        path: String::new(),
        explain: false,
        json: false,
        caps: Caps::default(),
        signatures: None,
    };
    while let Some(x) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match x.as_str() {
            "--explain" => a.explain = true,
            "--json" => a.json = true,
            "--head" => a.caps.head = parse_size(&val("--head")?).ok_or("bad --head")?,
            "--tail" => a.caps.tail = parse_size(&val("--tail")?).ok_or("bad --tail")?,
            "--rounds" => a.caps.rounds = val("--rounds")?.parse().map_err(|_| "bad --rounds")?,
            "--max-bytes" => {
                a.caps.max_bytes = parse_size(&val("--max-bytes")?).ok_or("bad --max-bytes")?
            }
            "--signatures" => a.signatures = Some(val("--signatures")?),
            "-h" | "--help" => return Err(USAGE.into()),
            s if s.starts_with("--") => return Err(format!("unknown option {s}\n{USAGE}")),
            s => {
                if !a.path.is_empty() {
                    return Err(USAGE.into());
                }
                a.path = s.to_owned();
            }
        }
    }
    if a.path.is_empty() {
        return Err(USAGE.into());
    }
    Ok(a)
}

fn read_range(f: &mut File, r: Range) -> std::io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(r.offset))?;
    let mut v = Vec::new();
    f.take(r.len).read_to_end(&mut v)?;
    Ok(v)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let user;
    let sigs = match &args.signatures {
        Some(p) => {
            let text = match std::fs::read_to_string(p) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("{p}: {e}");
                    return ExitCode::from(2);
                }
            };
            user = match Signatures::with_user(p, &text) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::from(2);
                }
            };
            &user
        }
        None => Signatures::builtin(),
    };
    let mut f = match File::open(&args.path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{}: {e}", args.path);
            return ExitCode::from(1);
        }
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let detector = Detector::with_signatures(sigs).caps(args.caps.clone());
    let report = match detector.detect_with(|r| read_range(&mut f, r), len, Some(&args.path)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}: {e}", args.path);
            return ExitCode::from(1);
        }
    };
    if args.json {
        let d = &report.analysis.detection;
        let out = serde_json::json!({
            "path": args.path,
            "size": len,
            "outcome": match report.outcome { Outcome::Detected(_) => "detected", Outcome::Unknown => "unknown", Outcome::NeedMore(_) => "need_more" },
            "detection": d,
            "rounds": report.rounds,
            "bytes_fetched": report.bytes_fetched,
            "fetched": report.fetched,
            "still_needed": report.analysis.need,
            "stop": report.stop,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return ExitCode::SUCCESS;
    }
    let d = &report.analysis.detection;
    let verdict = match report.outcome {
        Outcome::Unknown => "unknown",
        _ => d.label.as_str(),
    };
    println!(
        "{}: {verdict} ({}%) {} [{}]",
        args.path, d.confidence, d.description, d.mime
    );
    let mut flags = Vec::new();
    if d.flags.extension_mismatch {
        flags.push("extension-mismatch");
    }
    if d.flags.suspicious_executable {
        flags.push("SUSPICIOUS-EXECUTABLE");
    }
    if d.flags.unverified {
        flags.push("unverified");
    }
    if d.flags.incomplete {
        flags.push("incomplete");
    }
    if !flags.is_empty() {
        println!("  flags: {}", flags.join(", "));
    }
    if let Some(g) = &d.game {
        println!("  game: {}", serde_json::to_string(g).unwrap_or_default());
    }
    if let Some(v) = &d.video {
        println!("  video: {}", serde_json::to_string(v).unwrap_or_default());
    }
    for fl in &d.fields {
        println!("  {}: {}", fl.key, fl.value);
    }
    if args.explain {
        println!("  extensions: {}", d.extensions.join(", "));
        println!("  candidates:");
        for c in &d.candidates {
            println!("    {:>3}  {:<22} {}", c.confidence, c.label, c.source);
        }
        let ranges: Vec<String> = report.fetched.iter().map(ToString::to_string).collect();
        println!(
            "  fetched: {} bytes in {} rounds: {}",
            report.bytes_fetched,
            report.rounds,
            ranges.join(" ")
        );
        if !report.analysis.need.is_empty() {
            let need: Vec<String> = report
                .analysis
                .need
                .iter()
                .map(ToString::to_string)
                .collect();
            let why = match report.stop {
                Stop::Rounds => "round cap reached",
                Stop::Budget => "byte budget spent",
                Stop::NoProgress => "no new bytes",
                Stop::Done => "",
            };
            println!("  still wanted ({why}): {}", need.join(" "));
        }
        println!(
            "  signatures: {} rules (max head {} B, max tail {} B)",
            sigs.len(),
            sigs.max_head(),
            sigs.max_tail()
        );
    }
    ExitCode::SUCCESS
}
