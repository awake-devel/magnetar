// The Search tab's state and requests (port of SearchViewModel, docs/APP.md §12): one instance
// per page, kept across view mounts so tab switches and Back keep the text, filters, results
// and scroll. The text box is debounced (250 ms); filters, sort, retry and the adult switch run
// at once; every run aborts the previous one, so a late answer never replaces a newer one.

import { ApiError, isAborted, isOffline, userMessage } from "../api.js";
import * as prefs from "../prefs.js";
import {
  buildQuery,
  cacheQuery,
  DEFAULT_LIMIT,
  emptySearchState,
  MAX_OFFSET,
  sameState,
  stateFromParams,
  stateToParams,
  supportedBy,
  supportedSort,
} from "../search-model.js";

/** Typing pause before a search runs. */
export const SEARCH_DEBOUNCE_MS = 250;

/**
 * @typedef {object} SearchView
 * @property {import("../search-model.js").SearchState} state
 * @property {import("../types.js").TorrentSummary[]} hits
 * @property {number} total
 * @property {boolean} lowerBound
 * @property {boolean} loading
 * @property {boolean} loadingMore
 * @property {boolean} moreFailed a next page failed: a Retry row, no automatic retry
 * @property {boolean} endReached
 * @property {ApiError | null} error
 * @property {boolean} offline
 * @property {number | null} cachedAt the shown page is the cache's (unix ms)
 * @property {number} generation bumped for every new first page (the list goes back to the top)
 * @property {boolean} ran a search ran at least once
 */

/**
 * @param {import("../types.js").Ctx} ctx
 */
