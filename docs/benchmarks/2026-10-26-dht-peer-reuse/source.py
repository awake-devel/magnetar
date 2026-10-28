"""Bounded test of the BEP 51 'source node' path: sample_infohashes to <=300 nodes, then for <=5
samples per responder a get_peers to that same node; values probed by BEP 3 handshake (<=400)."""
import asyncio, json, os, sys, random, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import exp
async def main():
    loop=asyncio.get_running_loop(); k=exp.Krpc()
    await loop.create_datagram_endpoint(lambda:k, local_addr=("0.0.0.0",0))
    exp.log("source test start"); await exp.bootstrap(k)
    nodes=random.sample(list(k.nodes.keys()), min(300,len(k.nodes)))
    rs=await asyncio.gather(*[k.query(a,b"sample_infohashes",{b"target":os.urandom(20)},4.0) for a in nodes])
    resp=[(a,r) for a,r in zip(nodes,rs) if r is not None]
    sup=[(a,r) for a,r in resp if isinstance(r.get(b"samples"),bytes) and len(r[b"samples"])>=20]
    exp.log(f"sample_infohashes: {len(nodes)} asked, {len(resp)} answered, {len(sup)} with samples")
    jobs=[]
    for a,r in sup:
        s=r[b"samples"]; hs=[s[i:i+20] for i in range(0,len(s)-len(s)%20,20)]
        for h in random.sample(hs,min(5,len(hs))): jobs.append((a,h))
    async def gp(a,h):
        await asyncio.sleep(random.random()*20)  # spread: <=1 q/s per node on average
        r=await k.query(a,b"get_peers",{b"info_hash":h},4.0)
        vals=[]
        if r and isinstance(r.get(b"values"),list):
            vals=[p for p in (exp.compact_peer(v) for v in r[b"values"] if isinstance(v,bytes)) if p]
        return a,h,r is not None,vals
    g=await asyncio.gather(*[gp(a,h) for a,h in jobs])
    exp.log(f"get_peers to source: {len(g)} queries")
    probes=[]
    todo=[(h,v) for a,h,ok,vals in g for v in vals[:3]]
    random.shuffle(todo); todo=todo[:400]
    async def pr(h,v):
        x=await exp.probe(v,h,0.0); probes.append(x)
    await asyncio.gather(*[pr(h,v) for h,v in todo])
    out={'asked':len(nodes),'answered':len(resp),'with_samples':len(sup),'gp_queries':len(g),
         'gp_answered':sum(1 for x in g if x[2]),'gp_with_values':sum(1 for x in g if x[3]),
         'values_per_hit':[len(x[3]) for x in g if x[3]],'probes':len(probes),
         'probe_out':collections.Counter(p['out'] for p in probes),
         'probe_real':sum(1 for p in probes if p['out']=='hs_ok' and p.get('metadata_size')),
         'hashes_real':len(set(p['ih'] for p in probes if p['out']=='hs_ok' and p.get('metadata_size'))),
         'hashes_probed':len(set(p['ih'] for p in probes))}
    json.dump(out,open('results/source.json','w'),default=str); exp.log(f"source test end: {out}")
asyncio.run(main())
