// The palettes of ChartColors.kt, copied exactly; slots never cycle.
import assert from "node:assert/strict";
import { chartColors, DARK, lerpColor, LIGHT, parseHex, slotOf, withAlpha } from "../js/chart-colors.js";

Deno.test("both palettes match the app and pick by scheme", () => {
  assert.deepEqual([...LIGHT.categorical], ["#2A78D6", "#EB6834", "#1BAF7A", "#EDA100", "#E87BA4", "#008300", "#6250D6", "#E34948"]);
  assert.deepEqual([...DARK.categorical], ["#3987E5", "#D95926", "#199E70", "#C98500", "#D55181", "#008300", "#9085E9", "#E66767"]);
  assert.equal(LIGHT.mapHigh, "#0B3D82");
  assert.equal(DARK.mapLow, "#1C3352");
  assert.equal(chartColors(true), DARK);
  assert.equal(chartColors(false), LIGHT);
  for (const p of [LIGHT, DARK]) {
    for (const [k, v] of Object.entries(p)) {
      if (k === "categorical") continue;
      assert.doesNotThrow(() => parseHex(/** @type {string} */ (v)), k);
    }
  }
});

Deno.test("slots, mixes and alpha", () => {
  const order = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
  assert.equal(slotOf(LIGHT, "b", order), LIGHT.categorical[1]);
  assert.equal(slotOf(LIGHT, "i", order), null, "past the last slot: Other, never cycled");
  assert.equal(slotOf(LIGHT, "zz", order), null);
  assert.equal(lerpColor("#000000", "#ffffff", 0.5), "#808080");
  assert.equal(lerpColor(LIGHT.mapLow, LIGHT.mapHigh, 0), "#cfe2f8");
  assert.equal(lerpColor(LIGHT.mapLow, LIGHT.mapHigh, 1), "#0b3d82");
  assert.deepEqual(parseHex("#0B0B0B66"), [11, 11, 11, 0x66 / 255]);
  assert.equal(withAlpha("#ff0000", 0.5), "rgb(255 0 0 / 50%)");
  assert.throws(() => parseHex("red"));
});
