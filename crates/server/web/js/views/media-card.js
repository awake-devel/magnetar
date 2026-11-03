// The identified film, series or anime of a torrent (MediaCard.kt, D40): 2:3 poster or a
// placeholder, facts, genres, episode, overview, IMDb and "All releases".

import { externalUrl, h, link, posterImg, routeLink, untrusted } from "../dom.js";
import { headlineSuffix, mediaFacts, seasonEpisodeText } from "../labels.js";

/**
 * @param {import("../types.js").MediaInfo} m
 * @param {{mediaReleases: boolean}} opts
 */
export function mediaCard(m, opts) {
  const overview = m.overview != null && m.overview.trim() !== ""
    ? h("p", { class: "overview clamp" }, untrusted(m.overview))
    : null;
  if (overview) {
    overview.tabIndex = 0;
    overview.setAttribute("aria-label", "Overview: activate to show it all");
    const toggle = () => overview.classList.toggle("clamp");
    overview.addEventListener("click", toggle);
    overview.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        toggle();
      }
    });
  }
  const imdb = externalUrl("imdb", m.imdb ?? undefined);
  /** @type {Node | null} */
  let releases = null;
  if (opts.mediaReleases && Number.isSafeInteger(m.media_id)) {
    try {
      releases = routeLink(link("title", { id: m.media_id }), { class: "btn text" }, "All releases");
    } catch {
      releases = null;
    }
  }
  const se = seasonEpisodeText(m);
  return h(
    "section",
    { class: "card media-card", "aria-label": "Title" },
    h(
      "div",
      { class: "media-header" },
      posterImg(m),
      h(
        "div",
        { class: "media-facts" },
        h("h3", { class: "media-title" }, untrusted(m.title ?? ""), headlineSuffix(m.year, m.end_year)),
        m.original_title != null && m.original_title !== m.title ? h("p", { class: "muted" }, untrusted(m.original_title)) : null,
        h("p", { text: mediaFacts(m) }),
        (m.genres ?? []).length > 0 ? h("p", { class: "muted" }, untrusted((m.genres ?? []).join(", "))) : null,
        se != null ? h("p", null, untrusted(se)) : null,
      ),
    ),
    overview,
    h(
      "div",
      { class: "row-actions" },
      imdb ? h("a", { href: imdb, class: "btn tonal" }, "IMDb") : null,
      releases,
    ),
  );
}
