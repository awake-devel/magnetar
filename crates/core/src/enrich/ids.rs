//! Strong external IDs (confidence 1.0, D40): NIP-35 `i` tags and links in the description.
//!
//! `i` tags arrive split at the first `:` (D4), so `tmdb:movie:603` is `("tmdb", "movie:603")`.
//! Forms seen in production (2026-10-25): `imdb:tt…` (also a full imdb.com URL in the legacy
//! top-level `imdb` tag), `tmdb:movie:N`, `ttvdb:series:N` / `ttvdb:movies:N`, `mal:anime:N`,
//! `anilist:anime:N` (nostr-nips.md §2.4). Description links: imdb.com/(xx/)title/tt…,
//! themoviedb.org/(movie|tv)/N[/season/S[/episode/E]], thetvdb.com/dereferrer/series/N,
//! thetvdb.com/?tab=series&id=N, myanimelist.net/anime/N, anilist.co/anime/N,
//! anidb.net/anime/N, anidb.net/perl-bin/animedb.pl?show=anime&aid=N.
//!
//! Descriptions sometimes carry a second, unrelated link of the same kind (the YGG template
//! links themoviedb.org/movie/324544 under every release of one uploader), always after the real
//! one: the first link of each kind wins and a description with several distinct IDs of one kind
//! is flagged `ambiguous` (confidence 0.9 instead of 1.0). The same template link also sits next
//! to the real link of *another* kind (an imdb.com link), so a description is trusted only for
//! its [`StrongIds::main_link`]; its other links are evidence, never attached to a media row.

use crate::enrich::method;

/// IDs found for one torrent. Season/episode come from a TMDB episode link.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StrongIds {
    /// `tt` + at least 7 digits.
    pub imdb: Option<String>,
    pub tmdb_movie: Option<u32>,
    pub tmdb_tv: Option<u32>,
    /// TheTVDB series id (TVDB movie ids live in another namespace and are ignored).
    pub tvdb: Option<u32>,
    pub mal: Option<u32>,
    pub anilist: Option<u32>,
    pub anidb: Option<u32>,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    /// Several distinct IDs of one kind in one description (the first one is kept).
    pub ambiguous: bool,
    /// `method::ITAG` if any ID came from an `i` tag, else `method::DESC_LINK`.
    pub method: &'static str,
    /// Description only: the kind of the earliest link (template links come after the real one).
    pub lead: Option<IdKind>,
}

/// One external id column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdKind {
    Imdb,
    TmdbMovie,
    TmdbTv,
    Tvdb,
    Mal,
    Anilist,
    Anidb,
}

impl StrongIds {
    pub fn is_empty(&self) -> bool {
        self.imdb.is_none()
            && self.tmdb_movie.is_none()
            && self.tmdb_tv.is_none()
            && self.tvdb.is_none()
            && self.mal.is_none()
            && self.anilist.is_none()
            && self.anidb.is_none()
    }

    /// 1.0, or 0.9 when a description held conflicting IDs of one kind.
    pub fn confidence(&self) -> f32 {
        if self.ambiguous { 0.9 } else { 1.0 }
    }

    /// The IDs a description is trusted for on its own: its IMDb link, else its earliest link
    /// (with that link's season/episode). The others may be template noise (module docs).
    pub fn main_link(&self) -> StrongIds {
        let kind = if self.imdb.is_some() {
            Some(IdKind::Imdb)
        } else {
            self.lead
        };
        let mut out = StrongIds {
            ambiguous: self.ambiguous,
            method: self.method,
            lead: kind,
            ..Default::default()
        };
        match kind {
            Some(IdKind::Imdb) => out.imdb = self.imdb.clone(),
            Some(IdKind::TmdbMovie) => out.tmdb_movie = self.tmdb_movie,
            Some(IdKind::TmdbTv) => {
                out.tmdb_tv = self.tmdb_tv;
                out.season = self.season;
                out.episode = self.episode;
            }
            Some(IdKind::Tvdb) => out.tvdb = self.tvdb,
            Some(IdKind::Mal) => out.mal = self.mal,
            Some(IdKind::Anilist) => out.anilist = self.anilist,
            Some(IdKind::Anidb) => out.anidb = self.anidb,
            None => {}
        }
        out
    }

