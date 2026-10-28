# 4 vs 16 DHT nodes, production, direct fiber (2026-10-28)

Production server and database (≈3.5 M torrents), D106 engine (`f829573`), direct fiber, crawl
workers running on their own lines. Settings for every run: QPS 5000, PEER_LOOKUPS 4096,
FETCH_CONCURRENCY 2048, TCP_RATE 2000, TCP_MAX 8000, 24 peers per hash, lookup timeout 4 s, seed
checks at DHT concurrency 256 / batch 5000. Only `MAGNETAR_DHT_NODES` changed (UDP 6891–6906
published). Four interleaved 10-minute runs, one sample every 2 minutes; raw samples in
`runs.jsonl` (script: `nodes-ab.sh`).

| start (UTC) | nodes | stored after ~10 min | metadata/min (last samples) | answer share | CPU (cores) | RSS at end | probe brake samples |
|---|---|---|---|---|---|---|---|
| 07:48 | 4 | 8,575 | 835–1,079 | 0.47 | 0.97–1.20 | 1.8–2.4 GB | 2 of 5 |
| 08:01 | 16 | **14,012** | 1,266–1,508 | 0.48–0.54 | 1.15–1.36 | 3.1 GB | 2 of 5 |
| 08:12 | 4 | 7,034 | 757–1,003 | 0.45–0.49 | 0.82–0.97 | 2.5 GB | 0 of 5 |
| 08:24 | 16 | **11,321** | 988–1,646 | 0.46–0.52 | 1.12–1.52 | 2.6 GB | 2 of 5 |

- **16 nodes stored 61–63 % more** than 4 in both pairs (means 12,667 vs 7,805).
- Cost: about +0.2–0.4 of a core and up to ~0.6 GB more memory. Conntrack stayed under 300 k of
  4 M; connect time to 1.1.1.1 stayed at 14–18 ms.
- The DHT health probe brake (halves query and connection rates for 5 min) tripped in both
  configurations, and the query rate often fell from 5,000 to 2,500–3,500 q/s. Metadata/min is
  well below this morning's ~2,400 at 4 nodes, when the seed checks were still at concurrency 32:
  the seed checks at 256 share the line and likely trigger the brake. Not isolated in this run.
- Each restart begins with a burst of samples (up to 277 k hashes/min in the first two minutes),
  which fills the fetch queue (up to 100 k); both configurations restart the same way.
- In the 08:12 run, `stored` did not move between the last two samples (7,034 → 7,034); not
  investigated.

Decision: production on the fiber line runs 16 nodes from 08:36 UTC.
