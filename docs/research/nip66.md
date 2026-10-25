# NIP-66 relay discovery: spec and live data (2026-10-25)

Backs D30. The spec is https://github.com/nostr-protocol/nips/blob/master/66.md (status
`draft optional relay`), fetched the same day. The measurements come from throwaway
Node/Python WebSocket probes run against public relays on 2026-10-25 around 09:00 UTC. They
are not in the repo. The samples are in `fixtures/kind30166_sample.jsonl` (39 events) and
`fixtures/kind10166_sample.jsonl` (4 announcements). Every sample event verifies (id and
BIP340 sig, `crates/core/tests/nip66.rs`).

## 1. Spec summary

- **kind 30166**: relay discovery. It is addressable, so a relay keeps one event per (monitor
  pubkey, `d`). The only required tag is `d`, which holds the relay's normalized URL, or a hex
  pubkey for relays that have no URL. Optional tags:
  - `rtt-open`, `rtt-read` and `rtt-write`, in milliseconds
  - `n`, the network: `clearnet`, `tor`, `i2p` or `loki`
  - `T`, the relay type in PascalCase
  - `N`, one tag per supported NIP
  - `R`, NIP-11 limitations such as `auth`, `writes`, `pow` and `payment`. A false value is
    written `!auth`.
  - `t` (topic), `k` (accepted kinds, `!` = not accepted) and `g` (geohash)

  `content` may hold the NIP-11 document.
- **kind 10166**: monitor announcement (replaceable). It carries `frequency` in seconds,
  `timeout`, `c` (the checks: open, read, write, nip11, dns, geo, ssl…) and `g`.
- **Risk mitigation** (from the spec): clients must not depend on 30166, a monitor can be wrong
  or malicious, and clients should not trust a single source.

## 2. Which relays hold monitor data

Probe: `REQ {"kinds":[10166],"limit":200}` and `{"kinds":[30166],"since":now-7d,...}`.

| Relay | 10166 | 30166, last 7 days | Notes |
|---|---|---|---|
| wss://relay.nostr.watch | 23 | 12,392 events, 1,495 distinct `d`, 57 authors | accepts limit 5000. Deep `until` pages are slow: about 100 s for one page, and one 60 s page timed out in the live test |
| wss://monitorlizard.nostr1.com | 24 | 11,113 events, 1,245 `d`, 23 authors | fast (5 s for a week) |
| wss://relaypag.es | 27 | 12,393 events, 1,472 `d`, 25 authors | clamps to 500 per page. 26 pages in 8.8 s |
| wss://nos.lol | 5 | 595 events, 440 `d`, 65 authors | carries monitor `19f3383d…`, which is missing from the three above |
| wss://relay.primal.net | 6 | 777 events, 653 `d`, 64 authors | same as nos.lol |
| wss://relay.damus.io | – | 785 events, 668 `d`, 81 authors | returned HTTP 503 on the first try and worked minutes later |
| wss://purplepag.es | 53 (mostly old) | – | CLOSED `blocked: limit too high: 5000 (max 500)`. Hence `limit` 500 |
| wss://nostr.mom | 4 | 15 | negligible |
| wss://history.nostr.watch | – | – | TLS/HTTP fails (`curl` 000), down |
| wss://relay.nostr.band | – | – | connect timeout, down |

relay.nostr.watch, monitorlizard and relaypag.es hold nearly the same set: the union of all
six sources is 13,804 distinct events. The defaults (D30) are relaypag.es and monitorlizard
(fast), relay.nostr.watch (canonical, slow), and nos.lol and relay.damus.io (for the
`19f3383d…` monitor).

## 3. Monitors

58 distinct pubkeys have a 10166 somewhere, and 123 pubkeys authored 30166 in the last week.
Seventeen of those authors are announced, and they produced 13,676 of the 13,804 events. The
106 unannounced authors have 1 to 3 events each, nearly all about a single relay, mostly
onion relays describing themselves.

