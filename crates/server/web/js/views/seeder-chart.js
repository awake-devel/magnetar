// Step chart of a torrent's seed history (SeederChart.kt, D45): seeders = max(tracker, DHT
// estimate), unanswered points skipped. SVG with role=img and its numbers written out.

import { chartColors } from "../chart-colors.js";
import { seederSeries, stepPath } from "../charts.js";
import { h, svg } from "../dom.js";
import { formatDate, nowSec } from "../format.js";

const W = 600;
const H = 120;

/**
 * @param {readonly import("../types-stats.js").LivenessPoint[]} points
 * @param {number} [now]
 */
export function seederChart(points, now = nowSec()) {
  const { seeders, leechers } = seederSeries(points);
  if (seeders.length === 0 && leechers.length === 0) return h("p", { class: "muted", text: "No seed checks recorded yet" });
  const colors = chartColors();
  const start = points[0].at;
  const end = Math.max(now, points[points.length - 1].at + 1);
  const top = Math.max(1, ...seeders.map((p) => p[1]), ...leechers.map((p) => p[1]));
  const last = seeders.length > 0 ? seeders[seeders.length - 1][1] : null;
  const label = `Seeders over time: ${seeders.length} checks, at most ${top}` + (last != null ? `, latest ${last}` : "");
  const chart = svg(
    "svg",
    { viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: "none", class: "chart seeder-chart", role: "img", "aria-label": label },
    svg("line", { x1: 0, y1: H - 1, x2: W, y2: H - 1, stroke: colors.axis, "stroke-width": 2, "vector-effect": "non-scaling-stroke" }),
    svg("line", { x1: 0, y1: 2, x2: W, y2: 2, stroke: colors.grid, "stroke-width": 1, "vector-effect": "non-scaling-stroke" }),
    leechers.length > 0
      ? svg("path", {
        d: stepPath(leechers, start, end, top, W, H),
        fill: "none",
        stroke: colors.categorical[1],
        "stroke-width": 2,
        "vector-effect": "non-scaling-stroke",
      })
      : null,
    seeders.length > 0
      ? svg("path", {
        d: stepPath(seeders, start, end, top, W, H),
        fill: "none",
        stroke: colors.categorical[0],
        "stroke-width": 2.5,
        "vector-effect": "non-scaling-stroke",
      })
      : null,
  );
  /** @param {string} color @param {string} text */
  const legend = (color, text) => {
    const dot = h("span", { class: "dot", "aria-hidden": "true" });
    dot.style.setProperty("background", color);
    return h("span", { class: "legend-item" }, dot, text);
  };
  return h(
    "figure",
    { class: "seeder-figure" },
    h(
      "div",
      { class: "chart-head" },
      h("span", { class: "muted", text: `max ${top}` }),
      h("span", { class: "legend" }, legend(colors.categorical[0], "seeders"), legend(colors.categorical[1], "leechers")),
    ),
    chart,
    h("div", { class: "chart-axis" }, h("span", { text: formatDate(start) }), h("span", { text: "now" })),
    h("figcaption", { class: "muted", text: label + (leechers.length > 0 ? `; leechers at most ${Math.max(...leechers.map((p) => p[1]))}` : "") }),
  );
}
