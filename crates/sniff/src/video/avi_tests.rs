//! `avi::tests`.

use super::super::testkit::info;

fn chunk(id: &[u8], b: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend_from_slice(&(b.len() as u32).to_le_bytes());
    v.extend_from_slice(b);
    if b.len() % 2 == 1 {
        v.push(0);
    }
    v
}

fn vids(fourcc: &[u8; 4], name: Option<&str>) -> Vec<u8> {
    let mut strh = b"vids".to_vec();
    strh.extend_from_slice(&[0; 16]);
    strh.extend_from_slice(&1u32.to_le_bytes());
    strh.extend_from_slice(&25u32.to_le_bytes());
    strh.extend_from_slice(&[0; 32]);
    let mut strf = vec![40, 0, 0, 0, 64, 0, 0, 0, 48, 0, 0, 0, 1, 0, 24, 0];
    strf.extend_from_slice(fourcc);
    strf.extend_from_slice(&[0; 20]);
    let mut kids = vec![
        b"strl".to_vec(),
        chunk(b"strh", &strh),
        chunk(b"strf", &strf),
    ];
    if let Some(n) = name {
        kids.push(chunk(b"strn", format!("{n}\0").as_bytes()));
    }
    chunk(b"LIST", &kids.concat())
}

#[test]
fn xsub_stream_is_a_subtitle() {
    let hdrl = chunk(
        b"LIST",
        &[
            b"hdrl".to_vec(),
            chunk(b"avih", &[0; 56]),
            vids(b"XVID", None),
            vids(b"DXSB", Some("English SDH")),
            vids(b"DXSA", None),
        ]
        .concat(),
    );
    let movi = chunk(b"LIST", b"movi");
    let f = chunk(b"RIFF", &[b"AVI ".to_vec(), hdrl, movi].concat());
    let i = info(&f);
    assert_eq!(i.video.len(), 1);
    assert_eq!(i.video[0].codec, "mpeg4");
    assert_eq!(i.subtitles.len(), 2, "{:?}", i.subtitles);
    assert!(i.subtitles.iter().all(|s| s.codec == "xsub"));
    assert_eq!(i.subtitles[0].id, 1);
    assert_eq!(i.subtitles[0].name.as_deref(), Some("English SDH"));
    assert_eq!(
        i.subtitles[0]
            .kinds
            .iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>(),
        ["sdh"]
    );
}
