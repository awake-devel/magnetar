// The filter panel (FilterSheet.kt, docs/APP.md §12, D140): one dropdown per filter group with
// its selection and live disjunctive counts (options stay where they first appeared), the
// active filters as removable chips above them, then rating, years, size, seeders, "Hide dead
// torrents" and min trust. Two modes: "draft" (the phone sheet and the medium drawer: closing
// applies, like Back) and "live" (the wide sidebar: each change applies, debounced 300 ms).

import { ApiError, isAborted } from "../api.js";
import { h, untrusted } from "../dom.js";
import {
  catalogue,
  fileKindChips,
  fileKindName,
  fileKindOptions,
  fileKindOrder,
  fileKindSummary,
  foldSearch,
  labelsOfFn,
  loadCatalogue,
  matchesSearch,
  toggleFileKind,
} from "../file-kinds.js";
import { formatCount } from "../format.js";
import { onFeaturesChange } from "../health.js";
import { categoryLabel, facetLabel } from "../labels.js";
import * as prefs from "../prefs.js";
import {
  activeCount,
  buildQuery,
  CATEGORIES,
  ChipOrder,
  DATE_PRESETS,
  datePreset,
  dropdownSummary,
  EMPTY_FILTERS,
  FACET_FIELDS,
  FACET_TITLES,
  facetBody,
  facetChips,
  FILE_KIND_FIELD,
  KIND_PAIR_HINT,
  LIST_FIELDS,
  makeFilters,
  ORIGIN_CHIPS,
  PROBE_FACET_FIELDS,
  PROBE_KIND_FIELDS,
  selected,
  supportedBy,
  toggle,
} from "../search-model.js";
import { banner } from "../ui/banner.js";
import { openDialog } from "../ui/dialog.js";
import { dropdown, optionRow, panelItems } from "../ui/dropdown.js";
import { slider } from "../ui/slider.js";

/** Pause after a change before the counts are asked again (and, live, before it applies). */
export const FACET_DEBOUNCE_MS = 300;
/** More options than this get a search box (File kind always has one). */
export const SEARCH_ABOVE = 12;

/** @typedef {import("../search-model.js").SearchFilters} SearchFilters */
/** @typedef {import("../search-model.js").FacetChip} FacetChip */

let idCounter = 0;

const TITLES = /** @type {Record<string, string>} */ ({ ...FACET_TITLES, category: "Category", origin: "Source", date: "Published" });

/**
 * @param {string} name facet field
 * @param {string} v
 */
const valueName = (name, v) =>
  name === "category" ? categoryLabel(v) : name === "origin" ? ORIGIN_CHIPS.find(([x]) => x === v)?.[1] ?? v : facetLabel(name, v);

