# 2026-10-31: worker router ramps and the collector cap (D114)

Two crawl workers, each on its own home line with a consumer router: **eee** (x86_64, 4 cores,
7.7 GB, FTTH) and **rpi** (Raspberry Pi 3B, 905 MB, a different ISP line). The real worker
(`magnetar-server worker --seconds 300`, v0.3.1) ran 5-minute steps under `worker_bench.py`,
which probes every 2 s and kills the worker at the first fault: gateway TCP:80 fails 3 times in
a row; 1.1.1.1:443 and 9.9.9.9:443 both lost in ≥ 3 of the last 6 probes; 1.1.1.1 median over
max(3 × baseline, baseline + 40 ms); gateway median over max(5 × baseline, baseline + 10 ms);
local conntrack over 90 %; default route change. It never retries a step.

## 1. Query rate ramp (`*/ramp.jsonl`)

`MAGNETAR_DHT_QPS` 25 → 1300, everything else default (64 collectors). **No fault on either
line**, but the real send rate stopped growing at ~157–180 q/s from the 200 setting on, and
stored metadata plateaued at ~600 per 5 min: sampling was paused by queue backpressure most of
the time and the 64 peer collections (~8.6 s each) were the cap, not the query budget.

## 2. Fetch ramp (`eee/fetch.jsonl`, aborted)

Raising `MAGNETAR_DHT_TCP_RATE` changed nothing: only ~40 TCP attempts/s were made against a
150/s cap. Aborted once the collector cap was identified.

## 3. Collector ramp (`*/lookups.jsonl`)

Steps are `PEER_LOOKUPS/TCP_RATE/FETCH_CONCURRENCY/TCP_MAX`.

| host | step | stored / 5 min | real q/s | TCP attempts/s | note |
|---|---|---:|---:|---:|---|
| eee | 128/150/128/350 | 1 093 | 420 | 77 | |
| eee | 192/150/160/400 | 1 685 | 475 | 108 | |
| eee | 256/200/192/450 | 2 108 | 607 | 136 | **kept** |
| eee | 384/300/256/500 | 2 734 | 837 | 199 | max without fault |
| eee | 512/400/320/600 | — | — | — | fault after 75 s: 1.1.1.1 + 9.9.9.9 lost 3/6 |
| rpi | 128/150/64/350 | 1 196 | 348 | 75 | **kept** (CPU 42 %) |
| rpi | 192/150/96/400 | 1 487 | 476 | 105 | CPU 53 % |
| rpi | 256/200/128/450 | 1 931 | 591 | 126 | CPU 67 %, max without fault |
| rpi | 384/300/160/500 | — | — | — | fault after 47 s: gateway TCP:80 failed 3× (router ~1 min to recover) |

Each lookup took 8.5–8.9 s at every step, so stored metadata grows linearly with collectors
until the line breaks, and each collection costs ~2.35 real q/s. The production settings are
about two thirds of the last clean step (eee 256 ≈ 420 stored/min, rpi 128 ≈ 240/min).

## Conclusion

The fixed aggressive default of 64 collectors silently capped any `MAGNETAR_DHT_QPS` above
~180. D114 derives it from the query rate: 0.4 per q/s, 8–4096 (200 q/s → 80, 640 → 256, the
ONT runs' 5 000+ → 2 000–4 096). An explicit `MAGNETAR_DHT_PEER_LOOKUPS` still wins.

Files: `worker_bench.py` (the harness), `*/ramp.jsonl`, `eee/fetch.jsonl`, `*/lookups.jsonl`
(baseline, 30 s step samples, pass/fault records). Worker logs are kept off the repo (they list
peer addresses).
