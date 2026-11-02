//! Classification from a torrent file list (paths + sizes, no bytes).

use sniff::Platform;
use sniff::listing::{Listing, classify};

fn one<'a>(ls: &'a [Listing], label: &str) -> &'a Listing {
    ls.iter()
        .find(|l| l.label == label)
        .unwrap_or_else(|| panic!("{label} not in {ls:#?}"))
}

#[test]
fn ps5_and_ps4_folder_dumps() {
    let ls = classify([
        ("PPSA12544-app/eboot.bin", 30_000_000u64),
        ("PPSA12544-app/sce_sys/param.json", 4_000),
        ("PPSA12544-app/sce_sys/icon0.png", 50_000),
        ("PPSA12544-app/sce_module/libc.prx", 1_000),
    ]);
    let l = one(&ls, "game.ps5.folder");
    assert_eq!(
        (
            l.platform,
            l.confidence,
            l.root.as_str(),
            l.title_id.as_deref()
        ),
        (Some(Platform::Ps5), 95, "PPSA12544-app", Some("PPSA12544"))
    );
    assert_eq!(
        l.metadata,
        vec!["PPSA12544-app/sce_sys/param.json".to_string()]
    );
    let ls = classify([
        ("Bloodborne/CUSA00207/eboot.bin", 1u64),
        ("Bloodborne/CUSA00207/sce_sys/param.sfo", 1),
        ("Bloodborne/CUSA00207/sce_sys/trophy/trophy00.trp", 1),
        ("Bloodborne/CUSA00207/sce_module/libc.prx", 1),
    ]);
    let l = one(&ls, "game.ps4.folder");
    assert_eq!(
        (l.platform, l.title_id.as_deref(), l.root.as_str()),
        (
            Some(Platform::Ps4),
            Some("CUSA00207"),
            "Bloodborne/CUSA00207"
        )
    );
    let ls = classify([
        ("PCSB00204/eboot.bin", 1u64),
        ("PCSB00204/sce_sys/param.sfo", 1),
        ("PCSB00204/sce_module/libfios2.suprx", 1),
    ]);
    assert_eq!(
        one(&ls, "game.vita.folder").platform,
        Some(Platform::PsVita)
    );
}

#[test]
fn other_console_folders() {
    let ls = classify([
        ("Red Dead Redemption [BLES00680]/PS3_DISC.SFB", 1u64),
        ("Red Dead Redemption [BLES00680]/PS3_GAME/PARAM.SFO", 1),
        (
            "Red Dead Redemption [BLES00680]/PS3_GAME/USRDIR/EBOOT.BIN",
            1,
        ),
    ]);
    let l = one(&ls, "game.ps3.folder");
    assert_eq!(
        (l.title_id.as_deref(), l.confidence),
        (Some("BLES00680"), 95)
    );
    assert_eq!(
        l.metadata,
        vec!["Red Dead Redemption [BLES00680]/PS3_GAME/PARAM.SFO".to_string()]
    );
    let ls = classify([
        ("Bayonetta/code/app.xml", 1u64),
        ("Bayonetta/code/Bayonetta.rpx", 1),
        ("Bayonetta/content/data.pak", 1),
        ("Bayonetta/meta/meta.xml", 1),
    ]);
    assert_eq!(one(&ls, "game.wiiu.folder").confidence, 95);
    let ls = classify([
        ("0005000010101D00/title.tmd", 1u64),
        ("0005000010101D00/title.tik", 1),
        ("0005000010101D00/00000000.app", 1),
    ]);
    assert_eq!(one(&ls, "game.wiiu.nus").platform, Some(Platform::WiiU));
    let ls = classify([("Halo/default.xbe", 1u64), ("Halo/media/x.xmv", 1)]);
    assert_eq!(one(&ls, "game.xbox.folder").root, "Halo");
    let ls = classify([
        (
            "Halo 3/4D5307E6/00007000/0123456789ABCDEF0123456789ABCDEF01234567",
            1u64,
        ),
        (
            "Halo 3/4D5307E6/00007000/0123456789ABCDEF0123456789ABCDEF01234567.data/Data0000",
            1,
        ),
    ]);
    let l = one(&ls, "game.x360.god");
    assert_eq!(
        (l.title_id.as_deref(), l.confidence),
        (Some("4D5307E6"), 95)
    );
}

