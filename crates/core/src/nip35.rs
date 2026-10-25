//! NIP-35 parser (lenient, D4). Owner: nostr agent. See nostr-nips.md §2.

use crate::magnet::{normalize_infohash, normalize_infohash_v2};
use crate::model::{
    Category, Comment, KIND_COMMENT, KIND_TORRENT, KIND_TORRENT_COMMENT, ParsedTorrent, RawEvent,
    Rejection, TorrentFile,
};
use crate::pow::effective_pow;

pub const MAX_DESCRIPTION_BYTES: usize = 64 * 1024;
pub const MAX_COMMENT_BYTES: usize = 64 * 1024;
pub const MAX_TITLE_BYTES: usize = 1024;
pub const MAX_FILES: usize = 5000;
/// Scraped "folder listing" file names (Nyaa bridge) are kept but capped to this.
pub const MAX_FILE_PATH_BYTES: usize = 1024;
pub const MAX_TRACKERS: usize = 30;
pub const MAX_HASHTAGS: usize = 64;
pub const MAX_EXTERNAL_IDS: usize = 64;

/// Parse a verified kind-2003 event. Err(NoInfohash) when neither x nor btih yields a v1 hash.
pub fn parse_torrent(raw: &RawEvent) -> Result<ParsedTorrent, Rejection> {
    if raw.kind != KIND_TORRENT {
        return Err(Rejection::UnsupportedKind);
    }
    // dtan skips every tag whose value is missing or empty; so do we.
    let tags = || {
        raw.tags
            .iter()
            .filter(|t| t.len() >= 2 && !t[1].trim().is_empty())
            .map(|t| (t[0].as_str(), t))
    };
    let values = |name: &'static str| {
        tags()
            .filter(move |(n, _)| *n == name)
            .map(|(_, t)| t[1].trim())
    };

    let infohash = values("x")
        .chain(values("btih"))
        .find_map(normalize_infohash)
        .ok_or(Rejection::NoInfohash)?;
    let infohash_v2 = values("x").find_map(normalize_infohash_v2);

    let mut files = Vec::new();
    let mut sizes_known = true;
    let mut files_sum: u64 = 0;
    for (_, t) in tags().filter(|(n, _)| *n == "file") {
        let file = parse_file(t);
        match file.size {
            Some(s) => files_sum = files_sum.saturating_add(s),
            None => sizes_known = false,
        }
        if files.len() < MAX_FILES {
            files.push(file);
        }
    }
    let size_tag = values("size").find_map(|s| s.parse::<u64>().ok());
    let total_size = if !files.is_empty() && sizes_known && files_sum > 0 {
        Some(files_sum)
    } else {
        size_tag.filter(|s| *s > 0)
    };

    let mut trackers: Vec<String> = Vec::new();
    for v in values("tracker") {
        for t in tracker_candidates(v) {
            if trackers.len() < MAX_TRACKERS && !trackers.contains(&t) {
                trackers.push(t);
            }
        }
    }

    let mut hashtags: Vec<String> = Vec::new();
    for t in values("t").map(str::to_lowercase) {
        if hashtags.len() < MAX_HASHTAGS && !hashtags.contains(&t) {
            hashtags.push(t);
        }
    }

    let mut external_ids: Vec<(String, String)> = Vec::new();
    let mut tcat: Option<Vec<String>> = None;
    let i_tags = values("i").filter_map(|v| {
        let (k, v) = v.split_once(':')?;
        Some((k.trim().to_lowercase(), v.trim().to_string()))
    });
    // `imdb` as a top-level tag is the "v0" form.
    let legacy_imdb = values("imdb").map(|v| ("imdb".to_string(), v.to_string()));
    for (k, v) in i_tags.chain(legacy_imdb) {
        if k.is_empty() || v.is_empty() {
            continue;
        }
        if k == "tcat" && tcat.is_none() {
            tcat = Some(
                v.split(',')
                    .map(|c| c.trim().to_lowercase())
                    .filter(|c| !c.is_empty())
                    .collect(),
            );
        }
        let pair = (k, v);
        if external_ids.len() < MAX_EXTERNAL_IDS && !external_ids.contains(&pair) {
            external_ids.push(pair);
        }
    }
    let category_path = tcat
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| hashtags.iter().take(3).cloned().collect());
    let category = category_of(&category_path, &hashtags);

    // NIP-48 says ["proxy", <id>, <protocol>]; bridges send ["proxy", "web", <url>]. Either order.
    let source_url = tags()
        .filter(|(n, _)| *n == "proxy")
        .flat_map(|(_, t)| t[1..].iter().take(2))
        .map(|s| s.trim())
        .find(|s| is_web_url(s))
        .map(str::to_string);

    let published_at =
        values("published_at").find_map(|s| s.parse::<i64>().ok().filter(|t| *t > 0));

    let title = values("title")
        .map(|t| cap(t, MAX_TITLE_BYTES))
        .next()
        .or_else(|| {
            files
                .iter()
                .find(|f| !f.path.contains('\n'))
                .or(files.first())
                .and_then(|f| f.path.lines().map(str::trim).find(|l| !l.is_empty()))
                .map(|p| cap(p, MAX_TITLE_BYTES))
        })
        .unwrap_or_else(|| infohash.clone());

    let (pow_bits, pow_committed) = effective_pow(&raw.id, &raw.tags);

    Ok(ParsedTorrent {
        event_id: raw.id.clone(),
        pubkey: raw.pubkey.clone(),
        created_at: raw.created_at,
        infohash,
        infohash_v2,
        title,
        description: cap(&raw.content, MAX_DESCRIPTION_BYTES),
        files,
        total_size,
        trackers,
        category,
        category_path,
        hashtags,
        external_ids,
        source_url,
        published_at,
        pow_bits,
        pow_committed,
    })
}

