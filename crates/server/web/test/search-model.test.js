import assert from "node:assert/strict";
import { facetLabel } from "../js/labels.js";
import {
  activeCount,
  buildQuery,
  cacheQuery,
  ChipOrder,
  EMPTY_FILTERS,
  emptySearchState,
  FACET_TITLES,
  facetBody,
  facetChips,
  facetFields,
  filtersOf,
  hasProbeFilters,
  makeFilters,
  PROBE_FACET_FIELDS,
  PROBE_KIND_FIELDS,
  sameState,
  selected,
  stateFromParams,
  stateToParams,
  supportedBy,
  supportedSort,
  toggle,
} from "../js/search-model.js";

const now = 1_800_000_000;

Deno.test("query and filters round-trip (SearchModelTest)", () => {
  const f = makeFilters({
    categories: ["book", "video"],
    minSizeMb: 100,
    maxSizeMb: 4000,
    date: "week",
    minSeeders: 3,
    hideDead: true,
    minTrust: 0,
    languages: ["fr", "en"],
    subtitles: ["fr"],
    resolutions: ["2160p"],
    sources: ["web-dl"],
    codecs: ["h265"],
    hdr: ["dv"],
    audio: ["atmos"],
    genres: ["drama"],
    kinds: ["series"],
    minRating: 7.5,
    minYear: 2000,
    maxYear: 2024,
    origins: ["dht"],
  });
  const q = buildQuery(" dune ", f, "rating", false, true, { nowSec: now });
  assert.equal(q.text, "dune");
  assert.deepEqual(q.categories, ["video", "book"]);
  assert.deepEqual(q.languages, ["en", "fr"]);
  assert.equal(q.min_size, 100 * 1024 * 1024);
  assert.equal(q.since, now - 7 * 86_400);
  assert.equal(q.min_trust, 0);
  assert.equal(q.include_adult, true);
  assert.equal(q.video_codecs instanceof Array, true);
  assert.equal(q.limit, 50);
  assert.equal(q.offset, 0);
  assert.deepEqual(filtersOf(q, now), f);
  assert.equal(activeCount(f), 18);
  // Null fields are left out, as the app's explicitNulls = false.
  const empty = buildQuery("", EMPTY_FILTERS, "relevance", true, false, { nowSec: now });
  assert.equal("min_size" in empty, false);
  assert.equal("since" in empty, false);
  assert.equal("min_trust" in empty, false);
  assert.equal(empty.hide_dead, false);
  assert.equal(empty.include_adult, false);
});

Deno.test("unsupported filters are dropped for an older server", () => {
  const f = makeFilters({ resolutions: ["1080p"], minRating: 7, origins: ["nostr"], categories: ["video"] });
  assert.deepEqual(supportedBy(f, []), makeFilters({ categories: ["video"] }));
  const withFilters = supportedBy(f, ["filters"]);
  assert.deepEqual(withFilters.resolutions, ["1080p"]);
  assert.deepEqual(withFilters.origins, []);
  assert.deepEqual(supportedBy(f, new Set(["filters", "origin"])), f);
  assert.equal(supportedSort("rating", new Set()), "relevance");
  assert.equal(supportedSort("rating", new Set(["filters"])), "rating");
});

Deno.test("toggle and selected cover every chip field", () => {
  let f = EMPTY_FILTERS;
  const fields = [
    "category", "language", "subtitles", "resolution", "source", "codec", "hdr", "audio", "genre", "media_kind",
    "origin", ...PROBE_FACET_FIELDS,
  ];
  for (const field of fields) {
    f = toggle(f, field, "x");
    assert.deepEqual(selected(f, field), ["x"], field);
    f = toggle(f, field, "x");
    assert.deepEqual(selected(f, field), [], field);
  }
  assert.equal(Object.isFrozen(toggle(f, "language", "fr")), true);
});

Deno.test("chips without counts are the built-in list, a selected unknown value is kept", () => {
  const chips = facetChips("resolution", ["1080p"], null);
  assert.equal(chips[0].value, "4320p");
  assert.ok(chips.every((c) => c.count == null));
  const counted = facetChips("language", ["ko"], {
    field: "language",
    total: 10,
    counts: [{ value: "fr", count: 7 }, { value: "en", count: 3 }],
  });
  assert.deepEqual(counted.map((c) => [c.value, c.count]), [["fr", 7], ["en", 3], ["ko", 0]]);
  assert.equal(counted[0].label, "French");
});

Deno.test("chips stay where they first appeared and a value that drops to zero stays in place", () => {
  const order = new ChipOrder();
  /** @param {[string, number][]} p */
  const counts = (...p) =>
    facetChips("resolution", [], { field: "resolution", total: 0, counts: p.map(([value, count]) => ({ value, count })) });
  const first = order.arrange("resolution", counts(["1080p", 50], ["2160p", 20], ["720p", 5]));
  assert.deepEqual(first.map((c) => c.value), ["1080p", "2160p", "720p"]);
  const second = order.arrange("resolution", counts(["2160p", 30], ["1080p", 10], ["sd", 1]));
  assert.deepEqual(second.map((c) => c.value), ["1080p", "2160p", "720p", "sd"]);
  assert.equal(second.find((c) => c.value === "720p")?.count, 0);
  assert.equal(second.find((c) => c.value === "2160p")?.count, 30);
  assert.equal(order.arrange("resolution", facetChips("resolution", [], null))[0].count, null);
});

