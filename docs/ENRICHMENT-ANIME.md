# Anime enrichment (D41)

Offline identification of anime torrents. It runs on the server only. The phone never downloads
these datasets and never contacts a third party; it sees the result through `TorrentDetail.media`
and `TorrentSummary.media_*`, which the IMDb enrichment serves (docs/ENRICHMENT.md).
Research: [research/metadata-enrichment.md](research/metadata-enrichment.md) §2, §4.2.

Code:
- `crates/core/src/enrich_anime.rs`: pure code. Dataset parsers, normalisation, the `AnimeIndex`
  matcher, link extraction, alternative titles. It is a top-level module (not
  `enrich/anime.rs`) because the IMDb branch owns `enrich/mod.rs`. It can move after the merge.
- `crates/server/src/enrich_anime.rs`: download and import, title parsing (both parser crates),
  the matching loop, and writes to `media` / `torrent_media`.
- `crates/server/migrations/20261027000100_anime_datasets.sql`: private `anime_*` tables only.
  `media` and `torrent_media` come from the IMDb migration.

## Datasets and licences

| Dataset | Source (default) | Size | Licence | How we use it |
|---|---|---|---|---|
| anime-offline-database (AOD) | `cedya77/anime-offline-database`, release asset `anime-offline-database.jsonl.zst` via `releases/latest/download/…` | 5.9 MB zst, 61 MB JSONL, 38,270 entries (release 2026-43, `lastUpdate` 2026-10-25) | ODbL 1.0 + DbCL 1.0 | titles, synonyms, type, episodes, year, score, MAL/AniList/AniDB/Kitsu ids |
| Fribb anime-lists | `Fribb/anime-lists` `master/anime-list-full.json` (measured at commit `08dc828d`, 2026-10-20) | 7.5 MB, 39,579 entries | **no licence file** (GitHub licence API: none) | AOD entry → TVDB / TMDB / IMDb ids, with TVDB/TMDB **season** and episode offset |

**Why cedya77.** manami-project/anime-offline-database was archived on 2026-07-04 (last release
`2026-27`). cedya77's repository says it continues it (README: built by `cedya77/modb-app`,
seeded from the final upstream release, same licence). It has one maintainer and is a few months
old, so we do not trust it blindly:
- Every import is validated: a metadata header, at least 10,000 entries and under 1 % unreadable
  lines. Fribb must also have at least 10,000 entries. A failed or suspicious download keeps the
  previous import.
- The source is configurable. `MAGNETAR_ANIME_AOD_URL=…/releases/download/2026-43/…` pins one
  weekly release, and a local path works for mirrors or offline installs.
