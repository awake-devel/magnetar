// Recent server answers in IndexedDB (docs/WEB.md "Caching and offline"), the web's copy of the
// app's PageCache (D83): keyed by SHA-256 of (endpoint, canonical request body incl. the adult
// switch), at most MAX_ENTRIES, least recently used out first. Only server answers go in.
// Every IndexedDB call is wrapped: the app works without it (private windows, blocked storage).

export const DB_NAME = "magnetar-pages";
export const STORE = "pages";
export const MAX_ENTRIES = 300;

/**
 * JSON with object keys sorted at every level, so equal bodies give equal keys.
 * @param {unknown} v
 * @returns {string}
 */
export function canonicalJson(v) {
  if (v === null || typeof v !== "object") return JSON.stringify(v ?? null);
  if (Array.isArray(v)) return `[${v.map((x) => canonicalJson(x === undefined ? null : x)).join(",")}]`;
  const o = /** @type {Record<string, unknown>} */ (v);
  const keys = Object.keys(o).filter((k) => o[k] !== undefined).sort();
  return `{${keys.map((k) => `${JSON.stringify(k)}:${canonicalJson(o[k])}`).join(",")}}`;
}

/**
 * The cache key of an endpoint and its request (hex SHA-256).
 * @param {string} endpoint e.g. "search", "detail/<ih>", "title/<id>", "stats/health"
 * @param {unknown} body the request as asked (for search: cacheQuery, with the adult switch)
 */
export async function cacheKey(endpoint, body) {
  const data = new TextEncoder().encode(`${endpoint}\n${canonicalJson(body)}`);
  const digest = await crypto.subtle.digest("SHA-256", data);
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/**
 * @typedef {object} CacheEntry
 * @property {number} savedAt unix ms
 * @property {any} json
 */

/**
 * @template T
 * @param {IDBRequest<T>} req
 * @returns {Promise<T>}
 */
function done(req) {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

/**
 * How long opening the database may take. An open queued behind a blocked delete or upgrade
 * (site data cleared, or a newer version, while another tab holds the old connection) gets no
 * event at all; without this limit the boot's sign-out path, which clears the cache, waited
 * forever and left a blank page.
 */
export const OPEN_TIMEOUT_MS = 2000;

/**
 * @param {() => void} onVersionChange
 * @returns {Promise<IDBDatabase | null>}
 */
function openDb(onVersionChange) {
  return new Promise((resolve) => {
    let settled = false;
    /** @param {IDBDatabase | null} v */
    const finish = (v) => {
      if (settled) {
        // Too late: give the connection back so it blocks nobody.
        v?.close();
        return;
      }
      settled = true;
      clearTimeout(timer);
      resolve(v);
    };
    const timer = setTimeout(() => finish(null), OPEN_TIMEOUT_MS);
    try {
      const req = indexedDB.open(DB_NAME, 1);
      req.onupgradeneeded = () => {
        const db = req.result;
        if (!db.objectStoreNames.contains(STORE)) {
          const s = db.createObjectStore(STORE, { keyPath: "key" });
          s.createIndex("usedAt", "usedAt");
        }
      };
      req.onsuccess = () => {
        const db = req.result;
        // Another tab (or "Clear site data") wants to delete or upgrade it: let go at once.
        db.onversionchange = () => {
          db.close();
          onVersionChange();
        };
        finish(db);
      };
      req.onerror = () => finish(null);
      req.onblocked = () => finish(null);
    } catch {
      finish(null);
    }
  });
}

export function createPageCache() {
  /** @type {Promise<IDBDatabase | null> | null} */
  let dbp = null;
  const db = () => (dbp ??= openDb(() => (dbp = null)).then((d) => {
    // Not available now (blocked, timed out): ask again next time instead of never.
    if (!d) dbp = null;
    return d;
  }));

  /**
   * @param {string} endpoint
   * @param {unknown} body
   * @returns {Promise<CacheEntry | null>}
   */
  async function get(endpoint, body) {
    try {
      const d = await db();
      if (!d) return null;
      const key = await cacheKey(endpoint, body);
      const tx = d.transaction(STORE, "readwrite");
      const s = tx.objectStore(STORE);
      const rec = await done(s.get(key));
      if (!rec) return null;
      rec.usedAt = Date.now();
      s.put(rec);
      return { savedAt: rec.savedAt, json: rec.json };
    } catch {
      return null;
    }
  }

  /**
   * @param {string} endpoint
   * @param {unknown} body
   * @param {unknown} json
   */
  async function put(endpoint, body, json) {
    try {
      const d = await db();
      if (!d) return;
      const key = await cacheKey(endpoint, body);
      const now = Date.now();
      const tx = d.transaction(STORE, "readwrite");
      const s = tx.objectStore(STORE);
      s.put({ key, savedAt: now, usedAt: now, json });
      const count = await done(s.count());
      if (count > MAX_ENTRIES) {
        let extra = count - MAX_ENTRIES;
        const cur = s.index("usedAt").openCursor();
        cur.onsuccess = () => {
          const c = cur.result;
          if (!c || extra <= 0) return;
          c.delete();
          extra--;
          c.continue();
        };
      }
    } catch {
      // A convenience only.
    }
  }

  /** Sign-out and 401: nothing of this session stays. */
  async function clearAll() {
    try {
      const d = await db();
      if (!d) return;
      await done(d.transaction(STORE, "readwrite").objectStore(STORE).clear());
    } catch {
      // ignore
    }
  }

  return { get, put, clear: clearAll };
}

/** @typedef {ReturnType<typeof createPageCache>} PageCache */
