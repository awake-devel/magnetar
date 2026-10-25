//! Accuracy of the offline IMDb title matcher against a ground-truth TSV (D40, docs/ENRICHMENT.md §4).
//!
//! `DATABASE_URL=… cargo run --release -p magnetar-server --example imdb_eval -- truth.tsv [--dht] [--no-fuzzy]`
//!
//! TSV columns: infohash, imdb id (tt…), category, torrent title, largest video file (may be
//! empty). Only the title (or with `--dht`, only the largest file name, as a DHT crawl would
//! give it) is used; the strong IDs in the torrent are ignored. A hit on the episode's series
//! counts as correct, as in the research script.

use std::collections::BTreeMap;

use magnetar_core::enrich::ids::imdb_num;
use magnetar_core::model::TorrentFile;
use magnetar_server::enrich::{imdb, match_one};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: imdb_eval truth.tsv [--dht] [--no-fuzzy]"))?;
    let dht = args.iter().any(|a| a == "--dht");
    let fuzzy = !args.iter().any(|a| a == "--no-fuzzy");
    let only_video = args.iter().any(|a| a == "--video");
    let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL")?).await?;
    let text = std::fs::read_to_string(path)?;
    let (mut correct, mut wrong, mut none, mut total) = (0u32, 0u32, 0u32, 0u32);
    // confidence bucket (tenths) -> (correct, wrong)
    let mut buckets: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
    let mut fuzzy_cw = (0u32, 0u32);
    let started = std::time::Instant::now();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 5 {
            continue;
        }
        if only_video && f[2] != "video" {
            continue;
        }
        let Some(truth) = imdb_num(f[1]) else {
            continue;
        };
        let files: Vec<TorrentFile> = if f[4].is_empty() {
            Vec::new()
        } else {
            vec![TorrentFile {
                path: f[4].to_string(),
                size: Some(1),
            }]
        };
        let title = if dht {
            if files.is_empty() {
                continue;
            }
            f[0]
        } else {
            f[3]
        };
        total += 1;
        let m = match_one(&pool, title, if dht { &files } else { &[] }, fuzzy).await?;
        let Some(m) = m else {
            none += 1;
            println!("NONE\t{}\t{title}", f[1]);
            continue;
        };
        let mut conn = pool.acquire().await?;
        let parent = match imdb::resolve_title(&mut conn, truth).await? {
            Some((row, _, _)) => u32::try_from(row.id).unwrap_or(0),
            None => truth,
        };
        let ok = m.imdb == truth || m.imdb == parent;
        let b = buckets
            .entry((m.confidence * 10.0).floor() as u32)
            .or_default();
        if ok {
            correct += 1;
            b.0 += 1;
        } else {
            wrong += 1;
            b.1 += 1;
            println!(
                "WRONG\t{}\tgot tt{:07}\t{:.2}\t{title}",
                f[1], m.imdb, m.confidence
            );
        }
        if m.fuzzy {
            if ok { fuzzy_cw.0 += 1 } else { fuzzy_cw.1 += 1 }
        }
    }
    let pct = |n: u32| 100.0 * f64::from(n) / f64::from(total.max(1));
    eprintln!(
        "{path} (dht={dht}, fuzzy={fuzzy}): correct {correct}/{total} ({:.1}%), wrong {wrong} ({:.1}%), none {none} ({:.1}%), precision {:.1}%; fuzzy {}/{} correct; {:.1} ms/torrent",
        pct(correct),
        pct(wrong),
        pct(none),
        100.0 * f64::from(correct) / f64::from((correct + wrong).max(1)),
        fuzzy_cw.0,
        fuzzy_cw.0 + fuzzy_cw.1,
        started.elapsed().as_secs_f64() * 1000.0 / f64::from(total.max(1)),
    );
    for (b, (c, w)) in buckets {
        eprintln!(
            "  confidence {:.1}-{:.1}: {c} correct, {w} wrong ({:.1}% precision)",
            f64::from(b) / 10.0,
            f64::from(b + 1) / 10.0,
            100.0 * f64::from(c) / f64::from((c + w).max(1))
        );
    }
    Ok(())
}
