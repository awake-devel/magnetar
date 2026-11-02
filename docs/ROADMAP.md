# Roadmap

Work that is agreed but not started yet. Items move to ARCHITECTURE.md (as decisions) and the
release notes when they are built.

## Planned

### Find more Nostr relays through certificate transparency
Search the public certificate transparency logs (crt.sh or a log mirror) for new hostnames that
look like relays (`relay.`, `nostr.`, `nos.`, `wss.`, …). Confirm each candidate with a single
NIP-11 request (`GET` with `Accept: application/nostr+json`): a relay answers with its JSON
description; anything else is dropped, and relays that require payment or authentication are
skipped. Confirmed relays go to the existing relay crawler with a new discovery source ("CT log"),
so the "where discoveries come from" card and the crawl map show whether it pays off. No IP-range
scanning: relays sit behind hostnames (SNI / virtual hosts), so scanning addresses would miss them
and would look like an attack from the home line. Cheap extra sources: relay lists published by
monitors such as nostr.watch, and relay lists in popular clients' public code.

### Header probes
Check a torrent's contents block by block from its peers: verify that it really contains what
its file list says. Never on torrents tagged adult or matched by the blocklist. Warn when no VPN
is active, since probing connects to peers from the server's IP. Per-machine limits for the
crawl workers come from the router benchmarks (docs/benchmarks/).

Status: the detector is done (D116, `crates/sniff`: file types by content, console game ids,
video and audio recon from sparse byte ranges). Left: (C) the probe engine in the worker
(piece planner, message whitelist, `probe_attrs` / `probe_files` storage, the opt-in upload
below, a trial on eee with both upload settings), then (D) games enrichment keyed by the ids
`sniff` reads (GameTDB files after GameTDB answers about automated downloads; Wikidata).

**Legal note, required with the feature (README "Legal" and the feature's doc).** Today
Magnetar only handles metadata: info dicts (names, sizes, hashes), never file data. Header
probes download pieces of the files themselves, so the legal position becomes blurry and the
README must say so when they ship. The user's point for France: what is enforced is
*sharing* (Arcom's graduated response, formerly HADOPI, acts on uploads seen in P2P swarms).
Downloading a fragment to learn what a torrent contains is not the targeted act, but it is not
explicitly lawful either: copying from an unlawful source is not covered by the private-copy
exception (CJEU C-435/12 *ACI Adam*, 2014), so do not write that probing is legal. What the
design must guarantee by default so a probe never shares: no piece data sent (never unchoke,
`HAVE_NONE` instead of a bitfield of real pieces, not announced as a seeder), only the bytes
needed for the check, and nothing kept after it. Protocol traffic (handshakes, requests) is not
sharing. An opt-in setting (off by default) may serve the few whole, hash-checked pieces a probe
holds to leechers, to get unchoked faster; its doc and the README must say that turning it on
*is* sharing. Decision (user, 2026-11-02): the upstream default stays off; the user's own
workers will run with it on after the eee trial measures both. **UNVERIFIED legal reading, not legal advice**; other countries differ
(Germany, for example, fines downloads too).

### DHT engine: collapse at extreme settings
At 8 nodes, 10,000 q/s and 8,192 lookups, both the D106 engine and the code before it sometimes
fall to about 20 % of their query cap, with the transaction map full (`busy`). Production runs far
below this (16 nodes, 5,000 q/s, 4,096 lookups). Find out why before raising limits further
(docs/DHT.md §20.5).

## Low priority

### App: stop infinite scroll at the search offset cap
Since D110 the server answers 400 above `offset` 10 000 (`query::MAX_OFFSET`). The app's
infinite scroll asks for `offset = hits shown`, so a list scrolled past 10 000 hits (200 pages
of 50) shows an error message instead of more hits. The app should stop loading at the cap and
say "refine the search" instead.
