#!/usr/bin/env python3
"""Bounded peer-reuse experiment (research only, not production).

Phases (each logged to experiment.log, raw results to results/*.jsonl):
  P1  iterative get_peers lookups (production-like: width 16, alpha 4, stop when the 8 closest
      answered or 10 s) for 250 alive hashes + 100 queued hashes; per-lookup cost timeline.
  P2  BitTorrent handshake probes (<=12 peers per hash) -> "good peers" (echoed the infohash).
      The first 300 successful sessions advertise ut_pex and stay up to 15 s to count PEX peers.
  P3  cross test: top good peers x NEW hashes (150 alive never looked up + 100 queued), each
      pair one BEP 3 handshake. Per peer >= 2 s between attempts.
  P4  incoming get_peers / announce_peer captured passively from production (tcpdump file given
      on the command line): probe querier ip:port (TCP, same port) and announcer ip:port.

Hard bounds: TCP attempts <= 150/s globally, <= 1500 TCP open, KRPC <= 150 q/s, wall clock
<= 9 min (aborts all phases after that).
"""
import asyncio, os, random, socket, struct, sys, time, json, hashlib, ipaddress, collections

T0 = time.monotonic()
WALL = 9 * 60
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "results")
os.makedirs(OUT, exist_ok=True)
LOG = os.path.join(os.path.dirname(os.path.abspath(__file__)), "experiment.log")


def log(msg):
    line = f"{time.strftime('%Y-%m-%dT%H:%M:%S')} +{time.monotonic()-T0:6.1f}s {msg}"
    print(line, flush=True)
    with open(LOG, "a") as f:
        f.write(line + "\n")


def expired():
    return time.monotonic() - T0 > WALL


# ---------------- bencode ----------------
def benc(x):
    if isinstance(x, int):
        return b"i%de" % x
    if isinstance(x, (bytes, bytearray)):
        return b"%d:%s" % (len(x), bytes(x))
    if isinstance(x, str):
        return benc(x.encode())
    if isinstance(x, list):
        return b"l" + b"".join(benc(i) for i in x) + b"e"
    if isinstance(x, dict):
        return b"d" + b"".join(benc(k) + benc(x[k]) for k in sorted(x)) + b"e"
    raise TypeError(type(x))


def bdec(b, i=0):
    c = b[i:i + 1]
    if c == b"i":
        j = b.index(b"e", i)
        return int(b[i + 1:j]), j + 1
    if c == b"l":
        i += 1
        out = []
        while b[i:i + 1] != b"e":
            v, i = bdec(b, i)
            out.append(v)
        return out, i + 1
    if c == b"d":
        i += 1
        out = {}
        while b[i:i + 1] != b"e":
            k, i = bdec(b, i)
            v, i = bdec(b, i)
            out[k] = v
        return out, i + 1
    if c.isdigit():
        j = b.index(b":", i)
        n = int(b[i:j])
        return b[j + 1:j + 1 + n], j + 1 + n
    raise ValueError("bad bencode")


def decode(b):
    try:
        return bdec(b)[0]
    except Exception:
        return None


def global_v4(ip):
    try:
        a = ipaddress.IPv4Address(ip)
    except Exception:
        return False
    return a.is_global


def compact_nodes(b):
    out = []
    for i in range(0, len(b) - len(b) % 26, 26):
        nid = b[i:i + 20]
        ip = socket.inet_ntoa(b[i + 20:i + 24])
        port = struct.unpack(">H", b[i + 24:i + 26])[0]
        if port and global_v4(ip):
            out.append((nid, (ip, port)))
    return out


def compact_peer(b):
    if len(b) != 6:
        return None
    ip = socket.inet_ntoa(b[:4])
    port = struct.unpack(">H", b[4:])[0]
    if port == 0 or not global_v4(ip):
        return None
    return (ip, port)


def xor(a, b):
    return int.from_bytes(a, "big") ^ int.from_bytes(b, "big")


# ---------------- rate limiter ----------------
class Pace:
    def __init__(self, rate):
        self.gap = 1.0 / rate
        self.next = time.monotonic()

    async def take(self):
        now = time.monotonic()
        at = max(now, self.next)
        self.next = at + self.gap
        if at > now:
            await asyncio.sleep(at - now)


