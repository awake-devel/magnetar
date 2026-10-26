# Known limitations

Everything here was observed on live relays and trackers, or read from primary documentation,
during the research on 2026-10-25. The source note is in brackets. Items not yet checked on
real hardware are marked **UNVERIFIED**.

## Nostr data and relays

- **relay.damus.io paginates erratically.** `COUNT kinds:[2003]` reported 6,867, but `until`
  pagination returned 23 events in pages of 6, then 2, then 0, and the next connection got
  HTTP 503. The cause (rate limit or query budget) is unknown. The crawler treats damus as
  low-yield, and an empty page there does not mark history as finished. [nostr-nips.md §1.7,
  §2]
- **Pagination in general:** `until = min(created_at)` is inclusive and duplicates are
  removed. The crawler stops only on a page with zero *new* events, never on
  "page < limit". [D8]
- **relay.nostr.band is dead.** The handshake timed out on test day, and so did
  nostr.fmt.wiz.biz. They are not in the seed list. dtan still lists nostr.band as a default.
  [nostr-nips.md §2, wot-relays.md §10]
- **NIP-77 negentropy is disabled on the big relays.** damus, nos.lol, primal and nostr.mom
  answer `NOTICE ERROR: bad msg: negentropy disabled`. Crawling uses REQ pagination.
  Negentropy is only used phone to server, with a batch POST fallback after `NEG-ERR`, a
  NOTICE, or 10 s without a `NEG-MSG`. [negentropy.md §6]
- **NIP-45 COUNT is not everywhere.** oxtr, snort and nostr.wine do not support it, so it is
  used only as a hint. [nostr-nips.md §4]
- **Bridge bots dominate the volume.** Most kind 2003 events are mirrors of Nyaa (one pubkey
  per uploader group), TPB (`i tpb:`, more than 15k events) and YGG/u2p (`i ygg:`, more than
  18k events). Their metadata quality is uneven: one Nyaa `file` tag holds a whole scraped
  HTML folder listing. Bridges (events with a `proxy` tag) are exempt from the burst penalty
  but get no extra trust. To hide them, raise the trust threshold. [nostr-nips.md §2.4,
  wot-relays.md §9]
- **PoW is not quality.** The bridge bots are exactly the publishers that mine NIP-13 PoW,
  and hand-published events have 0 PoW. PoW is evidence of cost, not of quality. It is
  weighted lightly: effective bits = min(actual, committed), at most +25. [nostr-nips.md §3,
  D19]
- **Private mutes are unreadable.** NIP-51 private items are NIP-44/NIP-04 encrypted to their
  owner. Magnetar stores no nsec, so it uses only public mute tags, including those of the
  root user. A NIP-46/NIP-55 signer (Amber) could add them later. [wot-relays.md §6]
- **Oversized relay lists** (more than 50 `r` tags) are penalised and not used for discovery.
  [D10]
- **Future-dated events** are rejected, and kind 2004 comments are kept only when they
  reference a known kind 2003 event. [D7]

## Liveness

- **DHT numbers are estimates.** The BEP 33 scrape returns Bloom filters. The swarm size is
  estimated from them, saturates around 8000, and only covers DHT peers. In live tests, a
  torrent with 491 seeders and 256 leechers on its tracker gave DHT estimates of about 97
  seeds and 811 peers. Seeds are undercounted because many clients never announce `seed=1`.
  DHT is used only when no tracker answered, and it fills `dht_peers_est`, never
  `seeders`. [bittorrent.md §4]
- **Unknown is not dead.** No answer means unknown. "Hide dead" drops a torrent only when it
  has a *fresh* liveness result with 0 peers. On the phone the TTLs are 6 h for alive, 24 h
  for dead and 1 h for errors [D16, D17]; the server re-checks by age (6 h for a week-old
  torrent, up to 4 days for old ones, 7 days after 3 dead checks in a row), so an old
  torrent's seeders can be days old (docs/LIVENESS.md, D45).
