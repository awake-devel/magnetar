"""Connectivity monitor used while a capacity test runs. Read-only, no privileges.

Thread-based blocking probes (accurate and simple; asyncio versions produced spurious
timeouts under concurrency). Public ICMP is blocked and ICMP to the gateway is rate-limited
by the neufbox, so health is sampled with flows that traverse the router NAT:
  - TCP connect to 1.1.1.1:443 and 9.9.9.9:443  -> primary signal (0% idle loss, ~16ms)
  - UDP DNS to 1.1.1.1:53 and 9.9.9.9:53        -> secondary UDP-path signal
  - TCP connect to <ip>:80               -> gateway/router-CPU signal (~1ms idle)
Each probe is one fresh flow, so loss/latency reflect NAT-table pressure directly.
"""
import threading, time, socket, struct

GW = "<ip>"
TCP_TARGETS = {"tcp": ("1.1.1.1", 443), "tcp9": ("9.9.9.9", 443)}
GW_TCP = ("<ip>", 80)
UDP_TARGETS = {"1.1.1.1": ("1.1.1.1", 53), "9.9.9.9": ("9.9.9.9", 53)}
_NAMES = [b"cloudflare.com", b"google.com", b"wikipedia.org", b"quad9.net",
          b"example.com", b"github.com", b"mozilla.org", b"debian.org"]


def _dns_query(name, tid):
    q = struct.pack(">HHHHHH", tid, 0x0100, 1, 0, 0, 0)
    for part in name.split(b"."):
        q += bytes([len(part)]) + part
    return q + b"\x00" + struct.pack(">HH", 1, 1)


class Monitor:
    def __init__(self, period=0.3):
        self.period = period
        self.keys = list(TCP_TARGETS) + ["gwtcp"] + list(UDP_TARGETS)
        self.samples = {k: [] for k in self.keys}
        self._stop = threading.Event()
        self._threads = []

    def _tcp_probe(self, key, target, timeout):
        while not self._stop.is_set():
            t = time.time()
            s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            s.settimeout(timeout)
            try:
                s.connect(target)
                self.samples[key].append((t, (time.time() - t) * 1000))
            except Exception:
                self.samples[key].append((t, None))
            finally:
                s.close()
            self._stop.wait(self.period)

    def _udp_probe(self, key, target, timeout=1.5):
        tid = 0
        while not self._stop.is_set():
            tid = (tid + 1) & 0xFFFF
            s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            s.settimeout(timeout)
            t = time.time()
            try:
                s.sendto(_dns_query(_NAMES[tid % len(_NAMES)], tid), target)
                s.recvfrom(512)
                self.samples[key].append((t, (time.time() - t) * 1000))
            except Exception:
                self.samples[key].append((t, None))
            finally:
                s.close()
            self._stop.wait(self.period)

    def start(self):
        for key, tgt in TCP_TARGETS.items():
            self._threads.append(threading.Thread(target=self._tcp_probe, args=(key, tgt, 2.0), daemon=True))
        self._threads.append(threading.Thread(target=self._tcp_probe, args=("gwtcp", GW_TCP, 1.0), daemon=True))
        for key, tgt in UDP_TARGETS.items():
            self._threads.append(threading.Thread(target=self._udp_probe, args=(key, tgt), daemon=True))
        for th in self._threads:
            th.start()

    def stop(self):
        self._stop.set()

    def stats(self, key, since=0.0, until=None):
        until = until or time.time() + 1
        w = [(t, r) for (t, r) in self.samples[key] if since <= t <= until]
        if not w:
            return {"n": 0, "loss": None, "med": None, "p95": None, "max": None}
        rtts = sorted(r for (_, r) in w if r is not None)
        loss = sum(1 for (_, r) in w if r is None) / len(w)
        q = lambda p: rtts[min(len(rtts) - 1, int(len(rtts) * p))] if rtts else None
        return {"n": len(w), "loss": round(loss, 4), "med": q(0.5), "p95": q(0.95),
                "max": rtts[-1] if rtts else None}

    def recent_loss(self, key, seconds):
        return self.stats(key, since=time.time() - seconds)["loss"]
