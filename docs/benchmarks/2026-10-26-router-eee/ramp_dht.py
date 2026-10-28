"""EEE: ramp KRPC find_node rate across distinct real DHT nodes (one fresh NAT flow per query)
while the monitor watches WAN/gateway/service health and CPU. Abort relative to EEE baseline."""
import asyncio, socket, time, os, sys, json, random
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import krpc
from mon import Monitor, BASE, load_baseline, degraded

RATES = [int(x) for x in sys.argv[1].split(",")] if len(sys.argv) > 1 else [50, 100, 200, 300, 400, 500, 600, 800]
STEP_SECS = 45
B = load_baseline()
CTRACK = "/proc/sys/net/netfilter/nf_conntrack_count"

def ctrack():
    try: return int(open(CTRACK).read())
    except Exception: return -1

async def run_step(rate, pool, idx, sock, my_id, logf):
    loop = asyncio.get_event_loop()
    m = Monitor(); m.start()
    await asyncio.sleep(3)
    t0 = time.time(); sent = recv = sendfail = skipped = 0
    ct_start = ctrack(); ct_peak = ct_start
    aborted = None; strikes = 0; last_ip = {}

    async def reader():
        nonlocal recv
        while True:
            try: await loop.sock_recvfrom(sock, 2048)
            except Exception: continue
            recv += 1
    rt = asyncio.create_task(reader())

    interval = 1.0 / rate; next_send = time.time(); last_check = time.time(); end = t0 + STEP_SECS
    while time.time() < end:
        now = time.time()
        if now >= next_send:
            node = pool[idx % len(pool)]; idx += 1
            if now - last_ip.get(node[0], 0) < 0.2:          # <=5 q/s per IP
                skipped += 1; continue
            last_ip[node[0]] = now
            try:
                await loop.sock_sendto(sock, krpc.find_node(os.urandom(2), my_id, krpc.rand_id()), node)
                sent += 1
            except OSError: sendfail += 1
            next_send += interval
            if next_send < now - 0.5: next_send = now
        else:
            await asyncio.sleep(min(interval, 0.002))
        if now - last_check >= 3.0:
            last_check = now
            ct_peak = max(ct_peak, ctrack())
            bad, hard, snap = degraded(m, now, B)
            if bad:
                strikes += 1
                print(f"   t={now-t0:.0f}s degraded {snap}", flush=True)
                if strikes >= 2 or hard:
                    aborted = snap; break
            else: strikes = 0

    dur = time.time() - t0
    await asyncio.sleep(1.0); rt.cancel(); m.stop()
    row = {"rate": rate, "sent": sent, "send_rate": round(sent / dur, 1), "recv": recv,
           "reply_rate": round(recv / sent, 3) if sent else 0, "sendfail": sendfail, "skipped_same_ip": skipped,
           "dur": round(dur, 1), "aborted": aborted, "cpu": m.cpu_stats(since=t0),
           **{k: m.stats(k, since=t0) for k in m.keys},
           "conntrack_start": ct_start, "conntrack_peak": ct_peak}
    print(json.dumps({k: row[k] for k in ("rate", "send_rate", "reply_rate", "sendfail", "dur", "aborted", "cpu")}), flush=True)
    print("   tcp", row["tcp"], "udp", row["1.1.1.1"], "icmp", row["icmp"], "gw", row["gwtcp"],
          "svc", row["svc"], "svc2", row["svc2"], "ct", ct_start, "->", ct_peak, flush=True)
    logf.write(json.dumps(row, default=str) + "\n"); logf.flush()
    return row, idx

async def recover(max_wait=60):
    m = Monitor(); m.start(); t0 = time.time(); rec = None
    await asyncio.sleep(2)
    while time.time() - t0 < max_wait:
        await asyncio.sleep(3)
        bad, _, snap = degraded(m, time.time(), B, W=6)
        if not bad and (m.stats("tcp", since=time.time() - 6)["n"] or 0) >= 3:
            rec = round(time.time() - t0, 1); break
    m.stop()
    print(f"  recovery {rec}s (conntrack {ctrack()})", flush=True)
    return rec

async def main():
    pool = [tuple(n) for n in json.load(open(os.path.join(BASE, "nodes.json")))]
    random.shuffle(pool)
    print(f"pool={len(pool)} conntrack={ctrack()} baseline tcp={B.get('tcp')}", flush=True)
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); sock.setblocking(False); sock.bind(("0.0.0.0", 0))
    try:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4*1024*1024)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 8*1024*1024)
    except Exception: pass
    my_id = krpc.rand_id()
    logf = open(os.path.join(BASE, "dht_ramp.jsonl"), "a")
    rows = []; idx = 0; prev_reply = None
    for rate in RATES:
        print(f"== DHT {rate} q/s ==", flush=True)
        row, idx = await run_step(rate, pool, idx, sock, my_id, logf)
        if row["aborted"] and not row["aborted"].get("lat_rise") and set(row["aborted"]["reasons"]) <= {"svc2"}:
            print(f"  abort only on noisy svc2 at {rate} -> cooldown + retry once", flush=True)
            await recover()
            row, idx = await run_step(rate, pool, idx, sock, my_id, logf)
        row["recovery_s"] = await recover()
        rows.append(row)
        if row["aborted"]: print(f"  ABORT at {rate} q/s", flush=True); break
        if row["send_rate"] < rate * 0.8: print(f"  cannot sustain {rate} (EEE CPU/pacing); stop", flush=True); break
        if prev_reply and row["reply_rate"] < 0.5 * prev_reply:
            print(f"  reply-rate halved vs prev ({prev_reply}->{row['reply_rate']}); knee", flush=True); break
        prev_reply = row["reply_rate"]
    json.dump(rows, open(os.path.join(BASE, "dht_rows.json"), "w"), indent=2, default=str)
    print("DONE", flush=True)

asyncio.run(main())
