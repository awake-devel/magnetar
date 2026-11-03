# Header probes

ARCHITECTURE.md D117. Roadmap item C of "Header probes" (docs/ROADMAP.md); the detector is
`crates/sniff` (D116, crates/sniff/README.md).

A crawl worker that just fetched a torrent's info dict already holds a connection to a peer
that has the torrent. A header probe keeps that connection, downloads only the few 16 KiB
blocks `sniff` needs, and learns from the bytes what the main files really are: the file type
by content (never by extension), console game ids (platform, title id, content id, title,
version, region) and video / audio quality (container, codec, resolution, frame rate, bit
depth, HDR10 / HDR10+ / HLG / Dolby Vision, audio codecs, channels, languages, Atmos, DTS:X,
subtitle languages). The result goes to the server with the dict, which checks and stores it
and shows it in the detail API (`TorrentDetail.probe`, docs/API.md).

**Probes download parts of files.** Read §7 (legal note) before turning them on.

## 1. Where they run, and where they never run

- **Crawl workers only** (`magnetar-server worker`, docs/WORKERS.md), with `MAGNETAR_PROBE=true`
  and the DHT crawler on (`MAGNETAR_DHT`). Off by default everywhere.
- **Never the main server.** `serve`, `dht` and every other command never build a prober; with
  `MAGNETAR_PROBE=true` they log a warning and ignore it. `compose.yml` pins `MAGNETAR_PROBE`
  and `MAGNETAR_PROBE_UPLOAD` to `false`. Test: `probes::tests::only_the_worker_crawler_builds_a_prober`.
- **Never on a torrent the worker's blocklist stops, or with no blocklist loaded** (the
  worker's own classifier and screen run first, docs/WORKERS.md §1; the classifier's adult
  result still turns on the blocklist's context tier, D53/D68). The server checks again with
  its own blocklist before storing anything.
- **Adult and BEP 27 private torrents are probed** like any other (D119, user decisions of
  2026-11-03): category `porn` or the adult signal is no gate, on the worker, on the server or
  in the backfill, and neither is `private = 1` in the info dict. For a private torrent the
  peers come from the DHT, against what the BEP 27 flag asks (the user's choice), and with
  `MAGNETAR_PROBE_UPLOAD=true` the worker also serves verified pieces to peers of that private
  swarm. The DHT row keeps no private flag (`dht_torrents` has no column for it), so the
  detail does not say a torrent is private.
- Not while a brake is on (router health probe, answered-share brake, pressure brake) or the
  governor reports memory / conntrack pressure; not on dicts over 2 MiB; not without a free
  slot (`MAGNETAR_PROBE_PARALLEL`) and a turn of the per-minute pace (`MAGNETAR_PROBE_PER_MIN`,
  no burst). A torrent that misses its chance is queued for the backfill (§10) at the
  server's next fill pass, when a worker runs it.
  Every skip is counted by reason in the heartbeat (`skipped`).
- At worker start with probes on, a warning says that probes connect to peers from this
  machine's address and download file data. There is no VPN detection.

## 2. What a probe does

1. **Hand-off.** With probes on, the worker's metadata fetcher sets the BEP 6 Fast bit, sends
   `HAVE_NONE` first when the peer also set it (nothing otherwise, never a bitfield), sends
   `INTERESTED` with its `ut_metadata` requests and records the peer's availability messages
   (docs/DHT.md §4). The connection that delivered the verified dict is handed to the probe;
   it still counts under `MAGNETAR_DHT_TCP_MAX`, and every further connection goes through the
   same fetcher: same address policy (D55, D112), attempt pace and brake, and the honest
   identity of D115 (`PEER_ID_PREFIX`, `v` = the client name). We never impersonate another
   client.
2. **Plan.** From the dict's piece layout and file list, at most 8 targets: the largest file
   always, whatever its extension; packs grouped by directory template and extension (largest
   and median member for video, one per directory for audio and images); junk, small samples,
   RAR volumes after the first and padding skipped; Blu-ray (`.mpls`, `.clpi`, `index.bdmv`
   whole plus the M2TS head) and DVD (`.IFO` whole) layouts count as one target. First read:
   64 KiB for video, 16 KiB for anything else; `sniff` asks for the rest (`NeedMore`: PS5 CNT
   block, MP4 `moov`, ZIP central directory, …) within its caps (10 rounds, 16 ranges, 4 MiB per
   file; a video payload, the largest file, fetched in whole pieces may use the torrent's budget
   less the first reads of the other targets, D120) and the
   torrent's `MAGNETAR_PROBE_MAX_BYTES`. An MP4 / MOV whose `moov` follows the `mdat` (39 % of
   MP4 probes had audio languages in production, 2026-11-03) is found from the `mdat` size in
   the head (32 or 64-bit), one 16 KiB block at the computed offset; when the box chain cannot
   be followed (a wrong `mdat` size) sniff searches the last 1 MiB for it. The `moov` is walked
   by its box headers, never fetched whole: first every track's header, `mdhd` (language),
   `hdlr` and `stsd` (codec), then the hops over the sample tables (frame rate, first sample),
   which audio tracks whose first frame adds nothing (AAC, Opus, FLAC, …) skip (§3 Caps).
