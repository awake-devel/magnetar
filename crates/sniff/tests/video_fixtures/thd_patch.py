import sys
def crc(data,poly=0x2d):
    c=0
    for x in data:
        c^=x<<8
        for _ in range(8):
            c=((c<<1)^poly)&0xffff if c&0x8000 else (c<<1)&0xffff
    return c
src,dst,nsub,info=sys.argv[1],sys.argv[2],int(sys.argv[3]),int(sys.argv[4],0)
d=bytearray(open(src,'rb').read())
i=0;n=0
while True:
    i=d.find(b'\xf8\x72\x6f\xba',i)
    if i<0: break
    b=d[i:i+28]
    if (crc(b[:24])^((b[24]<<8)|b[25]))==((b[26]<<8)|b[27]):
        d[i+16]=(d[i+16]&0x0f)|(nsub<<4)
        d[i+17]=info
        c=crc(d[i:i+24])^((d[i+24]<<8)|d[i+25])
        d[i+26]=c>>8; d[i+27]=c&0xff; n+=1
    i+=4
open(dst,'wb').write(d); print('patched',n)
