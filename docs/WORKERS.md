# Crawl workers

Other machines can share the crawling with the main server: each runs
`magnetar-server worker` from its own internet line, takes a share of the **seed checks**
and runs its own **DHT crawler** (nodes, peer lookups, BEP 9 metadata fetches). The main
server stays the single source of truth: Postgres, the search index and the API live only
there; a worker has no database and talks to the server over its HTTPS API with a worker
token. Decisions: ARCHITECTURE.md D96–D100.

```
   phone ──API token──▶ ┌─────────────── main server ───────────────┐
                        │ Postgres · search index · API             │
                        │ own Nostr crawler, seed checks, (DHT)     │
                        │ /api/v1/worker/* (worker tokens)          │
                        └──────▲──────────────────────▲─────────────┘
              lease / results  │  triage / uploads    │  (HTTPS, e.g. Tailscale)
                     ┌─────────┴────────┐   ┌─────────┴────────┐
                     │ worker "eee"      │   │ worker "rpi"      │
                     │ seed checks + DHT │   │ seed checks + DHT │
                     │ own line, own IDs │   │ own line, own IDs │
                     └───────────────────┘   └───────────────────┘
```

What a worker does **not** do: Nostr relay crawling (the relay crawler keeps its cursors
and relay state in Postgres and one crawler already covers the ~30 relays; D99), header
probes (do not exist yet), downloads, search.

## 1. How it works

### Tokens (D96)

The server reads `MAGNETAR_WORKER_TOKENS=name:token,name:token` — one token per worker, each
with a name (1–32 of `a-z 0-9 - _`), at least 24 characters, all distinct and different from
`MAGNETAR_API_TOKEN` (the server refuses to start otherwise). The two kinds never overlap:

| | API token (phone, `MAGNETAR_API_TOKEN`) | worker token |
|---|---|---|
| search, detail, stats, settings, saved searches, relays, `/relay`, events batch, downloads | yes | **401** |
| `/api/v1/worker/*` | **401** | yes |

Every comparison is constant-time and walks all configured tokens. Without
`MAGNETAR_WORKER_TOKENS` the worker endpoints answer 403 `workers_disabled`. `/health`
lists the feature `workers` when tokens are configured.

### Version skew (D96)

Every worker request carries `X-Magnetar-Worker-Api: 1` (`fleet::proto::WORKER_API`). A
server of another number answers 409 `worker_api_mismatch` naming both versions; a server
without the worker API answers 404. Either way the worker stops at once with that message and
exits with status **2** (the systemd unit below does not restart on 2). Network errors and 5xx
are never fatal: the worker retries (2 s doubling to 60 s), so a server restart only pauses
it. Rule: run the same Magnetar version on the server and its workers; the number moves with
any change an older peer could misread.

### Endpoints (all `POST`, JSON, worker token)

| Path | Body → answer | |
|---|---|---|
| `/api/v1/worker/hello` | `Hello {version, arch, liveness, dht, dht_ports, metadata_max, blocklist_version}` → `HelloResponse {name, server_version, worker_api, lease_secs, max_lease, max_triage, max_results, max_body, metadata_max}` | at start, and again after a failed heartbeat |
| `/api/v1/worker/heartbeat` | `Heartbeat {uptime_secs, rss_mb, mem_available_mb, pressure, dht}` → 204 | every 30 s |
| `/api/v1/worker/liveness/lease` | `{max}` → `Lease {expires_at, items:[{infohash, trackers}]}` | |
| `/api/v1/worker/liveness/results` | `{results:[Liveness]}` → `{accepted, ignored}` | |
| `/api/v1/worker/liveness/release` | `{infohashes}` → `{accepted, ignored}` | unchecked leases given back at stop |
| `/api/v1/worker/dht/triage` | `{items:[{h, via}]}` (≤ 2000) → `{fetch:[h], blocked, known}` | |
| `/api/v1/worker/dht/results` | `{items:[DhtResult]}` (≤ 1000, ≤ 32 MiB) → `{stored, not_new, blocked, rejected, applied, ignored}` | |

`DhtResult = {sighted, via, peers_seen, kind, …}` with `kind` one of `metadata {raw}` (the raw
bencoded info dict, base64), `blocked {v1?, tier, rule_version}` (hash only), `private`,
`v2_only`, `invalid {code}`, `oversize`, `failed {code}`.

### Seed checks: leases (D98)

`liveness_leases (infohash, worker, leased_at, expires_at)`. A lease is taken in one
transaction under an advisory lock: the server's own due query (lanes, due order, D45), then
`liveness_schedule.due_at` moved to the lease's end and the holder recorded. So:

