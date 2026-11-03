// Port of android/.../ui/stats/StatsModel.kt (D47, docs/STATS.md): the activity metrics, the
// breakdown fields and their cross-filters, and the Status tab's label → value lines. Pure.

import { formatAgo, formatBytes, nowSec } from "./format.js";
import { facetLabel } from "./labels.js";
import { backlogEta, defaultLocale, formatNumber, formatPercent, formatRate, formatUptime } from "./numfmt.js";
import { makeFilters, selected, toggle } from "./search-model.js";

/** @typedef {import("./search-model.js").SearchFilters} SearchFilters */
/** @typedef {import("./types-stats.js").LivenessStats} LivenessStats */
/** @typedef {import("./types-stats.js").SearchIndexStatus} SearchIndexStatus */
/** @typedef {import("./types-stats.js").DhtStats} DhtStats */
/** @typedef {[string, string]} Line */

/**
 * One activity chart the user can pick (D47 metrics, D74 DHT metrics, D118 probe backfill).
 * @typedef {object} Activity
 * @property {string} metric
 * @property {string} label
 * @property {import("./charts.js").ValueUnit} unit
 * @property {boolean} dht
 * @property {boolean} probes
 */

/**
 * @param {string} metric
 * @param {string} label
 * @param {Partial<Activity>} [o]
 * @returns {Readonly<Activity>}
 */
function act(metric, label, o = {}) {
  return Object.freeze({ metric, label, unit: o.unit ?? "count", dht: o.dht ?? false, probes: o.probes ?? false });
}

export const ACTIVITIES = Object.freeze([
  act("torrents_total", "Torrents over time"),
  act("torrents_added", "Added"),
  act("torrents_published", "Published"),
  act("crawl_events", "Crawled events"),
  act("relays_discovered", "Relays found"),
  act("liveness_checks", "Seed checks"),
  act("db_size", "Database size", { unit: "bytes" }),
  act("dht_torrents_added", "DHT torrents added", { dht: true }),
  act("dht_hashes_seen", "DHT hashes found", { dht: true }),
  act("dht_fetch_ok", "DHT metadata fetched", { dht: true }),
  act("dht_fetch_fail", "DHT fetches failed", { dht: true }),
  // A count only: the illegal-content blocklist's matches are never listed (docs/DHT.md §7).
  act("dht_blocked", "DHT blocked (count)", { dht: true }),
  // DHT rows the probe backfill added for Nostr-only torrents (not discoveries, D118).
  act("probe_backfill_dht_rows", "Probe backfill DHT rows", { probes: true }),
]);

/**
 * The metrics a server with `dht` (crawler on) and `probes` (backfill counts) offers.
 * @param {boolean} dht
 * @param {boolean} [probes]
 */
export function offeredActivities(dht, probes = false) {
  return ACTIVITIES.filter((a) => (!a.dht || dht) && (!a.probes || probes));
}

/** @param {string} metric */
export function activityOf(metric) {
  return ACTIVITIES.find((a) => a.metric === metric) ?? ACTIVITIES[0];
}

/** The breakdown charts, in screen order (years come last, as a histogram of their own). */
export const BREAKDOWN_FIELDS = Object.freeze([
  "category", "language", "resolution", "media_kind", "source", "codec", "hdr", "audio", "genre", "subcategory",
]);

/**
 * The breakdowns a server with `features` can count: `origin` (Nostr / DHT) is newer.
 * @param {ReadonlySet<string>} features
 */
export function breakdownFields(features) {
  return features.has("origin") ? [...BREAKDOWN_FIELDS, "origin"] : [...BREAKDOWN_FIELDS];
}

export const FIELD_TITLES = Object.freeze(/** @type {Record<string, string>} */ ({
  language: "Audio languages", resolution: "Resolutions", category: "Categories",
  source: "Release sources", codec: "Video codecs", hdr: "HDR", audio: "Audio formats",
  genre: "Genres", media_kind: "Movies, series, anime", subcategory: "Publisher categories",
  year: "Years", origin: "Found via (Nostr / DHT)",
}));

/** Fields whose values are search filters (cross-filter on click); the others open a text search. */
export const FILTERABLE = Object.freeze(new Set([
  "language", "resolution", "category", "source", "codec", "hdr", "audio", "genre", "media_kind", "year", "origin",
]));

/** A torrent can count under several values of these. */
export const MULTI_VALUED = Object.freeze(new Set(["language", "hdr", "audio", "genre", "origin"]));

