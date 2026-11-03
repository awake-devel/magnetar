// The Saved tab's summary line (SavedScreen.kt `describe`) and list order. Pure.

import { formatBytes, formatRating } from "./format.js";
import { categoryLabel, facetLabel } from "./labels.js";

/**
 * @param {unknown} v
 * @returns {string[]}
 */
function list(v) {
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
 * One line of what a saved query asks: "\"dune\" · Video · 2160p (4K) · ★ ≥ 7.0".
 * @param {Record<string, unknown> | null | undefined} query a SearchQuery body
 */
export function describe(query) {
  const q = query ?? {};
  /** @type {string[]} */
  const out = [];
  const text = typeof q.text === "string" ? q.text : "";
  out.push(text.trim() === "" ? "any text" : `"${text}"`);
  const cats = list(q.categories);
  if (cats.length > 0) out.push(cats.map(categoryLabel).join("/"));
  const origins = list(q.origins);
  if (origins.length > 0) out.push(origins.map((o) => facetLabel("origin", o)).join("/"));
  const minSize = num(q.min_size);
  const maxSize = num(q.max_size);
  if (minSize != null) out.push(`≥ ${formatBytes(minSize)}`);
  if (maxSize != null) out.push(`≤ ${formatBytes(maxSize)}`);
  const minSeeders = num(q.min_seeders);
  if (minSeeders != null) out.push(`≥ ${minSeeders} seeders`);
  if (q.hide_dead === true) out.push("alive only");
  const minTrust = num(q.min_trust);
  if (minTrust != null) out.push(`trust ≥ ${minTrust}`);
  /** @type {[string, string][]} */
  const lists = [
    ["language", "languages"], ["subtitles", "subtitles"], ["resolution", "resolutions"], ["source", "sources"],
    ["codec", "video_codecs"], ["hdr", "hdr"], ["audio", "audio"], ["genre", "genres"], ["media_kind", "media_kinds"],
  ];
  for (const [field, key] of lists) {
    const values = list(q[key]);
    if (values.length > 0) out.push(values.map((v) => facetLabel(field, v)).join("/"));
  }
  /** @type {[string, string][]} */
  const probes = [
    ["probe_language", "probe_languages"], ["probe_audio_kind", "probe_audio_kinds"],
    ["probe_subtitles", "probe_subtitles"], ["probe_sub_kind", "probe_sub_kinds"],
    ["probe_sub_format", "probe_sub_formats"], ["probe_resolution", "probe_resolutions"],
    ["probe_codec", "probe_video_codecs"], ["probe_hdr", "probe_hdr"], ["probe_audio", "probe_audio"],
  ];
  const probeParts = probes.map(([field, key]) => list(q[key]).map((v) => facetLabel(field, v)).join("/"))
    .filter((s) => s !== "");
  if (probeParts.length > 0) out.push("files: " + probeParts.join(", "));
  const minRating = num(q.min_rating);
  if (minRating != null) out.push(formatRating(minRating).replace("★ ", "★ ≥ "));
  const minYear = num(q.min_year);
  const maxYear = num(q.max_year);
  if (minYear != null && maxYear != null) out.push(`${minYear}–${maxYear}`);
  else if (minYear != null) out.push(`≥ ${minYear}`);
  else if (maxYear != null) out.push(`≤ ${maxYear}`);
  return out.join(" · ");
}

/**
 * Saved searches by name, case-insensitive (as the app's `sortedBy { name.lowercase() }`).
 * @template {{name?: string}} T
 * @param {readonly T[]} searches
 * @returns {T[]}
 */
export function sortByName(searches) {
  return [...searches].sort((a, b) => {
    const x = (a.name ?? "").toLowerCase();
    const y = (b.name ?? "").toLowerCase();
    return x < y ? -1 : x > y ? 1 : 0;
  });
}
