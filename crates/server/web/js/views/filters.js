// The filter panel (FilterSheet.kt, docs/APP.md §12): live disjunctive counts next to every
// chip, chips that stay where they first appeared, then rating, years, size, date, seeders,
// "Hide dead torrents" and min trust. Two modes: "draft" (the phone sheet and the medium
// drawer: closing applies, like Back) and "live" (the wide sidebar: each change applies,
// debounced 300 ms).

import { ApiError, isAborted } from "../api.js";
import { h } from "../dom.js";
import { formatCount } from "../format.js";
import { onFeaturesChange } from "../health.js";
import { categoryLabel } from "../labels.js";
import * as prefs from "../prefs.js";
import {
  activeCount,
  buildQuery,
  CATEGORIES,
  ChipOrder,
  DATE_PRESETS,
  EMPTY_FILTERS,
  FACET_FIELDS,
  FACET_TITLES,
  facetChips,
  facetFields,
  KIND_PAIR_HINT,
  makeFilters,
  ORIGIN_CHIPS,
  PROBE_FACET_FIELDS,
  PROBE_KIND_FIELDS,
  selected,
  supportedBy,
  toggle,
} from "../search-model.js";
import { banner } from "../ui/banner.js";
import { chip, chipRow } from "../ui/chips.js";
import { openDialog } from "../ui/dialog.js";
import { slider } from "../ui/slider.js";

/** Pause after a change before the counts are asked again (and, live, before it applies). */
export const FACET_DEBOUNCE_MS = 300;
/** Values asked per field. */
export const FACET_SIZE = 40;

/** @typedef {import("../search-model.js").SearchFilters} SearchFilters */

let idCounter = 0;

/**
 * @param {{ctx: import("../types.js").Ctx, mode: "draft" | "live", initial: SearchFilters,
 *   text: () => string, onChange?: (f: SearchFilters) => void}} opts
 */
