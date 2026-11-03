// The server's REST API from the browser (docs/WEB.md "API client"): same-origin, the session
// cookie (never a token), X-CSRF-Token on state-changing calls, a timeout per request class as
// in the app (docs/APP.md §4), and one error type the views render.

/** Call timeouts per request class (ms), as the app's RequestClass. */
export const TIMEOUTS = Object.freeze({ SEARCH: 10_000, DETAIL: 15_000, CHECK: 45_000, STATS: 30_000 });
/** @typedef {keyof typeof TIMEOUTS} RequestClass */

/** What a call resolves to when its caller aborted it: views ignore it. */
export const ABORTED = Object.freeze({ aborted: true });

/**
 * @typedef {"unauthorized" | "forbidden" | "not_found" | "index_building" | "rate_limited" | "http" |
 *   "timeout" | "network" | "decode"} ErrorKind
 */

export class ApiError extends Error {
  /**
   * @param {ErrorKind} kind
   * @param {string} message
   * @param {{status?: number, code?: string, retryAfter?: number | null}} [extra]
   */
  constructor(kind, message, extra = {}) {
    super(message);
    this.name = "ApiError";
    /** @type {ErrorKind} */
    this.kind = kind;
    /** @type {number} */
    this.status = extra.status ?? 0;
    /** @type {string} */
    this.code = extra.code ?? "";
    /** @type {number | null} seconds, from Retry-After */
    this.retryAfter = extra.retryAfter ?? null;
  }
}

/**
 * What a view says for a failed call (Errors.kt; 401 reads as a session end on the web).
 * @param {unknown} e
 * @returns {string}
 */
export function userMessage(e) {
  if (!(e instanceof ApiError)) return e instanceof Error ? e.message : String(e);
  switch (e.kind) {
    case "unauthorized":
      return "Your session ended: sign in again";
    case "not_found":
      return "Not on this server";
    case "index_building":
      return "The server is building its search index";
    case "timeout":
      return "The server did not answer in time";
    case "network":
      return "Server unreachable";
    case "decode":
      return "Unexpected answer from the server";
    case "rate_limited":
      return e.message || "Too many requests: try again later";
    default:
      return e.message || `HTTP ${e.status}`;
  }
}

/**
 * Errors after which a cached page is still worth showing (with an "Offline" banner).
 * @param {unknown} e
 */
export function isOffline(e) {
  return e instanceof ApiError && (e.kind === "network" || e.kind === "timeout" || e.kind === "index_building");
}

/**
 * @param {unknown} v
 * @returns {v is typeof ABORTED}
 */
export function isAborted(v) {
  return v === ABORTED;
}

/**
 * @typedef {object} ApiHooks
 * @property {() => string | null} [csrf] the CSRF token kept in memory by session.js
 * @property {() => Promise<boolean>} [refreshCsrf] fetch the session again after a 403; true when renewed
 * @property {() => void} [onUnauthorized] a 401 on an authenticated call
 */

/**
 * @typedef {object} RequestOpts
 * @property {unknown} [body] JSON body
 * @property {AbortSignal} [signal] the caller's signal: aborting resolves to ABORTED
 * @property {RequestClass} [cls]
 * @property {boolean} [noAuthHooks] a 401 is the answer itself (sign-in), not a session end
 */

/** @param {string | null} v */
function retryAfterOf(v) {
  if (v == null) return null;
  const n = Number(v);
  return Number.isFinite(n) && n >= 0 ? Math.ceil(n) : null;
}

/**
 * @param {typeof fetch} [fetchFn]
 * @param {ApiHooks} [hooks]
 */