/// `["file", path, size, ...]`, or the u2p bridge's single-element `["file", "path;size"]`.
fn parse_file(t: &[String]) -> TorrentFile {
    let mut path = t[1].as_str();
    let mut size = t.get(2).and_then(|s| s.trim().parse::<u64>().ok());
    if t.get(2).is_none_or(|s| s.trim().is_empty())
        && let Some((p, s)) = path.rsplit_once(';')
        && !s.is_empty()
        && s.bytes().all(|b| b.is_ascii_digit())
    {
        path = p;
        size = s.parse().ok();
    }
    TorrentFile {
        path: cap(path.trim(), MAX_FILE_PATH_BYTES),
        size,
    }
}

/// Valid trackers inside one tag value. Values are sometimes several URLs glued with spaces or
/// `&tr=`; machine-translated ones (`udp: //host: 451 / anunciar`) yield nothing.
fn tracker_candidates(v: &str) -> impl Iterator<Item = String> + '_ {
    v.split(char::is_whitespace)
        .flat_map(|p| p.split("&tr="))
        .filter_map(valid_tracker)
}

/// udp/http/https (scrapeable) and wss (WebTorrent, display only) with a host; udp needs a port.
pub fn valid_tracker(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s.len() > 512 {
        return None;
    }
    let u = url::Url::parse(s).ok()?;
    let host = u.host_str().filter(|h| !h.is_empty())?;
    match u.scheme() {
        "http" | "https" | "wss" => {}
        "udp" if u.port().is_some() => {}
        _ => return None,
    }
    if host.contains(['%', '\\']) || !u.username().is_empty() {
        return None;
    }
    Some(u.to_string())
}

fn is_web_url(s: &str) -> bool {
    url::Url::parse(s)
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}

/// Trimmed, at most `max` bytes (cut on a char boundary).
fn cap(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].trim_end().to_string()
}