    /// Fill every field still None from `other` (self wins).
    pub fn merge(&mut self, other: StrongIds) {
        if self.is_empty() && !other.is_empty() {
            self.method = other.method;
        }
        if self.lead.is_none() {
            self.lead = other.lead;
        }
        self.ambiguous |= other.ambiguous;
        fill(&mut self.imdb, other.imdb);
        fill(&mut self.tmdb_movie, other.tmdb_movie);
        fill(&mut self.tmdb_tv, other.tmdb_tv);
        fill(&mut self.tvdb, other.tvdb);
        fill(&mut self.mal, other.mal);
        fill(&mut self.anilist, other.anilist);
        fill(&mut self.anidb, other.anidb);
        if self.season.is_none() {
            self.season = other.season;
            if self.episode.is_none() {
                self.episode = other.episode;
            }
        }
    }

    /// `i` tags first, then the description.
    pub fn from_torrent(external_ids: &[(String, String)], description: &str) -> StrongIds {
        let mut ids = from_itags(external_ids);
        ids.merge(from_description(description));
        ids
    }
}

fn fill<T>(slot: &mut Option<T>, v: Option<T>) {
    if slot.is_none() {
        *slot = v;
    }
}

/// Keep the first value; a later different one marks the set ambiguous.
fn first<T: PartialEq>(slot: &mut Option<T>, v: T, ambiguous: &mut bool) {
    match slot {
        None => *slot = Some(v),
        Some(old) if *old != v => *ambiguous = true,
        Some(_) => {}
    }
}

/// `first`, also remembering the kind of the earliest link (`at` = its offset).
fn first_at<T: PartialEq>(
    slot: &mut Option<T>,
    v: T,
    ambiguous: &mut bool,
    at: usize,
    kind: IdKind,
    lead: &mut Option<(usize, IdKind)>,
) {
    if lead.is_none_or(|(p, _)| at < p) {
        *lead = Some((at, kind));
    }
    first(slot, v, ambiguous);
}

/// `tt` + digits anywhere in `s` (bare id or URL), normalised to at least 7 digits.
pub fn imdb_id(s: &str) -> Option<String> {
    let lower = s.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find("tt") {
        let start = from + pos;
        let before_ok = start == 0 || !lower.as_bytes()[start - 1].is_ascii_alphanumeric();
        if before_ok && let Some((n, len)) = digits(&lower[start + 2..]) {
            let after = start + 2 + len;
            if len <= 10
                && lower
                    .as_bytes()
                    .get(after)
                    .is_none_or(|b| !b.is_ascii_digit())
            {
                return Some(format_imdb(n));
            }
        }
        from = start + 2;
    }
    None
}

/// `tt` + zero-padded 7 digits (IMDb's own form; 8 digits once past 9,999,999).
pub fn format_imdb(n: u64) -> String {
    format!("tt{n:07}")
}

/// Numeric part of `tt0123456`.
pub fn imdb_num(id: &str) -> Option<u32> {
    id.strip_prefix("tt")?.parse().ok()
}

/// Leading ASCII digits as a number (and how many digit bytes were read).
fn digits(s: &str) -> Option<(u64, usize)> {
    let len = s.bytes().take_while(u8::is_ascii_digit).count();
    if len == 0 || len > 12 {
        return None;
    }
    s[..len].parse().ok().map(|n| (n, len))
}

fn id32(s: &str) -> Option<u32> {
    let (n, _) = digits(s)?;
    u32::try_from(n).ok().filter(|n| *n > 0)
}

/// Value of an `i` tag after an optional `anime:` / `series:` style type prefix.
fn typed<'a>(v: &'a str, accept: &[&str]) -> Option<&'a str> {
    match v.split_once(':') {
        Some((ty, rest)) => accept
            .contains(&ty.trim().to_ascii_lowercase().as_str())
            .then_some(rest.trim()),
        None => Some(v.trim()),
    }
}