- The release tag (taken from GitHub's `latest` redirect) and the file's `lastUpdate` are stored
  in `anime_datasets.version`. The asset of a tag can be re-uploaded: 2026-43 was replaced on
  2026-10-25, six days after its release. So we also compare the SHA-256 and do not rely on the
  tag alone.

**ODbL obligations.** We use AOD locally and do not redistribute the database. The anime data is
never published to Nostr or anywhere else (D41; the same rule applies to IMDb and TMDB data). The
`media` rows built from it are a derivative database. As long as the server serves only its
owner's devices (the default: loopback plus a bearer token, D20), nothing is "publicly used" and
there is no share-alike duty. **If you expose the API publicly**, ODbL §4.3 requires this notice,
and §4.4 requires the derived anime data to be offered under the ODbL:

> Anime data from anime-offline-database by cedya77 (https://github.com/cedya77/anime-offline-database),
> continuing manami-project (https://github.com/manami-project/anime-offline-database),
> licensed under ODbL 1.0 / DbCL 1.0. ID mappings from Fribb/anime-lists
> (https://github.com/Fribb/anime-lists).

**Fribb decision.** The repository has no licence file, so its terms are unverified. What we
know:
- The data is identifier cross-references (facts). It is generated from AOD (ODbL) and
  Anime-Lists/anime-lists. Its README describes the files for others to consume.
- Many open-source media servers fetch it at runtime the same way. We do the same: we download it
  from the author's repository, keep it locally, and never redistribute it.
- `MAGNETAR_ANIME_FRIBB_URL=none` turns it off. Matching then runs on AOD alone: 94.8 % instead of
  96.4 % correct (precision 95.4 % instead of 97.1 %), and `media` rows get no IMDb, TMDB or TVDB
  ids, so the TMDB decoration has nothing to fetch for anime.
- If the author ever publishes restrictive terms, set it to `none`. Nothing else changes.

**Refresh.** The loop looks every 10 minutes. A dataset is re-checked when its last check is more
than 7 days old or its source setting changed. The check is a conditional GET (`If-None-Match` /
`If-Modified-Since`, then a SHA-256 comparison). A 304 or an identical file only updates
`checked_at`. Each import is one transaction (DELETE + multi-row INSERT). The index is then rebuilt
in memory: about 0.6 s, around 200k title keys. The AOD release uses zstd `--long` (128 MiB
window). ruzstd 0.9.0 refuses windows above 100 MiB by default, so we raise the limit
(`MAX_ZSTD_WINDOW`); the buffer never exceeds the 61 MB output.

## Matching

Input per infohash: the canonical event (highest trust, then earliest), meaning its title,
description, files and `i` tags. Only `category = 'anime'` qualifies, minus the `category_path`
values `live-action`, `literature`, `audio`, `lossless`, `pictures`, `amv`, `manga` and `music`:
7,782 of the 82,568 anime infohashes in production. These rules also cover a DHT crawler's input
(a name and a file list, no description):

1. **Strong ids.** First `i` tags (`mal:`, `anilist:`, `anidb:`, confidence 1.0, method `itag`).
   Then description links (`myanimelist.net/anime/N`, `anilist.co/anime/N`, `anidb.net/anime/N`,
   `anidb.net/aN`, `animedb.pl?…aid=N`; confidence 0.97, method `desc_link`).
2. **Name.** The torrent title. If it is empty or a bare infohash (DHT), we use the largest video
   file's base name instead.
3. **Parse.** `parse-torrent-title` 2.0.2 (MIT) is the primary parser: seasons, ranges and years.
   `anitomy-ng` 1.0.10 (MPL-2.0) gives a second title candidate and fills gaps. To these we add
   alternative titles from `(…)` groups and after ` | `. Inside brackets we split on `;`, on `,`
   without a following space, and on `, ` only when the group also holds tags like
   `VF, FRENCH, VOSTFR` (titles contain commas: `Kanojo, Okarishimasu`). ptt reads
   `X Season 3 - 10` as seasons 3 to 10; when anitomy-ng sees one season and an episode there, its
   season is used.
4. **Lookup.** We look up every title candidate as an exact normalised key: lowercase, letters
   and digits of any script, `&` read as "and", apostrophes dropped. Synthetic keys are also tried:
   the title without its season suffix (`x season 2`, `x 2nd season`, `x s2`, `x tv-2`), and for
   season S > 1 the usual sequel spellings (`x 2`, `x season 2`, `x 2nd season`, `x s2`, `x ii`).
   With a release year Y, `x Y` is tried too: AOD names remakes and same-title shows
   `Doraemon (2005)`, `Pokemon (2023)`, and the parser took the year out of the title. A "year"
   past the dataset's last year is part of a title (`SAC_2045`) and is ignored.
5. **Rank** the candidates for a key: year match, episode within the entry's count, TV over
   ONA/movie, then the Fribb season equal to the parsed one, then newest.
6. **Remap through Fribb.**
   - Season given and the matched entry is another season, or the episode lies past this cour:
     take the entry Fribb maps to (series, S). Several cours of one season are told apart by
     `episode_offset`.
   - No season, but the episode lies past the entry's length (`Golden Kamuy - 38`): walk the
     series' TV entries in season order and add up their episode counts.
   - The release year contradicts the matched entry (more than one year apart): take the
     series' TV/ONA entry that aired that year (Erai-raws name later seasons
     `One Punch Man (2025)`), method `offline_title`, reported as `yearremap`.
7. **Score** each candidate match by how well it agrees with the release: season (+2 / −2),
   episode in range (±1), specials (S00) fitting OVA/special/ONA, and year (+1 fits within a
   year, −2 contradicts). The best wins, and earlier titles break ties.
8. **Confidence.** 0.9 for a unique key or a year match, 0.75 otherwise, 0.85 for a season remap
   and 0.8 for an episode or year remap. An alternative title costs −0.03 and a disagreeing
   match −0.15. Matches below **0.6** are not written: that bucket was 29 % precise.
   Two contradictions set the confidence to 0.45 (`CONF_CONTRADICTED`), so the match is not
   written:
   - **Year.** The release year fits neither the entry nor (for a remap) the matched title's
     entry or any entry of its series, and no year remap applies: `Hokuto no Ken (2026)` when only
     the 1984 show is in the data, `GeGeGe no Kitaro (1968)` when only the 2018 one matches the
     spelling. A remap keeps the show's first year: `Black Clover (2017) S02` is fine.
   - **Season.** The release states a season S ≥ 2 that no remap or season-named key found, and
     the entry is another Fribb season: `Golden Kamuy S06E01` or `Chainsaw Man S03E01` would
     otherwise land on season 1. The season-1 entry's season is never written over the
     release's.
9. **Fallback.** If nothing matched, step 2 runs again on the largest video file name.

`AnimeIndex::cross_ids` gives the Fribb ids of the matched entry. Several MAL entries (seasons,
cours, recaps) share one IMDb / TVDB / TMDB-tv id, but `media` holds each external id once. So an
id is **owned** by the series' first TV entry (lowest season ≥ 1, then offset, then year), and
only the owner writes it.

### Writes (contract tables)

**`media`:** one row per AOD entry, `kind = 'anime'`. It holds the mal/anilist/anidb ids plus the
owned imdb/tmdb/tvdb ids, `title` (the AOD main title, usually romaji), `year`, `genres` (AOD
tags filtered to a fixed genre list), `rating` (median provider score, 1-10) and `runtime_min`
(per episode). Ids are only added, never moved:
- If another row already holds an id, it stays there and our row goes without it.
- If another matcher created a row for the same work (same IMDb/TMDB/TVDB id, no anime ids), we
  **adopt** it: the anime ids are added, and its `kind` and `title` stay with its owner.
- Each upsert runs in a savepoint and is retried once, in case a concurrent matcher inserts the
  same id between our SELECT and INSERT.

**`torrent_media`:**
- `season` is the TVDB season and `episode` is the episode within the matched entry (rebased by
  the remaps). `matcher_version` is `enrich_anime::MATCHER_VERSION` (2).
- A `manual` row is never touched. Another matcher's row (its media is not `kind = 'anime'`) is
  replaced only by a strictly more confident match. This is checked before writing and again in
  the `ON CONFLICT … WHERE` clause.
- If a torrent stops matching after a dataset or matcher change, our old `offline_title` guess is
  deleted.

**`anime_attempts`** records (infohash, dataset version, matcher version). Unmatched torrents are
retried only when the datasets or the matcher change.

## Results (measured 2026-10-25, matcher version 2)

**These figures are in-sample.** The matcher was tuned on the MAL-link set below, and that set
is not a sample of the torrents most title links come from:

| Release group | MAL-link set (8,613) | holdout (711) | all anime infohashes (82,568) |
|---|---|---|---|
| Tsundere-Raws | 5,540 | | 5,740 |
| Yameii | 1,452 | 462 | 2,028 |
| Trix / Gecko | 325 / 316 | 7 (Gecko) | |
| Erai-raws | 0 | 0 | 9,403 |
| ToonsHub | 0 | 0 | 6,990 |
| SubsPlease | 0 | 0 | 6,668 |
| VARYG | 0 | 0 | 5,095 |
| shincaps | 0 | 0 | 4,697 |
| Doomdos, ASW, NanakoRaws, ANi | 0 | 0 | 2,551, 2,077, 1,982, 1,913 |

(Group = leading `[group]` or trailing `-group`.) The truth set is 64 % Tsundere-Raws, the
holdout is 65 % Yameii, and none of the eight largest groups of title-only torrents is in either.
So the truth-set accuracy says how well the matcher reads Tsundere-Raws / Yameii names, not the
precision of all title links. For that, see the stratified sample further down.

Ground truth comes from production: anime torrents whose description links `myanimelist.net/anime/N`.
That is 8,613 infohashes and 872 distinct MAL ids, mostly current-season fansub and raw
releases. We exported them with a read-only SELECT (ids, titles and largest file name only, no
descriptions). A **holdout** set has no MAL link but has an AniDB or AniList link: 711 infohashes,
judged by those ids, never looked at while tuning.

The parser comparison (matcher version 1) uses the same matcher for every row (scratch harness
`anime-eval`, linked against `magnetar-core`):

| Parsing | MAL set correct | wrong | none | precision | holdout correct | precision | µs/title |
|---|---|---|---|---|---|---|---|
| research baseline: Python PTT, naive title match | 65.0 % | 25.9 % | 7.4 % | | | | |
| research baseline + Fribb season remap | 87.3 % | 5.3 % | 7.4 % | | | | |
| parse-torrent-title only | 91.1 % | 2.5 % | 6.4 % | 97.3 % | 78.2 % | 96.4 % | ~600 |
| anitomy-ng only | 91.0 % | 2.4 % | 6.6 % | 97.4 % | 90.0 % | 97.1 % | ~75 |
| ptt + alt titles | 96.1 % | 2.8 % | 1.0 % | 97.1 % | 81.4 % | 96.5 % | |
| anitomy + alt titles | 95.9 % | 2.8 % | 1.2 % | 97.1 % | 93.2 % | 97.2 % | |
| **ptt + anitomy + alt titles** (shipped) | **96.4 %** | 2.8 % | 0.9 % | 97.2 % | **93.5 %** | 96.9 % | ~550 |
| same, + file-name fallback | 96.4 % | 2.9 % | 0.7 % | 97.1 % | 94.7 % | 97.0 % | |
| DHT input: largest file name only | 94.1 % | 3.0 % | 2.8 % | 96.9 % | 89.9 % | 97.0 % | |
| shipped, without Fribb (`…FRIBB_URL=none`) | 94.8 % | 4.5 % | 0.7 % | 95.4 % | 91.3 % | 93.5 % | |

- Neither parser wins alone. `parse-torrent-title` loses `[Yameii] X - S01E02 [English Dub]`
  style names (holdout 78 %), while anitomy-ng reads them. Using both titles as candidates is
  best on both sets. anitomy-ng is about 8× faster; ptt reads season ranges and years better.
- What moved the result from 87 % to 96 % (all tuned on this set):
  - alternative titles: Tsundere-Raws and Yameii put the romaji title in brackets;
  - synthetic sequel keys for shows without TVDB mapping (donghua, ONAs);
  - cour selection by episode;
  - scoring across all title candidates instead of taking the first hit.

**End to end** (`cargo test --release -p magnetar-server --test it enrich_anime::e2e -- --ignored`
with `MAGNETAR_ANIME_E2E=<dir>`). Real datasets (2026-43, Fribb `08dc828d`) are imported through
the server code into a test schema, and the production rows are inserted as torrents:

- **In-sample accuracy, links hidden** (the 8,613 MAL-truth torrents with empty descriptions):
  96.9 % correct, 2.2 % wrong, 1.0 % none, precision 97.8 % (version 1: 96.9 / 2.4 / 0.8 %,
  97.6 %). By method: 6,461 title, 2,065 season remap, 5 episode remap. 31 matches fell below the
  0.6 floor.
  - Precision by confidence floor: 0.60: 11/15, 0.70: 51/59, 0.75: 799/842, 0.80: 43/50,
    0.85: 2,366/2,424, 0.90: 5,074/5,141.
- **Coverage** (all 82,568 anime infohashes, links restored as bare URLs): 7,782 excluded by
  path, 74,786 scanned, **71,205 linked (95.2 %)**.
  - By method: 9,320 `desc_link`, 49,681 title, 10,768 season remap, 1,434 episode remap,
    2 year remap.
  - 3,581 unmatched, 318 below the floor (version 1: 69; the difference is the two
    contradiction rules).
  - 93 s for the whole set (about 1.2 ms per torrent including Postgres).
- **Contradictions** (scratch probe over all 82,568 names, title matching only, links ignored):
  title links whose release year is more than a year from the entry's went from 910 to 92, and
  the remaining 92 are all season or episode remaps where the year is the show's first year
  (`JoJo's Bizarre Adventure (2012) S06` to Steel Ball Run). Links that wrote season 1 (or another
  Fribb season) over an explicit release season ≥ 2 went from 121 (version-1 matcher, same
  parser) to 0. The other release-season / Fribb-season disagreements (419) are keys that name
  the season themselves (`Honzuki no Gekokujou … Ryoushu no Youjo` for S04, which Fribb numbers as season 1 of its own TVDB entry); those store the
  release's season.
- **Out-of-sample spot check** (hand-labelled, 2026-10-25): 10 random title links (seed
  20261025) from each of the 12 largest groups absent from the truth sets, among torrents
  without any MAL/AniList/AniDB link. **116 / 120 judged correct (96.7 %; Wilson 95 % interval
  about 91.7–98.7 %).** By group: Erai-raws, ToonsHub, VARYG, ASW, ANi, LoliHouse, DKB, Judas
  10/10; SubsPlease, shincaps, Doomdos, NanakoRaws 9/10. The four misses:
  - `Ace of Diamond Act II S2 - 10` → `Diamond no Ace: Second Season` (2015): the release
    counts seasons of Act II, the Fribb remap took TVDB season 2 of the whole series.
  - `Dr. Stone Science Future - 31` → `Dr. Stone: Stone Wars` E7: episode 31 counts within
    Science Future, but the absolute-episode remap walked the series from season 1.
  - `[Doomdos] 搜神记 - 第9话` → `Sou Shen Ji` (2006 TV) instead of the 2026 ONA: same title,
    and no year in the name.
  - `[shincaps] Youkoso Jitsuryoku … - 10` → season 1 (2017). It may be a rerun, but the 2026
    4th season is more likely.
  This is a sanity check, not a precision estimate per group: the sample is small, and it was
  judged from titles by the agent that wrote the matcher. Remaining weak spots are new versions or sequels with the same
  bare title and no year in the name.

Remaining errors are mostly ground-truth ambiguity, where the description links the season page
while the episode belongs to its second cour (Fire Force S03E14 to E24, Uma Musume Cinderella
Gray S01E17 and later). The rest are specials (S00) with no specific synonym, and Russian `TV-2`
names whose main title loses the number. AniList, Jikan and AniDB are not used online: Jikan
timed out three times on 2026-10-25, AniList is degraded to 30 requests a minute, and AniDB bans
easily.

## Operating

| Env | Default | |
|---|---|---|
| `MAGNETAR_ANIME_DB` | `true` | `false`: no download, no matching |
| `MAGNETAR_ANIME_AOD_URL` | latest cedya77 release (`.jsonl.zst`) | URL or local path; pin with `…/download/<tag>/…` |
| `MAGNETAR_ANIME_FRIBB_URL` | Fribb `master/anime-list-full.json` | URL or local path; `none` = AOD only |

- `magnetar-server enrich-anime [--force]` imports when due (or always with `--force`), matches
  everything pending and prints a JSON report. `serve` runs the same steps every 10 minutes,
  starting 30 s after boot.
- The loop only starts working once the `media` / `torrent_media` tables exist (the IMDb
  migration). Until then it logs one warning.
- Resources: about 14 MB downloaded per weekly change, about 40k rows per table, and the in-memory
  index (tens of MB).
- Docker / native: nothing else to install. ruzstd and both parsers are pure Rust.
