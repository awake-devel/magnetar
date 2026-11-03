// File kinds from header probes (D137, D140): sniff's catalogue (`GET /api/v1/probe/file-kinds`,
// cached per server version), names, and the File kind selection rules. A selection holds group
// ids ("video") and label ids ("video.mkv"); a group stands for all of its labels.

/**
 * @typedef {{id: string, name: string, extensions: readonly string[], group: string}} FileKindLabel
 * @typedef {{id: string, name: string, labels: readonly FileKindLabel[]}} FileKindGroup
 * @typedef {{groups: readonly FileKindGroup[], group: ReadonlyMap<string, FileKindGroup>,
 *   label: ReadonlyMap<string, FileKindLabel>}} Catalogue
 * @typedef {readonly {value: string, count: number}[] | null | undefined} Counts
 * @typedef {(group: string) => readonly string[]} LabelsOf
 */

export const FILE_KIND_FIELD = "probe_file_kind";
/** Values asked for the file-kind facet (`sizes`, D137 A4): every group and label with a count. */
export const FILE_KIND_FACET_SIZE = 400;
const ID_RE = /^[a-z0-9_][a-z0-9_.+-]{0,63}$/;

/** @param {string} v the part before the first dot ("exe.pe.dotnet" → "exe") */
export const groupOf = (v) => v.split(".", 1)[0];
/** @param {string} v */
export const isGroup = (v) => !v.includes(".");
/** @param {string} s */
const cap = (s) => (s === "" ? s : s[0].toUpperCase() + s.slice(1));
/** @param {unknown} v */
const str = (v) => (typeof v === "string" ? v : "");

/**
 * The catalogue of an API answer, or null when it is not one.
 * @param {any} raw
 * @returns {Catalogue | null}
 */
export function parseCatalogue(raw) {
  if (!Array.isArray(raw?.groups)) return null;
  /** @type {Map<string, FileKindGroup>} */
  const group = new Map();
  /** @type {Map<string, FileKindLabel>} */
  const label = new Map();
  for (const g of raw.groups) {
    const id = str(g?.id);
    if (!ID_RE.test(id) || !isGroup(id) || group.has(id)) continue;
    /** @type {FileKindLabel[]} */
    const labels = [];
    for (const l of Array.isArray(g.labels) ? g.labels : []) {
      const lid = str(l?.id);
      if (!ID_RE.test(lid) || isGroup(lid) || groupOf(lid) !== id || label.has(lid)) continue;
      const ext = Array.isArray(l.extensions) ? l.extensions.filter((/** @type {unknown} */ e) => typeof e === "string") : [];
      const fl = { id: lid, name: str(l.name).trim() || lid, extensions: ext, group: id };
      labels.push(fl);
      label.set(lid, fl);
    }
    group.set(id, { id, name: str(g.name).trim() || cap(id), labels });
  }
  return group.size > 0 ? { groups: [...group.values()], group, label } : null;
}

/** @type {Catalogue | null} */
let current = null;
/** @type {{version: string, promise: Promise<Catalogue | null>} | null} */
let pending = null;

/** The catalogue loaded in this page, or null. */
export const catalogue = () => current;
/** @param {Catalogue | null} c */
export function setCatalogue(c) {
  current = c;
}

/**
 * Loads the catalogue once per server version (page cache first). Nothing is asked without
 * `probe_file_kinds`; null when nothing had it (callers then use the facet's values, D137 A8).
 * @param {{api: {get: (path: string, opts?: any) => Promise<any>}, cache: {get: (e: string, k: unknown) => Promise<{json: unknown} | null>,
 *   put: (e: string, k: unknown, v: unknown) => Promise<void>}, health: {has: (f: string) => boolean, version: () => string}}} ctx
 * @returns {Promise<Catalogue | null>}
 */
export function loadCatalogue(ctx) {
  if (!ctx.health.has("probe_file_kinds")) return Promise.resolve(current);
  const version = ctx.health.version();
  if (pending?.version === version) return pending.promise;
  const key = { version };
  const promise = (async () => {
    const cached = parseCatalogue((await ctx.cache.get("probe/file-kinds", key).catch(() => null))?.json);
    if (cached) return (current = cached);
    try {
      const raw = await ctx.api.get("/api/v1/probe/file-kinds", { cls: "DETAIL" });
      const c = parseCatalogue(raw);
      if (c) {
        current = c;
        void ctx.cache.put("probe/file-kinds", key, raw);
      }
    } catch {
      if (pending?.version === version) pending = null; // asked again next time
    }
    return current;
  })();
  pending = { version, promise };
  return promise;
}

