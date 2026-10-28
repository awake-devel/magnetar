# RPI (Raspberry Pi 3B, dad's line) router benchmark — 2026-10-26 16:58–17:33

Baseline: TCP connect 1.1.1.1/9.9.9.9 ~6 ms, gateway TCP:80 1.1 ms, ICMP allowed. Throughput ~80–91 Mbit/s down, ~80 up (Pi 3 100 Mbit NIC is the cap).
Router: never reached a limit (2000 DHT q/s, 300 TCP attempts/s, 416 held connections).
Real limit: the Pi's own nf_conntrack table, max 7680 entries (Tailscale firewall rules enable conntrack). DHT clean up to 800 q/s; drops ("table full") from 1200 q/s.
TCP attempts: clean up to 300/s (16% connect, peer-side). Held: ~400 clean (peer-limited at 416).
CPU: ≤15% total for the Python bench; RAM free ≥538 MB. vcgencmd get_throttled = 0x50000 (past under-voltage/throttling since boot — check the power supply).
Recommended worker defaults: KRPC 400 q/s (550 if alone), TCP attempts 150/s (max 200), concurrent TCP 300, header-probe swarms ≤12, worker RAM < 200 MB (slow down below 250 MB free); avoid re-querying the same node from the same socket within 30 s; backoff on TCP-connect probe to 1.1.1.1:443 (median > ~26 ms) or conntrack > 90% full.
Optional (root): raise net.netfilter.nf_conntrack_max to 32k–64k to remove the Pi-side limit.
Raw logs and adapted scripts: this directory.
