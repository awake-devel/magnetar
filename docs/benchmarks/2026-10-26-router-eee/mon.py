"""EEE variant of the laptop router-bench monitor. Read-only, no privileges.

Probes (each one a fresh flow through the router NAT):
  - TCP connect 1.1.1.1:443 / 9.9.9.9:443   -> primary WAN/NAT signal
  - UDP DNS 1.1.1.1:53                      -> UDP path
  - ICMP ping 1.1.1.1 (EEE's line answers public ICMP, unlike the laptop's)
  - TCP connect gateway:80 + ICMP gateway   -> router CPU signal
  - service guard: every 2 s fetch the user's public front (host caddy -> tinyauth,
    https://ocm.escd.cc resolved to <ip>) and ocm-web on 127.0.0.1:8794
  - CPU: /proc/stat busy/iowait + this process's own CPU, loadavg
"""
import threading, time, socket, struct, subprocess, os, json

BASE = os.path.expanduser("~/router-bench")
GW = "<ip>"
TCP_TARGETS = {"tcp": ("1.1.1.1", 443), "tcp9": ("9.9.9.9", 443)}
GW_TCP = (GW, 80)
UDP_TARGETS = {"1.1.1.1": ("1.1.1.1", 53)}
ICMP_TARGETS = {"icmp": "1.1.1.1", "gwicmp": GW}
SVC = {"svc": ["curl", "-sk", "-m", "4", "--resolve", "ocm.escd.<ip6><ip>", "-o", "/dev/null",
               "-w", "%{http_code} %{time_total}", "https://ocm.escd.cc/"],
       "svc2": ["curl", "-s", "-m", "4", "-o", "/dev/null", "-w", "%{http_code} %{time_total}",
                "http://127.0.0.1:8794/"]}
_NAMES = [b"cloudflare.com", b"google.com", b"wikipedia.org", b"quad9.net",
          b"example.com", b"github.com", b"mozilla.org", b"debian.org"]


def load_baseline():
    try:
        return json.load(open(os.path.join(BASE, "baseline.json")))
    except Exception:
        return {}


def _dns_query(name, tid):
    q = struct.pack(">HHHHHH", tid, 0x0100, 1, 0, 0, 0)
    for part in name.split(b"."):
        q += bytes([len(part)]) + part
    return q + b"\x00" + struct.pack(">HH", 1, 1)


def _cpu_times():
    v = list(map(int, open("/proc/stat").readline().split()[1:]))
    idle = v[3] + v[4]
    return sum(v), idle, v[4]


class Monitor:
    def __init__(self, period=0.3):
        self.period = period
        self.keys = list(TCP_TARGETS) + ["gwtcp"] + list(UDP_TARGETS) + list(ICMP_TARGETS) + list(SVC)
        self.samples = {k: [] for k in self.keys}
        self.cpu = []   # (t, busy%, iowait%, self%, load1)
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

    def _icmp_probe(self, key, host):
        while not self._stop.is_set():
            t = time.time()
            try:
                r = subprocess.run(["ping", "-n", "-c", "1", "-W", "1", host],
                                   capture_output=True, text=True, timeout=3)
                ms = None
                for tok in r.stdout.split():
                    if tok.startswith("time="):
                        ms = float(tok[5:])
                self.samples[key].append((t, ms))
            except Exception:
                self.samples[key].append((t, None))
            self._stop.wait(0.5)

    def _svc_probe(self, key, cmd):
        while not self._stop.is_set():
            t = time.time()
            try:
                r = subprocess.run(cmd, capture_output=True, text=True, timeout=6)
                code, tt = r.stdout.split()
                ok = code != "000" and not code.startswith("5")
                self.samples[key].append((t, float(tt) * 1000 if ok else None))
            except Exception:
                self.samples[key].append((t, None))
            self._stop.wait(2.0)

    def _cpu_probe(self):
        tot0, idle0, io0 = _cpu_times()
        me0 = os.times(); w0 = time.time()
        hz_cores = os.cpu_count() or 1
        while not self._stop.wait(1.0):
            tot1, idle1, io1 = _cpu_times()
            me1 = os.times(); w1 = time.time()
            dt = max(1, tot1 - tot0)
            busy = 100.0 * (1 - (idle1 - idle0) / dt)
            iow = 100.0 * (io1 - io0) / dt
            selfp = 100.0 * ((me1.user + me1.system) - (me0.user + me0.system)) / (w1 - w0)  # % of one core
            load1 = os.getloadavg()[0]
            self.cpu.append((w1, round(busy, 1), round(iow, 1), round(selfp, 1), load1))
            tot0, idle0, io0, me0, w0 = tot1, idle1, io1, me1, w1

    def start(self):
        for key, tgt in TCP_TARGETS.items():
            self._threads.append(threading.Thread(target=self._tcp_probe, args=(key, tgt, 2.0), daemon=True))
        self._threads.append(threading.Thread(target=self._tcp_probe, args=("gwtcp", GW_TCP, 1.0), daemon=True))
        for key, tgt in UDP_TARGETS.items():
            self._threads.append(threading.Thread(target=self._udp_probe, args=(key, tgt), daemon=True))
        for key, h in ICMP_TARGETS.items():
            self._threads.append(threading.Thread(target=self._icmp_probe, args=(key, h), daemon=True))
        for key, cmd in SVC.items():
            self._threads.append(threading.Thread(target=self._svc_probe, args=(key, cmd), daemon=True))
        self._threads.append(threading.Thread(target=self._cpu_probe, daemon=True))
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
        q = lambda p: round(rtts[min(len(rtts) - 1, int(len(rtts) * p))], 2) if rtts else None
        return {"n": len(w), "loss": round(loss, 4), "med": q(0.5), "p95": q(0.95),
                "max": round(rtts[-1], 2) if rtts else None}

    def cpu_stats(self, since=0.0):
        w = [c for c in self.cpu if c[0] >= since]
        if not w:
            return {}
        avg = lambda i: round(sum(c[i] for c in w) / len(w), 1)
        return {"busy_avg": avg(1), "busy_max": max(c[1] for c in w), "iowait_avg": avg(2),
                "self_core_avg": avg(3), "self_core_max": max(c[3] for c in w),
                "load1_max": round(max(c[4] for c in w), 2)}

    def recent_loss(self, key, seconds):
        return self.stats(key, since=time.time() - seconds)["loss"]