Deno.test("probe filters round-trip and are dropped for a server without probe_filters", () => {
  const f = makeFilters({
    probeLanguages: ["jpn", "fre"],
    probeSubtitles: ["eng"],
    probeResolutions: ["2160p"],
    probeCodecs: ["hevc"],
    probeHdr: ["any"],
    probeAudio: ["atmos"],
    categories: ["video"],
  });
  const q = buildQuery("", f, "date", true, false, { nowSec: now });
  assert.deepEqual(q.probe_languages, ["fre", "jpn"]);
  assert.deepEqual(q.probe_subtitles, ["eng"]);
  assert.deepEqual(q.probe_resolutions, ["2160p"]);
  assert.deepEqual(q.probe_video_codecs, ["hevc"]);
  assert.deepEqual(q.probe_hdr, ["any"]);
  assert.deepEqual(q.probe_audio, ["atmos"]);
  assert.deepEqual(filtersOf(q, now), f);
  assert.equal(activeCount(f), 7);
  assert.equal(hasProbeFilters(f), true);
  const old = supportedBy(f, ["filters", "origin"]);
  assert.deepEqual(old, makeFilters({ categories: ["video"] }));
  assert.equal(hasProbeFilters(old), false);
  assert.deepEqual(supportedBy(f, ["probe_filters"]), f);
  const both = makeFilters({ resolutions: ["2160p"], probeResolutions: ["2160p"] });
  assert.deepEqual(supportedBy(both, ["filters"]), makeFilters({ resolutions: ["2160p"] }));
});

Deno.test("probe kind filters round-trip and are dropped without probe_kinds (C-FILT-16)", () => {
  const f = makeFilters({
    probeSubtitles: ["fre"],
    probeSubKinds: ["sdh", "forced"],
    probeAudioKinds: ["description"],
    probeSubFormats: ["image"],
    probeLanguages: ["fre"],
  });
  const q = buildQuery("", f, "date", true, false, { nowSec: now });
  assert.deepEqual(q.probe_sub_kinds, ["forced", "sdh"]);
  assert.deepEqual(q.probe_audio_kinds, ["description"]);
  assert.deepEqual(q.probe_sub_formats, ["image"]);
  assert.deepEqual(filtersOf(q, now), f);
  assert.equal(activeCount(f), 5);
  const json = JSON.stringify(q);
  assert.ok(json.includes('"probe_sub_kinds":["forced","sdh"]'), json);
  assert.deepEqual(
    supportedBy(f, ["filters", "probe_filters"]),
    makeFilters({ probeSubtitles: ["fre"], probeLanguages: ["fre"] }),
  );
  assert.deepEqual(supportedBy(f, ["probe_kinds"]), EMPTY_FILTERS);
  assert.deepEqual(supportedBy(f, ["probe_filters", "probe_kinds"]), f);
});

Deno.test("kind chips toggle and read as names", () => {
  assert.deepEqual([...PROBE_FACET_FIELDS], [
    "probe_language", "probe_audio_kind", "probe_subtitles", "probe_sub_kind", "probe_sub_format",
    "probe_resolution", "probe_codec", "probe_hdr", "probe_audio",
  ]);
  assert.ok(PROBE_KIND_FIELDS.every((k) => PROBE_FACET_FIELDS.includes(k)));
  assert.equal(FACET_TITLES.probe_audio_kind, "Audio track kind");
  assert.deepEqual(
    facetChips("probe_audio_kind", [], null).map((c) => c.label),
    ["Original", "Dubbed", "Voice-over", "Audio description", "Clear dialogue", "Commentary", "Music only", "Karaoke", "Alternate mix"],
  );
  assert.deepEqual(
    facetChips("probe_sub_kind", [], null).map((c) => c.label),
    ["SDH", "Closed captions", "Forced", "Signs & songs", "Commentary", "Descriptions", "Honorifics", "Bilingual", "Easy reader"],
  );
  assert.deepEqual(
    facetChips("probe_sub_format", [], null).map((c) => c.label),
    ["Text (SRT, ASS…)", "Image (PGS, VobSub…)", "Closed captions (608/708)"],
  );
  assert.equal(facetLabel("probe_hdr", "any"), "Any HDR");
  const counted = facetChips("probe_sub_kind", ["forced"], { field: "probe_sub_kind", total: 3, counts: [{ value: "sdh", count: 3 }] });
  assert.deepEqual(counted.map((c) => [c.label, c.count]), [["SDH", 3], ["Forced", 0]]);
});