/** Fixed vocabularies drawn as donuts: a value keeps its colour slot whatever its rank. */
export const DONUT_ORDER = Object.freeze(/** @type {Record<string, readonly string[]>} */ ({
  category: Object.freeze(["video", "anime", "audio", "book", "game", "application", "porn", "picture"]),
  resolution: Object.freeze(["2160p", "1080p", "720p", "576p", "480p", "sd", "1440p", "4320p"]),
  media_kind: Object.freeze(["movie", "series", "anime"]),
  origin: Object.freeze(["nostr", "dht"]),
}));

/**
 * The label of a chart value ("fr" → "French", "web-dl" → "WEB-DL", "dht" → "DHT").
 * @param {string} field
 * @param {string} value
 */
export function valueLabel(field, value) {
  if (field === "origin") return value === "nostr" ? "Nostr" : value === "dht" ? "DHT" : value;
  if (field === "subcategory" || field === "year") return value;
  return facetLabel(field, value);
}

/**
 * `value` of `field` switched on or off as a cross-filter (a year is min = max = that year).
 * @param {Readonly<SearchFilters>} f
 * @param {string} field
 * @param {string} value
 * @returns {Readonly<SearchFilters>}
 */
export function toggleStat(f, field, value) {
  if (field === "year") {
    if (!/^-?\d{1,6}$/.test(value)) return f;
    const y = Number(value);
    return f.minYear === y && f.maxYear === y
      ? makeFilters({ ...f, minYear: null, maxYear: null })
      : makeFilters({ ...f, minYear: y, maxYear: y });
  }
  if (FILTERABLE.has(field)) return toggle(f, field, value);
  return f;
}

/**
 * Values of `field` the cross-filter selects.
 * @param {Readonly<SearchFilters>} f
 * @param {string} field
 * @returns {readonly string[]}
 */
export function selectedStat(f, field) {
  if (field === "year") return f.minYear != null && f.minYear === f.maxYear ? [String(f.minYear)] : [];
  return selected(f, field);
}

/**
 * The active cross-filters as [field, value] chips, in chart order.
 * @param {Readonly<SearchFilters>} f
 * @returns {[string, string][]}
 */
export function statChips(f) {
  /** @type {[string, string][]} */
  const out = [];
  for (const field of [...BREAKDOWN_FIELDS, "origin", "year"]) {
    if (!FILTERABLE.has(field)) continue;
    for (const v of [...selectedStat(f, field)].sort()) out.push([field, v]);
  }
  return out;
}

/**
 * Seed checks as the health card shows them: the latest result of the torrents checked at least
 * once, and, separately, those not checked yet.
 * @param {LivenessStats} l
 */
export function seedChecks(l) {
  const seeded = l.alive ?? 0;
  const dead = l.dead ?? 0;
  const noAnswer = l.unknown ?? 0;
  const notChecked = l.never_checked ?? 0;
  return {
    seeded,
    dead,
    noAnswer,
    notChecked,
    torrents: Math.max(l.torrents ?? 0, seeded + dead + noAnswer + notChecked),
    checked: seeded + dead + noAnswer,
  };
}

/**
 * The seed-check backlog as label → value lines (Status tab).
 * @param {LivenessStats} l
 * @param {string} [locale]
 * @param {number} [now]
 * @returns {Line[]}
 */
export function backlogLines(l, locale = defaultLocale(), now = nowSec()) {
  /** @type {Line[]} */
  const out = [];
  const n = (/** @type {number | undefined} */ v) => formatNumber(v ?? 0, locale);
  if (!l.enabled) out.push(["Checker", "turned off on the server"]);
  const eta = backlogEta(l.due_now ?? 0, l.checks_per_hour ?? 0);
  out.push(["Due now", n(l.due_now) + (eta != null ? ` (cleared ${eta})` : "")]);
  out.push(["Never checked", `${n(l.never_checked)} of ${n(l.torrents)}`]);
  out.push(["Checked, last 24 h", n(l.checked_24h)]);
  out.push(["Checks per hour", n(l.checks_per_hour)]);
  if ((l.trackers ?? 0) > 0) out.push(["Trackers backing off", `${n(l.trackers_backed_off)} of ${n(l.trackers)}`]);
  const b = l.last_batch;
  if (b) {
    out.push([
      "Last batch",
      `${n(b.hashes)} in ${formatUptime((b.ms ?? 0) / 1000)}` + (l.last_batch_at != null ? `, ${formatAgo(l.last_batch_at, now)}` : ""),
    ]);
  }
  return out;
}

/** Features the Status card names when the server lacks them. */
const NAMED_FEATURES = Object.freeze([
  ["stats", "charts"], ["graphs", "graphs"], ["saved_search_poll", "saved-search alerts"],
  ["filters", "release filters"], ["origin", "Nostr / DHT source"], ["crawl_sources", "discovery sources"],
  ["crawl_map", "crawl map"], ["probe_filters", "probe filters"], ["probe_kinds", "track-kind filters"],
]);

