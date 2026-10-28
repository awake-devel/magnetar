# RPI router benchmark, run 2 (nf_conntrack_max 65536). 2026-10-26, 17:50 to 18:20

Setup: Pi 3B, eth0 100 Mbit, gateway <ip>. Same scripts as run 1 (backed up in `run1/`) with these changes:
- conntrack max is now read live
- abort when the Pi's conntrack is over 90% full
- `vcgencmd get_throttled` sampled every 3 s
- eth0 pps and Mbit/s logged
- at most 3 DHT nodes and 3 peers per IP, so no IP gets more than about 1 q/s
- logs tagged `_r2` / `_combo_*`

Idle re-check (`idle_r2.log`) matched `baseline.json`:

| Probe | Idle median |
|---|---|
| TCP 1.1.1.1:443 | 6.4 ms |
| TCP 9.9.9.9:443 | 5.7 ms |
| Gateway TCP:80 | 1.1 ms |
| ICMP | 6.2 ms |

## Verdict
**The router is now the limit, at about 3000 packets/s out from the Pi (about 4500 pps in total).** The 100 Mbit link and the Pi's CPU are not the limit at that point:
- **Traffic volume:** only 3 to 4 Mbit/s at the knee.
- **Pi CPU:** the Python bench used 68% of one core at the DHT knee.
- **Where the delay shows:** the router's own TCP:80 answer time rises first, then new flows to 1.1.1.1 / 9.9.9.9 slow down. ICMP stays flat, which also rules out the bench's own Python threads being the cause.
- **The same pps knee every time:**

| Load | Packets/s out | Result |
|---|---|---|
| DHT 3000 q/s | ~3000 | degradation starts |
| DHT 3500 q/s | ~3500 | collapse, ~150 ms |
| TCP 800/s | 3161 | collapse |
| DHT 2000 + TCP 400 | 3187 | collapse |
| DHT 1600 + TCP 300 | 2892 | degradation starts |

Other results:
- **Kernel messages:** no new dmesg lines in run 2, so no "table full" (run 1 had them).
- **Pi conntrack:** never full. It did reach 58.2k of 65536 (89%) in the combined tests, because unanswered SYNs stay in SYN_SENT for 120 s and the DHT working set is about 40k nodes. That is the next Pi-side limit.
- **Power:** `vcgencmd get_throttled` was 0x50000 before and after. Those are sticky bits from before this run (under-voltage and throttling happened at some point since boot). The current bits were 0x0 in every sample of every step, so there was **no under-voltage during the test**. The kernel log shows no voltage messages since 17:50.
- **Recovery:** link health returned to baseline at the first check, about 4 s after every step, including the three collapses. Conntrack drains in about 30 s for UDP and up to 120 s for TCP (SYN_SENT).

## 1. DHT KRPC find_node, 45 s steps, pool of 40k distinct nodes
Latencies are median / p95 in ms. CPU is the Python process as % of one core.

| q/s sent | Reply rate | TCP 1.1.1.1 | Gateway TCP:80 | ICMP | CPU | Max core % | Pi conntrack max | Result |
|---|---|---|---|---|---|---|---|---|
| 400 | 0.75 | 6.4 / 7.5 | 1.2 / 1.8 | 6.2 / 7.0 | 28 | 31 | 16k | ok |
| 800 | 0.72 | 6.4 / 7.3 | 1.1 / 2.0 | 6.1 / 7.4 | 37 | 44 | 29k | ok |
| 1200 | 0.69 | 6.4 / 7.8 | 1.3 / 2.4 | 6.3 / 7.6 | 43 | 46 | 40k | ok |
| 1600 | 0.66 | 6.6 / 8.5 | 1.3 / 3.2 | 6.5 / 7.7 | 50 | 54 | 41k | ok |
| 2000 | 0.62 | 6.9 / 9.1 | 1.6 / 3.9 | 6.7 / 7.9 | 56 | 60 | 41k | ok |
| 2500 | 0.59 | 7.5 / 11.5 | 2.1 / 5.3 | 6.7 / 8.0 | 62 | 68 | 41k | ok, slight |
| 3000 | 0.56 | 12.8 / 64 | 6.3 / 56 | 6.9 / 8.2 | 68 | 73 | 41k | **knee** |
| 3500 | 0.49 | 157 / 180 | 143 / 174 | 7.1 / 12.8 | 59 | 76 | 41k | **ABORT after 6 s** |

- **RAM:** at least 525 MB MemAvailable in every step.
- **Load and drops:** load1 stayed at 0.75 or below, and there were no UDP buffer errors.
- **Reply rate:** it falls with the rate because each node is re-asked more often (every ~13 s at 3000 q/s), not because packets are lost.

## 2. New TCP attempts (BEP 3 handshake to Linux-ISO peers, 45 s steps)

| Attempts/s | Connect OK | TCP 1.1.1.1 | Gateway TCP:80 | CPU | Max core % | Pi conntrack | Packets/s out / in | Result |
|---|---|---|---|---|---|---|---|---|
| 150 | 0.25 | 6.4 / 7.5 | 1.1 / 1.8 | 29 | 35 | 7k | 646 / 272 | ok |
| 300 | 0.26 | 6.5 / 9.6 | 1.2 / 3.9 | 44 | 50 | 17k | 1250 / 510 | ok |
| 450 | 0.26 | 6.8 / 12.9 | 1.7 / 7.9 | 64 | 73 | 29k | 1875 / 754 | ok, slight |
| 600 | 0.26 | 7.3 / 16.6 | 2.0 / 9.7 | 82 | 91 | 22k* | 2488 / 1001 | **knee** |
| 800 (782 achieved) | 0.28 | 16 / 118 | 8.1 / 119 | 93 to 100 | 100 | 15k* | 3161 / 1223 | **ABORT after 16 s** |