/// Strong IDs from NIP-35 `i` tags (`external_ids`, split at the first `:`).
pub fn from_itags(external_ids: &[(String, String)]) -> StrongIds {
    let mut ids = StrongIds {
        method: method::ITAG,
        ..Default::default()
    };
    let mut amb = false;
    for (k, v) in external_ids {
        match k.as_str() {
            "imdb" => {
                if let Some(id) = imdb_id(v) {
                    first(&mut ids.imdb, id, &mut amb);
                }
            }
            // A bare TMDB number does not say movie or tv: unusable.
            "tmdb" => {
                if let Some((ty, n)) = v.split_once(':') {
                    match (ty.trim().to_ascii_lowercase().as_str(), id32(n.trim())) {
                        ("movie", Some(n)) => first(&mut ids.tmdb_movie, n, &mut amb),
                        ("tv", Some(n)) => first(&mut ids.tmdb_tv, n, &mut amb),
                        _ => {}
                    }
                }
            }
            "ttvdb" | "tvdb" => {
                if let Some(n) = typed(v, &["series", "tv"]).and_then(id32) {
                    first(&mut ids.tvdb, n, &mut amb);
                }
            }
            "mal" => {
                if let Some(n) = typed(v, &["anime"]).and_then(id32) {
                    first(&mut ids.mal, n, &mut amb);
                }
            }
            "anilist" => {
                if let Some(n) = typed(v, &["anime"]).and_then(id32) {
                    first(&mut ids.anilist, n, &mut amb);
                }
            }
            "anidb" => {
                if let Some(n) = typed(v, &["anime"]).and_then(id32) {
                    first(&mut ids.anidb, n, &mut amb);
                }
            }
            _ => {}
        }
    }
    // Tags are deliberate: two different imdb tags is a publisher error, not template noise.
    ids.ambiguous = amb;
    ids
}

/// Every occurrence of `needle` in `hay` (ASCII-lowercased already), as the offset just after it.
fn after_all<'a>(hay: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    hay.match_indices(needle)
        .map(move |(i, _)| i + needle.len())
}

