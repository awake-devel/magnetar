"""Pi phase 2: ramp KRPC find_node rate across distinct real DHT nodes (one fresh NAT flow
per query, each node <=1 query per pool cycle). Thread monitor watches link health + Pi
CPU/mem/conntrack; abort relative to the Pi's own baseline. Steps <=45 s."""
import asyncio, socket, time, os, sys, json, random
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import krpc
from mon import Monitor, degraded, ctrack, BASE, BL

RATES = [int(x) for x in sys.argv[1].split(",")] if len(sys.argv) > 1 else [25, 50, 100, 200, 300, 400, 600]
STEP_SECS = int(os.environ.get("STEP_SECS", 45))
POOL_CAP = int(os.environ.get("POOL_CAP", 40000))
TAG = os.environ.get("TAG", "_r2")
PER_IP = {}


async def run_step(rate, pool, pool_set, idx, sock, my_id, logf):
    loop = asyncio.get_running_loop()
    m = Monitor(); m.start(); await asyncio.sleep(3)
    t0 = time.time(); sent = recv = sendfail = 0; aborted = None; strikes = 0
    ct_start = ctrack()

    async def reader():
        nonlocal recv
        while True:
            try: data, _ = await loop.sock_recvfrom(sock, 2048)
            except Exception: continue
            recv += 1
            if len(pool) < POOL_CAP:
                try:
                    for np in krpc.parse_nodes(krpc.bdec(data)[0].get(b"r", {}).get(b"nodes", b"")):
                        if np not in pool_set and len(pool) < POOL_CAP and PER_IP.get(np[0], 0) < 3:
                            pool_set.add(np); pool.append(np); PER_IP[np[0]] = PER_IP.get(np[0], 0) + 1
                except Exception: pass
    rt = asyncio.create_task(reader())
    end = t0 + STEP_SECS; last_check = t0
    while time.time() < end:
        now = time.time()
        due = int((now - t0) * rate) - sent - sendfail
        for _ in range(min(due, rate)):   # never burst more than 1 s worth
            node = pool[idx % len(pool)]; idx += 1
            try:
                sock.sendto(krpc.find_node(os.urandom(2), my_id, krpc.rand_id()), node); sent += 1
            except BlockingIOError:
                sendfail += 1
            except OSError:
                sendfail += 1
        await asyncio.sleep(0.005)
        if now - last_check >= 3.0:
            last_check = now
            bad, snap = degraded(m, now)
            if bad:
                strikes += 1
                if strikes >= 2 or (snap["tmed"] or 0) > 150: aborted = snap; break
            else: strikes = 0
    dur = time.time() - t0
    await asyncio.sleep(1.5); rt.cancel(); m.stop()
    row = {"rate": rate, "sent": sent, "send_rate": round(sent / dur, 1), "recv": recv,
           "reply_rate": round(recv / sent, 3) if sent else 0, "sendfail": sendfail,
           "dur": round(dur, 1), "aborted": aborted, "pool": len(pool),
           **{k: m.stats(k, since=t0) for k in ("tcp", "tcp9", "gwtcp", "udp", "icmp", "icmpgw")},
           "pi": m.res_stats(since=t0), "conntrack_start": ct_start}
    print(json.dumps({k: row[k] for k in ("rate", "send_rate", "reply_rate", "sendfail", "dur", "pool", "aborted")}), flush=True)
    print("   tcp", row["tcp"], "gw", row["gwtcp"], "udp", row["udp"], "icmp", row["icmp"], flush=True)
    print("   pi", row["pi"], flush=True)
    logf.write(json.dumps(row, default=str) + "\n"); logf.flush()
    return row, idx


async def recover(max_wait=60):
    m = Monitor(); m.start(); t0 = time.time(); rec = None
    while time.time() - t0 < max_wait:
        await asyncio.sleep(4)
        bad, _ = degraded(m, time.time(), W=4)
        if not bad and (m.stats("tcp", since=time.time() - 4)["n"] or 0) >= 3:
            rec = round(time.time() - t0, 1); break
    m.stop(); print(f"  recovery {rec}s (conntrack {ctrack()})", flush=True)
    return rec


async def ct_drain(limit=45000, max_wait=60):
    t0 = time.time()
    while ctrack() > limit and time.time() - t0 < max_wait: await asyncio.sleep(2)
    w = round(time.time() - t0, 1)
    if w > 1: print(f"  conntrack drain wait {w}s -> {ctrack()}", flush=True)
    return w


async def main():
    pool = []
    for n in json.load(open(os.path.join(BASE, "nodes.json"))):   # <=3 nodes per IP (<=10 q/s per IP at any rate)
        if PER_IP.get(n[0], 0) < 3 and len(pool) < POOL_CAP: PER_IP[n[0]] = PER_IP.get(n[0], 0) + 1; pool.append(tuple(n))
    random.shuffle(pool); pool_set = set(pool)
    print(f"pool={len(pool)} conntrack={ctrack()} baseline={BL}", flush=True)
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); sock.setblocking(False); sock.bind(("0.0.0.0", 0))
    for opt, v in ((socket.SO_SNDBUF, 1 << 20), (socket.SO_RCVBUF, 2 << 20)):
        try: sock.setsockopt(socket.SOL_SOCKET, opt, v)
        except Exception: pass
    my_id = krpc.rand_id()
    logf = open(os.path.join(BASE, f"dht_ramp{TAG}.jsonl"), "w")
    rows = []; idx = 0; prev_reply = None
    for rate in RATES:
        print(f"== DHT {rate} q/s ==", flush=True)
        row, idx = await run_step(rate, pool, pool_set, idx, sock, my_id, logf)
        if row["aborted"] and not row["aborted"]["lat_rise"] and not row["aborted"]["pi_bad"] and row["aborted"]["tloss"] < 0.2:
            print(f"  transient abort at {rate} -> cooldown + retry once", flush=True)
            await recover(); row, idx = await run_step(rate, pool, pool_set, idx, sock, my_id, logf)
        row["recovery_s"] = await recover()
        row["ct_after_recovery"] = ctrack(); rows.append(row)
        if row["aborted"]: print(f"  ABORT at {rate} q/s", flush=True); break
        if row["send_rate"] < rate * 0.8: print(f"  Pi cannot sustain {rate} q/s; stop", flush=True); break
        if prev_reply and row["reply_rate"] < 0.5 * prev_reply:
            print(f"  reply-rate halved ({prev_reply}->{row['reply_rate']}); knee", flush=True); break
        prev_reply = row["reply_rate"]
        await ct_drain()
    json.dump(rows, open(os.path.join(BASE, f"dht_rows{TAG}.json"), "w"), indent=1, default=str)
    print("DONE", flush=True)

asyncio.run(main())
