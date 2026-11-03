// Port of android/.../ui/stats/ProbesModel.kt: the "Header probes" card of the Status tab (D117,
// D118, docs/PROBES.md §10) and "Your corrections" (D130, D131). Counts only. Pure.

import { formatAgo, nowSec } from "./format.js";
import { defaultLocale, formatNumber, formatPercent } from "./numfmt.js";
import { AUDIO_KIND_LABELS, SUB_KIND_LABELS } from "./probe-format.js";
import { redactIpHost } from "./sources-model.js";

/** @typedef {import("./types-stats.js").ProbeBackfillStats} ProbeBackfillStats */
/** @typedef {import("./types-stats.js").WorkerStats} WorkerStats */
/** @typedef {import("./types-stats.js").ProbeCorrectionStats} ProbeCorrectionStats */
/** @typedef {import("./types-stats.js").FleetStats} FleetStats */
/** @typedef {[string, string]} Line */

/**
 * The `(name, count)` pairs that decoded, most first. A malformed entry is dropped; a blank name
 * is a real bucket (closed rows with no recorded outcome).
 * @param {unknown} pairs
 * @returns {{name: string, count: number}[]}
 */
export function named(pairs) {
  if (!Array.isArray(pairs)) return [];
  /** @type {{name: string, count: number, i: number}[]} */
  const out = [];
  pairs.forEach((p, i) => {
    if (!Array.isArray(p) || typeof p[0] !== "string") return;
    const c = typeof p[1] === "number" && Number.isFinite(p[1]) ? p[1] : 0;
    out.push({ name: p[0], count: c, i });
  });
  return out.sort((a, b) => b.count - a.count || a.i - b.i).map(({ name, count }) => ({ name, count }));
}

/**
 * Backfill progress: torrents with a probe against those still to try.
 * @param {ProbeBackfillStats} b
 */
export function backfillProgress(b) {
  const probed = Math.max(0, b.probed ?? 0);
  const remaining = Math.max(0, Math.max(b.remaining ?? 0, (b.waiting ?? 0) + (b.leased ?? 0) + (b.retrying ?? 0)));
  const total = probed + remaining;
  return { probed, remaining, total, fraction: total > 0 ? Math.min(1, Math.max(0, probed / total)) : 0 };
}

/**
 * "42% probed", "<1% probed", "100% probed", "nothing queued".
 * @param {ReturnType<typeof backfillProgress>} p
 * @param {string} [locale]
 */
export function backfillPercent(p, locale = defaultLocale()) {
  if (p.total <= 0) return "nothing queued";
  if (p.remaining === 0) return formatPercent(1, locale) + " probed";
  if (p.probed > 0 && p.fraction < 0.005) return "<" + formatPercent(0.01, locale) + " probed";
  // Never round up to 100% while something is left.
  return formatPercent(p.fraction >= 0.995 ? 0.99 : p.fraction, locale) + " probed";
}

/**
 * Label of a closed outcome (`no_peers` → "no peers").
 * @param {string | null | undefined} outcome
 */
export function outcomeLabel(outcome) {
  switch (outcome) {
    case null:
    case undefined:
    case "":
      return "unknown";
    case "no_peers":
      return "no peers";
    case "v2_only":
      return "v2 only";
    case "oversize":
      return "too large";
    default: {
      const t = outcome.trim();
      return (t === "" ? "unknown" : t).replaceAll("_", " ");
    }
  }
}

/**
 * The backfill queue as label → value lines, under its progress bar.
 * @param {ProbeBackfillStats} b
 * @param {string} [locale]
 * @param {number} [now]
 * @returns {Line[]}
 */
export function backfillLines(b, locale = defaultLocale(), now = nowSec()) {
  const n = (/** @type {number | undefined} */ v) => formatNumber(v ?? 0, locale);
  const p = backfillProgress(b);
  /** @type {Line[]} */
  const out = [
    ["Probed", `${n(p.probed)} of ${n(p.total)}`],
    ["Remaining", n(p.remaining)],
    ["Waiting / leased / retrying", `${n(b.waiting)} / ${n(b.leased)} / ${n(b.retrying)}`],
  ];
  const closed = named(b.closed);
  if (closed.length > 0) {
    out.push(["Closed", n(closed.reduce((a, c) => a + c.count, 0))]);
    for (const c of closed) out.push(["  " + outcomeLabel(c.name), n(c.count)]);
  }
  out.push([
    "Queue fill",
    b.filling ? "filling now" : b.filled_at != null ? "last pass " + formatAgo(b.filled_at, now) : "not filled yet",
  ]);
  if ((b.at ?? 0) > 0) out.push(["Counted", formatAgo(/** @type {number} */ (b.at), now)]);
  return out;
}

