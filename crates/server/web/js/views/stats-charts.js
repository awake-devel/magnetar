// The Stats charts as SVG and HTML (Charts.kt, D47, D89): breakdown bars, donuts and the year
// histogram with cross-filtering, the status bar, the activity series. Every bar, slice and
// column has its label and number written next to it or in a values list, so colour never
// carries meaning alone; chart values (publisher categories, genres) are untrusted text.

import { withAlpha } from "../chart-colors.js";
import { axisValue, ChartWindow, donutPath, seriesValue, sliceAt, slices } from "../charts.js";
import { h, svg, untrusted } from "../dom.js";
import { icon } from "../icons.js";
import { axisLabel, bucketLabel, formatCompact, formatNumber, formatShare } from "../numfmt.js";
import { DONUT_ORDER, FIELD_TITLES, MULTI_VALUED, valueLabel } from "../stats-model.js";

/** @typedef {import("../chart-colors.js").ChartColors} ChartColors */
/** @typedef {import("../types-stats.js").Breakdown} Breakdown */
/** @typedef {import("../types-stats.js").FacetCount} FacetCount */

/** Hold time of a touch long press ("Search for X"). */
export const LONG_PRESS_MS = 500;

/**
 * Label → value lines.
 * @param {readonly [string, string][]} lines
 * @param {{untrustedValues?: boolean}} [opts]
 */
export function kvLines(lines, opts = {}) {
  return h(
    "dl",
    { class: "kv" },
    ...lines.flatMap(([k, v]) => [
      h("dt", { text: k.trim(), class: k.startsWith("  ") ? "indent" : undefined }),
      h("dd", null, opts.untrustedValues ? untrusted(v) : v),
    ]),
  );
}

/**
 * A collapsible text list of a chart's values (for screen readers and a quick read).
 * @param {readonly [string, string][]} rows
 * @param {string} [summary]
 */
export function valuesList(rows, summary = "Values") {
  return h(
    "details",
    { class: "values-list" },
    h("summary", { text: summary }),
    h("ul", { class: "plain-list small" }, ...rows.map(([k, v]) => h("li", null, untrusted(k), `: ${v}`))),
  );
}

/** @param {string} color */
function dot(color) {
  const d = h("span", { class: "dot", "aria-hidden": "true" });
  d.style.setProperty("background", color);
  return d;
}

/**
 * One part of a stacked status bar.
 * @typedef {{label: string, n: number, color: string}} StatusPart
 */

/**
 * A stacked bar in the status colours, every part labelled with its number and share.
 * @param {readonly StatusPart[]} parts
 */
export function statusBar(parts) {
  const sum = parts.reduce((a, p) => a + p.n, 0);
  const bar = h("div", {
    class: "status-bar",
    role: "img",
    "aria-label": parts.map((p) => `${p.label} ${formatNumber(p.n)} (${formatShare(p.n, sum)})`).join(", "),
  });
  for (const p of parts) {
    if (p.n <= 0 || sum <= 0) continue;
    const seg = h("span", { class: "status-seg" });
    seg.style.setProperty("flex-grow", String(p.n / sum));
    seg.style.setProperty("background", p.color);
    bar.append(seg);
  }
  return h(
    "div",
    { class: "status-bar-wrap" },
    bar,
    h(
      "p",
      { class: "legend" },
      ...parts.map((p) => h("span", { class: "legend-item" }, dot(p.color), `${p.label} ${formatCompact(p.n)} (${formatShare(p.n, sum)})`)),
    ),
  );
}

/**
 * Click toggles; a touch press held LONG_PRESS_MS runs `onLong` instead.
 * @param {HTMLElement | SVGElement} el
 * @param {() => void} onClick
 * @param {() => void} onLong
 */
