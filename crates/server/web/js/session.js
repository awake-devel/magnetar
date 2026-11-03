// The browser session (docs/WEB.md "Security model"): an HttpOnly cookie set by the server at
// sign-in. This page never stores the API token; it keeps only the CSRF token, in memory, and
// gets it back from `GET /api/v1/web/session` after a reload.

import { ApiError } from "./api.js";
import * as prefs from "./prefs.js";
import { patch } from "./store.js";

/**
 * @typedef {object} SessionInfo
 * @property {number | null} expiresAt
 * @property {number | null} createdAt
 * @property {boolean} ntfy the server has an ntfy topic for saved-search alerts
 */

/**
 * @param {{api: import("./api.js").Api, cache: import("./pagecache.js").PageCache,
 *   store: import("./store.js").AppStore, onSignedOut: (reason: "ended" | "signed_out") => void}} deps
 */
export function createSession(deps) {
  const { api, cache, store } = deps;
  /** @type {string | null} */
  let csrf = null;
  /** @type {SessionInfo} */
  let info = { expiresAt: null, createdAt: null, ntfy: false };
  /** @type {Set<() => void>} */
  const listeners = new Set();

  /** @param {any} s */
  function adopt(s) {
    csrf = typeof s?.csrf === "string" ? s.csrf : null;
    info = {
      expiresAt: typeof s?.expires_at === "number" ? s.expires_at : null,
      createdAt: typeof s?.created_at === "number" ? s.created_at : info.createdAt,
      ntfy: s?.ntfy === true,
    };
    prefs.setRaw(prefs.KEYS.signedIn, "1");
    patch(store, { signedIn: true });
    for (const l of listeners) l();
  }

  async function forget() {
    csrf = null;
    info = { expiresAt: null, createdAt: null, ntfy: false };
    prefs.setRaw(prefs.KEYS.signedIn, null);
    patch(store, { signedIn: false, trust: null });
    await cache.clear();
    for (const l of listeners) l();
  }

  api.hooks.csrf = () => csrf;
  api.hooks.refreshCsrf = async () => {
    try {
      const s = await api.get("/api/v1/web/session", { cls: "DETAIL", noAuthHooks: true });
      adopt(s);
      return true;
    } catch {
      return false;
    }
  };
  let ending = false;
  api.hooks.onUnauthorized = () => {
    if (ending) return;
    ending = true;
    void forget().finally(() => {
      ending = false;
      deps.onSignedOut("ended");
    });
  };

  /**
   * At start: is there a session? "offline" when the server could not be asked (the page then
   * runs on its cache if this browser was signed in); "ended" when this browser was signed in
   * and the server no longer knows the session (rotated token, expired, signed out elsewhere),
   * so the sign-in page can say so after a full page load too.
   * @returns {Promise<"ok" | "signed_out" | "ended" | "offline">}
   */
  async function boot() {
    try {
      const s = await api.get("/api/v1/web/session", { cls: "DETAIL", noAuthHooks: true });
      adopt(s);
      return "ok";
    } catch (e) {
      const was = prefs.getRaw(prefs.KEYS.signedIn) === "1";
      if (e instanceof ApiError && (e.kind === "unauthorized" || e.kind === "not_found")) {
        await forget();
        return was && e.kind === "unauthorized" ? "ended" : "signed_out";
      }
      patch(store, { signedIn: was, offline: true });
      return was ? "offline" : "signed_out";
    }
  }

  /**
   * Opens a session with the API token. The token is sent once and kept nowhere.
   * @param {string} token
   */
  async function login(token) {
    const s = await api.post("/api/v1/web/login", { token }, { cls: "DETAIL", noAuthHooks: true });
    adopt(s);
  }

  /**
   * Signs this browser (or every browser) out.
   * @param {boolean} [all]
   */
  async function logout(all = false) {
    try {
      await api.post(all ? "/api/v1/web/logout?all=1" : "/api/v1/web/logout", undefined, {
        cls: "DETAIL",
        noAuthHooks: true,
      });
    } finally {
      await forget();
      deps.onSignedOut("signed_out");
    }
  }

  return {
    boot,
    login,
    logout,
    /** @returns {SessionInfo} */
    info: () => info,
    signedIn: () => store.get().signedIn,
    /** @param {() => void} fn */
    subscribe(fn) {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
  };
}

/** @typedef {ReturnType<typeof createSession>} Session */

/**
 * The server's trust settings, for "Min trust: N (server threshold)" and the trust badges.
 * @param {import("./types.js").Ctx} ctx
 */
export async function loadTrust(ctx) {
  try {
    const t = await ctx.api.get("/api/v1/settings/trust", { cls: "DETAIL" });
    if (t && typeof t.threshold === "number") {
      patch(ctx.store, { trust: { threshold: t.threshold, root: typeof t.root === "string" ? t.root : null } });
    }
  } catch {
    // Offline: the badges use the default threshold.
  }
}
