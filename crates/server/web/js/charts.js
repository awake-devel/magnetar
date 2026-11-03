// Pure chart geometry for the Stats and detail charts (ports of Charts.kt and SeederChart.kt):
// the zoom/pan window of a series, the donut's slices and hit test, step-line points, value
// labels. The SVG itself is built in views/stats-charts.js and views/seeder-chart.js.

import { slotOf } from "./chart-colors.js";
import { formatBytes } from "./format.js";
import { formatCompact, formatNumber } from "./numfmt.js";

/** @typedef {import("./types-stats.js").Breakdown} Breakdown */
/** @typedef {import("./types-stats.js").FacetCount} FacetCount */
/** @typedef {import("./types-stats.js").LivenessPoint} LivenessPoint */

/** @param {number} v @param {number} lo @param {number} hi */
function clamp(v, lo, hi) {
  return Math.min(hi, Math.max(lo, v));
}

/**
 * The visible part of a series of `n` points: `first` and `count` (at least MIN points, or all
 * of them). Zoom keeps the point under the focus in place; pan moves within bounds.
 */
export class ChartWindow {
  static MIN = 7;

  /**
   * @param {number} n
   * @param {number} [first]
   * @param {number} [count]
   */
  constructor(n, first = 0, count = n) {
    this.n = n;
    this.first = first;
    this.count = count;
    Object.freeze(this);
  }

  /** @param {number} f */
  withFirst(f) {
    return new ChartWindow(this.n, clamp(f, 0, Math.max(0, this.n - this.count)), this.count);
  }

  /**
   * Zoom by `factor` (> 1 = in) keeping the point at `focus` (0–1 across the view) in place.
   * @param {number} factor
   * @param {number} focus
   */
  zoom(factor, focus) {
    if (this.n <= ChartWindow.MIN || factor <= 0) return this;
    const fo = clamp(focus, 0, 1);
    const newCount = clamp(Math.trunc(this.count / factor), Math.min(ChartWindow.MIN, this.n), this.n);
    const anchor = this.first + fo * this.count;
    const newFirst = Math.trunc(anchor - fo * newCount);
    return new ChartWindow(this.n, this.first, newCount).withFirst(newFirst);
  }

  /**
   * Pan by `fraction` of the view (positive = towards older points).
   * @param {number} fraction
   */
  pan(fraction) {
    return this.withFirst(this.first - Math.trunc(fraction * this.count));
  }

  /**
   * Point index under `x` (0–1 across the view).
   * @param {number} x
   */
  indexAt(x) {
    return clamp(this.first + Math.trunc(clamp(x, 0, 0.9999) * this.count), 0, Math.max(0, this.n - 1));
  }

  /** @param {ChartWindow} o */
  equals(o) {
    return this.n === o.n && this.first === o.first && this.count === o.count;
  }
}

/**
 * Index of the slice under `angleDeg` (0 = 12 o'clock, clockwise) of slices sized `sizes`.
 * @param {number} angleDeg
 * @param {readonly number[]} sizes
 * @returns {number | null}
 */
export function sliceAt(angleDeg, sizes) {
  const sum = sizes.reduce((a, b) => a + b, 0);
  if (sum <= 0) return null;
  let a = angleDeg % 360;
  if (a < 0) a += 360;
  let start = 0;
  for (let i = 0; i < sizes.length; i++) {
    const sweep = (sizes[i] / sum) * 360;
    if (a < start + sweep) return i;
    start += sweep;
  }
  return null;
}

/**
 * A donut slice: its value (null = "Other"), count and colour.
 * @typedef {object} Slice
 * @property {FacetCount | null} count
 * @property {number} n
 * @property {string} color
 */

/**
 * Slices of a fixed-vocabulary field: values past the colour slots fold into "Other".
 * @param {Breakdown} b
 * @param {readonly string[]} order
 * @param {import("./chart-colors.js").ChartColors} colors
 * @returns {Slice[]}
 */
