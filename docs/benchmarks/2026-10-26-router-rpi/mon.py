"""Pi variant of the laptop's connectivity monitor (read-only, no privileges).

Link health (each probe = one fresh flow through the home router's NAT):
  - TCP connect 1.1.1.1:443, 9.9.9.9:443   primary WAN signal
  - TCP connect gateway:80                   router-CPU signal
  - UDP DNS 1.1.1.1:53                       UDP path
  - ICMP echo 1.1.1.1 and gateway            unprivileged ICMP datagram socket
Pi resources (1 s): per-core CPU (incl. softirq), MemAvailable, load1, the Pi's OWN
nf_conntrack count (max 7680 on this box), UDP socket errors, this process's CPU.
"""
import threading, time, socket, struct, os, json

BASE = os.path.dirname(os.path.abspath(__file__))
GW = "<ip>"
TCP_TARGETS = {"tcp": ("1.1.1.1", 443), "tcp9": ("9.9.9.9", 443)}
GW_TCP = (GW, 80)
UDP_TARGETS = {"udp": ("1.1.1.1", 53)}
ICMP_TARGETS = {"icmp": "1.1.1.1", "icmpgw": GW}
_NAMES = [b"cloudflare.com", b"google.com", b"wikipedia.org", b"quad9.net",
          b"example.com", b"github.com", b"mozilla.org", b"debian.org"]
CT = "/proc/sys/net/netfilter/nf_conntrack_count"
CT_MAX = int(open("/proc/sys/net/netfilter/nf_conntrack_max").read()) if os.path.exists(CT) else -1


def ctrack():
    try: return int(open(CT).read())
    except Exception: return -1


def _dns_query(name, tid):
    q = struct.pack(">HHHHHH", tid, 0x0100, 1, 0, 0, 0)
    for part in name.split(b"."):
        q += bytes([len(part)]) + part
    return q + b"\x00" + struct.pack(">HH", 1, 1)


def _cpu_times():
    out = {}
    for line in open("/proc/stat"):
        if line.startswith("cpu"):
            p = line.split(); v = list(map(int, p[1:8]))
            # user nice system idle iowait irq softirq
            out[p[0]] = (sum(v), v[3] + v[4], v[5] + v[6])
    return out


def _udp_err():
    lines = [l.split() for l in open("/proc/net/snmp") if l.startswith("Udp:")]
    d = dict(zip(lines[0][1:], map(int, lines[1][1:])))
    return d.get("InErrors", 0), d.get("RcvbufErrors", 0), d.get("SndbufErrors", 0)


def _self_cpu():
    p = open("/proc/self/stat").read().rsplit(")", 1)[1].split()
    return int(p[11]) + int(p[12])