# ---------------- KRPC client ----------------
class Krpc(asyncio.DatagramProtocol):
    def __init__(self):
        self.pending = {}
        self.nid = os.urandom(20)
        self.pace = Pace(150)
        self.sent = 0
        self.answered = 0
        self.rtts = []
        self.nodes = {}  # addr -> id (all learnt)

    def connection_made(self, t):
        self.t = t

    def datagram_received(self, data, addr):
        m = decode(data)
        if not isinstance(m, dict):
            return
        tid = m.get(b"t")
        key = (tid, addr)
        fut = self.pending.pop(key, None)
        if fut and not fut.done():
            fut.set_result(m)

    async def query(self, addr, q, a, timeout=4.0):
        await self.pace.take()
        tid = os.urandom(2)
        while (tid, addr) in self.pending:
            tid = os.urandom(2)
        a = dict(a)
        a[b"id"] = self.nid
        msg = {b"t": tid, b"y": b"q", b"q": q, b"a": a, b"ro": 1}
        fut = asyncio.get_running_loop().create_future()
        self.pending[(tid, addr)] = fut
        t = time.monotonic()
        try:
            self.t.sendto(benc(msg), addr)
        except Exception:
            self.pending.pop((tid, addr), None)
            return None
        self.sent += 1
        try:
            m = await asyncio.wait_for(fut, timeout)
        except asyncio.TimeoutError:
            self.pending.pop((tid, addr), None)
            return None
        if m.get(b"y") != b"r":
            return None
        self.answered += 1
        self.rtts.append(time.monotonic() - t)
        return m.get(b"r", {})

    def learn(self, nodes):
        for nid, addr in nodes:
            if len(self.nodes) < 300000:
                self.nodes[addr] = nid

    def closest(self, target, n):
        return sorted(self.nodes.items(), key=lambda kv: xor(kv[1], target))[:n]


BOOT = [("router.bittorrent.com", 6881), ("dht.transmissionbt.com", 6881),
        ("router.utorrent.com", 6881), ("dht.libtorrent.org", 25401)]


async def bootstrap(k):
    addrs = []
    for h, p in BOOT:
        try:
            for ai in socket.getaddrinfo(h, p, socket.AF_INET, socket.SOCK_DGRAM):
                addrs.append(ai[4][:2])
        except Exception:
            pass
    addrs = list(set(addrs))
    # a few find_node walks towards random targets to populate the table
    async def walk():
        target = os.urandom(20)
        cands = {a: None for a in addrs}
        asked = set()
        for _ in range(6):
            todo = sorted([a for a in cands if a not in asked],
                          key=lambda a: xor(cands[a], target) if cands[a] else 1 << 170)[:16]
            if not todo:
                break
            asked.update(todo)
            rs = await asyncio.gather(*[k.query(a, b"find_node", {b"target": target}, 2.0) for a in todo])
            for r in rs:
                if r and b"nodes" in r:
                    ns = compact_nodes(r[b"nodes"])
                    k.learn(ns)
                    for nid, a in ns:
                        cands.setdefault(a, nid)
    await asyncio.gather(*[walk() for _ in range(40)])
    log(f"bootstrap: {len(k.nodes)} nodes known, {k.sent} queries")


