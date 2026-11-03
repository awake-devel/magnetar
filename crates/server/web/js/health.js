// The server's `/health` features (docs/WEB.md "Feature gating"): the gate of every
// feature-dependent control, cached in localStorage so an offline start still knows them.

import { ApiError } from "./api.js";
import * as prefs from "./prefs.js";
import { observable } from "./store.js";

/** Refreshed this often, and when the tab becomes visible again. */
export const HEALTH_REFRESH_MS = 5 * 60_000;

/**
 * @typedef {object} HealthSnapshot
 * @property {ReadonlySet<string>} features
 * @property {string} version
 * @property {boolean | null} ok the last refresh answered (null = not asked yet)
 */

/**
 * @param {import("./api.js").Api} api
 */
export function createHealth(api) {
  const cached = prefs.getJson(prefs.KEYS.features, /** @type {{features?: unknown, version?: unknown}} */ ({}));
  const initialFeatures = Array.isArray(cached.features) ? cached.features.filter((f) => typeof f === "string") : [];
  const state = observable(/** @type {HealthSnapshot} */ ({
    features: new Set(initialFeatures),
    version: typeof cached.version === "string" ? cached.version : "",
    ok: null,
  }));
  let fetchedAt = 0;
  /** @type {Promise<HealthSnapshot | null> | null} */
  let inflight = null;

  /**
   * @param {{force?: boolean}} [opts]
   * @returns {Promise<HealthSnapshot | null>} null when the server did not answer
   */
  function refresh(opts = {}) {
    if (!opts.force && state.get().ok === true && Date.now() - fetchedAt < HEALTH_REFRESH_MS) {
      return Promise.resolve(state.get());
    }
    if (inflight) return inflight;
    inflight = (async () => {
      try {
        const h = await api.get("/health", { cls: "DETAIL" });
        const features = Array.isArray(h?.features) ? h.features.filter((/** @type {unknown} */ f) => typeof f === "string") : [];
        const version = typeof h?.version === "string" ? h.version : "";
        fetchedAt = Date.now();
        prefs.setJson(prefs.KEYS.features, { features, version });
        state.set({ features: new Set(features), version, ok: true });
        return state.get();
      } catch (e) {
        if (e instanceof ApiError) {
          state.set({ ...state.get(), ok: false });
          return null;
        }
        throw e;
      } finally {
        inflight = null;
      }
    })();
    return inflight;
  }

  /** @param {string} feature */
  const has = (feature) => state.get().features.has(feature);

  /** Starts the 5-minute refresh and the refresh on becoming visible. */
  function startAutoRefresh() {
    setInterval(() => void refresh(), HEALTH_REFRESH_MS);
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") void refresh();
    });
  }

  return {
    refresh,
    has,
    startAutoRefresh,
    /** @returns {ReadonlySet<string>} */
    features: () => state.get().features,
    version: () => state.get().version,
    subscribe: state.subscribe,
    get: state.get,
  };
}

/** @typedef {ReturnType<typeof createHealth>} HealthState */

/**
 * Calls `fn` when the set of features changes (not on every refresh). A view mounted before
 * the first `/health` answer (a first sign-in: nothing cached yet) uses this to ask again what
 * it skipped, such as the filter counts behind `facet_set`.
 * @param {{features: () => ReadonlySet<string>, subscribe: (fn: (v: HealthSnapshot) => void) => () => void}} health
 * @param {() => void} fn
 * @returns {() => void} unsubscribe
 */
export function onFeaturesChange(health, fn) {
  const key = () => [...health.features()].sort().join(",");
  let last = key();
  return health.subscribe(() => {
    const k = key();
    if (k === last) return;
    last = k;
    fn();
  });
}
