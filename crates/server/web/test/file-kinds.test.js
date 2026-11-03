// File kinds (D137, D140 C1): the selection vectors shared with the app's FileKindsTest, the
// collapsed summary, search matching, the catalogue and its fallback names.
import assert from "node:assert/strict";
import {
  catalogue,
  fileKindName,
  fileKindOptions,
  fileKindChips,
  fileKindOrder,
  fileKindState,
  fileKindSummary,
  foldSearch,
  labelsOfFn,
  loadCatalogue,
  matchesSearch,
  normalizeFileKinds,
  parseCatalogue,
  setCatalogue,
  toggleFileKind,
} from "../js/file-kinds.js";
import { facetLabel } from "../js/labels.js";
import { ChipOrder, dropdownSummary } from "../js/search-model.js";

const RAW = {
  groups: [
    {
      id: "video",
      name: "Video",
      labels: [
        { id: "video.mkv", name: "Matroska video", extensions: ["mkv", "mk3d"] },
        { id: "video.mp4", name: "MPEG-4 video", extensions: ["mp4", "m4v"] },
        { id: "video.avi", name: "AVI video", extensions: ["avi"] },
      ],
    },
    {
      id: "exe",
      name: "Executables",
      labels: [
        { id: "exe.pe", name: "Windows PE executable", extensions: ["exe", "dll"] },
        { id: "exe.pe.dotnet", name: ".NET assembly", extensions: [] },
        { id: "exe.elf", name: "ELF executable", extensions: [] },
      ],
    },
    {
      id: "doc",
      name: "Documents and e-books",
      labels: [
        { id: "doc.epub", name: "EPUB e-book", extensions: ["epub"] },
        { id: "doc.pdf", name: "PDF document", extensions: ["pdf"] },
      ],
    },
    { id: "archive", name: "Archives", labels: [{ id: "archive.zip", name: "ZIP archive", extensions: ["zip"] }] },
  ],
};
const CAT = /** @type {import("../js/file-kinds.js").Catalogue} */ (parseCatalogue(RAW));
const lo = labelsOfFn(CAT, null);
/** The C1 catalogue: video and exe only. */
const C1 = labelsOfFn(parseCatalogue({ groups: RAW.groups.slice(0, 2) }), null);

Deno.test("toggleFileKind: the C1 vectors", () => {
  /** @type {[string[], string, string[]][]} */
  const vectors = [
    [[], "video", ["video"]],
    [["video.mkv"], "video", ["video"]],
    [["video"], "video", []],
    [["video"], "video.mkv", ["video.avi", "video.mp4"]],
    [["video.avi", "video.mp4"], "video.mkv", ["video"]],
    [["video.mkv"], "video.mkv", []],
    [["archive.zip"], "video.mkv", ["archive.zip", "video.mkv"]],
    [["exe.pe"], "exe.pe.dotnet", ["exe.pe", "exe.pe.dotnet"]],
  ];
  for (const [sel, value, want] of vectors) {
    assert.deepEqual(toggleFileKind(sel, value, C1), want, `${JSON.stringify(sel)} + ${value}`);
  }
});

Deno.test("normalize: case, dedupe, a group drops its labels, all labels become the group", () => {
  assert.deepEqual(normalizeFileKinds([" VIDEO.MKV", "video"], C1), ["video"]);
  // A group with one label: picking it is picking the group.
  assert.deepEqual(toggleFileKind([], "archive.zip", lo), ["archive"]);
  assert.deepEqual(normalizeFileKinds(["video.mkv", "Video.MKV", ""], lo), ["video.mkv"]);
  assert.deepEqual(normalizeFileKinds(["exe.pe", "exe.elf", "exe.pe.dotnet"], lo), ["exe"]);
  // Without a catalogue nothing collapses (no group has known labels).
  assert.deepEqual(normalizeFileKinds(["exe.pe", "exe.elf", "exe.pe.dotnet"]), ["exe.elf", "exe.pe", "exe.pe.dotnet"]);
  // Without a catalogue, the counts give a group's labels.
  const fromCounts = labelsOfFn(null, [{ value: "doc", count: 3 }, { value: "doc.epub", count: 2 }, { value: "doc.pdf", count: 1 }]);
  assert.deepEqual(fromCounts("doc"), ["doc.epub", "doc.pdf"]);
  assert.deepEqual(toggleFileKind(["doc.epub"], "doc.pdf", fromCounts), ["doc"]);
});

