# Home-router capacity benchmark — SFR neufbox (MiniUPnPd 1.9)
Date 2026-10-26. Laptop 192.168.1.59, wired, NAT, public WAN IP (no CGNAT).
See the subagent report text for the full tables; key results:

PROBES: public ICMP blocked; gateway ICMP rate-limited by neufbox (~83% idle, unusable).
Health sampled via TCP-connect to 1.1.1.1:443 / 9.9.9.9:443 (primary, 0% idle loss, ~17ms),
UDP DNS to 1.1.1.1:53 (~3% idle, ~18.5ms), and TCP-connect to gateway:80 (~0.8ms, router CPU).
Abort keyed to median-latency rise, relative to baseline.

BASELINE: TCP 17ms/0% loss; UDP 18.5ms/~3%; gw:80 0.8ms; down ~230Mbps single/~800 agg, up ~315-380.

TEST1 DHT q/s (distinct nodes, pool 40,756): 50/100/200/300 OK (median ~17-19ms, 0% loss, ~70% reply);
400 ABORT (TCP med 122ms, UDP 66ms, gw loss 14%/194ms, reply 63%). Knee: 300 safe, 400 breaks. Recovery ~5s.

TEST2 new TCP conn rate (attempts/s, each SYN=NAT entry): 10-200/s OK (med ~17ms, <1% loss);
400/s ABORT (100% TCP loss, UDP 148ms). Knee: 200 safe, 400 breaks. Recovery ~5s.
~16% connect success = stale peers (peer-side), NOT router; probes pristine to 200/s.

TEST3 concurrent established (slow 80/s open): 437 held 90s OK (17ms/0%); pushing toward ~900 ABORT.
Knee: ~440-500 safe, breaks 500-900. Recovery 5-15s.

TEST4 recovery: baseline within 5-15s after each abort; conntrack 38-40k -> 16-20k in ~10s.

ATTRIBUTION: router is the bottleneck. Laptop conntrack peak ~36-40k/262144, CPU low, 0 UDP
sendfail, no EMFILE/ENOBUFS, send rates sustained exactly. At saturation the gateway's OWN
TCP:80 latency spiked + lost packets alongside WAN probes = router CPU/NAT exhaustion.

RECOMMENDED DEFAULTS (~30% under knee):
- KRPC queries/s: 200 (knee ~300; lower crawler's 250 default)
- New TCP connections/s: 150 attempts/s
- Concurrent open TCP: ~350
- Concurrent swarms (header probe): <=15
- Keep adaptive backoff keyed on a TCP-connect probe to 1.1.1.1:443 (ICMP useless here).
- Leave ~20k conntrack + live bandwidth headroom for the user's other jobs (present during tests).

NOT MEASURED: router's own NAT table size (no admin), ISP CGNAT (unverified from LAN),
pure concurrent-established >440 (bounded 500-900; limited by ~15% peer reachability),
inbound UDP, multi-hour sustained behavior.

Scripts/logs in ~/.cache/router-bench/ (mon.py krpc.py dht_harvest.py ramp_dht.py peers_harvest.py ramp_tcp.py baseline.py; *.jsonl *_rows.json).