export function pressable(el, onClick, onLong) {
  /** @type {ReturnType<typeof setTimeout> | null} */
  let timer = null;
  let fired = false;
  const cancel = () => {
    if (timer) clearTimeout(timer);
    timer = null;
  };
  el.addEventListener("pointerdown", (e) => {
    fired = false;
    if (/** @type {PointerEvent} */ (e).pointerType !== "touch") return;
    cancel();
    timer = setTimeout(() => {
      fired = true;
      timer = null;
      onLong();
    }, LONG_PRESS_MS);
  });
  for (const ev of ["pointerup", "pointercancel", "pointerleave"]) el.addEventListener(ev, cancel);
  el.addEventListener("contextmenu", (e) => {
    if (fired) e.preventDefault();
  });
  el.addEventListener("click", (e) => {
    if (fired) {
      e.preventDefault();
      fired = false;
      return;
    }
    onClick();
  });
}

/**
 * @typedef {object} BreakdownHandlers
 * @property {(field: string, value: string) => void} onToggle cross-filter on the value
 * @property {(field: string, value: string) => void} onSearch open Search with the value
 */

/**
 * The secondary "Search for X" action of a value (focus or hover shows it).
 * @param {string} field
 * @param {string} value
 * @param {string} label
 * @param {BreakdownHandlers} hd
 */
function searchFor(field, value, label, hd) {
  return h("button", {
    type: "button",
    class: "btn icon-only search-for",
    "aria-label": `Search for ${label}`,
    title: `Search for ${label}`,
    "on:click": () => hd.onSearch(field, value),
  }, icon("search"));
}

/**
 * "Other: 9,120 in 41 more values · Unknown: 310,200".
 * @param {Breakdown} b
 * @param {string} [missingLabel]
 */
function restLine(b, missingLabel = "Unknown") {
  /** @type {string[]} */
  const parts = [];
  if ((b.other ?? 0) > 0) parts.push(`Other: ${formatNumber(b.other ?? 0)} in ${formatNumber(b.other_values ?? 0)} more values`);
  if ((b.missing ?? 0) > 0) parts.push(`${missingLabel}: ${formatNumber(b.missing ?? 0)}`);
  return parts.length > 0 ? h("p", { class: "muted small", text: parts.join(" · ") }) : null;
}

/**
 * @param {Breakdown} b
 * @param {readonly string[]} selected
 * @param {ChartColors} colors
 * @param {BreakdownHandlers} hd
 * @param {{rowExtra?: (value: string, label: string) => Node | null, missingLabel?: string}} [o]
 *   a button after a row's bar (the File kinds drill-down), the name of `missing`
 */
export function breakdownBars(b, selected, colors, hd, o = {}) {
  const counts = b.counts ?? [];
  const max = Math.max(1, ...counts.map((c) => c.count));
  if (counts.length === 0) return h("div", null, h("p", { class: "muted", text: "No values" }), restLine(b, o.missingLabel));
  return h(
    "div",
    null,
    h(
      "ul",
      { class: "bars", "aria-label": FIELD_TITLES[b.field] ?? b.field },
      ...counts.map((c) => {
        const on = selected.includes(c.value);
        const label = valueLabel(b.field, c.value);
        const fill = h("span", { class: "bar-fill" });
        fill.style.setProperty("--w", String(Math.max(0.02, c.count / max)));
        fill.style.setProperty("background", on ? colors.barSelected : colors.bar);
        const btn = h(
          "button",
          {
            type: "button",
            class: ["bar-btn", on && "on"],
            "aria-pressed": String(on),
            "aria-label": `${label}: ${formatNumber(c.count)}, ${formatShare(c.count, b.total ?? 0)}`,
            dataset: { fkey: `bar-${b.field}-${c.value}` },
          },
          h("span", { class: "bar-label" }, untrusted(label)),
          h("span", { class: "bar-track" }, fill),
          h("span", { class: "bar-num", text: formatCompact(c.count) }),
          h("span", { class: "bar-share muted", text: formatShare(c.count, b.total ?? 0) }),
        );
        pressable(btn, () => hd.onToggle(b.field, c.value), () => hd.onSearch(b.field, c.value));
        return h("li", { class: "bar-row" }, btn, searchFor(b.field, c.value, label, hd), o.rowExtra?.(c.value, label) ?? null);
      }),
    ),
    restLine(b, o.missingLabel),
  );
}