class Monitor:
    def __init__(self, period=0.3):
        self.period = period
        self.keys = list(TCP_TARGETS) + ["gwtcp"] + list(UDP_TARGETS) + list(ICMP_TARGETS)
        self.samples = {k: [] for k in self.keys}
        self.res = []   # (t, dict)
        self._stop = threading.Event()
        self._threads = []

    def _tcp_probe(self, key, target, timeout):
        while not self._stop.is_set():
            t = time.time()
            s = socket.socket(socket.AF_INET, socket.SOCK_STREAM); s.settimeout(timeout)
            try:
                s.connect(target); self.samples[key].append((t, (time.time() - t) * 1000))
            except Exception:
                self.samples[key].append((t, None))
            finally:
                s.close()
            self._stop.wait(self.period)

    def _udp_probe(self, key, target, timeout=1.5):
        tid = 0
        while not self._stop.is_set():
            tid = (tid + 1) & 0xFFFF
            s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.settimeout(timeout)
            t = time.time()
            try:
                s.sendto(_dns_query(_NAMES[tid % len(_NAMES)], tid), target); s.recvfrom(512)
                self.samples[key].append((t, (time.time() - t) * 1000))
            except Exception:
                self.samples[key].append((t, None))
            finally:
                s.close()
            self._stop.wait(self.period)

    def _icmp_probe(self, key, host, timeout=1.5):
        try:
            s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_ICMP)
        except Exception:
            return
        s.settimeout(timeout); seq = 0
        while not self._stop.is_set():
            seq = (seq + 1) & 0xFFFF
            t = time.time()
            try:
                s.sendto(struct.pack(">BBHHH", 8, 0, 0, 0, seq) + b"rpibench", (host, 0))
                while True:
                    d, _ = s.recvfrom(256)
                    if len(d) >= 8 and struct.unpack(">H", d[6:8])[0] == seq: break
                self.samples[key].append((t, (time.time() - t) * 1000))
            except Exception:
                self.samples[key].append((t, None))
            self._stop.wait(self.period)
        s.close()

    def _res(self):
        hz = os.sysconf("SC_CLK_TCK")
        prev = _cpu_times(); pself = _self_cpu(); pt = time.time()
        while not self._stop.wait(1.0):
            cur = _cpu_times(); cself = _self_cpu(); now = time.time()
            cores = {}
            for k, (tot, idle, irq) in cur.items():
                dt = tot - prev[k][0] or 1
                cores[k] = (round(100 * (1 - (idle - prev[k][1]) / dt), 1),
                            round(100 * (irq - prev[k][2]) / dt, 1))
            mem = {l.split(":")[0]: int(l.split()[1]) for l in open("/proc/meminfo")}
            la = open("/proc/loadavg").read().split()
            ie, rbe, sbe = _udp_err()
            self.res.append((now, {
                "cpu": cores["cpu"][0], "soft": cores["cpu"][1],
                "core": [cores[f"cpu{i}"][0] for i in range(4) if f"cpu{i}" in cores],
                "coresoft": [cores[f"cpu{i}"][1] for i in range(4) if f"cpu{i}" in cores],
                "mem_avail_mb": mem["MemAvailable"] // 1024, "load1": float(la[0]),
                "ct": ctrack(), "udp_inerr": ie, "udp_rcvbuf": rbe, "udp_sndbuf": sbe,
                "self_cpu": round(100 * (cself - pself) / hz / (now - pt), 1)}))
            prev, pself, pt = cur, cself, now

    def start(self):
        for key, tgt in TCP_TARGETS.items():
            self._threads.append(threading.Thread(target=self._tcp_probe, args=(key, tgt, 2.0), daemon=True))
        self._threads.append(threading.Thread(target=self._tcp_probe, args=("gwtcp", GW_TCP, 1.0), daemon=True))
        for key, tgt in UDP_TARGETS.items():
            self._threads.append(threading.Thread(target=self._udp_probe, args=(key, tgt), daemon=True))
        for key, h in ICMP_TARGETS.items():
            self._threads.append(threading.Thread(target=self._icmp_probe, args=(key, h), daemon=True))
        self._threads.append(threading.Thread(target=self._res, daemon=True))
        for th in self._threads: th.start()

    def stop(self):
        self._stop.set()

    def stats(self, key, since=0.0, until=None):
        until = until or time.time() + 1
        w = [(t, r) for (t, r) in self.samples[key] if since <= t <= until]
        if not w:
            return {"n": 0, "loss": None, "med": None, "p95": None, "max": None}
        rtts = sorted(r for (_, r) in w if r is not None)
        loss = sum(1 for (_, r) in w if r is None) / len(w)
        q = lambda p: round(rtts[min(len(rtts) - 1, int(len(rtts) * p))], 1) if rtts else None
        return {"n": len(w), "loss": round(loss, 3), "med": q(0.5), "p95": q(0.95),
                "max": round(rtts[-1], 1) if rtts else None}

    def recent_loss(self, key, seconds):
        return self.stats(key, since=time.time() - seconds)["loss"]

    def res_stats(self, since=0.0, until=None):
        until = until or time.time() + 1
        w = [d for (t, d) in self.res if since <= t <= until]
        if not w: return {}
        ncore = len(w[0]["core"])
        return {"n": len(w),
                "cpu_mean": round(sum(d["cpu"] for d in w) / len(w), 1),
                "cpu_max": max(d["cpu"] for d in w),
                "soft_mean": round(sum(d["soft"] for d in w) / len(w), 1),
                "core_max": [max(d["core"][i] for d in w) for i in range(ncore)],
                "core_mean": [round(sum(d["core"][i] for d in w) / len(w), 1) for i in range(ncore)],
                "coresoft_mean": [round(sum(d["coresoft"][i] for d in w) / len(w), 1) for i in range(ncore)],
                "self_cpu_mean": round(sum(d["self_cpu"] for d in w) / len(w), 1),
                "self_cpu_max": max(d["self_cpu"] for d in w),
                "mem_avail_min": min(d["mem_avail_mb"] for d in w),
                "load1_max": max(d["load1"] for d in w),
                "ct_max": max(d["ct"] for d in w), "ct_end": w[-1]["ct"], "ct_limit": CT_MAX,
                "udp_inerr_d": w[-1]["udp_inerr"] - w[0]["udp_inerr"],
                "udp_rcvbuf_d": w[-1]["udp_rcvbuf"] - w[0]["udp_rcvbuf"],
                "udp_sndbuf_d": w[-1]["udp_sndbuf"] - w[0]["udp_sndbuf"]}

    def last_res(self):
        return self.res[-1][1] if self.res else {}


# ---- health / abort logic, relative to the Pi's own baseline.json -------------------
def load_baseline():
    try:
        b = json.load(open(os.path.join(BASE, "baseline.json")))["idle"]
        return {k: (b[k]["med"] or 0) for k in ("tcp", "udp", "gwtcp", "icmp")}
    except Exception:
        return {"tcp": 10.0, "udp": 10.0, "gwtcp": 1.0, "icmp": 6.0}


BL = load_baseline()


def degraded(m, now, W=8):
    s = {k: m.stats(k, since=now - W) for k in ("tcp", "tcp9", "udp", "gwtcp", "icmp")}
    tmed = s["tcp"]["med"] or 0; t9med = s["tcp9"]["med"] or 0
    tloss = ((s["tcp"]["loss"] or 0) + (s["tcp9"]["loss"] or 0)) / 2
    umed = s["udp"]["med"] or 0; uloss = s["udp"]["loss"] or 0
    gmed = s["gwtcp"]["med"] or 0; gloss = s["gwtcp"]["loss"] or 0
    imed = s["icmp"]["med"] or 0
    lim = lambda b: max(3 * b, b + 20)
    lat_rise = tmed > lim(BL["tcp"]) or umed > lim(BL["udp"]) or imed > lim(BL["icmp"])
    bad = (lat_rise or gmed > max(10, 10 * BL["gwtcp"])
           or tloss > 0.2 or gloss > 0.2 or (uloss > 0.3 and umed > 2 * BL["udp"]))
    r = m.last_res()
    pi_bad = bool(r) and (r["mem_avail_mb"] < 150 or r["load1"] > 3.5)
    snap = dict(tmed=tmed, t9med=t9med, tloss=round(tloss, 2), umed=umed, uloss=uloss,
                gmed=gmed, gloss=gloss, imed=imed, lat_rise=lat_rise, pi_bad=pi_bad,
                mem=r.get("mem_avail_mb"), load1=r.get("load1"), cpu=r.get("cpu"),
                core=r.get("core"), ct=r.get("ct"))
    return (bad or pi_bad), snap
