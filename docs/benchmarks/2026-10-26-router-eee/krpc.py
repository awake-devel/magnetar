"""Minimal BEP 5 KRPC (bencode over UDP) helpers for a legitimate DHT node probe."""
import os, struct, socket

def benc(x):
    if isinstance(x, int): return b"i%de" % x
    if isinstance(x, str): x = x.encode()
    if isinstance(x, (bytes, bytearray)): return b"%d:%s" % (len(x), bytes(x))
    if isinstance(x, list): return b"l" + b"".join(benc(v) for v in x) + b"e"
    if isinstance(x, dict):
        out = b"d"
        for k in sorted(x, key=lambda k: k if isinstance(k, bytes) else k.encode()):
            kk = k if isinstance(k, bytes) else k.encode()
            out += benc(kk) + benc(x[k])
        return out + b"e"
    raise TypeError(type(x))

def bdec(b, i=0):
    c = b[i:i+1]
    if c == b"i":
        j = b.index(b"e", i); return int(b[i+1:j]), j+1
    if c == b"l":
        i += 1; out = []
        while b[i:i+1] != b"e":
            v, i = bdec(b, i); out.append(v)
        return out, i+1
    if c == b"d":
        i += 1; out = {}
        while b[i:i+1] != b"e":
            k, i = bdec(b, i); v, i = bdec(b, i); out[k] = v
        return out, i+1
    j = b.index(b":", i); n = int(b[i:j]); return b[j+1:j+1+n], j+1+n

def rand_id():
    return os.urandom(20)

def find_node(tid, my_id, target):
    return benc({b"t": tid, b"y": b"q", b"q": b"find_node",
                 b"a": {b"id": my_id, b"target": target}})

def ping(tid, my_id):
    return benc({b"t": tid, b"y": b"q", b"q": b"ping", b"a": {b"id": my_id}})

def parse_nodes(blob):
    """compact node info: 26 bytes each = 20 id + 4 ipv4 + 2 port."""
    out = []
    for i in range(0, len(blob) - 25, 26):
        ip = socket.inet_ntoa(blob[i+20:i+24])
        port = struct.unpack(">H", blob[i+24:i+26])[0]
        if port and not ip.startswith(("0.", "10.", "127.", "192.168.", "169.254.")):
            out.append((ip, port))
    return out