3. **Download** (§3), then `sniff` on each target (`Detector::analyze` / `finish`, off the async
   threads), until it needs nothing more, a cap is reached or the 45 s session ends.
4. **Second peer.** With `MAGNETAR_PROBE_PEERS` ≥ 2, a second connection (peers that took our
   handshake for this hash first, then the DHT's) is opened when the first peer stays choked,
   lacks a needed piece or is gone, and for the agreement read: the main file's first read
   from both peers, both detections compared (`agreed` / `disagree`).
5. **Report.** Only detections of confidence 80+ (the sniff contract); file paths never travel
   (display indexes into the dict the server stores). The dict waits for its probe, never
   longer than the session cap; a stop sends it without one.

## 3. Download and verification policy (user decision)

- **Blocks.** Only the 16 KiB blocks sniff asks for are requested. Bytes that were not
  hash-checked enter `sniff` as `SparseFile::insert_unverified`, so a result that read them is
  `verified = false`.
- **Whole pieces** when the piece length is ≤ `MAGNETAR_PROBE_WHOLE_PIECE_MAX` (default 1 MiB):
  each needed block is widened to its piece, the piece is fetched from one peer, SHA-1 checked
  against the dict as it streams in, and its bytes enter sniff as verified. A mismatch discards
  the bytes, strikes the peer and voids every unverified result that used its blocks.
- **Surprise check.** When a result is surprising (`suspicious_executable`,
  `extension_mismatch`, or two peers disagreeing on the label or by more than 15 confidence
  points), the probe fetches the whole pieces under the ranges the verdict read (within what is
  left of the budget), checks them and runs sniff again on verified bytes. Same verdict:
  `surprise_verified`; a different verified verdict wins (`surprise_changed`); no room:
  `surprise_unverified` and the result stays unverified.
- **Caps.** Per torrent `MAGNETAR_PROBE_MAX_BYTES` (8 MiB; ~1–2 % of torrents end
  `incomplete`), per file sniff's caps (4 MiB; a video payload in whole pieces up to the torrent's budget less
  the other targets' first-read pieces, so a long multi-track MP4 cannot starve them; the
  surprise re-read up to the pieces its verdict read, D120),
  per peer 64 requests in flight (capped by its BEP 10 `reqq`), 15 s unchoke wait, 10 s stall,
  45 s per probe, `MAGNETAR_PROBE_PARALLEL` probes at once (memory ≤ parallel × max bytes).
  With whole pieces a follow-up is charged the pieces it costs, not the bytes sniff asked for
  (a 16-byte box header behind a 1 MiB piece costs 1 MiB), and one that does not fit what is
  left of the file's cap is skipped; the first read always goes. Measured on the loopback fake
  seeder (D120): a 2 h MP4 with three audio tracks and its `moov` at the end (11.2 MB of
  tables) costs 6.7 MiB at 1 MiB pieces (all three languages; before: 4 MiB, one language),
  4.2 MiB at 512 KiB (before: 5 MiB), 0.18 MiB with blocks (before: 0.23 MiB); single-audio
  files with the `moov` at the end cost what they did (1.7–2.3 MiB at 1 MiB pieces,
  0.08–0.16 MiB with blocks; iPhone, Jellyfin and remux samples).
- **Peer protocol.** Outgoing messages are a closed type (`probe::msg::ProbeMsg`): keep-alive,
  `interested`, `not_interested`, `request`, `cancel`, `have_none`, `reject_request`, the
  extended handshake and the `ut_metadata` reject. It cannot express `unchoke`, `have`,
  `bitfield`, `have_all` or `piece`. A peer's `request` is rejected (Fast) or ignored, its
  `ut_metadata` request rejected. A block we did not ask for, a wrong size, a bitfield with
  spare bits or a `have` past the last piece drops the peer. Without the Fast extension a choke
  re-queues every outstanding request.

## 4. Upload mode (`MAGNETAR_PROBE_UPLOAD`, off by default)

**Turning it on is sharing.** With it on, a probe:

- sends `HAVE` for each whole, SHA-1-verified piece it holds, to its own peers only;
- unchokes a peer that is interested and lacks a held piece, while the probe's caps (4 MiB
  served, 256 KiB/s) are not reached, and chokes everyone at the cap;
- serves `piece` only inside held pieces; any other request is rejected (Fast) or ignored;
- never sends a bitfield or `have_all`, never listens for inbound connections, never fetches
  extra pieces to have something to share. On torrents with pieces above
  `MAGNETAR_PROBE_WHOLE_PIECE_MAX` it holds nothing, so upload changes nothing there.

Bytes fetched as unverified blocks can never be served: a `piece` message can only be built
from a held piece, and a held piece only from a piece whose hash matched (types in
`probe::serve` and `probe::verify`). Off: no piece data ever leaves, `HAVE_NONE` (or nothing),
never an unchoke. Tests: `probe::upload_off_never_shares_with_a_hostile_peer` (12 runs and a
negative control), `probe::upload_mode_shares_only_verified_pieces_within_caps`.

