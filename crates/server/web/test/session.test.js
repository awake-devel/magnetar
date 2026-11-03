// Session boot (B2 of the first browser check): a full page load that finds the server no longer
// knows this browser's session says "ended" (the sign-in page then shows "Your session ended"),
// while a browser that never signed in, or a server with the web UI off, is plainly signed out.

import assert from "node:assert/strict";
import { ApiError } from "../js/api.js";
import * as prefs from "../js/prefs.js";
import { createSession } from "../js/session.js";
import { createAppStore } from "../js/store.js";

/** @param {ApiError | null} fail */
function boot(fail) {
  const api = /** @type {any} */ ({
    hooks: {},
    get: () => fail ? Promise.reject(fail) : Promise.resolve({ csrf: "c", expires_at: 1, created_at: 0, ntfy: false }),
  });
  const cache = /** @type {any} */ ({ clear: () => Promise.resolve() });
  const store = createAppStore();
  return createSession({ api, cache, store, onSignedOut: () => {} }).boot();
}

// Deno's localStorage persists between runs: every case sets the flag it needs and clears it.
Deno.test("a 401 at boot after this browser was signed in is an ended session", async () => {
  try {
    prefs.setRaw(prefs.KEYS.signedIn, "1");
    assert.equal(await boot(new ApiError("unauthorized", "x", { status: 401 })), "ended");
    assert.equal(prefs.getRaw(prefs.KEYS.signedIn), null, "the flag is cleared: the next load is plainly signed out");
    assert.equal(await boot(new ApiError("unauthorized", "x", { status: 401 })), "signed_out");
    prefs.setRaw(prefs.KEYS.signedIn, "1");
    assert.equal(await boot(new ApiError("not_found", "x", { status: 404 })), "signed_out", "web UI off");
    prefs.setRaw(prefs.KEYS.signedIn, "1");
    assert.equal(await boot(null), "ok");
  } finally {
    prefs.setRaw(prefs.KEYS.signedIn, null);
  }
});
