import json,collections,statistics as st
L=lambda f:[json.loads(l) for l in open(f)]
p1=L('p1_lookups.jsonl'); p2=L('p2_probes.jsonl'); p3=L('p3_cross.jsonl'); p4=L('p4_queriers.jsonl'); S=json.load(open('summary.json'))
def q(xs,p): xs=sorted(xs); return xs[int(p*(len(xs)-1))] if xs else None
print('== P1 lookups')
for kind in ['alive','queued']:
    r=[x for x in p1 if x['kind']==kind]
    wp=[x for x in r if x['npeers']>0]
    print(kind,'n',len(r),'with peers',len(wp),f"{len(wp)/len(r):.2f}",'queries mean',round(st.mean(x['queried'] for x in r),1),'median',st.median(x['queried'] for x in r),
      'secs median',round(st.median(x['secs'] for x in r),1),'p90',round(q([x['secs'] for x in r],.9),1),
      'first-value q median',st.median([x['first_value_q'] for x in wp]),'t',round(st.median([x['first_value_t'] for x in wp]),2),
      'q12 median',st.median([x['q12'] for x in wp if x['q12']] or [0]), 'reach12',sum(1 for x in wp if x['q12']),
      'peers median',st.median(x['npeers'] for x in wp))
rt=S['rtt_ms']; print('KRPC rtt ms p50/p90/p99/max',q(rt,.5),q(rt,.9),q(rt,.99),max(rt),'answered',S['krpc_answered'],'/',S['krpc_sent'])
print('share of answers >1s',sum(1 for x in rt if x>1000)/len(rt),'>2s',sum(1 for x in rt if x>2000)/len(rt))
print('== P2 probes')
acc_peer_ids=set()
for kind in ['alive','queued']:
    r=[x for x in p2 if x['kind']==kind]
    c=collections.Counter(x['out'] for x in r)
    hashes=collections.defaultdict(list)
    for x in r: hashes[x['ih']].append(x)
    ok=[h for h,rs in hashes.items() if any(y['out']=='hs_ok' and y.get('metadata_size') for y in rs)]
    print(kind,'attempts',len(r),dict(c),'hashes probed',len(hashes),'hashes with a real (metadata_size) peer',len(ok))
fake=[x for x in p2 if x['out']=='hs_ok' and not x.get('ext')]
print('hs_ok without ext handshake (fake/accept-all-like):',len(fake),collections.Counter(x.get('peer_id','')[:3] for x in fake).most_common(5))
pex=[x for x in p2 if 'pex_peers' in x]
print('PEX sessions',len(pex),'advertised ut_pex',sum(1 for x in pex if x.get('has_ut_pex')),'got >=1 pex msg',sum(1 for x in pex if x.get('pex_msgs')),
  'peers per pex session mean',round(st.mean(len(x['pex_peers']) for x in pex),1) if pex else 0,
  'median among those with msg',st.median([len(x['pex_peers']) for x in pex if x.get('pex_msgs')] or [0]),
  'first pex t median',st.median([x['pex_t'] for x in pex if x.get('pex_t')] or [0]))
# PEX novelty vs lookup peers for same hash
lk={x['ih']:set(x['peers']) for x in p1}
nov=[len(set(x['pex_peers'])-lk.get(x['ih'],set())) for x in pex if x.get('pex_msgs')]
print('PEX new peers not in the lookup values: mean',round(st.mean(nov),1) if nov else 0,'total',sum(nov))
vs=collections.Counter((x.get('v') or '?').split('/')[0].split(' ')[0] for x in p2 if x['out']=='hs_ok' and x.get('ext'))
print('clients of good peers',vs.most_common(10))
print('== P3 cross')
byp=collections.defaultdict(list)
for r in p3: byp[r['addr']].append(r)
acc={a for a,rs in byp.items() if sum(r['out']=='hs_ok' for r in rs)>len(rs)*0.5}
real=[r for r in p3 if r['out']=='hs_ok' and r['addr'] not in acc and r.get('metadata_size')]
anyhs=[r for r in p3 if r['out']=='hs_ok' and r['addr'] not in acc]
n=len([r for r in p3 if r['addr'] not in acc])
print('pairs',len(p3),'accept-all peers',len(acc),'pairs excl',n,'hs_ok excl',len(anyhs),'with metadata_size',len(real),f"rate {len(real)/n:.4%}")
for kind in ['alive_new','queued']:
    nk=len([r for r in p3 if r['addr'] not in acc and r['kind']==kind]); rk=[r for r in real if r['kind']==kind]
    hk=len(set(r['ih'] for r in p3 if r['kind']==kind))
    print(kind,'pairs',nk,'real hits',len(rk),f"{len(rk)/nk:.4%}",'hashes with >=1 real hit',len(set(r['ih'] for r in rk)),'/',hk)
multi=[a for a in byp if a not in acc and byp[a][0]['peer_hashes_p1']>=2]
mh=[r for r in real if r['peer_hashes_p1']>=2]; mn=sum(len(byp[a]) for a in multi)
print('multi-swarm peers',len(multi),'pairs',mn,'hits',len(mh), f"{len(mh)/max(1,mn):.4%}")
sh=[r for r in real if r['peer_hashes_p1']<2]; sn=n-mn
print('single-swarm peers pairs',sn,'hits',len(sh),f"{len(sh)/max(1,sn):.4%}")
print('P3 outcomes excl accept-all',collections.Counter(r['out'] for r in p3 if r['addr'] not in acc))
print('== P4')
for kind in ['announcer','get_peers_querier']:
    r=[x for x in p4 if x['kind']==kind]; c=collections.Counter(x['out'] for x in r)
    real4=[x for x in r if x['out']=='hs_ok' and x.get('metadata_size')]
    print(kind,'n',len(r),dict(c),'real (metadata_size)',len(real4),f"{len(real4)/max(1,len(r)):.2%}",'distinct hashes real',len(set(x['ih'] for x in real4)),'of',len(set(x['ih'] for x in r)))
    print('  clients', collections.Counter((x.get('v') or '?')[:14] for x in real4).most_common(6))
