# Benchmarks

Raw data and reports from every measurement run on Magnetar's crawler and the machines it runs on,
newest first. Each folder keeps the scripts, per-run logs and summaries as they were produced; the
headline numbers and conclusions also live in the docs named below.

What was removed before committing (the repo is public):

- every IP address (replaced with `<ip>` / `<ip6>`), except the public resolvers used as latency
  probes (1.1.1.1, 9.9.9.9) and localhost;
- raw packet captures (`*.pcap`), raw node/peer lists (`nodes.json`, `*.tsv`) and any file holding
  real torrent infohashes;
- binaries, `perf` data files, test-suite logs and sample `.torrent` files.

The raw captures and node lists have since been deleted; the other excluded files stay only on
the machine that produced them.

| Folder | Date | What | Headline | Docs |
|---|---|---|---|---|
| `2026-10-28-dht-nodes-4-vs-16/` | 2026-10-28 | Production A/B: 4 vs 16 DHT nodes, fiber, 5,000 q/s | 16 nodes stored 61–63 % more (12,667 vs 7,805 per 10 min) for +0.2–0.4 core and ~0.6 GB | its `README.md` |
| `2026-10-28-dht-multicore/` | 2026-10-28 | D106 multi-core engine: `perf` profile, loopback bench, 17 live runs on fiber | No single-core bottleneck; a pacer bug wasted booked slots. New engine sends 100 % of its cap; at TCP 2,000/s ~6,100–6,200 metadata/min vs ~5,500–5,600 for main (empty database) | DHT.md §20, D106 |
| `2026-10-28-dht-balancer/` | 2026-10-28 | D107 sampling/lookup balancer, 10 interleaved runs on fiber | Parity with main within noise (scratch databases); sampling is ~1 % of queries either way | branch `dht/balance` (not merged) |
| `2026-10-27-dht-lookup-ranking-prod/` | 2026-10-27 | Production A/B of D95 lookup ranking (6 interleaved runs) | +7 % stored at 2,048 lookups, +11 % at 4,096 | DHT.md §19.3, D95 |
| `2026-10-26-dht-peer-reuse/` | 2026-10-26 | Can peers already crawled be reused for more fetches? | No: 0.07 % hit rate. Bottlenecks were the 4 s query timeout and a slow status UPDATE; fake peers echo any hash | DHT.md §18 |
| `2026-10-26-router-laptop/` | 2026-10-26 | How hard can the laptop crawl through the Neufbox? | Router limits for the home line (latency, NAT table) | `docs/research/router-bench.md` |
| `2026-10-26-router-eee/` | 2026-10-26 | Same, from EEE on its own line | KRPC clean to 1,600 q/s; TCP knee 400–500/s; worker defaults 1,000 q/s, TCP 300/s, 350 open | WORKERS.md |
| `2026-10-26-router-rpi/` | 2026-10-26 | Same, from the RPI (run 2 after raising its conntrack table) | Pi's own conntrack (7,680) was the limit; KRPC clean to 800 q/s; worker defaults 400 q/s, TCP 150/s, 300 open | WORKERS.md |
