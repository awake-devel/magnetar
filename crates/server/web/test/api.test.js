import assert from "node:assert/strict";
import { ABORTED, ApiError, createApi, isOffline, TIMEOUTS, userMessage } from "../js/api.js";

/**
 * @param {number} status
 * @param {string} body
 * @param {Record<string, string>} [headers]
 */
const reply = (status, body, headers = {}) => Promise.resolve(new Response(body === "" ? null : body, { status, headers }));

/** @param {() => Promise<unknown>} fn */
async function errorOf(fn) {
  try {
    await fn();
  } catch (e) {
    if (e instanceof ApiError) return e;
    throw e;
  }
  throw new Error("no error");
}

Deno.test("JSON answers, 204 and the CSRF header on state-changing calls only", async () => {
  /** @type {{url: string, init: RequestInit}[]} */
  const seen = [];
  const api = createApi((url, init) => {
    seen.push({ url: String(url), init: /** @type {RequestInit} */ (init) });
    return String(url).includes("del") ? reply(204, "") : reply(200, '{"ok":1}');
  }, { csrf: () => "tok" });
  assert.deepEqual(await api.get("/api/v1/x"), { ok: 1 });
  assert.deepEqual(await api.post("/api/v1/x", { a: 1 }), { ok: 1 });
  assert.equal(await api.del("/api/v1/del"), null);
  const h0 = /** @type {Record<string, string>} */ (seen[0].init.headers);
  const h1 = /** @type {Record<string, string>} */ (seen[1].init.headers);
  assert.equal(h0["x-csrf-token"], undefined);
  assert.equal(h1["x-csrf-token"], "tok");
  assert.equal(h1["content-type"], "application/json");
  assert.equal(seen[1].init.body, '{"a":1}');
  assert.equal(seen[1].init.credentials, "same-origin");
  await assert.rejects(() => api.get("//evil.example/x"), TypeError);
  await assert.rejects(() => api.get("https://evil.example/x"), TypeError);
});

Deno.test("errors map to kinds and messages (C-ERR-03, C-ERR-04, C-ERR-05)", async () => {
  let unauthorized = 0;
  /** @type {[number, string, Record<string, string>?][]} */
  const answers = [
    [404, '{"error":"not_found","message":"no such torrent"}'],
    [503, '{"error":"search_index_unavailable","message":"building"}'],
    [400, '{"error":"invalid","message":"offset too large"}'],
    [500, "<html>oops</html>"],
    [200, "{not json"],
    [429, '{"error":"rate_limited","message":"too many seed checks"}', { "retry-after": "7" }],
    [401, '{"error":"unauthorized","message":"no"}'],
  ];
  let i = 0;
  const api = createApi(() => {
    const [s, b, h] = answers[i++];
    return reply(s, b, h);
  }, { onUnauthorized: () => unauthorized++ });
  const nf = await errorOf(() => api.get("/a"));
  assert.equal(nf.kind, "not_found");
  assert.equal(userMessage(nf), "Not on this server");
  const ib = await errorOf(() => api.post("/a", {}));
  assert.equal(ib.kind, "index_building");
  assert.equal(isOffline(ib), true);
  assert.equal(userMessage(ib), "The server is building its search index");
  const inv = await errorOf(() => api.get("/a"));
  assert.equal(inv.kind, "http");
  assert.equal(userMessage(inv), "offset too large");
  const html = await errorOf(() => api.get("/a"));
  assert.equal(userMessage(html), "HTTP 500");
  const dec = await errorOf(() => api.get("/a"));
  assert.equal(dec.kind, "decode");
  assert.equal(userMessage(dec), "Unexpected answer from the server");
  const rl = await errorOf(() => api.post("/a", {}));
  assert.equal(rl.kind, "rate_limited");
  assert.equal(rl.retryAfter, 7);
  const ua = await errorOf(() => api.get("/a"));
  assert.equal(ua.kind, "unauthorized");
  assert.equal(unauthorized, 1);
  assert.equal(userMessage(ua), "Your session ended: sign in again");
});

Deno.test("network failure and timeout (C-ERR-01, C-ERR-02)", async () => {
  const down = createApi(() => Promise.reject(new TypeError("Failed to fetch")));
  const n = await errorOf(() => down.get("/health"));
  assert.equal(n.kind, "network");
  assert.equal(userMessage(n), "Server unreachable");
  assert.equal(isOffline(n), true);
  // A fetch that never answers on its own: only the signal ends it.
  const hang = createApi((_u, init) =>
    new Promise((_res, rej) => {
      const s = /** @type {AbortSignal} */ (init?.signal);
      s.addEventListener("abort", () => rej(s.reason));
    })
  );
  const realTimeout = AbortSignal.timeout;
  AbortSignal.timeout = () => realTimeout.call(AbortSignal, 5);
  try {
    const t = await errorOf(() => hang.get("/slow", { cls: "SEARCH" }));
    assert.equal(t.kind, "timeout");
    assert.equal(userMessage(t), "The server did not answer in time");
  } finally {
    AbortSignal.timeout = realTimeout;
  }
  assert.equal(TIMEOUTS.CHECK, 45_000);
});

Deno.test("a caller's abort is not an error", async () => {
  const api = createApi((_u, init) =>
    new Promise((_res, rej) => {
      const s = /** @type {AbortSignal} */ (init?.signal);
      s.addEventListener("abort", () => rej(new DOMException("aborted", "AbortError")));
    })
  );
  const ac = new AbortController();
  const p = api.get("/x", { signal: ac.signal });
  ac.abort();
  assert.equal(await p, ABORTED);
  assert.equal(await api.get("/x", { signal: ac.signal }), ABORTED);
});

Deno.test("a 403 CSRF answer renews the session once and retries once", async () => {
  let calls = 0;
  let renewals = 0;
  const api = createApi(() => {
    calls++;
    return calls === 1 ? reply(403, '{"error":"forbidden","message":"missing or bad CSRF token"}') : reply(200, "{}");
  }, { refreshCsrf: () => Promise.resolve(++renewals > 0) });
  assert.deepEqual(await api.post("/x", {}), {});
  assert.equal(calls, 2);
  assert.equal(renewals, 1);
  let c2 = 0;
  const always = createApi(() => {
    c2++;
    return reply(403, '{"error":"forbidden","message":"missing or bad CSRF token"}');
  }, { refreshCsrf: () => Promise.resolve(true) });
  const e = await errorOf(() => always.post("/x", {}));
  assert.equal(e.kind, "forbidden");
  assert.equal(c2, 2);
});
