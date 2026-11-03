# HTTP API for a thin client

What a client that talks only to the server's REST API (no Rust ffi, no phone-side crawl) uses,
endpoint by endpoint. The full table with request/response types is ARCHITECTURE.md §6; the
types are `magnetar_core::api` / `magnetar_core::model` (JSON field names = Rust field names,
enums in `snake_case`). Checked for the app rebuild in 2026-10 (D77).

## Basics

- Base URL: the server's address, e.g. `https://<host>`. JSON in and out.
- Auth: `Authorization: Bearer <MAGNETAR_API_TOKEN>` on everything except `GET /health`, the
  poster images included (an image loader needs the header, e.g. an OkHttp interceptor).
- Errors: `{"error": "<code>", "message": "…"}` with 4xx/5xx. Codes: `unauthorized`, `invalid`,
  `invalid_body`, `not_found`, `search_index_unavailable` (503 while the index builds), `downloads_unavailable` (503),
  `upstream` (502), `timeout`, `store`, `internal`.
- Feature detection: `GET /health` → `{"status","version","db","features":[…]}`. A client
  hides what the server does not list. Since this stage: `saved_search_poll` (D77) and
  `browse_checked_dht` (D76), next to `filters`, `facet_set`, `media_releases`,
  `liveness_history`, `stats`, `graphs`, `adult_filter`, `origin`, `crawl_sources`, `crawl_map`
  (D102, D103), and `dht` (crawler on).
- Unknown query parameters are ignored; unknown JSON fields are ignored; every newer field has
  a default, so an older server's answers still parse.

## What each screen needs

| Need | Endpoint | Notes |
|---|---|---|
| Search / browse | `GET /api/v1/search?q=…` or `POST /api/v1/search` (`SearchQuery`) | `SearchPage {hits, total, source, total_is_lower_bound}`. Query string: `q, cat, min_size, max_size, since, until, min_seeders, hide_dead, min_trust, sort (relevance/date/size/seeders/rating), desc, limit (≤ 200), offset (≤ 10 000, else 400 `invalid`, D110), lang, sub, res, source, codec, hdr, audio, genre, kind, min_rating, min_year, max_year, origin (nostr,dht), adult=1`. `min_trust` omitted = the server's threshold. |
| Adult switch (off by default) | `adult=1` / `"include_adult": true` on every search, facet, breakdown, releases and saved-search poll | Adult torrents are hidden, never deleted (D52, D70). Selecting category `porn` also shows them. |
| Empty-query browse | same | D76: no text and no source filter → DHT-only torrents only once a seed check found peers. Text searches, `origin=dht` and any `probe_*` filter (D122) list every DHT result. |
| Result row | `TorrentSummary` | `title, category, total_size, file_count, created_at, seeders, leechers, dead, best_peers` (max of tracker seeders and DHT peer estimate; show this when `seeders` is null), `trust, publisher_count, origin (nostr/dht/both), media_title, media_year, media_kind, media_id, media_rating, attrs` (resolution, languages, …), `matched_file`. |
| Filter sheet counts | `POST /api/v1/facets` (`{query, fields, size}`) or `GET /api/v1/facets?field=…` | Disjunctive counts (docs/FILTERS.md); 503 while the index builds. |
| Detail | `GET /api/v1/torrents/{infohash}` | `TorrentDetail`: summary, `magnet` (ready to hand to a torrent app), files, description, events, comments, trackers, `trust` (score + factors), `media` (`MediaInfo` incl. `poster`), `dht` (`DhtInfo` for DHT rows), `probe` (`ProbeInfo`, header-probe results, absent when never probed; see below). 404 unknown. |
| Seed check now | `POST /api/v1/torrents/{infohash}/liveness` | `Liveness {seeders, leechers, dht_peers_est, source, checked_at, expires_at, …}`. Works for DHT-only torrents since D77 (was 404). Rate-limited since D113: a repeat within a minute returns the stored result, more than 20 a minute server-wide → `429` with `Retry-After`. |
| Seed history chart | `GET /api/v1/torrents/{infohash}/liveness-history?days=N` | ≤ 90 days, oldest first. |
| Title page | `GET /api/v1/media/{id}/releases` (+ search query string) or `POST` (`SearchQuery`) | `MediaReleases {media, total, truncated, groups}`; `GET /api/v1/media/{id}/torrents` = flat `SearchPage`. |
| Posters | `GET /api/v1/media/{id}/poster` | Image bytes, `ETag` + `If-None-Match` → 304, `Cache-Control: private, max-age=604800`; 404 when there is no poster or TMDB is not configured. URL: `MediaInfo.poster` (server-relative), or built from a hit's `media_id`. No backdrops or stills. |
| Stats tab | `GET\|POST /api/v1/stats/breakdown`, `GET /api/v1/stats/timeseries`, `GET /api/v1/stats/health`, `GET /api/v1/stats`, `GET /api/v1/stats/sources`, `GET /api/v1/stats/map` | docs/STATS.md. Breakdowns count the whole catalogue (D76 does not apply). |
| Graphs | `GET /api/v1/graphs/wot`, `/relays`, `/media/{id}`, `/swarm/{infohash}` | `Graph {kind, nodes, edges, truncated, note}`. |
| Trust settings | `GET` / `PUT /api/v1/settings/trust` (`{"root": npub-or-hex or null, "threshold": 0–100}`) | |
| Block / trust a publisher | `PUT /api/v1/overrides/{pubkey}` (`{"action": "block"\|"trust"\|null}`) | 204. There is no list endpoint: a client that shows "blocked publishers" keeps its own list. |
| Publisher trust | `GET /api/v1/trust/{pubkey}` | `TrustScore`. |
| Relays | `GET` / `PUT /api/v1/relays` (`{"url","enabled"}`) | The server's crawl relays. |
| Saved searches | `GET /api/v1/saved-searches`, `POST` (create, `id` 0), `GET\|PUT\|DELETE /api/v1/saved-searches/{id}` | Stored on the server (`SavedSearch {id, name, query, notify, last_checked, last_seen_created_at}`). `POST {"name","query","notify"}` is enough (every field defaults). `PUT` takes name, query and notify and keeps the server notifier's watermarks (a changed query resets them). An edit made while the server notifier runs is kept: the notifier writes only its watermark columns. |
| Notifications | `GET /api/v1/saved-searches/poll?since=<watermark>[&adult=1][&all=1][&id=1,2]` | See below. |
| Server downloads | `GET\|POST /api/v1/downloads`, `POST /api/v1/downloads/{id}/{pause\|resume}`, `DELETE /api/v1/downloads/{id}` | Only when the server runs with downloads; 503 `downloads_unavailable` otherwise. |

