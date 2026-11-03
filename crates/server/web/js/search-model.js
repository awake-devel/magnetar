// Port of android/.../ui/search/SearchModel.kt, plus the web's URL <-> search state mapping
// (docs/WEB.md "Routing and URL parameters"). Pure: filters are frozen plain objects whose list
// fields are sorted arrays, so equal filters serialise to the same JSON and the same URL.

import { FILE_KIND_FACET_SIZE, FILE_KIND_FIELD } from "./file-kinds.js";
import { facetLabel, FILTER_OPTIONS } from "./labels.js";

/** @typedef {import("./types.js").FacetCounts} FacetCounts */

/**
 * @typedef {object} DatePreset
 * @property {"any" | "day" | "week" | "month" | "year"} key
 * @property {string} label
 * @property {number | null} seconds
 */

/** "Published" presets of the filter sheet. */
export const DATE_PRESETS = Object.freeze(/** @type {readonly DatePreset[]} */ ([
  Object.freeze({ key: "any", label: "Any time", seconds: null }),
  Object.freeze({ key: "day", label: "24 h", seconds: 86_400 }),
  Object.freeze({ key: "week", label: "7 days", seconds: 7 * 86_400 }),
  Object.freeze({ key: "month", label: "30 days", seconds: 30 * 86_400 }),
  Object.freeze({ key: "year", label: "1 year", seconds: 365 * 86_400 }),
]));

/**
 * @param {string} key
 * @returns {DatePreset}
 */
export function datePreset(key) {
  return DATE_PRESETS.find((p) => p.key === key) ?? DATE_PRESETS[0];
}

/** Facets of the filter sheet with chips, in order (docs/FILTERS.md names). */
export const FACET_FIELDS = Object.freeze([
  "language", "subtitles", "resolution", "source", "codec", "hdr", "audio", "genre", "media_kind",
]);

/** @type {Readonly<Record<string, string>>} */
export const FACET_TITLES = Object.freeze({
  language: "Audio language",
  subtitles: "Subtitles",
  resolution: "Resolution",
  source: "Release source",
  codec: "Video codec",
  hdr: "HDR",
  audio: "Audio",
  genre: "Genre",
  media_kind: "Type",
  probe_language: "Audio track",
  probe_subtitles: "Subtitle track",
  probe_resolution: "Picture",
  probe_codec: "Video track",
  probe_hdr: "HDR (probed)",
  probe_audio: "Audio format",
  probe_audio_kind: "Audio track kind",
  probe_sub_kind: "Subtitle kind",
  probe_sub_format: "Subtitle format",
  probe_file_kind: "File kind",
});

/** Header-probe facets (D122), in the sheet's order. */
export const PROBE_FACET_FIELDS = Object.freeze([
  "probe_language", "probe_audio_kind", "probe_subtitles", "probe_sub_kind", "probe_sub_format",
  "probe_resolution", "probe_codec", "probe_hdr", "probe_audio",
]);

/** File kinds from the probed files (D137): only with `probe_file_kinds`; first of the probe group. */
export { FILE_KIND_FIELD };

/** Track-kind facets (D125): only with `probe_kinds`. */
export const PROBE_KIND_FIELDS = Object.freeze(["probe_audio_kind", "probe_sub_kind", "probe_sub_format"]);

export const KIND_PAIR_HINT = "A kind with a language means one track: French + SDH = a French SDH track.";

/** Category codes in the order of the sheet. */
export const CATEGORIES = Object.freeze([
  "video", "audio", "application", "game", "porn", "anime", "book", "picture", "other",
]);

/** Source chips (D51): value and label. */
export const ORIGIN_CHIPS = Object.freeze(/** @type {readonly (readonly [string, string])[]} */ ([
  Object.freeze(/** @type {const} */ (["nostr", "Nostr"])),
  Object.freeze(/** @type {const} */ (["dht", "DHT"])),
]));

export const SORTS = Object.freeze(/** @type {const} */ (["relevance", "date", "size", "seeders", "rating"]));
/** @typedef {typeof SORTS[number]} Sort */

/** @type {Readonly<Record<Sort, string>>} */
export const SORT_LABELS = Object.freeze({
  relevance: "Relevance",
  date: "Date",
  size: "Size",
  seeders: "Seeders",
  rating: "Rating",
});

export const DEFAULT_LIMIT = 50;
export const MAX_OFFSET = 10_000;

