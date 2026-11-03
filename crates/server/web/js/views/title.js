// The title page (TitleScreen.kt, D44): every release of one movie or series, grouped by
// season/episode, quality and language, with the Search tab's filters (no text, sort seeders).
// In-page chips narrow the server's groups without a request.

import { ApiError, isAborted, isOffline, userMessage } from "../api.js";
import { h, link, posterImg, routeLink, untrusted } from "../dom.js";
import { cachedAgo } from "../format.js";
import { facetLabel, headlineSuffix, mediaFacts } from "../labels.js";
import * as prefs from "../prefs.js";
import { activeCount, buildQuery, cacheQuery, EMPTY_FILTERS, supportedBy } from "../search-model.js";
import { banner, centered, progressBar } from "../ui/banner.js";
import { chip, chipRow } from "../ui/chips.js";
import { pageHeader } from "../ui/shell.js";
import { resultRow } from "./result-row.js";
import { searchController } from "./search.js";

/** @typedef {import("../types.js").ReleaseGroup} ReleaseGroup */

/**
 * "Season 5 · Episode 9", "Season 5 (pack)", "Complete series / packs", "Episode 3", or null (films).
 * @param {ReleaseGroup} g
 * @param {boolean} series
 * @returns {string | null}
 */
export function episodeHeading(g, series) {
  if (!series) return null;
  if (g.season != null && g.episode != null) return `Season ${g.season} · Episode ${g.episode}`;
  if (g.season != null) return `Season ${g.season} (pack)`;
  if (g.episode != null) return `Episode ${g.episode}`;
  return "Complete series / packs";
}

/**
 * "1080p · MULTi (VFF) · 3 releases".
 * @param {ReleaseGroup} g
 */
export function groupHeading(g) {
  const n = (g.releases ?? []).length;
  return [
    g.resolution != null ? facetLabel("resolution", g.resolution) : "Unknown quality",
    g.language_label && g.language_label.trim() !== "" ? g.language_label : "Unknown language",
    n === 1 ? "1 release" : `${n} releases`,
  ].join(" · ");
}

/**
 * @param {HTMLElement} el
 * @param {import("../types.js").ViewArgs} args
 * @returns {import("../types.js").ViewHandle}
 */
