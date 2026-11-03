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
its file list says. Never on torrents matched by the blocklist (adult and BEP 27 private
torrents are probed, D119). Warn when no VPN
is active, since probing connects to peers from the server's IP (as built: probes run on the
crawl workers only, which log this at start; no VPN detection yet). Per-machine limits for the
crawl workers come from the router benchmarks (docs/benchmarks/).

Status: the detector is done (D116, `crates/sniff`: file types by content, console game ids,
video and audio recon from sparse byte ranges). (C) is built (D117, docs/PROBES.md): the probe
engine on the workers (piece planner, message whitelist, streaming SHA-1, the opt-in upload
below), results uploaded with the dict and stored in `probe_attrs` / `probe_files`, shown in
the detail API and the app's "Detected" card (D121), searchable (D122). Left:
- **next: the eee trial** with both upload settings (off, then on), under the router
  watchdog, measuring unchoke share, bytes and packets per probe, verified share and peer
  reactions; then the Pi. That is how the production switch (user decision: on) is
  recommended to happen, docs/PROBES.md §8;
- a VPN check (the backfill of rows stored before C and of Nostr-only rows is built: D118,
  docs/PROBES.md §10; the probe search filters and facets too: D122, docs/FILTERS.md);
- (D) games enrichment keyed by the ids `sniff` reads (GameTDB files after GameTDB answers
  about automated downloads; Wikidata).

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
*is* sharing. Decision (user, 2026-11-02): the upstream default stays off; production (the
user's workers) runs with it on. Not switched on yet: the workers' env files are outside the
repository and no agent may start peer traffic (docs/PROBES.md §8). **UNVERIFIED legal reading, not legal advice**; other countries differ
(Germany, for example, fines downloads too).

### Track kinds: what D123 left out
D123–D126 read subtitle and audio kinds (SDH, forced, signs, commentary, audio description,
dub, …) from bytes a probe already fetches (docs/PROBES.md §11). Deferred, with why:
- **CEA-608 / 708 for H.264 / MPEG-2 in Matroska and MP4**, and AV1 T.35: one 16 KiB block per
  file at the first video sample. Measure the hit rate on real WEB-DL and HDTV releases first.
- **AC-3 bsmod in Matroska**: one block per AC-3 track (the frame is not read when the header
  gives the channels); the research expects bsmod 0 on almost all film tracks.
- **DTS ExSS asset type**: the value table (ETSI TS 102 114 §7.4) was not verified.
- **DVD IFO** audio and subpicture `code_extension` (forced, captions, director's comments,
  visually impaired) and **Blu-ray clpi / mpls** languages plus the secondary-audio hint: new
  parsers.
- **ASS style names** ("Sign", "Song", "OP", "ED") as a signs heuristic.
- **Torrent-name and external sidecar-file kinds** in `release.rs` / `torrent_attrs` (D44):
  hardsub / SUBBED / HC, DUBBED, Lektor, MVO / DVO, AD / DVS, `.sdh.srt`, `.forced.srt`,
  `Subs/N_English.srt`, the missing sidecar extensions, German DL / ML, NORDiC / SWESUB…; new
  name facets and a `PARSER_VERSION` bump. The classifier (`sniff::video::classify_name`) is
  pure and dependency-free, but core uses sniff only behind the `probe` feature: reusing it
  means sharing `kind.rs` with core.
- **Original vs dub from TMDB `original_language`**: that field is not stored today.
- **Language variants** (pt-BR, es-419, fr-CA, zh-Hans / zh-Hant, sr-Latn) from names and
  BCP 47.
- **Re-probe version 1 rows for kinds**: costs peer traffic; needs the user's decision and the
  D109 watchdog rules.
- **Server-side re-classification of stored names** when the classifier changes.
- **The rest**: MKV FlagEnabled, MP4 `txas` / `folw` / `alternate_group`, ATSC 0xCC, DVB EIT
  component_descriptor, SCTE-27, per-sample forced (PGS, VobSub, `frcd`), SDH detected from
  subtitle content, Ogg / ASF / FLV roles, and a sign-language video kind (DASH `sign`).

### Track corrections: what D128–D131 left out

- Codec, resolution and HDR corrections: the user corrects only what VLC shows per track
  (language, kinds, name, missing or extra tracks); machines detect the rest better.
- An offline correction queue on the phone: corrections are rare and the open sheet allows a
  retry, so a failed send is reported, not queued.
- Corrections in the result list's badges (only the detail's Detected card shows them).

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
