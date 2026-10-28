import json, re, os, sys, datetime as dt
L=os.path.expanduser('~/.cache/magnetar-balance-scratch/logs')
runs=[r for r in ['main1','new1','main2','new2','main3','new3','main4','new4','main5','new5'] if os.path.getsize(f'{L}/{r}/report.json' if os.path.exists(f'{L}/{r}/report.json') else '/dev/null')>0]
print('| run | start | build | stored/min | unbraked stored/min | hashes/min | metadata/min | q/s | sample q share | paused s | brake | queue end (sampled) | share per minute | stored per 1000 queries sent (sample route, D107) | CPU |')
print('|'+'---|'*15)
for r in runs:
    rep=json.load(open(f'{L}/{r}/report.json'))
    start=open(f'{L}/{r}/start.txt').readline().strip()
    t0=dt.datetime.fromisoformat(start)
    err=open(f'{L}/{r}/stderr.log',errors='replace').read()
    brakes=[]
    for m in re.finditer(r'(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)\.\d+Z.{0,40}?WARN.{0,80}saturated',err):
        t=dt.datetime.fromisoformat(m.group(1)+'+00:00'); brakes.append((t-t0).total_seconds())
    braked=set()
    for b in brakes:
        for s in range(0,630,30):
            if b-30 < s <= b+300+30: braked.add(s)
    # stored per 30s window
    prev=0; rates=[]
    for line in open(f'{L}/{r}/samples.tsv'):
        f=line.rstrip('\n').split('\t'); t=int(f[0])
        n=sum(int(x.split('=')[1]) for x in f[3].split(',') if '=' in x)
        if t>60 and t not in braked: rates.append((n-prev)*2)
        prev=n
    ub=sum(rates)/len(rates) if rates else float('nan')
    shares=[float(x) for x in re.findall(r'DHT balance: \w+ share ([0-9.]+)',err)]
    e=rep['engine']; sq=e['sent_sample_infohashes']; gp=e['sent_get_peers']
    bstr=('%d (%s)'%(len(brakes),','.join('%ds'%b for b in brakes[:1]))) if brakes else '0'
    print(f"| {r} | {start[11:16]} | {'main' if r.startswith('main') else 'D107'} | {rep['persist']['stored']/(rep['seconds']/60):.0f} | {ub:.0f} ({len(rates)}×30 s) | {rep['unique_hashes_per_min']:.0f} | {rep['metadata_per_min']:.0f} | {rep['queries_per_sec']:.0f} | {sq/(sq+gp)*100:.2f} % | {rep['sampling_paused_secs']:.0f} | {bstr} | {rep['queue_len']} ({rep['queue_lanes'][1]}) | {' '.join('%.3f'%s for s in shares) or '1 (D62)'} | {rep['persist']['stored']*1000/e['queries_sent']:.1f}{(' (%.1f)'%rep['stored_per_kq'][0]) if 'stored_per_kq' in rep else ''} | {rep['cpu_seconds']/rep['seconds']:.2f} |")
