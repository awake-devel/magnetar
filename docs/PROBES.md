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
64 requests in flight, second peer after 3 s, 8 targets, minimum
confidence 80, agreement slack 15, upload 4 MiB / 256 KiB/s and a 5 s linger, dicts ≤ 2 MiB,
connection attempts (D142: `connect_attempts` unbounded, so every candidate address is tried until `peers` are alive or the session ends; `connect_parallel` 4 attempts in flight at once, each through the shared fetcher), peer exchange (D143): `pex_per_peer` 200, `pex_msgs_per_peer` 16, `pex_candidates_max` 1024, `pex_total` 400, `ports_per_ip` 3, `pex_wait` 3 s (see §13), sniff caps per file of 10 rounds (6 before D120: an MP4 asks its track skeleton before its
sample tables, a 2 h film with four tracks takes 9), 16 ranges and 4 MiB (a video payload in whole
pieces: the torrent's budget less the other targets' first reads).

## 6. What is stored and shown

The worker sends `DhtResult.probe` (`ProbeReport`: version, status `ok` / `partial`, bytes in
and served, peers used, files: index, size, label, confidence, verified, flags, game, video).
`PROBE_VERSION` is **2** since D123: each audio and subtitle track may carry `kinds` (values of
`PROBE_AUDIO_KINDS` / `PROBE_SUB_KINDS`, alphabetical, no duplicates, left out when empty) and
`name` (the track name, cleaned, at most 64 bytes, left out when absent; stored so the server
can re-classify later; shown by the app since D131, as VLC shows it). Version 1 rows have
neither. When names would
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
  `sf:text` / `sf:image` / `sf:cc`; since D137 `fk:{label}` and `fg:{group}` for **every**
  probed file, verified or not: the file-kind filter, facet and Stats breakdown, docs/FILTERS.md
  "File kind"). On version 2 rows `a:{lang}` leaves out audio tracks
  whose kinds include `commentary`, `music` or `karaoke`, and `s:{lang}` leaves out subtitle
  tracks whose kinds include `forced`, `signs`, `commentary` or `description`, so "French
  subtitles" means a French track to read the film with. The `audio_langs` / `sub_langs`
  columns keep every language. Since D129 the summary columns and `tags` are built from the
  **effective** tracks: the user's track corrections (§12) applied over the detection, so
  filters and facets count corrected values. About 1 GB per million probes (storage report,
  S4).
- `probe_files`: the per-file JSON (`[ProbeFile]`), always the raw detection (never corrected
  values).
- `probe_corrections` (D128, §12): the user's track corrections, never written by a probe.

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
queue torrents indexed since). Skipped at fill time: torrents with a probe of the current
probe version, blocked hashes,
and Nostr-only torrents that no default search shows (every event
from a blocked publisher, or below the trust threshold and not from a trusted publisher): a
DHT row would bring them back as DHT torrents, since a DHT row passes every trust threshold
(D51) and a publisher block only hides that publisher's events. They are queued by a later
pass once shown (block lifted, threshold lowered in Settings). Checked again at lease time: a
probe stored meanwhile deletes the row, and so does a torrent no longer shown; blocked or gone
closes it. **Re-probes (D127):** a torrent whose stored probe is older than `PROBE_VERSION`
(version 1, before track kinds, D123) is queued again with rank and first `due_at` 1: after
every never-tried row (negative) and before the retries (unix times). Its old probe stays
shown until a newer one replaces it (§8 store rule); a re-probe that gets no probe is retried
like any attempt, and the row is closed after the last one with the old probe kept. Probes
are "probed" for the queue only at the current version (fill, lease, `dht/results`, a new
find's probe). Adult torrents (category `porn` or the adult signal) are queued and leased like any
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
(pt-BR, es-419, zh-Hant); a re-probe of version 1 rows (since done, D127); server-side
re-classification; MKV FlagEnabled, MP4 `txas` / `folw` / `alternate_group`, ATSC 0xCC, DVB EIT
component descriptors, SCTE-27, per-sample forced flags, SDH from subtitle content, Ogg / ASF /
FLV roles and a sign-language video kind.

## 12. Corrections (D128–D131)

The user checks a file in VLC and corrects, per audio and subtitle track, what the probe got
wrong: the **language**, the **kinds** (forced, SDH, audio description, commentary, …), the
**name**, a track that **does not exist**, and a track the probe **missed**. Codec, resolution
and HDR are not correctable (machines detect them better). The app edits them from the detail's
Detected card (docs/APP.md "Detail"); the API is docs/API.md "Track corrections".

### Storage (`probe_corrections`, migration `20261109000000_probe_corrections.sql`)

Corrections live apart from the detection: `probe_files` always holds what the worker sent, and
**no probe ever writes a correction**, D127's re-probes included. The table references
`dht_torrents` (cascade), not `probe_attrs`, so a probe deleted after a worker token leak
(docs/WORKERS.md §7) and stored again gets its corrections back. One row per correction:

- `(infohash, file_i, track, pos)`: `file_i` is `ProbeFile.i`, the file's index in the DHT
  row's list (`ProbeFileInfo.dht_index` in the detail; the shown `index` is not used, since the
  shown list changes when a Nostr event arrives); `track` is `audio` or `sub`; `pos` the 0-based
  index in that file's stored `video.audio` / `video.subs` (the app shows "Track {pos+1}"),
  NULL for a track the user added;