/**
 * List fields: filter key, facet field, request key, URL parameter.
 * @type {readonly (readonly [string, string, string, string])[]}
 */
export const LIST_FIELDS = Object.freeze([
  ["categories", "category", "categories", "cat"],
  ["languages", "language", "languages", "lang"],
  ["subtitles", "subtitles", "subtitles", "sub"],
  ["resolutions", "resolution", "resolutions", "res"],
  ["sources", "source", "sources", "source"],
  ["codecs", "codec", "video_codecs", "codec"],
  ["hdr", "hdr", "hdr", "hdr"],
  ["audio", "audio", "audio", "audio"],
  ["genres", "genre", "genres", "genre"],
  ["kinds", "media_kind", "media_kinds", "kind"],
  ["origins", "origin", "origins", "origin"],
  ["probeLanguages", "probe_language", "probe_languages", "probe_lang"],
  ["probeSubtitles", "probe_subtitles", "probe_subtitles", "probe_sub"],
  ["probeResolutions", "probe_resolution", "probe_resolutions", "probe_res"],
  ["probeCodecs", "probe_codec", "probe_video_codecs", "probe_codec"],
  ["probeHdr", "probe_hdr", "probe_hdr", "probe_hdr"],
  ["probeAudio", "probe_audio", "probe_audio", "probe_audio"],
  ["probeAudioKinds", "probe_audio_kind", "probe_audio_kinds", "probe_audio_kind"],
  ["probeSubKinds", "probe_sub_kind", "probe_sub_kinds", "probe_sub_kind"],
  ["probeSubFormats", "probe_sub_format", "probe_sub_formats", "probe_sub_format"],
  ["probeFileKinds", "probe_file_kind", "probe_file_kinds", "probe_file_kind"],
].map((r) => Object.freeze(/** @type {[string, string, string, string]} */ (r))));

/**
 * What the filter sheet sets. List fields are sorted arrays.
 * @typedef {object} SearchFilters
 * @property {readonly string[]} categories
 * @property {number | null} minSizeMb
 * @property {number | null} maxSizeMb
 * @property {DatePreset["key"]} date
 * @property {number | null} minSeeders
 * @property {boolean} hideDead
 * @property {number | null} minTrust
 * @property {readonly string[]} languages
 * @property {readonly string[]} subtitles
 * @property {readonly string[]} resolutions
 * @property {readonly string[]} sources
 * @property {readonly string[]} codecs
 * @property {readonly string[]} hdr
 * @property {readonly string[]} audio
 * @property {readonly string[]} genres
 * @property {readonly string[]} kinds
 * @property {number | null} minRating
 * @property {number | null} minYear
 * @property {number | null} maxYear
 * @property {readonly string[]} origins
 * @property {readonly string[]} probeLanguages
 * @property {readonly string[]} probeSubtitles
 * @property {readonly string[]} probeResolutions
 * @property {readonly string[]} probeCodecs
 * @property {readonly string[]} probeHdr
 * @property {readonly string[]} probeAudio
 * @property {readonly string[]} probeAudioKinds
 * @property {readonly string[]} probeSubKinds
 * @property {readonly string[]} probeSubFormats
 * @property {readonly string[]} probeFileKinds file kinds: group ids and label ids (D137)
 */

/** @type {SearchFilters} */
const EMPTY = {
  categories: [],
  minSizeMb: null,
  maxSizeMb: null,
  date: "any",
  minSeeders: null,
  hideDead: false,
  minTrust: null,
  languages: [],
  subtitles: [],
  resolutions: [],
  sources: [],
  codecs: [],
  hdr: [],
  audio: [],
  genres: [],
  kinds: [],
  minRating: null,
  minYear: null,
  maxYear: null,
  origins: [],
  probeLanguages: [],
  probeSubtitles: [],
  probeResolutions: [],
  probeCodecs: [],
  probeHdr: [],
  probeAudio: [],
  probeAudioKinds: [],
  probeSubKinds: [],
  probeSubFormats: [],
  probeFileKinds: [],
};

/**
 * @param {readonly string[]} list
 * @returns {readonly string[]}
 */
function sortedSet(list) {
  return Object.freeze([...new Set(list)].sort());
}

/**
 * New filters from the defaults and the given fields (lists sorted, deduplicated, frozen).
 * @param {Partial<SearchFilters>} [over]
 * @returns {Readonly<SearchFilters>}
 */
