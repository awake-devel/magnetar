//! Header probes (D117, docs/PROBES.md) end to end against a scratch schema: results uploaded
//! with a dict are checked against the server's own file list and stored only for rows the
//! server classified as not adult and did not block; a refused probe never costs its dict; the
//! detail API shows them with the server's paths; a weaker re-probe does not replace a stronger
//! one; deleting the row removes them. Then an in-process worker on a fake DHT probes loopback
//! fake seeders (`core` tests' `fake_seeder`): the allowed torrent is probed and stored, the
//! adult and the blocklisted one never get a block request, a peer that never unchokes costs
//! nothing but the probe. Needs MAGNETAR_TEST_DATABASE_URL (else SKIPPED). Loopback only.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use magnetar_core::dht::Dht;
use magnetar_core::model::{ProbeFile, ProbeGame, ProbeVideo, ProbeVideoTrack};
use magnetar_core::probe::ProbeConfig;
use magnetar_server::fleet::proto::*;
use magnetar_server::probes;
use magnetar_server::worker::client::Client;
use magnetar_server::worker::{self};
use serde_json::Value as J;

use crate::common::*;
use crate::dht_fetch_pg::{announce, fake_cfg, info, sha1};
use crate::fake_seeder::{self as fs, Opts, Torrent, Unchoke};
use crate::workers::{W1, count, http, meta, opts, spawn, triage_one, tweak, upload, worker_dht};

/// `info()`'s single display file.
const SIZE: u64 = 1_500_000_000;

pub(crate) fn mkv_file(i: u32, size: u64) -> ProbeFile {
    ProbeFile {
        i,
        size,
        label: "video.mkv".into(),
        confidence: 97,
        verified: true,
        flags: vec!["agreed".into()],
        game: None,
        video: Some(ProbeVideo {
            container: "matroska".into(),
            duration_ms: Some(5_400_000),
            video: vec![ProbeVideoTrack {
                codec: "hevc".into(),
                height: Some(2160),
                width: Some(3840),
                hdr: vec!["dolby_vision".into()],
                ..Default::default()
            }],
            ..Default::default()
        }),
    }
}

pub(crate) fn report(files: Vec<ProbeFile>) -> ProbeReport {
    ProbeReport {
        v: magnetar_core::probe::PROBE_VERSION,
        status: "ok".into(),
        bytes_in: 262_144,
        uploaded: 0,
        peers: 1,
        files,
    }
}

pub(crate) fn with_probe(sighted: [u8; 20], raw: &[u8], r: ProbeReport) -> DhtResult {
    DhtResult {
        probe: Some(r),
        ..meta(sighted, raw)
    }
}

