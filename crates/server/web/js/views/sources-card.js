// "Where discoveries come from" (SourcesCard.kt, feature `crawl_sources`): DHT routes with their
// conversion and the last 48 hours, Nostr relays by discovery source and the top relays, one
// block per machine, and the Crawl map button (`crawl_map`). Relay URLs and machine names are
// untrusted text, IP literals already redacted (sources-model.js).

import { h, link, routeLink, svg, untrusted } from "../dom.js";
import { formatDate } from "../format.js";
import { axisLabel, bucketLabel, formatCompact, formatNumber } from "../numfmt.js";
import {
  formatConversion,
  hourRoutes,
  machineLines,
  machineTitle,
  orderedRoutes,
  relayPlace,
  relaySourceLabel,
  ROUTE_ORDER,
  routeLabel,
  shortRelayUrl,
} from "../sources-model.js";
import { progressBar } from "../ui/banner.js";
import { kvLines, valuesList } from "./stats-charts.js";

/** @typedef {import("../chart-colors.js").ChartColors} ChartColors */

/** @param {string} color */
function dot(color) {
  const d = h("span", { class: "dot", "aria-hidden": "true" });
  d.style.setProperty("background", color);
  return d;
}

/**
 * @param {number} frac
 * @param {string} color
 * @param {string} text
 */
function valueBar(frac, color, text) {
  const fill = h("span", { class: "bar-fill" });
  fill.style.setProperty("--w", String(Math.min(1, Math.max(0.01, frac))));
  fill.style.setProperty("background", color);
  return h("div", { class: "value-bar" }, h("span", { class: "bar-track" }, fill), h("span", { class: "bar-num", text }));
}

/**
 * @param {NonNullable<import("../types-stats.js").CrawlSources["dht"]>} d
 * @param {ChartColors} colors
 * @param {{stored: boolean, picked: number | null}} state
 * @param {() => void} rerender
 */
