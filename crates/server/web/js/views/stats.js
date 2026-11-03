// The Stats tab, `/stats`, `/stats/graphs`, `/stats/status` (StatsScreen.kt, StatsViewModel.kt,
// D47, docs/STATS.md): Charts (health, breakdowns with cross-filtering, activity, "Where
// discoveries come from"), Graphs and Status. One mounted view for the three routes; the last
// tab is remembered. Every answer is cached in the page cache and shown offline with its age.

import { ApiError, isAborted, isOffline, userMessage } from "../api.js";
import { chartColors } from "../chart-colors.js";
import { h, untrusted } from "../dom.js";
import { cachedAgo, formatAgo } from "../format.js";
import { formatNumber, formatShare } from "../numfmt.js";
import * as prefs from "../prefs.js";
import { redactFleet } from "../probes-model.js";
import { activeCount, buildQuery, EMPTY_FILTERS, makeFilters, supportedBy } from "../search-model.js";
import { redactSources } from "../sources-model.js";
import {
  activityOf,
  breakdownFields,
  dhtLines,
  FILTERABLE,
  offeredActivities,
  seedChecks,
  selectedStat,
  statChips,
  toggleStat,
  valueLabel,
} from "../stats-model.js";
import { banner, progressBar } from "../ui/banner.js";
import { iconButton, pageHeader } from "../ui/shell.js";
import { runSearch } from "./search.js";
import { sourcesCard } from "./sources-card.js";
import { activityChart, breakdownCard, kvLines, statusBar } from "./stats-charts.js";
import { graphsTab } from "./stats-graphs.js";
import { seedParts, statusTab } from "./stats-status.js";

/** @typedef {"charts" | "graphs" | "status"} Tab */

const TABS = /** @type {const} */ ([["charts", "Charts", "/stats"], ["graphs", "Graphs", "/stats/graphs"], ["status", "Status", "/stats/status"]]);

/**
 * @typedef {object} StatsState
 * @property {boolean} healthAsked
 * @property {boolean} healthOk
 * @property {ReadonlySet<string>} features
 * @property {string} version
 * @property {string} status
 * @property {boolean} db
 * @property {Readonly<import("../search-model.js").SearchFilters>} filters
 * @property {import("../types-stats.js").BreakdownSet | null} breakdowns
 * @property {import("../types-stats.js").Breakdown | null} years
 * @property {boolean} loadingBreakdowns
 * @property {string | null} breakdownError
 * @property {number | null} breakdownCachedAt
 * @property {import("../types-stats.js").StatsHealth | null} stats
 * @property {string | null} statsError
 * @property {number | null} statsCachedAt
 * @property {string} activity
 * @property {boolean} hourly
 * @property {import("../types-stats.js").Timeseries | null} series
 * @property {string | null} seriesError
 * @property {number | null} seriesCachedAt
 * @property {import("../types-stats.js").CrawlSources | null} sources
 * @property {string | null} sourcesError
 * @property {number | null} sourcesCachedAt
 * @property {import("../types-stats.js").FleetStats | null} fleet
 * @property {number | null} fleetCachedAt
 */

/** @param {string} pathname @returns {Tab} */
function tabOf(pathname) {
  if (pathname.startsWith("/stats/graphs")) return "graphs";
  if (pathname.startsWith("/stats/status")) return "status";
  return "charts";
}

/**
 * @param {HTMLElement} el
 * @param {import("../types.js").ViewArgs} args
 * @returns {import("../types.js").ViewHandle}
 */