/**
 * Badges of a worker block: what it is set to do.
 * @param {WorkerStats} w
 */
export function workerBadges(w) {
  const d = w.dht;
  if (!d) return [];
  /** @type {string[]} */
  const out = [];
  if (d.discover === false) out.push("discovery off");
  if (d.backfill != null) out.push("backfill on");
  if (d.probe != null) out.push("probes on");
  return out;
}

/**
 * A worker's backfill counters. Empty when it never took part.
 * @param {WorkerStats} w
 * @param {string} [locale]
 * @returns {Line[]}
 */
export function workerBackfillLines(w, locale = defaultLocale()) {
  const n = (/** @type {number | undefined} */ v) => formatNumber(v ?? 0, locale);
  const b = w.dht?.backfill ?? null;
  if (!w.backfill_leased_total && !w.backfill_probed_total && !w.backfill_unprobed_total && b == null) return [];
  /** @type {Line[]} */
  const out = [
    ["Leased", n(w.backfill_leased_total)],
    ["Probed / without a probe", `${n(w.backfill_probed_total)} / ${n(w.backfill_unprobed_total)}`],
  ];
  if (b != null) {
    out.push(["In the worker now", n(b.outstanding)]);
    out.push(["Fetched / no peers / failed", `${n(b.fetched)} / ${n(b.no_peers)} / ${n(b.failed)}`]);
    if ((b.lease_errors ?? 0) > 0) out.push(["Lease errors", n(b.lease_errors)]);
  }
  return out;
}

/**
 * The user checked or added at least one track: worth a "Your corrections" block.
 * @param {ProbeCorrectionStats | null | undefined} s
 */
export function hasCorrections(s) {
  return s != null && (s.tracks ?? 0) + (s.added ?? 0) > 0;
}

/**
 * "SDH (subtitles)", "Audio description (audio)".
 * @param {string} track
 * @param {string} kind
 */
function kindTitle(track, kind) {
  const labels = track === "audio" ? AUDIO_KIND_LABELS : SUB_KIND_LABELS;
  const raw = Object.hasOwn(labels, kind) ? labels[kind] : kind.replaceAll("_", " ");
  const label = raw === "" ? raw : raw[0].toUpperCase() + raw.slice(1);
  return `${label} (` + (track === "audio" ? "audio" : "subtitles") + ")";
}

/**
 * "Your corrections": at most 8 kind lines (most wrong first, as sent) and 4 missed kinds.
 * @param {ProbeCorrectionStats} s
 * @param {string} [locale]
 * @returns {Line[]}
 */
export function correctionLines(s, locale = defaultLocale()) {
  const n = (/** @type {number | undefined} */ v) => formatNumber(v ?? 0, locale);
  const l = s.languages ?? {};
  /** @type {Line[]} */
  const out = [
    ["Tracks checked", n(s.tracks)],
    ["Matched VLC", n(s.checked)],
    ["Corrected", n(s.corrected)],
    ["Not in the file", n(s.removed)],
    ["Added by you", n(s.added)],
    ["Languages", `${n(l.wrong)} of ${n(l.checked)} wrong` + ((l.filled ?? 0) > 0 ? `, ${n(l.filled)} filled in` : "")],
  ];
  for (const k of (s.kinds ?? []).slice(0, 8)) {
    const from = k.source === "name" ? "track names" : "file flags";
    out.push([`${kindTitle(k.track ?? "", k.kind ?? "")} from ${from}`, `${n(k.wrong)} of ${n(k.checked)} wrong`]);
  }
  for (const m of (s.missed ?? []).slice(0, 4)) out.push([`${kindTitle(m.track ?? "", m.kind ?? "")} missed`, n(m.count)]);
  return out;
}

/**
 * The fleet answer with every IP-literal worker name redacted (as the app does).
 * @param {FleetStats} f
 * @returns {FleetStats}
 */
export function redactFleet(f) {
  return { ...f, workers: (f.workers ?? []).map((w) => ({ ...w, name: redactIpHost(w.name ?? "") })) };
}