/// At most `n` bytes of `s`, cut back to a char boundary (descriptions are not ASCII).
fn head(s: &str, n: usize) -> &str {
    let mut end = s.len().min(n);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Skip an optional locale path segment such as `fr/` or `en-us/`.
fn skip_locale(s: &str) -> &str {
    let seg = s.split('/').next().unwrap_or("");
    let b = seg.as_bytes();
    // "tv" is a TMDB path, not a locale.
    let is_locale = s.len() > seg.len()
        && seg != "tv"
        && match b.len() {
            2 => b.iter().all(u8::is_ascii_lowercase),
            5 => b[2] == b'-' && b[..2].iter().chain(&b[3..]).all(u8::is_ascii_lowercase),
            _ => false,
        };
    if is_locale { &s[seg.len() + 1..] } else { s }
}

/// Strong IDs from links in a torrent description.
pub fn from_description(description: &str) -> StrongIds {
    let mut ids = StrongIds {
        method: method::DESC_LINK,
        ..Default::default()
    };
    if description.is_empty() {
        return ids;
    }
    let d = description.to_ascii_lowercase();
    let mut amb = false;
    let mut lead: Option<(usize, IdKind)> = None;

    for at in after_all(&d, "imdb.com/") {
        let rest = skip_locale(&d[at..]);
        if let Some(r) = rest.strip_prefix("title/tt")
            && let Some((n, _)) = digits(r)
        {
            first_at(
                &mut ids.imdb,
                format_imdb(n),
                &mut amb,
                at,
                IdKind::Imdb,
                &mut lead,
            );
        }
    }
    let mut tmdb_tv_first: Option<u32> = None;
    for at in after_all(&d, "themoviedb.org/") {
        let rest = skip_locale(&d[at..]);
        if let Some(r) = rest.strip_prefix("movie/") {
            if let Some(n) = id32(r) {
                first_at(
                    &mut ids.tmdb_movie,
                    n,
                    &mut amb,
                    at,
                    IdKind::TmdbMovie,
                    &mut lead,
                );
            }
        } else if let Some(r) = rest.strip_prefix("tv/")
            && let Some((n, len)) = digits(r)
            && let Ok(n) = u32::try_from(n)
        {
            first_at(&mut ids.tmdb_tv, n, &mut amb, at, IdKind::TmdbTv, &mut lead);
            // Season/episode only from links to the series we kept.
            if tmdb_tv_first.is_none_or(|f| f == n) {
                tmdb_tv_first = Some(n);
                let tail = &r[len..];
                if ids.season.is_none()
                    && let Some(s) = tail.strip_prefix("/season/")
                    && let Some((season, slen)) = digits(s)
                {
                    ids.season = i32::try_from(season).ok();
                    if let Some(e) = s[slen..].strip_prefix("/episode/") {
                        ids.episode = digits(e).and_then(|(e, _)| i32::try_from(e).ok());
                    }
                }
            }
        }
    }
    for at in after_all(&d, "thetvdb.com/") {
        let rest = &d[at..];
        let rest = head(rest, 80);
        let n = if let Some(r) = rest.strip_prefix("dereferrer/series/") {
            id32(r)
        } else if let Some(r) = rest.strip_prefix("series/") {
            // /series/<slug> carries no id; only numeric paths count.
            id32(r)
        } else if rest.contains("tab=series") {
            rest.find("&id=")
                .map(|i| i + 4)
                .or_else(|| rest.find("&amp;id=").map(|i| i + 8))
                .and_then(|i| id32(&rest[i..]))
        } else {
            None
        };
        if let Some(n) = n {
            first_at(&mut ids.tvdb, n, &mut amb, at, IdKind::Tvdb, &mut lead);
        }
    }
    for at in after_all(&d, "myanimelist.net/anime") {
        let rest = &d[at..];
        let n = if let Some(r) = rest.strip_prefix('/') {
            id32(r)
        } else if let Some(r) = rest.strip_prefix(".php?id=") {
            id32(r)
        } else {
            None
        };
        if let Some(n) = n {
            first_at(&mut ids.mal, n, &mut amb, at, IdKind::Mal, &mut lead);
        }
    }
    for at in after_all(&d, "anilist.co/anime/") {
        if let Some(n) = id32(&d[at..]) {
            first_at(
                &mut ids.anilist,
                n,
                &mut amb,
                at,
                IdKind::Anilist,
                &mut lead,
            );
        }
    }
    for at in after_all(&d, "anidb.net/") {
        let rest = &d[at..];
        let n = if let Some(r) = rest.strip_prefix("anime/") {
            id32(r)
        } else if let Some(r) = rest.strip_prefix('a') {
            // anidb.net/a1234 short links
            id32(r)
        } else if rest.starts_with("perl-bin/animedb.pl") {
            let rest = head(rest, 80);
            (rest.contains("show=anime"))
                .then(|| {
                    rest.find("aid=")
                        .map(|i| i + 4)
                        .and_then(|i| id32(&rest[i..]))
                })
                .flatten()
        } else {
            None
        };
        if let Some(n) = n {
            first_at(&mut ids.anidb, n, &mut amb, at, IdKind::Anidb, &mut lead);
        }
    }
    ids.ambiguous = amb;
    ids.lead = lead.map(|(_, k)| k);
    ids
}

/// A search token that selects the torrents linked to one media entry (D40).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaRef {
    Imdb(String),
    TmdbMovie(u32),
    TmdbTv(u32),
    Tvdb(u32),
    Mal(u32),
    Anilist(u32),
    Anidb(u32),
    /// `media:N`: the server's own `media.id`.
    Media(i64),
}