/**
 * Donut for fields with a short fixed vocabulary; the legend carries label, count and share.
 * @param {Breakdown} b
 * @param {readonly string[]} selected
 * @param {ChartColors} colors
 * @param {BreakdownHandlers} hd
 */
export function breakdownDonut(b, selected, colors, hd) {
  const order = DONUT_ORDER[b.field] ?? [];
  const parts = slices(b, order, colors);
  const sum = Math.max(1, parts.reduce((a, p) => a + p.n, 0));
  const size = 132;
  const r = size / 2 - 4;
  const ri = r * 0.55;
  let start = 0;
  /** @type {SVGElement[]} */
  const paths = [];
  for (const p of parts) {
    const sweep = (p.n / sum) * 360;
    const on = p.count != null && selected.includes(p.count.value);
    const gap = parts.length > 1 ? 1 : 0;
    const path = svg("path", {
      d: donutPath(size / 2, size / 2, on ? r + 3 : r, ri, start + gap, start + Math.max(gap * 2 + 0.5, sweep) - gap),
      fill: p.color,
      class: "slice",
    });
    if (p.count) {
      const v = p.count.value;
      pressable(path, () => hd.onToggle(b.field, v), () => hd.onSearch(b.field, v));
    }
    paths.push(path);
    start += sweep;
  }
  const chart = svg(
    "svg",
    {
      viewBox: `0 0 ${size} ${size}`,
      width: size,
      height: size,
      class: "donut",
      role: "img",
      "aria-label": `Donut of ${FIELD_TITLES[b.field] ?? b.field}; values listed beside it`,
    },
    ...paths,
    svg("text", { x: size / 2, y: size / 2 - 2, "text-anchor": "middle", class: "donut-total", text: formatCompact(b.total ?? 0) }),
    svg("text", { x: size / 2, y: size / 2 + 14, "text-anchor": "middle", class: "donut-sub", text: "total" }),
  );
  // Clicks between path edges: the angle decides.
  chart.addEventListener("click", (e) => {
    if (e.target !== chart) return;
    const rect = chart.getBoundingClientRect();
    const x = e.clientX - rect.left - rect.width / 2;
    const y = e.clientY - rect.top - rect.height / 2;
    const d = Math.hypot(x, y);
    if (d < (ri / size) * rect.width || d > (r / size) * rect.width) return;
    const i = sliceAt((Math.atan2(y, x) * 180) / Math.PI + 90, parts.map((p) => p.n));
    const c = i != null ? parts[i].count : null;
    if (c) hd.onToggle(b.field, c.value);
  });
  const legend = h(
    "ul",
    { class: "donut-legend" },
    ...parts.map((p) => {
      const c = p.count;
      const on = c != null && selected.includes(c.value);
      const label = c ? valueLabel(b.field, c.value) : "Other";
      const content = [
        dot(p.color),
        h("span", { class: "bar-label" }, untrusted(label)),
        h("span", { class: "bar-num", text: formatCompact(p.n) }),
        h("span", { class: "bar-share muted", text: formatShare(p.n, b.total ?? 0) }),
      ];
      // "Other" has no search button: a spacer of its width keeps the columns aligned.
      if (!c) {
        return h(
          "li",
          { class: "bar-row" },
          h("span", { class: "bar-btn static" }, ...content),
          h("span", { class: "search-for-spacer", "aria-hidden": "true" }),
        );
      }
      const btn = h("button", {
        type: "button",
        class: ["bar-btn", on && "on"],
        "aria-pressed": String(on),
        "aria-label": `${label}: ${formatNumber(p.n)}, ${formatShare(p.n, b.total ?? 0)}`,
        dataset: { fkey: `slice-${b.field}-${c.value}` },
      }, ...content);
      pressable(btn, () => hd.onToggle(b.field, c.value), () => hd.onSearch(b.field, c.value));
      return h("li", { class: "bar-row" }, btn, searchFor(b.field, c.value, label, hd));
    }),
  );
  return h(
    "div",
    { class: "donut-wrap" },
    chart,
    legend,
    (b.missing ?? 0) > 0 ? h("p", { class: "muted small", text: `Unknown: ${formatNumber(b.missing ?? 0)}` }) : null,
  );
}

