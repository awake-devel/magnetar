//! Deterministic local data for the web UI check (docs/WEB.md "Local check",
//! scripts/webcheck.sh): ~200 torrents with media, probes, track corrections, seed-check
//! history, saved searches, activity series, relays and crawl-map counters, plus a handful of
//! named fixtures (W1–W12) whose ids go to `--out` as JSON.
//!
//! ```text
//! cargo run --offline -p magnetar-server --example webcheck_seed -- \
//!   --admin-url postgres://magnetar:magnetar@127.0.0.1:54329/magnetar \
//!   --db magnetar_webcheck --reset --out fixtures.json [--now <unix>]
//! ```
//!
//! Safety: only a loopback Postgres on port 54329, only a database named
//! `magnetar_webcheck[a-z0-9_]*`; `--reset` drops and recreates that one database only. No
//! other network: torrents are written through the server's own write paths, every tracker is
//! `127.0.0.1:9`, and the keys are throwaway test keys derived from fixed strings.

use std::sync::Arc;

use anyhow::{Context, bail, ensure};
use magnetar_core::api::ProbeCorrectionRequest;
use magnetar_core::ingest::{self, IngestOutcome};
use magnetar_core::model::{
    Liveness, LivenessSource, OverrideAction, ProbeFile, ProbeGame, RawEvent, SavedSearch,
};
use magnetar_core::query::SearchQuery;
use magnetar_core::store::{Store, keys};
use magnetar_server::PgStore;
use magnetar_server::fleet::proto::ProbeReport;
use magnetar_server::{probe_fix, probes, stats};
use nostr::prelude::*;
use serde_json::{Value as J, json};
use sha2::{Digest, Sha256};
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection};

const HOUR: i64 = 3600;
const DAY: i64 = 86400;
const GB: u64 = 1 << 30;
const MB: u64 = 1 << 20;
const TRACKERS: [&str; 2] = ["udp://127.0.0.1:9/announce", "http://127.0.0.1:9/announce"];

struct Args {
    admin_url: String,
    db: String,
    reset: bool,
    out: std::path::PathBuf,
    now: i64,
}

fn args() -> anyhow::Result<Args> {
    let mut admin_url = None;
    let mut db = None;
    let mut reset = false;
    let mut out = None;
    let mut now = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--admin-url" => admin_url = it.next(),
            "--db" => db = it.next(),
            "--reset" => reset = true,
            "--out" => out = it.next().map(Into::into),
            "--now" => now = Some(it.next().context("--now <unix>")?.parse()?),
            "-h" | "--help" => {
                println!(
                    "webcheck_seed --admin-url <loopback postgres url> --db magnetar_webcheck \
                     [--reset] --out <fixtures.json> [--now <unix>]"
                );
                std::process::exit(0);
            }
            other => bail!("unknown argument {other:?}"),
        }
    }
    Ok(Args {
        admin_url: admin_url.context("--admin-url is required")?,
        db: db.context("--db is required")?,
        reset,
        out: out.context("--out is required")?,
        now: now.unwrap_or_else(magnetar_core::now),
    })
}

/// The refusals of the module doc: loopback, port 54329, a `magnetar_webcheck*` database.
fn check_target(admin: &str, db: &str) -> anyhow::Result<String> {
    let u = url::Url::parse(admin).context("--admin-url")?;
    ensure!(
        matches!(u.scheme(), "postgres" | "postgresql"),
        "--admin-url must be postgres://"
    );
    let host = u.host_str().unwrap_or_default();
    ensure!(
        matches!(host, "127.0.0.1" | "::1" | "[::1]" | "localhost"),
        "refusing a non-loopback Postgres host {host:?}"
    );
    ensure!(
        u.port() == Some(54329),
        "refusing a Postgres port other than 54329"
    );
    let rest = db
        .strip_prefix("magnetar_webcheck")
        .context("the database name must start with magnetar_webcheck")?;
    ensure!(
        rest.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
        "the database name must match ^magnetar_webcheck[a-z0-9_]*$"
    );
    let mut target = u.clone();
    target.set_path(&format!("/{db}"));
    Ok(target.to_string())
}

/// First 40 hex digits of sha256("webcheck:" + n).
fn ih(n: u64) -> String {
    hex::encode(&Sha256::digest(format!("webcheck:{n}").as_bytes())[..20])
}

/// Throwaway test keys: secret = sha256("webcheck-key:" + i).
fn key(i: u32) -> Keys {
    let sk = SecretKey::from_slice(&Sha256::digest(format!("webcheck-key:{i}").as_bytes()))
        .expect("32-byte secret");
    Keys::new(sk)
}

fn hexkey(k: &Keys) -> String {
    k.public_key().to_hex()
}

struct Seed {
    store: Arc<PgStore>,
    now: i64,
    n: u64,
    fixtures: serde_json::Map<String, J>,
    warnings: Vec<String>,
}

/// A Nostr torrent to publish.
struct Nt<'a> {
    key: &'a Keys,
    title: String,
    files: Vec<(String, u64)>,
    tags: Vec<&'static str>,
    desc: String,
    age: i64,
}

impl Seed {
    fn next(&mut self) -> u64 {
        self.n += 1;
        self.n
    }

    fn pool(&self) -> &sqlx::PgPool {
        self.store.pool()
    }

    async fn ingest(&mut self, raw: RawEvent, what: &str) -> anyhow::Result<()> {
        match ingest::ingest_into(&*self.store, raw, self.now).await? {
            IngestOutcome::Inserted => {}
            other => self.warnings.push(format!("{what}: {other:?}")),
        }
        Ok(())
    }

    async fn sign(
        &self,
        k: &Keys,
        kind: u16,
        tags: Vec<Vec<String>>,
        content: &str,
        at: i64,
    ) -> anyhow::Result<RawEvent> {
        let tags: Vec<Tag> = tags
            .into_iter()
            .map(|t| Tag::parse(t).map_err(anyhow::Error::from))
            .collect::<anyhow::Result<_>>()?;
        let ev = EventBuilder::new(Kind::from(kind), content)
            .tags(tags)
            .custom_created_at(Timestamp::from(at as u64))
            .finalize(k)?;
        Ok(RawEvent::from_json(&ev.as_json())?)
    }

    /// Publish a kind 2003 through the server's ingest path; returns (infohash, event id).
    async fn nostr(&mut self, t: Nt<'_>) -> anyhow::Result<(String, String)> {
        let n = self.next();
        let h = ih(n);
        let mut tags = vec![
            vec!["title".to_string(), t.title.clone()],
            vec!["x".to_string(), h.clone()],
        ];
        for (p, s) in &t.files {
            tags.push(vec!["file".into(), p.clone(), s.to_string()]);
        }
        for tr in TRACKERS {
            tags.push(vec!["tracker".into(), tr.into()]);
        }
        for tag in &t.tags {
            tags.push(vec!["t".into(), (*tag).into()]);
        }
        let raw = self
            .sign(t.key, 2003, tags, &t.desc, self.now - t.age)
            .await?;
        let id = raw.id.clone();
        self.ingest(raw, &t.title).await?;
        Ok((h, id))
    }

    /// The same torrent republished by another key (several publishers).
    async fn republish(
        &mut self,
        k: &Keys,
        h: &str,
        title: &str,
        files: &[(String, u64)],
        age: i64,
    ) -> anyhow::Result<()> {
        let mut tags = vec![
            vec!["title".to_string(), title.to_string()],
            vec!["x".to_string(), h.to_string()],
            vec!["t".into(), "movie".into()],
        ];
        for (p, s) in files {
            tags.push(vec!["file".into(), p.clone(), s.to_string()]);
        }
        tags.push(vec!["tracker".into(), TRACKERS[0].into()]);
        let raw = self.sign(k, 2003, tags, "", self.now - age).await?;
        self.ingest(raw, title).await
    }

    async fn comment(
        &mut self,
        k: &Keys,
        event_id: &str,
        content: &str,
        age: i64,
    ) -> anyhow::Result<()> {
        let tags = vec![vec![
            "e".to_string(),
            event_id.to_string(),
            String::new(),
            "root".to_string(),
        ]];
        let raw = self.sign(k, 2004, tags, content, self.now - age).await?;
        self.ingest(raw, "comment").await
    }

