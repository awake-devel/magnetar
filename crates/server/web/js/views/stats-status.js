// The Status tab (StatsScreen.kt Status): server, search index, seed-check backlog, relays and
// crawler, DHT crawler, header probes.

import { h } from "../dom.js";
import { cachedAgo } from "../format.js";
import { formatNumber } from "../numfmt.js";
import { backlogLines, dhtLines, indexLines, missingFeatures, seedChecks } from "../stats-model.js";
import { banner, progressBar } from "../ui/banner.js";
import { probesCard, showsProbes } from "./probes-card.js";
import { kvLines, statusBar } from "./stats-charts.js";

/**
 * @param {string} title
 * @param {...import("../dom.js").Children} children
 */
export function statusCard(title, ...children) {
  return h("section", { class: "card", "aria-label": title }, h("h3", { class: "card-title", text: title }), ...children);
}

/**
 * @param {import("./stats.js").StatsState} s
 * @param {import("../chart-colors.js").ChartColors} colors
 */
export function seedParts(s, colors) {
  const l = s.stats?.liveness;
  if (!l) return [];
  const c = seedChecks(l);
  return [
    { label: "Seeded", n: c.seeded, color: colors.good },
    { label: "Dead", n: c.dead, color: colors.critical },
    { label: "No answer", n: c.noAnswer, color: colors.unknown },
  ];
}

/**
 * @param {import("./stats.js").StatsState} s
 * @param {import("../chart-colors.js").ChartColors} colors
 */
export function statusTab(s, colors) {
  /** @type {Node[]} */
  const out = [];
  /** @type {Node[]} */
  const server = [];
  if (!s.healthAsked) server.push(progressBar());
  else if (!s.healthOk) server.push(h("p", { class: "error-text", text: "Unreachable: Server unreachable" }));
  else {
    const lines = /** @type {[string, string][]} */ ([
      ["State", s.status === "ok" && s.db ? "healthy" : `${s.status || "?"}, database ${s.db ? "ok" : "down"}`],
      ["Version", s.version.trim() === "" ? "?" : s.version],
    ]);
    const missing = missingFeatures(s.features);
    if (missing.length > 0) lines.push(["Needs a newer server for", missing.join(", ")]);
    server.push(kvLines(lines));
  }
  out.push(statusCard("Server", ...server));
  const h0 = s.stats;
  if (!h0) {
    if (s.statsError != null) out.push(banner(s.statsError, { kind: "error" }));
    else if (s.features.has("stats")) out.push(progressBar());
  } else {
    if (s.statsCachedAt != null) out.push(banner(cachedAgo(s.statsCachedAt) + (s.statsError != null ? ` · ${s.statsError}` : "")));
    if (h0.search_index) out.push(statusCard("Search index", kvLines(indexLines(h0.search_index))));
    if (h0.liveness) out.push(statusCard("Seed checks", statusBar(seedParts(s, colors)), kvLines(backlogLines(h0.liveness))));
    const r = h0.relays ?? {};
    const c = h0.crawler ?? {};
    /** @type {[string, string][]} */
    const relayLines = [
      ["Relays", `${formatNumber(r.enabled ?? 0)} enabled, ${formatNumber(r.ok_24h ?? 0)} answered in 24 h`],
      ["Failing now", formatNumber(r.failing ?? 0)],
      ["Ever delivered a torrent", formatNumber(r.productive ?? 0)],
      ["Crawl passes", formatNumber(c.passes ?? 0)],
    ];
    if (c.last_report) {
      relayLines.push(["Last pass", `${c.last_report.relays_ok ?? 0}/${c.last_report.relays_tried ?? 0} relays, ${formatNumber(c.last_report.inserted ?? 0)} new events`]);
    }
    out.push(statusCard("Relays and crawler", kvLines(relayLines)));
    out.push(statusCard(
      "DHT crawler",
      h0.dht ? kvLines(dhtLines(h0.dht)) : h("p", { class: "muted", text: "Turned off on this server (MAGNETAR_DHT=off): the crawl workers may still find DHT torrents." }),
    ));
  }
  if (showsProbes(s.fleet) && s.fleet) out.push(probesCard(s.fleet, colors, s.fleetCachedAt));
  return h("div", { class: "status-tab" }, ...out);
}