pub(crate) async fn detail(base: &str, ih: &str) -> J {
    http()
        .get(format!("{base}/api/v1/torrents/{ih}"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

const PROBE_ROWS: &str = "SELECT count(*) FROM probe_attrs WHERE infohash = $1";

#[tokio::test]
async fn uploaded_probes_are_checked_stored_and_shown() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (base, _) = spawn(s, 900).await;
    let c1 = Client::new(&base, W1).unwrap();

    // A good probe: stored with the dict, shown with the server's own path.
    let plain = info("Probe.Film.2024.2160p", "0");
    let h = sha1(&plain);
    let ih = hex::encode(h);
    triage_one(&c1, h).await;
    let a = upload(
        &c1,
        vec![with_probe(h, &plain, report(vec![mkv_file(0, SIZE)]))],
    )
    .await;
    assert_eq!(
        (a.stored, a.probe_stored, a.probe_rejected),
        (1, 1, 0),
        "{a:?}"
    );
    let d = detail(&base, &ih).await;
    let p = &d["probe"];
    assert_eq!(p["status"], "ok");
    assert_eq!(p["verified"], true);
    let f = &p["files"][0];
    assert_eq!(f["path"], "Probe.Film.2024.2160p.mkv", "{d}");
    assert_eq!(f["label"], "video.mkv");
    assert_eq!(f["description"], "Matroska video");
    assert_eq!(f["video"]["video"][0]["codec"], "hevc");
    let (height, by, tags): (Option<i32>, Option<String>, Vec<String>) =
        sqlx::query_as("SELECT height, supplied_by, tags FROM probe_attrs WHERE infohash = $1")
            .bind(&ih)
            .fetch_one(s.pool())
            .await
            .unwrap();
    assert_eq!((height, by.as_deref()), (Some(2160), Some("w1")));
    assert!(tags.iter().any(|t| t == "r:2160p") && tags.iter().any(|t| t == "h:dolby_vision"));

    // Refused probes never cost their dict: wrong size, index past the server's list,
    // unknown label, an invalid game id.
    let bad = [
        report(vec![mkv_file(0, SIZE - 1)]),
        report(vec![mkv_file(1, SIZE)]),
        report(vec![ProbeFile {
            label: "video.trust_me".into(),
            ..mkv_file(0, SIZE)
        }]),
        report(vec![ProbeFile {
            label: "game.ps5.pkg".into(),
            video: None,
            game: Some(ProbeGame {
                platform: "ps5".into(),
                db_id: Some("'; DROP TABLE x".into()),
                ..Default::default()
            }),
            ..mkv_file(0, SIZE)
        }]),
    ];
    for (k, r) in bad.into_iter().enumerate() {
        let raw = info(&format!("Probe.Bad.{k}.2024.1080p"), "0");
        let hb = sha1(&raw);
        triage_one(&c1, hb).await;
        let a = upload(&c1, vec![with_probe(hb, &raw, r)]).await;
        assert_eq!(
            (a.stored, a.probe_stored, a.probe_rejected),
            (1, 0, 1),
            "{k}: {a:?}"
        );
        assert_eq!(count(s, PROBE_ROWS, &hex::encode(hb)).await, 0);
    }

    // A probe on a status report, or for a hash this worker does not hold: not stored.
    let raw = info("Probe.Status.2024.1080p", "0");
    let hs = sha1(&raw);
    triage_one(&c1, hs).await;
    let a = upload(
        &c1,
        vec![DhtResult {
            outcome: DhtOutcome::Failed {
                code: "timeout".into(),
            },
            ..with_probe(hs, &raw, report(vec![mkv_file(0, SIZE)]))
        }],
    )
    .await;
    assert_eq!((a.applied, a.probe_rejected, a.probe_stored), (1, 1, 0));
    let raw = info("Probe.Unheld.2024.1080p", "0");
    let hu = sha1(&raw);
    let a = upload(
        &c1,
        vec![with_probe(hu, &raw, report(vec![mkv_file(0, SIZE)]))],
    )
    .await;
    assert_eq!((a.ignored, a.probe_stored), (1, 0));
    assert_eq!(count(s, PROBE_ROWS, &hex::encode(hu)).await, 0);

    // Adult by the server's own classification, or blocked by its own screen: no probe row.
    for name in [
        "Studio.XXX.Probe.Scene.2024.1080p",
        "Probe.Zorblax.2024.1080p",
    ] {
        let raw = info(name, "0");
        let hx = sha1(&raw);
        triage_one(&c1, hx).await;
        let a = upload(
            &c1,
            vec![with_probe(hx, &raw, report(vec![mkv_file(0, SIZE)]))],
        )
        .await;
        assert_eq!((a.probe_stored, a.probe_rejected), (0, 1), "{name}: {a:?}");
        assert_eq!(count(s, PROBE_ROWS, &hex::encode(hx)).await, 0, "{name}");
    }

    // A re-probe replaces the stored one only when it verifies at least as many files.
    let weaker = report(vec![ProbeFile {
        verified: false,
        label: "video.webm".into(),
        ..mkv_file(0, SIZE)
    }]);
    let o = probes::store(
        s.pool(),
        "w2",
        vec![probes::Upload {
            infohash: ih.clone(),
            report: weaker,
        }],
        NOW,
    )
    .await
    .unwrap();
    assert_eq!((o.stored, o.rejected), (0, 1));
    assert_eq!(
        detail(&base, &ih).await["probe"]["files"][0]["label"],
        "video.mkv"
    );
    let o = probes::store(
        s.pool(),
        "w2",
        vec![probes::Upload {
            infohash: ih.clone(),
            report: report(vec![ProbeFile {
                label: "video.webm".into(),
                ..mkv_file(0, SIZE)
            }]),
        }],
        NOW,
    )
    .await
    .unwrap();
    assert_eq!((o.stored, o.rejected), (1, 0));
    assert_eq!(
        detail(&base, &ih).await["probe"]["files"][0]["label"],
        "video.webm"
    );

    // The server reclassifying the torrent as adult hides the probe; deleting the row (as a
    // `supplied_by` clean-up does) removes it.
    sqlx::query("UPDATE dht_torrents SET category = 'porn' WHERE infohash = $1")
        .bind(&ih)
        .execute(s.pool())
        .await
        .unwrap();
    assert!(detail(&base, &ih).await.get("probe").is_none());
    sqlx::query("DELETE FROM dht_torrents WHERE infohash = $1")
        .bind(&ih)
        .execute(s.pool())
        .await
        .unwrap();
    assert_eq!(count(s, PROBE_ROWS, &ih).await, 0);
    assert_eq!(
        count(
            s,
            "SELECT count(*) FROM probe_files WHERE infohash = $1",
            &ih
        )
        .await,
        0
    );
}

fn ebml(id: &[u8], data: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    if data.len() < 0x7f {
        v.push(0x80 | data.len() as u8);
    } else {
        v.extend_from_slice(&(0x4000u16 | data.len() as u16).to_be_bytes());
    }
    v.extend_from_slice(data);
    v
}

/// A Matroska header (HEVC + E-AC-3) then filler: sniff's `video.mkv` at 97.
pub(crate) fn mkv(size: usize) -> Vec<u8> {
    let mut head = ebml(&[0x42, 0x86], &[1]);
    head.extend(ebml(&[0x42, 0x82], b"matroska"));
    let mut out = ebml(&[0x1A, 0x45, 0xDF, 0xA3], &head);
    let mut entries = Vec::new();
    for (i, (ty, codec)) in [(1u8, "V_MPEGH/ISO/HEVC"), (2, "A_EAC3")]
        .iter()
        .enumerate()
    {
        let mut te = ebml(&[0xD7], &[i as u8 + 1]);
        te.extend(ebml(&[0x83], &[*ty]));
        te.extend(ebml(&[0x86], codec.as_bytes()));
        entries.extend(ebml(&[0xAE], &te));
    }
    let mut seg = ebml(
        &[0x15, 0x49, 0xA9, 0x66],
        &ebml(&[0x2A, 0xD7, 0xB1], &[0x0F, 0x42, 0x40]),
    );
    seg.extend(ebml(&[0x16, 0x54, 0xAE, 0x6B], &entries));
    seg.extend(ebml(&[0x1F, 0x43, 0xB6, 0x75], &[0u8; 32]));
    out.extend(ebml(&[0x18, 0x53, 0x80, 0x67], &seg));
    let start = out.len();
    out.extend((start..size).map(|i| (i * 31 + 7) as u8));
    out
}

/// One in-process worker with probes on, on a fake DHT, four loopback seeders: an allowed
/// torrent (probed, verified, stored and shown), an adult one and a blocklisted one (never a
/// block request), and one whose peer never unchokes (its dict is stored without a probe).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_probes_only_what_it_may() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (base, bodies) = spawn(s, 900).await;

    let mut net = Vec::new();
    let mut boot = Vec::new();
    for i in 0..6u8 {
        let ip = Ipv4Addr::new(127, 0, 61, 10 + i);
        let (d, _rx) = Dht::start(fake_cfg(ip, boot.clone())).await.unwrap();
        let a = SocketAddrV4::new(ip, d.local_ports()[0]);
        if boot.len() < 2 {
            boot.push(a);
        }
        net.push((d, a));
    }
    let seeder_ip = Ipv4Addr::new(127, 0, 62, 1);
    let kib = 1024u64;
    let plain = Torrent::new(
        "x",
        vec![("Probe.Movie.2024.2160p.mkv", mkv(2_000_000))],
        256 * kib,
    );
    let adult = Torrent::new(
        "x",
        vec![("Studio.XXX.Probe.Scene.2024.1080p.mkv", mkv(2_000_000))],
        256 * kib,
    );
    let blocked = Torrent::new(
        "x",
        vec![("Probe.Show.Zorblax.2024.720p.mkv", mkv(2_000_000))],
        256 * kib,
    );
    let shy = Torrent::new(
        "x",
        vec![("Probe.Shy.2024.1080p.mkv", mkv(2_000_000))],
        256 * kib,
    );
    let mut seeders = HashMap::new();
    for (name, t, o) in [
        ("plain", &plain, Opts::default()),
        ("adult", &adult, Opts::default()),
        ("blocked", &blocked, Opts::default()),
        (
            "shy",
            &shy,
            Opts {
                unchoke: Unchoke::Never,
                ..Opts::default()
            },
        ),
    ] {
        seeders.insert(name, (t.hash, fs::start_at(seeder_ip, t.clone(), o).await));
    }
    let (client, _crx) = Dht::start(fake_cfg(seeder_ip, vec![net[0].1]))
        .await
        .unwrap();
    for (k, (h, sd)) in seeders.values().enumerate() {
        announce(&client, net[k % net.len()].1, *h, sd.addr.port()).await;
    }

    let mut o = opts(&base, W1, Some(worker_dht(vec![0])), None);
    o.probe = Some(ProbeConfig {
        // No pace in the test: fetches finishing together must not take each other's turn.
        per_min: 6_000_000,
        parallel: 4,
        unchoke_wait: Duration::from_secs(2),
        stall: Duration::from_secs(2),
        session: Duration::from_secs(8),
        second_peer_after: Duration::from_millis(300),
        upload_linger: Duration::from_millis(300),
        ..ProbeConfig::default()
    });
    let dir = o.state_dir.clone();
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let r = worker::run(o, stop, Some(Duration::from_secs(18)), {
        // Bootstrap from every fake node: they start read-only (no external address
        // vote yet), so none of them lists the others.
        let all: Vec<SocketAddrV4> = net.iter().map(|(_, a)| *a).collect();
        let base = tweak(Ipv4Addr::new(127, 0, 63, 1), net[0].1);
        move |c: &mut magnetar_core::dht::EngineConfig| {
            base(c);
            c.bootstrap = all;
        }
    })
    .await
    .unwrap();
    client.shutdown().await;
    let rep = serde_json::to_string(&r).unwrap();
    assert!(r.error.is_none(), "{rep}");
    let ps = r.dht.as_ref().unwrap().probe.clone().expect("probe stats");

    let hex_of = |n: &str| hex::encode(seeders[n].0);
    // The allowed torrent: probed from its peer, verified (256 KiB pieces), stored, shown.
    assert_eq!(count(s, PROBE_ROWS, &hex_of("plain")).await, 1, "{rep}");
    let d = detail(&base, &hex_of("plain")).await;
    let f = &d["probe"]["files"][0];
    assert_eq!(f["label"], "video.mkv", "{d}");
    assert_eq!(f["verified"], true);
    assert_eq!(f["path"], "Probe.Movie.2024.2160p.mkv");
    assert_eq!(f["video"]["video"][0]["codec"], "hevc");
    assert!(seeders["plain"].1.log().requests > 0);
    // Adult and blocklisted: the probe never asked their peers for a block.
    for n in ["adult", "blocked"] {
        let l = seeders[n].1.log();
        assert_eq!(l.requests, 0, "{n} got block requests: {rep}");
        assert_eq!(count(s, PROBE_ROWS, &hex_of(n)).await, 0, "{n}");
    }
    let skipped: HashMap<String, u64> = ps.skipped.iter().cloned().collect();
    assert!(skipped.get("adult").copied().unwrap_or(0) >= 1, "{rep}");
    assert!(skipped.get("blocklist").copied().unwrap_or(0) >= 1, "{rep}");
    // Never unchoked: no probe, but the dict is stored all the same.
    assert_eq!(
        count(
            s,
            "SELECT count(*) FROM dht_torrents WHERE infohash = $1",
            &hex_of("shy")
        )
        .await,
        1,
        "{rep}"
    );
    assert_eq!(count(s, PROBE_ROWS, &hex_of("shy")).await, 0);
    assert!(ps.choked >= 1, "{rep}");
    assert_eq!(ps.uploaded, 0, "upload off: nothing served");
    // On the wire: probes only with dicts, never on the blocked hash-only report.
    for (path, body) in bodies.lock().unwrap().iter() {
        if path != DHT_RESULTS {
            continue;
        }
        let up: DhtResults = serde_json::from_slice(body).unwrap();
        for it in up.items {
            if it.probe.is_some() {
                assert!(matches!(it.outcome, DhtOutcome::Metadata { .. }));
                assert_eq!(it.sighted, hex_of("plain"));
            }
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// A DHT row as the crawler stores it, files given (display order).
pub(crate) async fn dht_row(
    s: &magnetar_server::store::PgStore,
    ih: &str,
    title: &str,
    category: &str,
    files: &[(&str, u64)],
) {
    let list: Vec<J> = files
        .iter()
        .map(|(p, n)| serde_json::json!({"path": p, "size": n}))
        .collect();
    let total: u64 = files.iter().map(|(_, n)| n).sum();
    sqlx::query(
        "INSERT INTO dht_torrents (infohash, title, files, file_count, files_text, total_size,
             category, metadata_size, first_seen, fetched_at, discovered_via, classifier_version)
         VALUES ($1, $2, $3, $4, '', $5, $6, 1000, $7, $7, 'sample', 1)",
    )
    .bind(ih)
    .bind(title)
    .bind(J::Array(list))
    .bind(files.len() as i32)
    .bind(total as i64)
    .bind(category)
    .bind(NOW)
    .execute(s.pool())
    .await
    .unwrap();
}

fn upload_of(ih: &str, files: Vec<ProbeFile>) -> Vec<probes::Upload> {
    vec![probes::Upload {
        infohash: ih.to_string(),
        report: report(files),
    }]
}

/// The server's adult gate is the worker's (`Classification::adult`: category `porn` or the
/// adult signal), on store and on read; on a torrent with a Nostr and a DHT row, probe indexes
/// point into the Nostr `files` the detail shows, and an adult Nostr row hides the probe.
#[tokio::test]
async fn probe_gates_and_indexes_follow_the_detail_shown() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let base = spawn_server(s).await;
    let flac = |i| ProbeFile {
        label: "audio.flac".into(),
        video: None,
        flags: Vec::new(),
        ..mkv_file(i, 40_000_000)
    };

    // Adult signal on a non-visual majority (category `audio`, not `porn`): never stored.
    let adult = ih(0xad01);
    dht_row(
        s,
        &adult,
        "Studio.XXX.Hits.2024",
        "audio",
        &[("01.flac", 40_000_000)],
    )
    .await;
    let o = probes::store(s.pool(), "w1", upload_of(&adult, vec![flac(0)]), NOW)
        .await
        .unwrap();
    assert_eq!((o.stored, o.rejected), (0, 1));
    assert_eq!(count(s, PROBE_ROWS, &adult).await, 0);

    // Stored under a plain name, then the row reads as adult: the detail hides it.
    let renamed = ih(0xad02);
    dht_row(
        s,
        &renamed,
        "Plain.Hits.2024",
        "audio",
        &[("01.flac", 40_000_000)],
    )
    .await;
    let o = probes::store(s.pool(), "w1", upload_of(&renamed, vec![flac(0)]), NOW)
        .await
        .unwrap();
    assert_eq!((o.stored, o.rejected), (1, 0));
    assert_eq!(
        detail(&base, &renamed).await["probe"]["files"][0]["index"],
        0
    );
    sqlx::query("UPDATE dht_torrents SET title = 'Studio.XXX.Hits.2024' WHERE infohash = $1")
        .bind(&renamed)
        .execute(s.pool())
        .await
        .unwrap();
    assert!(detail(&base, &renamed).await.get("probe").is_none());

    // Nostr and DHT rows list the files in different orders (and the Nostr one with the root
    // folder): the probe of the DHT row's file 1 (b.mkv) is the Nostr list's file 0.
    let both = ih(0xb0b0);
    dht_row(
        s,
        &both,
        "Both.Film",
        "video",
        &[("a.nfo", 1_000), ("b.mkv", SIZE)],
    )
    .await;
    let o = probes::store(
        s.pool(),
        "w1",
        upload_of(&both, vec![mkv_file(1, SIZE)]),
        NOW,
    )
    .await
    .unwrap();
    assert_eq!((o.stored, o.rejected), (1, 0));
    put(
        s,
        torrent(0xb0b0, 7, NOW, 0xb0b0, "Both.Film")
            .size(SIZE + 1_000)
            .cat(magnetar_core::model::Category::Video)
            .files(&[("Both.Film/b.mkv", SIZE), ("Both.Film/a.nfo", 1_000)]),
    )
    .await;
    let d = detail(&base, &both).await;
    assert_eq!(d["summary"]["origin"], "both", "{d}");
    let f = &d["probe"]["files"][0];
    let at = f["index"].as_u64().unwrap() as usize;
    assert_eq!(at, 0, "{d}");
    assert_eq!(d["files"][at]["path"], "Both.Film/b.mkv");
    assert_eq!(f["path"], d["files"][at]["path"]);
    assert_eq!(f["label"], "video.mkv");

    // The Nostr row the detail shows is adult: no probe, whatever the DHT row says.
    let nostr_adult = ih(0xb0b1);
    dht_row(
        s,
        &nostr_adult,
        "Film.Two",
        "video",
        &[("Film.Two.mkv", SIZE)],
    )
    .await;
    let o = probes::store(
        s.pool(),
        "w1",
        upload_of(&nostr_adult, vec![mkv_file(0, SIZE)]),
        NOW,
    )
    .await
    .unwrap();
    assert_eq!((o.stored, o.rejected), (1, 0));
    put(
        s,
        torrent(0xb0b1, 7, NOW, 0xb0b1, "Film.Two")
            .size(SIZE)
            .cat(magnetar_core::model::Category::Porn)
            .files(&[("Film.Two.mkv", SIZE)]),
    )
    .await;
    let d = detail(&base, &nostr_adult).await;
    assert_eq!(d["summary"]["origin"], "both", "{d}");
    assert!(d.get("probe").is_none(), "{d}");
}