/**
 * The catalogue's name; without it a group id with a capital, a label's raw id (D137 A8).
 * @param {string} v
 * @param {Catalogue | null} [cat]
 */
export function fileKindName(v, cat = current) {
  return isGroup(v) ? cat?.group.get(v)?.name ?? cap(v) : cat?.label.get(v)?.name ?? v;
}

/**
 * A group's label ids: the catalogue's, else those in the current counts.
 * @param {Catalogue | null} cat
 * @param {Counts} counts
 * @returns {LabelsOf}
 */
export function labelsOfFn(cat, counts) {
  return (g) => {
    const c = cat?.group.get(g)?.labels.map((l) => l.id) ?? [];
    return c.length > 0 ? c : (counts ?? []).map((x) => x.value).filter((v) => !isGroup(v) && groupOf(v) === g);
  };
}

/**
 * Lower-case, trim, dedupe; a selected group drops its labels; a group whose every label is
 * selected becomes the group; sorted (D140 C1).
 * @param {readonly string[]} sel
 * @param {LabelsOf} [labelsOf]
 */
export function normalizeFileKinds(sel, labelsOf = () => []) {
  const all = new Set(sel.map((v) => v.trim().toLowerCase()).filter((v) => v !== ""));
  const set = new Set([...all].filter((v) => isGroup(v) || !all.has(groupOf(v))));
  for (const g of new Set([...set].filter((v) => !isGroup(v)).map(groupOf))) {
    const ls = labelsOf(g);
    if (ls.length > 0 && ls.every((l) => set.has(l))) {
      for (const l of ls) set.delete(l);
      set.add(g);
    }
  }
  return [...set].sort();
}

/**
 * Checkbox state of a group header.
 * @param {readonly string[]} sel
 * @param {string} g
 * @returns {"checked" | "mixed" | "unchecked"}
 */
export function fileKindState(sel, g) {
  if (sel.includes(g)) return "checked";
  return sel.some((v) => !isGroup(v) && groupOf(v) === g) ? "mixed" : "unchecked";
}

/**
 * A click on a group or a label, with real checkbox semantics (unchecking one label of a checked
 * group leaves the others); always normalised.
 * @param {readonly string[]} sel
 * @param {string} value
 * @param {LabelsOf} [labelsOf]
 */
export function toggleFileKind(sel, value, labelsOf = () => []) {
  const v = value.trim().toLowerCase();
  const cur = normalizeFileKinds(sel, labelsOf);
  const g = groupOf(v);
  if (v !== "" && !isGroup(v) && cur.includes(g)) {
    return normalizeFileKinds([...cur.filter((x) => x !== g), ...labelsOf(g).filter((l) => l !== v)], labelsOf);
  }
  return normalizeFileKinds(cur.includes(v) ? cur.filter((x) => x !== v) : [...cur, v], labelsOf);
}

/** More labels of one group than this are shown as one chip (D140 C3). */
export const KIND_CHIP_MAX = 3;

/**
 * The selection as removable chips (D140 C3): a group is one chip, up to KIND_CHIP_MAX labels of a
 * group are one chip each, more are one chip for the group: "{group} except {the 1–2 labels left
 * out}" or "{group}: {n} kinds". Unticking one label of a checked group leaves every other
 * catalogue label selected; this keeps that one chip instead of one per label.
 * @param {readonly string[]} sel a normalised selection
 * @param {LabelsOf} labelsOf
 * @param {(v: string) => string} name
 * @returns {{values: string[], label: string}[]}
 */
export function fileKindChips(sel, labelsOf, name) {
  /** @type {{values: string[], label: string}[]} */
  const out = [];
  for (const g of [...new Set(sel.map(groupOf))].sort()) {
    if (sel.includes(g)) out.push({ values: [g], label: name(g) });
    const labels = sel.filter((v) => !isGroup(v) && groupOf(v) === g).sort();
    if (labels.length <= KIND_CHIP_MAX) {
      for (const l of labels) out.push({ values: [l], label: name(l) });
      continue;
    }
    const left = labelsOf(g).filter((l) => !labels.includes(l));
    const label = left.length > 0 && left.length <= 2
      ? `${name(g)} except ${left.map(name).join(", ")}`
      : `${name(g)}: ${labels.length} kinds`;
    out.push({ values: labels, label });
  }
  return out;
}

/**
 * The collapsed File kind text: the chips' labels as `dropdownSummary` would put them ("Any",
 * the one chip, or "{first in option order}, +{n-1}").
 * @param {readonly string[]} sel
 * @param {readonly string[]} optionOrder
 * @param {LabelsOf} labelsOf
 * @param {(v: string) => string} name
 */