export function createSearchController(ctx) {
  /** @type {SearchView} */
  let v = {
    // The last search of this browser, so a page opened directly (Saved, a title page) and the
    // Search tab's link see it before the Search view runs it (restoreLast).
    state: lastSearchState(),
    hits: [],
    total: 0,
    lowerBound: false,
    loading: false,
    loadingMore: false,
    moreFailed: false,
    endReached: true,
    error: null,
    offline: false,
    cachedAt: null,
    generation: 0,
    ran: false,
  };
  /** @type {Set<(v: SearchView) => void>} */
  const subs = new Set();
  /** @type {AbortController | null} */
  let running = null;
  /** @type {AbortController | null} */
  let more = null;
  /** @type {ReturnType<typeof setTimeout> | null} */
  let debounce = null;
  let seq = 0;
  let shownSeq = -1;
  /** Scroll positions saved when the view unmounts. */
  const scroll = { window: 0, column: 0 };

  /** @param {Partial<SearchView>} p */
  function set(p) {
    v = { ...v, ...p };
    for (const s of [...subs]) s(v);
  }

  function persist() {
    prefs.setRaw(prefs.KEYS.lastSearch, stateToParams(v.state));
    if (location.pathname === "/") {
      const qs = stateToParams(v.state);
      const url = qs === "" ? "/" : `/?${qs}`;
      if (location.pathname + location.search !== url) history.replaceState(history.state, "", url);
    }
  }

  /**
   * @param {Partial<import("../search-model.js").SearchState>} p
   * @param {boolean} immediate
   */
  function change(p, immediate) {
    set({ state: { ...v.state, ...p } });
    persist();
    enqueue(immediate);
  }

  /** @param {boolean} immediate */
  function enqueue(immediate) {
    if (debounce) clearTimeout(debounce);
    debounce = null;
    if (immediate) void run();
    else debounce = setTimeout(() => void run(), SEARCH_DEBOUNCE_MS);
  }

  function features() {
    return ctx.health.features();
  }

  /**
   * @param {import("../types.js").TorrentSummary[]} hits
   * @param {ReadonlySet<string>} feats
   */
  function visible(hits, feats) {
    // A server without the adult filter (pre-D52) cannot hide adult torrents: drop them here.
    if (feats.has("adult_filter") || prefs.showAdult() || v.state.filters.categories.includes("porn")) return hits;
    return hits.filter((h) => h.category !== "porn");
  }

  /**
   * @param {ReadonlySet<string>} feats
   * @param {number} offset
   */
  function query(feats, offset) {
    const s = v.state;
    return buildQuery(
      s.text,
      supportedBy(s.filters, feats),
      supportedSort(s.sort, feats),
      s.descending,
      prefs.showAdult(),
      { offset },
    );
  }

  async function run() {
    running?.abort();
    more?.abort();
    const ac = new AbortController();
    running = ac;
    const mySeq = ++seq;
    const s = v.state;
    set({ loading: true, loadingMore: false, moreFailed: false, error: null, ran: true });
    const key = cacheQuery(s.text, s.filters, s.sort, s.descending, prefs.showAdult());
    // The cached page first: offline, /health may take its whole timeout to fail.
    const cached = await ctx.cache.get("search", key);
    if (ac.signal.aborted) return;
    if (cached && cached.json) {
      set({
        hits: visible(cached.json.hits ?? [], features()),
        total: cached.json.total ?? 0,
        lowerBound: cached.json.total_is_lower_bound === true,
        cachedAt: cached.savedAt,
        endReached: true,
        generation: v.generation + 1,
      });
    }
    const health = await ctx.health.refresh();
    if (ac.signal.aborted) return;
    const feats = features();
    try {
      if (health == null) throw new ApiError("network", "Server unreachable");
      const page = await ctx.api.post("/api/v1/search", query(feats, 0), { cls: "SEARCH", signal: ac.signal });
      if (isAborted(page) || ac.signal.aborted) return;
      const hits = visible(page?.hits ?? [], feats);
      const sameTop = v.cachedAt != null && v.hits[0]?.infohash === hits[0]?.infohash;
      set({
        hits,
        total: page?.total ?? 0,
        lowerBound: page?.total_is_lower_bound === true,
        loading: false,
        error: null,
        offline: false,
        cachedAt: null,
        endReached: (page?.hits ?? []).length < DEFAULT_LIMIT,
        generation: sameTop ? v.generation : v.generation + 1,
      });
      shownSeq = mySeq;
      void ctx.cache.put("search", key, page);
    } catch (e) {
      if (ac.signal.aborted) return;
      const err = e instanceof ApiError ? e : new ApiError("network", String(e));
      const keep = isOffline(err) && cached != null;
      set({
        loading: false,
        error: err,
        offline: isOffline(err),
        hits: keep ? v.hits : [],
        total: keep ? v.total : 0,
        cachedAt: keep ? v.cachedAt : null,
        endReached: true,
      });
      if (isOffline(err)) void ctx.health.refresh({ force: true });
    } finally {
      if (running === ac) running = null;
    }
  }

  function loadMore() {
    if (v.loading || v.loadingMore || v.endReached || v.hits.length === 0 || v.cachedAt != null || shownSeq !== seq) {
      return;
    }
    if (v.hits.length >= MAX_OFFSET) {
      set({ endReached: true });
      return;
    }
    const mySeq = seq;
    const ac = new AbortController();
    more = ac;
    set({ loadingMore: true, moreFailed: false });
    const feats = features();
    void (async () => {
      try {
        const page = await ctx.api.post("/api/v1/search", query(feats, v.hits.length), { cls: "SEARCH", signal: ac.signal });
        if (isAborted(page) || seq !== mySeq) return;
        const known = new Set(v.hits.map((h) => h.infohash));
        const merged = [...v.hits, ...visible(page?.hits ?? [], feats).filter((h) => !known.has(h.infohash))];
        const total = page?.total ?? 0;
        const lb = page?.total_is_lower_bound === true;
        set({
          hits: merged,
          total,
          lowerBound: lb,
          loadingMore: false,
          endReached: (page?.hits ?? []).length < DEFAULT_LIMIT || (!lb && merged.length >= total) ||
            merged.length >= MAX_OFFSET,
        });
      } catch (e) {
        if (ac.signal.aborted || seq !== mySeq) return;
        set({ loadingMore: false, moreFailed: true });
        if (e instanceof ApiError && e.kind !== "unauthorized") {
          ctx.snackbar.show(`More results: ${e.message || "failed"}`);
        }
      }
    })();
  }

  /**
   * Saves the current search on the server (with the adult switch, as the app).
   * @param {string} name
   * @param {boolean} notify
   */
  async function saveSearch(name, notify) {
    const feats = features();
    const s = v.state;
    const q = buildQuery(s.text, supportedBy(s.filters, feats), supportedSort(s.sort, feats), s.descending, prefs.showAdult());
    try {
      await ctx.api.post("/api/v1/saved-searches", { name, query: q, notify }, { cls: "DETAIL" });
      ctx.snackbar.show(`Saved “${name}”`);
    } catch (e) {
      ctx.snackbar.show(`Not saved: ${userMessage(e)}`);
    }
    return true;
  }

  // The adult switch and the server's trust threshold change what a search returns.
  prefs.onChange((key) => {
    if (key === prefs.KEYS.showAdult && v.ran) enqueue(true);
  });
  let lastThreshold = ctx.store.get().trust?.threshold ?? null;
  ctx.store.subscribe((s) => {
    const t = s.trust?.threshold ?? null;
    if (t !== lastThreshold) {
      const was = lastThreshold;
      lastThreshold = t;
      if (was != null && v.ran) enqueue(true);
    }
  });

  return {
    get: () => v,
    /** @param {(v: SearchView) => void} fn */
    subscribe(fn) {
      subs.add(fn);
      return () => subs.delete(fn);
    },
    /** @param {string} text */
    setText(text) {
      if (text === v.state.text) return;
      change({ text }, false);
    },
    submit() {
      persist();
      enqueue(true);
    },
    /** @param {import("../search-model.js").SearchFilters} filters */
    setFilters(filters) {
      change({ filters }, true);
    },
    /** @param {import("../search-model.js").Sort} sort */
    setSort(sort) {
      change({ sort, descending: true }, true);
    },
    toggleDirection() {
      change({ descending: !v.state.descending }, true);
    },
    /**
     * A state from the URL, a saved search or the last search: runs when it differs.
     * @param {import("../search-model.js").SearchState} state
     * @param {{force?: boolean}} [opts]
     */
    setState(state, opts = {}) {
      if (!opts.force && v.ran && sameState(state, v.state)) return;
      set({ state });
      persist();
      enqueue(true);
    },
    /** The state of the last search of this browser (pref), for a page opened directly. */
    restoreLast() {
      if (v.ran) return;
      set({ state: lastSearchState() });
      enqueue(true);
    },
    loadMore,
    retryMore() {
      set({ moreFailed: false });
      loadMore();
    },
    saveSearch,
    scroll,
    /** The Search tab's href: "/" plus the current search. */
    href() {
      const qs = stateToParams(v.state);
      return qs === "" ? "/" : `/?${qs}`;
    },
  };
}

/** The last search of this browser (pref), or the empty one. */
function lastSearchState() {
  const raw = prefs.getRaw(prefs.KEYS.lastSearch);
  return raw == null ? emptySearchState() : stateFromParams(new URLSearchParams(raw));
}

/** @typedef {ReturnType<typeof createSearchController>} SearchController */
