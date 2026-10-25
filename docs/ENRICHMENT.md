# Metadata enrichment: IMDb (D40)

Titles, years, ratings and episode numbers are not on Nostr or the DHT. The server links each
infohash to a `media` row from data it holds itself. This page covers the shared tables, the
strong-ID and IMDb parts (D40). Anime matching is in [ENRICHMENT-ANIME.md](ENRICHMENT-ANIME.md)
(D41), TMDB posters and overviews in [ENRICHMENT-TMDB.md](ENRICHMENT-TMDB.md) (D42). Research and
licences: [research/metadata-enrichment.md](research/metadata-enrichment.md).

**Never republished.** Enriched data stays on the server (IMDb data is for personal,
non-commercial use; TMDB data is not redistributable). The phone gets it only inside API
responses from its own server, and never contacts IMDb or TMDB.

---

## 1. Tables (migration `20261026000000_enrich_imdb.sql`)

| Table | Owner | What |
|---|---|---|
| `media` | shared | One identified title: `kind` (`movie`/`series`/`anime`), external ids (`imdb` unique, partial unique indexes on `tmdb_movie`, `tmdb_tv`, `tvdb`, `mal`, `anilist`, `anidb`), title, year, genres, rating, votes, runtime. Created lazily, only for titles a torrent links to |
| `torrent_media` | shared | One link per infohash: `media_id`, `season`/`episode` of this torrent, `method`, `confidence`, `matcher_version`, `matched_at` |
| `enrich_checked` | imdb | Infohashes looked at, by matcher version; `pending_title` = title matching found nothing yet (retried after each dataset import). `PgStore::put_torrent` deletes the row when a new event arrives for the infohash, so links in a later description are picked up |
| `enrich_id_pairs` | imdb | (kind, value, infohash) → IMDb id: the non-IMDb ids each IMDb-linked torrent carries, to tell agreed pairs from template links (§1 ownership rules) |
| `imdb_titles`, `imdb_names`, `imdb_episodes` | imdb | The dataset, UNLOGGED (rebuilt from files, no WAL). `imdb_names` = normalised primary/original titles (+ akas) with a btree and a trigram index; `imdb_episodes` packs ~9.7M episodes into 168k rows (`tconst >> 8` → 9-byte records) |
| `imdb_datasets` | imdb | `Last-Modified`, size and row count of each imported file |

`method` values: `itag`, `desc_link`, `offline_title`, `api_search`, `manual`.

### Ownership rules (all enrichers)
- `manual` is never overwritten or deleted.
- Non-anime torrents: this enricher replaces or deletes its own methods (`itag`, `desc_link`,
  `offline_title`). Another enricher's link (`api_search`) is replaced only by an equally or more
  confident one, and never deleted.
- Anime torrents (category `anime`): only strong IDs are written here, and only when their
  confidence is ≥ the existing link's; nothing is deleted. Title matching is D41's.
- A strong ID for a title without an IMDb row (TMDB-, MAL-, AniList- or AniDB-only) creates a
  media row with the torrent's parsed title and year; D41/D42 fill the details later.
- Ids that choose, create or extend a row: `i` tags (deliberate) and each description's *main
  link* (its IMDb link, else its earliest link). A description's other links are only
  *evidence*, because a template link sits next to other films' own IMDb links: YGG's
  `themoviedb.org/movie/324544` (In the Lost Lands, ~750 production descriptions) under Holy
  Ghost releases, and `themoviedb.org/movie/1530875` next to two IMDb ids.
- Pairs (`enrich_id_pairs`, private): every non-IMDb id of a torrent linked to an IMDb title is
  recorded with that title (the series for an episode id). A pair is *agreed* when the id was
  never seen next to another IMDb id and it is deliberate, or it is held by a TMDB-only row whose
  title is one of the IMDb title's names (dataset names, else the row's). How often a pair was
  seen is no evidence: one uploader puts the same wrong IMDb link (Pékin express) under every
  episode of Les Apprentis Aventuriers next to its right TMDB link. Agreed ids are added to the
  IMDb row where free, or the IMDb-less, non-anime row holding them is merged into it (its links
  move, the duplicate is deleted; its `media_extra` rows go with it and are re-fetched lazily).
  The same test runs the other way round, so the order does not matter: a TMDB-only release
  whose id is paired with one IMDb title having the release's title among its names links to that
  title. Ids are never moved between rows that both have an IMDb id.
