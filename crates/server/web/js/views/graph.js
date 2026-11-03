// One D47 graph, `/graph?path=` (GraphScreen.kt, GraphViewModel.kt): laid out in a module
// Worker (graph-worker.js) and drawn on a canvas; wheel or pinch zoom, drag to pan, double-click
// to fit, click a node for its card; "List" shows the nodes as rows. Labels and details are
// untrusted text (canvas text cannot run anything; the list and the card use untrusted()).

import { ApiError, isAborted, isOffline, userMessage } from "../api.js";
import { chartColors, withAlpha } from "../chart-colors.js";
import { h, link, routeLink, untrusted } from "../dom.js";
import { cachedAgo } from "../format.js";
import { createLayout, fitTransform, GRAPH_PATH, graphTitle, kindLabel, kindSlot, nodeAt, subjectOf } from "../graph-layout.js";
import { formatCompact } from "../numfmt.js";
import { npub } from "../nip19.js";
import { banner, centered, progressBar } from "../ui/banner.js";
import { iconButton, pageHeader } from "../ui/shell.js";
import { policyAllowed, scriptUrl } from "../tt.js";
import { copyText } from "./detail.js";

/** @typedef {import("../types-stats.js").Graph} Graph */

/** @type {Promise<boolean> | null} */
let workerOk = null;

/** Whether this page may start its layout worker (the CSP allows the page's Trusted Types policy). */
function canUseWorker() {
  workerOk ??= policyAllowed().then((a) => a === "yes" && typeof Worker === "function");
  return workerOk;
}

/**
 * A label for canvas drawing: no bidi or control characters, at most 40 characters.
 * @param {string} s
 */
function canvasLabel(s) {
  const c = [...s].filter((ch) => {
    const cp = /** @type {number} */ (ch.codePointAt(0));
    return !(cp < 0x20 || (cp >= 0x7f && cp <= 0x9f) || cp === 0x200e || cp === 0x200f || (cp >= 0x202a && cp <= 0x202e) || (cp >= 0x2066 && cp <= 0x2069));
  });
  return c.length > 40 ? c.slice(0, 39).join("") + "…" : c.join("");
}

/**
 * @param {HTMLElement} el
 * @param {import("../types.js").ViewArgs} args
 * @returns {import("../types.js").ViewHandle}
 */
