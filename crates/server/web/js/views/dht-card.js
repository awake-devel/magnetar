// "Found on the DHT" (DetailScreen.kt DhtCard, D51, D74): what the server's crawler verified.

import { h, untrusted } from "../dom.js";
import { formatTimestamp, shortKey } from "../format.js";
import { dhtChip } from "./result-row.js";

/**
 * @param {import("../types-stats.js").DhtInfo} d
 * @param {string} shownTitle
 */
export function dhtCard(d, shownTitle) {
  const title = d.title ?? "";
  return h(
    "section",
    { class: "card dht-card", "aria-label": "Found on the DHT" },
    h("div", { class: "card-head" }, h("h3", { class: "card-title", text: "Found on the DHT" }), dhtChip()),
    h("p", {
      text: "Name and files were downloaded from the swarm and verified against the infohash; no publisher stands behind them.",
    }),
    title !== "" && title !== shownTitle ? h("p", null, "Torrent name: ", untrusted(title)) : null,
    h("p", {
      class: "muted small",
      text: `First seen ${formatTimestamp(d.first_seen ?? 0)} · ${d.peers_seen ?? 0} peers then · via ${
        (d.discovered_via ?? "").replaceAll("_", " ")
      }`,
    }),
    d.infohash_v2 != null ? h("p", { class: "muted small mono" }, "BitTorrent v2 hash: ", untrusted(shortKey(d.infohash_v2))) : null,
  );
}