async def lookup(k, target, budget=10.0):
    """Production-like get_peers walk; records the cost timeline."""
    t0 = time.monotonic()
    cands = {}
    for a, nid in k.closest(target, 16):
        cands[a] = {"id": nid, "st": "new", "values": []}
    for h in BOOT[:0]:
        pass
    queried = 0
    first_value_q = None
    first_value_t = None
    q12 = None
    peers = []
    seen = set()
    inflight = set()
    deadline = t0 + budget

    def window():
        live = [(xor(c["id"], target) if c["id"] else 1 << 170, a) for a, c in cands.items() if c["st"] != "fail"]
        live.sort()
        return [a for _, a in live[:16]]

    async def one(a):
        r = await k.query(a, b"get_peers", {b"info_hash": target}, 4.0)
        return a, r

    while True:
        w = window()
        done = w and all(cands[a]["st"] == "ok" for a in w[:8])
        if done or time.monotonic() >= deadline:
            break
        for a in w:
            if len(inflight) >= 4:
                break
            if cands[a]["st"] == "new":
                cands[a]["st"] = "fly"
                queried += 1
                inflight.add(asyncio.ensure_future(one(a)))
        if not inflight:
            break
        d, inflight = await asyncio.wait(inflight, timeout=max(0.01, deadline - time.monotonic()),
                                         return_when=asyncio.FIRST_COMPLETED)
        for f in d:
            a, r = f.result()
            if r is None:
                cands[a]["st"] = "fail"
                continue
            cands[a]["st"] = "ok"
            if r.get(b"id") and len(r[b"id"]) == 20:
                cands[a]["id"] = r[b"id"]
            vals = r.get(b"values") or []
            if isinstance(vals, list):
                for v in vals:
                    p = compact_peer(v) if isinstance(v, bytes) else None
                    if p and p not in seen:
                        seen.add(p)
                        peers.append(p)
                if peers and first_value_q is None:
                    first_value_q = queried
                    first_value_t = time.monotonic() - t0
                if len(peers) >= 12 and q12 is None:
                    q12 = queried
            if b"nodes" in r and isinstance(r[b"nodes"], bytes):
                ns = compact_nodes(r[b"nodes"])
                k.learn(ns)
                for nid, na in ns:
                    if len(cands) < 512 and na not in cands:
                        cands[na] = {"id": nid, "st": "new", "values": []}
    for f in inflight:
        f.cancel()
    return {"peers": peers, "queried": queried, "first_value_q": first_value_q,
            "first_value_t": first_value_t, "q12": q12, "secs": time.monotonic() - t0}


# ---------------- BitTorrent probe ----------------
PEER_ID = b"-XX0001-" + os.urandom(12)
tcp_pace = Pace(150)
tcp_open = asyncio.Semaphore(1500)
tcp_attempts = 0


async def read_exact(r, n, until):
    return await asyncio.wait_for(r.readexactly(n), max(0.01, until - time.monotonic()))


async def probe(addr, ih, pex_hold=0.0):
    """One BEP 3 (+BEP 10) session. Returns dict with outcome."""
    global tcp_attempts
    res = {"addr": f"{addr[0]}:{addr[1]}", "ih": ih.hex(), "out": None}
    async with tcp_open:
        await tcp_pace.take()
        tcp_attempts += 1
        t = time.monotonic()
        try:
            r, w = await asyncio.wait_for(asyncio.open_connection(addr[0], addr[1]), 3.0)
        except Exception:
            res["out"] = "connect"
            return res
        res["connect_ms"] = int((time.monotonic() - t) * 1000)
        try:
            reserved = bytearray(8)
            reserved[5] |= 0x10
            reserved[7] |= 0x01
            w.write(b"\x13BitTorrent protocol" + bytes(reserved) + ih + PEER_ID)
            await w.drain()
            until = time.monotonic() + 5.0
            try:
                hs = await read_exact(r, 68, until)
            except asyncio.IncompleteReadError as e:
                res["out"] = "closed_no_hs" if not e.partial else "short_hs"
                return res
            except asyncio.TimeoutError:
                res["out"] = "hs_timeout"
                return res
            except (ConnectionResetError, OSError):
                res["out"] = "reset_no_hs"
                return res
            if hs[28:48] != ih:
                res["out"] = "wrong_ih"
                return res
            res["out"] = "hs_ok"
            res["peer_id"] = hs[48:56].decode("latin1")
            if not hs[25] & 0x10:
                res["no_ext"] = True
                return res
            m = {b"m": {b"ut_metadata": 1}}
            if pex_hold:
                m[b"m"][b"ut_pex"] = 2
            body = b"\x14\x00" + benc(m)
            w.write(struct.pack(">I", len(body)) + body)
            await w.drain()
            end_ext = time.monotonic() + 5.0
            hold_until = time.monotonic() + pex_hold
            pex_id = None
            pex_peers = set()
            got_ext = False
            while True:
                lim = hold_until if got_ext and pex_hold else end_ext
                if time.monotonic() >= lim:
                    break
                ln = struct.unpack(">I", await read_exact(r, 4, lim))[0]
                if ln == 0:
                    continue
                if ln > 1 << 20:
                    break
                msg = await read_exact(r, ln, lim)
                if msg[0] == 20 and msg[1] == 0 and not got_ext:
                    d = decode(msg[2:]) or {}
                    got_ext = True
                    res["ext"] = True
                    res["metadata_size"] = d.get(b"metadata_size")
                    v = d.get(b"v")
                    res["v"] = v.decode("utf8", "replace")[:40] if isinstance(v, bytes) else None
                    mm = d.get(b"m") or {}
                    pex_id = mm.get(b"ut_pex")
                    res["has_ut_pex"] = bool(pex_id)
                    if not pex_hold or not pex_id:
                        break
                elif msg[0] == 20 and msg[1] == 2 and pex_hold:  # our ut_pex id
                    d = decode(msg[2:]) or {}
                    a = d.get(b"added", b"")
                    for i in range(0, len(a) - len(a) % 6, 6):
                        p = compact_peer(a[i:i + 6])
                        if p:
                            pex_peers.add(f"{p[0]}:{p[1]}")
                    res.setdefault("pex_msgs", 0)
                    res["pex_msgs"] += 1
                    res["pex_t"] = round(time.monotonic() - t, 1)
            if pex_hold:
                res["pex_peers"] = sorted(pex_peers)
        except (asyncio.TimeoutError, asyncio.IncompleteReadError, ConnectionResetError, OSError):
            pass
        finally:
            try:
                w.close()
            except Exception:
                pass
        return res


