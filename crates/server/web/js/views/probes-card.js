// Header probes (ProbesCard.kt, D117, D118, D131): the backfill queue's progress, its states and
// closed outcomes, one block per worker, then "Your corrections". Counts only; worker names are
// untrusted text with IP literals redacted.

import { h, untrusted } from "../dom.js";
import { cachedAgo } from "../format.js";
import {
  backfillLines,
  backfillPercent,
  backfillProgress,
  correctionLines,
  hasCorrections,
  workerBackfillLines,
  workerBadges,
} from "../probes-model.js";
import { kvLines } from "./stats-charts.js";

/** @param {string} color */
function dot(color) {
  const d = h("span", { class: "dot", "aria-hidden": "true" });
  d.style.setProperty("background", color);
  return d;
}

/**
 * Whether the card has anything to show.
 * @param {import("../types-stats.js").FleetStats | null} fleet
 */
export function showsProbes(fleet) {
  return fleet != null && (fleet.probe_backfill != null || hasCorrections(fleet.probe_corrections));
}

/**
 * @param {import("../types-stats.js").FleetStats} fleet
 * @param {import("../chart-colors.js").ChartColors} colors
 * @param {number | null} cachedAt
 */
export function probesCard(fleet, colors, cachedAt) {
  /** @type {Node[]} */
  const body = [];
  if (cachedAt != null) body.push(h("p", { class: "muted small", text: cachedAgo(cachedAt) }));
  const b = fleet.probe_backfill;
  if (b) {
    const p = backfillProgress(b);
    const pct = backfillPercent(p);
    const fill = h("span", { class: "bar-fill" });
    fill.style.setProperty("--w", String(p.fraction));
    fill.style.setProperty("background", colors.good);
    body.push(h("div", { class: "route-head" }, h("span", { class: "muted small", text: "Backfill" }), h("span", { class: "small", text: pct })));
    body.push(h("div", {
      class: "progress-meter",
      role: "progressbar",
      "aria-label": `Backfill ${pct}`,
      "aria-valuemin": "0",
      "aria-valuemax": "100",
      "aria-valuenow": String(Math.floor(p.fraction * 100)),
    }, h("span", { class: "bar-track" }, fill)));
    body.push(kvLines(backfillLines(b)));
    const workers = [...(fleet.workers ?? [])].sort((x, y) => ((x.name ?? "") < (y.name ?? "") ? -1 : 1));
    if (workers.length > 0) {
      body.push(h("h4", { class: "section-title", text: "Workers" }));
      for (const w of workers) {
        const lines = workerBackfillLines(w);
        body.push(h(
          "div",
          { class: "machine" },
          h(
            "div",
            { class: "machine-head" },
            h("strong", null, untrusted((w.name ?? "").trim() === "" ? "?" : w.name)),
            h("span", { class: "small" }, dot(w.connected ? colors.good : colors.unknown), w.connected ? "connected" : "not connected"),
          ),
          workerBadges(w).length > 0 ? h("p", { class: "badges" }, ...workerBadges(w).map((x) => h("span", { class: "tag", text: x }))) : null,
          lines.length === 0 ? h("p", { class: "muted small", text: "no backfill" }) : kvLines(lines),
        ));
      }
    }
  }
  const c = fleet.probe_corrections;
  if (c && hasCorrections(c)) {
    body.push(h("h4", { class: "section-title", text: "Your corrections" }));
    body.push(kvLines(correctionLines(c)));
  }
  return h("section", { class: "card", "aria-label": "Header probes" }, h("h3", { class: "card-title", text: "Header probes" }), ...body);
}