    /// A DHT-only torrent (as the crawler stores one).
    async fn dht(
        &mut self,
        title: &str,
        files: &[(String, u64)],
        category: &str,
        via: &str,
        peers: i32,
        age: i64,
    ) -> anyhow::Result<String> {
        let n = self.next();
        let h = ih(n);
        let total: u64 = files.iter().map(|f| f.1).sum();
        let files_json: Vec<J> = files
            .iter()
            .map(|(p, s)| json!({"path": p, "size": s}))
            .collect();
        let files_text = files
            .iter()
            .map(|f| f.0.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        sqlx::query(
            "INSERT INTO dht_torrents (infohash, title, files, file_count, files_text, total_size,
                 category, metadata_size, piece_length, first_seen, fetched_at, discovered_via,
                 peers_seen, classifier_version)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 4096, 4194304, $8, $8, $9, $10, 1)",
        )
        .bind(&h)
        .bind(title)
        .bind(J::Array(files_json))
        .bind(files.len() as i32)
        .bind(files_text)
        .bind(total as i64)
        .bind(category)
        .bind(self.now - age)
        .bind(via)
        .bind(peers)
        .execute(self.pool())
        .await?;
        Ok(h)
    }

    async fn link(
        &self,
        h: &str,
        media: i64,
        season: Option<i32>,
        episode: Option<i32>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO torrent_media (infohash, media_id, season, episode, method, confidence,
                 matcher_version, matched_at)
             VALUES ($1, $2, $3, $4, 'manual', 1, 1, $5)
             ON CONFLICT (infohash) DO UPDATE SET media_id = EXCLUDED.media_id,
                 season = EXCLUDED.season, episode = EXCLUDED.episode",
        )
        .bind(h)
        .bind(media)
        .bind(season)
        .bind(episode)
        .bind(self.now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// A stored seed check, fresh for a week (`liveness.enabled=false` keeps the loop idle).
    async fn live(
        &self,
        h: &str,
        seeders: u32,
        leechers: u32,
        dht: Option<u32>,
        checked_ago: i64,
    ) -> anyhow::Result<()> {
        self.store
            .put_liveness(&Liveness {
                infohash: h.to_string(),
                seeders: Some(seeders),
                leechers: Some(leechers),
                completed: Some(seeders * 3),
                dht_peers_est: dht,
                source: LivenessSource::Udp,
                trackers_ok: 1,
                trackers_failed: 1,
                checked_at: self.now - checked_ago,
                expires_at: self.now + 7 * DAY,
            })
            .await?;
        Ok(())
    }

    /// `days` of history every 6 h, with steps (a few unanswered points).
    async fn history(&self, h: &str, days: i64, base: i64) -> anyhow::Result<()> {
        let mut at = Vec::new();
        let mut s = Vec::new();
        let mut l = Vec::new();
        let mut p = Vec::new();
        let points = days * 4;
        for i in 0..points {
            let t = self.now - (points - i) * 6 * HOUR;
            at.push(t);
            if i % 17 == 5 {
                s.push(None);
                l.push(None);
                p.push(None);
                continue;
            }
            let step = base + (i / 24) * 3 - (i % 24 / 12);
            s.push(Some(step.max(0) as i32));
            l.push(Some((step / 6).max(0) as i32));
            p.push(Some((step + 4) as i32));
        }
        sqlx::query(
            "INSERT INTO liveness_history (infohash, at, seeders, leechers, peers_est)
             SELECT $1, u.* FROM unnest($2::bigint[], $3::int[], $4::int[], $5::int[])
                  AS u(at, seeders, leechers, peers_est)
             ON CONFLICT DO NOTHING",
        )
        .bind(h)
        .bind(at)
        .bind(s)
        .bind(l)
        .bind(p)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The info dict a probe backfill lease brings for a Nostr-only torrent (D118): a
    /// `dht_torrents` row marked as backfill, so its triggers count it under
    /// `probe_backfill_dht_rows`, not as a DHT discovery. Probes are stored against it.
    async fn backfill_dict(&self, h: &str) -> anyhow::Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT set_config('magnetar.origin', 'backfill', true)")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO dht_torrents (infohash, title, files, file_count, files_text, total_size,
                 category, metadata_size, first_seen, fetched_at, discovered_via, peers_seen,
                 classifier_version, supplied_by)
             SELECT t.infohash, t.title, t.files, t.file_count, t.files_text,
                    coalesce(t.total_size, 0), t.category, 4096, t.created_at, $2, 'sample', 2,
                    1, 'eee'
             FROM torrents t WHERE t.infohash = $1 ORDER BY t.created_at LIMIT 1
             ON CONFLICT (infohash) DO NOTHING",
        )
        .bind(h)
        .bind(self.now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn probe(&mut self, h: &str, files: Vec<ProbeFile>, what: &str) -> anyhow::Result<()> {
        self.backfill_dict(h).await?;
        let o = probes::store(
            self.pool(),
            "eee",
            vec![probes::Upload {
                infohash: h.to_string(),
                report: ProbeReport {
                    v: magnetar_core::probe::PROBE_VERSION,
                    status: "ok".into(),
                    bytes_in: 262_144,
                    uploaded: 0,
                    peers: 2,
                    files,
                },
            }],
            self.now,
        )
        .await?;
        if o.stored != 1 {
            self.warnings.push(format!(
                "probe {what}: stored {} rejected {}",
                o.stored, o.rejected
            ));
        }
        Ok(())
    }

    fn fixture(&mut self, name: &str, v: J) {
        self.fixtures.insert(name.into(), v);
    }
}

fn video_file(i: u32, size: u64, video: J) -> ProbeFile {
    ProbeFile {
        i,
        size,
        label: "video.mkv".into(),
        confidence: 97,
        verified: true,
        video: Some(serde_json::from_value(video).expect("probe video")),
        ..Default::default()
    }
}

/// W1's mkv: Dolby Vision 8.1, 3 audio tracks, 15 subtitle tracks (`drop_last` / `renamed`
/// give the changed re-probe).
fn w1_video(renamed: bool, drop_last: bool) -> J {
    let mut subs = vec![
        json!({"codec": "subrip", "lang": "fre", "kinds": ["forced"]}),
        json!({"codec": "hdmv_pgs_subtitle", "lang": "eng", "kinds": ["sdh"],
               "name": if renamed { "English SDH (full)" } else { "English (SDH)" }}),
        json!({"codec": "eia_608", "kinds": ["cc"]}),
    ];
    let extra = [
        "ger", "spa", "ita", "por", "dut", "swe", "dan", "fin", "nor", "pol", "cze", "hun",
    ];
    for (k, lang) in extra.iter().enumerate() {
        let codec = if k % 2 == 0 {
            "subrip"
        } else {
            "hdmv_pgs_subtitle"
        };
        subs.push(json!({"codec": codec, "lang": lang}));
    }
    if drop_last {
        subs.pop();
    }
    json!({
        "container": "matroska",
        "duration_ms": 9_960_000u64,
        "bitrate_kbps": 62_000,
        "video": [{"codec": "hevc", "profile": "Main 10", "width": 3840, "height": 2160,
                   "fps_milli": 23976, "bit_depth": 10, "hdr": ["dolby_vision", "hdr10"],
                   "dv_profile": 8, "dv_compat": 1}],
        "audio": [
            {"codec": "eac3", "channels": 6, "lang": "fre", "atmos": true},
            {"codec": "ac3", "channels": 2, "lang": "eng", "kinds": ["commentary"],
             "name": if renamed { "Director's Commentary (2.0)" } else { "Director's Commentary" }},
            {"codec": "aac", "channels": 2, "lang": "eng"}],
        "subs": subs,
    })
}

struct MediaRow {
    kind: &'static str,
    title: &'static str,
    year: i32,
    rating: f32,
    votes: i32,
    runtime: i32,
    genres: &'static [&'static str],
    imdb: &'static str,
}

