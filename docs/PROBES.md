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
- **Never on a torrent the worker's blocklist stops, one classified adult, or with no
  blocklist loaded** (the worker's own classifier and screen run first, docs/WORKERS.md §1).
  The server checks again with its own classification and blocklist before storing anything.
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
   block, MP4 `moov`, ZIP central directory, …) within its caps (6 rounds, 16 ranges, 4 MiB per
   file) and the torrent's `MAGNETAR_PROBE_MAX_BYTES`.
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
  `incomplete`), per file sniff's caps, per peer 64 requests in flight (capped by its BEP 10
  `reqq`), 15 s unchoke wait, 10 s stall, 45 s per probe, `MAGNETAR_PROBE_PARALLEL` probes at
  once (memory ≤ parallel × max bytes).
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
confidence 80, agreement slack 15, upload 4 MiB / 256 KiB/s and a 5 s linger, dicts ≤ 2 MiB.

## 6. What is stored and shown

The worker sends `DhtResult.probe` (`ProbeReport`: version, status `ok` / `partial`, bytes in
and served, peers used, files: index, size, label, confidence, verified, flags, game, video).
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
- writes it only when the `dht_torrents` row exists, the server does not classify it adult
  (the worker's gate, `Classification::adult`: category `porn` **or** the adult signal on its
  name and paths, re-computed from the stored row) and the hash is not blocked, one savepoint per row (D110); a re-probe replaces it only when it is of a
  newer version or verifies at least as many files;
- answers `probe_stored` / `probe_rejected` in `DhtAck`. A refused probe never costs its dict.

Tables (migration `20261105000000_probes.sql`):

- `probe_attrs` (one row per torrent, references `dht_torrents`, cascade): version, time,
  `supplied_by` (the worker), status, files probed / verified, bytes, and a summary of the main
  payload (largest probed file): kind, platform, title id, container, video codec, height, HDR,
  bit depth, duration, audio codecs and languages, subtitle languages, flags, and `tags`
  (`k:video`, `r:2160p`, `v:hevc`, `h:dolby_vision`, `a:fre`, `s:eng`, `ac:eac3`, `p:ps5`,
  `x:atmos`). About 1 GB per million probes (storage report, S4).
- `probe_files`: the per-file JSON (`[ProbeFile]`).

No GIN index on `tags` and no search-index trigger yet: with `ORDER BY probed_at LIMIT` a tags
GIN collapses to a full scan on rare mixes (check1 #3); facets go to the search index when
filters are built. Detail API: `TorrentDetail.probe` with each file's path from the server's own
list and the label's description (docs/API.md). It is hidden when the server now classifies the
torrent adult (the DHT row, and the Nostr row the detail shows: category `porn` or the adult
signal). File indexes point into the `files` the detail returns: on a torrent with a Nostr
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

What to watch: `workers[].dht.probe` in `GET /api/v1/stats` (started, ok / partial / choked /
no_pieces / failed, skipped by reason, bytes in, uploaded, verified files, surprises),
`probe_backfill` and `workers[].dht.backfill` (§10), and the router health brakes.

## 9. Not done / UNVERIFIED

- **Not run on real swarms.** Every test uses loopback fake peers (`fake_seeder`). Unverified:
  how often real clients unchoke a peer that has nothing (`HAVE_NONE`), anti-leech bans,
  whether clients accept `HAVE_NONE` before the extended handshake, whether the Fast bit changes
  metadata success, real bytes and packets per probe on eee and the Pi, Pi SHA-1 speed.
- No search filters on probe results (tags, facets in the search index), no app screen: the
  Android `TorrentDetail` has no `probe` field yet, its JSON decoder drops the key
  (`ignoreUnknownKeys`).
- v2-only piece checks (BEP 52 Merkle proofs) are not built: hybrid torrents are checked with
  their v1 piece hashes.
- uTP peers are not reached (TCP only, as the metadata fetcher).

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
category `porn` on any row, and Nostr-only torrents that no default search shows (every event
from a blocked publisher, or below the trust threshold and not from a trusted publisher): a
DHT row would bring them back as DHT torrents, since a DHT row passes every trust threshold
(D51) and a publisher block only hides that publisher's events. They are queued by a later
pass once shown (block lifted, threshold lowered in Settings). Checked again at lease time: a
probe stored meanwhile deletes the row, and so does a torrent no longer shown; blocked, gone,
or adult on any of its rows (DHT or Nostr, the worker's `Classification::adult` rule: category
`porn` or the adult signal on its name and paths) closes it. A lease whose rows were all
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
the worker's blocklist and adult check, then the probe gates. A backfill dict waits up to 30 s
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
| `private`, `v2_only`, `invalid`, `oversize` (worker limit ≥ the server's), worker `blocked` | closed with that outcome |
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