- **The DHT is polluted.** Node farms answer every lookup with made-up peers; Magnetar ignores
  nodes on an IP seen with 3+ ports in one walk and ids made for the target. A farm that
  spreads over many IPs would still make a dead torrent look alive. [D45]
- **Fallback trackers only prove life.** Torrents without trackers are scraped on the magnet
  default trackers; zero there means "not registered", so the DHT decides between dead and
  unknown. [D45]
- UDP scrape batches are 74 infohashes, and a truncated reply counts as unanswered.
  [bittorrent.md §1]

## Android

- **Android 15+ caps `dataSync` foreground services at 6 hours per 24 hours**, shared by all
  of the app's dataSync services. At the cap, phone downloads pause, save their state and
  notify. A dataSync service cannot start from `BOOT_COMPLETED`, so downloads never resume
  at boot. Start them again by tapping. Use server downloads for long transfers.
  [android.md §6]
- **librqbit's HTTPS tracker requests may fail on the phone.** Its reqwest uses rustls
  platform-verifier, which needs JNI initialisation on Android. UDP trackers and DHT still
  work. Our own HTTP code always uses ring with webpki-roots. [D28]
- **Phone downloads on arm64 are UNVERIFIED.** librqbit downloaded a real torrent on the
  x86_64 API 37 emulator only. If the engine fails, the app opens the magnet in another
  client. [android.md §4]
- **Background crawl only runs while unmetered and charging**, once an hour. The phone crawl
  is also bounded: at most 8 relays and a time budget.
- **Plain `http://` server URLs are accepted, with a warning** unless the host is loopback, a
  MagicDNS `*.ts.net` name or a Tailscale IPv6 address (`fd7a:115c:a1e0::/48`), which only
  resolve or route through the tunnel. On any other network the token and the results travel
  readable: use HTTPS (`tailscale serve` or a reverse proxy). A Tailscale IPv4 address
  (`100.64.0.0/10`) gets its own warning: that range is also carrier-grade NAT (RFC 6598), so
  with Tailscale off on the phone the request goes over the carrier's network in clear; the
  check sees the address, not whether the tunnel is up. The warning cannot tell a short MagicDNS name (`http://nas:8787`) from a LAN
  name, so it warns for those too. [D39] **UNVERIFIED:** whether Android 16+ local-network
  permission rules affect LAN or Tailscale addresses.
- **ntfy click actions:** whether ntfy-android opens `magnet:` or `magnetar://` links is
  **UNVERIFIED**. [termux.md §4]

## Server deployment

- **The token is the only lock.** Every endpoint except `/health` needs it, but anyone who
  has it, or can read the server's `.env`, has full API access. The default publish address
  is loopback; expose the server through HTTPS (docs/DEPLOY.md). [D20, D39]
- **arm64 is UNVERIFIED on hardware.** The arm64 image and the `aarch64` binary are built on
  GitHub's native arm64 runners and smoke-tested there (`--version`); they have not run on a
  Raspberry Pi or an ARM NAS. ARM Synology models are not a target, and 32-bit ARM has no
  build. [D39]
- **The CI workflows have not run yet** at the time of writing: they were checked with
  actionlint only, since GitHub Actions cannot run locally. [D39]
- **No service manager integration.** Nothing installs systemd units, timers or udev rules.
  Docker's restart policy keeps the containers running; for the native binary, starting it
  at boot is up to you (docs/LINUX.md has examples to adapt).
- **The phone is not a server any more.** The Termux setup of v0.1.0 was removed (Android's
  phantom process killer and battery management made it fragile). [D39]

## Search

- Postgres uses the `simple` config for titles and file names and `english` for descriptions,
  with a `pg_trgm` fallback. SQLite FTS5 uses `unicode61 remove_diacritics 2` with `._-[]()`
  as separators. Phone and server rankings are therefore similar but not identical.
  [D11, D12]
