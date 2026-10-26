//! Release-attribute parser (D44) against a hand-checked sample of production titles
//! (`data/release_sample.tsv`: category, title, files when they matter, expected attributes).
//! docs/FILTERS.md has how the sample was drawn and checked, and the first-pass accuracy.
//!
//! Every field must stay at or above its floor; mismatches are printed so a parser change
//! shows exactly which titles it broke or fixed.

use magnetar_core::model::{Category, TorrentFile};
use magnetar_core::release::{ReleaseAttrs, parse_release};
use serde_json::Value;

/// (field, minimum share of rows where it matches).
const FLOORS: &[(&str, f64)] = &[
    ("languages", 0.98),
    ("subtitles", 0.98),
    ("resolution", 0.99),
    ("source", 0.99),
    ("video_codec", 0.99),
    ("hdr", 0.99),
    ("audio", 0.99),
    ("channels", 0.99),
    ("group", 0.98),
    ("season", 0.98),
    ("episode", 0.98),
    ("complete", 0.98),
    ("year", 0.98),
    ("formats", 0.99),
    ("bit_depth", 0.99),
    ("bitrate", 0.99),
    ("sample_rate", 0.99),
];

fn field(a: &Value, name: &str) -> Value {
    match a.get(name) {
        None | Some(Value::Null) => match name {
            "complete" => Value::Bool(false),
            "languages" | "subtitles" | "hdr" | "audio" | "formats" => Value::Array(vec![]),
            _ => Value::Null,
        },
        Some(v) => v.clone(),
    }
}

#[test]
fn hand_checked_sample() {
    let data = include_str!("../data/release_sample.tsv");
    let mut rows = 0usize;
    let mut misses: Vec<(usize, String)> = vec![(0, String::new()); FLOORS.len()];
    let mut perfect = 0;
    for line in data.lines().filter(|l| !l.is_empty()) {
        let cols: Vec<&str> = line.splitn(4, '\t').collect();
        let [cat, title, files, expected] = cols[..] else {
            panic!("bad fixture line: {line}");
        };
        let files: Vec<TorrentFile> = serde_json::from_str(files).unwrap();
        let cat: Category = cat.parse().unwrap();
        let got: ReleaseAttrs = parse_release(title, &files, cat);
        let got = serde_json::to_value(&got).unwrap();
        let want: Value = serde_json::from_str(expected).unwrap();
        rows += 1;
        let mut all = true;
        for (k, (name, _)) in FLOORS.iter().enumerate() {
            let (g, w) = (field(&got, name), field(&want, name));
            if g != w {
                all = false;
                misses[k].0 += 1;
                misses[k]
                    .1
                    .push_str(&format!("\n  {name}: got {g}, want {w}: {title}"));
            }
        }
        perfect += usize::from(all);
    }
    assert!(rows >= 250, "the sample has {rows} rows");
    let mut failed = Vec::new();
    for ((name, floor), (n, detail)) in FLOORS.iter().zip(&misses) {
        let share = 1.0 - *n as f64 / rows as f64;
        println!("{name:12} {:.1}% ({n} misses){detail}", share * 100.0);
        if share < *floor {
            failed.push(format!(
                "{name} {:.1}% < {:.0}%",
                share * 100.0,
                floor * 100.0
            ));
        }
    }
    println!("all fields right: {perfect}/{rows}");
    assert!(failed.is_empty(), "below the floor: {failed:?}");
}
