// Line icons drawn as SVG from fixed path data (24 × 24). Decorative: the control that holds
// one carries the accessible name (aria-label or a visible label).

import { svg } from "./dom.js";

/** @type {Readonly<Record<string, readonly string[]>>} */
const PATHS = Object.freeze({
  search: ["M10.5 4a6.5 6.5 0 1 0 0 13a6.5 6.5 0 1 0 0-13z", "M15.5 15.5L21 21"],
  star: ["M12 2.8l2.8 5.9 6.4.8-4.7 4.4 1.2 6.4L12 17.1 6.3 20.3l1.2-6.4-4.7-4.4 6.4-.8z"],
  saved: ["M6 3h12v18l-6-4.5L6 21z"],
  stats: ["M5 20V11", "M11 20V5", "M17 20v-8", "M3 20.5h18"],
  settings: ["M4 7h9", "M17 7h3", "M4 17h3", "M11 17h9", "M15 4.5v5", "M9 14.5v5"],
  filter: ["M3 5h18l-7 8.2V19l-4 2v-7.8z"],
  sort: ["M4 6h16", "M4 12h11", "M4 18h6"],
  close: ["M6 6l12 12", "M18 6L6 18"],
  back: ["M20 12H5", "M11 5l-7 7 7 7"],
  chevron: ["M9 6l6 6-6 6"],
  down: ["M12 5v14", "M6 13l6 6 6-6"],
  up: ["M12 19V5", "M6 11l6-6 6 6"],
  refresh: ["M20 12a8 8 0 1 1-2.4-5.7", "M20 4v5h-5"],
  check: ["M5 12.5l4.5 4.5L19 7"],
  copy: ["M9 9h11v11H9z", "M5 15H4V4h11v1"],
  external: ["M14 4h6v6", "M20 4l-9 9", "M18 14v6H4V6h6"],
  graph: ["M6 6a2 2 0 1 0 0 .1z", "M18 8a2 2 0 1 0 0 .1z", "M12 18a2 2 0 1 0 0 .1z", "M7.7 7l8.6 1", "M7 7.8l4.2 8.4", "M17 9.8l-4.2 6.4"],
  magnet: ["M6 4h4v8a2 2 0 0 0 4 0V4h4v8a6 6 0 0 1-12 0z"],
  logout: ["M15 4h4v16h-4", "M10 8l-4 4 4 4", "M6 12h10"],
});

/** Filled shapes (the rest are strokes). */
const FILLED = new Set(["star", "magnet"]);

/**
 * @param {string} name
 * @param {{filled?: boolean, cls?: string}} [opts]
 * @returns {SVGElement}
 */
export function icon(name, opts = {}) {
  const paths = PATHS[name] ?? PATHS.close;
  const filled = opts.filled ?? FILLED.has(name);
  return svg(
    "svg",
    {
      viewBox: "0 0 24 24",
      width: "24",
      height: "24",
      class: ["icon", opts.cls].filter(Boolean).join(" "),
      "aria-hidden": "true",
      focusable: "false",
      fill: filled ? "currentColor" : "none",
      stroke: filled ? "none" : "currentColor",
      "stroke-width": "2",
      "stroke-linecap": "round",
      "stroke-linejoin": "round",
    },
    ...paths.map((d) => svg("path", { d })),
  );
}

/** The app mark: the launcher icon's magnet and star (android ic_launcher_foreground.xml). */
export function logo() {
  return svg(
    "svg",
    { viewBox: "14 14 80 80", width: "28", height: "28", class: "logo", "aria-hidden": "true", focusable: "false" },
    svg("rect", { x: "14", y: "14", width: "80", height: "80", rx: "18", fill: "#1B1F3B" }),
    svg("path", { fill: "#E5484D", d: "M34 40L46 40L46 62A8 8 0 0 0 62 62L62 40L74 40L74 62A20 20 0 0 1 34 62Z" }),
    svg("path", { fill: "#E8EAF2", d: "M34 40L46 40L46 48L34 48Z M62 40L74 40L74 48L62 48Z" }),
    svg("path", { fill: "#FFC53D", d: "M54 20L57 30L67 33L57 36L54 46L51 36L41 33L51 30Z" }),
  );
}
