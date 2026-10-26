//! The BEP 9 fetcher against a real client: an in-process librqbit 9.0.1 session seeds a
//! generated multi-file torrent on 127.0.0.1 (TCP only, no DHT, no trackers), and the fetcher
//! downloads and verifies its info dict (docs/DHT.md §4). Needs the `downloads` feature too
//! (librqbit), which `cargo test --workspace` enables through the server crate.
#![cfg(all(feature = "dht-crawl", feature = "downloads"))]

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::time::Duration;

use librqbit::{AddTorrent, AddTorrentOptions, ListenerOptions, Session, SessionOptions};
use magnetar_core::liveness::bencode::{self, Value};
use magnetar_core::metainfo::{FetchConfig, Fetcher, Outcome};

fn dict(mut e: Vec<(&str, Value)>) -> Value {
    e.sort_by(|a, b| a.0.cmp(b.0));
    Value::Dict(
        e.into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    )
}

fn b(s: &str) -> Value {
    Value::Bytes(s.as_bytes().to_vec())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fetches_metadata_from_a_librqbit_seed() {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("metainfo-librqbit-{:08x}", rand::random::<u32>()));
    let name = "Generated.Test.Torrent.2026";
    let root = base.join(name);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    // Two files of ~300 KiB in total, plus the extra files below.
    let files: Vec<(Vec<&str>, Vec<u8>)> = vec![
        (
            vec!["a.bin"],
            (0..200_000u32).map(|i| (i % 251) as u8).collect(),
        ),
        (
            vec!["sub", "b.bin"],
            (0..100_001u32).map(|i| (i % 13) as u8).collect(),
        ),
    ];
    let piece_len = 16 * 1024; // small pieces: a bigger `pieces` string, several metadata blocks
    let mut all = Vec::new();
    for (path, data) in &files {
        let mut p = root.clone();
        for part in path {
            p = p.join(part);
        }
        std::fs::write(&p, data).unwrap();
        all.extend_from_slice(data);
    }
    let mut pieces = Vec::new();
    for chunk in all.chunks(piece_len) {
        pieces.extend_from_slice(&sha1_smol::Sha1::from(chunk).digest().bytes());
    }
    // 400 small extra files make the info dict span several 16 KiB metadata blocks.
    let mut file_list: Vec<Value> = files
        .iter()
        .map(|(path, data)| {
            dict(vec![
                ("length", Value::Int(data.len() as i64)),
                ("path", Value::List(path.iter().map(|p| b(p)).collect())),
            ])
        })
        .collect();
    let mut extra = Vec::new();
    for i in 0..400 {
        let n = format!("extra-file-with-a-longish-name-{i:04}.txt");
        let data = format!("{i}\n").into_bytes();
        std::fs::write(root.join(&n), &data).unwrap();
        extra.push((n, data));
    }
    // Pieces must cover the extra files too: rebuild the concatenation in list order.
    for (n, data) in &extra {
        file_list.push(dict(vec![
            ("length", Value::Int(data.len() as i64)),
            ("path", Value::List(vec![b(n)])),
        ]));
        all.extend_from_slice(data);
    }
    pieces.clear();
    for chunk in all.chunks(piece_len) {
        pieces.extend_from_slice(&sha1_smol::Sha1::from(chunk).digest().bytes());
    }
    let info = dict(vec![
        ("files", Value::List(file_list)),
        ("name", b(name)),
        ("piece length", Value::Int(piece_len as i64)),
        ("pieces", Value::Bytes(pieces)),
    ]);
    let info_raw = bencode::encode(&info);
    assert!(info_raw.len() > 16 * 1024, "several metadata blocks");
    let info_hash = sha1_smol::Sha1::from(&info_raw).digest().bytes();
    let torrent = bencode::encode(&dict(vec![("info", info)]));

    let session = Session::new_with_opts(
        base.clone(),
        SessionOptions {
            dht: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            listen: Some(ListenerOptions {
                listen_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                ipv4_only: true,
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .expect("librqbit session");
    let handle = session
        .add_torrent(
            AddTorrent::from_bytes(torrent),
            Some(AddTorrentOptions {
                overwrite: true,
                output_folder: Some(root.to_string_lossy().into_owned()),
                ..Default::default()
            }),
        )
        .await
        .expect("add torrent")
        .into_handle()
        .expect("handle");
    // Wait until the existing files are checked and the torrent is complete (seeding).
    let t0 = std::time::Instant::now();
    loop {
        let st = handle.stats();
        assert!(st.error.is_none(), "{:?}", st.error);
        if st.finished {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "never finished: {st}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let port = session.listen_addr().expect("listen address").port();
    let peer = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);

    let f = Fetcher::new(FetchConfig {
        allow_loopback: true,
        connects_per_sec: 1000,
        ..FetchConfig::default()
    });
    let out = f.fetch(info_hash, &[peer]).await.expect("fetched");
    let Outcome::Ok(got) = out else {
        panic!("{out:?}")
    };
    assert_eq!(got.v1, info_hash);
    assert_eq!(got.metadata_size, info_raw.len());
    assert_eq!(got.info.name, name);
    assert_eq!(got.info.files.len(), 402);
    assert_eq!(got.info.files[1].path, "sub/b.bin");
    assert_eq!(got.info.total_size, all.len() as u64);
    let st = f.stats();
    eprintln!("{}", serde_json::to_string(&st).unwrap());
    assert_eq!(st.ok, 1);

    session.stop().await;
    let _ = std::fs::remove_dir_all(&base);
}