def degraded(m, now, B, W=9):
    """Shared abort rule, relative to EEE's own baseline B (from baseline.json).
    Router NAT/CPU saturation = median-latency rise on WAN/gw probes; the service guard
    aborts on a 3x latency rise (with an absolute floor, the box is noisy) or errors."""
    st = {k: m.stats(k, since=now - W) for k in ("tcp", "1.1.1.1", "gwtcp", "icmp", "svc", "svc2")}
    b = lambda k, d: (B.get(k) or {}).get("med") or d
    tmed = st["tcp"]["med"] or 0; tloss = st["tcp"]["loss"] or 0
    umed = st["1.1.1.1"]["med"] or 0; uloss = st["1.1.1.1"]["loss"] or 0
    gmed = st["gwtcp"]["med"] or 0; gloss = st["gwtcp"]["loss"] or 0
    imed = st["icmp"]["med"] or 0
    smed = st["svc"]["med"] or 0; sloss = st["svc"]["loss"] or 0
    s2med = st["svc2"]["med"] or 0; s2loss = st["svc2"]["loss"] or 0
    bt, bu, bg, bi = b("tcp", 6), b("1.1.1.1", 6), b("gwtcp", 1), b("icmp", 6)
    bs, bs2 = b("svc", 10), b("svc2", 40)
    reasons = []
    if tmed > max(3 * bt, bt + 15): reasons.append("tcp_lat")
    if umed > max(3 * bu, bu + 15): reasons.append("udp_lat")
    if imed and imed > max(3 * bi, bi + 15): reasons.append("icmp_lat")
    if gmed > max(10 * bg, 10): reasons.append("gw_lat")
    if tloss > 0.25 and tmed > 2 * bt: reasons.append("tcp_loss")
    if gloss > 0.3: reasons.append("gw_loss")
    if (uloss > 0.25 and umed > 2 * bu): reasons.append("udp_loss")
    if smed > max(3 * bs, bs + 30) or sloss >= 0.5: reasons.append("svc")
    if s2med > max(3 * bs2, bs2 + 150) or s2loss >= 0.5: reasons.append("svc2")
    lat_rise = tmed > 2 * bt or umed > 2 * bu or (imed and imed > 2 * bi)
    snap = dict(tmed=tmed, tloss=round(tloss, 2), umed=umed, uloss=round(uloss, 2), imed=imed,
                gmed=gmed, gloss=round(gloss, 2), smed=smed, sloss=round(sloss, 2),
                s2med=s2med, s2loss=round(s2loss, 2), reasons=reasons, lat_rise=bool(lat_rise))
    hard = (tmed > 150 or umed > 150 or sloss >= 0.5 or "svc" in reasons and smed > 500)
    return bool(reasons), hard, snap