Deno.test("group header state", () => {
  assert.equal(fileKindState(["video"], "video"), "checked");
  assert.equal(fileKindState(["video.mkv"], "video"), "mixed");
  assert.equal(fileKindState(["exe.pe"], "video"), "unchecked");
});

Deno.test("summary: Any, one label, first in option order plus the rest", () => {
  const groups = fileKindOptions(["video", "archive.zip"], null, CAT);
  const order = fileKindOrder(groups);
  const name = (/** @type {string} */ v) => fileKindName(v, CAT);
  assert.equal(dropdownSummary([], order, name), "Any");
  assert.equal(dropdownSummary(["video"], order, name), "Video");
  assert.equal(dropdownSummary(["video", "archive.zip"], order, name), "Video, +1");
  assert.equal(dropdownSummary(["video.mkv", "exe.pe"], order, name), "Matroska video, +1");
  const langs = ["en", "fr", "de"];
  assert.equal(dropdownSummary(["fr", "de"], langs, (v) => facetLabel("language", v)), "French, +1");
  assert.equal(dropdownSummary(["de", "fr", "en"], ["fr", "en", "de"], (v) => facetLabel("language", v)), "French, +2");
});

Deno.test("search: case and diacritics, names, ids, extensions and group names", () => {
  assert.equal(foldSearch("  Fré "), "fre");
  assert.ok(matchesSearch(foldSearch("fre"), ["French"]));
  assert.ok(matchesSearch(foldSearch("FRANÇ"), ["Français"]));
  assert.ok(!matchesSearch(foldSearch("ger"), ["French", "fr"]));
  assert.ok(matchesSearch("", ["anything"]));
  // By extension: only Documents, expanded, with only the matching label.
  const epub = fileKindOptions([], null, CAT, "epub");
  assert.deepEqual(epub.map((g) => [g.id, g.matched, g.labels.map((l) => l.id)]), [["doc", true, ["doc.epub"]]]);
  // By name.
  assert.deepEqual(fileKindOptions([], null, CAT, "Matroska").map((g) => g.labels.map((l) => l.id)), [["video.mkv"]]);
  // By id.
  assert.deepEqual(fileKindOptions([], null, CAT, "exe.pe.dot").map((g) => g.labels.map((l) => l.id)), [["exe.pe.dotnet"]]);
  // A group name keeps the whole group.
  assert.deepEqual(fileKindOptions([], null, CAT, "execut").map((g) => g.labels.length), [3]);
  assert.deepEqual(fileKindOptions([], null, CAT, "zzz"), []);
});

Deno.test("options: groups by count, labels with a count or selected; without counts the catalogue", () => {
  const counts = [
    { value: "doc", count: 5 },
    { value: "video", count: 9 },
    { value: "video.mkv", count: 7 },
    { value: "video.mp4", count: 2 },
    { value: "doc.epub", count: 5 },
    { value: "exe", count: 0 },
  ];
  const g = fileKindOptions(["video.avi", "exe"], counts, CAT);
  assert.deepEqual(g.map((x) => [x.id, x.count, x.state]), [["video", 9, "mixed"], ["doc", 5, "unchecked"], ["exe", 0, "checked"]]);
  assert.deepEqual(g[0].labels.map((l) => [l.id, l.count, l.checked]), [["video.mkv", 7, false], ["video.mp4", 2, false], ["video.avi", 0, true]]);
  const none = fileKindOptions([], null, CAT);
  assert.deepEqual(none.map((x) => x.id), ["video", "exe", "doc", "archive"]);
  assert.equal(none[1].labels.length, 3);
  assert.ok(none.every((x) => x.count == null));
  // The order stays while the panel is open.
  const order = new ChipOrder();
  fileKindOptions([], counts, CAT, "", order);
  const later = fileKindOptions([], [{ value: "doc", count: 50 }, { value: "video", count: 1 }], CAT, "", order);
  assert.deepEqual(later.map((x) => [x.id, x.count]), [["video", 1], ["doc", 50]]);
});

Deno.test("names: the catalogue's, else a capitalised group and the raw label id", () => {
  assert.equal(fileKindName("video.mkv", CAT), "Matroska video");
  assert.equal(fileKindName("exe", CAT), "Executables");
  assert.equal(fileKindName("gamedata", null), "Gamedata");
  assert.equal(fileKindName("gamedata.wad", null), "gamedata.wad");
  assert.equal(fileKindName("video.unknown", CAT), "video.unknown");
  setCatalogue(CAT);
  try {
    assert.equal(facetLabel("probe_file_kind", "doc.epub"), "EPUB e-book");
  } finally {
    setCatalogue(null);
  }
});

