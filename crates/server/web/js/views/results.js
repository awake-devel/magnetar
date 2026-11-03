// The result list: count line, rows, infinite scroll (an IntersectionObserver near the end),
// the "loading more" row or a Retry row when a next page failed, and the empty and error states.

import { userMessage } from "../api.js";
import { h } from "../dom.js";
import { cachedAgo, resultsText } from "../format.js";
import { activeCount } from "../search-model.js";
import { banner, centered } from "../ui/banner.js";
import { resultRow } from "./result-row.js";

/** Rows ahead of the end at which the next page is asked. */
const AHEAD_ROWS = 10;

/**
 * @param {{ctx: import("../types.js").Ctx, controller: import("./search-controller.js").SearchController,
 *   scrollRoot: () => Element | null, selected: () => string | null}} deps
 */
export function createResults(deps) {
  const { ctx, controller } = deps;
  const offline = h("div", { class: "results-banner" });
  const count = h("p", { class: "result-count", role: "status" });
  const list = h("ul", { class: "result-list", "aria-label": "Results" });
  const footer = h("div", { class: "result-footer" });
  const sentinel = h("div", { class: "sentinel", "aria-hidden": "true" });
  const empty = h("div", { class: "result-empty" });
  const el = h("div", { class: "results" }, offline, count, empty, list, footer, sentinel);
  let shownGeneration = -1;
  let shownCount = 0;
  let rebuild = false;
  /** @type {IntersectionObserver | null} */
  let io = null;

  function observe() {
    io?.disconnect();
    io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) controller.loadMore();
    }, { root: deps.scrollRoot(), rootMargin: `0px 0px ${AHEAD_ROWS * 90}px 0px` });
    io.observe(sentinel);
  }

  function threshold() {
    return ctx.store.get().trust?.threshold ?? 20;
  }

  /** @param {import("./search-controller.js").SearchView} v */
  function render(v) {
    const now = Math.floor(Date.now() / 1000);
    const opts = {
      threshold: threshold(),
      mediaLinks: ctx.health.has("media_releases"),
      now,
    };
    offline.replaceChildren(
      ...(v.offline && v.hits.length > 0 && v.error
        ? [banner(`Offline: ${userMessage(v.error)}. Showing the cached page.`, { kind: "error" })]
        : []),
    );
    if (v.generation !== shownGeneration || v.hits.length < shownCount || rebuild) {
      const fresh = shownGeneration !== -1 && v.generation !== shownGeneration;
      shownGeneration = v.generation;
      rebuild = false;
      const sel = deps.selected();
      list.replaceChildren(...v.hits.map((hit) => resultRow(hit, { ...opts, selected: hit.infohash === sel })));
      shownCount = v.hits.length;
      // A new search starts at the top (not a view mounted again, which keeps its scroll).
      if (fresh) {
        const root = deps.scrollRoot();
        if (root) root.scrollTop = 0;
        else if (list.getBoundingClientRect().top < 0) globalThis.scrollTo(0, 0);
      }
    } else if (v.hits.length > shownCount) {
      const sel = deps.selected();
      list.append(...v.hits.slice(shownCount).map((hit) => resultRow(hit, { ...opts, selected: hit.infohash === sel })));
      shownCount = v.hits.length;
    }
    count.textContent = v.hits.length > 0 || (v.ran && !v.loading && !v.error)
      ? resultsText(v.total, v.lowerBound) + (v.cachedAt != null ? ` · ${cachedAgo(v.cachedAt)}` : "")
      : "";
    /** @type {Node[]} */
    const e = [];
    if (v.error && v.hits.length === 0) {
      e.push(centered(userMessage(v.error), { label: "Retry", onClick: () => controller.submit() }));
    } else if (v.hits.length === 0 && v.ran && !v.loading) {
      e.push(centered(
        activeCount(v.state.filters) > 0
          ? "No results. Filters and the trust threshold apply."
          : "No results. The trust threshold applies.",
      ));
    }
    empty.replaceChildren(...e);
    footer.replaceChildren(
      ...(v.loadingMore
        ? [h("div", { class: "spinner-row", role: "status", "aria-label": "Loading more results" }, h("span", { class: "spinner" }))]
        : v.moreFailed
        ? [h("div", { class: "retry-row" }, h("span", { text: "More results did not load." }), h("button", {
          type: "button",
          class: "btn tonal",
          text: "Retry",
          "on:click": () => controller.retryMore(),
        }))]
        : []),
    );
  }

  /** Marks the row of the detail shown in the pane. */
  function markSelected() {
    const sel = deps.selected();
    for (const li of list.children) {
      const e = /** @type {HTMLElement} */ (li);
      e.classList.toggle("selected", e.dataset.ih === sel);
    }
  }

  const unsub = controller.subscribe(render);
  let lastThreshold = threshold();
  const unsubTrust = ctx.store.subscribe(() => {
    if (threshold() === lastThreshold) return;
    lastThreshold = threshold();
    rebuild = true;
    render(controller.get());
  });
  render(controller.get());
  queueMicrotask(observe);

  return {
    el,
    markSelected,
    destroy() {
      unsub();
      unsubTrust();
      io?.disconnect();
    },
  };
}
