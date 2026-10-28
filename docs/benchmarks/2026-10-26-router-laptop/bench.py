#!/usr/bin/env python3
"""Home-router stress bench (stdlib only). Subcommands: baseline, dht, peers, tcprate, tcpconc.
All runs keep a monitor (ping 1.1.1.1 / 9.9.9.9 / gateway + new-flow probes) and abort on degradation."""
import asyncio, os, sys, time, json, random, socket, struct, hashlib, ipaddress, statistics, re, glob, errno

HOME = os.path.expanduser("~/.cache/router-bench")
GW = "<ip>"
LOG = None

def log(*a):
    s = time.strftime("%H:%M:%S ") + " ".join(str(x) for x in a)
    print(s, flush=True)
    if LOG:
        LOG.write(s + "\n"); LOG.flush()

# ---------------- bencode ----------------
def bdecode(b, i=0):
    c = b[i:i+1]
    if c == b"i":
        j = b.index(b"e", i); return int(b[i+1:j]), j+1
    if c == b"l":
        i += 1; out = []
        while b[i:i+1] != b"e":
            v, i = bdecode(b, i); out.append(v)
        return out, i+1
    if c == b"d":
        i += 1; out = {}
        while b[i:i+1] != b"e":
            k, i = bdecode(b, i); v, i = bdecode(b, i); out[k] = v
        return out, i+1
    j = b.index(b":", i); n = int(b[i:j]); return b[j+1:j+1+n], j+1+n

def benc(x):
    if isinstance(x, int): return b"i%de" % x
    if isinstance(x, (bytes, bytearray)): return b"%d:%s" % (len(x), x)
    if isinstance(x, str): return benc(x.encode())
    if isinstance(x, list): return b"l" + b"".join(benc(v) for v in x) + b"e"
    if isinstance(x, dict):
        return b"d" + b"".join(benc(k if isinstance(k, bytes) else k.encode()) + benc(x[k]) for k in sorted(x, key=lambda k: k if isinstance(k, bytes) else k.encode())) + b"e"
    raise TypeError(type(x))

def info_span(b):
    # locate raw info dict bytes for exact hashing
    i = b.index(b"4:info") + 6
    _, j = bdecode(b, i)
    return b[i:j]

# ---------------- monitor ----------------
class Monitor:
    def __init__(self):
        self.ping = {"1.1.1.1": [], "9.9.9.9": [], GW: []}  # (t, rtt or None)
        self.flow = []  # (t, kind, ok, ms)
        self.procs = []
        self.tasks = []
        self.cpu_prev = self._cpu()

    @staticmethod
    def _cpu():
        with open("/proc/stat") as f:
            v = [int(x) for x in f.readline().split()[1:]]
        return sum(v), v[3] + v[4]

    def cpu_pct(self):
        t, idle = self._cpu(); pt, pidle = self.cpu_prev; self.cpu_prev = (t, idle)
        return 100.0 * (1 - (idle - pidle) / max(1, t - pt))

    async def _pinger(self, host):
        p = await asyncio.create_subprocess_exec("ping", "-n", "-D", "-O", "-i", "0.2", "-W", "1", host,
                                                 stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL)
        self.procs.append(p)
        rx = re.compile(rb"time