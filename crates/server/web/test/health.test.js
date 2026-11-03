// onFeaturesChange (B1 of the first browser check): a view mounted before the first /health
// answer, on a first sign-in with nothing cached, hears once that the features arrived, so the
// filter panel asks for the counts it skipped. A refresh that changes nothing stays quiet.

import assert from "node:assert/strict";
import { onFeaturesChange } from "../js/health.js";
import { observable } from "../js/store.js";

function fakeHealth() {
  const state = observable({ features: /** @type {ReadonlySet<string>} */ (new Set()), version: "", ok: /** @type {boolean | null} */ (null) });
  return {
    features: () => state.get().features,
    subscribe: state.subscribe,
    /** @param {string[]} f */
    answer: (f) => state.set({ features: new Set(f), version: "1", ok: true }),
    fail: () => state.set({ ...state.get(), ok: false }),
  };
}

Deno.test("the first /health answer after sign-in is heard once", () => {
  const health = fakeHealth();
  let calls = 0;
  const unsub = onFeaturesChange(health, () => calls++);
  health.answer(["facet_set", "filters"]);
  assert.equal(calls, 1, "the features arrived: the counts are asked");
  health.answer(["filters", "facet_set"]);
  assert.equal(calls, 1, "same features, other order: nothing to redo");
  health.fail();
  assert.equal(calls, 1, "an offline refresh keeps the features");
  health.answer(["facet_set", "filters", "probe_filters"]);
  assert.equal(calls, 2, "a server upgrade adds a feature");
  unsub();
  health.answer([]);
  assert.equal(calls, 2, "unsubscribed");
});
