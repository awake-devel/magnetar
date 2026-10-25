# Metadata enrichment: data that is not on the DHT

Research date: 2026-10-25. Full report: `/tmp/claude-1000/-home-dude-backups/4523d0ae-a215-4c5b-b5e1-5b80e2cf6cbf/scratchpad/bitmagnet/enrichment.md`
(per-source sections §3.1-3.14). Scripts: `…/bitmagnet/enrichment-scripts/`; data and outputs:
`…/bitmagnet/enrichment/`; fact-check scripts: `…/bitmagnet/check-enrichment/`.
The Magnetar database was only read (SELECT); nothing under `~/magnetar` was changed by the research.
Related: [bitmagnet.md](bitmagnet.md) §5 (its TMDB integration), [dht-crawling.md](dht-crawling.md).

Titles, posters, year, ratings and episode info are **not on the DHT or in the info dict**. The
info dict gives only name, file list, sizes, piece hashes. Everything else comes from filename
parsing plus external databases.

---

## 1. What Magnetar's data already contains

Snapshot cut at `events.received_at <= 1792948332` (2026-10-25 17:12:12 UTC, 125,714 rows);
fact-check SQL `check-enrichment/cov.sql`.

| Category | Distinct infohashes with a strong ID (i-tag or description link) | Full DB at check time |
|---|---|---|
| Video | 15,257 / 31,017 = **49.2%** (re-derived 49.1%) | 23,216 / 42,431 = 54.7% (189,004 rows) |
| Anime | 9,186 / 66,764 = **13.8%** (re-derived 13.7%) | 11,128 / 82,565 = 13.5% |

- Strong IDs: IMDb, TMDB, TVDB, MyAnimeList, AniList, AniDB.
- Spec `i` tags are rare: `imdb` 1,585 (1,654 now), `tmdb` 5, `mal` 3, `ttvdb` 2, `anilist` 2.
  Source prefixes dominate: `tcat` 91,274, `nyaa` 74,095, `ygg` 29,781, `tpb` 16,338.
- Most IDs are **description links**: 15,141 rows link TMDB, 13,535 of them from the YGG bridge
  pubkey `6aeb5506…`. Anime IDs are mostly MAL/AniDB links in Nyaa descriptions.
- **Step 0 of any pipeline: extract these links. It costs nothing.**

---

## 2. Sources

| Source | Covers | Access / rate limit | Licence / terms | Notes |
|---|---|---|---|---|
| **IMDb datasets** | Film, TV, episodes, ratings, AKAs | Bulk TSV, refreshed daily; `title.basics.tsv.gz` 229,191,244 B (Last-Modified 2026-10-25) | **Personal, non-commercial only** (developer.imdb.com/non-commercial-datasets) | Best offline identifier. |
| **TMDB API** | Posters, overviews (FR/EN), IDs | "somewhere in the 40 requests per second range" (legacy 40/10 s removed 2019-12-16); key required | §1.C cache ≤ 6 months; §2.A commercial use needs agreement; §3 TMDB logo + "not endorsed" notice; data not redistributable | bitmagnet: 20/s burst 5 with own key, 1/s burst 8 with shared key, 2 in flight. Key sign-up and live enforcement UNVERIFIED. |
| **TVmaze** | Shows, episodes | No key; "at least 20 calls every 10 seconds per IP" | CC BY-SA | `/lookup/shows?imdb=tt0944947` → 301 `/shows/82`; `?thetvdb=81189` → 301 `/shows/169` (live). |
| **Wikidata** | IMDb↔TMDB↔TVDB↔MAL bridges | SPARQL: 60 s query deadline; 60 s processing per 60 s per client; 5 parallel per IP | CC0 | Dump `latest-all.json.gz` 156.6 GB (bz2 103.5 GB). Use paged SPARQL exports instead. |
| **anime-offline-database** | Anime titles, cross-IDs, seasons | Weekly JSON | ODbL 1.0 + DbCL 1.0 (share-alike) | manami-project archived (last push 2026-07-04, release 2026-27). Continued by **cedya77/anime-offline-database** (independent continuation repo, not a GitHub fork; release 2026-43, 2026-10-19). Single maintainer; stability UNVERIFIED. |
| **Fribb anime-lists** | MAL/AniDB/AniList/TVDB/TMDB mapping incl. seasons | Static JSON | **No licence file** (UNVERIFIED terms) | Season mapping is what makes anime matching work. |
| **AniList** | Anime covers, metadata | GraphQL, no key; **currently degraded to 30 req/min** (live `x-ratelimit-limit: 30`; normal 90) | Third-party ToS UNVERIFIED | |
| **Jikan** (MAL proxy) | Anime | 3/s, 60/min, 24 h cache (`jikan-rest` api-docs) | – | **Timed out 3× on 2026-10-25.** Don't depend on it. |
| **AniDB** | Anime | Strict, bans easily; docs behind Cloudflare | – | Limits second-hand only (UNVERIFIED). |
| **MusicBrainz** | Audio | 1 req/s per IP; meaningful User-Agent required | Core CC0; supplementary CC BY-NC-SA 3.0 | On demand only. |
| **IGDB** | Games | 4 req/s, 8 open requests; Twitch account with 2FA | Free non-commercial (Twitch Developer Service Agreement) | |
| **OMDb** | Film/TV | 1,000 req/day; posters patrons only | Data licence UNVERIFIED | |
| **Trakt**, **Kitsu**, **Open Library** | – | Limits UNVERIFIED (Trakt: only deprecated docs read) | UNVERIFIED | |
| **itorrents.org / .net** | .torrent files for DHT-less hashes | HTTP | – | **14/20** hits; fact-check re-run **16/20** (anime 9/9, book 4/6, video 3/3, audio 0/1, app 0/1) → ~70-80%. **Misses return HTTP 200 HTML spam** (77,286 B); validate bencode + `SHA-1(info) == infohash`. btcache.me / torrage.info: only HTML seen (UNVERIFIED). |

