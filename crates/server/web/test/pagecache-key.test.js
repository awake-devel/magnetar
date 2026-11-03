import assert from "node:assert/strict";
import { cacheKey, canonicalJson } from "../js/pagecache.js";
import { cacheQuery, makeFilters } from "../js/search-model.js";

Deno.test("canonical JSON sorts keys at every level", () => {
  assert.equal(canonicalJson({ b: 1, a: { d: [1, { z: 1, y: 2 }], c: null } }), '{"a":{"c":null,"d":[1,{"y":2,"z":1}]},"b":1}');
  assert.equal(canonicalJson({ a: undefined, b: 2 }), '{"b":2}');
  assert.equal(canonicalJson("x"), '"x"');
});

Deno.test("cache keys: same request same key, any difference another key", async () => {
  const a = await cacheKey("search", { text: "dune", include_adult: false });
  assert.match(a, /^[0-9a-f]{64}$/);
  assert.equal(await cacheKey("search", { include_adult: false, text: "dune" }), a);
  assert.notEqual(await cacheKey("search", { text: "dune", include_adult: true }), a);
  assert.notEqual(await cacheKey("title/1", { text: "dune", include_adult: false }), a);
  // The date preset, not the clock: the same key tomorrow.
  const f = makeFilters({ date: "week" });
  const k1 = await cacheKey("search", cacheQuery("x", f, "relevance", true, false));
  const k2 = await cacheKey("search", cacheQuery("x", f, "relevance", true, false));
  assert.equal(k1, k2);
});
