// Port of android/.../ui/graph/ForceLayout.kt: force-directed layout (Fruchterman–Reingold with
// gravity and cooling) for the D47 graphs, plus the view fit and the tap hit test. O(n²) per
// step, fine for the server's 200-node cap. Deterministic (a seeded XorWow, the generator behind
// Kotlin's Random(seed)), and run in a module Worker by views/graph.js. Pure.

/** Kotlin's `Random(seed)` (XorWowRandom), so the start positions follow the same sequence. */
export class XorWow {
  /** @param {number} seed */
  constructor(seed) {
    const seed1 = seed | 0;
    const seed2 = seed1 >> 31;
    this.x = seed1;
    this.y = seed2;
    this.z = 0;
    this.w = 0;
    this.v = ~seed1;
    this.addend = (seed1 << 10) ^ (seed2 >>> 4);
    for (let i = 0; i < 64; i++) this.nextInt();
  }

  nextInt() {
    let t = this.x;
    t = t ^ (t >>> 2);
    this.x = this.y;
    this.y = this.z;
    this.z = this.w;
    const v0 = this.v;
    this.w = v0;
    t = (t ^ (t << 1)) ^ v0 ^ (v0 << 4);
    this.v = t | 0;
    this.addend = (this.addend + 362437) | 0;
    return (t + this.addend) | 0;
  }

  /** A float in [0, 1) from the top 24 bits, as Kotlin's nextFloat. */
  nextFloat() {
    return (this.nextInt() >>> 8) / (1 << 24);
  }
}

/**
 * @param {number} n nodes
 * @param {readonly (readonly [number, number])[]} edges pairs of node indexes
 * @param {number | null} [pinned] stays at the origin (the graph's subject)
 * @param {number} [seed]
 */
export function createLayout(n, edges, pinned = null, seed = 7) {
  const x = new Float64Array(n);
  const y = new Float64Array(n);
  const dx = new Float64Array(n);
  const dy = new Float64Array(n);
  /** Ideal edge length (layout units; the view scales to fit). */
  const k = 40;
  const cutoff2 = (6 * k) * (6 * k);
  const crossCutoff2 = (1.5 * k) * (1.5 * k);
  const part = new Int32Array(n);
  for (let i = 0; i < n; i++) part[i] = i;
  /** @param {number} a */
  const find = (a) => {
    let r = a;
    while (part[r] !== r) r = part[r];
    let c = a;
    while (part[c] !== r) {
      const next = part[c];
      part[c] = r;
      c = next;
    }
    return r;
  };
  for (const [a, b] of edges) if (a >= 0 && a < n && b >= 0 && b < n) part[find(a)] = find(b);
  for (let i = 0; i < n; i++) part[i] = find(i);
  let temperature = k * 3;
  let steps = 0;
  const rnd = new XorWow(seed);
  const r0 = k * Math.sqrt(n);
  for (let i = 0; i < n; i++) {
    const a = rnd.nextFloat() * 2 * Math.PI;
    const d = r0 * Math.sqrt(rnd.nextFloat());
    x[i] = d * Math.cos(a);
    y[i] = d * Math.sin(a);
  }
  if (pinned != null && pinned >= 0 && pinned < n) {
    x[pinned] = 0;
    y[pinned] = 0;
  }

  /** One step; returns the largest move (a settled layout moves under 0.5). */
  function step() {
    dx.fill(0);
    dy.fill(0);
    const k2 = k * k;
    for (let i = 0; i < n; i++) {
      for (let j = i + 1; j < n; j++) {
        let ddx = x[i] - x[j];
        let ddy = y[i] - y[j];
        let d2 = ddx * ddx + ddy * ddy;
        if (d2 < 0.01) {
          ddx = (((i * 31 + j) % 7) - 3) * 0.1 + 0.05;
          ddy = (((i * 17 + j) % 5) - 2) * 0.1 + 0.05;
          d2 = ddx * ddx + ddy * ddy;
        }
        if (d2 > (part[i] === part[j] ? cutoff2 : crossCutoff2)) continue;
        const f = k2 / d2;
        dx[i] += ddx * f;
        dy[i] += ddy * f;
        dx[j] -= ddx * f;
        dy[j] -= ddy * f;
      }
    }
    for (const [a, b] of edges) {
      if (a < 0 || a >= n || b < 0 || b >= n || a === b) continue;
      const ddx = x[a] - x[b];
      const ddy = y[a] - y[b];
      const d = Math.max(0.01, Math.sqrt(ddx * ddx + ddy * ddy));
      const f = d / k;
      dx[a] -= ddx * f;
      dy[a] -= ddy * f;
      dx[b] += ddx * f;
      dy[b] += ddy * f;
    }
    let moved = 0;
    for (let i = 0; i < n; i++) {
      if (i === pinned) continue;
      dx[i] -= x[i] * 0.02;
      dy[i] -= y[i] * 0.02;
      const len = Math.sqrt(dx[i] * dx[i] + dy[i] * dy[i]);
      if (len > 0) {
        const m = Math.min(len, temperature);
        x[i] += (dx[i] / len) * m;
        y[i] += (dy[i] / len) * m;
        moved = Math.max(moved, m);
      }
    }
    temperature = Math.max(temperature * 0.97, 0.25);
    steps++;
    return moved;
  }

  /**
   * Steps until settled or `maxSteps`; `onProgress` gets copies every `every` steps.
   * @param {number} [maxSteps]
   * @param {number} [every]
   * @param {(x: Float64Array, y: Float64Array) => void} [onProgress]
   */
  function run(maxSteps = 400, every = 25, onProgress) {
    for (let s = 1; s <= maxSteps; s++) {
      const moved = step();
      if (onProgress && s % every === 0) onProgress(x.slice(), y.slice());
      if (moved < 0.5 && s > 50) break;
    }
  }

  return {
    x,
    y,
    step,
    run,
    get steps() {
      return steps;
    },
  };
}

