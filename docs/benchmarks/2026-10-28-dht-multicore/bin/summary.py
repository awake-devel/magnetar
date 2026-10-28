#!/usr/bin/env python3
"""One table row per run directory: report.json + samples.txt + env.txt."""
import json, sys, os, re, statistics
def row(d):
    try: r=json.load(open(f'{d}/report.json'))
    except Exception: return f'{os.path.basename(d)}: no report'
    env=dict(l.strip().split('=',1) for l in open(f'{d}/env.txt'))
    e=r['engine']; p=r['peers']; secs=r['seconds']
    smp=[l.split() for l in open(f'{d}/samples.txt').read().splitlines()[1:]]
    cores=[float(x[1]) for x in smp if len(x)>1 and x[1] not in ('','na')]
    conn=[float(x[-2]) for x in smp if len(x)>2]
    ct=[int(x[-3]) for x in smp if len(x)>3 and x[-3].isdigit()]
    other=[int(x[-1]) for x in smp if len(x)>3 and x[-1].isdigit()]
    st=open(f'{d}/start').read().strip()[11:16]
    guard=os.path.exists(f'{d}/guard')
    h=r.get('health',{})
    f=r['fetch']; lk=env.get('MAGNETAR_DHT_PEER_LOOKUPS'); tr=env.get('MAGNETAR_DHT_TCP_RATE'); pr=env.get('MAGNETAR_DHT_SAMPLE_PRIORITY','0')
    return ('| {st} | {b} | {n} | {q} | {lk} | {tr} | {pr} | {mm:.0f} | {hm:.0f} | {col:.0f} | {ans:.2f} | {qs:.0f} ({pc:.0f} %) | {smp:.1f} % | {sp:.0f} s | {tcp:.0f} | {fs:.1f} % | {cpu:.2f} / {cpum:.2f} | {rss:.0f} | {ct}k | {cm:.0f} / {cx:.0f} ms | {g} |').format(
        lk=lk, tr=tr, pr=pr, col=r['peers']['hashes']/secs*60, smp=100*e['sent_sample_infohashes']/max(1,e['queries_sent']), tcp=f['peers_tried']/secs,
        st=st, b=os.path.basename(d).split('-')[1], n=env['MAGNETAR_DHT_NODES'], q=env['MAGNETAR_DHT_QPS'],
        mm=r['metadata_per_min'], hm=r['unique_hashes_per_min'], ans=r['answered_share'],
        qs=r['queries_per_sec'], pc=100*r['queries_per_sec']/int(env['MAGNETAR_DHT_QPS']),
        fs=100*r['fetch_success_rate'], sp=r['sampling_paused_secs'],
        cpu=r['cpu_seconds']/secs, cpum=max(cores) if cores else 0, rss=r['rss_peak_mb'],
        ct=max(ct)//1000 if ct else 0, cm=1000*statistics.median(conn) if conn else 0, cx=1000*max(conn) if conn else 0,
        oth=max(other) if other else '-', g='guard' if guard else ('brake %d'%e.get('brake_engaged',0)))
print('| start | build | nodes | QPS | lookups | TCP rate | sample priority | metadata/min | hashes/min | collected/min | answered | sent q/s (of cap) | sample queries | sampling paused | TCP attempts/s | fetch success | CPU mean / max 10 s (cores) | RSS MB | conntrack max | 1.1.1.1 connect median / max | brakes |')
print('|'+'---|'*21)
for d in sys.argv[1:]: print(row(d.rstrip('/')))
