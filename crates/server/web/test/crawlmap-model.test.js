// Values from CrawlMapTest.kt ("Scale and labels", the sample map).
import assert from "node:assert/strict";
import {
  buildMonth,
  countryCounts,
  countryName,
  geoReasonMessage,
  LogScale,
  mapParams,
  metricIndex,
  networkLabel,
  topBy,
  totalOf,
} from "../js/crawlmap-model.js";

const en = "en-US";
const fr = "fr-FR";

Deno.test("log scale shades", () => {
  const s = /** @type {LogScale} */ (LogScale.of([0, 10, 100, 1000]));
  assert.equal(s.min, 10);
  assert.equal(s.max, 1000);
  assert.equal(s.shade(0), null);
  assert.ok(Math.abs(/** @type {number} */ (s.shade(10)) - 0) < 1e-6);
  assert.ok(Math.abs(/** @type {number} */ (s.shade(100)) - 0.5) < 1e-6);
  assert.ok(Math.abs(/** @type {number} */ (s.shade(1000)) - 1) < 1e-6);
  assert.equal(s.shade(5000), 1);
  assert.equal(LogScale.of([7])?.shade(7), 1);
  assert.equal(LogScale.of([0, 0]), null);
  assert.equal(LogScale.of([]), null);
});

Deno.test("country names, months, networks and reasons", () => {
  assert.equal(countryName("DE", en), "Germany");
  assert.equal(countryName("de", fr), "Allemagne");
  assert.equal(countryName("ZZZ", en), "ZZZ");
  assert.equal(countryName("1", en), "1");
  assert.equal(buildMonth(1790000000, en), "September 2026");
  assert.equal(networkLabel({ asn: 24940, as_org: "Hetzner Online GmbH" }), "AS24940 Hetzner Online GmbH");
  assert.equal(networkLabel({ asn: 3320 }), "AS3320");
  assert.equal(networkLabel({}), "Unknown network");
  assert.ok(geoReasonMessage("missing").includes("not downloaded yet"));
  assert.equal(geoReasonMessage("disabled"), "The server's geo database is off, so nothing new is placed on the map.");
  assert.ok(geoReasonMessage(null).trim() !== "");
});

const sample = {
  layer: "dht", window: "day", available: true, metrics: ["hashes", "stored", "nodes"],
  countries: [
    { country: "US", counts: [120_000, 3_000, 40_000] },
    { country: "de", counts: [5_000, 4_000, 800] },
    { country: "FR", counts: [0, 0, 10] },
    { country: null, counts: [9, 9, 9] },
  ],
  networks: [{ country: "DE", asn: 24940, as_org: "Hetzner", counts: [5_000, 200, 800] }],
  unknown: [1_000, 10, 50],
  totals: [400_000, 9_000, 120_000],
};

Deno.test("counts per country and top lists follow the metric", () => {
  assert.equal(metricIndex(sample, "stored"), 1);
  assert.equal(metricIndex(sample, "relays"), 0);
  assert.equal(metricIndex(sample, null), 0);
  assert.deepEqual([...countryCounts(sample, 0)], [["US", 120_000], ["DE", 5_000]]);
  assert.equal(countryCounts(sample, 2).get("FR"), 10);
  assert.deepEqual(topBy(sample.countries, 1, 10).map((g) => g.country), ["de", "US", null]);
  assert.equal(topBy(sample.countries, 0, 1)[0].country, "US");
  assert.equal(totalOf(sample, 1), 9_000);
  assert.equal(totalOf({ ...sample, totals: [] }, 0), 120_000 + 5_000 + 9 + 1_000);
});

Deno.test("map parameters from the URL are checked", () => {
  assert.deepEqual(mapParams(new URLSearchParams("layer=nostr&window=all&metric=events")), { layer: "nostr", window: "all", metric: "events" });
  assert.deepEqual(mapParams(new URLSearchParams("layer=evil&window=x&metric=a%20b")), { layer: "dht", window: "day", metric: null });
});