## Saved-search notifications (D77)

`GET /api/v1/saved-searches/poll` runs the saved searches with `notify` (all of them with
`all=1`; `id=` narrows to a comma list) and returns what is new since the client's watermark:

```json
{"watermark": 1790000050,
 "results": [{"search": {"id": 1, "name": "Ubuntu", "query": {…}, "notify": true, …},
              "hits": [TorrentSummary, …], "truncated": false}]}
```

- Stateless on the server: nothing is written. The client stores `watermark` and sends it back
  as `since`. So two phones, a phone and the server's own ntfy notifier (D25) never take each
  other's new hits.
- First poll (no `since`): no hits, `watermark` = the server's clock.
- "New" = `created_at > since` (the publication time, as the ntfy notifier and the old phone
  poller): hits newest first, at most 200 per search (`truncated` when there were more); the
  next watermark is the newest `created_at` returned, or `since` when nothing matched. An event
  that reaches the server late with an older `created_at` than the watermark is not reported
  (Nostr events are accepted up to 15 min in the future, so a watermark cannot run ahead by
  more than that). The same holds for a DHT torrent stored long after it was first sighted
  (its `created_at` is the first sighting), and for a saved search with no text, where D76
  lists a DHT-only torrent only after a seed check: such a torrent can predate the watermark by
  the time it becomes visible. A per-arrival watermark would need an arrival time in the
  search index; not done.
- `adult=1` adds the client's adult switch to every query; otherwise each saved query's own
  `include_adult` applies.
- A periodic poll from Android WorkManager (e.g. 15 min) is enough; each poll is one search per
  saved search.

## Crawl map (D101–D105)

