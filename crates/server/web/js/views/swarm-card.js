// Swarm (DetailScreen.kt SwarmCard, D45): the latest seed check, "Check now", the seeder
// history chart and the swarm graph link.

import { h, link, routeLink } from "../dom.js";
import { formatAgo } from "../format.js";
import { progressBar } from "../ui/banner.js";
import { seederChart } from "./seeder-chart.js";

/**
 * @param {import("../types-stats.js").Liveness | null | undefined} l
 * @returns {"seeded" | "dead" | "unknown"}
 */
export function swarmStatus(l) {
  if (!l) return "unknown";
  const best = l.seeders == null && l.dht_peers_est == null ? null : Math.max(l.seeders ?? 0, l.dht_peers_est ?? 0);
  if (best == null) return "unknown";
  return best === 0 ? "dead" : "seeded";
}

/**
 * @param {{
 *   liveness: import("../types-stats.js").Liveness | null | undefined,
 *   bestPeers: number | null | undefined,
 *   checking: boolean,
 *   history: import("../types-stats.js").LivenessHistory | null,
 *   showHistory: boolean,
 *   graphs: boolean,
 *   infohash: string,
 *   onCheck: () => void,
 * }} o
 */
export function swarmCard(o) {
  const l = o.liveness;
  /** @type {Node[]} */
  const body = [];
  if (o.checking) body.push(progressBar());
  if (!l) {
    body.push(h("p", { class: "swarm-line", text: o.bestPeers != null ? `≈${o.bestPeers} peers (last check)` : "Not checked yet" }));
  } else {
    const status = swarmStatus(l);
    body.push(h(
      "p",
      { class: "swarm-line" },
      h("span", { class: ["status-word", `status-${status}`], text: `Swarm ${status}: ` }),
      `${l.seeders ?? "?"} seeders · ${l.leechers ?? "?"} leechers` + (l.completed != null ? ` · ${l.completed} completed` : ""),
    ));
    if (l.dht_peers_est != null) body.push(h("p", { text: `≈${l.dht_peers_est} DHT peers` }));
    body.push(h("p", {
      class: "muted small",
      text: `checked ${formatAgo(l.checked_at ?? 0)} · via ${(l.source ?? "none").replaceAll("_", " ")} · trackers ${l.trackers_ok ?? 0} ok / ${
        l.trackers_failed ?? 0
      } failed`,
    }));
  }
  if (o.showHistory && o.history) {
    const days = (o.history.keep_days ?? 0) > 0 ? o.history.keep_days : 90;
    body.push(h("h4", { class: "chart-title", text: `Seeders, last ${days} days` }));
    body.push(seederChart(o.history.points ?? []));
  }
  /** @type {Node | null} */
  let graph = null;
  if (o.graphs) {
    try {
      graph = routeLink(link("graph", { path: `swarm/${o.infohash}` }), { class: "btn text" }, "Swarm graph");
    } catch {
      graph = null;
    }
  }
  return h(
    "section",
    { class: "card swarm-card", "aria-label": "Swarm" },
    h(
      "div",
      { class: "card-head" },
      h("h3", { class: "card-title", text: "Swarm" }),
      h("button", {
        type: "button",
        class: "btn tonal",
        disabled: o.checking,
        text: o.checking ? "Checking…" : "Check now",
        "on:click": o.onCheck,
      }),
    ),
    ...body,
    graph,
  );
}
