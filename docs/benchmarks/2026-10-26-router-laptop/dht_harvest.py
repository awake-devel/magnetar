"""Phase 1: crawl the public mainline DHT (BEP 5 find_node) to harvest a large pool of
distinct real node addresses, so the capacity ramp can open one fresh NAT flow per query.
Honest: our own random node id, find_node only, <=10 q/s to any single IP."""
import asyncio, socket, time, os, sys, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import krpc

BOOT = [("router.bittorrent.com", 6881), ("dht.transmissionbt.com", 6881),
        ("router.utorrent.com", 6881), ("dht.libtorrent.org", 25401),
        ("router.bitcomet.com", 6881)]
TARGET_POOL = int(sys.argv[1]) if len(sys.argv) > 1 else 40000
MAX_SECONDS = int(sys.argv[2]) if len(sys.argv) > 2 else 90

async def main():
    loop = asyncio.get_event_loop()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setblocking(False)
    sock.bind(("0.0.0.0", 0))
    my_id = krpc.rand_id()
    pool = {}          # (ip,port) -> seen
    queue = []         # nodes still to query
    inflight_per_ip = {}

    for h, p in BOOT:
        try:
            ip = socket.gethostbyname(h)
            queue.append((ip, p)); pool[(ip, p)] = 1
        except Exception as e:
            print("boot resolve fail", h, e)

    sent = recv = 0
    stop = time.time() + MAX_SECONDS

    async def reader():
        nonlocal recv
        while True:
            try:
                data, addr = await loop.sock_recvfrom(sock, 2048)
            except Exception:
                continue
            recv += 1
            try:
                msg, _ = krpc.bdec(data)
                r = msg.get(b"r", {})
                nodes = r.get(b"nodes", b"")
                for np in krpc.parse_nodes(nodes):
                    if np not in pool:
                        pool[np] = 1
                        queue.append(np)
            except Exception:
                pass

    rt = asyncio.create_task(reader())
    i = 0
    # send find_node at ~800 q/s across many distinct nodes until pool big enough
    while time.time() < stop and len(pool) < TARGET_POOL:
        if not queue:
            await asyncio.sleep(0.05)
            if not queue:
                break
            continue
        batch = queue[:80]; queue = queue[80:]
        for (ip, p) in batch:
            tid = os.urandom(2)
            try:
                await loop.sock_sendto(sock, krpc.find_node(tid, my_id, krpc.rand_id()), (ip, p))
                sent += 1
            except Exception:
                pass
        await asyncio.sleep(0.1)
        i += 1
        if i % 10 == 0:
            print(f"  t={int(time.time()-(stop-MAX_SECONDS))}s pool={len(pool)} sent={sent} recv={recv} q={len(queue)}", flush=True)

    await asyncio.sleep(1.5)
    rt.cancel()
    nodes = list(pool.keys())
    with open(os.path.expanduser("~/.cache/router-bench/nodes.json"), "w") as f:
        json.dump(nodes, f)
    print(f"harvested {len(nodes)} distinct nodes (sent {sent}, recv {recv})")

asyncio.run(main())
