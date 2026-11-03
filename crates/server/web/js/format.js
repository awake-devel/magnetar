// Port of android/.../ui/common/Format.kt: sizes, counts, ages, peers, HTML-to-text.
// Pure: no DOM, no clock unless a default argument reads it.

/** @returns {number} the current unix time in seconds. */
export function nowSec() {
  return Math.floor(Date.now() / 1000);
}

/**
 * "4.37 GiB", "512 B", "?" for a missing or negative size.
 * @param {number | null | undefined} bytes
 */
export function formatBytes(bytes) {
  if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return "?";
  if (bytes < 1024) return `${Math.trunc(bytes)} B`;
  const units = ["KiB", "MiB", "GiB", "TiB", "PiB"];
  let v = bytes / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v < 10 ? 2 : 1)} ${units[i]}`;
}

/**
 * "1,234" with ROOT-locale grouping (as Kotlin's "%,d"), "1,000+" for a lower bound (D43).
 * @param {number} n
 * @param {boolean} [lowerBound]
 */
export function formatCount(n, lowerBound = false) {
  const s = String(Math.trunc(Math.abs(n))).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return (n < 0 ? "-" : "") + s + (lowerBound ? "+" : "");
}

/**
 * "1 result", "12 results", "1,000+ results".
 * @param {number} n
 * @param {boolean} lowerBound
 */
export function resultsText(n, lowerBound) {
  return formatCount(n, lowerBound) + (n === 1 && !lowerBound ? " result" : " results");
}

/**
 * "now", "3m", "5h", "12d", "2y".
 * @param {number} epochSec
 * @param {number} [now]
 */
export function formatAge(epochSec, now = nowSec()) {
  const d = Math.max(0, now - epochSec);
  if (d < 60) return "now";
  if (d < 3600) return `${Math.floor(d / 60)}m`;
  if (d < 86_400) return `${Math.floor(d / 3600)}h`;
  if (d < 365 * 86_400) return `${Math.floor(d / 86_400)}d`;
  return `${Math.floor(d / (365 * 86_400))}y`;
}

/**
 * "just now", "5m ago".
 * @param {number} epochSec
 * @param {number} [now]
 */
export function formatAgo(epochSec, now = nowSec()) {
  const a = formatAge(epochSec, now);
  return a === "now" ? "just now" : `${a} ago`;
}

/**
 * Date and time in the browser's locale.
 * @param {number} epochSec
 * @param {string} [locale]
 */
export function formatTimestamp(epochSec, locale) {
  return new Intl.DateTimeFormat(locale, { dateStyle: "medium", timeStyle: "short" }).format(
    new Date(epochSec * 1000),
  );
}

/**
 * Date in the browser's locale.
 * @param {number} epochSec
 * @param {string} [locale]
 */
export function formatDate(epochSec, locale) {
  return new Intl.DateTimeFormat(locale, { dateStyle: "medium" }).format(new Date(epochSec * 1000));
}

/**
 * "★ 7.4".
 * @param {number} r
 */
export function formatRating(r) {
  return `★ ${r.toFixed(1)}`;
}

/**
 * "12 S / 3 L", "≈40 peers" (DHT estimate only) or "peers unknown".
 * @param {number | null | undefined} seeders
 * @param {number | null | undefined} leechers
 * @param {number | null | undefined} [bestPeers]
 */
export function peersText(seeders, leechers, bestPeers = null) {
  if (seeders == null && leechers == null && bestPeers != null) return `≈${bestPeers} peers`;
  if (seeders == null && leechers == null) return "peers unknown";
  return `${seeders ?? "?"} S / ${leechers ?? "?"} L`;
}

/**
 * Last path component of a file inside a torrent ("Season 1/E01.mkv" → "E01.mkv").
 * @param {string} path
 */
export function fileName(path) {
  const p = path.replace(/\/+$/, "");
  const i = p.lastIndexOf("/");
  return i < 0 ? p : p.slice(i + 1);
}

/**
 * "npub1abcdefgh…uvwxyz" for long keys.
 * @param {string} key
 */
export function shortKey(key) {
  return key.length > 20 ? `${key.slice(0, 12)}…${key.slice(-6)}` : key;
}

/** Trust factor codes that pin a publisher to "do not show". */
export const BLOCKING_TRUST_CODES = Object.freeze(["BLOCKED", "ROOT_MUTED"]);

/** @typedef {"ok" | "low" | "negative"} TrustLevel */

/**
 * How a trust badge is tinted: ok at or above the threshold; negative only for a block, a root
 * mute or a negative factor; otherwise low (neutral).
 * @param {number} score
 * @param {number} threshold
 * @param {readonly string[]} [factorCodes]
 * @param {boolean} [negativeFactor]
 * @returns {TrustLevel}
 */
export function trustLevel(score, threshold, factorCodes = [], negativeFactor = false) {
  if (factorCodes.some((c) => BLOCKING_TRUST_CODES.includes(c))) return "negative";
  if (score >= threshold) return "ok";
  if (negativeFactor) return "negative";
  return "low";
}

const BLOCK_TAGS = /<\s*(br|\/p|\/div|\/li|\/h[1-6]|\/tr)\b[^>]*>/gi;
const ANY_TAG = /<[^>]*>/g;
const NUMERIC_ENTITY = /&#(x?)([0-9a-fA-F]+);/g;
/** @type {[string, string][]} */
const NAMED_ENTITIES = [
  ["&nbsp;", " "],
  ["&lt;", "<"],
  ["&gt;", ">"],
  ["&quot;", '"'],
  ["&#39;", "'"],
  ["&apos;", "'"],
];

/**
 * Event descriptions are often HTML or BBCode-ish: shown as plain text (Format.kt stripHtml).
 * The result is only ever put into a Text node.
 * @param {string} input
 */
export function stripHtml(input) {
  let s = input.replace(BLOCK_TAGS, "\n").replace(ANY_TAG, "");
  s = s.replace(NUMERIC_ENTITY, (m, x, digits) => {
    const radix = x === "" ? 10 : 16;
    const valid = radix === 10 ? /^[0-9]+$/.test(digits) : true;
    if (!valid) return m;
    const cp = parseInt(digits, radix);
    if (!Number.isFinite(cp) || cp < 0 || cp > 0x10ffff) return m;
    return String.fromCodePoint(cp);
  });
  for (const [k, v] of NAMED_ENTITIES) s = s.split(k).join(v);
  s = s.split("&amp;").join("&");
  return s
    .split(/\r\n|\r|\n/)
    .map((l) => l.replace(/\s+$/, ""))
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

/**
 * "cached · 5 min ago" for a page saved at savedAtMs.
 * @param {number} savedAtMs
 * @param {number} [nowMs]
 */
export function cachedAgo(savedAtMs, nowMs = Date.now()) {
  const s = Math.max(0, Math.floor((nowMs - savedAtMs) / 1000));
  let age;
  if (s < 60) age = "just now";
  else if (s < 3600) age = `${Math.floor(s / 60)} min ago`;
  else if (s < 86_400) age = `${Math.floor(s / 3600)} h ago`;
  else age = `${Math.floor(s / 86_400)} d ago`;
  return `cached · ${age}`;
}
