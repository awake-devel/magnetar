//! Scores the detector against a corpus manifest.
//!
//! usage: cargo run --release -p sniff --example eval -- <manifest.tsv> [--head N] [--tail N]
//!        [--rounds N] [--misses] [--rows]
//!
//! Without options it uses the default caps (16 KiB head, no tail, 3 rounds); `--proto` uses
//! the prototype's evaluation setting (64 KiB head + 64 KiB tail, 6 rounds).

#[path = "../tests/support/eval.rs"]
mod eval;

use sniff::Caps;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(manifest) = args.first() else {
        eprintln!(
            "usage: eval <manifest.tsv> [--proto] [--head N] [--tail N] [--rounds N] [--misses] [--rows]"
        );
        std::process::exit(2);
    };
    let mut caps = Caps::default();
    let mut i = 1;
    let (mut misses, mut rows) = (false, false);
    while let Some(a) = args.get(i) {
        let num = |j: usize| args.get(j).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
        match a.as_str() {
            "--proto" => caps = eval::proto_caps(),
            "--head" => {
                caps.head = num(i + 1);
                i += 1;
            }
            "--tail" => {
                caps.tail = num(i + 1);
                i += 1;
            }
            "--rounds" => {
                caps.rounds = u32::try_from(num(i + 1)).unwrap_or(3);
                i += 1;
            }
            "--misses" => misses = true,
            "--rows" => rows = true,
            other => {
                eprintln!("unknown option {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let s = eval::run(std::path::Path::new(manifest), &caps);
    if rows {
        for r in &s.rows {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                r.path, r.want, r.got, r.confidence, r.rounds, r.bytes, r.flags
            );
        }
    }
    if misses || rows {
        for r in s.misses() {
            println!("MISS\t{}\t{}\t{}", r.path, r.want, r.got);
        }
    }
    let max_rounds = s.rows.iter().map(|r| r.rounds).max().unwrap_or(0);
    println!(
        "{manifest}: exact {}/{} ({:.1}%), missing files {}, head {} tail {} rounds {} (max used {}), bytes median {} p90 {}, panics {}",
        s.exact(),
        s.rows.len(),
        s.pct(),
        s.missing,
        caps.head,
        caps.tail,
        caps.rounds,
        max_rounds,
        s.median_bytes(),
        s.p90_bytes(),
        sniff::panics_caught()
    );
}
