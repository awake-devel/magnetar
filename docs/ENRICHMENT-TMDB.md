# TMDB decoration (D42)

Posters, overviews and episode names for torrents that are already **identified** offline
(IMDb datasets, docs/ENRICHMENT.md, D40; anime lists, docs/ENRICHMENT-ANIME.md, D41). TMDB is
never used to *find* what a torrent is: it is asked **by ID only**, on the server, with the
operator's own token. The phone never talks to TMDB and never holds a key.

Research: [research/metadata-enrichment.md](research/metadata-enrichment.md) §2 (TMDB terms and
limits), §4.3, §5 (pipeline steps 3-4, 6).

## Switch it on

```
MAGNETAR_TMDB_TOKEN=<your TMDB API Read Access Token>   # unset = TMDB off
MAGNETAR_TMDB_LANGS=en-US,fr-FR                        # preferred first
MAGNETAR_IMAGE_CACHE_DIR=/var/cache/magnetar/images     # default ~/.cache/magnetar/images
MAGNETAR_TMDB_RPS=20
MAGNETAR_TMDB_PREFETCH_PER_HOUR=60                      # 0 = only on detail views
```

The token goes in the server's environment only (env file, Docker secret/env, systemd
`EnvironmentFile`). It is never in the repo, the APK or a log line: request logs (`RUST_LOG=
magnetar_server::tmdb=debug`) print the path without the query. A 32-hex v3 API key is also
accepted (sent as `api_key=`); anything else is sent as `Authorization: Bearer`. Get your own key:
TMDB's terms expect every application to use its own, so do not reuse one from another project
(bitmagnet ships one).

Everything else is optional; with no token the server behaves exactly as before.

## What is fetched, and when

For one media row (`media.id`):

| Step | Request | Stored as `media_extra(source, lang)` | Fresh for |
|---|---|---|---|
| IMDb → TMDB | `GET /3/find/{tt…}?external_source=imdb_id` (skipped when `media.tmdb_movie`/`tmdb_tv` is set, e.g. by the anime lists) | `tmdb_find`, `''`; the id is also written to `media.tmdb_movie`/`tmdb_tv` if empty | positive: kept; no match: retried after 30 d |
| Text, all languages | `GET /3/movie/{id}` or `/3/tv/{id}` `?language=<first>&append_to_response=translations` | `tmdb`, one row per configured language: title, overview, tagline, poster/backdrop path | 90 d |
| Episode names | `GET /3/tv/{id}/season/{n}?language=<lang>`, per language, only when a torrent has season **and** episode | `tmdb_season:<n>`, per language | 90 d |
| Poster | `GET image.tmdb.org/t/p/w342{poster_path}` | file `<cache>/<sha256[0..2]>/<sha256>.<ext>`, row `tmdb_image`, `''` | 90 d |

So a film costs 3 requests the first time (find, details, poster), an episode 5 with two
languages, and nothing after that until the refresh. The kind the matcher decided chooses
between `movie_results` and `tv_results`; an IMDb *episode* id resolves to its show
(`tv_episode_results[].show_id`).

Language choice: the details body is in the first language; the other languages come from
`translations` (exact region first, then any region of the same language). The overview shown
is the first configured language that has one; TMDB leaves untranslated overviews empty, so
`fr-FR` often falls back to `en-US` and vice versa.

Triggers:
- **Detail view** (`GET /api/v1/torrents/{infohash}`): `MediaInfo.overview`, `.poster`,
  `.episode_title` and `.tmdb` are filled from `media_extra` (one indexed query). If something
  is missing or stale, the media is queued for the background workers and the response is
  returned at once: TMDB never sits in the request path. A job is not queued again within
  15 min (also after a failure).
- **Prefetch**: every 10 min, up to `MAGNETAR_TMDB_PREFETCH_PER_HOUR / 6` media matched in the
  last 7 days without fresh data, most linked releases first, then IMDb votes.
- **Poster request** for a media whose poster path is known but whose file is missing: fetched
  then (≤ 15 s), so the first view after a cache wipe still shows the poster.

Rate limit: one token bucket for API and image requests, `MAGNETAR_TMDB_RPS` per second, bursts
of up to 5 (TMDB documents "somewhere in the 40 requests per second range"). 429 and 5xx are
retried up to 3 times, waiting `Retry-After` seconds when present (else 2, 4, 8 s; at most 60 s).
404 is a normal answer (cached as "nothing"), 401 is an error in the log.

## Serving posters

`GET /api/v1/media/{id}/poster` (bearer token like every API route): the cached bytes with
`Content-Type`, `ETag: "<sha256>"`, `Cache-Control: private, max-age=604800`; `If-None-Match`
gives 304. 404 when TMDB is off or the media has no poster. The server never redirects to or
embeds `image.tmdb.org` URLs: clients only see `/api/v1/…` paths (`MediaInfo.poster`).

`GET /api/v1/media/{id}/torrents` lists the other releases of the media (the `media:N` search
token of D40, best seeded first).

## Terms we follow (TMDB API Terms of Use, read 2026-10-25)