function dhtPart(d, colors, state, rerender) {
  const routes = orderedRoutes(d.routes ?? []);
  const max = Math.max(1, ...routes.map((r) => r.seen ?? 0));
  /** @type {Node[]} */
  const out = [h("h4", { class: "section-title", text: "DHT: hashes seen and torrents stored, per route" })];
  if (routes.length === 0) out.push(h("p", { class: "muted small", text: "Nothing seen yet" }));
  for (const r of routes) {
    const conv = formatConversion(r.stored ?? 0, r.seen ?? 0);
    out.push(h(
      "div",
      {
        class: "route",
        role: "group",
        "aria-label": `${routeLabel(r.via)}: ${formatNumber(r.seen ?? 0)} seen, ${formatNumber(r.stored ?? 0)} stored, ${conv} stored`,
      },
      h("div", { class: "route-head" }, h("span", null, untrusted(routeLabel(r.via))), h("strong", { text: `${conv} stored` })),
      valueBar((r.seen ?? 0) / max, colors.bar, `seen ${formatCompact(r.seen ?? 0)}`),
      valueBar((r.stored ?? 0) / max, colors.barSelected, `stored ${formatCompact(r.stored ?? 0)}`),
    ));
  }
  const hours = d.hourly ?? [];
  if (hours.length > 0) {
    const names = hourRoutes(hours);
    /** @param {number} i */
    const values = (i) => names.map((n) => (state.stored ? hours[i].stored ?? {} : hours[i].seen ?? {})[n] ?? 0);
    const top = Math.max(1, ...hours.map((_, i) => values(i).reduce((a, b) => a + b, 0)));
    /** @param {string} name */
    const colorOf = (name) => {
      const i = ROUTE_ORDER.indexOf(name);
      return i >= 0 ? colors.categorical[i] : colors.other;
    };
    const W = 480;
    const H = 96;
    const cw = W / hours.length;
    const gap = cw > 4 ? cw * 0.2 : 0;
    const p = state.picked != null && state.picked < hours.length ? state.picked : null;
    /** @type {SVGElement[]} */
    const cols = [];
    hours.forEach((_, i) => {
      let y = H;
      /** @type {SVGElement[]} */
      const parts = [];
      names.forEach((n, k) => {
        const v = values(i)[k];
        if (v <= 0) return;
        const hh = Math.max(1, (v / top) * (H - 4));
        parts.push(svg("rect", {
          x: Math.round((i * cw + gap / 2) * 100) / 100,
          y: Math.round((y - hh) * 100) / 100,
          width: Math.max(0.5, Math.round((cw - gap) * 100) / 100),
          height: Math.round(hh * 100) / 100,
          fill: colorOf(n),
        }));
        y -= hh;
      });
      // A full-height hit area per hour.
      const hit = svg("rect", { x: Math.round(i * cw * 100) / 100, y: 0, width: Math.round(cw * 100) / 100, height: H, fill: "transparent", class: "hit" });
      hit.addEventListener("click", () => {
        state.picked = state.picked === i ? null : i;
        rerender();
      });
      cols.push(svg("g", null, ...parts, i === p ? svg("rect", { x: Math.round(i * cw * 100) / 100, y: 0, width: Math.max(1, Math.round(cw * 100) / 100), height: H, fill: "none", stroke: colors.ink, "stroke-width": 1, "vector-effect": "non-scaling-stroke" }) : null, hit));
    });
    const kind = state.stored ? "Stored" : "Seen";
    out.push(h(
      "div",
      { class: "route-hours-head" },
      h("span", { class: "small strong", text: `Last ${hours.length} hours` }),
      h(
        "span",
        { class: "chips", role: "group", "aria-label": "Count" },
        ...[["Seen", false], ["Stored", true]].map(([label, v]) =>
          h("button", {
            type: "button",
            class: ["chip", state.stored === v && "on"],
            "aria-pressed": String(state.stored === v),
            text: /** @type {string} */ (label),
            dataset: { fkey: `hours-${label}` },
            "on:click": () => {
              state.stored = /** @type {boolean} */ (v);
              rerender();
            },
          })
        ),
      ),
    ));
    out.push(h("p", {
      class: ["small", p != null && "strong"],
      role: "status",
      text: p != null
        ? bucketLabel(hours[p].bucket, true) + ": " + names.map((n, k) => `${routeLabel(n)} ${formatNumber(values(p)[k])}`).join(" · ")
        : `highest hour ${formatNumber(top)} · click a column`,
    }));
    out.push(svg(
      "svg",
      {
        viewBox: `0 0 ${W} ${H}`,
        preserveAspectRatio: "none",
        class: "chart route-hours",
        role: "img",
        "aria-label": `${kind} per hour by route, ${hours.length} hours, highest ${formatNumber(top)}`,
      },
      svg("line", { x1: 0, y1: H - 0.5, x2: W, y2: H - 0.5, stroke: colors.axis, "vector-effect": "non-scaling-stroke" }),
      ...cols,
    ));
    out.push(h("div", { class: "chart-axis" }, h("span", { text: axisLabel(hours[0].bucket, true) }), h("span", { text: axisLabel(hours[hours.length - 1].bucket, true) })));
    out.push(h("p", { class: "legend" }, ...names.map((n) => h("span", { class: "legend-item" }, dot(colorOf(n)), untrusted(routeLabel(n))))));
    out.push(valuesList(
      hours.map((hr, i) => [bucketLabel(hr.bucket, true), names.map((n, k) => `${routeLabel(n)} ${formatNumber(values(i)[k])}`).join(" · ")]),
      `${kind} per hour`,
    ));
  }
  return out;
}

/**
 * @param {NonNullable<import("../types-stats.js").CrawlSources["nostr"]>} n
 * @param {ChartColors} colors
 */
