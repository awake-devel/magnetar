// Trust (DetailScreen.kt TrustCard): the publisher's score with its level in words, the factors
// on demand, the publisher, the other publishers and proof of work.

import { h, untrusted } from "../dom.js";
import { formatAge, formatAgo, shortKey, trustLevel } from "../format.js";
import { npub } from "../nip19.js";
import { trustBadge } from "./result-row.js";

/** @param {import("../format.js").TrustLevel} level */
function levelWord(level) {
  return level === "ok" ? "trusted" : level === "negative" ? "blocked or distrusted" : "below the threshold";
}

/**
 * @param {{
 *   detail: import("../types-stats.js").TorrentDetail,
 *   threshold: number,
 *   open: boolean,
 *   onToggle: () => void,
 *   onCopy: (label: string, text: string) => void,
 * }} o
 */
export function trustCard(o) {
  const t = o.detail.trust ?? {};
  const s = o.detail.summary;
  const factors = t.factors ?? [];
  const score = t.score ?? 0;
  const level = trustLevel(score, o.threshold, factors.map((f) => f.code ?? ""), factors.some((f) => (f.delta ?? 0) < 0));
  const factorsId = `trust-factors-${s.infohash}`;
  /** @type {Node[]} */
  const inner = [];
  if (o.open) {
    if (t.root != null) inner.push(h("p", { class: "small" }, "Relative to root ", untrusted(shortKey(npub(t.root)))));
    if (factors.length === 0) inner.push(h("p", { class: "small", text: "No factors recorded." }));
    for (const f of factors) {
      const delta = f.delta ?? 0;
      inner.push(h(
        "div",
        { class: "factor" },
        h("span", { class: ["factor-delta", delta >= 0 ? "plus" : "minus"], text: (delta >= 0 ? "+" : "") + delta }),
        h(
          "div",
          { class: "factor-body" },
          h("p", null, untrusted((f.detail ?? "").trim() !== "" ? f.detail : f.code)),
          h("p", { class: "muted small" }, untrusted(`${f.code ?? ""}: ${(delta >= 0 ? "+" : "") + delta}`)),
          ...(f.evidence ?? []).slice(0, 5).map((e) => h("p", { class: "muted small" }, "• ", untrusted(shortKey(npub(e))))),
        ),
      ));
    }
    if ((t.computed_at ?? 0) > 0) inner.push(h("p", { class: "muted small", text: `Computed ${formatAgo(t.computed_at ?? 0)}` }));
  }
  const pub = s.npub ?? (s.pubkey ? npub(s.pubkey) : "");
  const others = (o.detail.events ?? []).filter((e) => e.event_id !== s.event_id);
  return h(
    "section",
    { class: "card trust-card", "aria-label": "Trust" },
    h(
      "div",
      { class: "card-head" },
      h("h3", { class: "card-title", text: "Trust" }),
      h("span", { class: "trust-score" }, trustBadge(score, o.threshold, level), " ", h("span", { class: "muted small", text: levelWord(level) })),
    ),
    h("button", {
      type: "button",
      class: "btn text",
      "aria-expanded": String(o.open),
      "aria-controls": factorsId,
      text: o.open ? "Hide trust factors" : "Show trust factors",
      "on:click": o.onToggle,
    }),
    h("div", { id: factorsId, hidden: !o.open }, ...inner),
    pub !== ""
      ? h(
        "div",
        { class: "kv-block" },
        h("h4", { class: "section-title", text: "Publisher" }),
        h(
          "div",
          { class: "copy-row" },
          h("code", { class: "mono" }, untrusted(pub)),
          h("button", { type: "button", class: "btn text", text: "Copy", "aria-label": "Copy the publisher's npub", "on:click": () => o.onCopy("npub", pub) }),
        ),
      )
      : null,
    (s.publisher_count ?? 0) > 1 ? h("p", { text: `${s.publisher_count} publishers announced this torrent` }) : null,
    (s.pow_bits ?? 0) > 0 ? h("p", { class: "muted small", text: `Proof of work: ${s.pow_bits} bits` }) : null,
    others.length > 0
      ? h(
        "div",
        { class: "kv-block" },
        h("h4", { class: "section-title", text: "Also published by" }),
        h(
          "ul",
          { class: "plain-list" },
          ...others.map((e) =>
            h(
              "li",
              { class: "event-row" },
              h(
                "div",
                { class: "event-body" },
                h("p", null, untrusted(e.title ?? "")),
                h("p", { class: "muted small" }, untrusted(shortKey(e.npub ?? "")), ` · ${formatAge(e.created_at ?? 0)}`),
              ),
              trustBadge(e.trust ?? 0, o.threshold),
            )
          ),
        ),
      )
      : null,
  );
}
