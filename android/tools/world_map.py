#!/usr/bin/env python3
"""World outline asset for the crawl map (docs/APP.md §15). Standard library only.

Input: Natural Earth 1:110m Admin 0 countries as GeoJSON (public domain),
  https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_110m_admin_0_countries.geojson
Output: android/app/src/main/assets/world_110m.bin

    python3 -I android/tools/world_map.py <ne_110m_admin_0_countries.geojson> <out.bin> [tolerance_deg]

Steps: clip every ring to the latitude band [SOUTH, NORTH] (Antarctica falls out), simplify it
with Douglas-Peucker at `tolerance_deg` (default 0.1 degree), project to equirectangular
(x = lon, y = -lat), quantise to SCALE units per degree and drop rings that collapse.
Each country is keyed by its ISO 3166-1 alpha-2 code: ISO_A2, or ISO_A2_EH when ISO_A2 is -99
(France, Norway), or FALLBACK for the two territories without one, which IP geolocation reports
under the country they are part of. Several features with one code are merged.

Format (big-endian):
    "MGWM" u8 version=1
    u16 scale (units per degree)  i16 west (lon)  i16 north (lat)  u16 width  u16 height
    u16 countries
    per country: 2 ASCII bytes code, u16 rings
        per ring: u16 points, then points x (u16 x, u16 y), open (the last point is not repeated)
x = (lon - west) * scale, y = (north - lat) * scale, so (0, 0) is the north-west corner.

The input is treated as data only: it is parsed with json, never evaluated.
"""

import json
import re
import struct
import sys

SCALE = 10  # units per degree: 0.1 degree, finer than the 1:110m source after simplification
WEST, EAST = -180, 180
NORTH, SOUTH = 84, -58
DEFAULT_TOLERANCE = 0.1
FALLBACK = {"CYN": "CY", "SOL": "SO"}  # N. Cyprus, Somaliland
CODE = re.compile(r"^[A-Z]{2}$")


def clip(ring, keep, cross):
    """Sutherland-Hodgman clip of a closed ring against one half-plane."""
    out = []
    n = len(ring)
    for i in range(n):
        cur, prev = ring[i], ring[i - 1]
        if keep(cur):
            if not keep(prev):
                out.append(cross(prev, cur))
            out.append(cur)
        elif keep(prev):
            out.append(cross(prev, cur))
    return out


def at_lat(lat):
    def cross(a, b):
        t = (lat - a[1]) / (b[1] - a[1])
        return (a[0] + t * (b[0] - a[0]), lat)
    return cross


def seg_dist(p, a, b):
    ax, ay = a
    bx, by = b
    px, py = p
    dx, dy = bx - ax, by - ay
    if dx == 0 and dy == 0:
        return ((px - ax) ** 2 + (py - ay) ** 2) ** 0.5
    t = max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)))
    qx, qy = ax + t * dx, ay + t * dy
    return ((px - qx) ** 2 + (py - qy) ** 2) ** 0.5


def dp(points, tol):
    """Douglas-Peucker of an open polyline, iterative; keeps both ends."""
    if len(points) < 3:
        return list(points)
    keep = [False] * len(points)
    keep[0] = keep[-1] = True
    stack = [(0, len(points) - 1)]
    while stack:
        i, j = stack.pop()
        best, idx = -1.0, -1
        for k in range(i + 1, j):
            d = seg_dist(points[k], points[i], points[j])
            if d > best:
                best, idx = d, k
        if idx >= 0 and best > tol:
            keep[idx] = True
            stack.append((i, idx))
            stack.append((idx, j))
    return [p for p, k in zip(points, keep) if k]


def simplify_ring(ring, tol):
    """A closed ring split at its first point and the point farthest from it."""
    if len(ring) < 4:
        return ring
    first = ring[0]
    far = max(range(len(ring)), key=lambda k: (ring[k][0] - first[0]) ** 2 + (ring[k][1] - first[1]) ** 2)
    a = dp(ring[: far + 1], tol)
    b = dp(ring[far:] + [first], tol)
    return a[:-1] + b[:-1]


def area2(ring):
    return sum(ring[i - 1][0] * ring[i][1] - ring[i][0] * ring[i - 1][1] for i in range(len(ring)))


def quantise(ring):
    out = []
    for lon, lat in ring:
        x = round((lon - WEST) * SCALE)
        y = round((NORTH - lat) * SCALE)
        x = max(0, min((EAST - WEST) * SCALE, x))
        y = max(0, min((NORTH - SOUTH) * SCALE, y))
        if not out or out[-1] != (x, y):
            out.append((x, y))
    while len(out) > 1 and out[0] == out[-1]:
        out.pop()
    return out


def code_of(props):
    for key in ("ISO_A2", "ISO_A2_EH"):
        c = str(props.get(key, "")).strip().upper()
        if CODE.match(c):
            return c
    return FALLBACK.get(str(props.get("ADM0_A3", "")))


def main():
    if len(sys.argv) not in (3, 4):
        sys.exit(__doc__)
    src, dst = sys.argv[1], sys.argv[2]
    tol = float(sys.argv[3]) if len(sys.argv) == 4 else DEFAULT_TOLERANCE
    with open(src, encoding="utf-8") as f:
        data = json.load(f)
    countries = {}
    points_in = 0
    for feat in data.get("features", []):
        props = feat.get("properties") or {}
        geom = feat.get("geometry") or {}
        code = code_of(props)
        if code is None:
            print("skipped (no code):", props.get("NAME"), file=sys.stderr)
            continue
        if geom.get("type") == "Polygon":
            polys = [geom["coordinates"]]
        elif geom.get("type") == "MultiPolygon":
            polys = geom["coordinates"]
        else:
            continue
        for poly in polys:
            for raw in poly:  # outer ring and holes alike: drawn with the even-odd rule
                ring = [(float(p[0]), float(p[1])) for p in raw]
                if len(ring) > 1 and ring[0] == ring[-1]:
                    ring = ring[:-1]
                points_in += len(ring)
                ring = clip(ring, lambda p: p[1] <= NORTH, at_lat(NORTH))
                ring = clip(ring, lambda p: p[1] >= SOUTH, at_lat(SOUTH))
                if len(ring) < 3:
                    continue
                q = quantise(simplify_ring(ring, tol))
                if len(q) < 3 or area2(q) == 0:
                    continue
                countries.setdefault(code, []).append(q)
    out = bytearray(b"MGWM")
    out += struct.pack(">BHhhHHH", 1, SCALE, WEST, NORTH, (EAST - WEST) * SCALE, (NORTH - SOUTH) * SCALE, len(countries))
    points_out = 0
    for code in sorted(countries):
        rings = countries[code]
        out += code.encode("ascii") + struct.pack(">H", len(rings))
        for ring in rings:
            out += struct.pack(">H", len(ring))
            for x, y in ring:
                out += struct.pack(">HH", x, y)
            points_out += len(ring)
    with open(dst, "wb") as f:
        f.write(out)
    rings_out = sum(len(r) for r in countries.values())
    print(f"{len(countries)} countries, {rings_out} rings, {points_in} -> {points_out} points, {len(out)} bytes (tolerance {tol} deg)")


if __name__ == "__main__":
    main()
