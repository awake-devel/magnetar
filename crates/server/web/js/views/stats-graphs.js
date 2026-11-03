// The Graphs tab (StatsScreen.kt Graphs): web of trust and relays; title and swarm graphs open
// from their pages.

import { h, link, routeLink } from "../dom.js";
import { icon } from "../icons.js";
import { banner, progressBar } from "../ui/banner.js";

const CARDS = Object.freeze([
  ["wot", "Web of trust", "You, the people you follow, and the publishers they vouch for, with each publisher's trust score and why."],
  ["relays", "Relays and publishers", "Which relays the most active publishers write to, sized by their torrents."],
]);

/**
 * @param {{hasGraphs: boolean, healthAsked: boolean, healthOk: boolean}} o
 */
export function graphsTab(o) {
  if (!o.hasGraphs) {
    if (!o.healthAsked) return h("div", null, progressBar());
    return banner(o.healthOk ? "This server is older than the app and has no graphs." : "The server did not answer.");
  }
  return h(
    "div",
    { class: "graphs-tab" },
    ...CARDS.map(([path, title, text]) =>
      routeLink(
        link("graph", { path }),
        { class: "card graph-card" },
        h("span", { class: "graph-icon", "aria-hidden": "true" }, icon("graph")),
        h("span", null, h("span", { class: "card-title block", text: title }), h("span", { class: "muted small block", text })),
      )
    ),
    h("p", {
      class: "muted small",
      text: "A title's releases (by quality, language and group) open from its title page, a torrent's swarm sources from its detail page.",
    }),
  );
}
