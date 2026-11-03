// Values from ProbeStatsTest.kt and StatsFormatTest.kt ("correction stats lines").
import assert from "node:assert/strict";
import {
  backfillLines,
  backfillPercent,
  backfillProgress,
  correctionLines,
  hasCorrections,
  named,
  outcomeLabel,
  redactFleet,
  workerBackfillLines,
  workerBadges,
} from "../js/probes-model.js";

const en = "en-US";

const newer = JSON.parse(`{"torrents":1800000,"crawler":{"running":false},"dht":null,
  "workers":[{"name":"eee","connected":true,"version":"0.9","arch":"x86_64","first_seen":1,"last_seen":2,
    "leases_held":0,"liveness_results_total":5,"stored_total":7,
    "backfill_leased_total":512,"backfill_probed_total":40,"backfill_unprobed_total":300,
    "pressure":false,"dht":{"mode":"polite","nodes":1,"ports":[6881],"qps":4.5,"discover":false,
      "probe":{"upload":false,"started":50,"ok":40,"skipped":[["rate",3],["adult",1]]},
      "backfill":{"leased":512,"released":2,"outstanding":30,"no_peers":250,"failed":20,"fetched":60,
        "probed":40,"unprobed":20,"lease_errors":1}}},
   {"name":"rpi","connected":false,"dht":{"mode":"polite","qps":1.0}}],
  "probe_backfill":{"waiting":1000,"leased":30,"retrying":200,"remaining":1230,
    "closed":[["no_peers",400],["adult",12],["private",3]],"probed":770,"filling":false,
    "filled_at":1793000000,"at":1793000100,"future":1}}`);

Deno.test("malformed closed pairs are dropped, a blank outcome counts as unknown", () => {
  assert.deepEqual(named([["gone", 5], [7, 1], "x", [], ["blocked"], { a: 1 }, ["adult", 9]]), [
    { name: "adult", count: 9 },
    { name: "gone", count: 5 },
    { name: "blocked", count: 0 },
  ]);
  const b = { closed: [["adult", 12], ["", 5], [3, 1]], probed: 1, remaining: 1 };
  const m = new Map(backfillLines(b, en));
  assert.equal(m.get("Closed"), "17");
  assert.equal(m.get("  unknown"), "5");
  assert.equal(outcomeLabel(""), "unknown");
  assert.deepEqual(named("nope"), []);
});

Deno.test("progress is probed against what is left to try", () => {
  const p = backfillProgress({ waiting: 10, leased: 5, retrying: 5, remaining: 20, probed: 60 });
  assert.equal(p.probed, 60);
  assert.equal(p.remaining, 20);
  assert.equal(p.total, 80);
  assert.equal(p.fraction, 0.75);
  assert.equal(backfillPercent(p, en), "75% probed");
  assert.equal(backfillProgress({}).fraction, 0);
  assert.equal(backfillPercent(backfillProgress({}), en), "nothing queued");
  assert.equal(backfillPercent(backfillProgress({ probed: 9_999, remaining: 1 }), en), "99% probed");
  assert.equal(backfillPercent(backfillProgress({ probed: 1, remaining: 9_999 }), en), "<1% probed");
  assert.equal(backfillPercent(backfillProgress({ probed: 5, remaining: 0 }), en), "100% probed");
  assert.equal(backfillProgress({ waiting: 7, leased: 3, remaining: 0, probed: 10 }).remaining, 10);
});

