"""Phase 2: ramp KRPC find_node rate across distinct real DHT nodes, one fresh NAT flow
per query, while a thread monitor watches link health. Abort relative to baseline."""
import asyncio, socket, time, os, sys, json, random
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import krpc
from mon import Monitor

# idle baselines
B_TCP_MED, B_TCP_P95, B_UDP_MED = 17.0, 25.0, 18.5
RATES = [50, 100, 200, 300, 400, 600, 800, 1600]
STEP_SECS = 45
CTRACK = "/proc/sys/net/netfilter/nf_conntrack_count"

def ctrack():
    try: return int(open(CTRACK).read())
    except Exception: return -1

def degraded(m, now, W=15):
    tp95 = m.stats("tcp", since=now - W)["p95"] or 0
    tmed = m.stats("tcp", since=now - W)["med"] or 0
    tloss = m.recent_loss("tcp", W) or 0
    umed = m.stats("1.1.1.1", since=now - W)["med"] or 0
    uloss = m.recent_loss("1.1.1.1", W) or 0
    gmed = m.stats("gwtcp", since=now - W)["med"] or 0
    # Router NAT/CPU saturation shows as a sharp MEDIAN latency rise (robust). Isolated loss
    # blips while median stays at baseline are external contention (the concurrent agent
    # swarm), not the router -- so loss only counts when paired with elevated latency.
    # p95 dropped: a single ~1s SYN-RTO outlier to anycast (normal, occurs at idle) dominated
    # p95 in small windows. Median is the robust saturation signal (hit 239ms at the knee).
    bad = (tmed > 3 * B_TCP_MED or umed > 3 * B_UDP_MED or gmed > 10
           or (tloss > 0.25 and tmed > 2 * B_TCP_MED)
           or (uloss > 0.25 and umed > 2 * B_UDP_MED))
    snap_lat_rise = tmed > 2 * B_TCP_MED or umed > 2 * B_UDP_MED
    return bad, dict(tp95=round(tp95), tmed=round(tmed), tloss=round(tloss, 2),
                     umed=round(umed), uloss=round(uloss, 2), gmed=round(gmed, 1),
                     lat_rise=snap_lat_rise)

async def run_step(rate, pool, pool_set, idx, sock, my_id, logf):
    loop = asyncio.get_event_loop()
    m = Monitor(); m.start()
    await asyncio.sleep(3)
    t0 = time.time(); sent = recv = sendfail = 0
    ct_start = ctrack(); ct_peak = ct_start
    aborted = None; strikes = 0

    async def reader():
        nonlocal recv
        while True:
            try: data, _ = await loop.sock_recvfrom(sock, 2048)
            except Exception: continue
            recv += 1
            try:
                for np in krpc.parse_nodes(krpc.bdec(data)[0].get(b"r", {}).get(b"nodes", b"")):
                    if np not in pool_set: pool_set.add(np); pool.append(np)
            except Exception: pass
    rt = asyncio.create_task(reader())

    interval = 1.0 / rate; next_send = time.time(); last_check = time.time(); end = t0 + STEP_SECS
    while time.time() < end:
        now = time.time()
        if now >= next_send:
            node = pool[idx % len(pool)]; idx += 1
            try:
                await loop.sock_sendto(sock, krpc.find_node(os.urandom(2), my_id, krpc.rand_id()), node)
                sent += 1
            except OSError: sendfail += 1
            next_send += interval
            if next_send < now - 0.5: next_send = now
        else:
            await asyncio.sleep(min(interval, 0.002))
        ct_peak = max(ct_peak, ctrack())
        if now - last_check >= 3.0:
            last_check = now
            bad, snap = degraded(m, now)
            if bad:
                strikes += 1
                if strikes >= 2 or snap["tmed"] > 150 or snap["umed"] > 150:
                    aborted = snap; break
            else: strikes = 0

    dur = time.time() - t0
    await asyncio.sleep(1.0); rt.cancel(); m.stop()
    row = {"rate": rate, "sent": sent, "send_rate": round(sent / dur, 1), "recv": recv,
           "reply_rate": round(recv / sent, 3) if sent else 0, "sendfail": sendfail,
           "dur": round(dur, 1), "aborted": aborted,
           "tcp": m.stats("tcp", since=t0), "tcp9": m.stats("tcp9", since=t0),
           "gwtcp": m.stats("gwtcp", since=t0), "udp": m.stats("1.1.1.1", since=t0),
           "conntrack_start": ct_start, "conntrack_peak": ct_peak}
    print(json.dumps({k: row[k] for k in ("rate","send_rate","reply_rate","sendfail","dur","aborted")}), flush=True)
    print("   tcp", row["tcp"], "udp", row["udp"], "gw", row["gwtcp"], "ct", ct_start, "->", ct_peak, flush=True)
    logf.write(json.dumps(row, default=str) + "\n"); logf.flush()
    return row, idx

async def recover(max_wait=60):
    m = Monitor(); m.start(); t0 = time.time(); rec = None
    while time.time() - t0 < max_wait:
        await asyncio.sleep(5)
        bad, _ = degraded(m, time.time(), W=8)
        if not bad and (m.stats("tcp", since=time.time()-8)["n"] or 0) >= 3:
            rec = round(time.time() - t0, 1); break
    m.stop()
    print(f"  recovery {rec}s (conntrack {ctrack()})", flush=True)
    return rec

async def main():
    pool = [tuple(n) for n in json.load(open(os.path.expanduser("~/.cache/router-bench/nodes.json")))]
    random.shuffle(pool); pool_set = set(pool)
    print(f"pool={len(pool)} conntrack={ctrack()}", flush=True)
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); sock.setblocking(False); sock.bind(("0.0.0.0", 0))
    try:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4*1024*1024)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 8*1024*1024)
    except Exception: pass
    my_id = krpc.rand_id()
    logf = open(os.path.expanduser("~/.cache/router-bench/dht_ramp.jsonl"), "w")
    rows = []; idx = 0; prev_reply = None
    for rate in RATES:
        print(f"== DHT {rate} q/s ==", flush=True)
        row, idx = await run_step(rate, pool, pool_set, idx, sock, my_id, logf)
        # abort with no latency rise => external contention, not the router: retry once
        if row["aborted"] and not row["aborted"].get("lat_rise"):
            print(f"  transient abort (no latency rise) at {rate} q/s -> cooldown + retry once", flush=True)
            await recover()
            row, idx = await run_step(rate, pool, pool_set, idx, sock, my_id, logf)
        row["recovery_s"] = await recover()
        rows.append(row)
        if row["aborted"]: print(f"  ABORT at {rate} q/s"); break
        if row["send_rate"] < rate * 0.6: print(f"  cannot sustain {rate} (laptop/pacing); stop"); break
        if prev_reply and row["reply_rate"] < 0.5 * prev_reply:
            print(f"  reply-rate halved vs prev ({prev_reply}->{row['reply_rate']}); knee"); break
        prev_reply = row["reply_rate"]
    json.dump(rows, open(os.path.expanduser("~/.cache/router-bench/dht_rows.json"), "w"), indent=2, default=str)
    print("DONE", flush=True)

asyncio.run(main())