**Nothing on disk.** Pieces live in memory for the probe's life only; held buffers are zeroed on
drop. The worker process turns core dumps off (`PR_SET_DUMPABLE` 0; `LimitCORE=0` in
`packaging/magnetar-worker.service`, `ulimits: core: 0` in `docker/compose.worker.yml`). That
is "not dumped and not stored on purpose", not "never anywhere": socket and reader buffers hold
copies for a while, and swap is the machine's choice (check1 #1).

## 5. Settings

Worker only (docs/CONFIG.md "Header probes"):

| Variable | Default | EEE | RPI |
|---|---|---|---|
| `MAGNETAR_PROBE` | `false` | `false` | `false` |
| `MAGNETAR_PROBE_UPLOAD` | `false` | `false` | `false` |
| `MAGNETAR_PROBE_WHOLE_PIECE_MAX` | `1048576` | same | same |
| `MAGNETAR_PROBE_PER_MIN` | `30` | `120` | `12` |
| `MAGNETAR_PROBE_PARALLEL` | `8` | `32` | `4` |
| `MAGNETAR_PROBE_PEERS` | `2` | same | same |
| `MAGNETAR_PROBE_MAX_BYTES` | `8388608` | same | same |

Why these numbers (planner cost simulation on real torrents and the check reports; all rates
**UNVERIFIED** on real swarms until the eee trial):

- 1 MiB whole-piece line: 55 % of sampled torrents get verified results; 1.23 MiB per torrent on
  average (policy D′), p99 10.7 MiB; blocks only would be 0.44 MiB.
- 8 MiB per torrent: blocks p99 3.1 MiB, so it only cuts whole-piece outliers (~1–2 %).
- Pi 12/min: its router knee is ~3 000 packets/s out (4 500 total) and the crawl already runs
  at ~⅔ of it; 0.2 probe/s × 1.23 MiB is ~260 packets/s. EEE 120/min ≈ 2.5 MB/s in.
- Parallel: memory ≤ parallel × max bytes (Pi 32 MiB, under its 200 MB RSS cap).
- 2 peers: the metadata peer and one for the agreement read or a choke fallback.

Constants (code, `probe::ProbeConfig::default`): 15 s unchoke wait, 10 s stall, 45 s session,
64 requests in flight, second peer after 3 s, 4 further connection attempts, 8 targets, minimum
confidence 80, agreement slack 15, upload 4 MiB / 256 KiB/s and a 5 s linger, dicts ≤ 2 MiB,
sniff caps per file of 10 rounds (6 before D120: an MP4 asks its track skeleton before its
sample tables, a 2 h film with four tracks takes 9), 16 ranges and 4 MiB (a video payload in whole
pieces: the torrent's budget less the other targets' first reads).

## 6. What is stored and shown

The worker sends `DhtResult.probe` (`ProbeReport`: version, status `ok` / `partial`, bytes in
and served, peers used, files: index, size, label, confidence, verified, flags, game, video).
`PROBE_VERSION` is **2** since D123: each audio and subtitle track may carry `kinds` (values of
`PROBE_AUDIO_KINDS` / `PROBE_SUB_KINDS`, alphabetical, no duplicates, left out when empty) and
`name` (the track name, cleaned, at most 64 bytes, left out when absent; stored so the server
can re-classify later, not shown by the app). Version 1 rows have neither. When names would
push a report over 64 KiB the worker drops every name first (the kinds stay). §11 has the
taxonomy and how sniff finds the kinds.

```json
"audio": [{"codec":"eac3","channels":6,"lang":"fre"},
          {"codec":"ac3","channels":2,"lang":"eng","kinds":["commentary"],"name":"Director's Commentary"}],
"subs":  [{"codec":"hdmv_pgs_subtitle","lang":"eng","kinds":["sdh"],"name":"English (SDH)"},
          {"codec":"subrip","lang":"fre","kinds":["forced"],"name":"Français (forcés)"},
          {"codec":"eia_608","kinds":["cc"]}]
```

The server (`server::probes`):

- takes it only on a `metadata` item for a hash the worker holds (D113), only once that dict
  verified, and checks every field: version, at most 16 files, each index inside **its own**
  file list with the same size, the label in sniff's registry, confidence 80–100, flags from a
  whitelist, platform names, game ids (`db_id` by `sniff::db_id_valid`), strings ≤ 128 B
  (title 256) without control characters, ≤ 8 video / 32 audio / 64 subtitle tracks, ≤ 64 KiB.
  Languages arrive as ISO 639-2/B lowercase (the worker maps `en`, `en-US`, `fra` → `eng`,
  `eng`, `fre`);
- refuses numbers outside their bounds (width / height 1–65 535, fps × 1000 ≤ 1 000 000, bit
  depth 1–32, channels 1–64, duration ≤ 30 days, bitrate ≤ 10 Gbit/s) and a block that does not
  fit its label (`video` only on `video.*` / `audio.*` labels; `game` only on `game`, `gamedata`,
  `package`, `disc`, `archive`, `exe` and `misc` labels); the worker drops the same values
  before sending;