- An id seen next to a second IMDb id is a template: it is taken off the IMDb rows it was added
  to through pairs, their torrents are requeued and relinked without it, and it is never adopted
  again. When one of a torrent's ids sits on a row with another
  IMDb id (e.g. set by D42 from TMDB's external ids), the link gets confidence 0.9.
- Rows of kind `anime` are D41's: a torrent whose MAL/AniList/AniDB id is on such a row links to
  it as is (nothing is attached to it), whatever its IMDb id. A new IMDb row created for an anime
  torrent (category `anime`, or an anime id) gets kind `anime`; D41 adopts it.
- `enrich::imdb::refresh_media` rewrites title/year/genres/rating/votes/runtime of every row with
  an `imdb` id after an import (kind too, unless it is `anime`), except rows of kind `anime` that
  carry an anime id: D41 writes those columns from its own dataset.

---

## 2. Pipeline per infohash

All events of the infohash are read, canonical first (highest trust, then earliest).

1. **Strong IDs, every category** (`core::enrich::ids`), confidence 1.0:
   - `i` tags: `imdb:tt…` (also a full URL in the legacy top-level `imdb` tag), `tmdb:movie:N`,
     `tmdb:tv:N`, `ttvdb:series:N` / `ttvdb:N` (`ttvdb:movies:N` ignored: another namespace),
     `mal:anime:N` / `mal:N` (manga ignored), `anilist:anime:N`, `anidb:N`. A bare `tmdb:N` is
     unusable (movie or tv?).
   - Description links: `imdb.com/(xx/)title/tt…`, `themoviedb.org/(xx-xx/)(movie|tv)/N`
     (`/season/S/episode/E` gives season/episode), `thetvdb.com/dereferrer/series/N`,
     `thetvdb.com/?tab=series&id=N`, `myanimelist.net/anime/N`, `anilist.co/anime/N`,
     `anidb.net/anime/N`, `anidb.net/aN`, `animedb.pl?show=anime&aid=N`. Person pages, image
     paths and slug-only TVDB links are not IDs.
   - Tags win over links; the first link of each kind wins; a description's other links are
     only adopted through agreed pairs (§1 ownership rules). Several *different* links of one
     kind in a description (the YGG uploader template links `themoviedb.org/movie/324544` under
     607 releases, always after the real link) give confidence 0.9.
   - An IMDb episode id resolves to its series, with that episode's season/episode.
2. **Offline title match, `video` only, when no strong ID** (`core::enrich::parse`,
   `core::enrich::imdb_match`):
   - Names tried: the torrent title (skipped when it is just the infohash), then the largest
     video file without extension. A DHT torrent has only these two.
   - `parse-torrent-title` =2.0.2 gives title, year (first of a range), season/episode (first of
     a pack). Alternatives: a trailing `(…)` group, split on ` / ` (the YGG bridge appends the
     French title that way), and the title without a `(US)`/trailing `US|UK|AU|NZ|CA` tag.
     `COMPLETE`/`INTEGRALE` left in the title are dropped and mark a complete-series pack
     (series preferred unless the name says collection/trilogy/saga).
   - Normalisation: NFKD, combining marks dropped, lowercase, `&` → `and`, other
     non-alphanumerics → space (letters of every script kept). Titles with apostrophes are
     indexed twice ("ocean s eleven", "oceans eleven").
   - Exact lookup of every key in `imdb_names`, then ranking: type (series if S/E or complete
     pack, else movie/tvMovie/video) 1000, exact year 500 (a series' start..end run counts as
     exact), ±1 year 300, main title over alternative 150, primary/original name over aka 120,
     votes up to 100.
   - Confidence by tier: right type + exact year 0.95, ±1 0.85, no year in the name 0.75,
     no year in IMDb 0.7, year off by more 0.45, wrong type 0.55/0.35. Minus 0.05/0.15/0.3 when a
     same-tier rival has ≥ 1/5, ≥ 1/2 or more of the votes (only rivals found through an
     equally strong name count), minus 0.05 for an alternative title, minus 0.05 for an aka hit
     with a year and 0.3 without one.
   - Fallbacks when nothing matched exactly and the name has a year: longest word prefix (≥ 2
     words) with exactly one same-type, same-year title (0.55; "Wake Up Dead Man A Knives Out
     Mystery" → "Wake Up Dead Man"), then pg_trgm similarity ≥ 0.6 with type and exact year
     (≤ 0.55).
3. **Anime** without a strong ID is left to D41. Other categories without a strong ID are not
   matched (book/audio/software names produce false film hits).

`MATCHER_VERSION` (`core::enrich::imdb_match`) is stored per link and per check; bumping it makes
the loop recheck every non-manual link.

### Display threshold
Links below `MIN_DISPLAY_CONFIDENCE` = **0.5** are stored (rematching and debugging use them) but
not shown: `TorrentDetail.media` and `TorrentSummary.media_*` stay None, and search tokens ignore
them. On the measured sets (§4) the links under 0.5 were right 50–80% of the time, those at or
above 0.5 about 97–98%.

---

## 3. Datasets and the loop

| | |
|---|---|
| Files | `title.basics` 229 MB, `title.episode` 55 MB, `title.ratings` 8.7 MB (+ `title.akas` 519 MB with `MAGNETAR_IMDB_AKAS=true`), from `https://datasets.imdbws.com/` |
| Download | `If-Modified-Since` from the stored `Last-Modified` (sidecar `<file>.last-modified`, else the file mtime); an unchanged file is one 304. A failed download keeps the local copy |
| Import | ratings into memory, then title.basics streamed (gzip → TSV → COPY) into `imdb_*_new` through two concurrent COPYs (titles, names), then episodes (and akas); indexes built on the staging tables, one transaction swaps them in. Readers never see a mix. An advisory lock keeps two processes from importing at once (held on the import's own
connection, closed if the import is cancelled); the import uses at most 2 of the server's 5 pool
connections (the lock connection + one more while the names COPY runs, akas included); fewer than 500,000 titles is refused (truncated file) |
| Kept | movie, tvMovie, video, tvSeries, tvMiniSeries, tvSpecial, non-adult: 1,563,990 titles, 1,776,289 names, 9,692,697 episodes |
| Refresh | `MAGNETAR_IMDB_REFRESH_HOURS` (168). Re-import only when a file changed, the akas setting changed, or the UNLOGGED tables are empty (crash) |
| Loop (`serve`) | drain what is due in batches of 200 → refresh check → sleep 60 s. New torrents are linked within about a minute; a fresh install links strong IDs before the first download finishes, title matches right after the import |
| CLI | `magnetar-server enrich [--reimport] [--rematch]`: refresh, drain everything due, print a JSON report with coverage per category and method |

With `MAGNETAR_IMDB=false` nothing is downloaded; strong IDs are still linked (media rows from
torrent titles).

---

## 4. Measurements (2026-10-25, laptop, Postgres 18 in docker)

Scripts: `crates/server/examples/imdb_eval.rs` (accuracy) and `magnetar-server enrich`
(import + backfill). Ground truth came from read-only SELECTs on the production database; the
test databases held the datasets and a copy of the production `torrents` table, nothing was
written to production.

### 4.1 Title matching accuracy (title path only, strong IDs ignored)
"Correct" also counts a hit on the series of a tagged episode, as in the research script.
Some "wrong" rows are publisher tagging errors (e.g. every "Wednesday S02E0x" tagged
tt3645090 "Wednesday's Downtown", "Only Murders in the Building S05" tagged a 2022 spin-off).

| Set | Rows | Correct | Wrong | None | Precision |
|---|---|---|---|---|---|
| Research set (1,328 imdb-tagged, 5 rows with a URL instead of an id skipped); research script re-run on the same files: **93.6%** | 1,323 | **96.1%** | 2.6% | 1.4% | 97.4% |
| Same, without the pg_trgm fallback (prefix fallback kept) | 1,323 | 95.6% | 2.6% | 1.8% | 97.4% |
| All imdb `i`-tagged infohashes now (1,361 video) | 1,406 | **96.2%** | 2.4% | 1.4% | 97.5% |
| Same, DHT input: name = infohash, only the largest video file | 1,239 | **96.6%** | 2.3% | 1.0% | 97.6% |
| imdb.com description links, video, mostly French YGG releases (no i tag) | 5,965 | **91.4%** | 3.0% | 5.6% | 96.8% |
| Same with `MAGNETAR_IMDB_AKAS=true` | 5,965 | **94.2%** | 3.1% | 2.7% | 96.8% |
| i-tag set with akas (measured before the primary-over-aka score, which only changes aka ties) | 1,406 | 96.9% | 2.4% | 0.6% | 97.6% |

Precision by confidence (no akas):

| Confidence | i-tag set: right / wrong | French set: right / wrong |
|---|---|---|
| < 0.5 (hidden) | 5 / 5 | 79 / 53 (≈ 50–60%) |
| 0.5–0.7 (prefix/fuzzy, aka, alternatives) | 15 / 4 | 120 / 13 |
| 0.7–0.8 (no year in the name) | 399 / 13 | 3,286 / 71 |
| ≥ 0.8 | 934 / 12 | 1,965 / 44 |
| **≥ 0.5 (shown)** | **1,348 / 29 = 97.9%** | **5,371 / 128 = 97.7%** (90.0% of the set shown) |

Hence `MIN_DISPLAY_CONFIDENCE = 0.5`: everything below it is roughly a coin flip, everything above
it ≥ 97%. Akas add 2.8 points on French names but cost a 519 MB download, ~300 MB of tables
and 3 more minutes per import, so they stay opt-in. The pg_trgm fallback adds 0.5 points.

### 4.2 Import (IMDb files of 2026-10-25, release build)

| | Without akas | With akas |
|---|---|---|
| Rows | 1,563,990 titles, 1,776,289 names, 9,692,697 episodes in 168,345 buckets | + 1,788,454 aka names |
| Time (stream + COPY + index + swap) | **67–98 s** (36–67 s COPY, 30 s indexes) | 275 s |
| Size in Postgres | **583 MB** (titles 213, names 250 incl. 81 trigram index, episodes 94) | 877 MB |
| One row per episode with a btree instead of buckets | 600 MB for episodes alone | |

Peak memory of the import: ratings map (1.7M entries) + kept-title map (1.6M) + episode buckets
(~90 MB), freed after the import.

### 4.3 Full backfill on a copy of production (366,687 events, 368,283 infohashes)

`magnetar-server enrich` on an empty `torrent_media`, datasets already imported: **20.5 min**
(1,228 s; 621 s of it in the prefix/pg_trgm fallbacks for the ~14,000 unmatched video names,
~45 ms each; exact matching is ~2–4 ms per torrent). Steady state is a few new torrents a
minute, so the loop is idle almost all the time.

Video torrents (145,989 infohashes):

| Method | Share |
|---|---|
| `desc_link` | 70.3% |
| `itag` | 1.0% |
| `offline_title` shown (≥ 0.5) | 19.0% |
| `offline_title` hidden (< 0.5) | 1.2% |
| none | 8.4% |
| **linked and shown** | **90.3%** |

Anime: 12.0% `desc_link` (MAL/AniDB/AniList links), the other 88% left to D41. Other
categories: only strong IDs (47 of 22,376 `other`). 36,015 media rows (25,711 movie, 9,323
series, 981 anime); `media` + `torrent_media` + `enrich_checked` = 93 MB.

After the review fixes (template links, D41 rows, pairs), same copy, from scratch: 963 s, the
same 143,299 links shown; 3,891 of 145,121 linked infohashes point elsewhere than before:
~2,800 anime releases on their anime rows (MAL/AniList/AniDB, D41's) instead of rows merged
into IMDb titles; 38 Les
Apprentis Aventuriers releases back on their own row (one uploader's wrong Pékin express IMDb
link had merged them); the two template ids (324544, 1530875) never adopted, so Holy Ghost and
In the Lost Lands stay apart; ~690 releases of titles named differently on TMDB and IMDb
("Star Wars Skeleton Crew" / "Skeleton Crew", "Mask Singer" / "Mask Singer (III)") stay on
TMDB-only rows (the price of not trusting unverified pairs; D42 still decorates them by TMDB id).
36,406 media rows; `enrich_id_pairs` 19,976 rows, 3.7 MB.

---

## 5. API

- `TorrentDetail.media: Option<MediaInfo>`; `TorrentSummary.media_title/media_year/media_kind`
  (search hits too). Both `#[serde(default)]`, so older clients and servers interoperate.
- Search tokens (whitespace-separated, anywhere in `q`, first one wins, the rest stays free
  text): `imdb:tt0133093`, `tmdb:movie:603`, `tmdb:tv:1399`, `tvdb:81189`, `mal:5114` (or
  `mal:anime:5114`), `anilist:N`, `anidb:N`, `media:<id>`.
- `GET /api/v1/media/{id}/torrents` with the search query string (`sort`, `limit`, `offset`,
  `q`, …): the other releases of one title. `ApiClient::media_torrents(id, limit, offset)`
  asks for `sort=seeders`.
- `MediaInfo.overview` / `poster` are None here; D42 fills them.