| Monitor (prefix) | 30166 in 7 d | Relays | `frequency` | Newest |
|---|---|---|---|---|
| 58faef87… | 1,319 | 1,304 | 86400 | 0.3 h |
| nostr.watch fleet: 0bffd9b5, 9bb7cd94, 9ba0ce3d, 9b85d54c, 9ba046db, 9ba1d789, 9bac3d58, 9ba64840 | ~1,190 each | ~1,150 each | 14400 | 0.3 h |
| 45df0580… | 1,176 | 1,137 | 21600 | 0.3 h |
| 19f3383d… | 819 | 583 | 21600 | 0.6 h (nos.lol, primal, damus only) |
| 9bbbb845… | 771 | 703 | 3600 | 0.3 h |
| b2c949c0… | 43 | 42 | 900 | 0.3 h (sets no `n`/`R`) |
| cb9b5ea0…, db57acaa…, a4257d79… | < 15 | < 15 | 300–86400 | 0.3–19 h |

A 10166 can be months old while its author is still active: the nostr.watch fleet's
announcements are about 95 days old, but their 30166 events are minutes old. Liveness
therefore comes from 30166 recency, and the age of the 10166 is ignored.

## 4. Volume and freshness

Kind 30166 is addressable, so the relays hold only the newest event per (monitor, relay). One
week from relay.nostr.watch is 12.4k events: 10k of them are from the last 5 h and 1.7k from
the remaining 6.8 days. Distinct usable relays (announced monitors only, clearnet, not
restricted) by the age of their newest event:

| Window | Relays | After dropping auth/payment |
|---|---|---|
| 6 h | 1,240 | 1,150 |
| 24 h | 1,287 | 1,197 |
| 48 h | 1,336 | 1,244 |
| 7 d | 1,386 | 1,288 |

D30 uses 7 days. It costs about 90 extra relays compared with 24 h, some of them dead (the
crawler's backoff handles those), and it keeps relays watched only by the daily monitors. The
live run of `discover` (ignored test `live_monitors`) found 40 monitors, 13,426 events,
1,288 candidates, 98 restricted and 176 skipped in 224 s. relay.nostr.watch timed out on one
page, and the other relays covered its data.

## 5. Tags as published

Tag counts are from 5,000 events (monitorlizard):

- `d`: every host is lowercase. Of relay.nostr.watch's 1,376 distinct values, 85% end in `/`
  (nostr.watch style).
  Monitor `19f3383d…` publishes without the slash. Paths are kept (`wss://relay.og.coop/inbox`,
  `wss://mysats.xyz/nostrrelay/Relai`, mixed case in the path), and so are ports
  (`wss://o-gits.com:8443/relay`). `ws://` URLs on public IPs and ports are common
  (`ws://89.167.52.249:7000/`). No hex-pubkey `d` was seen. `normalize_relay(d, false)`
  handles all of these.
- `n`: `clearnet` (4,946), `tor` (onion URLs) and `i2p` (`.i2p` URLs; the normalizer accepts
  these hosts, so they are dropped explicitly). The tag is missing on about 1% of events,
  all from the b2c949c0 monitor, which publishes only `d` and rtt tags.
- `R`: `!pow` 4,312, `!auth` 4,147, `!payment` 4,061, `payment` 261, `auth` 175, `pow` 9,
  `writes` 2. In the union, 82 relays get a restriction from one monitor and no `R` at all from
  another. That is missing data, not disagreement, so D30 drops a relay as soon as any monitor
  says `auth` or `payment` (98 relays).
- `N`: about 9 per event. Of 13k events, 702 list NIP-50 only, 3,577 list NIP-77 only, and 520
  list both. D30 does not use `N`: the crawler sends plain REQs to public relays, and NIP-77
  is used only against our own server.
- `T`: rare (28 of 5,000 events: PublicOutbox, Community, Directory…). Not used.
- Also seen: `g` (about 11 per event), `l`/`L` (language and country labels), `rtt-*`,
  `client`, `s`, `p`, `k`, `t` and `W`. `content` averages about 490 bytes, with a maximum of
  88 KB (a NIP-11 document). That is far under the 16 MiB frame limit.

## 6. Abuse considerations

- Anyone can publish a 30166 that names any URL. D30 counts only authors with a 10166. A
  spammer can publish a 10166 too, so this filters drive-by junk, not a determined attacker.
  The other limits:
  - the normalizer refuses private, loopback and CGNAT addresses and non-ws schemes
  - at most 2,000 new rows per run
  - new relays only get exploration slots, and failures back off (up to 24 h)
  - existing rows are never changed (`upsert_relay`)
- Relays are ranked by the number of distinct monitors that report them (1,146 of 1,386 have
  5 or more), so if the cap applies, the best-corroborated relays go in first.
