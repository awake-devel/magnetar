// Port of android/.../ui/crawlmap/CrawlMapModel.kt: labels, counts and the log scale of the
// crawl map (feature `crawl_map`, docs/APP.md §15). Pure.

import { defaultLocale } from "./numfmt.js";

/** @typedef {import("./types-stats.js").CrawlMap} CrawlMap */
/** @typedef {import("./types-stats.js").GeoCount} GeoCount */

/** The two layers (`layer=`). */
export const MAP_LAYERS = Object.freeze([
  Object.freeze({ api: "dht", label: "DHT discoveries" }),
  Object.freeze({ api: "nostr", label: "Nostr relays" }),
]);

/** `window=`: the last 24 h or everything since `CrawlMap.since`. */
export const MAP_WINDOWS = Object.freeze([
  Object.freeze({ api: "day", label: "Day" }),
  Object.freeze({ api: "all", label: "All" }),
]);

/** Rows of the top lists before "Show all". */
export const TOP_ROWS = 15;

/** @param {string} metric */
export function metricLabel(metric) {
  switch (metric) {
    case "hashes":
      return "Hashes";
    case "stored":
      return "Stored";
    case "nodes":
      return "Nodes";
    case "relays":
      return "Relays";
    case "events":
      return "Events";
    case "torrents":
      return "Torrents";
    default:
      return metric === "" ? metric : metric[0].toUpperCase() + metric.slice(1);
  }
}

/** @param {string} metric */
export function metricHint(metric) {
  switch (metric) {
    case "hashes":
      return "Infohashes nodes there gave us";
    case "stored":
      return "Torrents stored from nodes there";
    case "nodes":
      return "DHT nodes heard from, each counted once per UTC day";
    case "relays":
      return "Relays whose host resolves there";
    case "events":
      return "Events received from relays there";
    case "torrents":
      return "Torrents received from relays there";
    default:
      return metric;
  }
}

/**
 * Why the server's geo database cannot place addresses (`available: false`).
 * @param {string | null | undefined} reason
 */
export function geoReasonMessage(reason) {
  switch (reason) {
    case "disabled":
      return "The server's geo database is off, so nothing new is placed on the map.";
    case "missing":
      return "The server's geo database is not downloaded yet, so nothing new is placed on the map.";
    case "invalid":
      return "The server's geo database could not be read, so nothing new is placed on the map.";
    default:
      return "The server's geo database is not available.";
  }
}

/**
 * "Germany" for `DE` in the browser's language; the code itself when it is not a region code.
 * @param {string} code
 * @param {string} [locale]
 */
export function countryName(code, locale = defaultLocale()) {
  const c = code.toUpperCase();
  if (!/^[A-Z]{2}$/.test(c)) return c;
  try {
    const name = new Intl.DisplayNames([locale], { type: "region", fallback: "code" }).of(c);
    return name == null || name.trim() === "" || name === c ? c : name;
  } catch {
    return c;
  }
}

/**
 * "September 2026" (UTC) for a database build time.
 * @param {number} epochSec
 * @param {string} [locale]
 */
export function buildMonth(epochSec, locale = defaultLocale()) {
  return new Intl.DateTimeFormat(locale, { month: "long", year: "numeric", timeZone: "UTC" }).format(new Date(epochSec * 1000));
}

/**
 * "AS24940 Hetzner Online GmbH" (or just the AS number / organisation).
 * @param {GeoCount} g
 */
export function networkLabel(g) {
  const s = [g.asn != null ? `AS${g.asn}` : null, g.as_org != null && g.as_org.trim() !== "" ? g.as_org : null]
    .filter((x) => x != null).join(" ");
  return s === "" ? "Unknown network" : s;
}

/**
 * @param {GeoCount} g
 * @param {number} metric
 */
export function countOf(g, metric) {
  const v = g.counts?.[metric];
  return typeof v === "number" && Number.isFinite(v) ? v : 0;
}

/**
 * Index of `metric` in the map's metrics, the first one when it is absent.
 * @param {CrawlMap} map
 * @param {string | null | undefined} metric
 */
export function metricIndex(map, metric) {
  return Math.max(0, (map.metrics ?? []).indexOf(metric ?? ""));
}

/**
 * Country code → count of `metric` (codes upper-cased; zero counts left out).
 * @param {CrawlMap} map
 * @param {number} metric
 * @returns {Map<string, number>}
 */
export function countryCounts(map, metric) {
  /** @type {Map<string, number>} */
  const out = new Map();
  for (const c of map.countries ?? []) {
    if (c.country == null) continue;
    const code = c.country.toUpperCase();
    const n = countOf(c, metric);
    if (n > 0) out.set(code, (out.get(code) ?? 0) + n);
  }
  return out;
}

/**
 * The `n` rows with the most `metric`, zero rows left out (stable).
 * @param {readonly GeoCount[]} rows
 * @param {number} metric
 * @param {number} n
 */
export function topBy(rows, metric, n) {
  return rows.map((g, i) => ({ g, i })).filter((x) => countOf(x.g, metric) > 0)
    .sort((a, b) => countOf(b.g, metric) - countOf(a.g, metric) || a.i - b.i)
    .slice(0, n).map((x) => x.g);
}

/**
 * The total of `metric`: the server's, else the sum of countries and unknown.
 * @param {CrawlMap} map
 * @param {number} metric
 */
export function totalOf(map, metric) {
  const t = map.totals?.[metric];
  if (typeof t === "number") return t;
  return (map.countries ?? []).reduce((a, c) => a + countOf(c, metric), 0) + (map.unknown?.[metric] ?? 0);
}

/**
 * Sequential log scale of the choropleth: a count → 0 (the smallest non-zero count) … 1 (the
 * largest); null for 0 or no data.
 */
export class LogScale {
  /**
   * @param {number} min
   * @param {number} max
   */
  constructor(min, max) {
    this.min = min;
    this.max = max;
  }

  /**
   * @param {number} v
   * @returns {number | null}
   */
  shade(v) {
    if (v <= 0) return null;
    if (this.max <= this.min) return 1;
    const t = (Math.log(v) - Math.log(this.min)) / (Math.log(this.max) - Math.log(this.min));
    return Math.min(1, Math.max(0, t));
  }

  /**
   * The scale of `values`; null when none is above zero.
   * @param {Iterable<number>} values
   */
  static of(values) {
    const positive = [...values].filter((v) => v > 0);
    if (positive.length === 0) return null;
    return new LogScale(Math.min(...positive), Math.max(...positive));
  }
}

/**
 * The crawl-map query from the URL: a known layer and window, a metric name that is a word.
 * @param {URLSearchParams} q
 */
export function mapParams(q) {
  const layer = MAP_LAYERS.some((l) => l.api === q.get("layer")) ? /** @type {string} */ (q.get("layer")) : "dht";
  const window = MAP_WINDOWS.some((w) => w.api === q.get("window")) ? /** @type {string} */ (q.get("window")) : "day";
  const m = q.get("metric");
  const metric = m != null && /^[a-z0-9_]{1,40}$/.test(m) ? m : null;
  return { layer, window, metric };
}
