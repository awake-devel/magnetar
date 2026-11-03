// A tiny observable value, and the app-wide state every view can read (docs/WEB.md).

/**
 * @template T
 * @param {T} initial
 */
export function observable(initial) {
  let value = initial;
  /** @type {Set<(v: T) => void>} */
  const subs = new Set();
  return {
    get: () => value,
    /** @param {T} v */
    set(v) {
      value = v;
      for (const s of [...subs]) s(v);
    },
    /**
     * @param {(v: T) => void} fn
     * @returns {() => void} unsubscribe
     */
    subscribe(fn) {
      subs.add(fn);
      return () => subs.delete(fn);
    },
  };
}

/**
 * @typedef {object} AppState
 * @property {boolean} signedIn
 * @property {boolean} offline the last server call failed with a network error
 * @property {{threshold: number, root: string | null} | null} trust the server's trust settings (null = unknown)
 * @property {string | null} swUpdate set when a new version took over: "Updated: reload"
 */

export function createAppStore() {
  return observable(/** @type {AppState} */ ({ signedIn: false, offline: false, trust: null, swUpdate: null }));
}

/** @typedef {ReturnType<typeof createAppStore>} AppStore */

/**
 * @param {AppStore} store
 * @param {Partial<AppState>} patch
 */
export function patch(store, patch) {
  store.set({ ...store.get(), ...patch });
}