export function slices(b, order, colors) {
  /** @type {Slice[]} */
  const out = [];
  let other = b.other ?? 0;
  for (const c of b.counts ?? []) {
    const color = slotOf(colors, c.value, order);
    if (color == null) other += c.count;
    else out.push({ count: c, n: c.count, color });
  }
  if (other > 0) out.push({ count: null, n: other, color: colors.other });
  return out;
}

/**
 * SVG path of a donut segment from `a0` to `a1` degrees (0 = 12 o'clock, clockwise).
 * @param {number} cx
 * @param {number} cy
 * @param {number} r outer radius
 * @param {number} ri inner radius
 * @param {number} a0
 * @param {number} a1
 */
export function donutPath(cx, cy, r, ri, a0, a1) {
  const sweep = Math.min(359.999, Math.max(0.01, a1 - a0));
  const end = a0 + sweep;
  /** @param {number} rad @param {number} deg */
  const pt = (rad, deg) => {
    const t = ((deg - 90) * Math.PI) / 180;
    return [cx + rad * Math.cos(t), cy + rad * Math.sin(t)].map((v) => Math.round(v * 100) / 100);
  };
  const large = sweep > 180 ? 1 : 0;
  const [x0, y0] = pt(r, a0);
  const [x1, y1] = pt(r, end);
  const [x2, y2] = pt(ri, end);
  const [x3, y3] = pt(ri, a0);
  return `M${x0} ${y0}A${r} ${r} 0 ${large} 1 ${x1} ${y1}L${x2} ${y2}A${ri} ${ri} 0 ${large} 0 ${x3} ${y3}Z`;
}

/**
 * A step series of the seed history (D45): (time, value) points, each value held until the
 * next. Seeders are max(tracker seeders, DHT estimate); a point where a source did not answer is
 * skipped, not drawn as 0.
 * @param {readonly LivenessPoint[]} points
 * @returns {{seeders: [number, number][], leechers: [number, number][]}}
 */
export function seederSeries(points) {
  /** @type {[number, number][]} */
  const seeders = [];
  /** @type {[number, number][]} */
  const leechers = [];
  for (const p of points) {
    if (p.seeders != null || p.peers_est != null) seeders.push([p.at, Math.max(p.seeders ?? 0, p.peers_est ?? 0)]);
    if (p.leechers != null) leechers.push([p.at, p.leechers]);
  }
  return { seeders, leechers };
}

/**
 * SVG path data of a step line over [start, end] in a w × h box, values scaled to `top`.
 * @param {readonly [number, number][]} pts
 * @param {number} start
 * @param {number} end
 * @param {number} top
 * @param {number} w
 * @param {number} h
 */
export function stepPath(pts, start, end, top, w, h) {
  if (pts.length === 0) return "";
  const span = Math.max(1, end - start);
  /** @param {number} t */
  const x = (t) => Math.round(((t - start) / span) * w * 10) / 10;
  /** @param {number} v */
  const y = (v) => Math.round((h - (v / Math.max(1, top)) * (h - 4) - 2) * 10) / 10;
  let d = "";
  pts.forEach(([t, v], i) => {
    d += (i === 0 ? "M" : "L") + `${x(t)} ${y(v)}`;
    const next = i + 1 < pts.length ? pts[i + 1][0] : end;
    d += `L${x(next)} ${y(v)}`;
  });
  return d;
}

/** @typedef {"count" | "bytes"} ValueUnit */

/**
 * @param {number} v
 * @param {ValueUnit} unit
 * @param {string} [locale]
 */
export function seriesValue(v, unit, locale) {
  return unit === "bytes" ? formatBytes(v) : formatNumber(v, locale);
}

/**
 * @param {number} v
 * @param {ValueUnit} unit
 * @param {string} [locale]
 */
export function axisValue(v, unit, locale) {
  return unit === "bytes" ? formatBytes(v) : formatCompact(v, locale);
}