- writes it only when the `dht_torrents` row exists and the hash is not blocked (adult rows
  included, D119), one savepoint per row (D110); a re-probe replaces it only when it is of a
  newer version or verifies at least as many files;
- answers `probe_stored` / `probe_rejected` in `DhtAck`. A refused probe never costs its dict.

Tables (migration `20261105000000_probes.sql`):

- `probe_attrs` (one row per torrent, references `dht_torrents`, cascade): version, time,
  `supplied_by` (the worker), status, files probed / verified, bytes, and a summary of the main
  payload (largest probed file): kind, platform, title id, container, video codec, height, HDR,
  bit depth, duration, audio codecs and languages, subtitle languages, flags, and `tags`
  (`k:video`, `r:2160p`, `v:hevc`, `h:dolby_vision`, `a:fre`, `s:eng`, `ac:eac3`, `p:ps5`,
  `x:atmos`; since D124 also `ak:{kind}` / `sk:{kind}` per audio / subtitle kind, the
  same-track pairs `ap:{lang}:{kind}` / `sp:{lang}:{kind}`, and the subtitle format class
  `sf:text` / `sf:image` / `sf:cc`). On version 2 rows `a:{lang}` leaves out audio tracks
  whose kinds include `commentary`, `music` or `karaoke`, and `s:{lang}` leaves out subtitle
  tracks whose kinds include `forced`, `signs`, `commentary` or `description`, so "French
  subtitles" means a French track to read the film with. The `audio_langs` / `sub_langs`
  columns keep every language. About 1 GB per million probes (storage report, S4).
- `probe_files`: the per-file JSON (`[ProbeFile]`).

No GIN index on `tags`: with `ORDER BY probed_at LIMIT` a tags GIN collapses to a full scan on
rare mixes (check1 #3). Since D122 the tags are search-index facets (`probe_language`,
`probe_subtitles`, `probe_resolution`, `probe_codec`, `probe_hdr`, `probe_audio`; filters in
docs/FILTERS.md "Probe filters"), and `probe_attrs` writes re-index their torrent through the
change log (migration `20261108220000_probe_search.sql`, which also re-classed `r:` from the
width: a 3840×1606 film is `r:2160p`). Detail API: `TorrentDetail.probe` with each file's path from the server's own
list and the label's description (docs/API.md), adult torrents included (D119). File indexes point into the `files` the detail returns: on a torrent with a Nostr
event that is the event's list, so each probed file is matched to it by path (with or without
the torrent's root folder) and size; a file not in that list is left out. After a worker token leak: `DELETE FROM probe_attrs WHERE supplied_by = '<name>'`
(docs/WORKERS.md §7). The server cannot re-check a probe's bytes, the same gap as a dict (D113).

## 7. Legal note

Until header probes, Magnetar only handled metadata: info dicts (names, sizes, hashes), never
file data. Probes download pieces of the files themselves, so the legal position is blurry.

- In France what is enforced is *sharing*: Arcom's graduated response (formerly HADOPI) acts on
  uploads seen in P2P swarms.
- Downloading a fragment to learn what a torrent contains is not the targeted act, but it is
  not explicitly lawful either: copying from an unlawful source is not covered by the
  private-copy exception (CJEU C-435/12 *ACI Adam*, 2014). This document does not say that
  probing is legal.
- By default a probe never shares: no piece data sent, never an unchoke, `HAVE_NONE` (or
  nothing) instead of a bitfield of real pieces, never announced as a seeder, only the bytes
  needed for the check, nothing kept after it. Protocol traffic (handshakes, requests) is not
  sharing.
- **Turning `MAGNETAR_PROBE_UPLOAD` on *is* sharing** (§4).
- Other countries differ (Germany, for example, fines downloads too).

**UNVERIFIED legal reading, not legal advice.**

## 8. Production (eee, rpi)

The user's decision (2026-11-02): "for the opt-in, off by default yes, but in our prod, put it
on". Upstream defaults stay off (`MAGNETAR_PROBE=false`, `MAGNETAR_PROBE_UPLOAD=false`);
production, the user's crawl workers eee and rpi, runs with both on.