Deno.test("the catalogue rejects a bad answer and keeps registry order", () => {
  assert.equal(parseCatalogue(null), null);
  assert.equal(parseCatalogue({ groups: "x" }), null);
  const c = parseCatalogue({
    groups: [
      { id: "Bad Id", name: "x", labels: [] },
      { id: "font", name: "", labels: [{ id: "video.mkv", name: "wrong group" }, { id: "font.ttf", name: "TrueType font", extensions: ["ttf", 3] }] },
    ],
  });
  assert.ok(c);
  assert.deepEqual(c.groups.map((g) => [g.id, g.name, g.labels.map((l) => l.id)]), [["font", "Font", ["font.ttf"]]]);
  assert.deepEqual(c.label.get("font.ttf")?.extensions, ["ttf"]);
});

Deno.test("the catalogue is asked once per server version and cached", async () => {
  let calls = 0;
  /** @type {Map<string, unknown>} */
  const store = new Map();
  let version = "1.0.0";
  let feature = true;
  const ctx = {
    api: {
      get: (/** @type {string} */ path) => {
        calls++;
        assert.equal(path, "/api/v1/probe/file-kinds");
        return Promise.resolve(RAW);
      },
    },
    cache: {
      get: (/** @type {string} */ e, /** @type {unknown} */ k) => Promise.resolve(store.has(e + JSON.stringify(k)) ? { json: store.get(e + JSON.stringify(k)) } : null),
      put: (/** @type {string} */ e, /** @type {unknown} */ k, /** @type {unknown} */ v) => {
        store.set(e + JSON.stringify(k), v);
        return Promise.resolve();
      },
    },
    health: { has: () => feature, version: () => version },
  };
  setCatalogue(null);
  feature = false;
  assert.equal(await loadCatalogue(ctx), null);
  assert.equal(calls, 0);
  feature = true;
  const a = await loadCatalogue(ctx);
  assert.equal(a?.label.get("video.mkv")?.name, "Matroska video");
  await loadCatalogue(ctx);
  assert.equal(calls, 1);
  assert.equal(catalogue(), a);
  // A new server version reads the page cache under its own key, then asks.
  version = "1.1.0";
  await loadCatalogue(ctx);
  assert.equal(calls, 2);
  assert.equal(store.size, 2);
  setCatalogue(null);
});

Deno.test("chips: many labels of one group are one chip, not one per catalogue label (D140 C3)", () => {
  // A group of 21 labels, all but one left after unticking PDF from the checked group.
  const ids = Array.from({ length: 21 }, (_, i) => `doc.k${String(i).padStart(2, "0")}`);
  const big = parseCatalogue({
    groups: [
      { id: "doc", name: "Documents and e-books", labels: [...ids, "doc.pdf"].map((id) => ({ id, name: id === "doc.pdf" ? "PDF document" : id })) },
      ...RAW.groups.slice(0, 2),
    ],
  });
  const blo = labelsOfFn(big, null);
  const name = (/** @type {string} */ v) => fileKindName(v, big);
  const sel = toggleFileKind(["doc"], "doc.pdf", blo);
  assert.equal(sel.length, 21);
  assert.deepEqual(fileKindChips(sel, blo, name), [{ values: sel, label: "Documents and e-books except PDF document" }]);
  const order = fileKindOrder(fileKindOptions(sel, null, big));
  assert.equal(fileKindSummary(sel, order, blo, name), "Documents and e-books except PDF document");
  // With a group and a few labels elsewhere: one chip each, the summary counts chips.
  const mixed = [...sel, "exe.pe", "video"];
  assert.deepEqual(fileKindChips(mixed, blo, name).map((c) => c.label), ["Documents and e-books except PDF document", "Windows PE executable", "Video"]);
  // Without counts the option order is the catalogue's: Documents first.
  assert.equal(fileKindSummary(mixed, fileKindOrder(fileKindOptions(mixed, null, big)), blo, name), "Documents and e-books except PDF document, +2");
  // More than two left out: "{group}: n kinds".
  const many = sel.slice(0, 10);
  assert.deepEqual(fileKindChips(many, blo, name), [{ values: many, label: "Documents and e-books: 10 kinds" }]);
  // Up to three labels stay one chip each.
  assert.deepEqual(fileKindChips(["doc.k00", "doc.k01", "doc.pdf"], blo, name).map((c) => c.values), [["doc.k00"], ["doc.k01"], ["doc.pdf"]]);
  assert.deepEqual(fileKindChips([], blo, name), []);
  assert.equal(fileKindSummary([], [], blo, name), "Any");
});
