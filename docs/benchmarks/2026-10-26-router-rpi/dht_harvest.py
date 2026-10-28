"""Pi phase 1: gentle BEP 5 find_node crawl to harvest <=20k distinct real DHT nodes.
~RATE q/s total (default 150, kept low because the Pi's own conntrack max is 7680 and
unreplied UDP entries live 30 s), each node queried at most once."""
import asyncio, socket, time, os, sys, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import krpc
from mon import Monitor, BASE

BOOT = [("router.bittorrent.com", 6881), ("dht.transmissionbt.com", 6881),
        ("router.utorrent.com", 6881), ("dht.libtorrent.org", 25401)]
TARGET_POOL = int(sys.argv[1]) if len(sys.argv) > 1 else 20000
MAX_SECONDS = int(sys.argv[2]) if len(sys.argv) > 2 else 120
RATE = int(sys.argv[3]) if len(sys.argv) > 3 else 150


async def main():
    loop = asyncio.get_running_loop()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); sock.setblocking(False); sock.bind(("0.0.0.0", 0))
    my_id = krpc.rand_id(); pool = {}; queue = []
    for h, p in BOOT:
        try:
            ip = socket.gethostbyname(h); queue.append((ip, p)); pool[(ip, p)] = 1
        except Exception as e:
            print("boot resolve fail", h, e)
    m = Monitor(); m.start()
    sent = recv = 0; t0 = time.time(); stop = t0 + MAX_SECONDS

    async def reader():
        nonlocal recv
        while True:
            try: data, _ = await loop.sock_recvfrom(sock, 2048)
            except Exception: continue
            recv += 1
            try:
                for np in krpc.parse_nodes(krpc.bdec(data)[0].get(b"r", {}).get(b"nodes", b"")):
                    if np not in pool and len(pool) < TARGET_POOL:
                        pool[np] = 1; queue.append(np)
            except Exception: pass
    rt = asyncio.create_task(reader())
    per_tick = max(1, RATE // 10); i = 0
    while time.time() < stop and len(pool) < TARGET_POOL:
        if not queue:
            await asyncio.sleep(0.2)
            if not queue: break
            continue
        batch = queue[:per_tick]; del queue[:per_tick]
        for np in batch:
            try:
                await loop.sock_sendto(sock, krpc.find_node(os.urandom(2), my_id, krpc.rand_id()), np); sent += 1
            except OSError: pass
        await asyncio.sleep(0.1); i += 1
        if i % 50 == 0:
            r = m.last_res()
            print(f"  t={int(time.time()-t0)}s pool={len(pool)} sent={sent} recv={recv} cpu={r.get('cpu')} "
                  f"mem={r.get('mem_avail_mb')} ct={r.get('ct')} tcp={m.stats('tcp', since=time.time()-5)['med']}", flush=True)
    await asyncio.sleep(1.5); rt.cancel(); m.stop()
    json.dump(list(pool.keys()), open(os.path.join(BASE, "nodes.json"), "w"))
    out = {"pool": len(pool), "sent": sent, "recv": recv, "secs": round(time.time() - t0, 1),
           "tcp": m.stats("tcp", since=t0), "pi": m.res_stats(since=t0)}
    print("HARVEST", json.dumps(out), flush=True)

asyncio.run(main())
