// The crawl map, `/crawl-map` (CrawlMapScreen.kt, feature `crawl_map`, docs/APP.md §15): DHT
// discoveries or Nostr relays per country on a canvas choropleth (sequential log scale), the top
// countries and networks. The DB-IP and Natural Earth attributions stay on screen. The world
// outline is the app's own asset, served as /_app/<V>/assets/world_110m.bin.

import { ApiError, isAborted, isOffline, userMessage } from "../api.js";
import { chartColors, lerpColor } from "../chart-colors.js";
import {
  buildMonth,
  countOf,
  countryCounts,
  countryName,
  geoReasonMessage,
  LogScale,
  MAP_LAYERS,
  MAP_WINDOWS,
  mapParams,
  metricHint,
  metricIndex,
  metricLabel,
  networkLabel,
  TOP_ROWS,
  topBy,
  totalOf,
} from "../crawlmap-model.js";
import { externalUrl, h, untrusted } from "../dom.js";
import { cachedAgo, formatAgo, formatDate } from "../format.js";
import { formatCompact, formatNumber, formatShare } from "../numfmt.js";
import { banner, progressBar } from "../ui/banner.js";
import { iconButton, pageHeader } from "../ui/shell.js";
import { countryAt, parseWorldMap } from "../worldmap.js";

/** @typedef {import("../types-stats.js").CrawlMap} CrawlMap */
/** @typedef {import("../types-stats.js").GeoCount} GeoCount */

/** @type {Promise<{map: import("../worldmap.js").WorldMap, paths: Path2D[]} | null> | null} */
let world = null;

/** The world outline, fetched once per page (it is in the service worker's shell cache). */
function loadWorld() {
  world ??= (async () => {
    try {
      const url = new URL("../../assets/world_110m.bin", import.meta.url);
      if (url.origin !== location.origin) return null;
      const res = await fetch(url.pathname, { credentials: "same-origin" });
      if (!res.ok) return null;
      const map = parseWorldMap(new Uint8Array(await res.arrayBuffer()));
      const paths = map.countries.map((c) => {
        const p = new Path2D();
        for (const r of c.rings) {
          if (r.length < 6) continue;
          p.moveTo(r[0], r[1]);
          for (let i = 1; i < r.length / 2; i++) p.lineTo(r[2 * i], r[2 * i + 1]);
          p.closePath();
        }
        return p;
      });
      return { map, paths };
    } catch {
      world = null;
      return null;
    }
  })();
  return world;
}

/**
 * @param {HTMLElement} el
 * @param {import("../types.js").ViewArgs} args
 * @returns {import("../types.js").ViewHandle}
 */