/** @param {ParentNode} root @param {string} sel */
function focusIn(root, sel) {
  const el = root.querySelector(sel);
  if (el instanceof HTMLElement) el.focus({ preventScroll: true });
  return el;
}

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
  /** The open dropdown (one at a time), its search text, the expanded file-kind groups. */
  /** @type {string | null} */
  let openField = null;
  let searchText = "";
  const expanded = new Set();
  let destroyed = false;

  const feats = () => ctx.health.features();
  const can = (/** @type {string} */ x) => feats().has(x);

  // --- dropdowns ---------------------------------------------------------------------------
  const ddHost = h("div", { class: "filter-dds" });
  const facetBanner = h("div", { class: "facet-banner" });
  /** @param {string} name */
  const field = (name) => facets?.facets?.find((x) => x.field === name) ?? null;
  const q = (/** @type {string} */ s) => CSS.escape(s);

  /**
   * @param {string | null} name null closes
   * @param {boolean} focusFirst
   */
  function setOpen(name, focusFirst) {
    const was = openField;
    openField = name;
    searchText = "";
    renderDropdowns();
    if (name == null) {
      if (focusFirst && was) focusIn(ddHost, `.dd-trigger[data-dd="${q(was)}"]`);
      return;
    }
    const wrap = ddHost.querySelector(`[data-dd-wrap="${q(name)}"]`);
    wrap?.querySelector(".dd-trigger")?.scrollIntoView({ block: "nearest" });
    const panel = wrap?.querySelector(".dd-panel");
    if (focusFirst && panel) panelItems(panel)[0]?.focus();
  }

  // Focus leaving the open dropdown closes it; checked after the event, as a render in between
  // puts the focus back on the same item. Focus on nothing (a press on plain text) is no move;
  // a press on another trigger: its click switches (closing first would move it from the pointer).
  ddHost.addEventListener("focusout", () =>
    setTimeout(() => {
      const a = document.activeElement;
      if (openField == null || destroyed || a == null || a === document.body || a.matches(".dd-trigger:active")) return;
      const wrap = ddHost.querySelector(`[data-dd-wrap="${q(openField)}"]`);
      if (wrap && !wrap.contains(a)) setOpen(null, false);
    }, 0));

  /**
   * @param {string} name
   * @param {string} summary
   * @param {boolean} has something is selected
   * @param {() => void} onClear
   * @param {() => Node[]} rows
   * @param {boolean} [search]
   * @param {string | null} [note]
   */
  function dd(name, summary, has, onClear, rows, search = false, note = null) {
    const open = openField === name;
    return dropdown({
      uid,
      field: name,
      title: TITLES[name] ?? name,
      summary,
      open,
      selection: has,
      note,
      search: open && search ? searchText : null,
      rows: open ? rows() : [],
      onOpen: (k) => setOpen(name, k),
      onClose: (k) => setOpen(null, k),
      onClear,
      onSearch: (t) => {
        searchText = t;
        renderDropdowns();
      },
      onExpand: (g, on) => {
        if (on) expanded.add(g);
        else expanded.delete(g);
        renderDropdowns();
      },
    });
  }

  const empty = (/** @type {string} */ text) => [h("p", { class: "muted small dd-empty", text })];

  /**
   * A multi-select of facet `name`.
   * @param {string} name
   * @param {FacetChip[]} chips
   */
  function listDd(name, chips) {
    const sel = selected(f, name);
    const arranged = order.arrange(name, chips);
    const search = arranged.length > SEARCH_ABOVE;
    const label = (/** @type {string} */ v) => arranged.find((c) => c.value === v)?.label ?? valueName(name, v);
    return dd(name, dropdownSummary(sel, arranged.map((c) => c.value), label), sel.length > 0, () => setList(name, []), () => {
      const fq = search ? foldSearch(searchText) : "";
      const rows = arranged.filter((c) => matchesSearch(fq, [c.label, c.value])).map((c) => {
        const on = sel.includes(c.value);
        return optionRow({
          field: name,
          value: c.value,
          label: c.label,
          count: c.count,
          checked: on,
          // A value nothing would match stays in place, disabled; the adult category stays
          // enabled (picking it is how the server shows adult torrents, D52).
          disabled: !(on || c.count !== 0 || (name === "category" && c.value === "porn")),
          onChange: () => update(toggle(f, name, c.value)),
        });
      });
      return rows.length > 0 ? rows : empty("No match");
    }, search);
  }

  /** Published: one choice; picking closes the dropdown. */
  function dateDd() {
    const p = datePreset(f.date);
    return dd("date", p.key === "any" ? "Any" : p.label, p.key !== "any", () => update(makeFilters({ ...f, date: "any" })), () =>
      DATE_PRESETS.map((x) =>
        optionRow({
          field: "date",
          value: x.key,
          label: x.label,
          checked: f.date === x.key,
          radio: `${uid}-date`,
          onChange: () => {
            openField = null;
            update(makeFilters({ ...f, date: x.key }));
            focusIn(ddHost, `.dd-trigger[data-dd="date"]`);
          },
        })
      ));
  }

  /** File kind (D137): group rows with their labels; a group stands for all of its labels. */
  function fileKindDd() {
    const sel = f.probeFileKinds;
    const fc = field(FILE_KIND_FIELD);
    const counts = fc ? fc.counts ?? [] : null;
    const cat = catalogue();
    const labelsOf = labelsOfFn(cat, counts);
    const all = fileKindOptions(sel, counts, cat, "", order);
    const pick = (/** @type {string} */ v) => update(makeFilters({ ...f, probeFileKinds: toggleFileKind(sel, v, labelsOf) }));
    const rows = () => {
      const searching = foldSearch(searchText) !== "";
      const groups = searching ? fileKindOptions(sel, counts, cat, searchText, order) : all;
      if (groups.length === 0) return empty(searching ? "No match" : "No probed files yet");
      return groups.map((g) => {
        const isOpen = g.matched || expanded.has(g.id);
        const head = h(
          "div",
          { class: "dd-group-row" },
          optionRow({
            field: FILE_KIND_FIELD,
            value: g.id,
            group: g.id,
            label: g.name,
            count: g.count,
            checked: g.state === "checked",
            mixed: g.state === "mixed",
            disabled: g.count === 0 && g.state === "unchecked",
            cls: "dd-group",
            onChange: () => pick(g.id),
          }),
          g.matched ? null : h("button", {
            type: "button",
            class: "btn icon-only dd-expand",
            "aria-expanded": String(isOpen),
            "aria-label": `${g.name} kinds`,
            dataset: { group: g.id, fkey: `exp:${g.id}` },
            "on:click": () => {
              if (isOpen) expanded.delete(g.id);
              else expanded.add(g.id);
              renderDropdowns();
            },
          }, untrusted("›")),
        );
        if (!isOpen) return head;
        const labels = g.labels.map((l) =>
          optionRow({
            field: FILE_KIND_FIELD,
            value: l.id,
            label: l.name,
            hint: l.extensions.slice(0, 4).map((e) => `.${e}`).join(" "),
            count: l.count,
            checked: l.checked,
            disabled: l.count === 0 && !l.checked,
            onChange: () => pick(l.id),
          })
        );
        return h(
          "div",
          null,
          head,
          h("div", { class: "dd-sub", role: "group", "aria-label": `${g.name} kinds` }, ...(labels.length > 0 ? labels : empty("No kind with a count"))),
        );
      });
    };
    const summary = fileKindSummary(sel, fileKindOrder(all), labelsOf, (v) => fileKindName(v, cat));
    return dd(FILE_KIND_FIELD, summary, sel.length > 0, () => setList(FILE_KIND_FIELD, []), rows, true, can("probe_file_kinds") ? null : "Needs a newer server");
  }

  /**
   * @param {string} name facet field
   * @param {readonly string[]} values
   */
  function setList(name, values) {
    const row = LIST_FIELDS.find((r) => r[1] === name);
    if (row) update(makeFilters({ ...f, [row[0]]: values }));
  }

  /**
   * The active list values as removable chips, in dropdown order: one per value, except many
   * labels of one file-kind group, which are one chip (D140 C3).
   */
  function activeRow() {
    const kindName = (/** @type {string} */ v) => fileKindName(v, catalogue());
    const fc = field(FILE_KIND_FIELD);
    const labelsOf = labelsOfFn(catalogue(), fc ? fc.counts ?? [] : null);
    const chips = ["category", "origin", ...FACET_FIELDS, FILE_KIND_FIELD, ...PROBE_FACET_FIELDS].flatMap((name) => {
      const sel = selected(f, name);
      const items = name === FILE_KIND_FIELD
        ? fileKindChips(sel, labelsOf, kindName)
        : sel.map((v) => ({ values: [v], label: valueName(name, v) }));
      return items.map(({ values, label }) => {
        const key = `${name}:${values.join(",")}`;
        return h("button", {
          type: "button",
          class: "chip on",
          "aria-label": `Remove ${TITLES[name]}: ${label}`,
          title: `${TITLES[name]}: ${label}`,
          dataset: { active: key, fkey: `act:${key}` },
          "on:click": () => setList(name, selected(f, name).filter((x) => !values.includes(x))),
        }, untrusted(label), h("span", { "aria-hidden": "true", text: " ×" }));
      });
    });
    return chips.length > 0 ? h("div", { class: "chips filter-active", role: "group", "aria-label": "Active filters" }, ...chips) : null;
  }

  /** @param {string} name @param {readonly (readonly [string, string])[]} values */
  const fixed = (name, values) => {
    const c = field(name);
    return values.map(([value, label]) => ({ value, label, count: c ? (c.counts ?? []).find((x) => x.value === value)?.count ?? 0 : null }));
  };

  function renderDropdowns() {
    const a = document.activeElement;
    const focusKey = a instanceof HTMLElement && ddHost.contains(a) ? a.dataset.fkey : undefined;
    const caret = a instanceof HTMLInputElement && a.type === "search" ? [a.selectionStart, a.selectionEnd] : null;
    const scrollTop = ddHost.querySelector(".dd-panel:not([hidden])")?.scrollTop ?? 0;
    /** @type {(Node | null)[]} */
    const out = [activeRow(), listDd("category", fixed("category", CATEGORIES.map((c) => [c, categoryLabel(c)])))];
    if (can("origin") || f.origins.length > 0) out.push(listDd("origin", fixed("origin", ORIGIN_CHIPS)));
    for (const name of FACET_FIELDS) {
      const sel = selected(f, name);
      const chips = can("filters") || sel.length > 0 ? facetChips(name, sel, field(name)) : [];
      if (chips.length > 0) out.push(listDd(name, chips));
    }
    out.push(dateDd());
    const probe = can("probe_file_kinds") || f.probeFileKinds.length > 0 ? [fileKindDd()] : [];
    const kindsOk = can("probe_filters") && can("probe_kinds");
    let kindShown = false;
    for (const name of PROBE_FACET_FIELDS) {
      const sel = selected(f, name);
      if ((!can("probe_filters") || (PROBE_KIND_FIELDS.includes(name) && !kindsOk)) && sel.length === 0) continue;
      const chips = facetChips(name, sel, field(name));
      if (chips.length === 0) continue;
      kindShown ||= name === "probe_audio_kind" || name === "probe_sub_kind";
      probe.push(listDd(name, chips));
    }
    if (probe.length > 0) {
      out.push(h("h3", { class: "filter-group-title", text: "From the files (probed torrents only)" }));
      if (kindShown) out.push(h("p", { class: "field-hint", text: KIND_PAIR_HINT }));
      out.push(...probe);
    }
    ddHost.replaceChildren(...out.filter((x) => x != null));
    if (openField != null && !ddHost.querySelector(`[data-dd-wrap="${q(openField)}"]`)) openField = null;
    const panel = ddHost.querySelector(".dd-panel:not([hidden])");
    if (panel) panel.scrollTop = scrollTop;
    // Focus back on the same item, the caret too; a removed chip or a clear goes to its trigger.
    if (focusKey) {
      const again = focusIn(ddHost, `[data-fkey="${q(focusKey)}"]`);
      if (again instanceof HTMLInputElement && caret) again.setSelectionRange(caret[0], caret[1]);
      else if (!again && /^(act|ddc):/.test(focusKey)) focusIn(ddHost, `.dd-trigger[data-dd="${q(focusKey.split(":")[1])}"]`);
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
      renderDropdowns();
      emit();
      return;
    }
    const ac = new AbortController();
    facetCall = ac;
    const body = facetBody(buildQuery(opts.text(), supportedBy(f, feats()), "relevance", true, prefs.showAdult()), feats());
    try {
      const r = await ctx.api.post("/api/v1/facets", body, { cls: "SEARCH", signal: ac.signal });
      if (isAborted(r) || ac.signal.aborted) return;
      facets = r;
      indexBuilding = false;
    } catch (e) {
      if (ac.signal.aborted) return;
      // 503 while the index builds, or offline: no counts, the dropdowns still work.
      facets = null;
      indexBuilding = e instanceof ApiError && e.kind === "index_building";
    }
    renderDropdowns();
    emit();
  }

  /** The File kind names, once per server version. */
  function askCatalogue() {
    if (!can("probe_file_kinds")) return;
    void loadCatalogue(ctx).then(() => {
      if (!destroyed) renderDropdowns();
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
  }

  /**
   * @param {SearchFilters} next
   * @param {{keepInputs?: boolean}} [o]
   */
  function update(next, o = {}) {
    f = next;
    if (!o.keepInputs) syncInputs();
    renderStatic();
    renderDropdowns();
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
    ddHost,
    ratingSection,
    yearSection,
    sizeSection,
    h("div", { class: "filter-section" }, minSeeders.el),
    deadRow,
    trustSection,
  );
  renderStatic();
  renderDropdowns();

  const unsubHealth = ctx.health.subscribe(() => {
    renderStatic();
    renderDropdowns();
  });
  // Mounted before the first /health answer (a first sign-in): the counts were skipped because
  // `facet_set` was not known yet; ask again once the features arrive.
  const unsubFeatures = onFeaturesChange(ctx.health, () => {
    askCatalogue();
    askFacets(0);
  });
  askCatalogue();
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
      renderDropdowns();
      emit();
    },
    /** @param {() => void} fn */
    onUpdate(fn) {
      listeners.add(fn);
    },
    clearAll: () => update(EMPTY_FILTERS),
    destroy() {
      destroyed = true;
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