---

## 3. Filename parsers
Test: 1,328 imdb-tagged torrents (~1,311 distinct infohashes; some IDs repeat up to 20× in episode
packs), mostly English scene releases. "Correct" also counts a hit on the episode's parent series.

| Parser | Language / licence | Correct | Speed | Verdict |
|---|---|---|---|---|
| **parse-torrent-title 2.0.2** | Rust, MIT | **93.6%** (1243/1328; 2.9% wrong, 3.5% none) | ~0.5-1.0 ms/title | **Pick.** Pin exact version: brand-new Rust rewrite by the original JS author. |
| PTT (Python) | Python | 93.4% | – | Reference. |
| guessit | Python | 91.9% | ~85 ms/title | Too slow. |
| hunch 2.0.2 | Rust (guessit port) | 91.7% | – | |
| anitomy-ng 1.0.10 | Rust | 91.0% | fastest | Anime-only cross-check. |
| torrent-name-parser 0.12.1 | Rust | 88.9% | – | Unmaintained since 2023. Avoid. |

bitmagnet uses its own regexes (`hedhyw/rex`) and Levenshtein threshold 5
(`internal/classifier/util.go:9`). French YGG title accuracy: UNVERIFIED (likely lower).

---

## 4. Matching strategy and results

### 4.1 Film / TV (offline IMDb)
Parse title → normalise → exact match against `title.basics` → rank by type, year, numVotes.
- 1,328 test: **93.6% correct, 2.9% wrong, 97.0% precision when a match is returned**.
- Add `title.akas` for French titles (gain UNVERIFIED).
- Add `title.episode` for S/E resolution and `title.ratings` for ranking.

### 4.2 Anime (offline AOD + Fribb)
- 8,530 MAL-linked torrents (881 distinct MAL IDs, mostly current-season titles); Python PTT parser.
- Naive title match: **65.0%**; 94% of errors were later seasons (MAL/AniDB list each season
  separately).
- With Fribb season remap: **87.3% correct, 5.3% wrong, 7.4% none**.

### 4.3 Online decoration (only for torrents users open)
- TMDB: posters, FR/EN overviews (by ID; no search needed when the ID is known).
- TVmaze: episodes (lookup by IMDb/TVDB).
- AniList: anime covers.
- MusicBrainz: audio, on demand.

---

## 5. Recommended pipeline (server on laptop; phone app without Google services)

0. **Existing IDs:** read `i` tags (including `tmdb:movie:ID` forms) and extract
   IMDb/TMDB/TVDB/MAL/AniList/AniDB links from descriptions.
1. **Parse title** with `parse-torrent-title` 2.0.2, `anitomy-ng` as anime cross-check. For DHT
   torrents use the info-dict name, or the largest video file's name.
2. **Identify offline**, refreshed weekly (~300 MB): IMDb `title.basics`, `title.episode`,
   `title.ratings` (+ optional `title.akas`); AOD (cedya77) + Fribb with season mapping;
   IMDb↔TMDB bridge from a paged Wikidata SPARQL export.
3. **Decorate online lazily**, rate-limited on the server, by ID only (TMDB, TVmaze, AniList,
   MusicBrainz).
4. **Images:** server caches posters (≤ 6 months for TMDB) and serves them; the phone never holds
   API keys or contacts TMDB/AniList.
5. **.torrent for DHT-less hashes:** itorrents fallback with strict bencode + SHA-1 validation;
   librqbit `list_only` with a hard timeout for single on-demand magnets.
6. **No republishing** of enriched data to Nostr relays: IMDb is non-commercial, TMDB data is not
   redistributable, AOD is ODbL share-alike.

Do not reuse bitmagnet's embedded TMDB key; TMDB terms expect each app to use its own.

---

## UNVERIFIED
- TMDB key issuance and live enforcement of ~40 rps.
- Trakt current limits; AniDB exact limits; Kitsu limits.
- OMDb, Open Library, AniList data terms; Fribb licence.
- btcache.me / torrage.info raw .torrent support.
- cedya77 long-term stability.
- Parser accuracy on French YGG titles; `title.akas` gain.