**Status: not switched on yet.** The switch lives in the workers' private env files, which are
not in the repository (`docker/*.env` is gitignored; the Pi's file is on the Pi). No agent
turned it on: it starts peer traffic on the workers' lines, and the agents that built this
change were not allowed any live network traffic (CLAUDE.md "Network safety": peer traffic only
under the router watchdog, stop at the first fault). The order below (eee first, upload off then
on, under the watchdog, then rpi) is the agents' recommendation for switching it on, not a
condition the user set.

eee, `docker/worker.env` (then `sudo docker compose -f docker/compose.worker.yml --env-file
docker/worker.env up -d --build`):

```sh
MAGNETAR_PROBE=true
MAGNETAR_PROBE_UPLOAD=true
MAGNETAR_PROBE_PER_MIN=120
MAGNETAR_PROBE_PARALLEL=32
```

rpi, `/etc/magnetar/worker.env` (then `sudo systemctl restart magnetar-worker`):

```sh
MAGNETAR_PROBE=true
MAGNETAR_PROBE_UPLOAD=true
MAGNETAR_PROBE_PER_MIN=12
MAGNETAR_PROBE_PARALLEL=4
```

For the backfill of past finds with no new crawling (user, 2026-11-02: "disable dht and nostr
crawl tho, so it can read every past finds"; §10), add to both workers' files

```sh
MAGNETAR_PROBE_BACKFILL=true
MAGNETAR_DHT_DISCOVER=false
```

and to the main server's `.env` `MAGNETAR_NOSTR_CRAWL=false` (then rebuild and restart it from
the checkout: `sudo docker compose build && sudo docker compose up -d`; the published image may
lag `main`). Not switched on either, for the same reasons; the server side needs no other
setting.

**Deploy the server before the workers** whenever `PROBE_VERSION` moves (2 since D123): a
server older than its workers refuses their reports (`probe_rejected`, reason "version")
instead of storing them without the new fields. The reverse is safe: a newer server takes
version 1 reports as before.

What to watch: `workers[].dht.probe` in `GET /api/v1/stats` (started, ok / partial / choked /
no_pieces / failed, skipped by reason, bytes in, uploaded, verified files, surprises),
`probe_backfill` and `workers[].dht.backfill` (§10), and the router health brakes.

## 9. Not done / UNVERIFIED

- **Not run on real swarms.** Every test uses loopback fake peers (`fake_seeder`). Unverified:
  how often real clients unchoke a peer that has nothing (`HAVE_NONE`), anti-leech bans,
  whether clients accept `HAVE_NONE` before the extended handshake, whether the Fast bit changes
  metadata success, real bytes and packets per probe on eee and the Pi, Pi SHA-1 speed.
- Search filters and filter-sheet chips on probe results are built (D122); the Android detail
  shows the probe as a "Detected" card (D121, docs/APP.md).
- v2-only piece checks (BEP 52 Merkle proofs) are not built: hybrid torrents are checked with
  their v1 piece hashes.
- uTP peers are not reached (TCP only, as the metadata fetcher).
- **Where languages are absent (D120).** AVI has none: `strh.wLanguage` is 0 in what we hold
  and FFmpeg ignores it; `strn` is a free-form name; the `INFO` list (`ILNG`, `IAS1`…) is
  rarely written. MPEG-TS carries them in the PMT (ISO 639 descriptor 0x0A, read; a PMT over
  several packets is now reassembled), but Blu-ray M2TS has none there (the demo M2TS files
  held here: none): a disc's languages are in the `.clpi` / `.mpls` stream tables, which are
  fetched for BDMV layouts but not parsed yet. MPEG-PS (`.mpg`, `.vob`), FLV, ASF / WMV and
  OGV have no recon parser, so their probe has no container or tracks (the "unknown" row);
  DVD languages are in the `.IFO`, not parsed either. An MP4 track tagged `und` (FFmpeg's
  default when the source has no tag) has no language by design. How the 61 % of MP4 probes
  without audio languages split between `und` tracks, whole-piece budget cuts (fixed by D120)
  and peers that lack the tail pieces is **UNVERIFIED** (production rows not re-read here).
- **Track kinds (D123), UNVERIFIED:** every non-English and non-French term of the name
  classifier; how often real releases set the Matroska v4 flags (0x55AB–0x55AF) or a non-zero
  AC-3 / E-AC-3 bsmod; how often captions are signalled in a first access unit; the hit rate
  on real swarms; how often a muxer sets FlagForced by mistake on an unnamed track (it then
  reads forced and leaves its language's subtitle filter); whether caption services silent in
  the first access unit are missed now that null padding no longer counts; the negation list
  beyond English, French and German. Checked here only on hand-built containers and on local ffmpeg 9.0.2 output
  (Matroska dispositions → v4 flags; MP4 `udta/name` and `kind` boxes; AC-3 / E-AC-3
  `-audio_service_type` in TS frames, MP4 `dac3` / `dec3` and Matroska E-AC-3 frames). The
  CEA-608 / 708 readers have only synthetic vectors. Version 1 rows have no kinds and cannot
  get any without a re-probe (names were never stored).

## 10. Backfill (D118)

Probes only follow fresh metadata fetches, so torrents indexed before them (~1.35 M DHT rows)
and Nostr-only torrents (~470 k, never fetched from the DHT) had none. The backfill probes
them. The server stores no info dict for them that a probe could use (the DHT rows keep only
the display file list), so the worker fetches the dict again from the swarm; the fetcher
verifies it against the infohash, so it cannot be made up.

**Setting** (worker): `MAGNETAR_PROBE_BACKFILL=true`, with `MAGNETAR_PROBE=true` and
`MAGNETAR_DHT`. To read only past finds, turn discovery off on the worker
(`MAGNETAR_DHT_DISCOVER=false`: no BEP 51 sampler, nothing it hears is triaged; the node only
looks peers up) and, if wanted, the Nostr crawl on the server (`MAGNETAR_NOSTR_CRAWL=false`:
no relay is contacted; phone sync, seed checks, enrichment, notifications and the worker
endpoints keep running). `polite` is the quieter DHT mode then: a read-only node that answers
nobody.

**Queue** (server, table `probe_backfill`, migration `20261106000000`). Filled in the background
by a keyset walk over `torrent_infohashes` (5 000 per step, ~180 ms each on 1.8 M seeded rows,
about a minute a pass), started by the first `probe/lease` and again at most every 6 h (to
queue torrents indexed since). Skipped at fill time: torrents with a probe, blocked hashes,
and Nostr-only torrents that no default search shows (every event
from a blocked publisher, or below the trust threshold and not from a trusted publisher): a
DHT row would bring them back as DHT torrents, since a DHT row passes every trust threshold
(D51) and a publisher block only hides that publisher's events. They are queued by a later
pass once shown (block lifted, threshold lowered in Settings). Checked again at lease time: a
probe stored meanwhile deletes the row, and so does a torrent no longer shown; blocked or gone
closes it. Adult torrents (category `porn` or the adult signal) are queued and leased like any
other since D119; migration `20261107000000` reopened the rows earlier closed as `adult` or
`private` (due again in their never-tried place), and the first fill pass after an upgrade
queues the `porn` torrents earlier fills skipped. A lease whose rows were all
closed or dropped reads the next due rows (up to 8 rounds), so a run of them does not send the
worker idle. Order (value first, fixed when queued): known seeders (or DHT peers) in power-of-two tiers,
most first; then torrents never seed-checked; known dead last; newest (`born`) first within a
tier. One ordering key (`due_at`, as the seed-check schedule, D45): negative for never-tried
rows, the lease end while leased, the retry time after an attempt, NULL when closed. The lease
reads the partial index `probe_backfill (due_at)` in order with `FOR UPDATE SKIP LOCKED` (0.35
ms for 256 rows on 1.8 M rows, then primary-key lookups in `probe_attrs`, `blocked_infohashes`,
`dht_torrents`, `torrents`), so two workers never share a hash. Lease length: the fleet's
`MAGNETAR_WORKER_LEASE_SECS`; an unanswered lease is due again at its end; `probe/release`
gives unworked ones back (never-tried rows in their first place).

**Worker.** A feeder leases while fewer than half of 2 × `MAGNETAR_PROBE_PARALLEL` leased
torrents are in the worker (none under a brake or memory / conntrack pressure), and queues
them for peer collection like triaged hashes: the crawler's DHT lookup (`collect_peers`, the
shared D109 pace), the metadata fetcher (TCP pace, brake, address policy D112, identity D115),
the worker's blocklist, then the probe gates (no adult or private check, D119). A backfill dict waits up to 30 s
for a probe slot and turn (`Prober::slot`, paced like the others, no burst) instead of being
skipped. Results go up on `dht/results` (`via: get_peers`); hashes still in the worker at a
stop are released.

**Server.** On `dht/results`, a hash leased to that worker (and not answered yet) counts as
held, as a triaged one does (D113). Its verified dict goes through the server crawler's writer
(classify, screen, store) with the "known from Nostr" check off: a Nostr-only torrent gets its
DHT row (`origin: both`, `supplied_by` = the worker), an existing DHT row is `not_new` and
never rewritten (`ON CONFLICT DO NOTHING`). The new row's `first_seen` is the oldest Nostr
event's `created_at` (not the upload time), so `since`, date sorts and the saved-search poll
do not see an old torrent as new. It is not a discovery: it is counted in the ack's
`backfill_stored` and the `probe_backfill_dht_rows` series, not in `stored`, the worker's
`stored_total`, `dht_torrents_added` or `dht_stored_<via>` (the writer marks its transaction,
`magnetar.origin = 'backfill'`, for the insert triggers). A dict for a torrent hidden since its
lease (publisher blocked meanwhile) is not stored and its row leaves the queue. The probe is checked and stored as any other (§6),
against the server's own file list; the detail maps its indexes into the list it shows (the
Nostr list on `origin: both`). Then the lease is consumed (`worker` cleared, so a second answer
is ignored):