export function makeFilters(over = {}) {
  /** @type {Record<string, unknown>} */
  const f = { ...EMPTY, ...over };
  for (const [key] of LIST_FIELDS) f[key] = sortedSet(/** @type {string[]} */ (f[key] ?? []));
  return Object.freeze(/** @type {SearchFilters} */ (f));
}

export const EMPTY_FILTERS = makeFilters();

/**
 * @param {string} field facet field ("category", "language", …)
 * @returns {string | null} the filter key
 */
function keyOfField(field) {
  const row = LIST_FIELDS.find((r) => r[1] === field);
  return row ? row[0] : null;
}

/**
 * Selected values of facet `field` (also `category` and `origin`).
 * @param {SearchFilters} f
 * @param {string} field
 * @returns {readonly string[]}
 */
export function selected(f, field) {
  const key = keyOfField(field);
  if (key == null) return [];
  return /** @type {readonly string[]} */ (/** @type {Record<string, unknown>} */ (f)[key]);
}

/**
 * `value` of facet `field` switched on or off.
 * @param {SearchFilters} f
 * @param {string} field
 * @param {string} value
 */
export function toggle(f, field, value) {
  const key = keyOfField(field);
  if (key == null) return f;
  const cur = selected(f, field);
  const next = cur.includes(value) ? cur.filter((v) => v !== value) : [...cur, value];
  return makeFilters({ ...f, [key]: next });
}

/**
 * How many filters are set (the Filters badge).
 * @param {SearchFilters} f
 */
export function activeCount(f) {
  return [
    f.categories.length > 0, f.minSizeMb != null || f.maxSizeMb != null, f.date !== "any",
    f.minSeeders != null, f.hideDead, f.minTrust != null,
    f.languages.length > 0, f.subtitles.length > 0, f.resolutions.length > 0, f.sources.length > 0,
    f.codecs.length > 0, f.hdr.length > 0, f.audio.length > 0, f.genres.length > 0, f.kinds.length > 0,
    f.minRating != null, f.minYear != null || f.maxYear != null, f.origins.length > 0,
    f.probeLanguages.length > 0, f.probeSubtitles.length > 0, f.probeResolutions.length > 0,
    f.probeCodecs.length > 0, f.probeHdr.length > 0, f.probeAudio.length > 0,
    f.probeAudioKinds.length > 0, f.probeSubKinds.length > 0, f.probeSubFormats.length > 0,
    f.probeFileKinds.length > 0,
  ].filter(Boolean).length;
}

/**
 * Any header-probe filter set (D122).
 * @param {SearchFilters} f
 */
export function hasProbeFilters(f) {
  return f.probeFileKinds.length > 0 || PROBE_FACET_FIELDS.some((field) => selected(f, field).length > 0);
}

/**
 * Only what the server can apply: a field it would ignore must not be sent (docs/APP.md §4).
 * @param {SearchFilters} f
 * @param {ReadonlySet<string> | readonly string[]} features
 */
export function supportedBy(f, features) {
  const has = (/** @type {string} */ x) =>
    Array.isArray(features) ? features.includes(x) : /** @type {ReadonlySet<string>} */ (features).has(x);
  /** @type {Partial<SearchFilters>} */
  const o = { ...f };
  if (!has("filters")) {
    Object.assign(o, {
      languages: [], subtitles: [], resolutions: [], sources: [], codecs: [], hdr: [], audio: [], genres: [],
      kinds: [], minRating: null, minYear: null, maxYear: null,
    });
  }
  if (!has("origin")) o.origins = [];
  if (!has("probe_filters")) {
    Object.assign(o, {
      probeLanguages: [], probeSubtitles: [], probeResolutions: [], probeCodecs: [], probeHdr: [], probeAudio: [],
    });
  }
  if (!has("probe_filters") || !has("probe_kinds")) {
    Object.assign(o, { probeAudioKinds: [], probeSubKinds: [], probeSubFormats: [] });
  }
  if (!has("probe_file_kinds")) o.probeFileKinds = [];
  return makeFilters(o);
}

/**
 * Rating order needs the release filters (D44).
 * @param {Sort} sort
 * @param {ReadonlySet<string>} features
 * @returns {Sort}
 */