const MEDIA: &[MediaRow] = &[
    MediaRow {
        kind: "movie",
        title: "Dune: Part Two",
        year: 2024,
        rating: 8.5,
        votes: 812_000,
        runtime: 166,
        genres: &["Action", "Adventure", "Drama"],
        imdb: "tt15239678",
    },
    MediaRow {
        kind: "movie",
        title: "Big Buck Bunny",
        year: 2008,
        rating: 6.4,
        votes: 4_100,
        runtime: 10,
        genres: &["Animation", "Comedy", "Short"],
        imdb: "tt1254207",
    },
    MediaRow {
        kind: "movie",
        title: "Webcheck Horizon",
        year: 2021,
        rating: 7.1,
        votes: 52_000,
        runtime: 128,
        genres: &["Sci-Fi", "Thriller"],
        imdb: "tt9000003",
    },
    MediaRow {
        kind: "movie",
        title: "The Quiet Harbor",
        year: 2019,
        rating: 6.8,
        votes: 23_000,
        runtime: 112,
        genres: &["Drama"],
        imdb: "tt9000004",
    },
    MediaRow {
        kind: "movie",
        title: "Paper Lanterns",
        year: 2016,
        rating: 7.6,
        votes: 88_000,
        runtime: 104,
        genres: &["Romance", "Drama"],
        imdb: "tt9000005",
    },
    MediaRow {
        kind: "movie",
        title: "Iron Meridian",
        year: 2023,
        rating: 5.9,
        votes: 14_000,
        runtime: 131,
        genres: &["Action"],
        imdb: "tt9000006",
    },
    MediaRow {
        kind: "movie",
        title: "Les Ombres du Nord",
        year: 2012,
        rating: 7.3,
        votes: 9_800,
        runtime: 98,
        genres: &["Crime", "Drama"],
        imdb: "tt9000007",
    },
    MediaRow {
        kind: "movie",
        title: "Glass Orchard",
        year: 1999,
        rating: 8.0,
        votes: 310_000,
        runtime: 142,
        genres: &["Mystery", "Drama"],
        imdb: "tt9000008",
    },
    MediaRow {
        kind: "movie",
        title: "Northbound",
        year: 2005,
        rating: 6.1,
        votes: 7_200,
        runtime: 95,
        genres: &["Comedy"],
        imdb: "tt9000009",
    },
    MediaRow {
        kind: "movie",
        title: "Saltwater Kings",
        year: 2022,
        rating: 6.9,
        votes: 33_000,
        runtime: 119,
        genres: &["Adventure", "Family"],
        imdb: "tt9000010",
    },
    MediaRow {
        kind: "movie",
        title: "Midnight Cartography",
        year: 2018,
        rating: 7.8,
        votes: 120_000,
        runtime: 137,
        genres: &["Sci-Fi", "Drama"],
        imdb: "tt9000011",
    },
    MediaRow {
        kind: "movie",
        title: "The Last Lighthouse",
        year: 2010,
        rating: 7.0,
        votes: 41_000,
        runtime: 108,
        genres: &["Horror", "Thriller"],
        imdb: "tt9000012",
    },
    MediaRow {
        kind: "series",
        title: "The Expanse",
        year: 2015,
        rating: 8.5,
        votes: 190_000,
        runtime: 43,
        genres: &["Sci-Fi", "Drama", "Mystery"],
        imdb: "tt3230854",
    },
    MediaRow {
        kind: "series",
        title: "Harbor Lights",
        year: 2020,
        rating: 7.4,
        votes: 18_000,
        runtime: 50,
        genres: &["Drama"],
        imdb: "tt9000014",
    },
    MediaRow {
        kind: "series",
        title: "Station Nine",
        year: 2017,
        rating: 7.9,
        votes: 64_000,
        runtime: 45,
        genres: &["Crime", "Thriller"],
        imdb: "tt9000015",
    },
    MediaRow {
        kind: "series",
        title: "Le Bureau des Cartes",
        year: 2014,
        rating: 8.2,
        votes: 30_000,
        runtime: 55,
        genres: &["Drama", "Thriller"],
        imdb: "tt9000016",
    },
    MediaRow {
        kind: "series",
        title: "Copper Valley",
        year: 2021,
        rating: 6.6,
        votes: 9_000,
        runtime: 42,
        genres: &["Western"],
        imdb: "tt9000017",
    },
    MediaRow {
        kind: "series",
        title: "Small Hours",
        year: 2019,
        rating: 7.2,
        votes: 12_000,
        runtime: 30,
        genres: &["Comedy"],
        imdb: "tt9000018",
    },
    MediaRow {
        kind: "series",
        title: "Deep Field",
        year: 2023,
        rating: 8.0,
        votes: 21_000,
        runtime: 52,
        genres: &["Sci-Fi"],
        imdb: "tt9000019",
    },
    MediaRow {
        kind: "series",
        title: "The Archive",
        year: 2011,
        rating: 7.5,
        votes: 47_000,
        runtime: 47,
        genres: &["Mystery"],
        imdb: "tt9000020",
    },
    MediaRow {
        kind: "anime",
        title: "Sky Garden Chronicles",
        year: 2022,
        rating: 8.1,
        votes: 25_000,
        runtime: 24,
        genres: &["Animation", "Fantasy"],
        imdb: "tt9000021",
    },
    MediaRow {
        kind: "anime",
        title: "Mecha Lantern",
        year: 2018,
        rating: 7.4,
        votes: 11_000,
        runtime: 24,
        genres: &["Animation", "Action"],
        imdb: "tt9000022",
    },
    MediaRow {
        kind: "anime",
        title: "Tea House Detectives",
        year: 2020,
        rating: 7.8,
        votes: 8_000,
        runtime: 23,
        genres: &["Animation", "Mystery"],
        imdb: "tt9000023",
    },
    MediaRow {
        kind: "anime",
        title: "Wind Courier",
        year: 2016,
        rating: 8.3,
        votes: 40_000,
        runtime: 24,
        genres: &["Animation", "Adventure"],
        imdb: "tt9000024",
    },
    MediaRow {
        kind: "anime",
        title: "Starfall Academy",
        year: 2024,
        rating: 6.9,
        votes: 5_000,
        runtime: 24,
        genres: &["Animation", "Comedy"],
        imdb: "tt9000025",
    },
];

fn slug(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' => Some(c),
            ' ' | '-' => Some('.'),
            _ => None,
        })
        .collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let a = args()?;
    let target = check_target(&a.admin_url, &a.db)?;
    magnetar_core::http::install_crypto_provider();
    magnetar_core::blocklist::install(magnetar_core::blocklist::Blocklist::builtin()?);

    let mut admin = PgConnection::connect(&a.admin_url)
        .await
        .context("connect to the admin database")?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(&a.db)
            .fetch_one(&mut admin)
            .await?;
    if a.reset {
        // The name was checked above (^magnetar_webcheck[a-z0-9_]*$): safe to splice.
        admin
            .execute(AssertSqlSafe(format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                a.db
            )))
            .await?;
        admin
            .execute(AssertSqlSafe(format!("CREATE DATABASE {}", a.db)))
            .await?;
    } else if exists {
        bail!("{} exists: pass --reset to recreate it", a.db);
    } else {
        admin
            .execute(AssertSqlSafe(format!("CREATE DATABASE {}", a.db)))
            .await?;
    }
    admin.close().await?;
    // The scratch database lives in the same cluster, so pg_trgm is created there too.
    let mut c = PgConnection::connect(&target).await?;
    c.execute("CREATE EXTENSION IF NOT EXISTS pg_trgm").await?;
    c.close().await?;

    let store = Arc::new(PgStore::connect(&target, 5).await?);
    store.migrate().await?;
    let mut s = Seed {
        store: store.clone(),
        now: a.now,
        n: 0,
        fixtures: serde_json::Map::new(),
        warnings: Vec::new(),
    };
    seed(&mut s).await?;

    let out = J::Object(std::mem::take(&mut s.fixtures));
    std::fs::write(&a.out, serde_json::to_string_pretty(&out)?)
        .with_context(|| format!("write {}", a.out.display()))?;
    let torrents: i64 = sqlx::query_scalar("SELECT count(*) FROM torrent_infohashes")
        .fetch_one(store.pool())
        .await?;
    println!(
        "seeded {} ({} torrents, now = {}); fixtures in {}",
        a.db,
        torrents,
        a.now,
        a.out.display()
    );
    for w in &s.warnings {
        eprintln!("warning: {w}");
    }
    Ok(())
}