- **never two holders on one infohash**: the server's own batches claim the same way
  (holder `''`, 30 min), so a worker never gets what the server is checking and vice versa;
- **a dead worker's work comes back** with no cleanup: at the lease's end (`MAGNETAR_WORKER_LEASE_SECS`,
  default 900 s) its rows are simply due again, for anyone;
- results are accepted only from the worker that still holds the row (a late answer for an
  infohash re-leased to someone else, or already checked by a forced check, is `ignored`);
  they go through the server's own `write_results` (result, next due time, streaks, history,
  search-index change log), i.e. exactly as if the server had checked them. The worker's
  `checked_at` is clamped to the lease window; `expires_at` is the server's schedule;
- writing any result deletes the lease row; the hourly maintenance removes rows a day past
  their end.

The worker checks with the same `core::liveness::Scraper` as the server (`MAGNETAR_LIVENESS_*`
apply to it), from its own line: trackers see another IP, so the per-tracker rate limit
(1 request/s) is per machine and the fleet's throughput adds up.

### DHT: dedupe and verification (D97)

1. The worker's own nodes (own BEP 42 ids for its external IP, own ports) sample and listen.
2. New hashes pass the worker's in-memory seen filter, then go to the server in batches
   (≤ 1000 / 1 s). The server runs **its own** triage (`dht::store::triage`): blocked → drop,
   already known → drop, in `dht_seen` and not due → drop, else `queued` with a 1 h lease. The
   answer lists what this worker may fetch. A second worker (or the server's crawler) asking
   about the same hash during that hour gets nothing: the whole fleet fetches each hash once.
   A worker that dies leaves leased rows that the server's retry loop takes back after 1 h
   (when the server's own crawler runs; else they wait for a later sighting).
3. The worker collects peers and fetches the info dict, keeps the **raw bytes**, classifies
   it and runs the hard blocklist (D53) on every name/path variant **before anything leaves
   the machine**: a blocked torrent is reported as `blocked` with its hash (and a hybrid's
   SHA-1) only — no name, no file, no dict. The server records it in `blocked_infohashes`
   (origin `dht`), only for a hash it handed out and still holds queued, and a hybrid's
   claimed SHA-1 only when nothing is stored under it: a worker cannot block what the index
   already has.
4. Otherwise the raw dict is uploaded. The server **verifies it again**
   (`metainfo::outcome_from_raw`: size ≤ its `MAGNETAR_DHT_METADATA_MAX`, SHA-1 or truncated
   SHA-256 of the exact bytes = the infohash, value count before decoding, parse), then hands
   it to its crawler's own writer (`persist_batch`): **its** classifier (adult category, D52,
   D69) and **its** blocklist run, then the row is stored. A mismatching dict is `rejected`
   and logged by hash; the row keeps its lease. The wire format has no field for a category or
   a verdict on a dict: nothing a worker says about content is trusted.
5. Statuses (`no_peers`, other failures with the 1 h / 6 h / 24 h backoff, private, v2-only,
   invalid) are applied only to hashes the server holds queued. `oversize` is final only from
   a worker whose limit is at least the server's; a smaller worker (the Pi's 4 MiB) leaves the
   torrent to someone with room (normal backoff).

### What a worker keeps (D100)

`<state dir>/dht.json` (identity: external IP, node ids, the ports they were made for; up to
1000 good contacts), written atomically every 10 min and at stop, and `<state dir>/heartbeat`
(unix time of the last accepted heartbeat, for health checks). State dir:
`MAGNETAR_WORKER_STATE_DIR`, default `$XDG_STATE_HOME/magnetar/worker` or
`~/.local/state/magnetar/worker`. No seen-cache is persisted (the server's triage is the
dedupe; the Bloom filter only saves requests) and no SQLite: the workspace dropped `rusqlite`
with the ffi crate (D79), and two small files need no database.

### Bounded memory (D100)