export function mount(el, { params, ctx }) {
  let id = Number(params.id);
  let filters = searchController(ctx).get().state.filters;
  /** @type {import("../types.js").MediaReleases | null} */
  let data = null;
  /** @type {import("../api.js").ApiError | null} */
  let error = null;
  let loading = true;
  /** @type {number | null} */
  let cachedAt = null;
  /** @type {{season: number | null, resolution: string | null, language: string | null}} */
  let pick = { season: null, resolution: null, language: null };
  /** @type {AbortController | null} */
  let call = null;

  const graphBtn = h("span");
  const header = pageHeader(ctx, { title: "Releases", back: true, actions: [graphBtn] });
  const body = h("div", { class: "title-page page-pad" });
  el.append(header, body);

  function groups() {
    return data?.groups ?? [];
  }

  function render() {
    const media = data?.media ?? null;
    const titleEl = header.querySelector(".page-title");
    if (titleEl) titleEl.replaceChildren(media?.title ? untrusted(media.title) : "Releases");
    graphBtn.replaceChildren(
      ctx.health.has("graphs")
        ? routeLink(link("graph", { path: `media/${id}` }), { class: "btn text", title: "Releases graph" }, "Releases graph")
        : "",
    );
    /** @type {Node[]} */
    const out = [];
    if (loading) out.push(progressBar());
    if (!ctx.health.has("media_releases") && ctx.health.get().ok === true) {
      out.push(centered("This server is older than the app and has no title pages (it lacks media_releases)."));
      body.replaceChildren(...out);
      return;
    }
    if (data && error) {
      out.push(banner((isOffline(error) ? "Offline: " : "") + userMessage(error) + (isOffline(error) ? ". Showing the cached page." : ""), { kind: "error" }));
    } else if (data && cachedAt != null) {
      out.push(banner(cachedAgo(cachedAt)));
    }
    if (!data) {
      if (error) out.push(centered(userMessage(error), { label: "Retry", onClick: () => void load() }));
      body.replaceChildren(...out);
      return;
    }
    if (media) {
      out.push(h(
        "div",
        { class: "media-header" },
        posterImg(media, { cls: "poster-sm" }),
        h(
          "div",
          { class: "media-facts" },
          h("h2", { class: "media-title" }, untrusted(media.title ?? ""), headlineSuffix(media.year, media.end_year)),
          h("p", { text: mediaFacts(media) }),
          (media.genres ?? []).length > 0 ? h("p", { class: "muted" }, untrusted((media.genres ?? []).join(", "))) : null,
          media.overview ? h("p", { class: "overview clamp" }, untrusted(media.overview)) : null,
        ),
      ));
    }
    const n = activeCount(filters);
    if (n > 0) {
      out.push(h(
        "div",
        { class: "filter-row" },
        h("span", { class: "muted", text: n === 1 ? "1 search filter applies" : `${n} search filters apply` }),
        h("button", {
          type: "button",
          class: "btn text",
          text: "Show all",
          "on:click": () => {
            filters = EMPTY_FILTERS;
            data = null;
            pick = { season: null, resolution: null, language: null };
            void load();
          },
        }),
      ));
    }
    const gs = groups();
    const seasons = [...new Set(gs.map((g) => g.season).filter((s) => s != null))].sort((a, b) => Number(a) - Number(b));
    const resolutions = [...new Set(gs.map((g) => g.resolution).filter((r) => r != null))];
    /** @type {Map<string, string>} */
    const languages = new Map();
    for (const g of gs) if (!languages.has(g.language ?? "")) languages.set(g.language ?? "", g.language_label ?? "");
    if (seasons.length > 1) {
      out.push(chipRow(seasons.map((s) =>
        chip({
          label: `Season ${s}`,
          selected: pick.season === s,
          onToggle: () => {
            pick = { ...pick, season: pick.season === s ? null : /** @type {number} */ (s) };
            render();
          },
        })
      ), "Season"));
    }
    if (resolutions.length > 1) {
      out.push(chipRow(resolutions.map((r) =>
        chip({
          label: facetLabel("resolution", /** @type {string} */ (r)),
          selected: pick.resolution === r,
          onToggle: () => {
            pick = { ...pick, resolution: pick.resolution === r ? null : /** @type {string} */ (r) };
            render();
          },
        })
      ), "Resolution"));
    }
    if (languages.size > 1) {
      out.push(chipRow([...languages].map(([key, label]) =>
        chip({
          label: label.trim() !== "" ? label : key === "" ? "Unknown language" : key,
          selected: pick.language === key,
          onToggle: () => {
            pick = { ...pick, language: pick.language === key ? null : key };
            render();
          },
        })
      ), "Language"));
    }
    const total = data.total ?? 0;
    out.push(h("p", {
      class: "result-count",
      text: (total === 1 ? "1 release" : `${total} releases`) + (data.truncated ? " (best seeded shown)" : ""),
    }));
    if (gs.length === 0) out.push(centered("No releases of this title match."));
    const series = gs.some((g) => g.season != null || g.episode != null);
    const shown = gs.filter((g) =>
      (pick.season == null || g.season === pick.season) &&
      (pick.resolution == null || g.resolution === pick.resolution) &&
      (pick.language == null || (g.language ?? "") === pick.language)
    );
    const threshold = ctx.store.get().trust?.threshold ?? 20;
    const now = Math.floor(Date.now() / 1000);
    /** @type {string | null} */
    let lastEp = null;
    for (const g of shown) {
      const ep = episodeHeading(g, series);
      if (ep != null && ep !== lastEp) {
        out.push(h("h2", { class: "episode-heading", text: ep }));
        lastEp = ep;
      }
      out.push(h("h3", { class: "group-heading", text: groupHeading(g) }));
      out.push(h(
        "ul",
        { class: "result-list" },
        ...(g.releases ?? []).map((t) => resultRow(t, { threshold, showMedia: false, now })),
      ));
    }
    body.replaceChildren(...out);
  }

  async function load() {
    call?.abort();
    const ac = new AbortController();
    call = ac;
    loading = true;
    error = null;
    render();
    const adult = prefs.showAdult();
    const key = cacheQuery("", filters, "seeders", true, adult);
    const endpoint = `title/${id}`;
    if (data == null) {
      const c = await ctx.cache.get(endpoint, key);
      if (ac.signal.aborted) return;
      if (c?.json && data == null) {
        data = c.json;
        cachedAt = c.savedAt;
        render();
      }
    }
    await ctx.health.refresh();
    if (ac.signal.aborted) return;
    const q = buildQuery("", supportedBy(filters, ctx.health.features()), "seeders", true, adult);
    try {
      const r = await ctx.api.post(`/api/v1/media/${id}/releases`, q, { cls: "SEARCH", signal: ac.signal });
      if (isAborted(r) || ac.signal.aborted) return;
      data = r;
      cachedAt = null;
      void ctx.cache.put(endpoint, key, r);
    } catch (e) {
      if (ac.signal.aborted) return;
      error = e instanceof ApiError ? e : new ApiError("network", String(e));
      // Other errors clear a cached page.
      if (!isOffline(error)) {
        data = null;
        cachedAt = null;
      }
    }
    loading = false;
    render();
  }

  void load();
  return {
    update(p) {
      const next = Number(p.id);
      if (next === id) return;
      id = next;
      data = null;
      pick = { season: null, resolution: null, language: null };
      filters = searchController(ctx).get().state.filters;
      void load();
    },
    unmount() {
      call?.abort();
    },
  };
}