#[test]
fn disc_folders() {
    let ls = classify([
        ("Movie.2019.1080p.BluRay/BDMV/index.bdmv", 1u64),
        ("Movie.2019.1080p.BluRay/BDMV/MovieObject.bdmv", 1),
        (
            "Movie.2019.1080p.BluRay/BDMV/STREAM/00000.m2ts",
            30_000_000_000,
        ),
        ("Movie.2019.1080p.BluRay/BDMV/META/DL/bdmt_eng.xml", 1),
        ("Movie.2019.1080p.BluRay/CERTIFICATE/id.bdmv", 1),
    ]);
    let l = one(&ls, "disc.bluray.folder");
    assert_eq!(
        (l.confidence, l.root.as_str()),
        (96, "Movie.2019.1080p.BluRay")
    );
    assert!(l.metadata.iter().any(|m| m.ends_with("bdmt_eng.xml")));
    assert_eq!(l.total_size, 30_000_000_004);
    let ls = classify([
        ("DVD/VIDEO_TS/VIDEO_TS.IFO", 1u64),
        ("DVD/VIDEO_TS/VTS_01_1.VOB", 1),
    ]);
    assert_eq!(one(&ls, "disc.dvd_video.folder").confidence, 96);
    // Streams without the index file.
    let ls = classify([("BDMV/STREAM/00001.m2ts", 1u64)]);
    assert_eq!(one(&ls, "disc.bluray.folder").root, "");
}

#[test]
fn split_files_and_multi_volume_archives() {
    let ls = classify([
        ("x/movie.part03.rar", 3u64),
        ("x/movie.part01.rar", 1),
        ("x/movie.part02.rar", 2),
    ]);
    let l = one(&ls, "archive.multipart");
    assert_eq!(l.split, Some("rar"));
    assert_eq!(
        l.parts,
        vec![
            "x/movie.part01.rar",
            "x/movie.part02.rar",
            "x/movie.part03.rar"
        ]
    );
    assert_eq!((l.total_size, l.confidence), (6, 92));
    let ls = classify([("a.rar", 1u64), ("a.r00", 1), ("a.r01", 1)]);
    assert_eq!(
        one(&ls, "archive.multipart").parts,
        vec!["a.rar", "a.r00", "a.r01"]
    );
    let ls = classify([("a.z01", 1u64), ("a.zip", 1)]);
    assert_eq!(one(&ls, "archive.multipart").parts, vec!["a.z01", "a.zip"]);
    let ls = classify([("b.7z.001", 1u64), ("b.7z.002", 1)]);
    assert_eq!(one(&ls, "archive.multipart").split, Some("7z"));
    // A gap in the numbering is still reported, at a lower confidence.
    let ls = classify([("b.7z.001", 1u64), ("b.7z.003", 1)]);
    assert_eq!(one(&ls, "archive.multipart").confidence, 80);
    // Single archives are not splits.
    assert!(classify([("a.rar", 1u64), ("b.zip", 1), ("c.7z.001", 1)]).is_empty());
    let ls = classify([
        ("Game [PPSA12544]/game_1.pkg", 4_294_967_296u64),
        ("Game [PPSA12544]/game_0.pkg", 4_294_967_296),
        ("Game [PPSA12544]/game_2.pkg", 100),
    ]);
    let l = one(&ls, "game.pkg.split");
    assert_eq!(
        l.parts,
        vec![
            "Game [PPSA12544]/game_0.pkg",
            "Game [PPSA12544]/game_1.pkg",
            "Game [PPSA12544]/game_2.pkg"
        ]
    );
    assert_eq!(
        (l.total_size, l.title_id.as_deref()),
        (8_589_934_692, Some("PPSA12544"))
    );
    let ls = classify([("Zelda.nsp/00", 1u64), ("Zelda.nsp/01", 1)]);
    assert_eq!(
        one(&ls, "game.switch.nsp.split").platform,
        Some(Platform::Switch)
    );
}

