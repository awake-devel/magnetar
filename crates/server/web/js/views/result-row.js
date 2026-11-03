// One result (docs/APP.md §1, ResultRow.kt): media line (title page link), title, size · peers ·
// dead · age, attribute badges, "matched in file", and the source badge. Every API string goes
// through untrusted().

import { h, link, routeLink, untrusted } from "../dom.js";
import { fileName, trustLevel } from "../format.js";
import { attrBadges, categoryLabel, mediaLineParts, statsLine } from "../labels.js";

/**
 * The trust star: "★ 42", tinted by level, with the level in words for screen readers.
 * @param {number} score
 * @param {number} threshold
 * @param {import("../format.js").TrustLevel} [level]
 */
export function trustBadge(score, threshold, level = trustLevel(score, threshold)) {
  const word = level === "ok" ? "trusted" : level === "negative" ? "blocked or distrusted" : "below the threshold";
  return h("span", {
    class: ["badge-trust", `trust-${level}`],
    title: `Trust ${score} (${word})`,
    "aria-label": `Trust ${score}, ${word}`,
    text: `★ ${score}`,
  });
}

/** The neutral "DHT" label (not a score). */
export function dhtChip(small = false) {
  return h("span", { class: ["badge-dht", small && "small"], title: "Found on the DHT", "aria-label": "Found on the DHT", text: "DHT" });
}

/**
 * Result badge by source (D51, D74).
 * @param {string | undefined} origin
 * @param {number} score
 * @param {number} threshold
 */
export function sourceBadge(origin, score, threshold) {
  if (origin === "dht") return h("span", { class: "source-badge" }, dhtChip());
  if (origin === "both") return h("span", { class: "source-badge both" }, trustBadge(score, threshold), dhtChip(true));
  return h("span", { class: "source-badge" }, trustBadge(score, threshold));
}

/**
 * @param {import("../types.js").TorrentSummary} hit
 * @param {{threshold: number, showMedia?: boolean, mediaLinks?: boolean, now?: number, selected?: boolean}} opts
 */
export function resultRow(hit, opts) {
  const line = opts.showMedia === false ? null : mediaLineParts(hit);
  /** @type {Node | null} */
  let media = null;
  if (line != null) {
    const text = [untrusted(line.title), line.rest];
    let url = null;
    if (opts.mediaLinks && hit.media_id != null) {
      try {
        url = link("title", { id: hit.media_id });
      } catch {
        url = null;
      }
    }
    media = url
      ? routeLink(url, { class: "media-line link" }, ...text, h("span", { "aria-hidden": "true", text: " ›" }))
      : h("span", { class: "media-line" }, ...text);
  }
  const badges = hit.attrs ? attrBadges(hit.attrs) : [];
  const body = [
    h("span", { class: "result-title" }, untrusted(hit.title ?? "")),
    h("span", { class: "result-meta", text: statsLine(hit, opts.now) }),
    badges.length > 0 ? h("span", { class: "result-badges", text: badges.join(" · ") }) : null,
    hit.matched_file
      ? h("span", { class: "result-matched" }, "matched in file: ", untrusted(fileName(hit.matched_file)))
      : null,
  ];
  let main;
  try {
    main = routeLink(link("torrent", { ih: hit.infohash }), { class: "result-main" }, ...body);
  } catch {
    main = h("div", { class: "result-main" }, ...body);
  }
  return h(
    "li",
    { class: ["result", opts.selected && "selected"], dataset: { ih: hit.infohash } },
    h("span", { class: "cat-badge", text: categoryLabel(hit.category ?? "other") }),
    h("div", { class: "result-body" }, media, main),
    sourceBadge(hit.origin, hit.trust ?? 0, opts.threshold),
  );
}