async fn seed(s: &mut Seed) -> anyhow::Result<()> {
    let now = s.now;
    // ---- keys, profiles, follows, settings -------------------------------------------------
    let root = key(0);
    let pubs: Vec<Keys> = (1..=8).map(key).collect();
    let low = key(9);
    let blocked = key(10);
    let names = [
        "Aurora Releases",
        "nordic-encodes",
        "CinemaVault",
        "<svg onload=console.error('XSS-FIRED')>",
        "SubSquad FR",
        "AnimeLantern",
        "linux-isos",
        "GameShelf",
    ];
    for (k, name) in pubs.iter().zip(names) {
        let raw = s
            .sign(
                k,
                0,
                vec![],
                &json!({"name": name}).to_string(),
                now - 30 * DAY,
            )
            .await?;
        s.ingest(raw, "profile").await?;
    }
    for (k, name) in [(&low, "low-trust uploader"), (&blocked, "blocked uploader")] {
        let raw = s
            .sign(
                k,
                0,
                vec![],
                &json!({"name": name}).to_string(),
                now - 30 * DAY,
            )
            .await?;
        s.ingest(raw, "profile").await?;
    }
    let mut follows: Vec<Vec<String>> = pubs
        .iter()
        .map(|k| vec!["p".to_string(), hexkey(k)])
        .collect();
    follows.push(vec!["p".into(), hexkey(&blocked)]);
    let raw = s.sign(&root, 3, follows, "", now - 20 * DAY).await?;
    s.ingest(raw, "root contact list").await?;
    // The publishers follow each other a little (a denser web of trust graph).
    for (i, k) in pubs.iter().enumerate() {
        let tags = vec![
            vec!["p".to_string(), hexkey(&pubs[(i + 1) % pubs.len()])],
            vec!["p".to_string(), hexkey(&pubs[(i + 3) % pubs.len()])],
        ];
        let raw = s.sign(k, 3, tags, "", now - 19 * DAY).await?;
        s.ingest(raw, "contact list").await?;
    }
    let st = &*s.store;
    st.set_setting(
        keys::TRUST_ROOT,
        &serde_json::to_string(&Some(hexkey(&root)))?,
    )
    .await?;
    st.set_setting(keys::TRUST_THRESHOLD, "20").await?;
    st.set_setting(keys::LIVENESS_ENABLED, "\"false\"").await?;
    st.set_override(&hexkey(&blocked), Some(OverrideAction::Block))
        .await?;
    st.bump_graph_version().await?;
    s.fixture(
        "keys",
        json!({
            "root_hex": hexkey(&root),
            "root_npub": root.public_key().to_bech32()?,
            "test_nsec": root.secret_key().to_bech32()?,
            "note": "throwaway test keys derived from fixed strings; never use them elsewhere",
            "publishers": pubs.iter().map(hexkey).collect::<Vec<_>>(),
            "below_threshold": hexkey(&low),
            "blocked": hexkey(&blocked),
        }),
    );

    // ---- media ------------------------------------------------------------------------------
    let mut media_ids = Vec::new();
    for (i, m) in MEDIA.iter().enumerate() {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO media (kind, imdb, title, year, end_year, genres, rating, votes,
                 runtime_min, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
        )
        .bind(m.kind)
        .bind(m.imdb)
        .bind(m.title)
        .bind(m.year)
        .bind((m.kind != "movie").then_some(m.year + 3))
        .bind(m.genres.iter().map(|g| g.to_string()).collect::<Vec<_>>())
        .bind(m.rating)
        .bind(m.votes)
        .bind(m.runtime)
        .bind(now)
        .fetch_one(s.pool())
        .await?;
        for (lang, overview) in [
            (
                "en-US",
                format!("{}: an overview for the local web check.", m.title),
            ),
            (
                "fr-FR",
                format!("{} : un résumé pour la vérification locale.", m.title),
            ),
        ] {
            let text = json!({"tmdb_kind": if m.kind == "movie" { "movie" } else { "tv" },
                "tmdb_id": 900_000 + i as u32, "title": m.title, "overview": overview,
                "tagline": null, "poster_path": null, "backdrop_path": null});
            sqlx::query(
                "INSERT INTO media_extra (media_id, source, lang, json, fetched_at)
                 VALUES ($1, 'tmdb', $2, $3, $4)",
            )
            .bind(id)
            .bind(lang)
            .bind(text)
            .bind(now - DAY)
            .execute(s.pool())
            .await?;
        }
        media_ids.push(id);
    }
    let mid = |title: &str| -> i64 {
        let i = MEDIA.iter().position(|m| m.title == title).expect("media");
        media_ids[i]
    };

    // ---- named fixtures ---------------------------------------------------------------------
    // W1: Dune, 3 publishers, comments, full probe, corrections, a changed re-probe, history.
    let w1_title =
        "Dune.Part.Two.2024.2160p.UHD.BluRay.REMUX.DV.HDR10.MULTi.TrueHD.Atmos.7.1-WEBCHECK";
    let w1_size = 78 * GB;
    let w1_files = vec![(format!("{w1_title}/{w1_title}.mkv"), w1_size)];
    let (w1, w1_event) = s
        .nostr(Nt {
            key: &pubs[0],
            title: w1_title.into(),
            files: w1_files.clone(),
            tags: vec!["movie", "uhd"],
            desc: "<p>Dune: Part Two</p><p>Remux of the UHD disc. <b>MULTi</b> &amp; Atmos.</p>"
                .into(),
            age: 3 * DAY,
        })
        .await?;
    for (k, age) in [(&pubs[1], 2 * DAY), (&pubs[2], DAY)] {
        s.republish(k, &w1, w1_title, &w1_files, age).await?;
    }
    s.comment(&pubs[3], &w1_event, "Great remux, thanks!", 2 * DAY)
        .await?;
    s.comment(&pubs[4], &w1_event, "French track is VFF, checked.", DAY)
        .await?;
    s.link(&w1, mid("Dune: Part Two"), None, None).await?;
    s.live(&w1, 42, 7, Some(55), 2 * HOUR).await?;
    s.history(&w1, 90, 20).await?;
    s.probe(
        &w1,
        vec![video_file(0, w1_size, w1_video(false, false))],
        "W1",
    )
    .await?;
    let req: ProbeCorrectionRequest = serde_json::from_value(json!({
        "device": "seed",
        "items": [
            {"file": 0, "track": "audio", "pos": 0, "set": []},
            {"file": 0, "track": "audio", "pos": 1, "set": ["lang", "name"], "lang": "fre",
             "name": "Commentaire du réalisateur"},
            {"file": 0, "track": "audio", "pos": 2, "removed": true, "set": []},
            {"file": 0, "track": "sub", "pos": 1, "set": []},
            {"file": 0, "track": "sub", "pos": 14, "set": ["lang"], "lang": "slo"},
            {"file": 0, "track": "sub", "set": ["lang", "kinds", "name"], "lang": "spa",
             "kinds": ["forced"], "name": "Español (forzados)"}
        ]
    }))?;
    if let Err(e) = probe_fix::correct(s.pool(), &w1, &req, now).await {
        s.warnings.push(format!("W1 corrections: {e:?}"));
    }
    // The probe changes: the commentary track (same place, same audio count) gets another name,
    // so its correction asks for a review; the SDH track is renamed and the last subtitle track
    // is gone, so with the subtitle count changed those corrections are unplaced.
    s.probe(
        &w1,
        vec![video_file(0, w1_size, w1_video(true, true))],
        "W1 re-probe",
    )
    .await?;
    let probe = s
        .store
        .torrent_detail(&w1, now)
        .await?
        .and_then(|d| d.probe)
        .map(|p| serde_json::to_value(p).unwrap_or_default());
    let review = probe
        .as_ref()
        .map(|p| p.to_string().contains("\"review\":true"))
        .unwrap_or(false);
    let unplaced = probe
        .as_ref()
        .and_then(|p| p.get("unplaced").and_then(|u| u.as_array()).map(Vec::len))
        .unwrap_or(0);
    if !review || unplaced == 0 {
        s.warnings.push(format!(
            "W1: expected a review flag and an unplaced correction, got review={review} unplaced={unplaced}"
        ));
    }
    s.fixture(
        "W1",
        json!({"infohash": w1, "title": w1_title, "media_id": mid("Dune: Part Two"),
               "review": review, "unplaced": unplaced}),
    );

    // W2: DHT-only, probed, seeded (so it shows in an empty-text browse, D76).
    let w2_title = "Big.Buck.Bunny.2008.1080p.BluRay.x264-WEBCHECK";
    let w2_size = 2 * GB;
    let w2 = s
        .dht(
            w2_title,
            &[(format!("{w2_title}.mkv"), w2_size)],
            "video",
            "sample",
            14,
            5 * DAY,
        )
        .await?;
    s.link(&w2, mid("Big Buck Bunny"), None, None).await?;
    s.live(&w2, 18, 2, Some(25), 3 * HOUR).await?;
    s.history(&w2, 20, 8).await?;
    s.probe(
        &w2,
        vec![video_file(
            0,
            w2_size,
            json!({"container": "matroska", "duration_ms": 596_000u64,
                "video": [{"codec": "h264", "width": 1920, "height": 1080, "bit_depth": 8}],
                "audio": [{"codec": "ac3", "channels": 6, "lang": "eng"},
                          {"codec": "aac", "channels": 2, "lang": "eng", "kinds": ["commentary"],
                           "name": "Commentary"}],
                "subs": [{"codec": "subrip", "lang": "fre", "kinds": ["forced"]}]}),
        )],
        "W2",
    )
    .await?;
    s.fixture("W2", json!({"infohash": w2, "title": w2_title}));

    // W3: both origins, an application, no probe.
    let w3_title = "ubuntu-24.04.3-desktop-amd64.iso";
    let (w3, _) = s
        .nostr(Nt {
            key: &pubs[6],
            title: w3_title.into(),
            files: vec![(w3_title.into(), 6 * GB)],
            tags: vec!["software", "linux"],
            desc: "Official image.".into(),
            age: 10 * DAY,
        })
        .await?;
    sqlx::query(
        "INSERT INTO dht_torrents (infohash, title, files, file_count, files_text, total_size,
             category, metadata_size, first_seen, fetched_at, discovered_via, peers_seen,
             classifier_version)
         VALUES ($1, $2, $3, 1, $2, $4, 'application', 4096, $5, $5, 'get_peers', 40, 1)",
    )
    .bind(&w3)
    .bind(w3_title)
    .bind(json!([{"path": w3_title, "size": 6 * GB}]))
    .bind((6 * GB) as i64)
    .bind(now - 9 * DAY)
    .execute(s.pool())
    .await?;
    s.live(&w3, 311, 12, Some(400), HOUR).await?;
    s.fixture("W3", json!({"infohash": w3, "title": w3_title}));

    // W4: hostile strings everywhere (none uses alert: a JS dialog would block the browser).
    // Track names carry bidi controls but no C0/C1 control characters: the server refuses
    // those in a probe upload (workers strip them), so they never reach a client.
    let w4_title =
        "<img src=x onerror=console.error('XSS-FIRED')> \"q\" & <b>b</b> \u{202E}gpj.exe";
    let long_name = format!("{}.mkv", "Very.Long.File.Name.".repeat(15));
    let w4_size = 700 * MB;
    let (w4, w4_event) = s
        .nostr(Nt {
            key: &pubs[3],
            title: w4_title.into(),
            files: vec![
                ("../../etc/passwd".into(), 4096),
                (
                    "<script>console.error('XSS-FIRED')</script>.mkv".into(),
                    w4_size,
                ),
                (long_name.clone(), 10 * MB),
            ],
            tags: vec!["movie"],
            desc: "<script>console.error('XSS-FIRED')</script><a href=\"javascript:console.error('XSS-FIRED')\">x</a> &amp; <p>para</p>".into(),
            age: 6 * HOUR,
        })
        .await?;
    s.comment(
        &pubs[5],
        &w4_event,
        "<img src=x onerror=console.error('XSS-FIRED')><a href=\"javascript:console.error('XSS-FIRED')\">x</a>",
        2 * HOUR,
    )
    .await?;
    s.live(&w4, 3, 1, None, 5 * HOUR).await?;
    s.probe(
        &w4,
        vec![ProbeFile {
            flags: vec!["suspicious_executable".into(), "extension_mismatch".into()],
            ..video_file(
                1,
                w4_size,
                json!({"container": "matroska",
                    "video": [{"codec": "h264", "width": 1280, "height": 720}],
                    "audio": [{"codec": "aac", "channels": 2, "lang": "eng",
                               "name": "\u{202E}kcart\u{2066} javascript:console.error('XSS-FIRED')\u{2069}\u{200F}x"}],
                    "subs": [{"codec": "subrip", "lang": "eng",
                              "name": "<img src=x onerror=console.error('XSS-FIRED')>"}]}),
            )
        }],
        "W4",
    )
    .await?;
    s.fixture(
        "W4",
        json!({"infohash": w4, "title": w4_title, "long_file": long_name}),
    );

    // W5: The Expanse season 5, two qualities per episode, a season pack and a complete pack.
    let expanse = mid("The Expanse");
    let mut w5 = Vec::new();
    for ep in 1..=10 {
        for (res, lang, codec, k) in [
            ("1080p", "FRENCH", "x264", 4usize),
            ("2160p", "MULTi", "x265", 1),
        ] {
            let title = format!("The.Expanse.S05E{ep:02}.{lang}.{res}.WEB-DL.{codec}-WEBCHECK");
            let size = if res == "2160p" { 4 * GB } else { 1300 * MB };
            let (h, _) = s
                .nostr(Nt {
                    key: &pubs[k],
                    title: title.clone(),
                    files: vec![(format!("{title}.mkv"), size)],
                    tags: vec!["tv"],
                    desc: String::new(),
                    age: (40 - ep) * DAY,
                })
                .await?;
            s.link(&h, expanse, Some(5), Some(ep as i32)).await?;
            s.live(&h, (ep as u32 * 7) % 40 + 1, 2, None, 6 * HOUR)
                .await?;
            w5.push(h);
        }
    }
    for (title, season) in [
        ("The.Expanse.S05.MULTi.1080p.BluRay.x264-WEBCHECK", Some(5)),
        (
            "The.Expanse.Complete.Series.MULTi.1080p.BluRay.x265-WEBCHECK",
            None,
        ),
    ] {
        let (h, _) = s
            .nostr(Nt {
                key: &pubs[1],
                title: title.into(),
                files: (1..=3)
                    .map(|e| (format!("{title}/Episode.{e:02}.mkv"), 1500 * MB))
                    .collect(),
                tags: vec!["tv"],
                desc: String::new(),
                age: 20 * DAY,
            })
            .await?;
        s.link(&h, expanse, season, None).await?;
        s.live(&h, 25, 4, None, 6 * HOUR).await?;
        w5.push(h);
    }
    s.fixture(
        "W5",
        json!({"media_id": expanse, "title": "The Expanse", "infohashes": w5}),
    );

    // W6: adult, hidden by default.
    let w6_title = "Webcheck.Adult.Fixture.XXX.1080p.MP4-WEBCHECK";
    let (w6, _) = s
        .nostr(Nt {
            key: &pubs[2],
            title: w6_title.into(),
            files: vec![(format!("{w6_title}.mp4"), GB)],
            tags: vec!["porn"],
            desc: String::new(),
            age: 4 * DAY,
        })
        .await?;
    s.live(&w6, 9, 1, None, 8 * HOUR).await?;
    s.fixture("W6", json!({"infohash": w6, "title": w6_title}));

    // W7: a PS5 package and a disguised executable.
    let w7_title = "Webcheck.Racer.PS5-WEBCHECK";
    let (w7, _) = s
        .nostr(Nt {
            key: &pubs[7],
            title: w7_title.into(),
            files: vec![
                ("Webcheck.Racer.PS5/game.pkg".into(), 30 * GB),
                ("Webcheck.Racer.PS5/Trailer.mkv".into(), 50 * MB),
            ],
            tags: vec!["game"],
            desc: String::new(),
            age: 7 * DAY,
        })
        .await?;
    s.probe(
        &w7,
        vec![
            ProbeFile {
                i: 0,
                size: 30 * GB,
                label: "game.ps5.pkg".into(),
                confidence: 95,
                verified: true,
                game: Some(ProbeGame {
                    platform: "ps5".into(),
                    kind: Some("package".into()),
                    format: Some("PS5 PKG".into()),
                    title_id: Some("PPSA01234".into()),
                    title: Some("Webcheck Racer".into()),
                    version: Some("01.000.000".into()),
                    region: Some("EU".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ProbeFile {
                i: 1,
                size: 50 * MB,
                label: "exe.pe".into(),
                confidence: 99,
                verified: true,
                flags: vec!["extension_mismatch".into(), "suspicious_executable".into()],
                ..Default::default()
            },
        ],
        "W7",
    )
    .await?;
    s.live(&w7, 5, 3, None, 4 * HOUR).await?;
    s.fixture("W7", json!({"infohash": w7, "title": w7_title}));

    // W8 dead (checked, 0 seeders); W9 never checked.
    let (w8, _) = s
        .nostr(Nt {
            key: &pubs[0],
            title: "Northbound.2005.720p.WEBRip.x264.AAC-WEBCHECK".into(),
            files: vec![(
                "Northbound.2005.720p.WEBRip.x264.AAC-WEBCHECK.mp4".into(),
                900 * MB,
            )],
            tags: vec!["movie"],
            desc: String::new(),
            age: 50 * DAY,
        })
        .await?;
    s.link(&w8, mid("Northbound"), None, None).await?;
    s.live(&w8, 0, 0, Some(0), 2 * HOUR).await?;
    s.history(&w8, 30, 0).await?;
    let (w9, _) = s
        .nostr(Nt {
            key: &pubs[1],
            title: "Glass.Orchard.1999.1080p.BluRay.DTS.x264-WEBCHECK".into(),
            files: vec![(
                "Glass.Orchard.1999.1080p.BluRay.DTS.x264-WEBCHECK.mkv".into(),
                12 * GB,
            )],
            tags: vec!["movie"],
            desc: String::new(),
            age: HOUR,
        })
        .await?;
    s.link(&w9, mid("Glass Orchard"), None, None).await?;
    s.fixture("W8", json!({"infohash": w8}));
    s.fixture("W9", json!({"infohash": w9}));

    // W10 from the below-threshold publisher; W11 from the blocked one.
    let (w10, _) = s
        .nostr(Nt {
            key: &low,
            title: "Saltwater.Kings.2022.1080p.WEB-DL.ENG.x264-LOWTRUST".into(),
            files: vec![(
                "Saltwater.Kings.2022.1080p.WEB-DL.ENG.x264-LOWTRUST.mkv".into(),
                3 * GB,
            )],
            tags: vec!["movie"],
            desc: String::new(),
            age: 2 * DAY,
        })
        .await?;
    s.live(&w10, 6, 1, None, 3 * HOUR).await?;
    let (w11, _) = s
        .nostr(Nt {
            key: &blocked,
            title: "Iron.Meridian.2023.1080p.WEB-DL.ENG.x264-BLOCKED".into(),
            files: vec![(
                "Iron.Meridian.2023.1080p.WEB-DL.ENG.x264-BLOCKED.mkv".into(),
                3 * GB,
            )],
            tags: vec!["movie"],
            desc: String::new(),
            age: 2 * DAY,
        })
        .await?;
    s.live(&w11, 6, 1, None, 3 * HOUR).await?;
    s.fixture("W10", json!({"infohash": w10}));
    s.fixture("W11", json!({"infohash": w11}));

    // W12: anime with Japanese audio and English signs & songs subtitles.
    let w12_title = "[WebcheckSubs] Sky Garden Chronicles - 07 [1080p][HEVC][AAC].mkv";
    let w12_size = 600 * MB;
    let (w12, _) = s
        .nostr(Nt {
            key: &pubs[5],
            title: w12_title.into(),
            files: vec![(w12_title.into(), w12_size)],
            tags: vec!["anime"],
            desc: String::new(),
            age: 12 * HOUR,
        })
        .await?;
    s.link(&w12, mid("Sky Garden Chronicles"), Some(1), Some(7))
        .await?;
    s.live(&w12, 64, 9, None, HOUR).await?;
    s.probe(
        &w12,
        vec![video_file(
            0,
            w12_size,
            json!({"container": "matroska", "duration_ms": 1_420_000u64,
                "video": [{"codec": "hevc", "width": 1920, "height": 1080, "bit_depth": 10}],
                "audio": [{"codec": "aac", "channels": 2, "lang": "jpn", "kinds": ["original"]}],
                "subs": [{"codec": "ass", "lang": "eng", "kinds": ["signs"],
                          "name": "Signs & Songs"},
                         {"codec": "ass", "lang": "eng", "name": "Full Subtitles"}]}),
        )],
        "W12",
    )
    .await?;
    s.fixture("W12", json!({"infohash": w12, "title": w12_title}));

    // ---- the bulk: Nostr torrents in every category ------------------------------------------
    let movies: Vec<&MediaRow> = MEDIA.iter().filter(|m| m.kind == "movie").collect();
    let res = ["720p", "1080p", "2160p"];
    let langs = ["MULTi.VFF", "FRENCH", "VOSTFR", "ENG"];
    let codecs = ["x264", "x265", "AV1"];
    let hdrs = ["", ".HDR10", ".DV"];
    let audios = ["DTS", "TrueHD.Atmos", "AAC"];
    let sources = ["BluRay", "WEB-DL", "REMUX"];
    let mut bulk = 0u64;
    for i in 0..60usize {
        let m = movies[i % movies.len()];
        let r = res[i % 3];
        let title = format!(
            "{}.{}.{}.{}.{}{}.{}.{}-WEBCHECK",
            slug(m.title),
            m.year,
            langs[i % 4],
            r,
            sources[(i / 3) % 3],
            if r == "2160p" { hdrs[(i / 2) % 3] } else { "" },
            audios[(i / 4) % 3],
            codecs[(i / 5) % 3],
        );
        let size = match r {
            "720p" => 900 * MB,
            "1080p" => 3 * GB,
            _ => 18 * GB,
        } + (i as u64) * MB;
        let k = &pubs[i % 6];
        let (h, _) = s
            .nostr(Nt {
                key: k,
                title: title.clone(),
                files: vec![
                    (format!("{title}/{title}.mkv"), size),
                    (format!("{title}/Subs/French.srt"), 80_000),
                    (format!("{title}/{title}.nfo"), 4_000),
                ],
                tags: vec!["movie"],
                desc: String::new(),
                age: (i as i64 % 45) * DAY + (i as i64) * HOUR,
            })
            .await?;
        let mi = MEDIA
            .iter()
            .position(|x| x.title == m.title)
            .expect("media");
        s.link(&h, media_ids[mi], None, None).await?;
        if i % 5 != 0 {
            s.live(
                &h,
                ((i * 13) % 90) as u32,
                ((i * 7) % 20) as u32,
                None,
                (i as i64 % 30) * HOUR,
            )
            .await?;
        }
        bulk += 1;
    }
    for i in 0..16usize {
        let m = &MEDIA[13 + i % 7];
        let title = format!(
            "{}.S{:02}E{:02}.{}.{}.WEB-DL.{}-WEBCHECK",
            slug(m.title),
            1 + i % 3,
            1 + i % 8,
            langs[i % 4],
            res[1 + i % 2],
            codecs[i % 2],
        );
        let (h, _) = s
            .nostr(Nt {
                key: &pubs[i % 5],
                title: title.clone(),
                files: vec![(format!("{title}.mkv"), 1200 * MB + i as u64 * MB)],
                tags: vec!["tv"],
                desc: String::new(),
                age: (i as i64 + 1) * 2 * DAY,
            })
            .await?;
        s.link(
            &h,
            media_ids[13 + i % 7],
            Some(1 + (i % 3) as i32),
            Some(1 + (i % 8) as i32),
        )
        .await?;
        s.live(&h, (i * 5) as u32 % 30, 1, None, 4 * HOUR).await?;
        bulk += 1;
    }
    for i in 0..10usize {
        let m = &MEDIA[20 + i % 5];
        let title = format!(
            "[WebcheckSubs] {} - {:02} [{}][VOSTFR].mkv",
            m.title,
            i + 1,
            res[1 + i % 2]
        );
        let (h, _) = s
            .nostr(Nt {
                key: &pubs[5],
                title: title.clone(),
                files: vec![(title.clone(), 500 * MB)],
                tags: vec!["anime"],
                desc: String::new(),
                age: (i as i64 + 1) * DAY,
            })
            .await?;
        s.link(&h, media_ids[20 + i % 5], Some(1), Some(i as i32 + 1))
            .await?;
        s.live(&h, 10 + i as u32, 2, None, 5 * HOUR).await?;
        bulk += 1;
    }
    let others: [(&str, &str, u64); 18] = [
        ("Webcheck.Band.Live.2019.FLAC.24bit", "music", 900 * MB),
        ("Orchestra.Webcheck.Symphonies.MP3.320", "music", 300 * MB),
        (
            "Webcheck.Audiobook.The.Long.Road.M4B",
            "audiobook",
            400 * MB,
        ),
        ("debian-13.1.0-amd64-netinst.iso", "linux", 700 * MB),
        ("Webcheck.Office.Suite.2025.x64", "software", 2 * GB),
        ("fedora-workstation-42-x86_64.iso", "linux", 2 * GB),
        ("Webcheck.Puzzle.Quest.GOG", "game", 5 * GB),
        ("Webcheck.Kart.Switch.NSP", "game", 8 * GB),
        ("Webcheck.Cookbook.2024.EPUB", "ebook", 20 * MB),
        ("Webcheck.Comics.Vol.1-5.CBZ", "comic", 600 * MB),
        ("Webcheck.Manga.Tome.01.FRENCH.CBZ", "manga", 120 * MB),
        ("Webcheck.Wallpapers.4K.Pack", "picture", 1500 * MB),
        ("Webcheck.Photo.Archive.2020", "image", 3 * GB),
        ("Webcheck.Misc.Dataset.2025", "other", 10 * GB),
        ("Webcheck.Fonts.Collection", "other", 200 * MB),
        ("Webcheck.Podcast.Season.3", "audio", 800 * MB),
        ("Webcheck.Retro.Games.Pack.psx", "psx", 4 * GB),
        ("Webcheck.Lecture.Notes.PDF", "book", 50 * MB),
    ];
    for (i, (title, tag, size)) in others.iter().enumerate() {
        let (h, _) = s
            .nostr(Nt {
                key: &pubs[6 + i % 2],
                title: (*title).into(),
                files: vec![(format!("{title}/data.bin"), *size)],
                tags: vec![tag],
                desc: format!("{title}: seeded for the local web check."),
                age: (i as i64 + 2) * DAY,
            })
            .await?;
        if i % 3 != 2 {
            s.live(&h, (i * 11 % 50) as u32, 3, None, 7 * HOUR).await?;
        }
        bulk += 1;
    }

    // ---- DHT-only torrents ------------------------------------------------------------------
    let vias = ["sample", "get_peers", "announce"];
    for i in 0..60usize {
        let (title, cat) = match i % 6 {
            0 => (
                format!(
                    "Midnight.Cartography.2018.{}.BluRay.x265-DHT{i}",
                    res[i % 3]
                ),
                "video",
            ),
            1 => (format!("Some.Album.{i}.FLAC"), "audio"),
            2 => (
                format!("[DHTSubs] Wind Courier - {:02} [1080p].mkv", i),
                "anime",
            ),
            3 => (
                format!(
                    "Station.Nine.S02E{:02}.FRENCH.1080p.WEB.x264-DHT",
                    i % 10 + 1
                ),
                "video",
            ),
            4 => (format!("tool-{i}-linux-x86_64.tar.gz"), "application"),
            _ => (
                format!("Les.Ombres.du.Nord.2012.MULTi.1080p.WEB-DL.x264-DHT{i}"),
                "video",
            ),
        };
        let h = s
            .dht(
                &title,
                &[(format!("{title}.mkv"), GB + i as u64 * 10 * MB)],
                cat,
                vias[i % 3],
                (i % 30) as i32,
                (i as i64 % 40) * DAY + i as i64 * 600,
            )
            .await?;
        if i % 6 == 0 {
            s.link(&h, mid("Midnight Cartography"), None, None).await?;
        }
        // Some DHT rows are seed-checked (they then show in browse, D76).
        if i % 4 == 0 {
            s.live(&h, (i % 17) as u32, 1, Some((i % 23) as u32), 9 * HOUR)
                .await?;
        }
    }
    // Twenty Nostr torrents also on the DHT ("both").
    let both: Vec<String> = sqlx::query_scalar(
        "SELECT t.infohash FROM torrents t WHERE t.category = 'video'
         AND NOT EXISTS (SELECT 1 FROM dht_torrents d WHERE d.infohash = t.infohash)
         GROUP BY t.infohash ORDER BY t.infohash LIMIT 20",
    )
    .fetch_all(s.pool())
    .await?;
    for (i, h) in both.iter().enumerate() {
        let (title, size): (String, i64) = sqlx::query_as(
            "SELECT title, coalesce(total_size, 0) FROM torrents WHERE infohash = $1 LIMIT 1",
        )
        .bind(h)
        .fetch_one(s.pool())
        .await?;
        sqlx::query(
            "INSERT INTO dht_torrents (infohash, title, files, file_count, files_text, total_size,
                 category, metadata_size, first_seen, fetched_at, discovered_via, peers_seen,
                 classifier_version)
             VALUES ($1, $2, $3, 1, $2, $4, 'video', 4096, $5, $5, $6, 5, 1)",
        )
        .bind(h)
        .bind(&title)
        .bind(json!([{"path": title, "size": size}]))
        .bind(size)
        .bind(now - (i as i64 + 1) * DAY)
        .bind(vias[i % 3])
        .execute(s.pool())
        .await?;
    }
    s.fixture(
        "counts",
        json!({"bulk_nostr": bulk, "dht_only": 60, "both": both.len()}),
    );

    // ---- saved searches ---------------------------------------------------------------------
    let q = |f: &dyn Fn(&mut SearchQuery)| {
        let mut q = SearchQuery {
            descending: true,
            ..Default::default()
        };
        f(&mut q);
        q
    };
    for (name, query, notify) in
        [
            (
                "Dune 4K",
                q(&|q: &mut SearchQuery| {
                    q.text = "dune".into();
                    q.resolutions = vec!["2160p".into()];
                }),
                true,
            ),
            (
                "French SDH subtitles",
                q(&|q: &mut SearchQuery| {
                    q.probe_languages = vec!["fre".into()];
                    q.probe_sub_kinds = vec!["sdh".into()];
                }),
                false,
            ),
            ("All torrents", q(&|_: &mut SearchQuery| {}), false),
            (
                "<img src=x onerror=console.error('XSS-FIRED')>",
                q(&|q: &mut SearchQuery| {
                    q.media_kinds = vec![magnetar_core::model::MediaKind::Series]
                }),
                false,
            ),
        ]
    {
        s.store
            .put_saved_search(&SavedSearch {
                id: 0,
                name: name.into(),
                query,
                notify,
                last_checked: Some(now - 20 * 60),
                last_seen_created_at: None,
            })
            .await?;
    }

    // ---- activity series --------------------------------------------------------------------
    let hours = 60 * 24;
    let first = (now / HOUR) * HOUR - (hours - 1) * HOUR;
    for (mi, metric) in stats::METRICS.iter().enumerate() {
        if matches!(*metric, stats::TORRENTS_TOTAL | stats::TORRENTS_PUBLISHED) {
            continue;
        }
        let mut buckets = Vec::new();
        let mut values = Vec::new();
        for h in 0..hours {
            let b = first + h * HOUR;
            let hour_of_day = (b / HOUR) % 24;
            let v = if *metric == stats::DB_SIZE {
                (2 << 30) + h * 3_000_000
            } else {
                let wave = 10 + (hour_of_day - 12).abs() * 3;
                ((wave * (mi as i64 + 2)) + (h % 7) * 5) / 2
            };
            buckets.push(b);
            values.push(v);
        }
        sqlx::query(
            "INSERT INTO stats_series (metric, bucket, value)
             SELECT $1, u.b, u.v FROM unnest($2::bigint[], $3::bigint[]) AS u(b, v)
             ON CONFLICT (metric, bucket) DO UPDATE SET value = EXCLUDED.value",
        )
        .bind(*metric)
        .bind(buckets)
        .bind(values)
        .execute(s.pool())
        .await?;
    }
    stats::recompute_published(s.pool()).await?;

    // ---- relays -----------------------------------------------------------------------------
    #[allow(clippy::type_complexity)]
    let relays: [(&str, &str, i64, i64, Option<&str>, &str, i32); 12] = [
        (
            "wss://relay.webcheck-one.example",
            "seed",
            4200,
            900,
            None,
            "FR",
            16276,
        ),
        (
            "wss://relay.webcheck-two.example",
            "seed",
            3100,
            640,
            None,
            "DE",
            24940,
        ),
        (
            "wss://nos.webcheck.example",
            "nip66",
            2500,
            300,
            None,
            "US",
            13335,
        ),
        (
            "wss://monitor.webcheck.example",
            "nip66",
            800,
            120,
            None,
            "NL",
            14061,
        ),
        (
            "wss://inbox.webcheck.example",
            "nip65",
            600,
            80,
            None,
            "US",
            16509,
        ),
        (
            "wss://outbox.webcheck.example",
            "nip65",
            450,
            40,
            None,
            "JP",
            2516,
        ),
        (
            "wss://contacts.webcheck.example",
            "kind3",
            300,
            25,
            None,
            "BR",
            28573,
        ),
        (
            "wss://hint.webcheck.example",
            "hint",
            120,
            9,
            None,
            "CA",
            577,
        ),
        (
            "wss://manual.webcheck.example",
            "manual",
            90,
            5,
            None,
            "FR",
            12322,
        ),
        ("wss://203.0.113.9", "nip66", 60, 3, None, "GB", 2856),
        (
            "wss://broken.webcheck.example",
            "seed",
            0,
            0,
            Some("connect: refused"),
            "",
            0,
        ),
        (
            "wss://quiet.webcheck.example",
            "kind3",
            0,
            0,
            None,
            "SE",
            3301,
        ),
    ];
    for (i, (url, source, events, torrents, err, cc, asn)) in relays.iter().enumerate() {
        sqlx::query(
            "INSERT INTO relays (url, source, enabled, last_ok, last_err, consecutive_failures,
                 next_try_at, events_found, torrents_found, geo_cc, geo_asn, geo_checked_at)
             VALUES ($1, $2, true, $3, $4, $5, 0, $6, $7, $8, $9, $10)
             ON CONFLICT (url) DO NOTHING",
        )
        .bind(url)
        .bind(source)
        .bind(err.is_none().then_some(now - (i as i64 + 1) * 600))
        .bind(err)
        .bind(if err.is_some() { 4 } else { 0 })
        .bind(events)
        .bind(torrents)
        .bind((!cc.is_empty()).then_some(*cc))
        .bind((*asn != 0).then_some(*asn))
        .bind(now - DAY)
        .execute(s.pool())
        .await?;
    }

    // ---- crawl map counters -----------------------------------------------------------------
    let countries = [
        ("FR", 16276),
        ("US", 13335),
        ("DE", 24940),
        ("NL", 14061),
        ("JP", 2516),
        ("BR", 28573),
        ("GB", 2856),
        ("CA", 577),
        ("SE", 3301),
        ("ES", 3352),
        ("IT", 3269),
        ("PL", 5617),
        ("RU", 12389),
        ("CN", 4134),
        ("IN", 9829),
        ("AU", 1221),
        ("KR", 4766),
        ("MX", 8151),
        ("AR", 7303),
        ("ZA", 37457),
        ("TR", 9121),
        ("UA", 15895),
        ("RO", 8708),
        ("CZ", 5610),
        ("FI", 790),
        ("NO", 2119),
        ("BE", 5432),
        ("CH", 3303),
        ("AT", 8447),
        ("PT", 3243),
    ];
    let layers: [(&str, &[&str]); 2] = [
        ("dht", &["hashes", "stored", "nodes"]),
        ("nostr", &["events", "torrents"]),
    ];
    let last_hour = (now / HOUR) * HOUR;
    for (layer, metrics) in layers {
        for (mi, metric) in metrics.iter().enumerate() {
            let mut ccs = Vec::new();
            let mut asns = Vec::new();
            let mut totals = Vec::new();
            for (ci, (cc, asn)) in countries.iter().enumerate() {
                ccs.push(cc.to_string());
                asns.push(*asn);
                totals.push(((30 - ci as i64) * (30 - ci as i64) * 40 + 7) / (mi as i64 + 1));
            }
            ccs.push(String::new());
            asns.push(0);
            totals.push(500 / (mi as i64 + 1));
            sqlx::query(
                "INSERT INTO geo_counts_total (layer, metric, cc, asn, value)
                 SELECT $1, $2, u.cc, u.asn, u.v
                 FROM unnest($3::text[], $4::int[], $5::bigint[]) AS u(cc, asn, v)",
            )
            .bind(layer)
            .bind(metric)
            .bind(&ccs)
            .bind(&asns)
            .bind(&totals)
            .execute(s.pool())
            .await?;
            let (mut b, mut c, mut a, mut v) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for h in 0..48 {
                for (i, cc) in ccs.iter().enumerate() {
                    if (i + h as usize).is_multiple_of(3) {
                        continue;
                    }
                    b.push(last_hour - h * HOUR);
                    c.push(cc.clone());
                    a.push(asns[i]);
                    v.push((totals[i] / 200).max(1) + h % 5);
                }
            }
            sqlx::query(
                "INSERT INTO geo_counts_hourly (layer, metric, bucket, cc, asn, value)
                 SELECT $1, $2, u.b, u.cc, u.asn, u.v
                 FROM unnest($3::bigint[], $4::text[], $5::int[], $6::bigint[])
                      AS u(b, cc, asn, v)",
            )
            .bind(layer)
            .bind(metric)
            .bind(b)
            .bind(c)
            .bind(a)
            .bind(v)
            .execute(s.pool())
            .await?;
        }
    }
    let as_names = [
        (16276, "OVH SAS"),
        (13335, "Cloudflare, Inc."),
        (24940, "Hetzner Online GmbH"),
        (14061, "DigitalOcean, LLC"),
        (2516, "KDDI CORPORATION"),
        (28573, "Claro NXT Telecomunicacoes Ltda"),
        (2856, "British Telecommunications PLC"),
        (577, "Bell Canada"),
        (3301, "Telia Company AB"),
        (12322, "Free SAS"),
    ];
    for (asn, org) in as_names {
        sqlx::query("INSERT INTO geo_as_names (asn, org, updated_at) VALUES ($1, $2, $3)")
            .bind(asn)
            .bind(org)
            .bind(now - DAY)
            .execute(s.pool())
            .await?;
    }
    sqlx::query(
        "INSERT INTO meta (key, value) VALUES ('crawl_map_since', $1)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(now - 20 * DAY)
    .execute(s.pool())
    .await?;

    // ---- probe backfill queue ---------------------------------------------------------------
    let unprobed: Vec<String> = sqlx::query_scalar(
        "SELECT ti.infohash FROM torrent_infohashes ti
         WHERE NOT EXISTS (SELECT 1 FROM probe_attrs p WHERE p.infohash = ti.infohash)
         ORDER BY ti.infohash LIMIT 40",
    )
    .fetch_all(s.pool())
    .await?;
    let outcomes = [
        Some("no_peers"),
        Some("adult"),
        Some("oversize"),
        Some("timeout"),
        None,
    ];
    for (i, h) in unprobed.iter().enumerate() {
        let rank = -(now - i as i64 * 100);
        #[allow(clippy::type_complexity)]
        let (due, attempts, outcome, worker, leased, expires): (
            Option<i64>,
            i16,
            Option<&str>,
            Option<&str>,
            Option<i64>,
            Option<i64>,
        ) = match i % 8 {
            0..=2 => (Some(rank), 0, None, None, None, None),
            3 => (
                Some(now + 60),
                1,
                None,
                Some("eee"),
                Some(now - 60),
                Some(now + 600),
            ),
            4 => (Some(now + 3600), 2, Some("timeout"), None, None, None),
            _ => (None, 3, outcomes[i % outcomes.len()], None, None, None),
        };
        sqlx::query(
            "INSERT INTO probe_backfill (infohash, rank, due_at, attempts, last_outcome, worker,
                 leased_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT DO NOTHING",
        )
        .bind(h)
        .bind(rank)
        .bind(due)
        .bind(attempts)
        .bind(outcome)
        .bind(worker)
        .bind(leased)
        .bind(expires)
        .execute(s.pool())
        .await?;
    }
    // ---- trackers -----------------------------------------------------------------------------
    // Ingest drops loopback trackers (nip35::valid_tracker), so the published ones never reach
    // the rows. Written here instead: the detail shows "Trackers (2)" and "Check now" scrapes
    // only 127.0.0.1:9 (DHT-only torrents have none; the check server runs with
    // MAGNETAR_LIVENESS_FALLBACK_TRACKERS=false, so nothing public is asked for them).
    sqlx::query("UPDATE torrents SET trackers = $1 WHERE cardinality(trackers) = 0")
        .bind(TRACKERS.iter().map(|t| t.to_string()).collect::<Vec<_>>())
        .execute(s.pool())
        .await?;

    s.fixture(
        "notes",
        json!({
            "trust": "scores are computed by the server's trust loop when it starts",
            "tmdb": "overviews are seeded in media_extra but shown only with TMDB on (off locally); posters 404 locally",
            "trackers": TRACKERS,
        }),
    );
    Ok(())
}