export function supportedSort(sort, features) {
  return sort !== "rating" || features.has("filters") ? sort : "relevance";
}

const MB = 1024 * 1024;

/**
 * The request body of a search (SearchQuery). Null fields are left out, as the app's
 * `explicitNulls = false`.
 * @param {string} text
 * @param {SearchFilters} f
 * @param {Sort} sort
 * @param {boolean} descending
 * @param {boolean} includeAdult
 * @param {{offset?: number, limit?: number, nowSec?: number}} [opts]
 * @returns {Record<string, unknown>}
 */
export function buildQuery(text, f, sort, descending, includeAdult, opts = {}) {
  const now = opts.nowSec ?? Math.floor(Date.now() / 1000);
  const preset = datePreset(f.date);
  /** @type {Record<string, unknown>} */
  const q = {
    text: text.trim(),
    categories: [
      ...CATEGORIES.filter((c) => f.categories.includes(c)),
      ...f.categories.filter((c) => !CATEGORIES.includes(c)).sort(),
    ],
    min_size: f.minSizeMb != null ? f.minSizeMb * MB : null,
    max_size: f.maxSizeMb != null ? f.maxSizeMb * MB : null,
    since: preset.seconds != null ? now - preset.seconds : null,
    min_seeders: f.minSeeders,
    hide_dead: f.hideDead,
    min_trust: f.minTrust,
    sort,
    descending,
    limit: opts.limit ?? DEFAULT_LIMIT,
    offset: opts.offset ?? 0,
    include_adult: includeAdult,
    min_rating: f.minRating,
    min_year: f.minYear,
    max_year: f.maxYear,
  };
  for (const [key, , reqKey] of LIST_FIELDS) {
    if (key === "categories") continue;
    q[reqKey] = [.../** @type {readonly string[]} */ (/** @type {Record<string, unknown>} */ (f)[key])].sort();
  }
  for (const k of Object.keys(q)) if (q[k] == null) delete q[k];
  return q;
}

/**
 * The page-cache form of a query: the date preset instead of the clock, so "7 days" is the
 * same key tomorrow (as the app's cacheKey).
 * @param {string} text
 * @param {SearchFilters} f
 * @param {Sort} sort
 * @param {boolean} descending
 * @param {boolean} includeAdult
 */
export function cacheQuery(text, f, sort, descending, includeAdult) {
  const q = buildQuery(text, f, sort, descending, includeAdult, { nowSec: 0 });
  const s = datePreset(f.date).seconds;
  if (s != null) q.since = -s;
  return q;
}

/**
 * @param {unknown} v
 * @returns {string[]}
 */
function strList(v) {
  return Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];
}

/**
 * @param {unknown} v
 * @returns {number | null}
 */
