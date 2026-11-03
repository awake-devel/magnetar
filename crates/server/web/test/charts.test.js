// Values from StatsFormatTest.kt (chart window, donut hit test) and FileTreeTest.kt (seed history).
import assert from "node:assert/strict";
import { LIGHT } from "../js/chart-colors.js";
import { ChartWindow, donutPath, seederSeries, seriesValue, sliceAt, slices, stepPath } from "../js/charts.js";

Deno.test("chart window zooms around the focus and pans within bounds", () => {
  const w = new ChartWindow(91);
  assert.equal(w.indexAt(0), 0);
  assert.equal(w.indexAt(1), 90);
  const z = w.zoom(3, 0.5);
  assert.equal(z.count, 30);
  assert.equal(z.indexAt(0.5), 45);
  assert.equal(z.pan(10).first, 0);
  assert.equal(z.pan(-10).first, 91 - 30);
  assert.equal(w.zoom(100, 0).count, ChartWindow.MIN);
  assert.ok(z.zoom(0.01, 0.5).equals(new ChartWindow(91, 0, 91)));
  assert.ok(new ChartWindow(5).zoom(4, 0.5).equals(new ChartWindow(5)));
});

Deno.test("donut hit test by angle", () => {
  const sizes = [50, 25, 25];
  assert.equal(sliceAt(10, sizes), 0);
  assert.equal(sliceAt(200, sizes), 1);
  assert.equal(sliceAt(300, sizes), 2);
  assert.equal(sliceAt(-10, sizes), 2);
  assert.equal(sliceAt(10, [0]), null);
});

Deno.test("slices keep their colour slot and fold the rest into Other", () => {
  const s = slices(
    { field: "category", total: 100, counts: [{ value: "audio", count: 30 }, { value: "zzz", count: 5 }, { value: "video", count: 60 }], other: 5 },
    ["video", "anime", "audio"],
    LIGHT,
  );
  assert.deepEqual(s.map((x) => [x.count?.value ?? null, x.n, x.color]), [
    ["audio", 30, LIGHT.categorical[2]],
    ["video", 60, LIGHT.categorical[0]],
    [null, 10, LIGHT.other],
  ]);
  assert.match(donutPath(50, 50, 40, 20, 0, 90), /^M50 10A40 40 0 0 1 90 50L70 50A20 20 0 0 0 50 30Z$/);
});

Deno.test("seed history skips unanswered points and takes the best of trackers and DHT", () => {
  const { seeders, leechers } = seederSeries([
    { at: 1, seeders: 3, leechers: 1, peers_est: 10 },
    { at: 2 },
    { at: 3, seeders: 4 },
  ]);
  assert.deepEqual(seeders, [[1, 10], [3, 4]]);
  assert.deepEqual(leechers, [[1, 1]]);
  assert.equal(stepPath([[0, 10], [5, 0]], 0, 10, 10, 100, 50), "M0 2L50 2L50 48L100 48");
  assert.equal(stepPath([], 0, 1, 1, 1, 1), "");
  assert.equal(seriesValue(1536, "bytes"), "1.50 KiB");
  assert.equal(seriesValue(1234, "count", "en-US"), "1,234");
});