/// Parse one query token: `imdb:tt0133093`, `tmdb:movie:603`, `tmdb:tv:1399`, `tvdb:N`,
/// `mal:N` / `mal:anime:N`, `anilist:N`, `anidb:N`, `media:N`. Case-insensitive.
pub fn media_ref(token: &str) -> Option<MediaRef> {
    let t = token.trim().to_ascii_lowercase();
    let (scheme, rest) = t.split_once(':')?;
    let anime = |r: &str| r.strip_prefix("anime:").unwrap_or(r).parse::<u32>().ok();
    match scheme {
        "imdb" => {
            let r = rest.strip_prefix("tt")?;
            (!r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
                .then(|| r.parse().ok().map(|n| MediaRef::Imdb(format_imdb(n))))
                .flatten()
        }
        "tmdb" => {
            let (ty, n) = rest.split_once(':')?;
            let n: u32 = n.parse().ok()?;
            match ty {
                "movie" => Some(MediaRef::TmdbMovie(n)),
                "tv" => Some(MediaRef::TmdbTv(n)),
                _ => None,
            }
        }
        "tvdb" | "ttvdb" => rest
            .strip_prefix("series:")
            .unwrap_or(rest)
            .parse()
            .ok()
            .map(MediaRef::Tvdb),
        "mal" => anime(rest).map(MediaRef::Mal),
        "anilist" => anime(rest).map(MediaRef::Anilist),
        "anidb" => anime(rest).map(MediaRef::Anidb),
        "media" => rest.parse().ok().map(MediaRef::Media),
        _ => None,
    }
}

/// Split search text into the remaining free text and the first media token, if any. Only
/// whitespace-separated tokens are considered, so `imdb` as a plain word stays free text.
pub fn split_media_token(text: &str) -> (String, Option<MediaRef>) {
    let mut found = None;
    let mut rest: Vec<&str> = Vec::new();
    for tok in text.split_whitespace() {
        if found.is_none()
            && let Some(r) = media_ref(tok)
        {
            found = Some(r);
            continue;
        }
        rest.push(tok);
    }
    (rest.join(" "), found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(k: &str, v: &str) -> (String, String) {
        (k.into(), v.into())
    }

    #[test]
    fn itag_forms() {
        let ids = from_itags(&[
            kv("imdb", "tt0133093"),
            kv("tmdb", "movie:603"),
            kv("ttvdb", "series:423075"),
            kv("mal", "anime:8425"),
            kv("anilist", "anime:21639"),
            kv("nyaa", "123"),
        ]);
        assert_eq!(ids.imdb.as_deref(), Some("tt0133093"));
        assert_eq!(ids.tmdb_movie, Some(603));
        assert_eq!(ids.tvdb, Some(423075));
        assert_eq!(ids.mal, Some(8425));
        assert_eq!(ids.anilist, Some(21639));
        assert_eq!(ids.method, method::ITAG);
        assert!(!ids.ambiguous);
        // Legacy top-level imdb tag with a URL; TVDB movie namespace and MAL manga ignored.
        let ids = from_itags(&[
            kv("imdb", "https://www.imdb.com/title/tt31392065/"),
            kv("ttvdb", "movies:346729"),
            kv("mal", "manga:17517"),
            kv("tmdb", "693134"),
        ]);
        assert_eq!(ids.imdb.as_deref(), Some("tt31392065"));
        assert_eq!(ids.tvdb, None);
        assert_eq!(ids.mal, None);
        assert_eq!(ids.tmdb_movie, None);
        assert_eq!(from_itags(&[kv("tmdb", "tv:1399")]).tmdb_tv, Some(1399));
        // Short ids are zero-padded to IMDb's 7 digits.
        assert_eq!(imdb_id("tt133093").as_deref(), Some("tt0133093"));
        assert_eq!(imdb_id("nott123"), None);
        assert_eq!(imdb_num("tt0133093"), Some(133093));
    }

    #[test]
    fn description_links() {
        assert_eq!(
            from_description("https://www.imdb.com/title/tt0133093/")
                .imdb
                .as_deref(),
            Some("tt0133093")
        );
        assert_eq!(
            from_description("https://www.themoviedb.org/en-us/movie/603").tmdb_movie,
            Some(603)
        );
        let d = r#"<a href="https://www.imdb.com/fr/title/tt0816692/">IMDb</a>
            TMDB : <a href="https://www.themoviedb.org/movie/1284496-c-etait-mieux-demain?language=fr-FR">Lien</a>
            <a href="https://www.themoviedb.org/movie/324544-in-the-lost-lands">x</a>
            https://www.thetvdb.com/dereferrer/series/81189
            https://myanimelist.net/anime/5114/Fullmetal https://anilist.co/anime/5114
            https://anidb.net/perl-bin/animedb.pl?show=anime&aid=6107"#;
        let ids = from_description(d);
        assert_eq!(ids.imdb.as_deref(), Some("tt0816692"));
        assert_eq!(ids.tmdb_movie, Some(1284496), "first link wins");
        assert!(ids.ambiguous);
        assert_eq!(ids.confidence(), 0.9);
        assert_eq!(ids.tvdb, Some(81189));
        assert_eq!(ids.mal, Some(5114));
        assert_eq!(ids.anilist, Some(5114));
        assert_eq!(ids.anidb, Some(6107));
        assert_eq!(ids.method, method::DESC_LINK);

        let ids = from_description(
            "https://www.themoviedb.org/tv/1399/season/2/episode/5/cast http://thetvdb.com/?tab=series&amp;id=121361 https://anidb.net/anime/14779",
        );
        assert_eq!(ids.tmdb_tv, Some(1399));
        assert_eq!((ids.season, ids.episode), (Some(2), Some(5)));
        assert_eq!(ids.tvdb, Some(121361));
        assert_eq!(ids.anidb, Some(14779));
        assert!(!ids.ambiguous);
        // Person pages, slug-only TVDB links and image paths are not IDs.
        let ids = from_description(
            "https://www.themoviedb.org/person/123 https://thetvdb.com/series/zone-interdite/episodes/42 \
             https://myanimelist.net/images/anime/1/2l.jpg https://www.imdb.com/name/nm0000206/",
        );
        assert!(ids.is_empty(), "{ids:?}");
        // Multi-byte text right after a link must not split a char.
        let d = format!(
            "thetvdb.com/{}é https://anidb.net/perl-bin/animedb.pl?{}é",
            "x".repeat(67),
            "y".repeat(60)
        );
        assert!(from_description(&d).is_empty());
        assert_eq!(head("aé", 2), "a");
    }

    #[test]
    fn tags_win_over_links() {
        let ids = StrongIds::from_torrent(
            &[kv("imdb", "tt0133093")],
            "https://www.imdb.com/title/tt0234215/ https://www.themoviedb.org/movie/603",
        );
        assert_eq!(ids.imdb.as_deref(), Some("tt0133093"));
        assert_eq!(ids.tmdb_movie, Some(603));
        assert_eq!(ids.method, method::ITAG);
        let ids = StrongIds::from_torrent(&[], "https://www.themoviedb.org/movie/603");
        assert_eq!(ids.method, method::DESC_LINK);
    }

    #[test]
    fn search_tokens() {
        assert_eq!(
            media_ref("IMDB:tt133093"),
            Some(MediaRef::Imdb("tt0133093".into()))
        );
        assert_eq!(media_ref("tmdb:movie:603"), Some(MediaRef::TmdbMovie(603)));
        assert_eq!(media_ref("tmdb:tv:1399"), Some(MediaRef::TmdbTv(1399)));
        assert_eq!(media_ref("mal:anime:5114"), Some(MediaRef::Mal(5114)));
        assert_eq!(media_ref("mal:5114"), Some(MediaRef::Mal(5114)));
        assert_eq!(media_ref("media:42"), Some(MediaRef::Media(42)));
        assert_eq!(media_ref("imdb:matrix"), None);
        assert_eq!(media_ref("tmdb:603"), None);
        let (rest, r) = split_media_token("1080p imdb:tt0133093 remux");
        assert_eq!(rest, "1080p remux");
        assert_eq!(r, Some(MediaRef::Imdb("tt0133093".into())));
        assert_eq!(
            split_media_token("imdb top 250"),
            ("imdb top 250".into(), None)
        );
    }

    #[test]
    fn main_link_is_imdb_else_earliest() {
        // A Holy Ghost release: its own IMDb link next to the YGG template TMDB link of another
        // film (In the Lost Lands). Different kinds, so not `ambiguous`; only IMDb is trusted.
        let d = "https://www.imdb.com/title/tt36364273/ \
                 https://www.themoviedb.org/movie/324544-in-the-lost-lands";
        let ids = from_description(d);
        assert_eq!(ids.tmdb_movie, Some(324544));
        assert!(!ids.ambiguous);
        let m = ids.main_link();
        assert_eq!(
            (m.imdb.as_deref(), m.tmdb_movie, m.lead),
            (Some("tt36364273"), None, Some(IdKind::Imdb))
        );
        // Template after the real link: the earliest one wins, with its season/episode.
        let d = "https://www.themoviedb.org/tv/1399/season/2/episode/5 \
                 https://www.themoviedb.org/movie/324544 https://myanimelist.net/anime/5114";
        let ids = from_description(d);
        assert_eq!(ids.lead, Some(IdKind::TmdbTv));
        let m = ids.main_link();
        assert_eq!(
            (m.tmdb_tv, m.season, m.episode, m.tmdb_movie, m.mal),
            (Some(1399), Some(2), Some(5), None, None)
        );
        let ids =
            from_description("x https://anilist.co/anime/5114 https://myanimelist.net/anime/1");
        assert_eq!(ids.main_link().anilist, Some(5114));
        assert_eq!(ids.main_link().mal, None);
        assert!(from_description("nothing").main_link().is_empty());
        assert_eq!(from_itags(&[]).lead, None);
    }
}