function nostrPart(n, colors) {
  const rows = [...(n.by_source ?? [])].sort((a, b) => (b.relays ?? 0) - (a.relays ?? 0));
  const max = Math.max(1, ...rows.map((r) => r.relays ?? 0));
  /** @type {Node[]} */
  const out = [h("h4", { class: "section-title", text: "Nostr: relays by how they were found" })];
  if (rows.length === 0) out.push(h("p", { class: "muted small", text: "No relays yet" }));
  for (const r of rows) {
    out.push(h(
      "div",
      { class: "route" },
      h("p", { class: "small" }, untrusted(relaySourceLabel(r.source ?? ""))),
      valueBar((r.relays ?? 0) / max, colors.bar, `${formatCompact(r.relays ?? 0)} relays`),
      h("p", {
        class: "muted small",
        text: `${formatNumber(r.enabled ?? 0)} enabled · ${formatNumber(r.delivered ?? 0)} delivered · ${formatCompact(r.events ?? 0)} events · ${
          formatCompact(r.torrents ?? 0)
        } torrents`,
      }),
    ));
  }
  const top = n.top_relays ?? [];
  if (top.length > 0) {
    out.push(h("h4", { class: "section-title", text: "Top relays" }));
    if ((n.torrents_since ?? 0) > 0) out.push(h("p", { class: "muted small", text: `Torrents counted since ${formatDate(n.torrents_since ?? 0)}` }));
    out.push(h(
      "ul",
      { class: "plain-list relays" },
      ...top.map((r) => {
        const state = !r.enabled ? "disabled" : r.ok_24h ? "answered in 24 h" : "not answering";
        const place = relayPlace(r);
        return h(
          "li",
          { class: "relay-row" },
          h(
            "div",
            { class: "relay-head" },
            h("span", { class: "relay-url" }, untrusted(shortRelayUrl(r.url ?? ""))),
            h("span", { class: "small", text: `${formatCompact(r.events ?? 0)} ev · ${formatCompact(r.torrents ?? 0)} tor` }),
          ),
          h("p", { class: "muted small" }, untrusted([relaySourceLabel(r.source ?? ""), state, place].filter((x) => x != null).join(" · "))),
        );
      }),
    ));
    out.push(h("p", { class: "muted small", text: "ev = events, tor = torrents" }));
  }
  return out;
}

/**
 * @param {readonly import("../types-stats.js").CrawlMachine[]} machines
 * @param {ChartColors} colors
 */
function machinesPart(machines, colors) {
  const sorted = [...machines].sort((a, b) => (a.kind === "server" ? 0 : 1) - (b.kind === "server" ? 0 : 1));
  return [
    h("h4", { class: "section-title", text: "Machines" }),
    ...sorted.map((m) =>
      h(
        "div",
        { class: "machine" },
        h(
          "div",
          { class: "machine-head" },
          h("strong", null, untrusted(machineTitle(m))),
          h("span", { class: "small" }, dot(m.connected ? colors.good : colors.unknown), m.connected ? "connected" : "not connected"),
        ),
        kvLines(machineLines(m)),
      )
    ),
  ];
}

/**
 * @param {{
 *   sources: import("../types-stats.js").CrawlSources | null,
 *   error: string | null,
 *   cachedNote: string | null,
 *   colors: ChartColors,
 *   crawlMap: boolean,
 *   ui: {stored: boolean, picked: number | null},
 *   rerender: () => void,
 * }} o
 */
export function sourcesCard(o) {
  const s = o.sources;
  const note = [o.error != null ? (s != null ? "Offline: " : "") + o.error : null, o.cachedNote].filter((x) => x != null).join(" · ");
  /** @type {Node[]} */
  const body = [];
  if (s) {
    if (s.dht) body.push(...dhtPart(s.dht, o.colors, o.ui, o.rerender));
    if (s.nostr) body.push(...nostrPart(s.nostr, o.colors));
    if ((s.machines ?? []).length > 0) body.push(...machinesPart(s.machines ?? [], o.colors));
  } else if (o.error == null) {
    body.push(progressBar());
  }
  return h(
    "section",
    { class: "card sources-card", "aria-label": "Where discoveries come from" },
    h("h3", { class: "card-title", text: "Where discoveries come from" }),
    note !== "" ? h("p", { class: ["small", o.error != null ? "error-text" : "muted"], text: note }) : null,
    ...body,
    o.crawlMap ? routeLink(link("crawl-map"), { class: "btn tonal" }, "Crawl map") : null,
  );
}