`GET /api/v1/stats/sources` (`CrawlSources`: DHT routes with seen/stored and 48 h per hour,
relays by discovery source and the top relays, one line per crawling machine) and
`GET /api/v1/stats/map?layer=dht|nostr&window=day|all` (`CrawlMap`: counts by country and the
top 50 networks, `available`/`reason` when the geo database is off or missing, and the DB-IP
`attribution` a client must show with a link to https://db-ip.com). Features `crawl_sources`,
`crawl_map`. No answer contains an IP address (D101). Details: docs/CRAWLMAP.md.

## Crawl workers

`/api/v1/worker/*` is for `magnetar-server worker` only (worker tokens, docs/WORKERS.md): the
app's API token gets 401 there. `GET /api/v1/stats` gains `workers` (one `WorkerStats` per
configured worker: connected, version, leases held, seed results and metadata per minute, DHT
state; empty without workers), and `/health` lists the feature `workers` when tokens are
configured. Clients may ignore both.

Worker wire additions for header probes (D117, docs/PROBES.md), all optional (worker API stays
1): `DhtResult.probe` (`ProbeReport {v, status, bytes_in, uploaded, peers, files: [ProbeFile]}`,
only with a `metadata` item), `DhtAck.probe_stored` / `probe_rejected`, and
`WorkerDhtState.probe` (`WorkerProbeState` counters) in the heartbeat and `workers[].dht`.
Probe backfill (D118, docs/PROBES.md §10), all optional too: the routes `probe/lease` and
`probe/release`, `DhtAck.backfill_stored` (new DHT rows from backfill dicts, not in `stored`),
`WorkerDhtState.discover` and `.backfill` (`WorkerBackfillState`),
`WorkerStats.backfill_*_total`, and `StatsResponse.probe_backfill` (`ProbeBackfillStats`: the
server's queue, absent without workers).

## Header probes (D117)

`TorrentDetail.probe` (absent when the torrent was never probed; adult and BEP 27 private
torrents are probed too since D119, while older servers hid it on adult ones; older servers
never send it; the Android app shows it as the detail's "Detected" card since D121):

```json
"probe": {
  "version": 2, "probed_at": 1793500000, "status": "ok", "verified": true,
  "files": [{
    "index": 0, "path": "Movie.2024.2160p.mkv", "size": 6000000000,
    "label": "video.mkv", "description": "Matroska video", "confidence": 97, "verified": true,
    "flags": ["agreed"],
    "video": {"container": "matroska", "duration_ms": 7200000,
              "video": [{"codec": "hevc", "width": 3840, "height": 2160, "bit_depth": 10,
                         "hdr": ["dolby_vision", "hdr10"], "dv_profile": 8, "dv_compat": 1}],
              "audio": [{"codec": "eac3", "channels": 6, "lang": "fre", "atmos": true},
                        {"codec": "ac3", "channels": 2, "lang": "eng", "kinds": ["commentary"],
                         "name": "Director's Commentary"}],
              "subs": [{"codec": "hdmv_pgs_subtitle", "lang": "eng", "kinds": ["sdh"],
                        "name": "English (SDH)"},
                       {"codec": "subrip", "lang": "fre", "kinds": ["forced"]},
                       {"codec": "eia_608", "kinds": ["cc"]}]}
  }]
}
```

- `index` points into `files` of the same response (the Nostr event's list when there is one,
  else the DHT row's); `path` is that entry's path. A probed file the shown list does not hold
  (matched by path, with or without the root folder, and size) is left out.
- `label` is a sniff label id (`video.mkv`, `game.ps5.pkg`, `exe.pe`, …), `confidence` 80–100.
- `verified`: the bytes the detection read were SHA-1 checked against the torrent's piece
  hashes; `false` means 16 KiB blocks from one peer, not checked (docs/PROBES.md §3).
- `flags`: `extension_mismatch`, `suspicious_executable` (worth a warning in a client),
  `incomplete`, `agreed` (two peers gave the same verdict), `disagree`, `surprise_verified`,
  `surprise_changed`, `surprise_unverified`.
- `game` (console files): `platform` (`ps5`, `switch`, …), `kind`, `format`, `title_id`,
  `content_id`, `db_id`, `title`, `version`, `region`.
- `video[].dv_profile` / `dv_compat` (D121): the Dolby Vision profile and base-layer
  compatibility id (profile 8: 1 HDR10, 2 SDR, 4 HLG) from the configuration record, only with
  `dolby_vision` in `hdr`; probes stored by workers older than D121 have neither.
- `audio[].kinds` / `subs[].kinds` (D123, `version` 2): what kind of track it is, alphabetical,
  left out when none. Audio: `alternate`, `commentary`, `description` (audio description),
  `dialogue` (dialogue-enhanced), `dub`, `karaoke`, `music` (music & effects, isolated score),
  `original`, `voiceover`. Subtitles: `bilingual`, `cc`, `commentary`, `description`,
  `easy_reader`, `forced`, `honorifics`, `sdh`, `signs`. A `cc` subtitle with codec `eia_608` /
  `eia_708` and no language is a caption channel signalled in the video stream (one with a
  language comes from an ATSC caption_service_descriptor or an MP4 caption track).
  Probes of `version` 1 have none.
- `audio[].name` / `subs[].name` (D123): the track name as the file has it (at most 64 bytes, no
  control characters), kept so the server can re-classify later; free text from peers, so
  clients should clean it before showing it (the app shows it since D131, as VLC does, without
  bidi controls).
- `pos`, `fix`, `dht_index`, `unplaced` (D128–D130): track corrections, below.
- Languages are ISO 639-2/B lowercase. Every optional field is left out when unknown.

Search filters on probe results (D122, `features` lists `probe_filters`): `SearchQuery`
`probe_languages`, `probe_subtitles`, `probe_resolutions`, `probe_video_codecs`, `probe_hdr`,
`probe_audio` (query string `probe_lang`, `probe_sub`, `probe_res`, `probe_codec`, `probe_hdr`,
`probe_audio`) and the facets `probe_language`, `probe_subtitles`, `probe_resolution`,
`probe_codec`, `probe_hdr` (with `any`), `probe_audio` in `POST /api/v1/facets`; values and
rules in docs/FILTERS.md "Probe filters". All optional: older clients send none.

Track-kind filters (D125, `features` lists `probe_kinds`): `SearchQuery` `probe_sub_kinds`,
`probe_audio_kinds`, `probe_sub_formats` (query string `probe_sub_kind`, `probe_audio_kind`,
`probe_sub_format`, comma lists or repeated) and the facets `probe_sub_kind`, `probe_audio_kind`,
`probe_sub_format`. A language and a kind of the same family match one track having both (French
+ SDH = a French SDH track) and their facets are counted that way; docs/FILTERS.md "Track
kinds". A server without `probe_kinds` ignores the fields and answers unfiltered: send them
only to a server that lists it.

## Track corrections (D128–D130)

`features` lists `probe_corrections`. The user corrects, per audio and subtitle track, what VLC
shows: the language, the kinds, the name, a track that does not exist, and a track the probe
missed. Corrections are stored apart from the detection, survive every re-probe, and apply
everywhere: the detail's `probe`, the stored tags, both search paths and the facet counts
(docs/PROBES.md §12, docs/FILTERS.md). Not correctable: codec, resolution, HDR.

| Route | Body | Answer |
|---|---|---|
| `POST /api/v1/torrents/{infohash}/probe/corrections` | `ProbeCorrectionRequest` | 200 `ProbeInfo` (the same object as `TorrentDetail.probe`, effective) |
| `DELETE /api/v1/torrents/{infohash}/probe/corrections/{id}` | | 200 `ProbeInfo` (undo: back to the detection) |

Request (all items or none, 1–96 items):

```json
{"device":"android","items":[
  {"file":0,"track":"sub","pos":1,"set":["lang","kinds"],"lang":"fre","kinds":["forced"]},
  {"file":0,"track":"audio","pos":0,"set":[]},
  {"file":0,"track":"audio","pos":2,"removed":true},
  {"file":0,"track":"sub","set":["lang","kinds","name"],"lang":"spa","kinds":["forced"],"name":"Español (forzados)"},
  {"id":42,"file":0,"track":"sub","set":["lang"],"lang":"cat"}
]}
```

| Field | Meaning |
|---|---|
| `file` | `ProbeFileInfo.dht_index` |
| `track` | `audio` or `sub` |
| `pos` | 0-based position in the file's detected list (`pos` of the track in the detail) |
| `set` | Which of `lang`, `kinds`, `name` the item overrides; a field not in `set` is ignored. `"lang"` with `lang` absent, null, empty or `und` means "no language"; `"name"` with `name` absent means "no name"; `"kinds"` with `kinds` absent means none |
| `removed` | "This track doesn't exist" (default false): a detected track only, with nothing else |
| `id` | Edit an added track (fields not in `set` keep their values) |
| `device` | Optional, 1–32 printable bytes, default `api` (one token for every device, so the client names itself) |

- **`pos`, no `id`**: the correction of that detected track. One already anchored there is
  updated in place (fresh snapshot of the detection, new `at`); else a new one. `set: []` (not
  `removed`) is "matches VLC": a check, no override. A value equal to the current detection is
  dropped from `set`, so saving an unchanged value is a check.
- **No `pos`, no `id`**: a track the probe missed. It needs a language, a kind or a name.
- **`id`**: an added track's language, kinds or name. A detected track's correction is changed
  with `pos`, not `id`.
- Languages go through the server's normalisation (`fr`, `fra`, `FRE` → `fre`, `mri` → `mao`)
  and must then be one of the 89 ISO 639-2/B codes the app's picker offers
  (`probe_fix::CORRECTION_LANGS`), or the track's own detected code (setting it back is a check);
  any other code (`xyz`) is `unknown language code`; kinds are the
  closed lists of the track type (above), no duplicates; names are trimmed, ≤ 64 bytes, no
  control characters, empty = none.

Errors: 400 `invalid` with the message prefixed `item {n}: ` (n 0-based) where it concerns one
item: `items: 1 to 96 per request`, `track must be "audio" or "sub"`, `no probed video file
{file}`, `no subtitle track at pos {pos} (the file has {n})`, `unknown language code "{x}"`,
`"{k}" is not a subtitle kind` / `is not an audio kind`, `duplicate kind "{k}"`, `name is longer
than 64 bytes`, `name has control characters`, `unknown field "{x}" in set`, `removed takes no
other field`, `duplicate track`, `use pos to correct a detected track`, `an added track needs a
language, a kind or a name`, `too many tracks` (32 audio / 64 subtitle tracks per file with the
added ones), `too many corrections for this torrent` (256), `device must be 1 to 32 printable
bytes`. 404 `not_found`: `unknown infohash`, `no probe for this torrent`, an unknown `id`
(`no correction {id} on this torrent`). 401 without the token. A body of the wrong shape: 422
`invalid_body`.

What the detail adds (every field defaulted, absent ones left out):

```json
"probe": {
  "version": 2, "probed_at": 1793500000, "status": "ok", "verified": true,
  "files": [{
    "index": 0, "dht_index": 0, "path": "Movie.mkv", "size": 6000000000, "label": "video.mkv",
    "description": "Matroska video", "confidence": 97, "verified": true,
    "video": {"container": "matroska",
      "audio": [
        {"codec":"eac3","channels":6,"lang":"fre","atmos":true,"pos":0,
         "fix":{"id":17,"fields":[],"at":1793500100,"by":"android"}},
        {"codec":"ac3","channels":2,"lang":"fre","kinds":["commentary"],"name":"Commentaire","pos":1,
         "fix":{"id":18,"fields":["lang","name"],"detected":{"lang":"eng","kinds":["commentary"],"name":"Commentary"},
                "at":1793500200,"by":"android"}},
        {"codec":"aac","channels":2,"lang":"eng","pos":2,
         "fix":{"id":19,"fields":[],"removed":true,"detected":{"lang":"eng"},"at":1793500300,"by":"android"}}],
      "subs": [
        {"codec":"subrip","lang":"fre","kinds":["forced"],"pos":0},
        {"codec":"","lang":"spa","kinds":["forced"],"name":"Español (forzados)",
         "fix":{"id":42,"fields":["lang","kinds","name"],"added":true,"review":true,"at":1793500400,"by":"android"}}]}
  }],
  "unplaced": [
    {"id":9,"file":0,"path":"Movie.mkv","track":"sub","pos":3,"reason":"changed",
     "was":{"codec":"hdmv_pgs_subtitle","lang":"eng","kinds":["sdh"],"name":"English SDH"},
     "fields":["kinds"],"kinds":["cc"],"at":1793400000}]
}
```

- `dht_index` (always sent): the file's index in the DHT row's list, what `file` names.
- Track values are **effective**: the detection with the correction's fields replaced. `pos`
  (always set on a detected track) is its 0-based position; the app shows "Track {pos+1}" as
  VLC numbers them. An added track has no `pos` and an empty `codec`.
- `fix`: `id` (for DELETE), `fields` (overridden, order `lang`, `kinds`, `name`; always sent,
  empty for a check or a removal), `removed`, `added`, `review` (the probe changed since the
  user checked: on an anchored track its language, kinds or name differ from the snapshot; on
  an added track the probe lists another number of tracks of that type), `detected` (the
  current detection of the track: on anchored tracks with `fields` or `removed`, on checks only
  with `review`), `at` (last edit), `by` (`device`).
- `unplaced`: corrections that anchor to no current track (`reason` `changed`: the probe now
  lists other tracks and none matches what the user corrected; `file`: the file is no longer
  probed). Kept, never applied to another track; `was` is the detection they were made on (none
  for an added track); DELETE removes one.
- **Older clients** (no `probe_corrections`) see the effective values, and also the removed
  tracks: they stay in the arrays, marked only by `fix.removed`, which such a client ignores.
  An older server has no routes (404): hide the edit actions without the feature.

`StatsResponse.probe_corrections` (always present on a server with the feature, zero counts
included; definitions in docs/PROBES.md §12):

```json
"probe_corrections": {
  "tracks": 140, "checked": 92, "corrected": 41, "removed": 7, "added": 4,
  "languages": {"checked": 120, "wrong": 6, "filled": 9},
  "names_changed": 3,
  "kinds": [{"track":"sub","kind":"sdh","source":"name","checked":31,"wrong":4},
            {"track":"sub","kind":"forced","source":"flags","checked":12,"wrong":1}],
  "missed": [{"track":"sub","kind":"forced","count":3}],
  "at": 1793500000
}
```

## Web sessions (D133)

With `MAGNETAR_WEB_UI` on (the default), the web UI (docs/WEB.md) signs in once with the API
token and then uses an HttpOnly session cookie instead of the bearer token. The bearer API is
unchanged: the app and the workers keep sending `Authorization: Bearer`.

| Route | Auth | Answer |
|---|---|---|
| `POST /api/v1/web/login` `{"token": "…"}` | none | 200 `{"csrf", "expires_at", "created_at", "ntfy"}` + `Set-Cookie`; 401 `unauthorized` ("token rejected"); 415 when not `application/json`; 413 over 1 KiB; 403 `forbidden` when `Sec-Fetch-Site` is present and not `same-origin`/`none`; 429 `rate_limited` + `Retry-After` after 5 failed sign-ins in a rolling minute (server-wide, the right token included) |
| `GET /api/v1/web/session` | cookie | 200 `{"csrf", "expires_at", "created_at", "ntfy"}` or 401 |
| `POST /api/v1/web/logout[?all=1]` | cookie + CSRF | 204 + a clearing `Set-Cookie` (`Max-Age=0`); `all=1` ends every web session |

- Cookie: `__Host-magnetar_session=<43 base64url chars>; Path=/; HttpOnly; SameSite=Strict;
  Secure; Max-Age=2592000` (30 days), always, whatever `Host` a proxy forwards (on plain http
  to `127.0.0.1` browsers treat loopback as secure and keep it). Only that name is read. The
  server stores only sha256 of the cookie's 32 random bytes (`web_sessions`); a session ends at
  expiry, on sign-out, or when `MAGNETAR_API_TOKEN` changes.
- `ntfy`: whether a topic is configured (the `ntfy.topic` setting or `MAGNETAR_NTFY_TOPIC`), so
  the UI can say whether saved-search alerts go out. The web UI uses no browser push.
- The cookie is accepted only on `/api/v1/…` routes, never on `/relay`, `/api/v1/worker/*`,
  `/api/v1/events/batch` or `/api/v1/downloads*` (those need the bearer token), and only when
  no `Authorization` header is sent (a wrong bearer token is not rescued by a cookie).
- CSRF: with the cookie, every method other than GET, HEAD and OPTIONS needs
  `X-CSRF-Token: <csrf>` and a `Sec-Fetch-Site` that is absent, `same-origin` or `none`;
  otherwise 403 `{"error": "forbidden", "message": "missing or bad CSRF token"}`.
- No CORS headers anywhere: other origins cannot read answers.

### Response headers (D134)

Every API and `/health` answer carries `X-Content-Type-Options: nosniff`,
`Referrer-Policy: no-referrer`, and `Cache-Control: no-store` unless the handler set its own
(posters keep `private, max-age=…`). Web UI responses carry the full set of docs/WEB.md
(CSP with Trusted Types and the one `magnetar` policy, `X-Frame-Options: DENY`, COOP/CORP, `Permissions-Policy`,
`X-Robots-Tag: noindex`). Bodies and status codes are unchanged.

## Not in the API (phone-local by design)

- Pinned / hidden torrents: client state.
- The phone's own Nostr crawl, sync and SQLite cache (ffi only): a thin client has no offline
  search; it can cache pages itself.
- Downloads on the phone: a thin client hands `TorrentDetail.magnet` to a torrent app (or uses
  the server downloads above).