export function mount(el, { ctx }) {
  /** @type {Tab} */
  let tab = tabOf(location.pathname);
  const remembered = prefs.getRaw(prefs.KEYS.statsTab);
  if (tab === "charts" && location.pathname === "/stats" && (remembered === "graphs" || remembered === "status")) {
    tab = remembered;
    queueMicrotask(() => ctx.nav(remembered === "graphs" ? "/stats/graphs" : "/stats/status", { replace: true }));
  }
  prefs.setRaw(prefs.KEYS.statsTab, tab);
  const storedMetric = prefs.getRaw(prefs.KEYS.activityMetric);
  /** @type {StatsState} */
  const s = {
    healthAsked: false,
    healthOk: false,
    features: ctx.health.features(),
    version: ctx.health.version(),
    status: "",
    db: false,
    filters: EMPTY_FILTERS,
    breakdowns: null,
    years: null,
    loadingBreakdowns: false,
    breakdownError: null,
    breakdownCachedAt: null,
    stats: null,
    statsError: null,
    statsCachedAt: null,
    activity: activityOf(storedMetric ?? "").metric,
    hourly: prefs.getRaw(prefs.KEYS.activityBucket) === "hour",
    series: null,
    seriesError: null,
    seriesCachedAt: null,
    sources: null,
    sourcesError: null,
    sourcesCachedAt: null,
    fleet: null,
    fleetCachedAt: null,
  };
  const activityUi = { window: /** @type {import("../charts.js").ChartWindow | null} */ (null), picked: /** @type {number | null} */ (null) };
  const sourcesUi = { stored: false, picked: /** @type {number | null} */ (null) };
  let life = new AbortController();
  /** @type {AbortController | null} */
  let breakdownCall = null;
  /** @type {AbortController | null} */
  let seriesCall = null;

  const tabBar = h("div", { class: "tabs", role: "tablist", "aria-label": "Stats" });
  const panel = h("div", { class: "stats-panel page-pad", role: "tabpanel", id: "stats-panel" });
  el.append(pageHeader(ctx, { title: "Stats", actions: [iconButton("refresh", "Refresh", () => void refresh())] }), tabBar, panel);

  const has = (/** @type {string} */ f) => s.features.has(f);
  const hasDht = () => has("dht") || s.stats?.dht != null;
  const hasProbes = () => s.fleet?.probe_backfill != null;

  /**
   * Reads a cached answer, then the live one; offline keeps the cached answer.
   * @template T
   * @param {{endpoint: string, key: unknown, call: (signal: AbortSignal) => Promise<T>, signal: AbortSignal,
   *   onCached: (v: T, at: number) => void, onLive: (v: T) => void, onError: (e: ApiError) => void}} o
   */
  async function cachedCall(o) {
    const c = await ctx.cache.get(o.endpoint, o.key);
    if (o.signal.aborted) return;
    if (c?.json != null) o.onCached(c.json, c.savedAt);
    try {
      const v = await o.call(o.signal);
      if (isAborted(v) || o.signal.aborted) return;
      void ctx.cache.put(o.endpoint, o.key, v);
      o.onLive(v);
    } catch (e) {
      if (o.signal.aborted) return;
      o.onError(e instanceof ApiError ? e : new ApiError("network", String(e)));
    }
  }

  async function refresh() {
    life.abort();
    life = new AbortController();
    const signal = life.signal;
    s.healthAsked = false;
    render();
    try {
      const hres = await ctx.api.get("/health", { cls: "DETAIL", signal });
      if (isAborted(hres) || signal.aborted) return;
      s.healthOk = true;
      s.status = typeof hres?.status === "string" ? hres.status : "";
      s.db = hres?.db === true;
      s.version = typeof hres?.version === "string" ? hres.version : "";
      s.features = new Set(Array.isArray(hres?.features) ? hres.features.filter((/** @type {unknown} */ f) => typeof f === "string") : []);
      void ctx.health.refresh({ force: true });
    } catch {
      if (signal.aborted) return;
      s.healthOk = false;
      s.features = ctx.health.features();
    }
    s.healthAsked = true;
    render();
    if (has("stats") || !s.healthOk) {
      void loadBreakdowns();
      void loadSeries();
      void loadStats(signal);
    }
    if (has("crawl_sources")) void loadSources(signal);
    else {
      s.sources = null;
      s.sourcesError = null;
    }
    if (has("workers")) void loadFleet(signal);
    else {
      s.fleet = null;
      keepActivityOffered();
    }
  }

  /** @param {AbortSignal} signal */
  function loadStats(signal) {
    return cachedCall({
      endpoint: "stats/health",
      key: {},
      signal,
      call: (sg) => ctx.api.get("/api/v1/stats/health", { cls: "STATS", signal: sg }),
      onCached: (v, at) => {
        if (s.stats == null) {
          s.stats = /** @type {any} */ (v);
          s.statsCachedAt = at;
          render();
        }
      },
      onLive: (v) => {
        s.stats = /** @type {any} */ (v);
        s.statsCachedAt = null;
        s.statsError = null;
        render();
      },
      onError: (e) => {
        s.statsError = userMessage(e);
        render();
      },
    });
  }

  /** @param {AbortSignal} signal */
  function loadSources(signal) {
    return cachedCall({
      endpoint: "stats/sources",
      key: {},
      signal,
      call: async (sg) => {
        const r = await ctx.api.get("/api/v1/stats/sources", { cls: "STATS", signal: sg });
        return isAborted(r) ? r : redactSources(r ?? {});
      },
      onCached: (v, at) => {
        if (s.sources == null) {
          s.sources = /** @type {any} */ (v);
          s.sourcesCachedAt = at;
          render();
        }
      },
      onLive: (v) => {
        s.sources = /** @type {any} */ (v);
        s.sourcesCachedAt = null;
        s.sourcesError = null;
        render();
      },
      onError: (e) => {
        s.sourcesError = userMessage(e);
        if (!isOffline(e)) {
          s.sources = null;
          s.sourcesCachedAt = null;
        }
        render();
      },
    });
  }

  /** @param {AbortSignal} signal */
  async function loadFleet(signal) {
    await cachedCall({
      endpoint: "stats/fleet",
      key: {},
      signal,
      call: async (sg) => {
        const r = await ctx.api.get("/api/v1/stats", { cls: "STATS", signal: sg });
        return isAborted(r) ? r : redactFleet(r ?? {});
      },
      onCached: (v, at) => {
        if (s.fleet == null) {
          s.fleet = /** @type {any} */ (v);
          s.fleetCachedAt = at;
        }
      },
      onLive: (v) => {
        s.fleet = /** @type {any} */ (v);
        s.fleetCachedAt = null;
      },
      onError: (e) => {
        if (!isOffline(e)) {
          s.fleet = null;
          s.fleetCachedAt = null;
        }
      },
    });
    if (signal.aborted) return;
    keepActivityOffered();
    render();
  }

  /** The picked activity is no longer offered: back to the total (its chip is hidden). */
  function keepActivityOffered() {
    if (offeredActivities(hasDht(), hasProbes()).some((a) => a.metric === s.activity)) return;
    s.activity = "torrents_total";
    s.series = null;
    void loadSeries();
  }

  /** @param {Readonly<import("../search-model.js").SearchFilters>} f */
  function queryOf(f) {
    return buildQuery("", supportedBy(f, s.features), "relevance", true, prefs.showAdult(), { limit: 1, nowSec: 0 });
  }

  async function loadBreakdowns() {
    breakdownCall?.abort();
    const ac = new AbortController();
    breakdownCall = ac;
    const q = queryOf(s.filters);
    const main = { query: q, fields: breakdownFields(s.features), top: 10 };
    const years = { query: q, fields: ["year"], top: 50 };
    s.loadingBreakdowns = true;
    s.breakdownError = null;
    const [cMain, cYears] = await Promise.all([ctx.cache.get("stats/breakdown", main), ctx.cache.get("stats/breakdown", years)]);
    if (ac.signal.aborted) return;
    if (cMain?.json) {
      s.breakdowns = cMain.json;
      s.breakdownCachedAt = cMain.savedAt;
    } else s.breakdownCachedAt = null;
    if (cYears?.json) s.years = (cYears.json.breakdowns ?? []).find((/** @type {{field: string}} */ b) => b.field === "year") ?? null;
    render();
    if (!s.healthOk && !cMain) {
      s.loadingBreakdowns = false;
      s.breakdownError = "Server unreachable";
      render();
      return;
    }
    const yearsCall = ctx.api.post("/api/v1/stats/breakdown", years, { cls: "STATS", signal: ac.signal }).catch(() => null);
    try {
      const set = await ctx.api.post("/api/v1/stats/breakdown", main, { cls: "STATS", signal: ac.signal });
      if (isAborted(set) || ac.signal.aborted) return;
      void ctx.cache.put("stats/breakdown", main, set);
      const ys = await yearsCall;
      if (ac.signal.aborted) return;
      if (ys && !isAborted(ys)) void ctx.cache.put("stats/breakdown", years, ys);
      s.breakdowns = set;
      s.years = ys && !isAborted(ys) ? (ys.breakdowns ?? []).find((/** @type {{field: string}} */ b) => b.field === "year") ?? null : null;
      s.breakdownCachedAt = null;
    } catch (e) {
      if (ac.signal.aborted) return;
      s.breakdownError = userMessage(e);
      if (!isOffline(e)) {
        s.breakdowns = null;
        s.years = null;
        s.breakdownCachedAt = null;
      }
    }
    s.loadingBreakdowns = false;
    render();
  }

  async function loadSeries() {
    seriesCall?.abort();
    const ac = new AbortController();
    seriesCall = ac;
    const metric = s.activity;
    const bucket = s.hourly ? "hour" : "day";
    s.seriesError = null;
    activityUi.window = null;
    activityUi.picked = null;
    render();
    await cachedCall({
      endpoint: "stats/timeseries",
      key: { metric, bucket },
      signal: ac.signal,
      call: (sg) =>
        ctx.api.get(`/api/v1/stats/timeseries?metric=${encodeURIComponent(metric)}&bucket=${bucket}`, { cls: "STATS", signal: sg }),
      onCached: (v, at) => {
        s.series = (/** @type {any} */ (v)?.series ?? []).find((/** @type {{metric: string}} */ x) => x.metric === metric) ?? null;
        s.seriesCachedAt = at;
        render();
      },
      onLive: (v) => {
        s.series = (/** @type {any} */ (v)?.series ?? []).find((/** @type {{metric: string}} */ x) => x.metric === metric) ?? null;
        s.seriesCachedAt = null;
        render();
      },
      onError: (e) => {
        if (!isOffline(e) || s.series == null) s.series = null;
        s.seriesError = userMessage(e);
        render();
      },
    });
  }

  /**
   * Click on a bar, slice or year: cross-filter every other chart on it.
   * @param {string} field
   * @param {string} value
   */
  function toggle(field, value) {
    if (!FILTERABLE.has(field)) return;
    s.filters = toggleStat(s.filters, field, value);
    void loadBreakdowns();
  }

  /**
   * Search with the cross-filters, plus field = value when given; a value that is not a search
   * filter (a publisher category) becomes the text.
   * @param {string} [field]
   * @param {string} [value]
   */
  function searchFor(field, value) {
    let f = s.filters;
    let text = "";
    if (field != null && value != null) {
      if (FILTERABLE.has(field)) {
        if (!selectedStat(f, field).includes(value)) f = toggleStat(f, field, value);
      } else text = value;
    }
    runSearch(ctx, buildQuery(text, f, "relevance", true, prefs.showAdult()));
  }

  function healthCard() {
    const st = s.stats;
    if (!st) return null;
    const colors = chartColors();
    /** @type {Node[]} */
    const body = [];
    if (st.liveness) {
      const c = seedChecks(st.liveness);
      body.push(h("p", { class: "muted small", text: `Seed checks: latest result of the ${formatNumber(c.checked)} torrents checked` }));
      body.push(statusBar(seedParts(s, colors)));
    }
    /** @type {[string, string][]} */
    const lines = [];
    if (st.liveness) {
      const c = seedChecks(st.liveness);
      lines.push(["Not checked yet", `${formatNumber(c.notChecked)} (${formatShare(c.notChecked, c.torrents)} of ${formatNumber(c.torrents)})`]);
    }
    const si = st.search_index;
    if (si) {
      lines.push([
        "Search index",
        si.state === "ready"
          ? (si.pending ?? 0) > 0 ? `${formatNumber(si.pending ?? 0)} changes queued` : "up to date"
          : si.state === "building"
          ? "building (charts count with it once done)"
          : si.state ?? "",
      ]);
    }
    const r = st.relays ?? {};
    lines.push(["Relays answering (24 h)", `${formatNumber(r.ok_24h ?? 0)} of ${formatNumber(r.enabled ?? 0)}`]);
    if ((r.failing ?? 0) > 0) lines.push(["Relays failing", formatNumber(r.failing ?? 0)]);
    lines.push(["Linked to a title", `${formatNumber(st.media_linked ?? 0)} of ${formatNumber(st.torrents ?? 0)}`]);
    if ((st.attrs_missing ?? 0) > 0) lines.push(["Names still to parse", formatNumber(st.attrs_missing ?? 0)]);
    const cr = st.crawler ?? {};
    lines.push([
      "Last crawl",
      (cr.running ? "running" : cr.last_run != null ? formatAgo(cr.last_run) : "not yet") + (cr.last_error != null ? ` · error: ${cr.last_error}` : ""),
    ]);
    body.push(kvLines(lines, { untrustedValues: true }));
    if (st.dht && has("dht")) {
      body.push(h("h4", { class: "section-title", text: "DHT crawler" }));
      body.push(kvLines(dhtLines(st.dht), { untrustedValues: true }));
    }
    return h("section", { class: "card health-card", "aria-label": "Health" }, h("h3", { class: "card-title", text: "Health" }), ...body);
  }

  function filterRow() {
    const chips = statChips(s.filters);
    if (chips.length === 0) {
      return h("p", { class: "muted small", text: "Click a bar or slice to filter every chart by it; use its search button (or long-press on touch) to search for it." });
    }
    return h(
      "div",
      { class: "stat-filters" },
      h(
        "div",
        { class: "chips", role: "group", "aria-label": "Chart filters" },
        ...chips.map(([field, value]) => {
          const label = valueLabel(field, value);
          return h("button", {
            type: "button",
            class: "chip on",
            "aria-label": `Remove ${label}`,
            dataset: { fkey: `chip-${field}-${value}` },
            "on:click": () => toggle(field, value),
          }, untrusted(label), h("span", { "aria-hidden": "true", text: " ×" }));
        }),
      ),
      h(
        "div",
        { class: "row-actions" },
        h("button", { type: "button", class: "btn tonal", text: "Search these", "on:click": () => searchFor() }),
        h("button", {
          type: "button",
          class: "btn text",
          text: "Clear",
          "on:click": () => {
            s.filters = makeFilters();
            void loadBreakdowns();
          },
        }),
      ),
    );
  }

  function activityCard() {
    const colors = chartColors();
    const offered = offeredActivities(hasDht(), hasProbes());
    const a = activityOf(s.activity);
    /** @type {Node | null} */
    let chart;
    if (s.series) {
      chart = h(
        "div",
        null,
        s.series.bucket === "day" && s.hourly ? h("p", { class: "muted small", text: "Kept per day only" }) : null,
        s.seriesError != null ? h("p", { class: "error-text small", text: `Offline: ${s.seriesError}` }) : null,
        s.seriesCachedAt != null ? h("p", { class: "muted small", text: cachedAgo(s.seriesCachedAt) }) : null,
        activityChart(s.series, a.unit, colors, activityUi),
      );
    } else if (s.seriesError != null) chart = h("p", { class: "error-text", text: s.seriesError });
    else chart = progressBar();
    /** @param {boolean} hourly @param {string} label */
    const bucketChip = (hourly, label) =>
      h("button", {
        type: "button",
        class: ["chip", s.hourly === hourly && "on"],
        "aria-pressed": String(s.hourly === hourly),
        text: label,
        dataset: { fkey: `bucket-${label}` },
        "on:click": () => {
          if (s.hourly === hourly) return;
          s.hourly = hourly;
          prefs.setRaw(prefs.KEYS.activityBucket, hourly ? "hour" : "day");
          s.series = null;
          void loadSeries();
        },
      });
    return h(
      "section",
      { class: "card activity-card", "aria-label": "Activity" },
      h("div", { class: "card-head" }, h("h3", { class: "card-title", text: "Activity" }), h("div", { class: "chips" }, bucketChip(false, "Days"), bucketChip(true, "Hours"))),
      h(
        "div",
        { class: "chips", role: "group", "aria-label": "Metric" },
        ...offered.map((x) =>
          h("button", {
            type: "button",
            class: ["chip", x.metric === s.activity && "on"],
            "aria-pressed": String(x.metric === s.activity),
            text: x.label,
            dataset: { fkey: `metric-${x.metric}` },
            "on:click": () => {
              if (x.metric === s.activity) return;
              s.activity = x.metric;
              prefs.setRaw(prefs.KEYS.activityMetric, x.metric);
              s.series = null;
              void loadSeries();
            },
          })
        ),
      ),
      chart,
    );
  }

  function chartsTab() {
    if (s.healthAsked && s.healthOk && !has("stats")) {
      return banner("This server is older than the app and has no charts (it lacks the stats feature).");
    }
    const colors = chartColors();
    /** @type {Node[]} */
    const out = [];
    if (s.loadingBreakdowns) out.push(progressBar());
    const set = s.breakdowns;
    if (s.breakdownError != null) {
      out.push(banner(
        (set != null ? "Offline: " : "") + s.breakdownError + (s.breakdownCachedAt != null ? ` · ${cachedAgo(s.breakdownCachedAt)}` : ""),
        { kind: "error", action: { label: "Retry", onClick: () => void loadBreakdowns() } },
      ));
    } else if (s.breakdownCachedAt != null) out.push(banner(cachedAgo(s.breakdownCachedAt)));
    const hc = healthCard();
    if (hc) out.push(hc);
    else if (s.statsError != null) out.push(banner(s.statsError, { kind: "error" }));
    out.push(filterRow());
    const hd = { onToggle: toggle, onSearch: searchFor };
    if (set) {
      out.push(h("p", { class: "stats-total", text: `${formatNumber(set.total ?? 0)} torrents` + (activeCount(s.filters) > 0 ? " match" : "") }));
      const cards = (set.breakdowns ?? []).filter((b) => (b.counts ?? []).length > 0 || (b.missing ?? 0) > 0)
        .map((b) => breakdownCard(b, selectedStat(s.filters, b.field), colors, hd));
      if (s.years && (s.years.counts ?? []).length > 0) cards.push(breakdownCard(s.years, selectedStat(s.filters, "year"), colors, hd));
      out.push(h("div", { class: "breakdowns" }, ...cards));
    }
    out.push(activityCard());
    if (has("crawl_sources")) {
      out.push(sourcesCard({
        sources: s.sources,
        error: s.sourcesError,
        cachedNote: s.sourcesCachedAt != null ? cachedAgo(s.sourcesCachedAt) : null,
        colors,
        crawlMap: has("crawl_map"),
        ui: sourcesUi,
        rerender: render,
      }));
    }
    return h("div", { class: "charts-tab" }, ...out);
  }

  function render() {
    const active = document.activeElement;
    const focusKey = active instanceof HTMLElement && el.contains(active) ? active.dataset.fkey : undefined;
    const openDetails = [...panel.querySelectorAll("details[open] > summary")].map((x) => x.textContent);
    tabBar.replaceChildren(...TABS.map(([key, label, path]) =>
      h("button", {
        type: "button",
        role: "tab",
        class: ["tab", tab === key && "on"],
        "aria-selected": String(tab === key),
        "aria-controls": "stats-panel",
        text: label,
        dataset: { fkey: `tab-${key}` },
        "on:click": () => {
          prefs.setRaw(prefs.KEYS.statsTab, key);
          if (tab !== key) ctx.nav(path);
        },
      })
    ));
    const colors = chartColors();
    const content = tab === "charts"
      ? chartsTab()
      : tab === "graphs"
      ? graphsTab({ hasGraphs: has("graphs"), healthAsked: s.healthAsked, healthOk: s.healthOk })
      : statusTab(s, colors);
    panel.replaceChildren(content);
    for (const d of panel.querySelectorAll("details")) {
      const sum = d.querySelector(":scope > summary");
      if (sum && openDetails.includes(sum.textContent)) d.open = true;
    }
    if (focusKey) {
      const again = el.querySelector(`[data-fkey="${CSS.escape(focusKey)}"]`);
      if (again instanceof HTMLElement) again.focus();
    }
  }

  const offPrefs = prefs.onChange((key) => {
    if (key === prefs.KEYS.showAdult) void loadBreakdowns();
  });
  const offScheme = (() => {
    try {
      const m = matchMedia("(prefers-color-scheme: dark)");
      const fn = () => render();
      m.addEventListener("change", fn);
      return () => m.removeEventListener("change", fn);
    } catch {
      return () => {};
    }
  })();

  void refresh();
  return {
    update() {
      const next = tabOf(location.pathname);
      if (next !== tab) {
        tab = next;
        prefs.setRaw(prefs.KEYS.statsTab, next);
        render();
      }
    },
    unmount() {
      life.abort();
      breakdownCall?.abort();
      seriesCall?.abort();
      offPrefs();
      offScheme();
    },
  };
}