/// Parse a kind 2004 (NIP-10 e tags, any marker position) or kind 1111 (`K`=2003, `E` root).
/// Returns None if no torrent reference can be found.
pub fn parse_comment(raw: &RawEvent) -> Option<Comment> {
    let torrent_event_id = match raw.kind {
        KIND_TORRENT_COMMENT => nip10_root(&raw.tags)?,
        KIND_COMMENT => nip22_root(&raw.tags)?,
        _ => return None,
    };
    Some(Comment {
        event_id: raw.id.clone(),
        pubkey: raw.pubkey.clone(),
        created_at: raw.created_at,
        kind: raw.kind,
        torrent_event_id,
        content: cap(&raw.content, MAX_COMMENT_BYTES),
    })
}

/// Lowercase 64-hex event id from a tag value.
fn tag_event_id(t: &[String]) -> Option<String> {
    let id = t.get(1)?.trim().to_ascii_lowercase();
    crate::model::is_hex32(&id).then_some(id)
}

/// Torrent id referenced by a kind 2004. Real shapes: `["e",id,"","root"]`, `["e",id,"","reply"]`,
/// `["e",id]` and `["e",id,"","",pubkey,"root"]` (marker at index 5), so the marker is searched
/// for after the relay hint. Preference: root, uppercase `E`, first unmarked, reply.
fn nip10_root(tags: &[Vec<String>]) -> Option<String> {
    let marker = |t: &[String]| {
        t.iter()
            .skip(3)
            .map(|m| m.trim().to_ascii_lowercase())
            .find(|m| matches!(m.as_str(), "root" | "reply" | "mention"))
    };
    let e_tags = || {
        tags.iter()
            .filter(|t| t.first().map(String::as_str) == Some("e"))
            .filter_map(|t| Some((tag_event_id(t)?, marker(t))))
    };
    let upper_e = || {
        tags.iter()
            .filter(|t| t.first().map(String::as_str) == Some("E"))
            .find_map(|t| tag_event_id(t))
    };
    e_tags()
        .find(|(_, m)| m.as_deref() == Some("root"))
        .map(|(id, _)| id)
        .or_else(upper_e)
        .or_else(|| e_tags().find(|(_, m)| m.is_none()).map(|(id, _)| id))
        .or_else(|| {
            e_tags()
                .find(|(_, m)| m.as_deref() == Some("reply"))
                .map(|(id, _)| id)
        })
}

/// NIP-22: root scope `["E", id, relay, pubkey]` with `["K", "2003"]`. A top-level comment's
/// parent `e` equals the root, so `e` is the fallback when `E` is missing.
fn nip22_root(tags: &[Vec<String>]) -> Option<String> {
    let has = |name: &str, value: &str| {
        tags.iter().any(|t| {
            t.first().map(String::as_str) == Some(name) && t.get(1).map(|v| v.trim()) == Some(value)
        })
    };
    if !has("K", "2003") {
        return None;
    }
    let first = |name: &str| {
        tags.iter()
            .filter(|t| t.first().map(String::as_str) == Some(name))
            .find_map(|t| tag_event_id(t))
    };
    first("E").or_else(|| has("k", "2003").then(|| first("e")).flatten())
}

/// Category mapping (ARCHITECTURE.md §3.1) from tcat path / t tags.
///
/// The first element (tcat path, else first `t`) decides; if it maps to nothing, the remaining
/// path elements and then the hashtags are tried, so `other,e-book` is a Book and YGG's
/// `[cbz, mangas, …, ebook]` is not lost to "cbz".
pub fn category_of(category_path: &[String], hashtags: &[String]) -> Category {
    category_path
        .iter()
        .chain(hashtags)
        .find_map(|c| map_category(&c.trim().to_lowercase()))
        .unwrap_or(Category::Other)
}