#[test]
fn hostile_lists_stay_bounded() {
    let names: Vec<String> = (0..50_000).map(|i| format!("d/f.part{i}.rar")).collect();
    let ls = classify(names.iter().map(|n| (n.as_str(), 1u64)));
    let l = one(&ls, "archive.multipart");
    assert!(l.parts.len() <= 1024);
    let long = "a/".repeat(5000);
    assert!(classify([(long.as_str(), 1u64), ("", 0), ("\\", 0)]).is_empty());
    let ls = classify([("PS3_GAME/PARAM.SFO", u64::MAX), ("PS3_DISC.SFB", u64::MAX)]);
    assert_eq!(one(&ls, "game.ps3.folder").total_size, u64::MAX);
}

/// Review finding: every marker rescanned the whole list (and the result list), so 40 000
/// `*/sce_sys/param.sfo` entries took over a minute. Lookups are binary searches now and scans
/// share a budget; nested roots and many split groups are bounded too.
#[test]
fn many_markers_classify_in_near_linear_time() {
    let t = std::time::Instant::now();
    let names: Vec<String> = (0..40_000)
        .map(|i| format!("g{i:06}/sce_sys/param.sfo"))
        .collect();
    let ls = classify(names.iter().map(|n| (n.as_str(), 1u64)));
    assert_eq!(ls.len(), 64);
    assert!(ls.iter().all(|l| l.total_size == 1));
    // Nested roots: each one's subtree holds everything below it.
    let mut names: Vec<String> = (0..400)
        .map(|d| format!("{}sce_sys/param.sfo", "a/".repeat(d)))
        .collect();
    names.extend((0..20_000).map(|i| format!("{}x/{i}.prx", "a/".repeat(399))));
    let ls = classify(names.iter().map(|n| (n.as_str(), 1u64)));
    assert!(!ls.is_empty());
    // Many split groups after filler.
    let mut names: Vec<String> = (0..190_000).map(|i| format!("filler/{i}.bin")).collect();
    names.extend((0..8192).map(|j| format!("d/m{:06}.part{}.rar", j / 2, j % 2 + 1)));
    let ls = classify(names.iter().map(|n| (n.as_str(), 1u64)));
    assert_eq!(ls.len(), 64);
    let took = t.elapsed();
    assert!(
        took < std::time::Duration::from_secs(20),
        "classify took {took:?}"
    );
}

/// Review finding: the listing labels were missing from the label registry (no MIME type or
/// extensions through `label::info`), while an unused `archive.split_part` id sat there.
#[test]
fn listing_labels_are_registered() {
    let documented = [
        "game.ps5.folder",
        "game.ps4.folder",
        "game.vita.folder",
        "game.ps3.folder",
        "game.psp.folder",
        "game.wiiu.folder",
        "game.wiiu.nus",
        "game.xbox.folder",
        "game.x360.folder",
        "game.x360.god",
        "game.pkg.split",
        "game.switch.nsp.split",
        "disc.bluray.folder",
        "disc.dvd_video.folder",
        "archive.multipart",
    ];
    for l in documented {
        assert!(sniff::label::info(l).is_some(), "{l} not registered");
    }
    assert!(sniff::label::info("archive.split_part").is_none());
    let ls = classify([
        ("a/PPSA12544-app/sce_sys/param.json", 1u64),
        ("b/BLES00680/PS3_GAME/PARAM.SFO", 1),
        ("c/BDMV/index.bdmv", 1),
        ("d/VIDEO_TS/VIDEO_TS.IFO", 1),
        ("e/Movie.part1.rar", 1),
        ("e/Movie.part2.rar", 1),
        ("f/game.nsp/00", 1),
        ("f/game.nsp/01", 1),
        ("g/Game_0.pkg", 1),
        ("g/Game_1.pkg", 1),
        (
            "h/4D5307E6/00007000/0123456789abcdef0123456789abcdef01234567",
            1,
        ),
    ]);
    assert!(ls.len() >= 8, "{ls:#?}");
    for l in &ls {
        assert!(documented.contains(&l.label), "{} undocumented", l.label);
        assert!(sniff::label::info(l.label).is_some(), "{}", l.label);
    }
}