- §1.C: no caching beyond 6 months. Rows are refreshed after 90 days when still viewed or
  recent, and **deleted after 180 days** by the purge (every 6 h), together with poster files
  no row references. Turning TMDB off (unsetting the token) stops all requests; to remove the
  cache, `DELETE FROM media_extra WHERE source LIKE 'tmdb%'` and delete the image directory
  (also required by §1.D when your licence ends).
- §2.A: commercial use needs an agreement with TMDB. Magnetar is a personal tool.
- §3: the app shows the TMDB logo (less prominent than Magnetar's own name) and the notice
  *"This product uses TMDB and the TMDB APIs but is not endorsed, certified, or otherwise
  approved by TMDB."* in Settings (`ui/settings/TmdbAttribution.kt`). The logo is TMDB's
  "alt short" SVG from themoviedb.org/about/logos-attribution, converted to a vector drawable.
- TMDB data is not redistributable: nothing from `media_extra` is published to Nostr or synced
  anywhere; the phone receives it only from its own server for display.

## Phone

- `FfiTorrentDetail.media` (`FfiMediaInfo`) and `FfiTorrentSummary.media_title/year/kind`.
- The server's `MediaInfo` of every detail opened online is stored in SQLite (`media_cache`,
  schema V3), so the media card works offline and `imdb:tt…`, `tmdb:movie:N`, `tmdb:tv:N`,
  `mal:N` work in local search too (also matching a torrent's own `i` tag). Rows of evicted
  torrents are dropped by `cleanup()`.
- `Magnetar.media_poster(media_id, poster_url)` downloads the poster from the server with the
  bearer token into `<db dir>/posters/` (`poster_cache`, 30 MB LRU, ETag revalidation after
  30 days) and returns the file path; offline it returns the cached file. Only `/api/v1/` paths
  are accepted.
- `Magnetar.media_torrents(media_id)`: server list, else the cached torrents linked to it.
- Detail screen: poster, title (year), kind, ★ rating (votes), genres, `S01E02 · name`,
  overview, an IMDb button (`https://www.imdb.com/title/tt…/`), and "Other releases". The
  screen re-reads the detail once after 4 s when the server had not decorated it yet.

## Measured

Live run on 2026-10-25 against the real TMDB API with the operator's token (branch
`enrich/tmdb`, debug build, test database; the `media`/`torrent_media` rows were built from the
real IMDb datasets for the torrents' own `imdb` i-tags, standing in for the enrich/imdb matcher).
Sample: 45 production torrents with distinct IMDb i-tags (29 films, 16 series; 12 with SxxEyy),
their real signed events ingested through `POST /api/v1/events/batch`; languages `en-US,fr-FR`,
prefetch off, so every request came from the lazy detail path.

| | Result |
|---|---|
| TMDB requests, total | **159** (45 `/find`, 29 `/movie`, 16 `/tv`, 24 `/tv/{id}/season`, 45 posters); 0 retries, 0 errors |
| Per title | 3.0 (film), 5.0 (episode, 2 languages) — as designed |
| Resolved to a TMDB id | 45/45 |
| Overview present | 45/45 en-US, 45/45 fr-FR (from `translations`) |
| Poster cached and served | 45/45, avg 42.6 KB (w342), 2.0 MB on disk; 45/45 `If-None-Match` → 304 |
| Episode name for SxxEyy torrents | 12/12 |
| Detail latency, first view (fetch queued) | avg 46 ms, max 311 ms (localhost, debug build) |
| Detail latency, decorated | avg 45 ms, max 100 ms |
| Background queue drained | < 30 s for all 45 (2 workers, 20 req/s) |
| Poster endpoint | avg 5.5 ms, max 15 ms; 401 without the token |
| Second round of views + posters | 0 further TMDB requests |
| Phone path (`magnetar-ffi` against that server, `media_against_server`) | detail with media, poster file (34.5 KB JPEG) via the bearer token, other releases, then the same media offline |

## Notes for operators

- Docker / native: the only state is the `media_extra` table and `MAGNETAR_IMAGE_CACHE_DIR`
  (mount it as a volume; losing it only costs re-downloads).
- Request budget: about 3 requests per new film and 5 per new episode with two languages;
  the default prefetch (60/h) is at most ~300 requests/hour, a fraction of TMDB's limit.

## Development notes

- `media_extra` references `media` (enrich/imdb's migration `20261026000000_enrich_imdb.sql`);
  this migration is `20261027200000_tmdb_media_extra.sql`. While enrich/tmdb was built without
  that migration, `crates/server/tests/common/contract_media.sql` (TEST-ONLY mirror of the
  contract tables) is created before migrating in tests; it is skipped automatically once a
  migration that creates `media` is present.
- Tests: `crates/server/tests/tmdb.rs` (fake TMDB in-process: find/details/season/poster, 429 +
  `Retry-After`, negative cache, purge, lazy detail decoration, poster ETag/304, other
  releases, prefetch selection), `crates/ffi/tests/media.rs` (offline cache, posters, local
  `imdb:` search; `media_against_server` is `#[ignore]` and needs a live server).
