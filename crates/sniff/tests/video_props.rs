//! Hostile input for the video recon: mutated, truncated and sparsely known containers never
//! panic (no parser panic is even caught), requests stay inside the file and bounded, and the
//! fetch loop terminates.

#[path = "video_fixtures/build.rs"]
mod build;

use build::*;
use proptest::prelude::*;
use sniff::video::{self, recommended_caps};
use sniff::{Range, SparseFile};

fn seeds() -> Vec<Vec<u8>> {
    let mkv = {
        let t = tracks(&[
            track(3, 2, "A_TRUEHD", &[]),
            track(1, 1, "V_MPEGH/ISO/HEVC", &el(&[0xE0], &uel(&[0xB0], 64))),
            track(2, 2, "A_EAC3", &[]),
            track(4, 2, "A_DTS", &[]),
        ]);
        let c = cluster(&[
            simple_block(1, &[0, 0, 0, 3, 0x4e, 1, 0x89, 0, 0, 0, 2, 0x26, 1]),
            simple_block(2, EAC3_JOC),
            simple_block(3, TRUEHD_ATMOS),
            simple_block(4, DTS_CORE),
        ]);
        let mut f = ebml_header("matroska");
        f.extend(segment(&[mkv_info(1000.0), t, c]));
        f
    };
    let mp4 = {
        let ftyp = bx(b"ftyp", b"isom\0\0\x02\0isom");
        let stsd = full(
            b"stsd",
            0,
            0,
            &[
                1u32.to_be_bytes().to_vec(),
                bx(
                    b"ec-3",
                    &[vec![0u8; 28], bx(b"dec3", &[0x0c, 0, 0x20, 0x0f, 0x00])].concat(),
                ),
            ]
            .concat(),
        );
        let stbl = bx(
            b"stbl",
            &[
                stsd,
                full(b"stsz", 0, 0, &[0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 6, 0]),
                full(b"stco", 0, 0, &[0, 0, 0, 1, 0, 0, 0, 40]),
            ]
            .concat(),
        );
        let mdia = bx(
            b"mdia",
            &[
                full(b"mdhd", 0, 0, &[0; 20]),
                full(b"hdlr", 0, 0, b"\0\0\0\0soun\0\0\0\0\0\0\0\0\0\0\0\0\0"),
                bx(b"minf", &stbl),
            ]
            .concat(),
        );
        let moov = bx(
            b"moov",
            &[
                full(b"mvhd", 0, 0, &[0; 96]),
                bx(b"trak", &[full(b"tkhd", 0, 1, &[0; 80]), mdia].concat()),
                bx(
                    b"mvex",
                    &full(
                        b"trex",
                        0,
                        0,
                        &[0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 4, 0, 0, 0, 6, 0, 0, 0, 0, 0],
                    ),
                ),
            ]
            .concat(),
        );
        let moof = bx(
            b"moof",
            &bx(
                b"traf",
                &[
                    full(b"tfhd", 0, 0x20000, &[0, 0, 0, 1]),
                    full(
                        b"trun",
                        0,
                        0x301,
                        &[
                            0, 0, 0, 2, 0, 0, 0, 100, 0, 0, 4, 0, 0, 0, 6, 0, 0, 0, 4, 0, 0, 0, 6,
                            0,
                        ],
                    ),
                ]
                .concat(),
            ),
        );
        let mut mdat = vec![0u8; 64];
        mdat.extend_from_slice(EAC3_JOC);
        let tfra = full(
            b"tfra",
            1,
            0,
            &[
                &[0, 0, 0, 1, 0, 0, 0, 0x3f, 0, 0, 0, 1][..],
                &[0u8; 16 + 12],
            ]
            .concat(),
        );
        let mfra = bx(
            b"mfra",
            &[tfra, full(b"mfro", 0, 0, &[0, 0, 0, 60])].concat(),
        );
        [ftyp, moov, moof, bx(b"mdat", &mdat), mfra].concat()
    };
    let ts = {
        let mut cc = 0u8;
        let mut f = ts_packets(0, &pat(0x100), &mut cc);
        f.extend(ts_packets(
            0x100,
            &pmt(&[
                (0x24, 0x1011, vec![0xb0, 5, 1, 0, 0x10, 0x3f, 0x10]),
                (0x83, 0x1100, vec![0x0a, 4, b'e', b'n', b'g', 0]),
                (0x84, 0x1101, vec![]),
            ]),
            &mut cc,
        ));
        f.extend(ts_packets(
            0x1011,
            &pes(
                0xe0,
                0,
                &[0, 0, 1, 0x42, 1, 0xff, 0, 0, 1, 0x4e, 1, 0x89, 2],
            ),
            &mut cc,
        ));
        f.extend(ts_packets(
            0x1100,
            &pes(0xfd, 90_000, TRUEHD_ATMOS),
            &mut cc,
        ));
        f.extend(ts_packets(0x1101, &pes(0xfd, 90_000, EAC3_JOC), &mut cc));
        f
    };
    let avi = {
        let mut strh = b"vids".to_vec();
        strh.extend_from_slice(&[0; 16]);
        strh.extend_from_slice(&1u32.to_le_bytes());
        strh.extend_from_slice(&25u32.to_le_bytes());
        strh.extend_from_slice(&[0; 32]);
        let mut strf = vec![40, 0, 0, 0, 64, 0, 0, 0, 48, 0, 0, 0, 1, 0, 24, 0];
        strf.extend_from_slice(b"XVID");
        strf.extend_from_slice(&[0; 20]);
        let chunk = |id: &[u8], b: &[u8]| {
            let mut v = id.to_vec();
            v.extend_from_slice(&(b.len() as u32).to_le_bytes());
            v.extend_from_slice(b);
            v
        };
        let strl = chunk(
            b"LIST",
            &[
                b"strl".to_vec(),
                chunk(b"strh", &strh),
                chunk(b"strf", &strf),
            ]
            .concat(),
        );
        let hdrl = chunk(
            b"LIST",
            &[b"hdrl".to_vec(), chunk(b"avih", &[0; 56]), strl].concat(),
        );
        let movi = chunk(
            b"LIST",
            &[
                b"movi".to_vec(),
                chunk(b"00dc", &[0, 0, 1, 0xb0, 0xf5, 0, 0, 1, 0x20]),
            ]
            .concat(),
        );
        let body = [b"AVI ".to_vec(), hdrl, movi].concat();
        chunk(b"RIFF", &body)
    };
    vec![mkv, mp4, ts, avi]
}