- `removed` ("this track doesn't exist"), `set_lang` + `lang` (NULL = no language), `set_kinds`
  + `kinds` (the closed lists of §11, sorted), `set_name` + `name`; a `set_*` flag is stored
  only when the value differs from the detection, so an item without any is a **check**
  ("matches VLC");
- `detected`: a snapshot of what the detector said when the user corrected the track, e.g.
  `{"v":2,"n":3,"codec":"ac3","channels":2,"lang":"eng","kinds":["sdh"],"name":"English SDH","src":{"sdh":"name"}}`
  (`v` the probe version, `n` the number of tracks of that type in the file, absent values left
  out; an added track's snapshot is `{"v":2,"n":3}`);
- `device` (`android`, `api`: one API token for every device, so the client names itself),
  `created_at`, `updated_at`.

Undo deletes the row (back to the detection). An edit updates it in place (same id and
`created_at`, new snapshot and `updated_at`). No history table: the detection stays in
`probe_files` and in the snapshot. At most 256 rows per torrent and 96 items per request.

### Anchoring (`probe_fix::anchor`): computed on every read

Nothing is rewritten when a torrent is probed again: each read (detail, correction write, probe
store) places every correction on the current tracks. Rows are taken newest first
(`updated_at DESC, id DESC`) and a track claimed by one is not claimed by another, so the
user's latest intent wins over an old correction that would match again. For a correction of a
detected track with snapshot S in file F:

1. F is gone or has no video recon: **unplaced**, reason `file`.
2. **Same place**: the list still has `S.n` tracks, `pos` is in it, its codec is `S.codec`
   (audio: same channels, or either unknown), and nobody claimed it. Language, kinds and name
   may differ: the detector changed its mind, the user's VLC check still holds.
3. **Moved**: the tracks (claimed or not) with the same codec, the same language, compatible
   channels and, when both the snapshot and the probe are version 2, the same kinds and name (a
   version-1 snapshot has neither: D127's v1 → v2 re-probes compare codec and language only).
   One candidate: there. Several, the correction's own position among them: there. The pick
   must then be unclaimed, else step 4. This is the D123 TS expansion: teletext pages become
   tracks and shift the list. Candidates are counted before claims on purpose: when two tracks
   match a correction, it stays unplaced even after the user checks or corrects one of them,
   instead of landing on the other one, a track the user never touched.
4. Otherwise **unplaced**, reason `changed`: kept, listed in `ProbeInfo.unplaced`, never applied
   to another track. The user removes it or corrects the track again.

An anchored correction gets `review` when the current detection differs from the snapshot (in
language, kinds or name; version-1 snapshots: language only); its overrides still apply. Added
tracks always apply, after the detected tracks of their type, oldest first; they get `review`
when the probe now lists another number of tracks of that type (it may list the added one).

### Track order and VLC

The stored order is sniff's, which is the container's: Matroska TrackEntry order, MP4 `trak`
order, MPEG-TS PMT order (D123 expands DVB subtitling and teletext entries into one track per
entry on the same PID), AVI stream order. Captions found in the video stream (`eia_608` /
`eia_708`, kind `cc`, no language) come after the container's subtitles. VLC lists audio and
subtitle tracks per type in elementary-stream creation order, the same container order ("Disable"
is not a track). Known or possible exceptions, all **UNVERIFIED** against VLC:

1. VLC lists CEA-608 captions as "Closed captions 1–4", and only once it decodes them; the probe
   stores one track per codec.
2. VLC's handling of TS teletext and DVB subtitling as one stream per page or entry is assumed to
   match D123's expansion.
3. An MP4 text track used for chapters (`tref/chap`) may be listed by sniff but hidden by VLC.
4. A track the worker dropped (a codec that is not a valid token) or beyond the caps (32 audio,
   64 subtitles) shifts every later number.

"This track doesn't exist" and "Add a track the probe missed" cover all four.

### Effective tracks everywhere (`probe_fix::apply`, D129)

The effective track is the detection with each `set_*` field replaced, plus `pos` and a `fix`
(id, fields, removed, added, review, the current detection, time, device); removed tracks stay
in the list, marked; added tracks have no codec. One function serves every reader:

- the **detail** (`probes::probe_for`): effective files, `dht_index`, `unplaced` (path from the
  shown list, else the DHT list);
- the **probe store path** (`probes::store`, D127 re-probes included): the `probe_attrs` row is
  locked `FOR UPDATE`, the corrections read, and the summary and tags written from
  `summary_of(apply(report))`; `probe_files` stores the raw report. A refused weaker re-probe
  changes nothing;
- the **correction write**: recomputes `audio_codecs`, `audio_langs`, `sub_langs` and `tags`
  and writes them only when they changed, which fires the D122 trigger, so the torrent is
  re-indexed at the next change-log pass (D43). A pure confirmation writes nothing.

`summary_of` skips removed tracks and gives no `ac:` tag for an added track's empty codec;
every other rule (§6, D124 language rules on the effective kinds, main file = largest probed
file) is unchanged, so corrections on another file show in the detail and change no tag. Both
write paths lock `probe_attrs` before reading `probe_corrections`: no deadlock, no lost update;
a correction needs a probe row, so it cannot race a first probe's INSERT.

Worker reports never carry corrections: `pos` and `fix` are left out when None, and the server
refuses a report track that carries either (`audio_track` / `sub_track`). The report format and
`PROBE_VERSION` (2) are unchanged.

### Detector accuracy (`StatsResponse.probe_corrections`, D130)

Where each detected kind came from is computed when the correction is written and stored in
`detected.src`: `name` when `sniff::video::classify_name` gives the kind from the stored track
name (and language), else `flags` (Matroska / MP4 / TS flags, bitstream fields, `enm`, captions
in the video). A kind that both a flag and the name gave counts as `name`, since the report does
not carry sniff's split. **UNVERIFIED**: names were cut to 64 bytes by the worker, which may hide
a kind word at the end of a long name. The language source is not split: it always comes from
the container tag.

All counts use the snapshot (what the detector said when the user checked), never the current
detection, over every row of `probe_corrections` (one SELECT per stats request):

| Count | Definition |
|---|---|
| `tracks` | corrections of detected tracks (`pos` set) |
| `removed` | of those, "doesn't exist" |
| `checked` | not removed, no field overridden ("matches VLC") |
| `corrected` | not removed, at least one field overridden |
| `added` | tracks the user added (`pos` NULL) |
| `languages.checked` | not removed, the snapshot has a language |
| `languages.wrong` | of those, the language overridden |
| `languages.filled` | the snapshot has no language, the user set one |
| `names_changed` | the name overridden |
| `kinds` | not removed, snapshot version ≥ 2: per snapshot kind, `(track, kind, source)` gets `checked + 1`, and `wrong + 1` when the kinds were overridden without that kind; sorted by wrong, then checked (descending), then track, kind, source |
| `missed` | kinds the user set that the snapshot (version ≥ 2) did not have, by `(track, kind)`, most first |

So "SDH from track names: 4 of 31 wrong" is the `sub` / `sdh` / `name` entry with `checked` 31
and `wrong` 4.

### Not done (docs/ROADMAP.md)

Codec, resolution and HDR corrections; an offline correction queue on the phone; corrections in
the result list's badges. **UNVERIFIED**: the app on a device, and VLC numbering parity per
container (above).

## 13. Diagnostics (D141)

Each probe fills a `ProbeDiag` (crates/core/src/probe/mod.rs) that the worker logs, one line
per probe, at debug level under the target `probe_diag`. Turn it on with
`RUST_LOG=info,probe_diag=debug` in the worker's environment. Nothing new is sent to the server.

```
kind=probe src=backfill status=no_pieces end=no_peers late=false ms=17342 whole=true piece_kib=1024
  targets=3 results=0 cut=2 no_bytes=1 no_det=0 low_conf=- struck=0 first_byte_ms=- unchoke_ms=-
  handoff_choked=true connects=ok:1,timeout:1 max_alive=1 cap_blocked=true cand_left=6
  drops=closed:2 choke_expired=0 pex_msgs=2 pex_added=9 pex_ok=1 bytes_in=0 incomplete=true
kind=skip why=slot_wait        # backfill dict not probed: slot_wait, no_handoff or gate
```

| field | meaning |
|---|---|
| `end` | why the target loop stopped: `done`, `deadline` (session), `budget` (`max_bytes`), `no_peers`; or a failure code (`timeout`, `layout`, `no_targets`, `big_dict`) |
| `late` | the session deadline had passed when it stopped |
| `whole`, `piece_kib` | first reads fetched whole pieces; the piece length |
| `cut`, `no_bytes`, `no_det` | targets never started, started with no byte, with bytes and no detection |
| `low_conf` | detections dropped under `min_confidence` (label:confidence) |
| `first_byte_ms`, `unchoke_ms` | since the probe started |
| `connects` | further connections by outcome: `ok`, the peer error code, `surplus` (connected after `peers` were already alive, closed) or `cancelled` (in flight when `peers` became alive, D142) |
| `drops` | peers closed, by reason (`stall`, `closed`, `write`, `unrequested`, `hash_mismatch`, ...) |
| `choke_expired` | peers that had a wanted piece and still choked us after `unchoke_wait` |
| `cap_blocked` | a further peer was wanted with fewer than `peers` alive and candidates left, refused because `connect_attempts` was used up (D142; before, dead slots counted toward `peers`, a bug: two dropped peers blocked every further connection) |
| `cand_left` | candidate addresses never tried (attempts cancelled in flight count as tried) |
| `pex_msgs` | `ut_pex` messages received from peers, all of them (D143) |
| `pex_added` | new candidate addresses learned from them: not already known (candidate, connected, tried or struck), within the bounds below |
| `pex_ok` | connections that came from such an address and succeeded (became a live peer; a `surplus` one does not count) |

Codes and counts only: no hash, name, path or address.

### Connections and partial pieces (D142)

Measured on rpi, 688 backfill probes (2026-11-04): 190 ended with nothing; 102 of them had used
all 4 connection attempts with candidates left (median 12), 13 were `cap_blocked`, 20 got part of
a whole piece from a slow peer and threw it away. Now:

- `peers` (`MAGNETAR_PROBE_PEERS`) bounds the **live** peers; dead slots (a dropped peer keeps its
  slot index for the session) do not count. A further connection starts only while fewer than
  `peers` are alive.
- Every candidate address is tried: `ProbeConfig::connect_attempts` defaults to `usize::MAX`
  (a code constant, no `MAGNETAR_` setting). Attempts run `ProbeConfig::connect_parallel` (default
  4, constant) at a time; each goes through `Fetcher::connect_for_probe`, so the address filter,
  attempt pace, brake and open-connection cap still apply. In-flight attempts are not counted
  toward `peers` (with `peers = 2` that would allow one attempt at a time and no parallelism):
  a connection that succeeds when `peers` are already alive is closed (`surplus`), and once
  `peers` are alive the attempts still in flight are cancelled (`cancelled`). No attempt starts
  after the session deadline. `connect_parallel` 0 is treated as 1. The worst case of the parallelism is `connect_parallel - 1` connections that succeed at the same moment and are closed as `surplus` (counted in `connects`, so the router cost can be measured; UNVERIFIED on the router). The bounds on connections are therefore: `peers` live, `connect_parallel`
  in flight, `connect_attempts` in all, `session` in time, plus the fetcher's own.
- Whole-piece mode (`piece length <= whole_piece_max`): a piece that completes is verified as
  before. Blocks of a piece that does not complete (the peer stalled, was dropped, or the session
  ended) are used as **unverified** data, as in block mode (`insert_unverified`, the sending peer
  recorded, so a later strike voids the result); the result has `verified=false` and
  `detection.flags.unverified`. Bytes of a piece that failed its hash are dropped, with the peer's
  other unverified blocks. `max_bytes` accounting is unchanged (every received byte counts).

### Peer exchange, receive-only (D143)

Measured on eee after D142 (657 probes): 28.5 % ended with nothing; the empty ones tried a
median of 4 candidate addresses and 153 of 187 had none left; 92 % of connection attempts fail
(stale DHT peer lists). Connected peers know fresh peers, so probe connections speak BEP 11
(`ut_pex`), receive-only:

- **Advertised on probe connections only.** With `FetchConfig::probe` on, our extended handshake
  adds `m.ut_pex` = 2 (the hand-off connection and `Fetcher::connect_for_probe`). With probes off
  the handshake is byte-identical to before (test `only_probe_connections_advertise_pex`).
- **Never sent.** D117's policy holds: only `ProbeMsg` goes out; there is no `ut_pex` variant, so
  a probe cannot tell a peer about other peers (the tests check the fake peers never get one).
- **Parsed with care** (`wire::parse_pex`): the bencoded dict's `added` (compact IPv4, 6 bytes
  each) and `added.f` (one flag byte each, `0x02` = seed). `added6`, `dropped` and `dropped6`
  are ignored. A trailing partial entry, port 0 and `0.0.0.0` are skipped; a wrong type is the
  same as absent; a payload that is not a dict is ignored (the peer is kept; a flood is cut by the
  per-peer skipped-bytes budget, 1 MiB, that already existed). The extended-message cap of 64 KiB
  applies as before.
- **Fed to the candidates.** New addresses (not already a candidate, connected, tried or struck)
  are queued, seeds at the front. They are tried like any candidate: `connect_parallel` at a time,
  each through `Fetcher::connect_for_probe` (address filter, pace, brake, open-connection cap).
  PEX never bypasses the fetcher. Messages that arrive while the hand-off peer still sends the
  metadata are kept (at most 8 messages, `PEX_HANDOFF_MSGS`) and handed to the probe.
- **A stuck probe waits** (`pex_wait`): when no live peer has a wanted piece (or all choke) and
  no candidate can be tried, the probe waits up to `pex_wait` after a peer connected for its first
  `ut_pex` message, only for a peer that advertised `ut_pex` in its extended handshake and has
  sent none yet; never past the session deadline.
- `probe_peers` (worker) no longer cuts the list at 16: every known address is passed, deduped.

Bounds (all `ProbeConfig` fields or constants, none is a `MAGNETAR_` setting; generous, meant to
stop abuse only):

| bound | value | why |
|---|---|---|
| `pex_per_peer` | 200 addresses per peer per probe | BEP 11 senders keep a message to about 50 entries; 200 is four messages' worth |
| `pex_msgs_per_peer` | 16 messages parsed per peer per probe | a client sends one about every minute; later ones are only counted |
| `pex_candidates_max` | 1024 queued candidates | memory; the addresses the job brings are never cut, PEX stops adding past it |
| `pex_total` | 400 addresses from PEX per probe, all peers together | `pex_per_peer` is per peer, and each PEX-found peer may name 200 more: this is the real ceiling on extra attempts |
| `ports_per_ip` | 3 distinct ports per IPv4 per probe (job addresses and PEX alike; the hand-off peer counts) | one ut_pex message must not become a port scan of one host from our line; NATed swarms rarely show more |
| `pex_wait` | 3 s (0 = never wait) | the same order as `second_peer_after`; also bounded by `session`; only taken while a peer slot is free and `connect_attempts` is not used up (a learned address could not be tried otherwise) |
| `wire::PEX_MAX_ADDED` | 1024 entries per message | the 64 KiB message cap would allow about 10 000 |
| `PEX_HANDOFF_MSGS` | 8 messages kept from the metadata phase | the hand-off connection reads the peer's messages before the probe starts; this bound (not `pex_msgs_per_peer`) governs that phase, and those messages count against `pex_msgs_per_peer` afterwards |

Not measured on real swarms (**UNVERIFIED**): the gain in results, and the cost on the line and
the router: every PEX address is one more connection attempt, so the number of attempts per probe
goes up. Worst case per probe: the job's addresses (at most `ports_per_ip` per IP) plus at most
`pex_total` 400 from PEX, each tried once; with `connect_parallel` 4 and a 3 s connect timeout
about 60 attempts fit in a 45 s session, more when attempts fail fast, always paced and braked
by the fetcher (before D143: at most 16). Watch `connects`,
`pex_added` and `pex_ok` on a worker before trusting it on the Pi's router.