/**
 * Years as columns in time order (a histogram: gaps stay visible).
 * @param {Breakdown} b
 * @param {readonly string[]} selected
 * @param {ChartColors} colors
 * @param {BreakdownHandlers} hd
 */
export function yearColumns(b, selected, colors, hd) {
  const years = (b.counts ?? []).filter((c) => /^-?\d{1,6}$/.test(c.value)).map((c) => /** @type {[number, FacetCount]} */ ([Number(c.value), c]))
    .sort((a, z) => a[0] - z[0]);
  if (years.length === 0) return h("p", { class: "muted", text: "No values" });
  const first = years[0][0];
  const last = years[years.length - 1][0];
  const span = Math.max(1, last - first + 1);
  const max = Math.max(1, ...years.map((y) => y[1].count));
  const W = 600;
  const H = 120;
  const w = W / span;
  const gap = w > 4 ? 1 : 0;
  const peak = years.reduce((a, y) => (y[1].count > a[1].count ? y : a));
  const label = years.map(([y, c]) => `${y}: ${formatNumber(c.count)}`).join(", ");
  const chart = svg(
    "svg",
    { viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: "none", class: "chart year-chart", role: "img", "aria-label": `Years: ${label}` },
    svg("line", { x1: 0, y1: H, x2: W, y2: H, stroke: colors.axis, "vector-effect": "non-scaling-stroke" }),
    ...years.map(([y, c]) => {
      const hh = Math.max(1, (c.count / max) * (H - 4));
      const rect = svg("rect", {
        x: Math.round(((y - first) * w + gap / 2) * 100) / 100,
        y: Math.round((H - hh) * 100) / 100,
        width: Math.max(1, Math.round((w - gap) * 100) / 100),
        height: Math.round(hh * 100) / 100,
        fill: selected.includes(c.value) ? colors.barSelected : colors.bar,
        class: "col",
      }, svg("title", { text: `${y}: ${formatNumber(c.count)}` }));
      pressable(rect, () => hd.onToggle("year", c.value), () => hd.onSearch("year", c.value));
      return rect;
    }),
  );
  const sel = years.filter(([, c]) => selected.includes(c.value));
  return h(
    "div",
    null,
    chart,
    h(
      "div",
      { class: "chart-axis" },
      h("span", { text: String(first) }),
      h("span", { text: `peak ${peak[0]}: ${formatNumber(peak[1].count)}` }),
      h("span", { text: String(last) }),
    ),
    sel.length > 0 ? h("p", { class: "strong", text: sel.map(([y, c]) => `${y}: ${formatNumber(c.count)}`).join(", ") }) : null,
    h(
      "details",
      { class: "values-list" },
      h("summary", { text: "Years" }),
      h(
        "ul",
        { class: "bars compact" },
        ...years.map(([y, c]) => {
          const on = selected.includes(c.value);
          const btn = h("button", {
            type: "button",
            class: ["bar-btn", on && "on"],
            "aria-pressed": String(on),
            dataset: { fkey: `year-${y}` },
          }, h("span", { class: "bar-label", text: String(y) }), h("span", { class: "bar-num", text: formatNumber(c.count) }));
          pressable(btn, () => hd.onToggle("year", c.value), () => hd.onSearch("year", c.value));
          return h("li", { class: "bar-row" }, btn, searchFor("year", c.value, String(y), hd));
        }),
      ),
    ),
    restLine(b),
  );
}

/**
 * One breakdown's card: title, "of N", the chart by field.
 * @param {Breakdown} b
 * @param {readonly string[]} selected
 * @param {ChartColors} colors
 * @param {BreakdownHandlers} hd
 */