export function fileKindSummary(sel, optionOrder, labelsOf, name) {
  const chips = fileKindChips(sel, labelsOf, name);
  if (chips.length === 0) return "Any";
  const at = (/** @type {{values: string[]}} */ c) => {
    const i = Math.min(...c.values.map((v) => optionOrder.indexOf(v)).filter((i) => i >= 0));
    return Number.isFinite(i) ? i : 1e9;
  };
  const first = [...chips].sort((a, b) => at(a) - at(b))[0];
  return chips.length === 1 ? first.label : `${first.label}, +${chips.length - 1}`;
}

/** @param {string} s case- and diacritics-free ("Fré" → "fre") */
export const foldSearch = (s) => s.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase().trim();

/**
 * Any of `texts` contains the folded query; "" matches.
 * @param {string} folded
 * @param {readonly string[]} texts
 */
export const matchesSearch = (folded, texts) => folded === "" || texts.some((t) => foldSearch(t).includes(folded));

/**
 * @typedef {{id: string, name: string, count: number | null, checked: boolean, extensions: readonly string[]}} KindOption
 * @typedef {{id: string, name: string, count: number | null, state: "checked" | "mixed" | "unchecked",
 *   labels: KindOption[], matched: boolean}} GroupOption
 * @typedef {{arrange: (field: string, chips: {value: string, label: string, count: number | null}[],
 *   labelOf?: (v: string) => string) => {value: string}[]}} Order
 */

/**
 * The File kind options. With counts: groups by count (catalogue order on ties), labels with a
 * count or selected; without: every catalogue group and label. A search keeps the groups that
 * match, or the matching labels of the others (`matched`: shown expanded).
 * @param {readonly string[]} sel
 * @param {Counts} counts null: no counts
 * @param {Catalogue | null} cat
 * @param {string} [query]
 * @param {Order | null} [order] keeps values where they first appeared
 * @returns {GroupOption[]}
 */
export function fileKindOptions(sel, counts, cat, query = "", order = null) {
  const counted = counts != null;
  const n = new Map((counts ?? []).map((c) => [c.value, c.count]));
  const name = (/** @type {string} */ v) => fileKindName(v, cat);
  /**
   * @param {Set<string>} ids
   * @param {readonly {id: string}[]} known catalogue order
   * @param {string} key
   */
  const sorted = (ids, known, key) => {
    const at = (/** @type {string} */ id) => {
      const i = known.findIndex((k) => k.id === id);
      return i < 0 ? 1e9 : i;
    };
    const out = [...ids].sort((a, b) => (counted ? (n.get(b) ?? 0) - (n.get(a) ?? 0) : 0) || at(a) - at(b) || a.localeCompare(b));
    if (!order || !counted) return out;
    return order.arrange(key, out.map((v) => ({ value: v, label: name(v), count: n.get(v) ?? 0 })), name).map((c) => c.value);
  };
  /** @param {(v: string) => boolean} keep */
  const ids = (keep, /** @type {readonly {id: string}[]} */ fromCat) =>
    new Set([...(counted ? [...n].filter(([v, c]) => c > 0 && keep(v)).map(([v]) => v) : fromCat.map((x) => x.id)), ...sel.filter(keep)]);
  const folded = foldSearch(query);
  /** @type {GroupOption[]} */
  const out = [];
  const groups = cat?.groups ?? [];
  for (const g of sorted(new Set([...ids(isGroup, groups), ...sel.map(groupOf)]), groups, `${FILE_KIND_FIELD}:`)) {
    const known = cat?.group.get(g)?.labels ?? [];
    const hit = folded !== "" && matchesSearch(folded, [name(g), g]);
    const labels = sorted(ids((v) => !isGroup(v) && groupOf(v) === g, known), known, `${FILE_KIND_FIELD}:${g}`)
      .map((id) => ({ id, name: name(id), count: counted ? n.get(id) ?? 0 : null, checked: sel.includes(id) || sel.includes(g), extensions: cat?.label.get(id)?.extensions ?? [] }))
      .filter((l) => hit || matchesSearch(folded, [l.name, l.id, ...l.extensions]));
    if (folded !== "" && !hit && labels.length === 0) continue;
    out.push({ id: g, name: name(g), count: counted ? n.get(g) ?? 0 : null, state: fileKindState(sel, g), labels, matched: folded !== "" });
  }
  return out;
}

/**
 * Option values in shown order (a group, then its labels), for the collapsed summary.
 * @param {readonly GroupOption[]} groups
 */
export const fileKindOrder = (groups) => groups.flatMap((g) => [g.id, ...g.labels.map((l) => l.id)]);
