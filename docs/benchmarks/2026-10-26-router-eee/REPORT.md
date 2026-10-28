# Home-router capacity benchmark — EEE (<ip>, gw <ip>, public <ip>)
Date 2026-10-26, 15:55–16:35 UTC. Run ON EEE over ssh (stdlib Python, no root, no router/sysctl/Docker changes).
Scripts adapted from ~/.cache/router-bench (EEE gateway, ICMP + service-guard probes, CPU sampling,
<=5 q/s per IP, abort after 2 bad 3-s checks over a 9-s window, i.e. <=~10 s). Logs: this dir (copy of EEE ~/router-bench).

## Baseline (60 s idle)
- TCP-connect 1.1.1.1:443 6.1 ms / 9.9.9.9:443 5.3 ms, 0% loss; UDP DNS 7.0 ms, 0%.
- ICMP WORKS on this line (unlike the laptop's): 1.1.1.1 6.0 ms, gateway 0.6 ms, 0% loss.
- Gateway TCP:80 0.8 ms (gw answers HTTP 401). Public IPv4 = own WAN IP (inbound ssh works) -> no CGNAT.
- Throughput: down 651 Mbps single / 901 Mbps 4-stream; up 357 single / 685 2-stream. Bufferbloat mild
  (WAN RTT 6 -> 18-20 ms during 4-stream download). Box CPU idle ~13% busy, load <1; heavy bursts of
  radicle-node upload (~200 Mbps seen once) and a radicle sync job with high iowait exist in the background.
- Service guard: host caddy -> tinyauth (https://ocm.escd.cc via <ip>) 8–11 ms med; ocm-web :8794 noisy (1–200 ms).

## TEST 1 — DHT find_node q/s to distinct nodes (pool 40,119 harvested from EEE)
| q/s | sent | reply | WAN TCP med/p95 | gw TCP med/p95 | svc med | EEE busy avg | py core | result |
|---|---|---|---|---|---|---|---|---|
| 50 | 50 | 0.79 | 5.9/6.6 | 0.78/1.0 | 8.6 | 6.5% | 7% | OK |
| 100 | 100 | 0.75 | 6.0/6.5 | 0.77/1.0 | 8.7 | 7% | 8% | OK |
| 200 | 200 | 0.74 | 6.0/6.5 | 0.74/1.0 | 8.9 | 12% | 10% | OK |
| 300 | 300 | 0.73 | 5.9/6.4 | 0.69/1.2 | 11.1 | 38%* | 12% | OK |
| 400 | 400 | 0.74 | 5.8/6.5 | 0.71/1.1 | 8.7 | 12% | 13% | OK |
| 500 | 500 | 0.72 | 5.8/6.7 | 0.69/1.2 | 9.1 | 14% | 14% | OK |
| 600 | 600 | 0.72 | 5.9/6.6 | 0.72/1.3 | 8.7 | 9% | 16% | OK |
| 800 | 800 | 0.71 | 5.9/6.8 | 0.76/1.3 | 8.6 | 10% | 18% | OK |
| 1000 | 1000 | 0.70 | 5.9/6.8 | 0.76/1.3 | 8.6 | 11% | 24% | OK |
| 1300 | 1300 | 0.67 | 6.0/6.8 | 0.79/1.3 | 8.7 | 12% | 27% | OK |
| 1600 | 1600 | 0.65 | 6.1/7.1 | 0.85/1.5 | 9.0 | 13% | 30% | OK |
*background iowait from radicle sync, not the test. No knee found up to 1600 q/s (stopped there).
Reply-rate drift 0.79->0.65 = repeat queries to the same 40k pool (node-side limits), not NAT loss.

## TEST 2 — new TCP attempts/s to real Linux-ISO swarm peers (BEP 3 handshake, nothing downloaded)
| att/s | connect ok | WAN TCP med/p95 | UDP med/p95 | gw TCP med/p95 | ICMP med | py core | result |
|---|---|---|---|---|---|---|---|
| 10–100 | 16–18% | 6.0/6.6 | 7.0/9.5 | 0.76/1.0 | 5.9 | 5–9% | OK |
| 150 | 16% | 6.0/7.7 | 7.0/9.3 | 0.73/1.3 | 5.9 | 11% | OK |
| 200 | 16% | 6.0/9.5 | 7.0/9.3 | 0.72/3.3 | 5.8 | 12% | OK |
| 300 | 17% | 6.0/12.3 | 7.0/14.4 | 0.77/6.2 | 5.8 | 16% | OK (p95 starting to rise) |
| 400 | 16% | 6.1/17.9 | 7.2/18.1 | 0.77/10.9 | 5.8 | 19% | OK (p95 3x) |
| 500 | 17% | 7.0/21.5 | 8.5/21.2 | 0.81/14.4 | 5.8 | 23% | OK on medians, tails 3x+ (edge) |
| 600 | — | 17–19 med | 16–19 med | 6.7–13 med | 5.9 | 25% | ABORT after ~15 s (gw TCP lat x15) |
Knee: 400 clean-ish, 500 edge, 600 breaks. Recovery 5 s (first check). ~84% failures are peer-side (timeouts/refused).

## TEST 3 — concurrent held established TCP (correct-swarm handshake + BEP 3 keep-alives, ~22 s hold)
| target | established held | probes | result |
|---|---|---|---|
| 200 | 185–193 | baseline (6.0 / gw 0.8) | OK |
| 350 | 339–347 | baseline | OK |
| 500 | 465–478 | baseline | OK |
| 700 | 470–480 (reachability cap) | baseline | OK, stopped: peers |
Held ~480 with zero degradation; ceiling NOT reached (limited by peer reachability within a 20 s open window).
(First attempt held only ~113: handshaking all peers with one infohash made other swarms hang up — fixed.)

## Attribution
Router, not EEE CPU. At the 600/s abort EEE was 12% busy, the Python worker used 25–34% of one core,
host conntrack 41k/262k (no table pressure), while the GATEWAY's own TCP:80 latency went 0.8 -> 13 ms
and WAN TCP/UDP 6 -> 19 ms. ICMP to 1.1.1.1 stayed at 5.9 ms throughout = link not saturated; it is the
router's per-new-flow (SYN/NAT setup) path that saturates. UDP KRPC at 1600 q/s (~48k UDP NAT entries)
was invisible to the router. The i3-550 never limited: py <=34% of one core at the highest rates.

## Effect on EEE's services
None measurable. caddy->tinyauth median stayed 8.4–11 ms in every step, 0 errors; ocm-web 0 errors.
Occasional p95 spikes (50–160 ms) coincided with background iowait (radicle sync/swap), not with rate.
All containers still Up afterwards; nothing left running; conntrack back to ~4.5k.

## Recommended Magnetar worker defaults on EEE (~30% margin)
- KRPC queries/s: 1000 (tested clean to 1600; no knee found). Start ~500 with adaptive ramp.
- New TCP attempts/s: 300 (knee 400–500, breaks 600). Hard cap 350.
- Concurrent open TCP: 350 proven (480 clean, ceiling unknown, likely higher).
- Concurrent swarms (header probe): <=20.
- Adaptive backoff: ICMP to 1.1.1.1 is usable here but NOT sensitive to the TCP-rate knee; key backoff on
  TCP-connect to 1.1.1.1:443 and gateway TCP:80 median (gw > ~5 ms or WAN TCP > ~15 ms = back off).
- Keep the user's radicle/qbittorrent traffic in mind (they share the uplink and the router's NAT).

## Not measured
Router NAT table size / model (no admin; gw <ip>, MAC <ip6>…), DHT beyond 1600 q/s,
concurrent > ~480 (peer reachability), inbound UDP/TCP (port-forward behaviour), IPv6, multi-hour sustained load,
upload-heavy concurrent with crawl. Upload test was short (50 MB/stream).
