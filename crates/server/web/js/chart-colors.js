// Port of android/.../ui/theme/ChartColors.kt (D47): chart and graph colours. The categorical
// slots are a validated colour-blind-safe order, used in this fixed order and never cycled: past
// the last slot values fold into "Other". Bars of one field use one hue. Status colours are for
// seeded / dead / unknown only and always come with a word. The crawl map is a sequential
// one-hue scale from mapLow to mapHigh.

/**
 * @typedef {object} ChartColors
 * @property {readonly string[]} categorical
 * @property {string} bar
 * @property {string} barSelected
 * @property {string} other
 * @property {string} good
 * @property {string} critical
 * @property {string} unknown
 * @property {string} grid
 * @property {string} axis
 * @property {string} ink
 * @property {string} inkMuted
 * @property {string} edge
 * @property {string} mapLow
 * @property {string} mapHigh
 * @property {string} mapEmpty
 * @property {string} mapBorder
 */

/** @type {Readonly<ChartColors>} */
export const LIGHT = Object.freeze({
  categorical: Object.freeze(["#2A78D6", "#EB6834", "#1BAF7A", "#EDA100", "#E87BA4", "#008300", "#6250D6", "#E34948"]),
  bar: "#86B6EF",
  barSelected: "#1C5CAB",
  other: "#C3C2B7",
  good: "#0CA30C",
  critical: "#D03B3B",
  unknown: "#898781",
  grid: "#E1E0D9",
  axis: "#C3C2B7",
  ink: "#0B0B0B",
  inkMuted: "#52514E",
  edge: "#0B0B0B66",
  mapLow: "#CFE2F8",
  mapHigh: "#0B3D82",
  mapEmpty: "#E1E0D9",
  mapBorder: "#FFFFFF",
});

/** @type {Readonly<ChartColors>} */
export const DARK = Object.freeze({
  categorical: Object.freeze(["#3987E5", "#D95926", "#199E70", "#C98500", "#D55181", "#008300", "#9085E9", "#E66767"]),
  bar: "#256ABF",
  barSelected: "#86B6EF",
  other: "#52514E",
  good: "#0CA30C",
  critical: "#D03B3B",
  unknown: "#898781",
  grid: "#2C2C2A",
  axis: "#383835",
  ink: "#F0EFEC",
  inkMuted: "#C3C2B7",
  edge: "#F0EFEC66",
  mapLow: "#1C3352",
  mapHigh: "#A9CDF7",
  mapEmpty: "#383835",
  mapBorder: "#141413",
});

/**
 * The palette of the page's colour scheme.
 * @param {boolean} [dark] defaults to prefers-color-scheme
 * @returns {Readonly<ChartColors>}
 */
export function chartColors(dark) {
  let d = dark;
  if (d == null) {
    try {
      d = globalThis.matchMedia?.("(prefers-color-scheme: dark)").matches === true;
    } catch {
      d = false;
    }
  }
  return d ? DARK : LIGHT;
}

/**
 * Slot of `value` in a field with a fixed vocabulary, so a value keeps its colour whatever its
 * rank; null = not in the first slots (drawn as "Other").
 * @param {Readonly<ChartColors>} colors
 * @param {string} value
 * @param {readonly string[]} order
 * @returns {string | null}
 */
export function slotOf(colors, value, order) {
  const i = order.indexOf(value);
  return i >= 0 && i < colors.categorical.length ? colors.categorical[i] : null;
}

/**
 * @param {string} hex "#RRGGBB" or "#RRGGBBAA"
 * @returns {[number, number, number, number]}
 */
export function parseHex(hex) {
  const m = /^#([0-9a-f]{6})([0-9a-f]{2})?$/i.exec(hex);
  if (!m) throw new TypeError(`bad colour ${hex}`);
  const n = parseInt(m[1], 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255, m[2] ? parseInt(m[2], 16) / 255 : 1];
}

/**
 * Linear mix of two colours: t = 0 → a, 1 → b. "#rrggbb".
 * @param {string} a
 * @param {string} b
 * @param {number} t
 */
export function lerpColor(a, b, t) {
  const x = parseHex(a);
  const y = parseHex(b);
  const k = Math.min(1, Math.max(0, t));
  return "#" + [0, 1, 2].map((i) => Math.round(x[i] + (y[i] - x[i]) * k).toString(16).padStart(2, "0")).join("");
}

/**
 * The colour with an alpha, as an rgb() string for canvas and SVG fills.
 * @param {string} hex
 * @param {number} alpha 0–1
 */
export function withAlpha(hex, alpha) {
  const [r, g, b] = parseHex(hex);
  return `rgb(${r} ${g} ${b} / ${Math.round(Math.min(1, Math.max(0, alpha)) * 1000) / 10}%)`;
}
