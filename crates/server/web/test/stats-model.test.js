// Values from StatsFormatTest.kt, ScreensViewModelTest.kt (describe) and FileTreeTest.kt (seed history).
import assert from "node:assert/strict";
import { describe, sortByName } from "../js/saved-model.js";
import { makeFilters } from "../js/search-model.js";
import {
  activityOf,
  backlogLines,
  breakdownFields,
  dhtLines,
  fileKindRequest,
  indexLines,
  missingFeatures,
  offeredActivities,
  openFileGroup,
  probedTotal,
  seedChecks,
  selectedStat,
  statChips,
  toggleStat,
  valueLabel,
} from "../js/stats-model.js";
import { formatShare } from "../js/numfmt.js";

const en = "en-US";

Deno.test("cross-filters toggle, years are one-year ranges, chips list them", () => {
  let f = makeFilters();
  f = toggleStat(toggleStat(toggleStat(f, "language", "fr"), "year", "2024"), "origin", "dht");
  assert.deepEqual(f.languages, ["fr"]);
  assert.equal(f.minYear, 2024);
  assert.equal(f.maxYear, 2024);
  assert.deepEqual(selectedStat(f, "year"), ["2024"]);
  assert.deepEqual(statChips(f), [["language", "fr"], ["origin", "dht"], ["year", "2024"]]);
  f = toggleStat(toggleStat(f, "year", "2024"), "subcategory", "filmvidéo");
  assert.equal(f.minYear, null);
  assert.deepEqual(selectedStat(f, "year"), []);
  assert.deepEqual(statChips(f), [["language", "fr"], ["origin", "dht"]]);
  assert.deepEqual(selectedStat(makeFilters({ minYear: 2000, maxYear: 2010 }), "year"), []);
  // A malformed year changes nothing.
  assert.equal(toggleStat(f, "year", "20x4"), f);
});

Deno.test("labels and fields", () => {
  assert.equal(valueLabel("origin", "dht"), "DHT");
  assert.equal(valueLabel("language", "fr-ca"), "French (Québec)");
  assert.equal(valueLabel("category", "video"), "Video");
  assert.equal(valueLabel("subcategory", "filmvidéo"), "filmvidéo");
  assert.ok(breakdownFields(new Set(["stats", "origin"])).includes("origin"));
  assert.ok(!breakdownFields(new Set(["stats"])).includes("origin"));
  assert.ok(!offeredActivities(false).some((a) => a.dht));
  const dht = offeredActivities(true).map((a) => a.metric);
  for (const m of ["dht_torrents_added", "dht_hashes_seen", "dht_blocked"]) assert.ok(dht.includes(m));
  assert.ok(!offeredActivities(true).some((a) => a.metric === "probe_backfill_dht_rows"));
  assert.ok(offeredActivities(false, true).some((a) => a.metric === "probe_backfill_dht_rows"));
  assert.equal(activityOf("db_size").unit, "bytes");
  assert.equal(activityOf("nope").metric, "torrents_total");
});

Deno.test("seed checks keep not checked yet apart from the results", () => {
  const l = { torrents: 957_462, never_checked: 367_301, alive: 340_984, dead: 233_018, unknown: 16_159 };
  const s = seedChecks(l);
  assert.equal(s.checked, 590_161);
  assert.equal(s.notChecked, 367_301);
  assert.equal(s.torrents, 957_462);
  assert.equal(formatShare(s.notChecked, s.torrents, en), "38%");
  const lines = new Map(backlogLines({ ...l, enabled: true, due_now: 431_893, checks_per_hour: 30_000 }, en));
  assert.equal(lines.get("Due now"), "431,893 (cleared ≈ 14 h)");
  assert.equal(lines.get("Never checked"), "367,301 of 957,462");
  assert.ok(!lines.has("Checker"));
  assert.deepEqual(backlogLines({ enabled: false }, en)[0], ["Checker", "turned off on the server"]);
});

Deno.test("index and DHT lines", () => {
  const idx = new Map(indexLines({ state: "building", build_done: 25, build_total: 100, docs: 10, pending: 3, lag_secs: 90 }, en));
  assert.equal(idx.get("State"), "building (25%)");
  assert.equal(idx.get("Changes queued"), "3 (oldest 1 min)");
  const d = {
    mode: "aggressive", running: true, uptime_secs: 508, virtual_nodes: 4, external_ip: "public",
    queries_per_sec: 2779.27, answered_share: 0.427, incoming_per_min: 28_992.5, hashes_per_min: 18_145.7,
    queue_len: 347, metadata_per_min: 1707.6, fetch_success_rate: 0.23, stored_total: 455_540,
    adult_total: 83_257, blocked_total: 4_982,
    upnp: { state: "mapped", mappings: [{ protocol: "udp", port: 6891 }, { protocol: "tcp", port: 6891 }] },
  };
  const lines = new Map(dhtLines(d, en));
  assert.equal(lines.get("Crawler"), "aggressive · 4 nodes · up 8 min");
  assert.equal(lines.get("Discovery"), "18,146/min new hashes");
  assert.equal(lines.get("Stored"), "455,540 torrents (83,257 adult)");
  assert.equal(lines.get("Blocked (illegal content)"), "4,982");
  assert.equal(lines.get("Router ports"), "udp 6891, tcp 6891");
  assert.equal(lines.get("Address"), "public");
  assert.equal(dhtLines({ mode: "polite", running: false, last_error: "bind failed" }, en)[0][1], "restarting after an error: bind failed");
});

