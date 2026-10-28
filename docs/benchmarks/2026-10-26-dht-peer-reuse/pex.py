"""Bounded PEX follow-up: <=120 known real peers (P2), one session each with their own hash,
ut_pex advertised, held 75 s, counting ut_pex messages and added peers. Raw m dicts logged."""
import asyncio, json, struct, os, sys, time, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import exp
async def sess(addr, ih, out):
    ip, port = addr.rsplit(':',1)
    async with exp.tcp_open:
        await exp.tcp_pace.take()
        res={'addr':addr,'ih':ih,'out':'connect'}; out.append(res)
        try: r,w=await asyncio.wait_for(asyncio.open_connection(ip,int(port)),3)
        except Exception: return
        t=time.monotonic(); ihb=bytes.fromhex(ih)
        try:
            rsv=bytearray(8); rsv[5]|=0x10
            w.write(b"\x13BitTorrent protocol"+bytes(rsv)+ihb+exp.PEER_ID); await w.drain()
            hs=await asyncio.wait_for(r.readexactly(68),5)
            if hs[28:48]!=ihb: res['out']='wrong'; return
            res['out']='hs_ok'
            body=b"\x14\x00"+exp.benc({b"m":{b"ut_metadata":1,b"ut_pex":2}})
            w.write(struct.pack(">I",len(body))+body); await w.drain()
            peers=set(); res['pex_msgs']=0
            end=t+75
            while time.monotonic()<end:
                ln=struct.unpack(">I",await asyncio.wait_for(r.readexactly(4),end-time.monotonic()))[0]
                if ln==0: continue
                if ln>1<<20: break
                m=await asyncio.wait_for(r.readexactly(ln),end-time.monotonic())
                if m[0]==20 and m[1]==0:
                    d=exp.decode(m[2:]) or {}
                    res['m']={k.decode('latin1'):v for k,v in (d.get(b'm') or {}).items() if isinstance(v,int)}
                    res['v']=(d.get(b'v') or b'').decode('utf8','replace')[:30]
                elif m[0]==20 and m[1]==2:
                    d=exp.decode(m[2:]) or {}
                    a=d.get(b'added',b'')
                    for i in range(0,len(a)-len(a)%6,6):
                        p=exp.compact_peer(a[i:i+6])
                        if p: peers.add(f"{p[0]}:{p[1]}")
                    res["pex_peers"]=sorted(peers); res["pex_msgs"]+=1; res.setdefault('pex_first_t',round(time.monotonic()-t,1))
                    # keepalive so we are not dropped as idle
                elif m[0]==20: res.setdefault('other_ext',[]).append(m[1])
            res['pex_peers']=sorted(peers)
        except Exception as e:
            res['err']=type(e).__name__
        finally:
            res['held']=round(time.monotonic()-t,1)
            try: w.close()
            except Exception: pass
async def main():
    p2=[json.loads(l) for l in open('results/p2_probes.jsonl')]
    cand={}
    for x in p2:
        if x['out']=='hs_ok' and x.get('metadata_size') and x['addr'] not in cand: cand[x['addr']]=x['ih']
    items=list(cand.items())[:120]
    exp.log(f"PEX follow-up start: {len(items)} peers, hold 75 s")
    out=[]
    async def keep(): pass
    await asyncio.gather(*[sess(a,h,out) for a,h in items])
    with open('results/pex.jsonl','w') as f:
        for r in out: f.write(json.dumps(r)+"\n")
    exp.log(f"PEX follow-up end: {len(out)} attempts")
asyncio.run(main())
