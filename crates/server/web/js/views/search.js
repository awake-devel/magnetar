// The Search workspace (docs/WEB.md "Layout"): phone = one column with a top bar and the filter
// sheet; medium = results + detail pane, filters in a drawer; wide = filters sidebar (live) +
// results + detail pane. On medium and wide, `/torrent/:ih` renders here, in the pane.

import { h } from "../dom.js";
import { icon } from "../icons.js";
import { logo } from "../icons.js";
import * as prefs from "../prefs.js";
import {
  activeCount,
  filtersOf,
  SORT_LABELS,
  SORTS,
  stateFromParams,
} from "../search-model.js";
import { progressBar } from "../ui/banner.js";
import { openDialog } from "../ui/dialog.js";
import { iconButton } from "../ui/shell.js";
import { createFilterPanel, openFilterSheet } from "./filters.js";
import { createResults } from "./results.js";
import { openSaveDialog } from "./save-dialog.js";
import { createSearchController } from "./search-controller.js";

/** @type {import("./search-controller.js").SearchController | null} */
let controller = null;

/**
 * The page's one search controller.
 * @param {import("../types.js").Ctx} ctx
 */
export function searchController(ctx) {
  controller ??= createSearchController(ctx);
  return controller;
}

/** The Search tab's href: the current search, else the last one of this browser. */
export function lastSearchHref() {
  if (controller) return controller.href();
  const raw = prefs.getRaw(prefs.KEYS.lastSearch) ?? "";
  const qs = new URLSearchParams(raw).toString();
  return qs === "" ? "/" : `/?${qs}`;
}

/**
 * Runs a search on the Search tab (a saved search, "Search for X" from Stats): its text,
 * filters and sort replace the current ones.
 * @param {import("../types.js").Ctx} ctx
 * @param {Record<string, unknown>} query a SearchQuery body
 */
export function runSearch(ctx, query) {
  const c = searchController(ctx);
  const sort = SORTS.find((s) => s === query.sort) ?? "relevance";
  c.setState({
    text: typeof query.text === "string" ? query.text : "",
    filters: filtersOf(query),
    sort,
    descending: query.descending !== false,
  }, { force: true });
  ctx.nav(c.href());
}

/**
 * @param {HTMLElement} el
 * @param {import("../types.js").ViewArgs} args
 * @returns {import("../types.js").ViewHandle}
 */