export function createApi(fetchFn, hooks = {}) {
  const doFetch = fetchFn ?? ((/** @type {RequestInfo | URL} */ i, /** @type {RequestInit} */ o) => fetch(i, o));

  /**
   * @param {string} method
   * @param {string} path same-origin, starting with "/"
   * @param {RequestOpts} [opts]
   * @param {boolean} [retried]
   * @returns {Promise<any>}
   */
  async function request(method, path, opts = {}, retried = false) {
    if (!path.startsWith("/") || path.startsWith("//")) throw new TypeError("api: same-origin paths only");
    const cls = opts.cls ?? "DETAIL";
    const caller = opts.signal;
    if (caller?.aborted) return ABORTED;
    const timeout = AbortSignal.timeout(TIMEOUTS[cls]);
    const signal = caller ? AbortSignal.any([caller, timeout]) : timeout;
    /** @type {Record<string, string>} */
    const headers = { accept: "application/json" };
    if (opts.body !== undefined) headers["content-type"] = "application/json";
    if (method !== "GET" && method !== "HEAD") {
      const t = hooks.csrf?.();
      if (t) headers["x-csrf-token"] = t;
    }
    /** @type {Response} */
    let res;
    let text;
    try {
      res = await doFetch(path, {
        method,
        headers,
        body: opts.body !== undefined ? JSON.stringify(opts.body) : undefined,
        signal,
        credentials: "same-origin",
        cache: "no-store",
        redirect: "error",
        referrerPolicy: "no-referrer",
      });
      text = await res.text();
    } catch (e) {
      if (caller?.aborted) return ABORTED;
      if (timeout.aborted) throw new ApiError("timeout", "The server did not answer in time");
      if (e instanceof DOMException && (e.name === "AbortError" || e.name === "TimeoutError")) {
        throw new ApiError("timeout", "The server did not answer in time");
      }
      throw new ApiError("network", "Server unreachable");
    }
    if (caller?.aborted) return ABORTED;
    if (res.ok) {
      if (res.status === 204 || text === "") return null;
      try {
        return JSON.parse(text);
      } catch {
        throw new ApiError("decode", "Unexpected answer from the server", { status: res.status });
      }
    }
    /** @type {{error?: unknown, message?: unknown}} */
    let body = {};
    try {
      const parsed = JSON.parse(text);
      if (parsed && typeof parsed === "object") body = parsed;
    } catch {
      // A non-JSON error page: the status says enough.
    }
    const code = typeof body.error === "string" ? body.error : "";
    const message = typeof body.message === "string" ? body.message : "";
    const extra = { status: res.status, code, retryAfter: retryAfterOf(res.headers.get("retry-after")) };
    if (res.status === 401) {
      if (!opts.noAuthHooks) hooks.onUnauthorized?.();
      throw new ApiError("unauthorized", message || "unauthorized", extra);
    }
    if (res.status === 403 && code === "forbidden") {
      if (!retried && !opts.noAuthHooks && hooks.refreshCsrf && method !== "GET") {
        const renewed = await hooks.refreshCsrf();
        if (renewed) return request(method, path, opts, true);
      }
      throw new ApiError("forbidden", message || "forbidden", extra);
    }
    if (res.status === 404) throw new ApiError("not_found", message || "Not on this server", extra);
    if (res.status === 503 && code === "search_index_unavailable") {
      throw new ApiError("index_building", message || "The server is building its search index", extra);
    }
    if (res.status === 429) throw new ApiError("rate_limited", message, extra);
    throw new ApiError("http", message || `HTTP ${res.status}`, extra);
  }

  return {
    request,
    /**
     * @param {string} path
     * @param {RequestOpts} [opts]
     */
    get: (path, opts) => request("GET", path, opts),
    /**
     * @param {string} path
     * @param {unknown} body
     * @param {RequestOpts} [opts]
     */
    post: (path, body, opts) => request("POST", path, { ...opts, body }),
    /**
     * @param {string} path
     * @param {unknown} body
     * @param {RequestOpts} [opts]
     */
    put: (path, body, opts) => request("PUT", path, { ...opts, body }),
    /**
     * @param {string} path
     * @param {RequestOpts} [opts]
     */
    del: (path, opts) => request("DELETE", path, opts),
    hooks,
  };
}

/** @typedef {ReturnType<typeof createApi>} Api */

/**
 * The path segment of an infohash, or a TypeError (never interpolate an unchecked value).
 * @param {string} ih
 */
export function ihSegment(ih) {
  const s = ih.toLowerCase();
  if (!/^[0-9a-f]{40}$/.test(s)) throw new TypeError("bad infohash");
  return s;
}