def dump(name, rows):
    with open(os.path.join(OUT, name), "w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")


async def main():
    here = os.path.dirname(os.path.abspath(__file__))
    alive = [bytes.fromhex(l.strip()) for l in open(os.path.join(here, "alive.txt")) if l.strip()]
    queued = [bytes.fromhex(l.strip()) for l in open(os.path.join(here, "queued.txt")) if l.strip()]
    pcap_tsv = sys.argv[1] if len(sys.argv) > 1 else None
    random.seed(7)
    p1_alive, new_alive = alive[:250], alive[250:400]
    log(f"start: alive={len(alive)} queued={len(queued)} pcap={pcap_tsv}")

    # ---- P4 queriers / announcers from production capture ----
    if pcap_tsv:
        items = []
        seenp = set()
        for line in open(pcap_tsv):
            parts = line.rstrip("\n").split("\t")
            if len(parts) < 4 or not parts[3]:
                continue
            try:
                b = bytes.fromhex(parts[3])
            except ValueError:
                continue
            m = decode(b)
            if not isinstance(m, dict) or m.get(b"y") != b"q":
                continue
            q = m.get(b"q")
            a = m.get(b"a") or {}
            ih = a.get(b"info_hash")
            if not isinstance(ih, bytes) or len(ih) != 20:
                continue
            ip, sport = parts[1], int(parts[2])
            if q == b"get_peers":
                key = (ip, sport, ih)
                if key in seenp:
                    continue
                seenp.add(key)
                items.append(((ip, sport), ih, "get_peers_querier"))
            elif q == b"announce_peer":
                port = sport if a.get(b"implied_port") else a.get(b"port", sport)
                items.append(((ip, int(port)), ih, "announcer"))
        random.shuffle(items)
        ann = [x for x in items if x[2] == "announcer"]
        gp = [x for x in items if x[2] == "get_peers_querier"][:1500]
        p4 = []

        async def p4one(addr, ih, kind):
            if expired():
                return
            rr = await probe(addr, ih, 0.0)
            rr["kind"] = kind
            p4.append(rr)
        a0 = tcp_attempts
        await asyncio.gather(*[p4one(*x) for x in ann + gp])
        log(f"P4 querier/announcer probes: {len(p4)}, TCP attempts {tcp_attempts-a0}")
        dump("p4_queriers.jsonl", p4)

    loop = asyncio.get_running_loop()
    k = Krpc()
    await loop.create_datagram_endpoint(lambda: k, local_addr=("0.0.0.0", 0))
    await bootstrap(k)

    # ---- P1 lookups ----
    sem = asyncio.Semaphore(64)
    lk = {}

    async def do(ih, kind):
        async with sem:
            if expired():
                return
            r = await lookup(k, ih)
            r["kind"] = kind
            lk[ih] = r
    q0 = k.sent
    await asyncio.gather(*[do(h, "alive") for h in p1_alive] + [do(h, "queued") for h in queued])
    log(f"P1 lookups done: {len(lk)} lookups, {k.sent-q0} queries, answered share "
        f"{k.answered/max(1,k.sent):.2f}")
    dump("p1_lookups.jsonl", [dict(ih=h.hex(), **{x: y for x, y in v.items() if x != 'peers'},
                                   npeers=len(v["peers"]), peers=[f"{a}:{b}" for a, b in v["peers"]])
                              for h, v in lk.items()])

    # ---- P2 probes ----
    p2 = []
    pex_budget = [300]

    async def fetch_hash(ih, v):
        peers = v["peers"][:12]
        rows = []
        async def one(p):
            hold = 0.0
            if pex_budget[0] > 0:
                pex_budget[0] -= 1
                hold = 15.0
            rr = await probe(p, ih, hold)
            if hold and rr["out"] != "hs_ok":
                pex_budget[0] += 1
            rr["kind"] = v["kind"]
            rows.append(rr)
        # <= 4 at once per hash (like production)
        s4 = asyncio.Semaphore(4)
        async def g(p):
            async with s4:
                await one(p)
        await asyncio.gather(*[g(p) for p in peers])
        p2.extend(rows)
    a0 = tcp_attempts
    await asyncio.gather(*[fetch_hash(h, v) for h, v in lk.items() if v["peers"]])
    log(f"P2 probes done: {tcp_attempts-a0} TCP attempts")
    dump("p2_probes.jsonl", p2)

    # good peers
    good = collections.defaultdict(set)
    gv = {}
    for r in p2:
        if r["out"] == "hs_ok":
            good[r["addr"]].add(r["ih"])
            gv[r["addr"]] = r.get("v")
    ranked = sorted(good, key=lambda a: (-len(good[a]), random.random()))
    top = ranked[:150]
    log(f"good peers: {len(good)} distinct; multi-swarm (>=2 of our hashes): "
        f"{sum(1 for a in good if len(good[a])>=2)}")

    # ---- P3 cross test ----
    if not expired():
        new = [(h, "alive_new") for h in new_alive[:75]] + [(h, "queued") for h in queued[:50]]
        random.shuffle(new)
        p3 = []

        async def per_peer(addr):
            ip, port = addr.rsplit(":", 1)
            for h, kind in new:
                if expired():
                    return
                t = time.monotonic()
                rr = await probe((ip, int(port)), h, 0.0)
                rr["kind"] = kind
                rr["peer_hashes_p1"] = len(good[addr])
                rr["peer_v"] = gv.get(addr)
                p3.append(rr)
                # >= 2 s between attempts to the same peer
                await asyncio.sleep(max(0, 1.6 - (time.monotonic() - t)))
        a0 = tcp_attempts
        await asyncio.gather(*[per_peer(a) for a in top])
        log(f"P3 cross test: {len(p3)} pairs, {tcp_attempts-a0} TCP attempts, "
            f"hits {sum(1 for r in p3 if r['out']=='hs_ok')}")
        dump("p3_cross.jsonl", p3)

    json.dump({"krpc_sent": k.sent, "krpc_answered": k.answered,
               "rtt_ms": sorted(int(x * 1000) for x in k.rtts), "tcp_attempts": tcp_attempts,
               "wall_s": time.monotonic() - T0}, open(os.path.join(OUT, "summary.json"), "w"))
    log(f"end: krpc {k.sent} sent / {k.answered} answered, TCP attempts {tcp_attempts}")


if __name__ == "__main__":
    asyncio.run(main())