Deno.test("a server without track kinds is named on the server card", () => {
  const d122 = ["stats", "graphs", "saved_search_poll", "filters", "origin", "crawl_sources", "crawl_map", "probe_filters"];
  assert.deepEqual(missingFeatures(new Set(d122)), ["track-kind filters", "file-kind filters"]);
  assert.deepEqual(missingFeatures(new Set([...d122, "probe_kinds"])), ["file-kind filters"]);
  // D137: the file kinds, named like the app's server card does.
  assert.deepEqual(missingFeatures(new Set([...d122, "probe_kinds", "probe_file_kinds"])), []);
});

Deno.test("a saved query is described in one line", () => {
  assert.equal(
    describe({ text: "dune", categories: ["video"], resolutions: ["2160p"], min_rating: 7, min_year: 2020, origins: ["dht"] }),
    "\"dune\" · Video · DHT · 2160p (4K) · ★ ≥ 7.0 · ≥ 2020",
  );
  assert.equal(describe({}), "any text");
  assert.equal(describe(null), "any text");
  assert.equal(describe({ probe_hdr: ["any"], probe_audio: ["atmos"], probe_languages: ["fre"] }), "any text · files: French, Any HDR, Atmos");
  assert.equal(describe({ probe_subtitles: ["fre"], probe_sub_kinds: ["sdh"] }), "any text · files: French, SDH");
  assert.equal(
    describe({ probe_languages: ["fre"], probe_audio_kinds: ["description"], probe_sub_formats: ["image", "text"] }),
    "any text · files: French, Audio description, Image (PGS, VobSub…)/Text (SRT, ASS…)",
  );
  assert.equal(describe({ min_seeders: 5, min_trust: 30, hide_dead: true, max_year: 1999 }), "any text · ≥ 5 seeders · alive only · trust ≥ 30 · ≤ 1999");
  // A hostile field type is ignored, never thrown.
  assert.equal(describe({ text: 42, categories: "video" }), "any text");
  assert.deepEqual(
    sortByName([{ name: "dune 4K" }, { name: "All torrents" }, { name: "French SDH subtitles" }, { name: "<img src=x>" }]).map((s) => s.name),
    ["<img src=x>", "All torrents", "dune 4K", "French SDH subtitles"],
  );
});

Deno.test("file kinds: the extra request only with the feature, the drill-down group (D139)", () => {
  const q = { text: "", probe_file_kinds: ["video"] };
  assert.equal(fileKindRequest(q, new Set(["stats", "probe_filters"]), null), null);
  const feats = new Set(["stats", "probe_file_kinds"]);
  assert.deepEqual(fileKindRequest(q, feats, null), { query: q, fields: ["file_group"], top: 50 });
  assert.deepEqual(fileKindRequest(q, feats, "doc"), { query: q, fields: ["file_group", "file_kind"], top: 50, file_group: "doc" });
  // Opens by itself when the cross-filter names one group; the user's pick wins.
  assert.equal(openFileGroup(makeFilters({ probeFileKinds: ["video"] }), null), "video");
  assert.equal(openFileGroup(makeFilters({ probeFileKinds: ["doc.epub", "doc.pdf"] }), null), "doc");
  assert.equal(openFileGroup(makeFilters({ probeFileKinds: ["doc", "video"] }), null), null);
  assert.equal(openFileGroup(makeFilters({ probeFileKinds: ["video"] }), ""), null);
  assert.equal(openFileGroup(makeFilters(), "exe"), "exe");
  assert.equal(probedTotal({ total: 1500, missing: 1400 }), 100);
  assert.equal(probedTotal(null), 0);
});

Deno.test("file kinds: bars cross-filter with the group rule, chips list them", () => {
  const labelsOf = (/** @type {string} */ g) => (g === "doc" ? ["doc.cbz", "doc.epub", "doc.pdf"] : []);
  let f = toggleStat(makeFilters(), "file_group", "video", labelsOf);
  assert.deepEqual(f.probeFileKinds, ["video"]);
  f = toggleStat(f, "file_kind", "doc.epub", labelsOf);
  assert.deepEqual(f.probeFileKinds, ["doc.epub", "video"]);
  assert.deepEqual(selectedStat(f, "file_group"), ["video"]);
  assert.deepEqual(selectedStat(f, "file_kind"), ["doc.epub"]);
  assert.deepEqual(statChips(f), [["file_group", "video"], ["file_kind", "doc.epub"]]);
  // Every label of a group becomes the group.
  f = toggleStat(toggleStat(f, "file_kind", "doc.pdf", labelsOf), "file_kind", "doc.cbz", labelsOf);
  assert.deepEqual(f.probeFileKinds, ["doc", "video"]);
  f = toggleStat(f, "file_group", "video", labelsOf);
  assert.deepEqual(f.probeFileKinds, ["doc"]);
  assert.equal(valueLabel("file_group", "gamedata"), "Gamedata");
  assert.equal(valueLabel("file_kind", "doc.epub"), "doc.epub");
});