| | default (worker) | RPi profile |
|---|---|---|
| crawl pool `MAGNETAR_DHT_POOL` | 200 000 (~20 MB; server default 1 M) | 100 000 |
| fetch queue | 20 000 | same |
| seen filter | 2 × 4 MiB | same |
| fetches in flight `MAGNETAR_DHT_FETCH_CONCURRENCY` | 64 | 32 |
| largest dict `MAGNETAR_DHT_METADATA_MAX` | 16 MiB (never above the server's) | 4 MiB |
| metadata in flight | 4 × max dict, 8–256 MiB; 2 decodes at once | 16 MiB |
| upload buffer `MAGNETAR_WORKER_UPLOAD_BUFFER_MB` | 16 (fetching waits when full) | 8 |

The **governor** reads `MemAvailable`, the worker's RSS and `nf_conntrack_count/max` every
5 s. Below `MAGNETAR_WORKER_MIN_FREE_MB` (default 250) or above `MAGNETAR_WORKER_RSS_MAX_MB`
(default off; 200 on the Pi) it pauses DHT sampling and new seed-check leases and halves the
KRPC and TCP rates (released 50 MB above the floor / under 90 % of the ceiling). Connection
tracking above 90 % (the Pi's table holds 7680 entries) halves the rates until it is under
80 %. On top of that the router **health probe** of D66 (`MAGNETAR_DHT_PROBE`) brakes as on the
server. Measured RSS: §6.

### Stats

`GET /api/v1/stats` (API token) → `workers: [WorkerStats]`, one per configured token:
`name, connected` (heard from in 2 min), `version, arch, first_seen, last_seen, uptime_secs,
leases_held, liveness_results_total, liveness_results_per_min, triaged_total, queued_total,
metadata_total, metadata_per_min, stored_total, blocked_total, rejected_total, rss_mb,
mem_available_mb, pressure` and `dht {mode, nodes, ports, qps, answered_share, brake_active,
health_brakes, sampling_paused, routing_nodes, pool, queue, unique_per_min, metadata_per_min,
upload_backlog, external_ip_known}`. Rates are counted by the server from what it accepted
(10-minute window); memory and DHT come from the last heartbeat. The app is unchanged: the
field has a default, older apps ignore it.

## 2. Server setup

Add to the server's environment (`.env` with compose, `server.env` natively):

```sh
# one line, one token per worker
MAGNETAR_WORKER_TOKENS=eee:$(openssl rand -hex 32),rpi:$(openssl rand -hex 32)
# optional
MAGNETAR_WORKER_LEASE_SECS=900
```

With compose, add the variable to `compose.override.yml` (compose.yml passes only listed
variables, D37a):

```yaml
services:
  magnetar:
    environment:
      MAGNETAR_WORKER_TOKENS: ${MAGNETAR_WORKER_TOKENS:-}
```

Restart the server. It logs `crawl workers: 2 token(s) configured`, and `GET /health` lists
`workers`. The server's own crawler, seed checks and DHT keep running; the workers add to
them. Workers reach the server the way the phone does (Tailscale serve, reverse proxy, LAN).

## 3. Worker configuration

| Variable | Default | |
|---|---|---|
| `MAGNETAR_WORKER_SERVER` | required | the server's base URL, e.g. `https://<host>.<tailnet>.ts.net` |
| `MAGNETAR_WORKER_TOKEN` | required | this worker's entry of `MAGNETAR_WORKER_TOKENS` (the token only) |
| `MAGNETAR_WORKER_STATE_DIR` | `<state dir>/worker` | identity and contacts (§1) |
| `MAGNETAR_WORKER_LIVENESS` | `true` | take seed-check leases |
| `MAGNETAR_WORKER_LIVENESS_BATCH` | `500` | infohashes per lease (capped by the server's `MAGNETAR_LIVENESS_BATCH`) |
| `MAGNETAR_WORKER_MIN_FREE_MB` | `250` | governor floor, 0 = off |
| `MAGNETAR_WORKER_RSS_MAX_MB` | `0` (off) | governor ceiling for the worker's own RSS |
| `MAGNETAR_WORKER_UPLOAD_BUFFER_MB` | `16` | results waiting for upload |
| `MAGNETAR_DHT` | `off` | `aggressive` (4 nodes on open UDP ports, passive capture) or `polite`; off = seed checks only |
| `MAGNETAR_DHT_PORT`, `_NODES`, `_QPS`, `_TCP_RATE`, `_TCP_MAX`, `_PROBE`, `_POOL`, `_FETCH_CONCURRENCY`, `_METADATA_MAX`, `_PEER_LOOKUPS`, `_LOOKUP_TIMEOUT_MS`, `_PEERS_PER_HASH` | as the server (docs/CONFIG.md), except pool 200 000 and fetches 64 | |
| `MAGNETAR_DHT_UPNP` | **off** for workers | `true` maps the UDP ports on the router (it is somebody else's router: ask first) |
| `MAGNETAR_LIVENESS_TRACKER_RPS`, `_TRACKER_CONCURRENCY`, `_DHT`, `_DHT_CONCURRENCY` | as the server | the worker's seed checker |
| `MAGNETAR_BLOCKLIST_FILE` | unset | extra blocklist terms, as on the server (the server screens again anyway) |

`DATABASE_URL` and `MAGNETAR_API_TOKEN` are not used. `magnetar-server worker --seconds N`
runs for N seconds; either way it prints a JSON report (seed checks, DHT counters, what the
server answered, RSS, CPU) when it stops. Exit status 2 = refused by the server (token,
version), 1 = other error (e.g. UDP port taken), 0 = stopped.

### Profiles (router benchmarks of 2026-10-26)

Both lines were measured with the stage-1 router benchmark (KRPC, new TCP attempts, held
connections; ~30 % margin under the knees).

**EEE** (x86_64, 4 cores, 7.7 GB, Docker; router: no knee up to 1600 KRPC q/s, TCP attempts
clean to 400/s, 480 held):

```sh
MAGNETAR_DHT=aggressive
MAGNETAR_DHT_QPS=1000
MAGNETAR_DHT_TCP_RATE=300
MAGNETAR_DHT_TCP_MAX=350
MAGNETAR_DHT_POOL=1000000
MAGNETAR_DHT_FETCH_CONCURRENCY=128
MAGNETAR_WORKER_LIVENESS_BATCH=1000
```

**RPI** (Raspberry Pi 3B, aarch64, 905 MB; the limit is its own conntrack table, 7680
entries: KRPC clean to 800 q/s, TCP attempts to 300/s, ~400 held):

```sh
MAGNETAR_DHT=aggressive
MAGNETAR_DHT_QPS=400
MAGNETAR_DHT_TCP_RATE=150
MAGNETAR_DHT_TCP_MAX=300
MAGNETAR_DHT_POOL=100000
MAGNETAR_DHT_FETCH_CONCURRENCY=32
MAGNETAR_DHT_METADATA_MAX=4194304
MAGNETAR_WORKER_RSS_MAX_MB=200
MAGNETAR_WORKER_MIN_FREE_MB=250
MAGNETAR_WORKER_UPLOAD_BUFFER_MB=8
MAGNETAR_WORKER_LIVENESS_BATCH=300
MAGNETAR_LIVENESS_DHT_CONCURRENCY=16
```

Optional on the Pi (root): `sysctl net.netfilter.nf_conntrack_max=32768` removes the
conntrack limit (the governor then rarely brakes).

## 4. Deploy on EEE (Docker)

`docker/compose.worker.yml` runs only the worker (no database), publishes only its DHT UDP
ports and keeps its state in a volume. On EEE, from a checkout of the same version as the
server:

```sh
cd magnetar
cat > docker/worker.env <<'EOF'
MAGNETAR_WORKER_SERVER=https://<server>.<tailnet>.ts.net
MAGNETAR_WORKER_TOKEN=<the eee token>
EOF
chmod 600 docker/worker.env
docker compose -f docker/compose.worker.yml --env-file docker/worker.env build   # until an image is published
docker compose -f docker/compose.worker.yml --env-file docker/worker.env up -d
docker compose -f docker/compose.worker.yml logs -f worker
```

The EEE profile is the file's default; override any value in `docker/worker.env`. The
container reaches a Tailscale name through the host when the host resolves it for Docker; if
`docker compose … exec worker getent hosts <server>.<tailnet>.ts.net` finds nothing (Docker
copies the host's upstream DNS, not MagicDNS), set `MAGNETAR_WORKER_SERVER_HOST` and
`MAGNETAR_WORKER_SERVER_IP` (the server's `tailscale ip -4`) in `docker/worker.env`: the
compose file then adds that `extra_hosts` entry (TLS still checks the name). The UDP ports
(`6891-6894` by default, `MAGNETAR_DHT_PUBLISH` to change the host side) must be free on EEE.
The health check reads the heartbeat file (healthy = a heartbeat accepted in the last 3 min).
`restart: unless-stopped` restarts it after crashes and reboots; a refusal by the server
(exit 2) also restarts it, every few seconds, until fixed: check the logs.

## 5. Deploy on the RPI (static binary)

The Pi needs a 64-bit OS (aarch64). Build the static binary on an arm64 machine
(`scripts/build-linux.sh aarch64`; CI's release job builds `magnetar-server-aarch64-linux` on
a native arm64 runner) or download it from the release with `SHA256SUMS`, then:

```sh
install -m 755 magnetar-server-aarch64-linux ~/.local/bin/magnetar-server
mkdir -p ~/.config/magnetar && umask 077
cat > ~/.config/magnetar/worker.env <<'EOF'
MAGNETAR_WORKER_SERVER=https://<server>.<tailnet>.ts.net
MAGNETAR_WORKER_TOKEN=<the rpi token>
# RPI profile (§3)
MAGNETAR_DHT=aggressive
MAGNETAR_DHT_QPS=400
...
EOF
```

`packaging/worker.env.example` has the full RPI profile. Then choose one of:

**A. Plain background process** (nothing installed):

```sh
set -a; . ~/.config/magnetar/worker.env; set +a
nohup ~/.local/bin/magnetar-server worker >> ~/.local/state/magnetar-worker.log 2>&1 &
# or inside tmux / screen: tmux new -d -s worker '~/.local/bin/magnetar-server worker'
kill -TERM <pid>   # clean stop: leases released, state saved, report printed
```

It does not come back after a reboot or a crash.

**B. systemd service** (`packaging/magnetar-worker.service`): a dedicated system user, the
settings in `/etc/magnetar/worker.env` (mode 600), state in `/var/lib/magnetar-worker`
(`StateDirectory`), restart on failure but not on a refusal by the server (exit 2), and
hardening (`NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`, no capabilities, private
`/tmp`, `MemoryHigh=220M`, `MemoryMax=300M`):

```sh
sudo install -m 755 magnetar-server-aarch64-linux /usr/local/bin/magnetar-server
sudo useradd --system --no-create-home --shell /usr/sbin/nologin magnetar-worker
sudo install -d -m 755 /etc/magnetar
sudo install -m 600 -o root -g root packaging/worker.env.example /etc/magnetar/worker.env
sudoedit /etc/magnetar/worker.env          # server URL + the rpi token
sudo install -m 644 packaging/magnetar-worker.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now magnetar-worker
journalctl -u magnetar-worker -f
```

The environment file is read by systemd as root before the service user starts, so it can be
600 root. `systemctl stop` sends SIGTERM: the worker releases its leases, saves its state and
prints its report to the journal. To remove: `systemctl disable --now magnetar-worker`, delete
the unit, `/etc/magnetar/worker.env`, `/var/lib/magnetar-worker` and the user.

## 6. Measured

**Live run, 2026-10-27, laptop** (release build; main server from the branch on an empty
scratch database with its own Nostr crawler and seed checks, its DHT off; one worker process
on the same line, 600 s, `MAGNETAR_DHT=aggressive` on UDP 27891–27894, KRPC 300 q/s, TCP
150/s ≤ 400 open, health probe on, pool 100 000, 32 fetches, 4 MiB dicts, RSS ceiling 200 MB,
upload buffer 8 MB, seed-check leases of 300; the production crawler ran alongside on the
same line, so the rates were kept gentle):

| | |
|---|---|
| seed checks by the worker | **7 685** in 49 leases, all accepted (0 ignored, 0 errors); the server's own checker did 3 045 meanwhile (12 batches of ≤ 500); never the same infohash twice, 0 leases left at the end |
| DHT | 11 098 new hashes triaged at the server (8 already known), 4 724 collected, 3 752 with peers, 1 250 dicts verified, **1 239 stored** by the server (213 classified adult by the server), 11 stopped by the worker's blocklist and recorded as hashes only (counted by tier, never viewed), 0 rejected, 0 upload errors (235 uploads) |
| rates | 158 KRPC q/s average (sampling paused 587 s of 600 by the queue high-water mark, as on the server, D62), answered 53 %; 86 MB TCP in, 14 MB UDP in / 10 MB out |
| router | probe baseline 14 ms, 0 failures, 0 brakes; `time_connect` to 1.1.1.1 every 15 s: max 22 ms |
| worker resources | RSS peak **105 MB** (86 MB at the end), 32.8 s CPU in 600 s (5.5 % of a core), governor never under pressure |

What the Pi will see is not measured: its CPU is slower (classification and the blocklist run
on the worker, once per verified dict) and its conntrack table smaller; the governor and the
health probe are the safeguards.

## 7. Not done / not verified

- Nostr relay crawling on workers (D99), header probes (not built), UPnP by default.
- Hashes leased to a worker that dies come back through the server's DHT retry loop only when
  the server's own DHT crawler runs; otherwise they wait until sighted again after the lease.
- A worker trusts the server it is pointed at (TLS checks the name); the server trusts a
  worker's seed-check numbers and failure statuses (they only move schedules), never its
  content claims.