Deno.test("facet fields follow the features", () => {
  assert.deepEqual(facetFields(new Set()), ["category"]);
  assert.deepEqual(facetFields(new Set(["origin"])), ["category", "origin"]);
  const all = facetFields(new Set(["origin", "filters", "probe_filters", "probe_kinds"]));
  assert.equal(all.length, 2 + 9 + 9);
  const noKinds = facetFields(new Set(["probe_filters"]));
  assert.ok(!noKinds.includes("probe_sub_kind"));
  assert.ok(noKinds.includes("probe_language"));
});

Deno.test("cache query uses the preset, not the clock, and carries the adult switch", () => {
  const f = makeFilters({ date: "week" });
  const a = cacheQuery("x", f, "relevance", true, false);
  assert.equal(a.since, -7 * 86_400);
  assert.equal(a.include_adult, false);
  assert.notDeepEqual(cacheQuery("x", f, "relevance", true, true), a);
});

Deno.test("URL state round-trips and drops malformed parameters (C-SRCH-10)", () => {
  const s = {
    text: "dune part two",
    filters: makeFilters({
      categories: ["video"],
      languages: ["fr", "multi"],
      genres: ["science fiction"],
      hdr: ["hdr10+"],
      minSizeMb: 100,
      maxSizeMb: 9000,
      date: "month",
      minSeeders: 2,
      hideDead: true,
      minTrust: 0,
      minRating: 7.5,
      minYear: 1999,
      maxYear: 2024,
      probeSubKinds: ["sdh"],
      probeLanguages: ["fre"],
    }),
    sort: /** @type {const} */ ("seeders"),
    descending: false,
  };
  const qs = stateToParams(s);
  const back = stateFromParams(new URLSearchParams(qs));
  assert.deepEqual(back, s);
  assert.ok(sameState(back, s));
  assert.equal(stateToParams(emptySearchState()), "");
  const bad = stateFromParams(new URLSearchParams(
    "q=x&cat=video,<img>,porn&min_size_mb=-4&max_size_mb=abc&date=decade&min_trust=500&min_rating=7.3" +
      "&min_year=99&sort=evil&desc=maybe&unknown=1&min_seeders=1e9",
  ));
  assert.deepEqual(bad.filters.categories, ["porn", "video"]);
  assert.equal(bad.filters.minSizeMb, null);
  assert.equal(bad.filters.maxSizeMb, null);
  assert.equal(bad.filters.date, "any");
  assert.equal(bad.filters.minTrust, null);
  assert.equal(bad.filters.minRating, null);
  assert.equal(bad.filters.minYear, null);
  assert.equal(bad.filters.minSeeders, null);
  assert.equal(bad.sort, "relevance");
  assert.equal(bad.descending, true);
  // The API's byte sizes are read as MiB.
  assert.equal(stateFromParams(new URLSearchParams("min_size=1048576")).filters.minSizeMb, 1);
});

Deno.test("file kinds: URL round-trip, request key, dropped without probe_file_kinds (D137)", () => {
  const back = stateFromParams(new URLSearchParams("probe_file_kind=video,archive.zip"));
  assert.deepEqual(back.filters.probeFileKinds, ["archive.zip", "video"]);
  assert.equal(stateToParams(back), "probe_file_kind=archive.zip%2Cvideo");
  assert.deepEqual(stateFromParams(new URLSearchParams(stateToParams(back))), back);
  const q = buildQuery("", back.filters, "relevance", true, false, { nowSec: now });
  assert.deepEqual(q.probe_file_kinds, ["archive.zip", "video"]);
  assert.deepEqual(filtersOf(q, now).probeFileKinds, ["archive.zip", "video"]);
  assert.equal(activeCount(back.filters), 1);
  assert.equal(hasProbeFilters(back.filters), true);
  assert.deepEqual(selected(back.filters, "probe_file_kind"), ["archive.zip", "video"]);
  // An older server never gets the field.
  const old = supportedBy(back.filters, new Set(["filters", "probe_filters", "probe_kinds"]));
  assert.deepEqual(old.probeFileKinds, []);
  assert.deepEqual(supportedBy(back.filters, new Set(["probe_file_kinds"])).probeFileKinds, ["archive.zip", "video"]);
  assert.equal(FACET_TITLES.probe_file_kind, "File kind");
});

Deno.test("file kinds: facet field and per-field size only with the feature (D137 A4)", () => {
  const q = { text: "" };
  const old = facetBody(q, new Set(["filters", "probe_filters"]));
  assert.equal(old.fields.includes("probe_file_kind"), false);
  assert.equal("sizes" in old, false);
  assert.equal(old.size, 40);
  const cur = facetBody(q, new Set(["filters", "probe_filters", "probe_file_kinds"]));
  assert.equal(cur.fields[cur.fields.length - 1], "probe_file_kind");
  assert.deepEqual(cur.sizes, { probe_file_kind: 400 });
  assert.equal(cur.size, 40);
  assert.ok(facetFields(new Set(["probe_file_kinds"])).includes("probe_file_kind"));
});
