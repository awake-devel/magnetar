# DHT metadata throughput research — 2026-10-26 (laptop on ONT, public IP)
Raw data and tools: this directory (experiment.log, results/*.jsonl, exp.py, pex.py, source.py, analyze.py, *.pcap).

Bottleneck: lookup LATENCY. Collectors run one lookup at a time; median lookup = full 10 s budget because unanswered
queries (35–52 %) hold a slot for QUERY_TIMEOUT 4 s (core/src/dht/engine.rs:33). Answer times p50 100 ms, p90 278 ms,
p99 559 ms; only 0.25 % > 1 s. ⇒ ~50–70 lookups/s ≈ 3.1–4.3k hashes/min collected vs 6–9k discovered.
Side stall: store::mark_failed single-row UPDATE takes 1.1–1.2 s inside the collector.

Known-good peer reuse (user's idea): 18,705 handshakes, real hit 0.068 % per pair (0.22 % for multi-swarm peers);
~1,465 TCP attempts per success ⇒ +16–37 metadata/min max. Don't build. 9 "accept-all" fake peers (peer id cf-aa84-,
also -XT Xunlei) echo any hash but never send the extension handshake: ~10 % of accepted handshakes — skip them 1 h.
announce_peer: 23/min incoming; BUG: announce dropped by triage seen filter after the announcer's own get_peers
(0 rows via=announce) ⇒ fix in server/src/dht/mod.rs triage_loop (+3–5/min).
Incoming get_peers querier as speculative peer: 0.87 % real ⇒ ~+17/min, scan-like connection attempts.
PEX: same-torrent only, ~0.9 peers/session ⇒ skip.

Ranked: (1) 1–1.5 s per-query lookup timeout + branch widening (engine.rs, lookup.rs) with (2) PEER_LOOKUPS 1024–2048 and
(3) batch mark_failed out of the collector ⇒ ~8–8.6k collected/min ⇒ ~2,000–2,600 metadata/min (KRPC 3,000 q/s ceiling).
(4) querier-as-peer +15–20, (5) announce bug +3–5, (6) stream peers to fetcher (latency), (7) skip fake peers,
(8) expose from_source/looked_up/incoming counters in /api/v1/stats.