fn check_need(f: &SparseFile, need: &[Range]) {
    assert!(need.len() <= 64, "{} ranges", need.len());
    for w in need.windows(2) {
        assert!(w[0] <= w[1], "unsorted");
    }
    for r in need {
        assert!(
            r.len > 0 && r.offset < f.len() && r.end() <= f.len(),
            "{r} outside {}",
            f.len()
        );
        assert!(
            r.len <= recommended_caps().max_range + 32 * 1024,
            "{r} too large"
        );
    }
}

fn run(bytes: &[u8], claimed: u64, known: &[(u64, u64)]) {
    let before = sniff::panics_caught();
    let len = claimed.max(bytes.len() as u64);
    let mut f = SparseFile::new(len);
    for &(o, l) in known {
        let o = (o as usize).min(bytes.len());
        let e = o.saturating_add(l as usize).min(bytes.len());
        if e > o {
            f.insert(o as u64, bytes[o..e].to_vec());
        }
    }
    let caps = recommended_caps();
    let r = video::analyze(&f, &caps);
    check_need(&f, &r.need);
    // A short fetch loop over the same bytes terminates and stays in bounds.
    let mut rounds = 0;
    let mut r = r;
    while !r.need.is_empty() && rounds < 6 {
        for n in &r.need {
            let o = (n.offset as usize).min(bytes.len());
            let e = (n.end() as usize).min(bytes.len());
            if e > o {
                f.insert(o as u64, bytes[o..e].to_vec());
            }
        }
        r = video::analyze(&f, &caps);
        check_need(&f, &r.need);
        rounds += 1;
    }
    assert_eq!(sniff::panics_caught(), before, "a video parser panicked");
}

#[test]
fn seeds_parse() {
    for s in seeds() {
        let mut f = SparseFile::new(s.len() as u64);
        f.insert(0, s.clone());
        let r = video::analyze(&f, &recommended_caps());
        assert!(r.info.is_some(), "seed not recognised");
        run(&s, 0, &[(0, s.len() as u64)]);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 768, ..ProptestConfig::default() })]

    #[test]
    fn mutated_containers_never_panic(
        seed in 0usize..4,
        flips in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..24),
        cut in any::<usize>(),
        known in proptest::collection::vec((0u64..400_000, 1u64..70_000), 1..5),
        claimed in prop_oneof![Just(0u64), 0u64..(1u64 << 42)],
    ) {
        let mut b = seeds().swap_remove(seed);
        for (at, v) in flips {
            let n = b.len();
            if n > 0 {
                b[at % n] ^= v;
            }
        }
        let cut = cut % (b.len() + 1);
        b.truncate(cut.max(16.min(b.len())));
        let mut k = known;
        k.push((0, 4096));
        run(&b, claimed, &k);
    }

    #[test]
    fn random_bytes_never_panic(b in proptest::collection::vec(any::<u8>(), 0..4096), head in 0usize..4) {
        // Random tails behind real container magic.
        let magic: [&[u8]; 4] = [&[0x1a, 0x45, 0xdf, 0xa3], b"\0\0\0\x18ftypisom", b"RIFF\0\0\0\0AVI LIST", &[0x47]];
        let mut v = magic[head].to_vec();
        v.extend(b);
        if head == 3 {
            // Make the TS sync pattern hold.
            v.resize(188 * 4, 0);
            for i in 0..4 {
                v[i * 188] = 0x47;
            }
        }
        run(&v, 0, &[(0, v.len() as u64)]);
    }
}