export function mount(el, { params, query, ctx }) {
  const c = searchController(ctx);
  const layout = ctx.layout;
  const phone = layout === "phone";

  // --- top: search box and actions -------------------------------------------------------
  const input = /** @type {HTMLInputElement} */ (h("input", {
    type: "search",
    id: "search-box",
    class: "search-input",
    placeholder: "Search torrents or imdb:tt…",
    "aria-label": "Search",
    autocomplete: "off",
    spellcheck: "false",
    enterkeyhint: "search",
    value: c.get().state.text,
  }));
  const clearBtn = h("button", {
    type: "button",
    class: "btn icon-only search-clear",
    "aria-label": "Clear search",
    title: "Clear search",
    "on:click": () => {
      input.value = "";
      c.setText("");
      c.submit();
      input.focus();
    },
  }, icon("close"));
  input.addEventListener("input", () => {
    clearBtn.hidden = input.value === "";
    c.setText(input.value);
  });
  const form = h(
    "form",
    {
      class: "searchbox",
      role: "search",
      "on:submit": (/** @type {SubmitEvent} */ e) => {
        e.preventDefault();
        c.setText(input.value);
        c.submit();
        if (phone) input.blur();
      },
    },
    h("span", { class: "searchbox-icon", "aria-hidden": "true" }, icon("search")),
    input,
    clearBtn,
  );

  const saveBtn = iconButton("star", "Save search", () => {
    const s = c.get().state;
    openSaveDialog({
      initialName: s.text.trim() || "All torrents",
      filterCount: activeCount(s.filters),
      phone,
      onSave: (name, notify) => c.saveSearch(name, notify),
    });
  });
  const sortBtn = iconButton("sort", "Sort", openSort);
  const filterHost = h("span", { class: "filter-btn-host" });
  function renderFilterBtn() {
    if (layout === "wide") {
      filterHost.replaceChildren();
      return;
    }
    const n = activeCount(c.get().state.filters);
    // The sheet's close gives focus back to the old button, then applying rebuilds it (new
    // badge): keep the focus on the new one, not on the body.
    const focused = filterHost.contains(document.activeElement);
    const btn = iconButton("filter", "Filters", openFilters, { badge: n });
    filterHost.replaceChildren(btn);
    if (focused) btn.focus({ preventScroll: true });
  }
  const actions = h("div", { class: "search-actions" }, saveBtn, sortBtn, filterHost);

  function openSort() {
    const v = c.get().state;
    const withRating = ctx.health.has("filters");
    const items = SORTS.filter((s) => withRating || s !== "rating").map((s) => {
      const id = `sort-${s}`;
      const radio = /** @type {HTMLInputElement} */ (h("input", { type: "radio", name: "sort", id, value: s, checked: v.sort === s }));
      radio.addEventListener("change", () => {
        c.setSort(s);
        d.close();
      });
      return h("label", { class: "menu-item", for: id }, radio, h("span", { text: SORT_LABELS[s] }));
    });
    const dir = h("button", {
      type: "button",
      class: "menu-item btn text",
      "on:click": () => {
        c.toggleDirection();
        d.close();
      },
    }, icon(v.descending ? "down" : "up"), h("span", { text: v.descending ? "Descending" : "Ascending" }));
    const d = openDialog({
      title: "Sort",
      cls: phone ? "sheet menu" : "menu",
      body: [h("div", { role: "radiogroup", "aria-label": "Sort by", class: "menu-group" }, ...items), h("hr"), dir],
      actions: [],
      closeButton: true,
    });
  }

  function openFilters() {
    openFilterSheet(ctx, {
      initial: c.get().state.filters,
      text: () => c.get().state.text,
      onApply: (f) => {
        if (JSON.stringify(f) !== JSON.stringify(c.get().state.filters)) c.setFilters(f);
      },
    });
  }

  const loading = progressBar();
  const top = h(
    "div",
    { class: "search-top" },
    phone ? h("div", { class: "topbar" }, h("span", { class: "topbar-brand" }, logo(), h("h1", { class: "topbar-title", text: "Magnetar" })), actions) : null,
    h("div", { class: "search-row" }, form, phone ? null : actions),
    loading,
  );

  // --- columns -----------------------------------------------------------------------------
  const resultsCol = h("section", { class: "results-col", "aria-label": "Search results" });
  const paneEl = h("section", { class: "detail-pane", "aria-label": "Torrent detail" });
  /** @type {{ih: string | null | undefined, handle: import("../types.js").ViewHandle | null, gen: number}} */
  const pane = { ih: undefined, handle: null, gen: 0 };
  const results = createResults({
    ctx,
    controller: c,
    scrollRoot: () => (phone ? null : resultsCol),
    selected: () => pane.ih ?? null,
  });
  resultsCol.append(top, results.el);

  /** @type {ReturnType<typeof createFilterPanel> | null} */
  let live = null;
  /** @type {Node[]} */
  const cols = [];
  if (layout === "wide") {
    live = createFilterPanel({
      ctx,
      mode: "live",
      initial: c.get().state.filters,
      text: () => c.get().state.text,
      onChange: (f) => c.setFilters(f),
    });
    cols.push(h("aside", { class: "filters-col", "aria-label": "Filters" }, live.el));
  }
  cols.push(resultsCol);
  if (!phone) cols.push(paneEl);
  const root = h("div", { class: ["search-ws", `ws-${layout}`] }, ...cols);
  el.append(root);

  /** @param {string | null} ih */
  function showDetail(ih) {
    if (phone || pane.ih === ih) return;
    pane.handle?.unmount();
    pane.handle = null;
    pane.ih = ih;
    const gen = ++pane.gen;
    paneEl.replaceChildren();
    results.markSelected();
    if (ih == null) {
      paneEl.append(h("div", { class: "pane-empty" }, h("p", { text: "Pick a result to see its details." })));
      return;
    }
    const host = h("div", { class: "pane-view" });
    paneEl.append(host);
    paneEl.scrollTop = 0;
    void import("./detail.js").then((mod) => {
      if (gen !== pane.gen) return;
      pane.handle = mod.mount(host, { params: { ih }, query: new URLSearchParams(), ctx: { ...ctx, pane: true } });
    });
  }

  // --- state from the route ------------------------------------------------------------------
  /**
   * @param {Record<string, string>} p
   * @param {URLSearchParams} q
   */
  function applyRoute(p, q) {
    if (p.ih) {
      c.restoreLast();
      showDetail(p.ih);
    } else {
      showDetail(null);
      c.setState(stateFromParams(q));
    }
  }

  let lastText = c.get().state.text;
  let lastFilters = c.get().state.filters;
  const unsub = c.subscribe((v) => {
    loading.hidden = !v.loading;
    if (v.state.text !== lastText) {
      lastText = v.state.text;
      if (input.value !== v.state.text) input.value = v.state.text;
      clearBtn.hidden = input.value === "";
      live?.refreshFacetsLater();
    }
    if (v.state.filters !== lastFilters) {
      lastFilters = v.state.filters;
      live?.setFilters(v.state.filters);
      live?.refreshFacetsLater();
      renderFilterBtn();
    }
  });
  const unsubHealth = ctx.health.subscribe(renderFilterBtn);
  clearBtn.hidden = input.value === "";
  loading.hidden = !c.get().loading;
  renderFilterBtn();
  applyRoute(params, query);
  live?.refreshFacets();

  /** @param {KeyboardEvent} e */
  function onKey(e) {
    if (e.key !== "/" || e.ctrlKey || e.metaKey || e.altKey) return;
    const t = e.target;
    if (t instanceof HTMLElement && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;
    if (document.querySelector("dialog[open]")) return;
    e.preventDefault();
    input.focus();
    input.select();
  }
  document.addEventListener("keydown", onKey);

  // Back to the same place: the controller keeps the scroll of the last mount.
  requestAnimationFrame(() => {
    if (phone) {
      if (c.scroll.window > 0) globalThis.scrollTo(0, c.scroll.window);
    } else if (c.scroll.column > 0) {
      resultsCol.scrollTop = c.scroll.column;
    }
  });

  return {
    update(p, q) {
      applyRoute(p, q);
    },
    unmount() {
      if (phone) c.scroll.window = globalThis.scrollY;
      else c.scroll.column = resultsCol.scrollTop;
      document.removeEventListener("keydown", onKey);
      unsub();
      unsubHealth();
      results.destroy();
      live?.destroy();
      pane.handle?.unmount();
    },
  };
}