| Answer | Queue row |
|---|---|
| dict + probe stored | deleted |
| dict without a stored probe (skipped, choked, nothing found, refused) | retried (`no_probe`) |
| `failed` (`no_peers`, `connect`, `timeout`, …), or `oversize` from a worker whose `MAGNETAR_DHT_METADATA_MAX` is below the server's | retried with that code (a worker with room gets it later) |
| `v2_only`, `invalid`, `oversize` (worker limit ≥ the server's), worker `blocked` | closed with that outcome |
| `private`, sent only by a worker older than D119 (an updated one uploads the private dict) | retried (`private`), so an updated worker probes it |
| dict for a torrent no default search shows any more | deleted (not stored) |

Retries come after 6 h, 3 days and 14 days; the 4th unsuccessful attempt closes the row. A
worker's block report closes the lease and never blocks the hash (the server never saw the
dict); a dict the server's own screen blocks is blocked as usual. Backfill statuses never
touch `dht_seen`. To try closed rows again:
`UPDATE probe_backfill SET due_at = rank, attempts = 0 WHERE due_at IS NULL AND last_outcome = 'no_peers'`.

**Progress.** `GET /api/v1/stats` → `probe_backfill {waiting, leased, retrying, remaining,
closed [[outcome, n]], probed, filling, filled_at, at}` (one scan of the narrow queue table,
~230 ms on 1.8 M rows, recomputed at most once a minute), `workers[].backfill_*_total`, and the
worker's heartbeat `workers[].dht.backfill {leased, released, outstanding, no_peers, failed,
fetched, probed, unprobed, lease_errors}` and `workers[].dht.discover`.

**UNVERIFIED** on real swarms: how many past finds still have reachable peers (old DHT finds
and Nostr releases may have none: those cost one DHT lookup each, then the backoff), the
probes per hour eee and the Pi reach, and their line load. The traffic per torrent is the shape
of a new find (one lookup, metadata TCP attempts, one probe), paced by the same settings; the
D109 router rules apply unchanged (CLAUDE.md "Network safety": not run here without the
watchdog). Tests: loopback fake seeders on a fake DHT only (`probe_backfill_pg`).

## 11. Track kinds (D123)

What a subtitle or audio track is for, read from bytes a probe already fetches: no new peer
reads. On the 78 sample videos under `~/sniff-samples/video` and the 53 files of the local
recon corpora, the bytes fetched and the rounds are identical before and after D123.

### Taxonomy (closed sets)

| subtitle kind | meaning | container / bitstream sources | name |
|---|---|---|---|
| `bilingual` | two languages in one track | — | 双语, dual subs, … |
| `cc` | closed captions (EIA-608 / 708) | MP4 `c608` / `c708`; CEA-608 / 708 in the first video access unit; ATSC caption_service_descriptor | CC, closed captions |
| `commentary` | subtitles of a commentary | Matroska FlagCommentary; DASH role `commentary` | commentary, Kommentar, … |
| `description` | text descriptions | Matroska FlagTextDescriptions, FlagVisualImpaired; `D_WEBVTT/DESCRIPTIONS`; DASH `description`; Apple `describes-video`; html-kind `descriptions` | text description(s) |
| `easy_reader` | easy reader | Apple `public.easy-to-read`; DASH `easyreader` | easy reader, … |
| `forced` | forced / foreign parts only | Matroska FlagForced (resolved, below); DASH `forced-subtitle`; `tx3g` displayFlags 0x80000000; `tref/forc` target | forced, forcés, … |
| `honorifics` | honorifics track | language `enm` | honorifics |
| `sdh` | SDH / hearing impaired | Matroska FlagHearingImpaired; `D_WEBVTT/CAPTIONS`; DASH `caption`; html-kind `captions`; Apple `describes-music-and-sound`; DVB subtitling_type 0x20–0x25; teletext type 5 | SDH, HI, SME, Hörgeschädigte, … |
| `signs` | signs & songs, songs, typesetting, karaoke | DASH `karaoke` on a subtitle | Signs & Songs, Songs, … |

| audio kind | meaning | container / bitstream sources | name |
|---|---|---|---|
| `alternate` | alternate mix | — | Original Mono, theatrical mix, … |
| `commentary` | commentary | Matroska FlagCommentary; AC-3 / E-AC-3 bsmod or DVB service_type 5; DASH / html-kind `commentary` | commentary, 音声解説, … |
| `description` | audio description | Matroska FlagVisualImpaired; bsmod 2; ISO 639 audio_type 3; DVB supplementary_audio editorial 1 or 3; DASH `description`; Apple `describes-video`; html-kind `description` / `main-desc` | AD, DVS, Audiodescription, … |
| `dialogue` | dialogue-enhanced / clean audio | Matroska FlagHearingImpaired on audio; bsmod 3 or 4; audio_type 2; editorial 2; DASH `enhanced-audio-intelligibility` | Dialogue Boost, … |
| `dub` | dubbed | Matroska FlagOriginal present and 0; DASH `dub`; html-kind `translation` | Dubbed, doublage, … |
| `karaoke` | karaoke | bsmod 7 on ≥ 2 channels | karaoke |
| `music` | music & effects, isolated score | bsmod 1; audio_type 1 | M&E, Isolated Score, … |
| `original` | original language | Matroska FlagOriginal = 1 | Original, VO (not ru / uk / pl), VOF, … |
| `voiceover` | voice-over | bsmod 7 on < 2 channels | Lektor, MVO, DVO, VO (ru / uk / pl), … |

DVB editorial_classification 3 ("spoken subtitles") is taken as `description` and bsmod 4
("dialogue only") as `dialogue`. The app shows `sdh` and `cc` apart. The subtitle format class
(`text`, `image`, `cc`) is derived from the codec on the server, never stored per track.

### Where sniff reads them

- **Matroska** `TrackEntry`: FlagHearingImpaired (0x55AB), FlagVisualImpaired (0x55AC),
  FlagTextDescriptions (0x55AD), FlagOriginal (0x55AE), FlagCommentary (0x55AF), FlagForced
  (0x55AA, kept raw in `SubtitleTrack::forced`), the name (0x536E), the WebVTT CodecIDs
  (`D_WEBVTT/METADATA` is no subtitle track), `S_ARIBSUB` (`arib_caption`), `S_TEXT/USF`.
  FlagEnabled is not read.
- **MP4 / MOV** inside `moov`: `trak/udta` `name` / `tnam` (three layouts; ffmpeg writes the
  bare text), Apple `tagc`, ISO `kind` (DASH roles and HTML kinds: ffmpeg writes one per
  disposition), `tref/forc`, the `hdlr` name as a fallback (muxer boilerplate dropped),
  `tx3g` displayFlags, `dac3` / `dec3` bsmod, `c708`, `dfxp`, `mp4s` under handler `subp`.
  `udta` ends the `trak` whose header the walk reads anyway.
- **MPEG-TS** PMT: ISO 639 audio_type, every DVB subtitling entry and every teletext subtitle
  page (one track each, with its language: teletext tracks had none before; a 0x46 VBI
  teletext PID is a subtitle track only when it lists a subtitle page), DVB
  supplementary_audio, the DVB AC-3 / E-AC-3 component_type, ATSC 0x81 (bsmod, with acmod from
  `num_channels`: bsmod 7 on a mono track is a voice-over, otherwise karaoke; only on AC-3 /
  E-AC-3 streams) and 0x86 (caption services); bsmod of the AC-3 / E-AC-3 frame already read.
- **First access units already read**: CEA-608 / 708 (`GA94` cc_data with a valid triple)
  in the HEVC prefix SEI in every container, the H.264 SEI and the MPEG-2 picture user data in
  TS. A 608 pair that is null once parity is dropped (`80 80`, the padding many encoders send
  with no caption service) and a zero DTVCC data pair do not count. These say captions are
  *signalled*, not that the text exists. They become `eia_608` / `eia_708` subtitle tracks
  with the video track's id, unless the container already lists a track of that codec (the PMT
  caption service, or an MP4 `c608` / `c708` track).
- **E-AC-3 frames** (read for Atmos in every container): bsmod when `infomdate` is set.
- **AVI**: a `vids` stream with FOURCC `DXSB` / `DXSA` is an `xsub` subtitle; `strn` is the
  name.

### Merge rules (`sniff::video::kind::resolve`)

1. A container or bitstream flag that is set wins. Absent or 0 is never evidence, except
   Matroska FlagOriginal = 0, which means `dub`.
2. Name kinds are added, never subtracted.
3. Forced is the one conflict the name wins: a subtitle whose name says "full", "complete",
   "dialogue", SDH or CC (and not "forced") loses a forced flag (muxers set it by mistake).
4. The default flag implies no kind.
5. Audio: commentary, description, music or karaoke removes original and dub; original or
   voice-over removes dub.

`kinds` is the result; `name_kinds` the part only the name gave.

### Name classifier (`sniff::video::classify_name`)

Pure Rust, no dependency. The name is normalised (full-width forms to ASCII, apostrophes
removed, `+` as `&`), cut into alphanumeric tokens (`&` is a token), and matched on whole
tokens, token prefixes, token sequences, case-sensitive tokens (`HI`, `AD`, `VO`, `MVO`) and
CJK substrings. `HI` is not SDH on a Hindi track; `VO` is a voice-over on a Russian, Ukrainian
or Polish track and the original elsewhere; `VF` / `VFF` / `VFQ` / `VFI` give nothing; "Sign
Language" is not signs; "Full HD" is not "full". A negation right before a term cancels it
(`non`, `no`, `not`, `without`, `sans`, `ohne`, `sin`, `senza`, `zonder`, `utan`, `uden`,
`uten`, `ilman`, `bez`, `без`): "Non-Forced", "Non-SDH", "No Commentary", "Sans commentaire"
and "Non-Dubbed" give nothing, while phrases that start with one ("per non udenti", "Non
English Parts") still match. Names that only repeat the codec, the channels or the language
give nothing. The 44 spec vectors and 11 negation vectors are `kind::tests::name_vectors`.

### Not read (docs/ROADMAP.md)

CEA-608 / 708 for H.264 / MPEG-2 in Matroska and MP4 (a new block per file) and AV1 T.35;
AC-3 bsmod in Matroska (the frame is not read when the header gives the channels); the DTS
ExSS asset type; DVD IFO and Blu-ray clpi / mpls; ASS style names; torrent-name and sidecar
file kinds (`.sdh.srt`, DUBBED, Lektor); original vs dub from TMDB; language variants
(pt-BR, es-419, zh-Hant); a re-probe of version 1 rows; server-side re-classification; showing
names in the app; MKV FlagEnabled, MP4 `txas` / `folw` / `alternate_group`, ATSC 0xCC, DVB EIT
component descriptors, SCTE-27, per-sample forced flags, SDH from subtitle content, Ogg / ASF /
FLV roles and a sign-language video kind.
