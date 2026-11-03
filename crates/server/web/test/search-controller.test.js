// The last search survives a full page load on another route (B1 of the second browser check):
// Saved and the title page read the search controller before the Search view ever mounts, and
// the Search tab's link comes from it, so a new controller starts from this browser's last search.

import assert from "node:assert/strict";
import * as prefs from "../js/prefs.js";
import { createAppStore } from "../js/store.js";
import { createSearchController } from "../js/views/search-controller.js";

function controller() {
  const ctx = /** @type {any} */ ({ store: createAppStore(), health: { features: () => new Set() } });
  return createSearchController(ctx);
}

// Deno's localStorage persists between runs: every case sets what it needs and clears it.
Deno.test("a new search controller starts from the last search of this browser", () => {
  try {
    prefs.setRaw(prefs.KEYS.lastSearch, "q=dune&res=2160p");
    const c = controller();
    const st = c.get().state;
    assert.equal(st.text, "dune");
    assert.deepEqual([...st.filters.resolutions], ["2160p"]);
    assert.equal(c.href(), "/?q=dune&res=2160p", "the Search tab links to the last search");
    assert.equal(c.get().ran, false, "nothing runs until the Search view asks");
  } finally {
    prefs.setRaw(prefs.KEYS.lastSearch, null);
  }
});

Deno.test("without a last search the controller starts empty", () => {
  prefs.setRaw(prefs.KEYS.lastSearch, null);
  const c = controller();
  assert.equal(c.get().state.text, "");
  assert.equal(c.href(), "/");
});
