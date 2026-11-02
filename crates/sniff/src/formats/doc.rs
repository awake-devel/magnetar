//! Documents needing structure: PDF (header anywhere in the first KiB, `%%EOF` in the tail) and
//! Palm database e-books (type/creator at 60). Simple magics live in `data/signatures/doc.toml`.

use crate::types::Hit;
use crate::util::*;
use crate::view::View;

pub(crate) static CHECKS: &[(&str, crate::formats::Check)] = &[("pdf", pdf), ("palm_db", palm_db)];

fn pdf(v: &View) -> Option<Hit> {
    let h = v.head();
    let i = find(pre(h, 1024), b"%PDF-")?;
    let ver = lossy(sl(h, i + 5, i + 8));
    // 95+ needs structure past the header: an indirect object (`1 0 obj`) in the head, or
    // the `%%EOF` trailer marker.
    let obj = contains(pre(h, 16384), b" obj") || contains(pre(h, 16384), b"\nobj");
    let base = match (i == 0, obj) {
        (true, true) => 97,
        (true, false) => 90,
        (false, _) => 85,
    };
    let mut hit = Hit::new("doc.pdf", base).d("version", ver);
    if let Some(t) = v.tail(1024)
        && rfind(t, b"%%EOF").is_some()
    {
        hit.conf = 99;
        hit.push("eof_marker", true);
    }
    if contains(pre(h, 4096), b"/Linearized") {
        hit.push("linearized", true);
    }
    Some(hit)
}

fn palm_db(v: &View) -> Option<Hit> {
    let h = v.head();
    let tc = h.get(60..68)?;
    let name_ok = pre(h, 32)
        .iter()
        .take_while(|&&c| c != 0)
        .all(|&c| c >= 0x20);
    if !name_ok {
        return None;
    }
    match tc {
        b"BOOKMOBI" => {
            let mut hit = Hit::new("doc.mobi", 96);
            // Record 0 offset at 78; MOBI header at rec0+16, version at +36 (8 => KF8/azw3).
            if let Some(r0) = u32be(h, 78) {
                let r0 = us(u64::from(r0));
                if at(h, r0.saturating_add(16), b"MOBI")
                    && let Some(ver) = u32be(h, r0.saturating_add(16 + 20 + 16))
                {
                    hit.push("mobi_version", ver);
                }
            }
            Some(hit)
        }
        b"TEXtREAd" => Some(Hit::new("doc.palmdoc", 95)),
        b"PNRdPPrs" => Some(Hit::new("doc.ereader", 95)),
        b"TPZ3BOOK" | b"PDBXBOOK" => Some(Hit::new("doc.mobi", 85)),
        _ => None,
    }
}
