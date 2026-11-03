// The page cache never hangs the app: an IndexedDB open queued behind a blocked delete gets no
// event at all (seen in Chrome after clearing site data while another tab held the database);
// the boot's sign-out path clears the cache, so a hang there left a blank page.

import assert from "node:assert/strict";
import { createPageCache, OPEN_TIMEOUT_MS } from "../js/pagecache.js";

Deno.test("an open that never answers gives up, and the next call asks again", async () => {
  let opens = 0;
  const g = /** @type {any} */ (globalThis);
  const before = g.indexedDB;
  // An open request that fires nothing, like one queued behind a blocked deleteDatabase.
  g.indexedDB = { open: () => (opens++, {}) };
  try {
    const cache = createPageCache();
    const t0 = Date.now();
    await cache.clear();
    const waited = Date.now() - t0;
    assert.ok(waited >= OPEN_TIMEOUT_MS - 50 && waited < OPEN_TIMEOUT_MS + 1000, `gave up after ${waited} ms`);
    assert.equal(await cache.get("search", { q: 1 }), null);
    assert.equal(opens, 2, "a failed open is not kept: the next call tries again");
  } finally {
    g.indexedDB = before;
  }
});
