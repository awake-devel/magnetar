//! Server-side classification of DHT torrents (D52, D69): `core::classify`, plus the anime
//! refinement through `anitomy-ng` (already a server dependency for D41, so the phone library
//! stays small). Pure and silent: it runs before the blocklist decision and never logs.

use anitomy_ng::ElementKind;
use magnetar_core::classify::{self, Classification, Rule, fansub_group};
use magnetar_core::model::{Category, TorrentFile};

/// `[Group] Title - 05 …` read by anitomy-ng: a release group in the first bracket that is not
/// a website/tagger, and an episode number, without a western `SxxEyy` token.
fn anitomy_anime(name: &str) -> bool {
    let n = name.trim();
    let Some((group, _)) = n.strip_prefix('[').and_then(|r| r.split_once(']')) else {
        return false;
    };
    if !fansub_group(group) {
        return false;
    }
    let western = n.split(|c: char| !c.is_ascii_alphanumeric()).any(|t| {
        let t = t.to_ascii_lowercase();
        let b = t.as_bytes();
        b.len() >= 6
            && b[0] == b's'
            && b[1].is_ascii_digit()
            && t[1..].find('e').is_some_and(|e| {
                t[1..1 + e].bytes().all(|x| x.is_ascii_digit())
                    && t[2 + e..].bytes().all(|x| x.is_ascii_digit())
                    && t.len() > 2 + e
            })
    });
    if western {
        return false;
    }
    let els = anitomy_ng::parse(n, anitomy_ng::Options::default());
    let has = |k: ElementKind| {
        els.iter()
            .any(|e| e.kind == k && !e.value.trim().is_empty())
    };
    has(ElementKind::ReleaseGroup) && (has(ElementKind::Episode) || has(ElementKind::FileChecksum))
}

/// Classify a DHT torrent by its display name and files.
pub fn classify(name: &str, files: &[TorrentFile]) -> Classification {
    let mut c = classify::classify(name, files);
    if c.category == Category::Video && anitomy_anime(name) {
        c.category = Category::Anime;
        c.rule = Rule::Anime;
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(name: &str) -> Category {
        classify(
            name,
            &[TorrentFile {
                path: "a.mkv".into(),
                size: Some(1 << 30),
            }],
        )
        .category
    }

    #[test]
    fn anime_refinement() {
        assert_eq!(video("[Group] Some Show - 05 (1080p).mkv"), Category::Anime);
        assert_eq!(video("[Group] Some Show 05 [1080p]"), Category::Anime);
        assert_eq!(video("[Group] Some.Show.S01E05.1080p"), Category::Video);
        assert_eq!(video("[ www.site.org ] Some Show 05"), Category::Video);
        assert_eq!(video("Some.Film.2020.1080p.BluRay"), Category::Video);
    }
}