export function mount(el, { query, ctx }) {
  const p0 = mapParams(query);
  let layer = p0.layer;
  let win = p0.window;
  /** @type {string | null} */
  let metric = p0.metric;
  /** @type {CrawlMap | null} */
  let map = null;
  let loading = true;
  /** @type {ApiError | null} */
  let error = null;
  let missing = false;
  /** @type {number | null} */
  let cachedAt = null;
  /** @type {string | null} */
  let selected = null;
  let showAllCountries = false;
  let showAllNetworks = false;
  /** @type {AbortController | null} */
  let call = null;
  /** @type {Awaited<ReturnType<typeof loadWorld>>} */
  let shapes = null;
  let zoom = 1;
  let off = { x: 0, y: 0 };

  const header = pageHeader(ctx, { title: "Crawl map", back: true, actions: [iconButton("refresh", "Refresh", () => void load())] });
  const top = h("div", { class: "page-pad map-top" });
  const canvas = /** @type {HTMLCanvasElement} */ (h("canvas", { class: "map-canvas", tabindex: "0", role: "img" }));
  const mapHint = h("p", { class: "muted small" });
  const legend = h("div", { class: "map-legend" });
  const selectedHost = h("div");
  const mapCard = h("section", { class: "card map-card", "aria-label": "World map" }, h("div", { class: "map-frame" }, canvas), mapHint, legend, selectedHost);
  const lists = h("div", { class: "map-lists" });
  const attribution = h("footer", { class: "map-attribution" });
  el.append(header, top, h("div", { class: "page-pad" }, mapCard, lists), attribution);

  function syncUrl() {
    const q = new URLSearchParams({ layer, window: win });
    if (metric) q.set("metric", metric);
    try {
      history.replaceState(history.state, "", `/crawl-map?${q}`);
    } catch {
      // A sandboxed page: the URL just does not follow.
    }
  }

  const metricIdx = () => (map ? metricIndex(map, metric) : 0);

  async function load() {
    call?.abort();
    const ac = new AbortController();
    call = ac;
    loading = true;
    error = null;
    missing = false;
    render();
    const key = { layer, window: win };
    const c = await ctx.cache.get("stats/map", key);
    if (ac.signal.aborted) return;
    if (c?.json) {
      map = c.json;
      cachedAt = c.savedAt;
      render();
    }
    try {
      const m = await ctx.api.get(`/api/v1/stats/map?layer=${encodeURIComponent(layer)}&window=${encodeURIComponent(win)}`, { cls: "STATS", signal: ac.signal });
      if (isAborted(m) || ac.signal.aborted) return;
      void ctx.cache.put("stats/map", key, m);
      map = m;
      cachedAt = null;
    } catch (e) {
      if (ac.signal.aborted) return;
      const err = e instanceof ApiError ? e : new ApiError("network", String(e));
      if (err.kind === "not_found") {
        missing = true;
        map = null;
        cachedAt = null;
      } else {
        error = err;
        if (!isOffline(err)) {
          map = null;
          cachedAt = null;
        }
      }
    }
    loading = false;
    render();
  }

  function controls() {
    /** @param {string} label @param {boolean} on @param {() => void} fn @param {string} key */
    const chip = (label, on, fn, key) =>
      h("button", { type: "button", class: ["chip", on && "on"], "aria-pressed": String(on), text: label, dataset: { fkey: key }, "on:click": fn });
    const out = [
      h(
        "div",
        { class: "chips", role: "group", "aria-label": "Layer and window" },
        ...MAP_LAYERS.map((l) =>
          chip(l.label, layer === l.api, () => {
            if (layer === l.api) return;
            // Another layer has other metrics: back to its first one.
            layer = l.api;
            metric = null;
            map = null;
            cachedAt = null;
            selected = null;
            syncUrl();
            void load();
          }, `layer-${l.api}`)
        ),
        h("span", { class: "chip-sep", "aria-hidden": "true" }),
        ...MAP_WINDOWS.map((w) =>
          chip(w.label, win === w.api, () => {
            if (win === w.api) return;
            win = w.api;
            map = null;
            cachedAt = null;
            syncUrl();
            void load();
          }, `window-${w.api}`)
        ),
      ),
    ];
    if (map && (map.metrics ?? []).length > 1) {
      const name = (map.metrics ?? [])[metricIdx()];
      out.push(h(
        "div",
        { class: "chips", role: "group", "aria-label": "Metric" },
        ...(map.metrics ?? []).map((m) =>
          chip(metricLabel(m), name === m, () => {
            metric = m;
            syncUrl();
            render();
          }, `metric-${m}`)
        ),
      ));
    }
    if (map) {
      const name = (map.metrics ?? [])[metricIdx()];
      const span = win === "day" ? "last 24 hours" : (map.since ?? 0) > 0 ? `since ${formatDate(map.since ?? 0)}` : "all time";
      out.push(h("p", {
        class: "muted small",
        text: [name ? metricHint(name) : null, span, (map.computed_at ?? 0) > 0 ? `computed ${formatAgo(map.computed_at ?? 0)}` : null]
          .filter((x) => x != null).join(" · "),
      }));
    }
    return out;
  }

  function size() {
    const r = canvas.getBoundingClientRect();
    return { w: Math.max(1, r.width), h: Math.max(1, r.height) };
  }

  function clampOff() {
    const { w, h: hh } = size();
    off = { x: Math.min(0, Math.max(w * (1 - zoom), off.x)), y: Math.min(0, Math.max(hh * (1 - zoom), off.y)) };
  }

  function draw() {
    const s = shapes;
    if (!s) return;
    const { w, h: hh } = size();
    const dpr = Math.min(3, globalThis.devicePixelRatio || 1);
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(hh * dpr)) {
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(hh * dpr);
    }
    const c = canvas.getContext("2d");
    if (!c) return;
    const colors = chartColors();
    const counts = map ? countryCounts(map, metricIdx()) : new Map();
    const scale = LogScale.of(counts.values());
    const k = (w / s.map.width) * zoom;
    c.setTransform(dpr, 0, 0, dpr, 0, 0);
    c.clearRect(0, 0, w, hh);
    c.setTransform(dpr * k, 0, 0, dpr * k, dpr * off.x, dpr * off.y);
    c.lineWidth = 0.6 / k;
    c.strokeStyle = colors.mapBorder;
    s.map.countries.forEach((cs, i) => {
      const t = scale?.shade(counts.get(cs.code) ?? 0) ?? null;
      c.fillStyle = t == null ? colors.mapEmpty : lerpColor(colors.mapLow, colors.mapHigh, t);
      c.fill(s.paths[i], "evenodd");
      c.stroke(s.paths[i]);
    });
    if (selected) {
      const i = s.map.countries.findIndex((cs) => cs.code === selected);
      if (i >= 0) {
        c.lineWidth = 2 / k;
        c.strokeStyle = colors.ink;
        c.stroke(s.paths[i]);
      }
    }
    canvas.classList.toggle("zoomed", zoom > 1);
    mapHint.textContent = zoom > 1 ? "Drag to move, double-click to reset" : "Click a country · wheel or pinch to zoom";
    const topC = [...counts.entries()].sort((a, b) => b[1] - a[1]).slice(0, 5);
    canvas.setAttribute(
      "aria-label",
      `World map, ${counts.size} countries with data` + (topC.length > 0 ? "; most: " + topC.map(([cc, n]) => `${countryName(cc)} ${formatNumber(n)}`).join(", ") : ""),
    );
  }

  function renderLegend() {
    const colors = chartColors();
    const counts = map ? countryCounts(map, metricIdx()) : new Map();
    const scale = LogScale.of(counts.values());
    const empty = h("span", { class: "swatch", "aria-hidden": "true" });
    empty.style.setProperty("background", colors.mapEmpty);
    /** @type {Node[]} */
    const parts = [empty, h("span", { class: "small muted", text: " no data" })];
    if (scale) {
      const ramp = h("span", { class: "ramp", "aria-hidden": "true" });
      ramp.style.setProperty("--lo", colors.mapLow);
      ramp.style.setProperty("--hi", colors.mapHigh);
      parts.push(
        h("span", { class: "small", text: formatCompact(scale.min) }),
        ramp,
        h("span", { class: "small", text: formatCompact(scale.max) }),
        h("span", { class: "small muted", text: ` Log scale from ${formatNumber(scale.min)} to ${formatNumber(scale.max)}` }),
      );
    }
    legend.replaceChildren(...parts);
  }

  function renderSelected() {
    if (!selected || !map) {
      selectedHost.replaceChildren();
      return;
    }
    const code = selected;
    const row = (map.countries ?? []).find((c) => (c.country ?? "").toUpperCase() === code);
    selectedHost.replaceChildren(h(
      "section",
      { class: "card selected-country", "aria-label": "Selected country" },
      h(
        "div",
        { class: "card-head" },
        h("h3", { class: "card-title", text: `${countryName(code)} (${code})` }),
        h("button", {
          type: "button",
          class: "btn text",
          text: "Clear",
          "on:click": () => {
            selected = null;
            renderSelected();
            draw();
          },
        }),
      ),
      row
        ? h(
          "dl",
          { class: "kv" },
          ...(map.metrics ?? []).flatMap((m, i) => {
            const n = countOf(row, i);
            return [h("dt", { text: metricLabel(m) }), h("dd", { text: `${formatNumber(n)} (${formatShare(n, totalOf(/** @type {CrawlMap} */ (map), i))})` })];
          }),
        )
        : h("p", { class: "muted small", text: "No data" }),
    ));
  }

  /**
   * @param {string} title
   * @param {GeoCount[]} rows
   * @param {(g: GeoCount) => string} label
   * @param {boolean} all
   * @param {() => void} toggleAll
   * @param {((g: GeoCount) => void) | null} onClick
   * @param {Node | null} footer
   */
  function topList(title, rows, label, all, toggleAll, onClick, footer) {
    const colors = chartColors();
    const m = metricIdx();
    const total = map ? totalOf(map, m) : 0;
    const max = Math.max(1, ...rows.map((g) => countOf(g, m)));
    return h(
      "section",
      { class: "card", "aria-label": title },
      h("h3", { class: "card-title", text: title }),
      rows.length === 0 ? h("p", { class: "muted small", text: "No data" }) : null,
      h(
        "ul",
        { class: "bars" },
        ...(all ? rows : rows.slice(0, TOP_ROWS)).map((g) => {
          const n = countOf(g, m);
          const on = selected != null && (g.country ?? "").toUpperCase() === selected;
          const fill = h("span", { class: "bar-fill" });
          fill.style.setProperty("--w", String(Math.max(0.02, n / max)));
          fill.style.setProperty("background", on ? colors.barSelected : colors.bar);
          const content = [
            h("span", { class: "bar-label wide" }, untrusted(label(g))),
            h("span", { class: "bar-track" }, fill),
            h("span", { class: "bar-num", text: formatCompact(n) }),
            h("span", { class: "bar-share muted", text: formatShare(n, total) }),
          ];
          return h(
            "li",
            { class: "bar-row" },
            onClick
              ? h("button", { type: "button", class: ["bar-btn", on && "on"], "aria-pressed": String(on), "on:click": () => onClick(g) }, ...content)
              : h("span", { class: "bar-btn static" }, ...content),
          );
        }),
      ),
      rows.length > TOP_ROWS ? h("button", { type: "button", class: "btn text", text: all ? "Show fewer" : `Show all ${rows.length}`, "on:click": toggleAll }) : null,
      footer,
    );
  }

  function render() {
    /** @type {Node[]} */
    const t = [];
    if (loading) t.push(progressBar());
    if (missing) t.push(banner("This server has no crawl map (it needs a newer version)."));
    else if (error) {
      t.push(banner((map != null ? "Offline: " : "") + userMessage(error) + (cachedAt != null ? ` · ${cachedAgo(cachedAt)}` : ""), {
        kind: "error",
        action: { label: "Retry", onClick: () => void load() },
      }));
    } else if (cachedAt != null) t.push(banner(cachedAgo(cachedAt)));
    if (map && !map.available) t.push(banner(geoReasonMessage(map.reason)));
    t.push(...controls());
    top.replaceChildren(...t);
    if (!shapes) mapHint.textContent = "Loading the world map…";
    renderLegend();
    renderSelected();
    draw();
    /** @type {Node[]} */
    const l = [];
    if (map) {
      const m = metricIdx();
      const total = totalOf(map, m);
      const unknown = map.unknown?.[m] ?? 0;
      l.push(topList(
        "Top countries",
        topBy(map.countries ?? [], m, Infinity),
        (g) => (g.country != null ? `${countryName(g.country)} (${g.country.toUpperCase()})` : "?"),
        showAllCountries,
        () => {
          showAllCountries = !showAllCountries;
          render();
        },
        (g) => {
          if (g.country == null) return;
          const cc = g.country.toUpperCase();
          selected = selected === cc ? null : cc;
          render();
        },
        unknown > 0 ? h("p", { class: "muted small", text: `Location unknown: ${formatNumber(unknown)} (${formatShare(unknown, total)})` }) : null,
      ));
      l.push(topList(
        "Top networks (AS)",
        topBy(map.networks ?? [], m, Infinity),
        (g) => networkLabel(g) + (g.country != null ? ` · ${g.country.toUpperCase()}` : ""),
        showAllNetworks,
        () => {
          showAllNetworks = !showAllNetworks;
          render();
        },
        null,
        (map.networks ?? []).length > 0 ? h("p", { class: "muted small", text: `The server lists its top ${(map.networks ?? []).length} networks.` }) : null,
      ));
    }
    lists.replaceChildren(...l);
    const built = map?.database?.country_build != null ? buildMonth(map.database.country_build) : null;
    const dbip = externalUrl("dbip");
    attribution.replaceChildren(
      dbip ? h("a", { href: dbip }, "IP Geolocation by DB-IP") : "IP Geolocation by DB-IP",
      ` (CC BY 4.0${built != null ? `, database of ${built}` : ""}) · Map: Natural Earth (public domain)`,
    );
  }

  // --- pointer interactions --------------------------------------------------------------------
  /** @type {Map<number, {x: number, y: number}>} */
  const pointers = new Map();
  let dragged = false;
  let downAt = { x: 0, y: 0 };
  /** @param {MouseEvent} e */
  const local = (e) => {
    const r = canvas.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };
  /** @param {number} next @param {{x: number, y: number}} at */
  const zoomTo = (next, at) => {
    const z = Math.min(8, Math.max(1, next));
    off = { x: at.x - (at.x - off.x) * (z / zoom), y: at.y - (at.y - off.y) * (z / zoom) };
    zoom = z;
    clampOff();
    draw();
  };
  canvas.addEventListener("pointerdown", (e) => {
    pointers.set(e.pointerId, local(e));
    downAt = local(e);
    dragged = false;
    if (zoom > 1 || pointers.size > 1) canvas.setPointerCapture(e.pointerId);
  });
  canvas.addEventListener("pointermove", (e) => {
    const prev = pointers.get(e.pointerId);
    if (!prev) return;
    const p = local(e);
    if (pointers.size >= 2) {
      const o = [...pointers.entries()].find(([id]) => id !== e.pointerId)?.[1];
      if (o) {
        const before = Math.hypot(prev.x - o.x, prev.y - o.y);
        const after = Math.hypot(p.x - o.x, p.y - o.y);
        if (before > 0) zoomTo(zoom * (after / before), { x: (p.x + o.x) / 2, y: (p.y + o.y) / 2 });
      }
      dragged = true;
    } else if (zoom > 1) {
      if (Math.hypot(p.x - downAt.x, p.y - downAt.y) > 4) dragged = true;
      if (dragged) {
        off = { x: off.x + p.x - prev.x, y: off.y + p.y - prev.y };
        clampOff();
        draw();
      }
    } else if (Math.hypot(p.x - downAt.x, p.y - downAt.y) > 6) dragged = true;
    pointers.set(e.pointerId, p);
  });
  /** @param {PointerEvent} e */
  const up = (e) => {
    const was = pointers.size;
    pointers.delete(e.pointerId);
    if (e.type !== "pointerup" || was !== 1 || dragged || !shapes) return;
    const p = local(e);
    const base = size().w / shapes.map.width;
    const hit = countryAt(shapes.map, (p.x - off.x) / (zoom * base), (p.y - off.y) / (zoom * base));
    const cc = hit?.code ?? null;
    selected = cc == null || cc === selected ? null : cc;
    renderSelected();
    draw();
    render();
  };
  canvas.addEventListener("pointerup", up);
  canvas.addEventListener("pointercancel", up);
  canvas.addEventListener("wheel", (e) => {
    const zoomIn = e.deltaY < 0;
    // Unzoomed, scrolling down keeps scrolling the page.
    if (!zoomIn && zoom <= 1) return;
    e.preventDefault();
    zoomTo(zoom * (zoomIn ? 1.25 : 0.8), local(e));
  }, { passive: false });
  canvas.addEventListener("dblclick", () => {
    zoom = 1;
    off = { x: 0, y: 0 };
    draw();
  });
  canvas.addEventListener("keydown", (e) => {
    const { w, h: hh } = size();
    if (e.key === "+" || e.key === "=") zoomTo(zoom * 1.25, { x: w / 2, y: hh / 2 });
    else if (e.key === "-") zoomTo(zoom * 0.8, { x: w / 2, y: hh / 2 });
    else if (e.key === "0") {
      zoom = 1;
      off = { x: 0, y: 0 };
      draw();
    } else return;
    e.preventDefault();
  });
  const ro = typeof ResizeObserver === "function" ? new ResizeObserver(() => {
    clampOff();
    draw();
  }) : null;
  ro?.observe(canvas);
  const scheme = matchMedia("(prefers-color-scheme: dark)");
  const onScheme = () => render();
  scheme.addEventListener("change", onScheme);

  void loadWorld().then((w) => {
    shapes = w;
    if (!w) mapHint.textContent = "The world map could not be loaded.";
    else {
      canvas.style.setProperty("aspect-ratio", `${w.map.width} / ${w.map.height}`);
      draw();
    }
  });
  syncUrl();
  void load();
  return {
    update(_p, q) {
      const n = mapParams(q);
      if (n.layer === layer && n.window === win && n.metric === metric) return;
      layer = n.layer;
      win = n.window;
      metric = n.metric;
      map = null;
      void load();
    },
    unmount() {
      call?.abort();
      ro?.disconnect();
      scheme.removeEventListener("change", onScheme);
    },
  };
}
