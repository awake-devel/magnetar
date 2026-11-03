// Port of the parser of android/.../ui/crawlmap/WorldMap.kt: the world outline of the crawl map,
// Natural Earth 1:110m Admin 0 countries (public domain), simplified, equirectangular, quantised
// by android/tools/world_map.py into assets/world_110m.bin (a byte copy of the app's asset).
// x = (lon − west) × scale, y = (north − lat) × scale. Pure: bytes in, shapes out.

const MAGIC = 0x4d47574d; // "MGWM"

/**
 * One country: its ISO alpha-2 code and its rings (x, y interleaved, map units).
 * @typedef {object} CountryShape
 * @property {string} code
 * @property {Uint16Array[]} rings
 */

/**
 * @typedef {object} WorldMap
 * @property {number} scale
 * @property {number} west
 * @property {number} north
 * @property {number} width
 * @property {number} height
 * @property {CountryShape[]} countries
 */

/**
 * Parses the asset (big-endian, as Java's DataInputStream); throws RangeError for anything that
 * is not a whole version-1 map.
 * @param {Uint8Array} bytes
 * @returns {WorldMap}
 */
export function parseWorldMap(bytes) {
  const d = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let o = 0;
  /** @param {number} n */
  const need = (n) => {
    if (o + n > d.byteLength) throw new RangeError("truncated world map");
  };
  const u8 = () => {
    need(1);
    return d.getUint8(o++);
  };
  const u16 = () => {
    need(2);
    const v = d.getUint16(o);
    o += 2;
    return v;
  };
  const i16 = () => {
    need(2);
    const v = d.getInt16(o);
    o += 2;
    return v;
  };
  need(4);
  if (d.getUint32(o) !== MAGIC) throw new RangeError("not a world map");
  o += 4;
  if (u8() !== 1) throw new RangeError("unknown world map version");
  const scale = u16();
  const west = i16();
  const north = i16();
  const width = u16();
  const height = u16();
  if (scale <= 0 || width <= 0 || height <= 0) throw new RangeError("empty world map");
  const n = u16();
  /** @type {CountryShape[]} */
  const countries = [];
  for (let c = 0; c < n; c++) {
    const code = String.fromCharCode(u8(), u8());
    const ringCount = u16();
    /** @type {Uint16Array[]} */
    const rings = [];
    for (let r = 0; r < ringCount; r++) {
      const points = u16();
      const ring = new Uint16Array(points * 2);
      for (let i = 0; i < points * 2; i++) ring[i] = u16();
      rings.push(ring);
    }
    countries.push({ code, rings });
  }
  if (o !== d.byteLength) throw new RangeError("trailing bytes in world map");
  return { scale, west, north, width, height, countries };
}

/**
 * Even-odd test over every ring (outer rings and holes alike), in map units.
 * @param {CountryShape} c
 * @param {number} x
 * @param {number} y
 */
export function countryContains(c, x, y) {
  let inside = false;
  for (const r of c.rings) {
    const n = r.length / 2;
    let j = n - 1;
    for (let i = 0; i < n; i++) {
      const xi = r[2 * i];
      const yi = r[2 * i + 1];
      const xj = r[2 * j];
      const yj = r[2 * j + 1];
      if ((yi > y) !== (yj > y) && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) inside = !inside;
      j = i;
    }
  }
  return inside;
}

/**
 * The country under (x, y) in map units, or null (sea, Antarctica, a country left out).
 * @param {WorldMap} m
 * @param {number} x
 * @param {number} y
 * @returns {CountryShape | null}
 */
export function countryAt(m, x, y) {
  return m.countries.find((c) => countryContains(c, x, y)) ?? null;
}

/**
 * @param {WorldMap} m
 * @param {string} code
 * @returns {CountryShape | null}
 */
export function countryByCode(m, code) {
  const c = code.toUpperCase();
  return m.countries.find((s) => s.code === c) ?? null;
}

/**
 * Map units of a longitude / latitude.
 * @param {WorldMap} m
 * @param {number} lon
 * @param {number} lat
 * @returns {[number, number]}
 */
export function project(m, lon, lat) {
  return [(lon - m.west) * m.scale, (m.north - lat) * m.scale];
}
