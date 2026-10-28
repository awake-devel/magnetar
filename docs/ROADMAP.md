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

### DHT engine: collapse at extreme settings
At 8 nodes, 10,000 q/s and 8,192 lookups, both the D106 engine and the code before it sometimes
fall to about 20 % of their query cap, with the transaction map full (`busy`). Production runs far
below this (16 nodes, 5,000 q/s, 4,096 lookups). Find out why before raising limits further
(docs/DHT.md §20.5).