/**
 * Screen transform of a layout: screen = layout × scale + (ox, oy).
 * @typedef {{scale: number, ox: number, oy: number}} ViewTransform
 */

/**
 * The transform that fits the first `n` positions into a width × height view with `pad` free
 * on every side, centred; scale clamped to minScale–maxScale.
 * @param {ArrayLike<number>} x
 * @param {ArrayLike<number>} y
 * @param {{n?: number, width: number, height: number, pad: number, minScale?: number, maxScale?: number}} o
 * @returns {ViewTransform}
 */
export function fitTransform(x, y, o) {
  const n = o.n ?? Math.min(x.length, y.length);
  const { width, height, pad } = o;
  if (n <= 0 || width <= 0 || height <= 0) return { scale: 1, ox: width / 2, oy: height / 2 };
  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  for (let i = 0; i < n; i++) {
    minX = Math.min(minX, x[i]);
    maxX = Math.max(maxX, x[i]);
    minY = Math.min(minY, y[i]);
    maxY = Math.max(maxY, y[i]);
  }
  const s = Math.min(
    o.maxScale ?? 6,
    Math.max(
      o.minScale ?? 0.05,
      Math.min(Math.max(1, width - 2 * pad) / Math.max(1, maxX - minX), Math.max(1, height - 2 * pad) / Math.max(1, maxY - minY)),
    ),
  );
  return { scale: s, ox: width / 2 - ((minX + maxX) / 2) * s, oy: height / 2 - ((minY + maxY) / 2) * s };
}

/**
 * Index of the node nearest to (px, py) on screen within its touch radius (radius or at least
 * minTouch pixels), or null.
 * @param {number} px
 * @param {number} py
 * @param {ArrayLike<number>} x
 * @param {ArrayLike<number>} y
 * @param {number} n
 * @param {ViewTransform} t
 * @param {ArrayLike<number>} radius
 * @param {number} minTouch
 * @returns {number | null}
 */
export function nodeAt(px, py, x, y, n, t, radius, minTouch) {
  /** @type {number | null} */
  let best = null;
  let bestD = Infinity;
  const m = Math.min(n, x.length, y.length, radius.length);
  for (let i = 0; i < m; i++) {
    const sx = x[i] * t.scale + t.ox;
    const sy = y[i] * t.scale + t.oy;
    const d = (sx - px) * (sx - px) + (sy - py) * (sy - py);
    const r = Math.max(radius[i], minTouch);
    if (d < r * r && d < bestD) {
      best = i;
      bestD = d;
    }
  }
  return best;
}

/** Kinds of a graph's subject (its first node), pinned at the centre. */
export const SUBJECT_KINDS = Object.freeze(new Set(["root", "media", "torrent"]));

/**
 * @param {{nodes?: {kind?: string}[]}} g
 * @returns {number | null}
 */
export function subjectOf(g) {
  const first = g.nodes?.[0];
  return first && SUBJECT_KINDS.has(first.kind ?? "") ? 0 : null;
}

/** Colour slot per node kind, the same in every graph. */
export const KIND_SLOT = Object.freeze(/** @type {Record<string, number>} */ ({
  root: 0, media: 0, torrent: 0, publisher: 1, release: 1, follow: 2, language: 2, tracker: 2,
  quality: 3, season: 4, relay: 5, group: 6, dht: 6,
}));

export const KIND_LABEL = Object.freeze(/** @type {Record<string, string>} */ ({
  root: "You (trust root)", follow: "Followed by you", publisher: "Publisher", relay: "Relay", media: "Title",
  quality: "Quality", release: "Release", language: "Language", group: "Release group", season: "Season",
  torrent: "Torrent", tracker: "Tracker", dht: "DHT",
}));

/** @param {string} kind */
export function kindLabel(kind) {
  return Object.hasOwn(KIND_LABEL, kind) ? KIND_LABEL[kind] : kind;
}

/** @param {string} kind */
export function kindSlot(kind) {
  return Object.hasOwn(KIND_SLOT, kind) ? KIND_SLOT[kind] : 7;
}

/** @param {string} path */
export function graphTitle(path) {
  if (path === "wot") return "Web of trust";
  if (path === "relays") return "Relays and publishers";
  if (path.startsWith("media/")) return "Releases of this title";
  if (path.startsWith("swarm/")) return "Swarm sources";
  return "Graph";
}

/** The graph paths the server has; anything else is refused before a request. */
export const GRAPH_PATH = /^(wot|relays|media\/[1-9]\d{0,17}|swarm\/[0-9a-f]{40})$/;