function num(v) {
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

/**
 * Inverse of buildQuery, for running a saved search; `since` snaps to the nearest preset.
 * @param {Record<string, unknown>} q
 * @param {number} [nowSec]
 */
export function filtersOf(q, nowSec = Math.floor(Date.now() / 1000)) {
  const since = num(q.since);
  /** @type {DatePreset["key"]} */
  let date = "any";
  if (since != null) {
    const window = nowSec - since;
    let best = DATE_PRESETS[1];
    for (const p of DATE_PRESETS) {
      if (p.seconds == null) continue;
      if (Math.abs(p.seconds - window) < Math.abs(/** @type {number} */ (best.seconds) - window)) best = p;
    }
    date = best.key;
  }
  const minSize = num(q.min_size);
  const maxSize = num(q.max_size);
  /** @type {Record<string, unknown>} */
  const o = {
    minSizeMb: minSize != null ? Math.floor(minSize / MB) : null,
    maxSizeMb: maxSize != null ? Math.floor(maxSize / MB) : null,
    date,
    minSeeders: num(q.min_seeders),
    hideDead: q.hide_dead === true,
    minTrust: num(q.min_trust),
    minRating: num(q.min_rating),
    minYear: num(q.min_year),
    maxYear: num(q.max_year),
  };
  for (const [key, , reqKey] of LIST_FIELDS) o[key] = strList(q[reqKey]);
  return makeFilters(/** @type {Partial<SearchFilters>} */ (o));
}

/**
 * @typedef {object} FacetChip
 * @property {string} value
 * @property {string} label
 * @property {number | null} count
 */

/**
 * Chips of one facet: with counts, the counted values plus any selected value they lack (count
 * 0); without, the built-in list.
 * @param {string} field
 * @param {readonly string[]} sel
 * @param {FacetCounts | null | undefined} counts
 * @returns {FacetChip[]}
 */
export function facetChips(field, sel, counts) {
  const chips = counts
    ? (counts.counts ?? []).map((c) => ({ value: c.value, label: facetLabel(field, c.value), count: c.count }))
    : (FILTER_OPTIONS[field] ?? []).map((v) => ({ value: v, label: facetLabel(field, v), count: null }));
  const missing = sel.filter((v) => !chips.some((c) => c.value === v)).sort()
    .map((v) => ({ value: v, label: facetLabel(field, v), count: counts ? 0 : null }));
  return [...chips, ...missing];
}

/**
 * Keeps each facet's chips where they first appeared while the panel is open.
 */
export class ChipOrder {
  /** @type {Map<string, string[]>} */
  #order = new Map();

  /**
   * @param {string} field
   * @param {FacetChip[]} chips
   * @param {(value: string) => string} [labelOf] the label of a value that left the counts
   * @returns {FacetChip[]}
   */
  arrange(field, chips, labelOf = (v) => facetLabel(field, v)) {
    if (!chips.some((c) => c.count != null)) return chips;
    let known = this.#order.get(field);
    if (known == null) {
      known = chips.map((c) => c.value);
      this.#order.set(field, known);
    }
    const k = known;
    const byValue = new Map(chips.map((c) => [c.value, c]));
    const fresh = chips.filter((c) => !k.includes(c.value));
    if (fresh.length > 0) this.#order.set(field, [...k, ...fresh.map((c) => c.value)]);
    return [...k.map((v) => byValue.get(v) ?? { value: v, label: labelOf(v), count: 0 }), ...fresh];
  }
}

/**
 * Facet fields to ask `/facets` for, by the server's features.
 * @param {ReadonlySet<string>} features
 * @returns {string[]}
 */
export function facetFields(features) {
  const out = ["category"];
  if (features.has("origin")) out.push("origin");
  if (features.has("filters")) out.push(...FACET_FIELDS);
  if (features.has("probe_filters")) {
    out.push(
      ...(features.has("probe_kinds")
        ? PROBE_FACET_FIELDS
        : PROBE_FACET_FIELDS.filter((f) => !PROBE_KIND_FIELDS.includes(f))),
    );
  }
  if (features.has("probe_file_kinds")) out.push(FILE_KIND_FIELD);
  return out;
}

/** Values asked per facet field. */
export const FACET_SIZE = 40;

/**
 * The `/facets` body: every field the server has, FACET_SIZE values each, and every file kind
 * with a count (`sizes`, D137 A4) when the server lists them.
 * @param {Record<string, unknown>} query
 * @param {ReadonlySet<string>} features
 */
export function facetBody(query, features) {
  /** @type {{query: Record<string, unknown>, fields: string[], size: number, sizes?: Record<string, number>}} */
  const body = { query, fields: facetFields(features), size: FACET_SIZE };
  if (features.has("probe_file_kinds")) body.sizes = { [FILE_KIND_FIELD]: FILE_KIND_FACET_SIZE };
  return body;
}

/**
 * The collapsed text of a dropdown (D140): "Any", the one value's label, or "{first in option
 * order}, +{n-1}".
 * @param {readonly string[]} sel
 * @param {readonly string[]} optionOrder the option values in their shown order
 * @param {(value: string) => string} labelOf
 */
export function dropdownSummary(sel, optionOrder, labelOf) {
  if (sel.length === 0) return "Any";
  const first = optionOrder.find((v) => sel.includes(v)) ?? [...sel].sort()[0];
  return sel.length === 1 ? labelOf(first) : `${labelOf(first)}, +${sel.length - 1}`;
}

// ---------------------------------------------------------------------------------------------
// URL <-> search state (web only)

/**
 * @typedef {object} SearchState
 * @property {string} text
 * @property {Readonly<SearchFilters>} filters
 * @property {Sort} sort
 * @property {boolean} descending
 */

/** @returns {SearchState} */
export function emptySearchState() {
  return { text: "", filters: EMPTY_FILTERS, sort: "relevance", descending: true };
}

const VALUE_RE = /^[\p{L}\p{N} _+\-.:'/]{1,64}$/u;
const MAX_TEXT = 500;

/**
 * @param {string | null} s
 * @param {number} min
 * @param {number} max
 * @returns {number | null}
 */
function intParam(s, min, max) {
  if (s == null || !/^\d{1,12}$/.test(s)) return null;
  const n = Number(s);
  return n >= min && n <= max ? n : null;
}

/**
 * Search state from a URL query string. Unknown parameters are ignored and malformed ones
 * dropped, never an error.
 * @param {URLSearchParams} p
 * @returns {SearchState}
 */
export function stateFromParams(p) {
  /** @type {Record<string, unknown>} */
  const o = {};
  for (const [key, , , param] of LIST_FIELDS) {
    const raw = p.get(param);
    if (raw == null) continue;
    o[key] = raw.split(",").map((v) => v.trim()).filter((v) => VALUE_RE.test(v)).slice(0, 50);
  }
  o.minSizeMb = intParam(p.get("min_size_mb"), 0, 1e9);
  o.maxSizeMb = intParam(p.get("max_size_mb"), 0, 1e9);
  // The API's own byte parameters are read too (as MiB).
  if (o.minSizeMb == null) {
    const b = intParam(p.get("min_size"), 0, Number.MAX_SAFE_INTEGER);
    if (b != null) o.minSizeMb = Math.floor(b / MB);
  }
  if (o.maxSizeMb == null) {
    const b = intParam(p.get("max_size"), 0, Number.MAX_SAFE_INTEGER);
    if (b != null) o.maxSizeMb = Math.floor(b / MB);
  }
  const date = p.get("date");
  o.date = DATE_PRESETS.some((d) => d.key === date && d.key !== "any") ? date : "any";
  o.minSeeders = intParam(p.get("min_seeders"), 0, 1e9);
  o.hideDead = p.get("hide_dead") === "1" || p.get("hide_dead") === "true";
  o.minTrust = intParam(p.get("min_trust"), 0, 100);
  const r = p.get("min_rating");
  if (r != null && /^\d(\.\d)?$/.test(r)) {
    const v = Number(r);
    if (v > 0 && v <= 9.5 && Math.round(v * 2) === v * 2) o.minRating = v;
  }
  for (const k of /** @type {const} */ (["min_year", "max_year"])) {
    const y = p.get(k);
    if (y != null && /^\d{4}$/.test(y)) o[k === "min_year" ? "minYear" : "maxYear"] = Number(y);
  }
  const sortRaw = p.get("sort");
  const sort = /** @type {Sort} */ (SORTS.find((s) => s === sortRaw) ?? "relevance");
  const desc = p.get("desc");
  const text = (p.get("q") ?? "").slice(0, MAX_TEXT);
  return {
    text,
    filters: makeFilters(/** @type {Partial<SearchFilters>} */ (o)),
    sort,
    descending: !(desc === "0" || desc === "false"),
  };
}

/**
 * The URL query string of a search state (without "?"); "" for the empty state.
 * @param {SearchState} s
 */
export function stateToParams(s) {
  const p = new URLSearchParams();
  if (s.text !== "") p.set("q", s.text);
  const f = s.filters;
  for (const [key, , , param] of LIST_FIELDS) {
    const v = /** @type {readonly string[]} */ (/** @type {Record<string, unknown>} */ (f)[key]);
    if (v.length > 0) p.set(param, v.join(","));
  }
  if (f.minSizeMb != null) p.set("min_size_mb", String(f.minSizeMb));
  if (f.maxSizeMb != null) p.set("max_size_mb", String(f.maxSizeMb));
  if (f.date !== "any") p.set("date", f.date);
  if (f.minSeeders != null) p.set("min_seeders", String(f.minSeeders));
  if (f.hideDead) p.set("hide_dead", "1");
  if (f.minTrust != null) p.set("min_trust", String(f.minTrust));
  if (f.minRating != null) p.set("min_rating", String(f.minRating));
  if (f.minYear != null) p.set("min_year", String(f.minYear));
  if (f.maxYear != null) p.set("max_year", String(f.maxYear));
  if (s.sort !== "relevance") p.set("sort", s.sort);
  if (!s.descending) p.set("desc", "0");
  return p.toString();
}

/**
 * @param {SearchState} a
 * @param {SearchState} b
 */
export function sameState(a, b) {
  return stateToParams(a) === stateToParams(b);
}