/**
 * Features the server lacks, as the "Needs a newer server for" line names them.
 * @param {ReadonlySet<string>} features
 */
export function missingFeatures(features) {
  return NAMED_FEATURES.filter(([f]) => !features.has(f)).map(([, label]) => label);
}

/**
 * The search index (D43) as label → value lines.
 * @param {SearchIndexStatus} s
 * @param {string} [locale]
 * @param {number} [now]
 * @returns {Line[]}
 */
export function indexLines(s, locale = defaultLocale(), now = nowSec()) {
  const state = s.state ?? "";
  let st;
  if (state === "ready") st = "ready";
  else if (state === "building") {
    const total = s.build_total ?? 0;
    st = "building" + (total > 0 ? ` (${formatPercent((s.build_done ?? 0) / total, locale)})` : "");
  } else if (state === "error") st = "error" + (s.error != null ? `: ${s.error}` : "");
  else st = state.trim() === "" ? "unknown" : state;
  /** @type {Line[]} */
  const out = [["State", st], ["Searchable torrents", formatNumber(s.docs ?? 0, locale)]];
  const pending = s.pending ?? 0;
  out.push([
    "Changes queued",
    formatNumber(pending, locale) + (pending > 0 && s.lag_secs != null ? ` (oldest ${formatUptime(s.lag_secs)})` : ""),
  ]);
  if (s.last_commit != null) out.push(["Last update", formatAgo(s.last_commit, now)]);
  if ((s.bytes ?? 0) > 0) out.push(["Size", formatBytes(s.bytes)]);
  return out;
}

/**
 * The server's DHT crawler (D58, D74) as label → value lines.
 * @param {DhtStats} d
 * @param {string} [locale]
 * @returns {Line[]}
 */
export function dhtLines(d, locale = defaultLocale()) {
  /** @type {Line[]} */
  const out = [];
  const mode = d.mode ?? "";
  const vn = d.virtual_nodes ?? 0;
  if (d.running) out.push(["Crawler", `${mode} · ${vn} node${vn === 1 ? "" : "s"} · up ${formatUptime(d.uptime_secs ?? 0)}`]);
  else if (d.last_error != null) out.push(["Crawler", `restarting after an error: ${d.last_error}`]);
  else out.push(["Crawler", `${mode} · starting`]);
  const stored = formatNumber(d.stored_total ?? 0, locale) + " torrents";
  if (!d.running) {
    out.push(["Stored", stored]);
    return out;
  }
  out.push(["Discovery", formatRate(d.hashes_per_min ?? 0, "min", locale) + " new hashes"]);
  out.push(["Queue", formatNumber(d.queue_len ?? 0, locale) + " waiting for peers" + (d.sampling_paused ? " (sampling paused)" : "")]);
  out.push([
    "Metadata",
    formatRate(d.metadata_per_min ?? 0, "min", locale) + " · " + formatPercent(d.fetch_success_rate ?? 0, locale) +
    " of fetches succeed",
  ]);
  out.push(["Stored", stored + ((d.adult_total ?? 0) > 0 ? ` (${formatNumber(d.adult_total ?? 0, locale)} adult)` : "")]);
  if ((d.blocked_total ?? 0) > 0) out.push(["Blocked (illegal content)", formatNumber(d.blocked_total ?? 0, locale)]);
  out.push([
    "DHT queries",
    formatRate(d.queries_per_sec ?? 0, "s", locale) + " · " + formatPercent(d.answered_share ?? 0, locale) + " answered" +
    (d.brake ? " · brake on" : ""),
  ]);
  if (mode === "aggressive") out.push(["Incoming queries", formatRate(d.incoming_per_min ?? 0, "min", locale)]);
  const u = d.upnp;
  if (u && (u.state ?? "off") !== "off") {
    const maps = (u.mappings ?? []).map((m) => `${m.protocol ?? ""} ${m.port ?? 0}`).join(", ");
    let v;
    switch (u.state) {
      case "mapped":
        v = maps;
        break;
      case "partial":
        v = `${maps} (some failed: ${u.last_error ?? "?"})`;
        break;
      case "failed":
        v = `not mapped: ${u.last_error ?? "no UPnP gateway answered"}`;
        break;
      case "searching":
        v = "looking for the router…";
        break;
      default:
        v = u.state ?? "";
    }
    out.push(["Router ports", v]);
  }
  if ((d.external_ip ?? "unknown") !== "unknown") out.push(["Address", d.external_ip ?? ""]);
  return out;
}