export function breakdownCard(b, selected, colors, hd) {
  const chart = b.field === "year"
    ? yearColumns(b, selected, colors, hd)
    : Object.hasOwn(DONUT_ORDER, b.field)
    ? breakdownDonut(b, selected, colors, hd)
    : breakdownBars(b, selected, colors, hd);
  return h(
    "section",
    { class: "card breakdown", dataset: { field: b.field } },
    h("h3", { class: "card-title", text: FIELD_TITLES[b.field] ?? b.field }),
    h("p", { class: "muted small", text: `of ${formatNumber(b.total ?? 0)}` + (MULTI_VALUED.has(b.field) ? " · a torrent can have several" : "") }),
    chart,
  );
}

/**
 * One activity series (D47): counters as columns, gauges as a line with an area. Wheel zooms,
 * drag pans, click or arrow keys read a value. Redraws itself; `state` keeps the zoom and the
 * picked point across the page's renders.
 * @param {import("../types-stats.js").Timeseries} series
 * @param {import("../charts.js").ValueUnit} unit
 * @param {ChartColors} colors
 * @param {{window: ChartWindow | null, picked: number | null}} state
 */
export function activityChart(series, unit, colors, state) {
  const points = series.points ?? [];
  if (points.length === 0) return h("p", { class: "muted", text: "No data yet" });
  const hourly = series.bucket === "hour";
  const counter = series.kind === "counter";
  if (!state.window || state.window.n !== points.length) state.window = new ChartWindow(points.length);
  const W = 600;
  const H = 160;
  const wrap = h("div", { class: "activity" });
  const head = h("div", { class: "chart-head" });
  const axis = h("div", { class: "chart-axis" });
  const chartHost = h("div", { class: "chart-host" });
  const summary = h("p", { class: "muted small" });
  wrap.append(head, chartHost, axis, summary);

  function draw() {
    const w = /** @type {ChartWindow} */ (state.window);
    const visible = points.slice(w.first, Math.min(points.length, w.first + w.count));
    const top = Math.max(1, ...visible.map((p) => p.value));
    let bottom = counter ? 0 : Math.min(...visible.map((p) => p.value));
    if (bottom === top) bottom = 0;
    const n = visible.length;
    const cw = W / n;
    /** @param {number} v */
    const y = (v) => Math.round((H - ((v - bottom) / Math.max(1, top - bottom)) * (H - 4) - 2) * 10) / 10;
    const picked = state.picked != null && state.picked < points.length ? points[state.picked] : null;
    const latest = points[points.length - 1];
    head.replaceChildren(
      h("span", { class: "muted small", text: axisValue(top, unit) }),
      h("span", {
        class: ["small", picked && "strong"],
        role: "status",
        text: picked ? `${bucketLabel(picked.at, hourly)}: ${seriesValue(picked.value, unit)}` : `latest ${seriesValue(latest.value, unit)}`,
      }),
    );
    const sel = state.picked != null ? state.picked - w.first : -1;
    /** @type {SVGElement[]} */
    const marks = [];
    const color = colors.categorical[0];
    if (counter) {
      const gap = cw > 4 ? cw * 0.2 : 0;
      visible.forEach((p, i) => {
        const t = y(p.value);
        marks.push(svg("rect", {
          x: Math.round((i * cw + gap / 2) * 100) / 100,
          y: t,
          width: Math.max(0.5, Math.round((cw - gap) * 100) / 100),
          height: Math.max(0.5, Math.round((H - t) * 10) / 10),
          fill: i === sel ? colors.barSelected : color,
        }));
      });
    } else {
      const xs = visible.map((_, i) => Math.round((i * cw + cw / 2) * 10) / 10);
      const line = visible.map((p, i) => `${i === 0 ? "M" : "L"}${xs[i]} ${y(p.value)}`).join("");
      const area = `M${xs[0]} ${H}` + visible.map((p, i) => `L${xs[i]} ${y(p.value)}`).join("") + `L${xs[n - 1]} ${H}Z`;
      marks.push(svg("path", { d: area, fill: withAlpha(color, 0.15) }));
      marks.push(svg("path", { d: line, fill: "none", stroke: color, "stroke-width": 2.5, "vector-effect": "non-scaling-stroke" }));
    }
    if (sel >= 0 && sel < n) {
      const x = Math.round((sel * cw + cw / 2) * 10) / 10;
      marks.push(svg("line", { x1: x, y1: 0, x2: x, y2: H, stroke: withAlpha(colors.ink, 0.4), "vector-effect": "non-scaling-stroke" }));
    }
    const label = `${series.metric}: ${points.length} ${hourly ? "hours" : "days"}, latest ${seriesValue(latest.value, unit)}, highest shown ${
      seriesValue(top, unit)
    }`;
    const chart = svg(
      "svg",
      {
        viewBox: `0 0 ${W} ${H}`,
        preserveAspectRatio: "none",
        class: "chart activity-chart",
        role: "img",
        "aria-label": label,
        tabindex: 0,
      },
      svg("line", { x1: 0, y1: H - 1, x2: W, y2: H - 1, stroke: colors.axis, "vector-effect": "non-scaling-stroke" }),
      ...marks,
    );
    attach(chart);
    chartHost.replaceChildren(chart);
    axis.replaceChildren(
      h("span", { text: axisLabel(visible[0].at, hourly) }),
      h("span", { text: w.count < w.n ? `zoomed: ${w.count} of ${w.n}` : "" }),
      h("span", { text: axisLabel(visible[n - 1].at, hourly) }),
    );
    summary.textContent = label.replace(`${series.metric}: `, "") +
      (counter ? ` · ${seriesValue(points.reduce((a, p) => a + p.value, 0), unit)} in all (UTC buckets)` : " (UTC buckets)") +
      " · click for a value, wheel to zoom";
  }

  /** @param {SVGElement} chart */
  function attach(chart) {
    /** @param {number} clientX */
    const frac = (clientX) => {
      const r = chart.getBoundingClientRect();
      return r.width > 0 ? (clientX - r.left) / r.width : 0;
    };
    let dragX = /** @type {number | null} */ (null);
    let moved = false;
    chart.addEventListener("pointerdown", (e) => {
      dragX = e.clientX;
      moved = false;
    });
    chart.addEventListener("pointermove", (e) => {
      if (dragX == null || e.pointerType === "touch") return;
      const r = chart.getBoundingClientRect();
      const dx = (e.clientX - dragX) / Math.max(1, r.width);
      const w = /** @type {ChartWindow} */ (state.window);
      if (Math.abs(dx * w.count) >= 1) {
        state.window = w.pan(dx);
        dragX = e.clientX;
        moved = true;
        draw();
      }
    });
    chart.addEventListener("pointerup", (e) => {
      if (!moved) {
        state.picked = /** @type {ChartWindow} */ (state.window).indexAt(frac(e.clientX));
        draw();
      }
      dragX = null;
    });
    chart.addEventListener("wheel", (e) => {
      const w = /** @type {ChartWindow} */ (state.window);
      const zoomIn = e.deltaY < 0;
      // Unzoomed, scrolling down keeps scrolling the page.
      if (!zoomIn && w.count >= w.n) return;
      e.preventDefault();
      state.window = w.zoom(zoomIn ? 1.25 : 0.8, frac(e.clientX));
      draw();
    }, { passive: false });
    chart.addEventListener("dblclick", () => {
      state.window = new ChartWindow(points.length);
      draw();
    });
    chart.addEventListener("keydown", (e) => {
      const w = /** @type {ChartWindow} */ (state.window);
      const cur = state.picked ?? points.length - 1;
      if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
        state.picked = Math.min(points.length - 1, Math.max(0, cur + (e.key === "ArrowLeft" ? -1 : 1)));
        if (state.picked < w.first) state.window = w.pan(1 / Math.max(1, w.count));
        if (state.picked >= w.first + w.count) state.window = w.pan(-1 / Math.max(1, w.count));
      } else if (e.key === "+" || e.key === "=") state.window = w.zoom(1.5, 0.5);
      else if (e.key === "-") state.window = w.zoom(0.66, 0.5);
      else return;
      e.preventDefault();
      draw();
      const again = chartHost.querySelector("svg");
      if (again instanceof SVGElement) again.focus();
    });
  }

  draw();
  return wrap;
}
