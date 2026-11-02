import sys
# Splice addbsie=1, addbsil=0, addbsi=0x01 (flag_ec3_extension_type_a) into each E-AC-3 independent frame;
# drop 14 zero bits before the CRC to keep the frame size; recompute crc2 (poly 0x8005 over bytes 2..end).
def bits(b): return ''.join(f'{x:08b}' for x in b)
def crc(data,poly=0x8005):
    c=0
    for x in data:
        c^=x<<8
        for _ in range(8):
            c=((c<<1)^poly)&0xffff if c&0x8000 else (c<<1)&0xffff
    return c
class R:
    def __init__(s,b): s.b=b; s.p=0
    def u(s,n): v=int(s.b[s.p:s.p+n] or '0',2); s.p+=n; return v
def addbsie_pos(fb):
    r=R(fb); r.u(16); st=r.u(2); r.u(3); fs=r.u(11); fscod=r.u(2)
    if fscod==3: r.u(2); nblk=6
    else: nblk=[1,2,3,6][r.u(2)]
    acmod=r.u(3); lfe=r.u(1); r.u(5)
    for i in range(1 if acmod else 2):
        r.u(5)
        if r.u(1): r.u(8)
    if st==1 and r.u(1): r.u(16)
    if r.u(1):
        if acmod>2:
            r.u(2)
            if acmod&1: r.u(6)
            if acmod&4: r.u(6)
        if lfe and r.u(1): r.u(5)
        if st==0:
            for i in range(1 if acmod else 2):
                if r.u(1): r.u(6)
            if r.u(1): r.u(6)
            m=r.u(2)
            if m==1: r.u(5)
            elif m==2: r.u(12)
            elif m==3: r.u((r.u(5)+2)*8)
            if acmod<2:
                for i in range(1 if acmod else 2):
                    if r.u(1): r.u(14)
            if r.u(1):
                for b in range(nblk):
                    if nblk==1 or r.u(1): r.u(5)
    if r.u(1):
        r.u(5)
        if acmod==2: r.u(4)
        if acmod>=6: r.u(2)
        for i in range(1 if acmod else 2):
            if r.u(1): r.u(8)
        if fscod!=3: r.u(1)
    if st==0 and nblk!=6: r.u(1)
    if st==2 and (nblk==6 or r.u(1)): r.u(6)
    return r.p, (fs+1)*2, st
src,dst=sys.argv[1],sys.argv[2]; addbsi=bytes.fromhex(sys.argv[3])
d=bytearray(open(src,'rb').read()); out=bytearray(); i=0; n=0
while i+6<=len(d) and d[i]==0x0b and d[i+1]==0x77:
    fb=bits(d[i:i+64]); p,fs,st=addbsie_pos(fb)
    fr=bits(d[i:i+fs])
    assert fr[p]=='0', 'addbsie already set'
    ins='1'+f'{len(addbsi)-1:06b}'+''.join(f'{x:08b}' for x in addbsi); k=len(ins)-1
    tail=fr[-17-k:-17]
    if st==0 and tail=='0'*k:
        fr=fr[:p]+ins+fr[p+1:-17-k]+fr[-17:]
        assert len(fr)==fs*8
        fbytes=bytearray(int(fr[k:k+8],2) for k in range(0,len(fr),8))
        c=crc(fbytes[2:fs-2]); fbytes[fs-2]=c>>8; fbytes[fs-1]=c&0xff
        out+=fbytes; n+=1
    else:
        out+=d[i:i+fs]
    i+=fs
open(dst,'wb').write(out); print('patched',n,'of frames; bytes',len(out),len(d))