Deno.test("backfill lines list the states and the closed outcomes", () => {
  const lines = backfillLines(newer.probe_backfill, en, 1793000160);
  const m = new Map(lines);
  assert.equal(m.get("Probed"), "770 of 2,000");
  assert.equal(m.get("Remaining"), "1,230");
  assert.equal(m.get("Waiting / leased / retrying"), "1,000 / 30 / 200");
  assert.equal(m.get("Closed"), "415");
  assert.equal(m.get("  no peers"), "400");
  assert.equal(m.get("  adult"), "12");
  assert.deepEqual(lines.map((l) => l[0]).filter((k) => k.startsWith("  ")), ["  no peers", "  adult", "  private"]);
  assert.ok(String(m.get("Queue fill")).startsWith("last pass"));
  assert.equal(new Map(backfillLines({ filling: true }, en)).get("Queue fill"), "filling now");
  assert.ok(!new Map(backfillLines({}, en)).has("Closed"));
  assert.equal(outcomeLabel("no_peers"), "no peers");
  assert.equal(outcomeLabel("v2_only"), "v2 only");
  assert.equal(outcomeLabel("oversize"), "too large");
  assert.equal(outcomeLabel("some_new_code"), "some new code");
});

Deno.test("worker badges and backfill counters", () => {
  const eee = newer.workers[0];
  assert.deepEqual(workerBadges(eee), ["discovery off", "backfill on", "probes on"]);
  const m = new Map(workerBackfillLines(eee, en));
  assert.equal(m.get("Leased"), "512");
  assert.equal(m.get("Probed / without a probe"), "40 / 300");
  assert.equal(m.get("In the worker now"), "30");
  assert.equal(m.get("Fetched / no peers / failed"), "60 / 250 / 20");
  assert.equal(m.get("Lease errors"), "1");
  const w = { name: "x", backfill_leased_total: 3, dht: { discover: true } };
  assert.deepEqual(workerBadges(w), []);
  assert.equal(new Map(workerBackfillLines(w, en)).get("Leased"), "3");
  assert.ok(!new Map(workerBackfillLines(w, en)).has("In the worker now"));
  assert.deepEqual(workerBadges({ dht: { backfill: {}, probe: {} } }), ["backfill on", "probes on"]);
  const rpi = newer.workers[1];
  assert.deepEqual(workerBadges(rpi), []);
  assert.deepEqual(workerBackfillLines(rpi, en), []);
  assert.deepEqual(redactFleet({ workers: [{ name: "10.0.0.2" }, { name: "rpi" }] }).workers?.map((x) => x.name), ["(IP address)", "rpi"]);
});

Deno.test("correction stats lines", () => {
  const s = {
    tracks: 140, checked: 92, corrected: 41, removed: 7, added: 4,
    languages: { checked: 120, wrong: 6, filled: 9 },
    names_changed: 3,
    kinds: [{ track: "sub", kind: "sdh", source: "name", checked: 31, wrong: 4 }, { track: "sub", kind: "forced", source: "flags", checked: 12, wrong: 1 }],
    missed: [{ track: "sub", kind: "forced", count: 3 }],
    at: 1793500000,
  };
  assert.ok(hasCorrections(s));
  assert.deepEqual(correctionLines(s, en), [
    ["Tracks checked", "140"],
    ["Matched VLC", "92"],
    ["Corrected", "41"],
    ["Not in the file", "7"],
    ["Added by you", "4"],
    ["Languages", "6 of 120 wrong, 9 filled in"],
    ["SDH (subtitles) from track names", "4 of 31 wrong"],
    ["Forced (subtitles) from file flags", "1 of 12 wrong"],
    ["Forced (subtitles) missed", "3"],
  ]);
  const many = {
    tracks: 1,
    languages: { checked: 1_234, wrong: 0 },
    kinds: Array.from({ length: 10 }, () => ({ track: "audio", kind: "description", source: "flags", checked: 2, wrong: 1 })),
    missed: Array.from({ length: 6 }, () => ({ track: "audio", kind: "voiceover", count: 1 })),
  };
  const lines = correctionLines(many, en);
  assert.ok(lines.some(([k, v]) => k === "Languages" && v === "0 of 1,234 wrong"));
  assert.equal(lines.filter(([k]) => k === "Audio description (audio) from file flags").length, 8);
  assert.equal(lines.filter(([k]) => k === "Voice-over (audio) missed").length, 4);
  assert.ok(!hasCorrections(null));
  assert.ok(!hasCorrections({}));
  assert.ok(hasCorrections({ added: 1 }));
});
