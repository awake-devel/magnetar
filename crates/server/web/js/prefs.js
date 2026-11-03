// Per-browser conveniences in localStorage (docs/WEB.md "Caching and offline"). Nothing secret:
// no token, no CSRF token, no trust settings (those live on the server). Every access is wrapped:
// storage can be missing or throw (private windows, blocked site data).

export const KEYS = Object.freeze({
  showAdult: "magnetar.showAdult",
  features: "magnetar.features",
  statsTab: "magnetar.stats.tab",
  activityMetric: "magnetar.activity.metric",
  activityBucket: "magnetar.activity.bucket",
  lastSearch: "magnetar.lastSearch",
  signedIn: "magnetar.signedIn",
});

/** @type {Set<(key: string) => void>} */
const listeners = new Set();

/**
 * @param {string} key
 * @returns {string | null}
 */
export function getRaw(key) {
  try {
    return globalThis.localStorage?.getItem(key) ?? null;
  } catch {
    return null;
  }
}

/**
 * @param {string} key
 * @param {string | null} value null removes it
 */
export function setRaw(key, value) {
  try {
    if (value == null) globalThis.localStorage?.removeItem(key);
    else globalThis.localStorage?.setItem(key, value);
  } catch {
    // ignore
  }
  for (const l of listeners) {
    try {
      l(key);
    } catch {
      // a listener's failure is its own
    }
  }
}

/**
 * @template T
 * @param {string} key
 * @param {T} fallback
 * @returns {T}
 */
export function getJson(key, fallback) {
  const raw = getRaw(key);
  if (raw == null) return fallback;
  try {
    return JSON.parse(raw);
  } catch {
    return fallback;
  }
}

/**
 * @param {string} key
 * @param {unknown} value
 */
export function setJson(key, value) {
  setRaw(key, JSON.stringify(value));
}

/** "Show adult content" (D52), off by default. */
export function showAdult() {
  return getRaw(KEYS.showAdult) === "true";
}

/** @param {boolean} on */
export function setShowAdult(on) {
  setRaw(KEYS.showAdult, on ? "true" : "false");
}

/**
 * Called with the key after every change made through this module.
 * @param {(key: string) => void} fn
 * @returns {() => void} unsubscribe
 */
export function onChange(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}