\* The step started after the 90 s conntrack drain wait.

At 800/s the Python process saturated one core, so the 800/s target was not fully reached. The router degraded at the same point (ICMP stayed flat at 7.5 ms). Each attempt costs about 4.2 packets out because unanswered SYNs are retransmitted.

## 3. DHT and TCP at the same time (two processes, 60 s)

| Combination | TCP 1.1.1.1 | Gateway TCP:80 | Total CPU / per process | Pi conntrack max | Packets/s out / in | Mbit/s out / in | Result |
|---|---|---|---|---|---|---|---|
| DHT 2000 + TCP 400 (first try) | 120 / 165 | 106 / 158 | 34% / 60% each | 38k | 3187 / 1647 | 3.0 / 3.9 | **ABORT after 12 s** |
| DHT 1200 + TCP 250 | 6.9 / 11.0 | 1.9 / 6.4 | 28% / 51% and 47% | 54.8k | 2298 / 1278 | 2.1 / 3.0 | ok, slight |
| DHT 1600 + TCP 300 (one step higher) | 9.6 / 18.0 | 4.8 / 13.4 | 32% / 59% and 56% | 58.2k (89%) | 2892 / 1572 | 2.7 / 3.7 | **knee** |

RAM was at least 498 MB available in all three.

## 4. Held connections (open rate 120/s, hold until 45 s)

| Target | Peak held | Lowest held in last 15 s | OK / attempted | Peer closed | CPU | TCP 1.1.1.1 |
|---|---|---|---|---|---|---|
| 300 | 302 | 287 | 681 / 2908 | 385 | 15% | 6.5 / 7.4 |
| 500 | 366 | 351 | 879 / 3597 | 522 | 18% | 6.5 / 7.3 |
| 800 | 393 | 383 | 916 / 3598 | 527 | 18% | 6.5 / 7.4 |

The ceiling comes from the peers, not from the router or the Pi. The pool is about 6300 peers across 9 swarms (5313 entries after the per-IP cap). About 25% of them accept a connection, and they close it after the handshake. Link health was unchanged throughout.

## Recommended worker defaults (~30% under the knees)
- **Budget:** at most **2000 packets/s out** in total (the knee is about 3000), and Pi conntrack at most **45k** (70%).
- **KRPC:** **2000 q/s** if DHT runs alone. **1000 q/s** when TCP runs at the same time.
- **TCP attempts:** **400/s** alone. **200/s** combined with DHT (1000 + 200 ≈ 1900 packets/s out, about 18k SYN_SENT entries).
- **Concurrent TCP:** **300**. 393 held caused no degradation, but anything above that is unverified because of the peer limit.
- **Header-probe swarms:** **12** in parallel, each with at most about 25 connections, sharing the 200 attempts/s budget.
- **DHT working set:** at most about 25k distinct nodes per 2 min, with each node asked at most once per 30 s.
- **RAM:** worker at most **200 MB**. Throttle when MemAvailable drops below 250 MB (it is about 560 MB at idle, and the bench used at most about 60 MB).
- **CPU (Python):** about 50% of one core at the recommended combined rates. One core is used up at about 800 TCP attempts/s or about 4000 KRPC q/s. Split DHT and TCP into separate processes or tasks.
- **Back-off signals:**
  - **Gateway TCP:80 connect:** median over 3 ms (baseline 1.1) or p95 over 10 ms. This is the earliest router signal.
  - **1.1.1.1:443 connect:** median over 10 ms (baseline 6.1).
  - **Pi conntrack:** over 75% full.
  - Do not use ICMP latency: it stays flat while the router saturates.

## Not measured
- The router's own CPU or NAT table size (no router login allowed). The router's TCP:80 answer time was used as a proxy. The NAT table never filled visibly; packet rate failed first.
- Held connections above about 393: there were not enough peers in the legal swarms.
- Fresh-node DHT: the 40k pool was cycled, so each node got a query every 13 to 100 s. A worker that always queries new nodes creates more new NAT entries per query and may hit the knee earlier.
- Sustained runs longer than 60 s, and router behaviour after its own UDP/TCP NAT timeouts.
- Bulk throughput was not re-measured (run 1: 80 to 91 Mbit/s down, about 80 up). It is not relevant here: the load stays under 4 Mbit/s.
- Packets/s for the DHT-only steps were not logged (the counters were added afterwards). The values above are estimates: about 1 packet out per query, and about (reply rate × q/s) packets in.
- The recovery time resolution is 4 s, the first check point.

Files: `*_r2.log/jsonl/json`, `combo_*.log`, `*_combo_*.jsonl`, `idle_r2.log`, `harvest_r2.log`, `peers_r2.log`, `throttled_*_r2.txt`, `dmesg_start_r2.txt`, scripts (`mon.py`, `ramp_dht.py`, `ramp_tcp.py`, `idle2.py`).