export function mount(el, { query, ctx }) {
  let path = query.get("path") ?? "";
  /** @type {Graph | null} */
  let graph = null;
  /** @type {ApiError | null} */
  let error = null;
  let missing = false;
  let loading = true;
  /** @type {number | null} */
  let cachedAt = null;
  let asList = false;
  /** @type {number | null} */
  let selected = null;
  /** @type {ArrayLike<number>} */
  let xs = [];
  /** @type {ArrayLike<number>} */
  let ys = [];
  let settled = false;
  let layoutGen = 0;
  /** @type {Worker | null} */
  let worker = null;
  let life = new AbortController();
  /** View transform; follows the layout until the user moves it. */
  let view = { scale: 1, ox: 0, oy: 0 };
  let userMoved = false;
  /** @type {Float64Array} */
  let radius = new Float64Array(0);

  const listBtn = h("button", { type: "button", class: "btn text", text: "List", "aria-pressed": "false" });
  const header = pageHeader(ctx, { title: graphTitle(path), back: true, actions: [listBtn, iconButton("refresh", "Reload", () => void load())] });
  const top = h("div", { class: "graph-top page-pad" });
  const canvas = /** @type {HTMLCanvasElement} */ (h("canvas", { class: "graph-canvas", tabindex: "0" }));
  const status = h("p", { class: "graph-status muted small", role: "status" });
  const stage = h("div", { class: "graph-stage" }, canvas, status);
  const list = h("ul", { class: "graph-list" });
  const card = h("div", { class: "graph-card-host page-pad" });
  const countLine = h("p", { class: "muted small" });
  el.append(header, top, stage, list, card);

  listBtn.addEventListener("click", () => {
    asList = !asList;
    render();
  });

  async function load() {
    life.abort();
    life = new AbortController();
    const signal = life.signal;
    missing = false;
    error = null;
    if (!GRAPH_PATH.test(path)) {
      graph = null;
      missing = true;
      loading = false;
      render();
      return;
    }
    loading = true;
    render();
    if (graph == null) {
      const c = await ctx.cache.get("graphs", { path });
      if (signal.aborted) return;
      if (c?.json && graph == null) show(c.json, c.savedAt);
    }
    try {
      const g = await ctx.api.get(`/api/v1/graphs/${path}`, { cls: "STATS", signal });
      if (isAborted(g) || signal.aborted) return;
      void ctx.cache.put("graphs", { path }, g);
      show(g, null);
    } catch (e) {
      if (signal.aborted) return;
      const err = e instanceof ApiError ? e : new ApiError("network", String(e));
      if (err.kind === "not_found") {
        graph = null;
        missing = true;
      } else {
        error = err;
        if (!isOffline(err)) graph = null;
      }
    }
    loading = false;
    render();
  }

  /**
   * @param {Graph} g
   * @param {number | null} at
   */
  function show(g, at) {
    const same = graph != null && JSON.stringify(graph) === JSON.stringify(g);
    graph = g;
    cachedAt = at;
    loading = false;
    if (!same) startLayout(g);
    render();
  }

  /** @param {Graph} g */
  function startLayout(g) {
    const gen = ++layoutGen;
    worker?.terminate();
    worker = null;
    selected = null;
    settled = false;
    userMoved = false;
    xs = [];
    ys = [];
    const nodes = g.nodes ?? [];
    const n = nodes.length;
    const maxW = Math.max(1, ...nodes.map((x) => x.weight ?? 0));
    radius = new Float64Array(n);
    for (let i = 0; i < n; i++) {
      const w = Math.sqrt(Math.max(0, nodes[i].weight ?? 0) / maxW);
      radius[i] = 5 + 13 * w;
    }
    if (n > 0 && subjectOf(g) === 0) radius[0] = Math.max(radius[0], 12);
    /** @type {[number, number][]} */
    const edges = (g.edges ?? []).map((e) => [e.source | 0, e.target | 0]);
    const pinned = subjectOf(g);
    /** @param {ArrayLike<number>} x @param {ArrayLike<number>} y @param {boolean} done */
    const progress = (x, y, done) => {
      if (gen !== layoutGen) return;
      xs = x;
      ys = y;
      settled = done;
      if (!userMoved) fit();
      draw();
      renderStatus();
    };
    void canUseWorker().then((ok) => {
      if (gen !== layoutGen) return;
      if (ok) {
        try {
          const url = new URL("../graph-worker.js", import.meta.url).pathname;
          const w = new Worker(scriptUrl(url), { type: "module" });
          worker = w;
          w.addEventListener("message", (e) => {
            const m = e.data;
            if (!m || m.id !== gen) return;
            progress(m.x, m.y, m.settled === true);
            if (m.settled) {
              w.terminate();
              if (worker === w) worker = null;
            }
          });
          w.addEventListener("error", () => {
            w.terminate();
            if (worker === w) worker = null;
            if (gen === layoutGen && !settled) layoutHere(gen, n, edges, pinned, progress);
          });
          w.postMessage({ id: gen, n, edges, pinned });
          return;
        } catch {
          // Fall through: lay out on this thread.
        }
      }
      layoutHere(gen, n, edges, pinned, progress);
    });
  }

  /**
   * The layout on the page's thread in slices (no worker: an older CSP or a dev page).
   * @param {number} gen
   * @param {number} n
   * @param {[number, number][]} edges
   * @param {number | null} pinned
   * @param {(x: ArrayLike<number>, y: ArrayLike<number>, done: boolean) => void} progress
   */
  function layoutHere(gen, n, edges, pinned, progress) {
    const l = createLayout(n, edges, pinned);
    let steps = 0;
    const tick = () => {
      if (gen !== layoutGen) return;
      let moved = Infinity;
      for (let i = 0; i < 25 && steps < 400; i++, steps++) moved = l.step();
      const done = steps >= 400 || (moved < 0.5 && steps > 50);
      progress(l.x.slice(), l.y.slice(), done);
      if (!done) setTimeout(tick, 0);
    };
    setTimeout(tick, 0);
  }

  function size() {
    const r = canvas.getBoundingClientRect();
    return { w: Math.max(1, r.width), h: Math.max(1, r.height) };
  }

  function fit() {
    const n = Math.min(graph?.nodes?.length ?? 0, xs.length);
    const { w, h: hh } = size();
    view = fitTransform(xs, ys, { n, width: w, height: hh, pad: 36 });
  }

  function draw() {
    const g = graph;
    if (!g || asList) return;
    const { w, h: hh } = size();
    const dpr = Math.min(3, globalThis.devicePixelRatio || 1);
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(hh * dpr)) {
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(hh * dpr);
    }
    const c = canvas.getContext("2d");
    if (!c) return;
    const colors = chartColors();
    c.setTransform(dpr, 0, 0, dpr, 0, 0);
    c.clearRect(0, 0, w, hh);
    const nodes = g.nodes ?? [];
    const n = Math.min(nodes.length, xs.length);
    if (n === 0) return;
    const sx = (/** @type {number} */ i) => xs[i] * view.scale + view.ox;
    const sy = (/** @type {number} */ i) => ys[i] * view.scale + view.oy;
    /** @type {Set<number>} */
    const near = new Set();
    const maxEdge = Math.max(1, ...(g.edges ?? []).map((e) => e.weight ?? 0));
    for (const e of g.edges ?? []) {
      if (selected != null && (e.source === selected || e.target === selected)) {
        near.add(e.source);
        near.add(e.target);
      }
    }
    for (const e of g.edges ?? []) {
      const a = e.source;
      const b = e.target;
      if (a < 0 || b < 0 || a >= n || b >= n) continue;
      const lit = selected != null && (a === selected || b === selected);
      c.strokeStyle = lit ? colors.ink : withAlpha(colors.ink, selected == null ? 0.25 : 0.08);
      c.lineWidth = 1 + 2 * ((e.weight ?? 0) / maxEdge) + (lit ? 1 : 0);
      c.beginPath();
      c.moveTo(sx(a), sy(a));
      c.lineTo(sx(b), sy(b));
      c.stroke();
    }
    for (let i = 0; i < n; i++) {
      const node = nodes[i];
      const dim = selected != null && i !== selected && !near.has(i);
      const col = colors.categorical[kindSlot(node.kind ?? "") % colors.categorical.length];
      c.fillStyle = dim ? withAlpha(col, 0.3) : col;
      c.beginPath();
      c.arc(sx(i), sy(i), radius[i], 0, Math.PI * 2);
      c.fill();
      if (node.score != null) {
        c.strokeStyle = withAlpha(colors.ink, dim ? 0.2 : 0.8);
        c.lineWidth = 2.5;
        c.beginPath();
        c.arc(sx(i), sy(i), radius[i] + 3, -Math.PI / 2, -Math.PI / 2 + (Math.PI * 2 * Math.min(100, Math.max(0, node.score))) / 100);
        c.stroke();
      }
      if (i === selected) {
        c.strokeStyle = colors.ink;
        c.lineWidth = 3;
        c.beginPath();
        c.arc(sx(i), sy(i), radius[i] + 6, 0, Math.PI * 2);
        c.stroke();
      }
    }
    const showAll = view.scale > 2.2 || n <= 25;
    c.font = "11px system-ui, sans-serif";
    c.textAlign = "center";
    c.textBaseline = "top";
    for (let i = 0; i < n; i++) {
      const big = radius[i] > 5 + 13 * 0.55;
      if (!(i === 0 || i === selected || near.has(i) || showAll || (big && selected == null))) continue;
      const text = canvasLabel(nodes[i].label ?? nodes[i].id ?? "");
      if (text === "") continue;
      const tw = c.measureText(text).width;
      const tx = sx(i);
      const ty = sy(i) + radius[i] + 3;
      c.fillStyle = withAlpha(colors.grid, 0.85);
      c.fillRect(tx - tw / 2 - 2, ty - 1, tw + 4, 14);
      c.fillStyle = colors.ink;
      c.fillText(text, tx, ty);
    }
  }

  function renderStatus() {
    const g = graph;
    const n = g?.nodes?.length ?? 0;
    const m = g?.edges?.length ?? 0;
    const text = `${n} nodes, ${m} links. Click a node for details, double-click to fit.` + (settled ? "" : " Laying out…");
    status.textContent = settled ? "" : "Laying out…";
    status.hidden = settled;
    canvas.setAttribute("aria-label", `${n} nodes, ${m} links. Use the List view to read them.`);
    canvas.setAttribute("role", "img");
    countLine.textContent = text.replace(" Laying out…", "");
  }

  /** @param {number} i */
  function nodeCard(i) {
    const g = /** @type {Graph} */ (graph);
    const node = (g.nodes ?? [])[i];
    if (!node) return null;
    const links = (g.edges ?? []).filter((e) => e.source === i || e.target === i).length;
    const id = node.id ?? "";
    /** @type {Node | null} */
    let action = null;
    const kind = node.kind ?? "";
    if ((kind === "torrent" || kind === "release") && /^[0-9a-fA-F]{40}$/.test(id)) {
      action = routeLink(link("torrent", { ih: id.toLowerCase() }), { class: "btn tonal" }, "Open torrent");
    } else if (kind === "media" && /^[1-9]\d{0,17}$/.test(id) && Number.isSafeInteger(Number(id))) {
      action = routeLink(link("title", { id: Number(id) }), { class: "btn tonal" }, "Open title");
    } else if (kind === "root" || kind === "follow" || kind === "publisher") {
      action = h("button", { type: "button", class: "btn tonal", text: "Copy npub", "on:click": () => void copyText(ctx, "npub", npub(id)) });
    } else if (kind === "relay" || kind === "tracker") {
      action = h("button", { type: "button", class: "btn tonal", text: "Copy URL", "on:click": () => void copyText(ctx, "URL", id) });
    }
    const dot = h("span", { class: "dot", "aria-hidden": "true" });
    dot.style.setProperty("background", chartColors().categorical[kindSlot(kind) % 8]);
    return h(
      "section",
      { class: "card node-card", "aria-label": "Selected node" },
      h(
        "div",
        { class: "card-head" },
        h("h3", { class: "card-title" }, dot, untrusted(node.label ?? id)),
        h("button", {
          type: "button",
          class: "btn text",
          text: "Close",
          "on:click": () => {
            selected = null;
            render();
          },
        }),
      ),
      h("p", { class: "muted small", text: [kindLabel(kind), node.score != null ? `trust ${node.score}` : null, `${links} links`].filter((x) => x != null).join(" · ") }),
      ...(node.detail ?? []).map((d) => h("p", { class: "small" }, untrusted(d))),
      action ? h("div", { class: "row-actions" }, action) : null,
    );
  }

  function render() {
    header.querySelector(".page-title")?.replaceChildren(graphTitle(path));
    listBtn.textContent = asList ? "Graph" : "List";
    listBtn.setAttribute("aria-pressed", String(asList));
    /** @type {Node[]} */
    const t = [];
    const g = graph;
    if (loading && !g) t.push(progressBar());
    if (missing) {
      t.push(centered("The server has no such graph (unknown item, or a server older than this app)."));
    } else if (!g && error) {
      t.push(centered(`Could not load the graph: ${userMessage(error)}`, { label: "Retry", onClick: () => void load() }));
    } else if (g && (g.nodes ?? []).length === 0) {
      t.push(centered(g.note ?? "Nothing to show yet."));
    } else if (g) {
      if (error) t.push(banner(`Offline: ${userMessage(error)}` + (cachedAt != null ? ` · ${cachedAgo(cachedAt)}` : ""), { kind: "error" }));
      else if (cachedAt != null) t.push(banner(cachedAgo(cachedAt)));
      if (g.note) t.push(banner(untrusted(g.note)));
      if (g.truncated) t.push(banner(`Only the ${(g.nodes ?? []).length} most important nodes are shown.`));
      const kinds = [...new Set((g.nodes ?? []).map((x) => x.kind ?? ""))];
      t.push(h(
        "p",
        { class: "legend" },
        ...kinds.map((k) => {
          const d = h("span", { class: "dot", "aria-hidden": "true" });
          d.style.setProperty("background", chartColors().categorical[kindSlot(k) % 8]);
          return h("span", { class: "legend-item" }, d, untrusted(kindLabel(k)));
        }),
      ));
      t.push(countLine);
    }
    top.replaceChildren(...t);
    const showGraph = g != null && (g.nodes ?? []).length > 0 && !missing;
    stage.hidden = !showGraph || asList;
    list.hidden = !showGraph || !asList;
    if (showGraph && asList) {
      const nodes = g.nodes ?? [];
      const order = nodes.map((_, i) => i).sort((a, b) => (a === 0 ? -1 : b === 0 ? 1 : (nodes[b].weight ?? 0) - (nodes[a].weight ?? 0)));
      list.replaceChildren(...order.map((i) => {
        const node = nodes[i];
        const d = h("span", { class: "dot", "aria-hidden": "true" });
        d.style.setProperty("background", chartColors().categorical[kindSlot(node.kind ?? "") % 8]);
        return h("li", null, h(
          "button",
          {
            type: "button",
            class: ["graph-row", i === selected && "on"],
            "aria-pressed": String(i === selected),
            "on:click": () => {
              selected = i;
              render();
            },
          },
          d,
          h(
            "span",
            { class: "graph-row-body" },
            h("span", { class: "block" }, untrusted(node.label ?? node.id ?? "")),
            h("span", { class: "muted small block", text: [kindLabel(node.kind ?? ""), node.score != null ? `trust ${node.score}` : null].filter((x) => x != null).join(" · ") }),
          ),
          h("span", { class: "small", text: formatCompact(node.weight ?? 0) }),
        ));
      }));
    }
    card.replaceChildren(...(g && selected != null ? [nodeCard(selected)].filter((x) => x != null) : []));
    renderStatus();
    if (showGraph && !asList) {
      if (!userMoved) fit();
      draw();
    }
  }

  // --- pointer interactions --------------------------------------------------------------------
  /** @type {Map<number, {x: number, y: number}>} */
  const pointers = new Map();
  let downAt = { x: 0, y: 0 };
  let dragged = false;
  /** @param {PointerEvent | WheelEvent | MouseEvent} e */
  const local = (e) => {
    const r = canvas.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };
  /** @param {number} factor @param {{x: number, y: number}} at */
  const zoomAt = (factor, at) => {
    const ns = Math.min(20, Math.max(0.05, view.scale * factor));
    const f = ns / view.scale;
    view = { scale: ns, ox: at.x - (at.x - view.ox) * f, oy: at.y - (at.y - view.oy) * f };
    userMoved = true;
    draw();
  };
  canvas.addEventListener("pointerdown", (e) => {
    canvas.setPointerCapture(e.pointerId);
    pointers.set(e.pointerId, local(e));
    downAt = local(e);
    dragged = false;
  });
  canvas.addEventListener("pointermove", (e) => {
    const prev = pointers.get(e.pointerId);
    if (!prev) return;
    const p = local(e);
    if (pointers.size >= 2) {
      const others = [...pointers.entries()].filter(([id]) => id !== e.pointerId).map(([, v]) => v);
      const o = others[0];
      const before = Math.hypot(prev.x - o.x, prev.y - o.y);
      const after = Math.hypot(p.x - o.x, p.y - o.y);
      if (before > 0) zoomAt(after / before, { x: (p.x + o.x) / 2, y: (p.y + o.y) / 2 });
      dragged = true;
    } else {
      if (Math.hypot(p.x - downAt.x, p.y - downAt.y) > 4) dragged = true;
      if (dragged) {
        view = { ...view, ox: view.ox + p.x - prev.x, oy: view.oy + p.y - prev.y };
        userMoved = true;
        draw();
      }
    }
    pointers.set(e.pointerId, p);
  });
  /** @param {PointerEvent} e */
  const up = (e) => {
    const was = pointers.size;
    pointers.delete(e.pointerId);
    if (e.type === "pointerup" && was === 1 && !dragged && graph) {
      const p = local(e);
      const n = Math.min(graph.nodes?.length ?? 0, xs.length);
      selected = nodeAt(p.x, p.y, xs, ys, n, view, radius, 12);
      render();
    }
  };
  canvas.addEventListener("pointerup", up);
  canvas.addEventListener("pointercancel", up);
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    zoomAt(e.deltaY < 0 ? 1.15 : 1 / 1.15, local(e));
  }, { passive: false });
  canvas.addEventListener("dblclick", () => {
    userMoved = false;
    fit();
    draw();
  });
  canvas.addEventListener("keydown", (e) => {
    const step = 40;
    if (e.key === "+" || e.key === "=") zoomAt(1.25, { x: size().w / 2, y: size().h / 2 });
    else if (e.key === "-") zoomAt(0.8, { x: size().w / 2, y: size().h / 2 });
    else if (e.key === "0") {
      userMoved = false;
      fit();
      draw();
    } else if (e.key.startsWith("Arrow")) {
      view = {
        ...view,
        ox: view.ox + (e.key === "ArrowLeft" ? step : e.key === "ArrowRight" ? -step : 0),
        oy: view.oy + (e.key === "ArrowUp" ? step : e.key === "ArrowDown" ? -step : 0),
      };
      userMoved = true;
      draw();
    } else if (e.key === "Escape" && selected != null) {
      selected = null;
      render();
    } else return;
    e.preventDefault();
  });
  const ro = typeof ResizeObserver === "function"
    ? new ResizeObserver(() => {
      if (!userMoved) fit();
      draw();
    })
    : null;
  ro?.observe(canvas);

  void load();
  return {
    update(_p, q) {
      const next = q.get("path") ?? "";
      if (next === path) return;
      path = next;
      graph = null;
      cachedAt = null;
      selected = null;
      void load();
    },
    unmount() {
      life.abort();
      layoutGen++;
      worker?.terminate();
      ro?.disconnect();
    },
  };
}