export function createFilterPanel(opts) {
  const { ctx } = opts;
  const uid = `f${++idCounter}`;
  let f = opts.initial;
  /** @type {import("../types.js").FacetSet | null} */
  let facets = null;
  let indexBuilding = false;
  const order = new ChipOrder();
  /** @type {AbortController | null} */
  let facetCall = null;
  /** @type {ReturnType<typeof setTimeout> | null} */
  let facetTimer = null;
  /** @type {ReturnType<typeof setTimeout> | null} */
  let applyTimer = null;
  /** @type {Set<() => void>} */
  const listeners = new Set();

  const feats = () => ctx.health.features();
  const can = (/** @type {string} */ x) => feats().has(x);

  // --- chips -----------------------------------------------------------------------------
  const chipsHost = h("div", { class: "filter-chips" });
  const facetBanner = h("div", { class: "facet-banner" });

  /** @param {string} name */
  const field = (name) => facets?.facets?.find((x) => x.field === name) ?? null;

  /**
   * @param {string} fieldName
   * @param {import("../search-model.js").FacetChip[]} chips
   */
  function chipButtons(fieldName, chips) {
    const sel = selected(f, fieldName);
    return order.arrange(fieldName, chips).map((c) => {
      const on = sel.includes(c.value);
      return chip({
        label: c.label,
        count: c.count,
        selected: on,
        // A value nothing would match stays in place, disabled; the adult category stays enabled
        // (picking it is how the server shows adult torrents, D52).
        disabled: !(on || c.count !== 0 || (fieldName === "category" && c.value === "porn")),
        dataset: { field: fieldName, value: c.value },
        onToggle: () => update(toggle(f, fieldName, c.value)),
      });
    });
  }

  /**
   * @param {string} title
   * @param {Node} content
   * @param {string} [level]
   */
  function section(title, content, level = "h3") {
    return h("section", { class: "filter-section" }, h(level, { class: "filter-title", text: title }), content);
  }

  function renderChips() {
    const focused = document.activeElement instanceof HTMLElement ? document.activeElement.dataset : null;
    const focusKey = focused?.field && focused?.value ? [focused.field, focused.value] : null;
    /** @type {Node[]} */
    const out = [];
    const cat = field("category");
    out.push(section(
      "Categories",
      chipRow(
        chipButtons(
          "category",
          CATEGORIES.map((c) => ({
            value: c,
            label: categoryLabel(c),
            count: cat ? (cat.counts ?? []).find((x) => x.value === c)?.count ?? 0 : null,
          })),
        ),
        "Categories",
      ),
    ));
    if (can("origin") || f.origins.length > 0) {
      const o = field("origin");
      out.push(section(
        "Source",
        chipRow(
          chipButtons(
            "origin",
            ORIGIN_CHIPS.map(([value, label]) => ({
              value,
              label,
              count: o ? (o.counts ?? []).find((x) => x.value === value)?.count ?? 0 : null,
            })),
          ),
          "Source",
        ),
      ));
    }
    for (const name of FACET_FIELDS) {
      const sel = selected(f, name);
      if (!can("filters") && sel.length === 0) continue;
      const chips = facetChips(name, sel, field(name));
      if (chips.length === 0) continue;
      const title = FACET_TITLES[name] ?? name;
      out.push(section(title, chipRow(chipButtons(name, chips), title)));
    }
    if (can("probe_filters") || PROBE_FACET_FIELDS.some((p) => selected(f, p).length > 0)) {
      const kindsOk = can("probe_filters") && can("probe_kinds");
      /** @type {[string, import("../search-model.js").FacetChip[]][]} */
      const probe = [];
      for (const name of PROBE_FACET_FIELDS) {
        const sel = selected(f, name);
        if (!can("probe_filters") && sel.length === 0) continue;
        if (PROBE_KIND_FIELDS.includes(name) && !kindsOk && sel.length === 0) continue;
        const chips = facetChips(name, sel, field(name));
        if (chips.length > 0) probe.push([name, chips]);
      }
      if (probe.length > 0) {
        out.push(h("h3", { class: "filter-group-title", text: "From the files (probed torrents only)" }));
        if (probe.some(([n]) => n === "probe_audio_kind" || n === "probe_sub_kind")) {
          out.push(h("p", { class: "field-hint", text: KIND_PAIR_HINT }));
        }
        for (const [name, chips] of probe) {
          const title = FACET_TITLES[name] ?? name;
          out.push(section(title, chipRow(chipButtons(name, chips), title)));
        }
      }
    }
    chipsHost.replaceChildren(...out);
    if (focusKey) {
      for (const b of chipsHost.querySelectorAll("button.chip")) {
        const el = /** @type {HTMLElement} */ (b);
        if (el.dataset.field === focusKey[0] && el.dataset.value === focusKey[1]) {
          el.focus();
          break;
        }
      }
    }
    facetBanner.replaceChildren(
      ...(indexBuilding
        ? [banner("The server is building its search index", {
          kind: "warn",
          action: { label: "Retry", onClick: () => askFacets(0) },
        })]
        : []),
    );
  }

  // --- counts ----------------------------------------------------------------------------
  /** @param {number} delay */
  function askFacets(delay = FACET_DEBOUNCE_MS) {
    if (facetTimer) clearTimeout(facetTimer);
    facetTimer = setTimeout(() => void loadFacets(), delay);
  }

  async function loadFacets() {
    facetCall?.abort();
    if (!can("facet_set")) {
      facets = null;
      renderChips();
      emit();
      return;
    }
    const ac = new AbortController();
    facetCall = ac;
    const body = {
      query: buildQuery(opts.text(), supportedBy(f, feats()), "relevance", true, prefs.showAdult()),
      fields: facetFields(feats()),
      size: FACET_SIZE,
    };
    try {
      const r = await ctx.api.post("/api/v1/facets", body, { cls: "SEARCH", signal: ac.signal });
      if (isAborted(r) || ac.signal.aborted) return;
      facets = r;
      indexBuilding = false;
    } catch (e) {
      if (ac.signal.aborted) return;
      // 503 while the index builds, or offline: no counts, the chips still work.
      facets = null;
      indexBuilding = e instanceof ApiError && e.kind === "index_building";
    }
    renderChips();
    emit();
  }

  // --- the rest of the panel --------------------------------------------------------------
  /**
   * @param {string} label
   * @param {string} value
   * @param {(v: string) => void} onInput
   * @param {number} [maxDigits]
   */
  function numberField(label, value, onInput, maxDigits = 12) {
    const id = `${uid}-${label.replace(/[^a-z]/gi, "").toLowerCase()}`;
    const input = /** @type {HTMLInputElement} */ (h("input", {
      type: "text",
      inputmode: "numeric",
      pattern: "[0-9]*",
      maxlength: String(maxDigits),
      id,
      class: "field-input",
      value,
      autocomplete: "off",
    }));
    input.addEventListener("input", () => {
      const clean = input.value.replace(/[^0-9]/g, "").slice(0, maxDigits);
      if (clean !== input.value) input.value = clean;
      onInput(clean);
    });
    return { el: h("div", { class: "field" }, h("label", { for: id, class: "field-label", text: label }), input), input };
  }

  /**
   * @param {string} s
   * @param {boolean} [year]
   */
  const numOf = (s, year = false) => {
    if (s === "" || (year && s.length !== 4)) return null;
    const n = Number(s);
    return Number.isSafeInteger(n) ? n : null;
  };

  const ratingTitle = h("span", { class: "filter-title" });
  const ratingReset = h("button", { type: "button", class: "btn text", text: "Reset" });
  const rating = slider({
    label: "Min rating",
    min: 0,
    max: 9.5,
    step: 0.5,
    value: f.minRating ?? 0,
    valueText: (v) => (v > 0 ? `${v.toFixed(1)} or more` : "any"),
    onInput: (v) => update(makeFilters({ ...f, minRating: v > 0 ? Math.round(v * 2) / 2 : null }), { keepInputs: true }),
  });
  ratingReset.addEventListener("click", () => update(makeFilters({ ...f, minRating: null })));
  const ratingSection = h(
    "section",
    { class: "filter-section" },
    h("div", { class: "filter-row" }, ratingTitle, ratingReset),
    rating.el,
  );

  const minYear = numberField("From", f.minYear?.toString() ?? "", (v) => update(makeFilters({ ...f, minYear: numOf(v, true) }), { keepInputs: true }), 4);
  const maxYear = numberField("To", f.maxYear?.toString() ?? "", (v) => update(makeFilters({ ...f, maxYear: numOf(v, true) }), { keepInputs: true }), 4);
  const yearSection = section("Year", h("div", { class: "field-pair" }, minYear.el, maxYear.el));

  const minSize = numberField("Min (MiB)", f.minSizeMb?.toString() ?? "", (v) => update(makeFilters({ ...f, minSizeMb: numOf(v) }), { keepInputs: true }));
  const maxSize = numberField("Max (MiB)", f.maxSizeMb?.toString() ?? "", (v) => update(makeFilters({ ...f, maxSizeMb: numOf(v) }), { keepInputs: true }));
  const sizeSection = section("Size", h("div", { class: "field-pair" }, minSize.el, maxSize.el));

  const dateHost = h("div");
  const dateSection = section("Published", dateHost);
  function renderDates() {
    dateHost.replaceChildren(chipRow(
      DATE_PRESETS.map((p) =>
        chip({
          label: p.label,
          selected: f.date === p.key,
          onToggle: () => update(makeFilters({ ...f, date: p.key })),
        })
      ),
      "Published",
    ));
  }

  const minSeeders = numberField("Min seeders", f.minSeeders?.toString() ?? "", (v) => update(makeFilters({ ...f, minSeeders: numOf(v) }), { keepInputs: true }));

  const hideDead = /** @type {HTMLInputElement} */ (h("input", {
    type: "checkbox",
    role: "switch",
    class: "switch",
    id: `${uid}-dead`,
    checked: f.hideDead,
  }));
  hideDead.addEventListener("change", () => update(makeFilters({ ...f, hideDead: hideDead.checked }), { keepInputs: true }));
  const deadRow = h("label", { class: "switch-row", for: `${uid}-dead` }, h("span", { text: "Hide dead torrents" }), hideDead);

  const trustTitle = h("span", { class: "filter-title" });
  const trustReset = h("button", { type: "button", class: "btn text", text: "Reset" });
  const serverThreshold = () => ctx.store.get().trust?.threshold ?? 20;
  const trust = slider({
    label: "Min trust",
    min: 0,
    max: 100,
    step: 1,
    value: f.minTrust ?? serverThreshold(),
    valueText: (v) => `${v} of 100`,
    onInput: (v) => update(makeFilters({ ...f, minTrust: Math.round(v) }), { keepInputs: true }),
  });
  trustReset.addEventListener("click", () => update(makeFilters({ ...f, minTrust: null })));
  const trustSection = h(
    "section",
    { class: "filter-section" },
    h("div", { class: "filter-row" }, trustTitle, trustReset),
    trust.el,
  );

  const clearAll = h("button", {
    type: "button",
    class: "btn text",
    text: "Clear all",
    "on:click": () => update(EMPTY_FILTERS),
  });

  function syncInputs() {
    minYear.input.value = f.minYear?.toString() ?? "";
    maxYear.input.value = f.maxYear?.toString() ?? "";
    minSize.input.value = f.minSizeMb?.toString() ?? "";
    maxSize.input.value = f.maxSizeMb?.toString() ?? "";
    minSeeders.input.value = f.minSeeders?.toString() ?? "";
    hideDead.checked = f.hideDead;
    rating.set(f.minRating ?? 0);
    trust.set(f.minTrust ?? serverThreshold());
  }

  function renderStatic() {
    const releases = can("filters");
    ratingSection.hidden = !(releases || f.minRating != null);
    yearSection.hidden = !(releases || f.minYear != null || f.maxYear != null);
    ratingTitle.textContent = f.minRating == null ? "Min rating: any" : `Min rating: ${f.minRating.toFixed(1)}`;
    ratingReset.hidden = f.minRating == null;
    const t = f.minTrust ?? serverThreshold();
    trustTitle.textContent = `Min trust: ${t}` + (f.minTrust == null ? " (server threshold)" : "");
    trustReset.hidden = f.minTrust == null;
    renderDates();
  }

  /**
   * @param {SearchFilters} next
   * @param {{keepInputs?: boolean}} [o]
   */
  function update(next, o = {}) {
    f = next;
    if (!o.keepInputs) syncInputs();
    renderStatic();
    renderChips();
    emit();
    if (opts.mode === "live") {
      if (applyTimer) clearTimeout(applyTimer);
      applyTimer = setTimeout(() => opts.onChange?.(f), FACET_DEBOUNCE_MS);
    } else {
      askFacets();
    }
  }

  function emit() {
    for (const l of listeners) l();
  }

  const el = h(
    "div",
    { class: ["filter-panel", `filter-${opts.mode}`] },
    opts.mode === "live"
      ? h("div", { class: "filter-panel-head" }, h("h2", { class: "filter-panel-title", text: "Filters" }), clearAll)
      : null,
    facetBanner,
    chipsHost,
    ratingSection,
    yearSection,
    sizeSection,
    dateSection,
    h("div", { class: "filter-section" }, minSeeders.el),
    deadRow,
    trustSection,
  );
  renderStatic();
  renderChips();

  const unsubHealth = ctx.health.subscribe(() => {
    renderStatic();
    renderChips();
  });
  // Mounted before the first /health answer (a first sign-in): the counts were skipped because
  // `facet_set` was not known yet; ask again once the features arrive.
  const unsubFeatures = onFeaturesChange(ctx.health, () => askFacets(0));
  const unsubStore = ctx.store.subscribe(() => renderStatic());

  return {
    el,
    /** @returns {SearchFilters} */
    filters: () => f,
    /** "Show 1,234" when the counts are known. */
    showLabel: () => (facets && typeof facets.total === "number" ? `Show ${formatCount(facets.total)}` : "Apply"),
    /** Counts again now (opening, or the search text changed in live mode). */
    refreshFacets: () => askFacets(0),
    /** @param {number} [delay] */
    refreshFacetsLater: (delay) => askFacets(delay),
    /**
     * Takes filters set elsewhere (a saved search, the URL) without applying them again.
     * @param {SearchFilters} next
     */
    setFilters(next) {
      if (activeCount(next) === activeCount(f) && JSON.stringify(next) === JSON.stringify(f)) return;
      f = next;
      syncInputs();
      renderStatic();
      renderChips();
      emit();
    },
    /** @param {() => void} fn */
    onUpdate(fn) {
      listeners.add(fn);
    },
    clearAll: () => update(EMPTY_FILTERS),
    destroy() {
      facetCall?.abort();
      if (facetTimer) clearTimeout(facetTimer);
      if (applyTimer) clearTimeout(applyTimer);
      unsubHealth();
      unsubFeatures();
      unsubStore();
    },
  };
}

/**
 * The phone sheet / medium drawer: a draft of the filters; Esc, the scrim, the close button and
 * "Show N" all apply it (as Back and swipe-down in the app).
 * @param {import("../types.js").Ctx} ctx
 * @param {{initial: SearchFilters, text: () => string, onApply: (f: SearchFilters) => void}} opts
 */
export function openFilterSheet(ctx, opts) {
  const panel = createFilterPanel({ ctx, mode: "draft", initial: opts.initial, text: opts.text });
  const d = openDialog({
    title: "Filters",
    cls: ctx.layout === "phone" ? "sheet" : "drawer",
    closeButton: true,
    body: panel.el,
    actions: [
      {
        label: "Clear all",
        kind: "text",
        onClick: () => {
          panel.clearAll();
          return false;
        },
      },
      { label: "Apply", kind: "primary" },
    ],
    onClose: () => {
      panel.destroy();
      opts.onApply(panel.filters());
    },
  });
  const show = /** @type {HTMLButtonElement | null} */ (d.footer.lastElementChild);
  panel.onUpdate(() => {
    if (show) show.textContent = panel.showLabel();
  });
  panel.refreshFacets();
  return d;
}