fn map_category(c: &str) -> Option<Category> {
    use Category::*;
    let exact = match c {
        "video" | "movie" | "tv" | "hd" | "uhd" | "4k" | "series" => Some(Video),
        "audio" | "music" | "flac" | "lossless" | "audio-book" | "audiobook" => Some(Audio),
        "application" | "software" | "windows" | "mac" | "unix" | "linux" | "android" | "ios" => {
            Some(Application)
        }
        "game" | "games" | "pc" | "psx" | "xbox" | "wii" => Some(Game),
        "porn" | "xxx" | "adult" => Some(Porn),
        "anime" => Some(Anime),
        "book" | "e-book" | "ebook" | "comic" | "manga" => Some(Book),
        "picture" | "pictures" | "image" => Some(Picture),
        _ => None,
    };
    // YGG French values, by substring. `jeu` before `vidéo` so "jeu-vidéo" is a Game.
    exact.or_else(|| {
        [
            ("jeu", Game),
            ("film", Video),
            ("série", Video),
            ("vidéo", Video),
            ("musique", Audio),
            ("logiciel", Application),
            ("ebook", Book),
            ("livre", Book),
        ]
        .into_iter()
        .find(|(needle, _)| c.contains(needle))
        .map(|(_, cat)| cat)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn categories() {
        assert_eq!(
            category_of(&s(&["video", "movie", "4k"]), &[]),
            Category::Video
        );
        assert_eq!(category_of(&s(&["anime", "video"]), &[]), Category::Anime);
        assert_eq!(
            category_of(&s(&["Audio", "lossless"]), &[]),
            Category::Audio
        );
        assert_eq!(category_of(&s(&["software"]), &[]), Category::Application);
        assert_eq!(category_of(&s(&["pictures"]), &[]), Category::Picture);
        assert_eq!(category_of(&s(&["manga"]), &[]), Category::Book);
        assert_eq!(category_of(&s(&["porn", "picture"]), &[]), Category::Porn);
        assert_eq!(category_of(&s(&["other"]), &[]), Category::Other);
        assert_eq!(category_of(&s(&["other", "e-book"]), &[]), Category::Book);
        assert_eq!(category_of(&[], &s(&["xxx", "films"])), Category::Porn);
        assert_eq!(category_of(&[], &s(&["filmvidéo"])), Category::Video);
        assert_eq!(category_of(&[], &s(&["série tv"])), Category::Video);
        assert_eq!(category_of(&[], &s(&["jeu-vidéo"])), Category::Game);
        assert_eq!(category_of(&[], &s(&["musique"])), Category::Audio);
        assert_eq!(category_of(&[], &s(&["logiciel"])), Category::Application);
        assert_eq!(category_of(&[], &s(&["livres"])), Category::Book);
        assert_eq!(category_of(&[], &[]), Category::Other);
        assert_eq!(category_of(&s(&["assumeutxo"]), &[]), Category::Other);
    }

    #[test]
    fn trackers_validated() {
        assert_eq!(
            valid_tracker("udp://tracker.opentrackr.org:1337/announce").as_deref(),
            Some("udp://tracker.opentrackr.org:1337/announce")
        );
        assert!(valid_tracker("udp://tracker.example.org/announce").is_none());
        assert!(valid_tracker("udp: //tracker.torrent.eu.org: 451 / anunciar").is_none());
        assert!(valid_tracker("").is_none());
        assert!(valid_tracker("magnet:?xt=urn:btih:abc").is_none());
        assert!(valid_tracker("wss://tracker.webtorrent.dev").is_some());
        let glued = "udp://a.example:6969 udp://b.example:80&tr= udp://c.example:1337";
        let got: Vec<String> = tracker_candidates(glued).collect();
        assert_eq!(
            got,
            s(&[
                "udp://a.example:6969",
                "udp://b.example:80",
                "udp://c.example:1337"
            ])
        );
    }

    #[test]
    fn cap_on_char_boundary() {
        assert_eq!(cap("  ééé  ", 3), "é");
        assert_eq!(cap("abc", 10), "abc");
    }
}
