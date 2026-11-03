// History-API router (docs/WEB.md "Routing and URL parameters"). Route matching is pure and
// tested; the Router mounts one view at a time into the shell's outlet.

import { safeNext } from "./dom.js";

/**
 * @typedef {object} RouteDef
 * @property {string} name
 * @property {string} path pattern, e.g. "/torrent/:ih"
 * @property {string} title the tab title part (never API text)
 * @property {() => Promise<{mount: (el: HTMLElement, args: import("./types.js").ViewArgs) => import("./types.js").ViewHandle}>} load
 * @property {boolean} [public] reachable signed out (the login page)
 * @property {string} [nav] the tab it belongs to: search | saved | stats | settings
 * @property {string} [paneOf] on medium and wide layouts, render inside this route's view
 * @property {string} [redirect] always replaced by this path (keeping the query)
 * @property {string} [view] routes with the same view key share one mounted view (update() instead of a new mount)
 */

/** Parameter validators: a route matches only when every parameter passes. */
const PARAM = /** @type {Record<string, (v: string) => string | null>} */ ({
  ih: (v) => (/^[0-9a-fA-F]{40}$/.test(v) ? v.toLowerCase() : null),
  id: (v) => (/^\d{1,18}$/.test(v) && Number.isSafeInteger(Number(v)) ? String(Number(v)) : null),
});

/**
 * @param {string} pattern
 * @param {string} pathname
 * @returns {Record<string, string> | null}
 */
export function matchPath(pattern, pathname) {
  const p = pathname.length > 1 ? pathname.replace(/\/+$/, "") : pathname;
  const a = pattern.split("/");
  const b = p.split("/");
  if (a.length !== b.length) return null;
  /** @type {Record<string, string>} */
  const params = {};
  for (let i = 0; i < a.length; i++) {
    if (a[i].startsWith(":")) {
      const name = a[i].slice(1);
      let raw;
      try {
        raw = decodeURIComponent(b[i]);
      } catch {
        return null;
      }
      const check = PARAM[name];
      const v = check ? check(raw) : raw;
      if (v == null) return null;
      params[name] = v;
    } else if (a[i] !== b[i]) {
      return null;
    }
  }
  return params;
}

/**
 * @param {readonly RouteDef[]} routes
 * @param {string} pathname
 * @returns {{route: RouteDef, params: Record<string, string>} | null}
 */
export function matchRoute(routes, pathname) {
  for (const route of routes) {
    const params = matchPath(route.path, pathname);
    if (params) return { route, params };
  }
  return null;
}

/**
 * Where to go after sign-in: the validated `next`, else "/".
 * @param {string | null} next
 */
export function nextTarget(next) {
  return next != null && safeNext(next) && !next.startsWith("/login") ? next : "/";
}

/**
 * The login URL that comes back to `here` (a same-origin path). `ended` adds `ended=1`, which
 * makes the sign-in page say that the session ended (it stays in the URL until sign-in).
 * @param {string} here pathname + search
 * @param {boolean} [ended]
 */
export function loginUrl(here, ended = false) {
  const base = safeNext(here) && !here.startsWith("/login") && here !== "/"
    ? `/login?next=${encodeURIComponent(here)}`
    : "/login?next=%2F";
  return ended ? `${base}&ended=1` : base;
}

/**
 * The sign-in page after a session ended while this browser was on `pathname` + `search`: it
 * comes back there, or keeps the `next` of a sign-in page the router already showed.
 * @param {string} pathname
 * @param {string} search
 */
export function endedLoginUrl(pathname, search) {
  const here = pathname === "/login" ? (new URLSearchParams(search).get("next") ?? "/") : pathname + search;
  return loginUrl(here, true);
}

/**
 * Shown when a view's module cannot be loaded (offline, not cached).
 * @param {HTMLElement} el
 * @returns {import("./types.js").ViewHandle}
 */
function unavailableView(el) {
  const p = document.createElement("p");
  p.className = "centered";
  p.textContent = "Server unreachable: this page is not available offline yet.";
  const b = document.createElement("button");
  b.type = "button";
  b.className = "btn tonal";
  b.textContent = "Retry";
  b.addEventListener("click", () => location.reload());
  el.append(p, b);
  return { unmount() {} };
}

/**
 * @typedef {object} RouterDeps
 * @property {readonly RouteDef[]} routes
 * @property {RouteDef} notFound
 * @property {HTMLElement} outlet
 * @property {() => import("./types.js").Ctx} ctx
 * @property {() => boolean} signedIn
 * @property {(route: RouteDef) => void} onRoute the shell marks the tab and shows or hides the nav
 */

/** @param {RouterDeps} deps */
export function createRouter(deps) {
  /** @type {{key: string, route: RouteDef, handle: import("./types.js").ViewHandle, layout: string} | null} */
  let current = null;
  let generation = 0;
  /** @type {Map<string, number>} */
  const scrolls = new Map();
  let entryKey = "";
  let counter = 0;

  function stateKey() {
    const s = history.state;
    return s && typeof s.key === "string" ? s.key : "";
  }

  function saveScroll() {
    if (entryKey) scrolls.set(entryKey, globalThis.scrollY);
  }

  /**
   * @param {string} to
   * @param {{replace?: boolean}} [opts]
   */
  function nav(to, opts = {}) {
    if (!safeNext(to)) return;
    saveScroll();
    const idx = typeof history.state?.idx === "number" ? history.state.idx : 0;
    const key = `k${Date.now().toString(36)}${(counter++).toString(36)}`;
    if (opts.replace) history.replaceState({ key, idx }, "", to);
    else history.pushState({ key, idx: idx + 1 }, "", to);
    void render(false);
  }

  /** Back to the previous page, or Search when this page was opened directly. */
  function back() {
    if (typeof history.state?.idx === "number" && history.state.idx > 0) history.back();
    else nav("/", { replace: true });
  }

  /** @param {boolean} pop */
  async function render(pop) {
    const gen = ++generation;
    const url = new URL(location.href);
    let m = matchRoute(deps.routes, url.pathname);
    if (m?.route.redirect) {
      history.replaceState(history.state, "", m.route.redirect + url.search);
      return render(pop);
    }
    if (m && !m.route.public && !deps.signedIn()) {
      history.replaceState(history.state, "", loginUrl(url.pathname + url.search));
      return render(pop);
    }
    if (m?.route.name === "login" && deps.signedIn()) {
      history.replaceState(history.state, "", nextTarget(url.searchParams.get("next")));
      return render(pop);
    }
    if (!m) m = { route: deps.notFound, params: {} };
    const ctx = deps.ctx();
    let route = m.route;
    const params = m.params;
    // Medium and wide: the detail renders in the search workspace's pane.
    if (route.paneOf && ctx.layout !== "phone") {
      const host = deps.routes.find((r) => r.name === route.paneOf);
      if (host) route = host;
    }
    document.title = `${m.route.title} · Magnetar`;
    deps.onRoute(m.route);
    const key = route.view ?? route.name;
    entryKey = stateKey();
    if (current && current.key === key && current.layout === ctx.layout && current.handle.update) {
      current.handle.update(params, url.searchParams);
      if (pop) restoreScroll();
      return;
    }
    /** @type {Awaited<ReturnType<RouteDef["load"]>>} */
    let mod;
    try {
      mod = await route.load();
    } catch {
      // Offline and this page's code is not in the shell cache yet.
      mod = { mount: unavailableView };
    }
    if (gen !== generation) return;
    if (current) {
      try {
        current.handle.unmount();
      } catch (e) {
        console.error(e);
      }
      current = null;
    }
    deps.outlet.replaceChildren();
    if (!pop) globalThis.scrollTo(0, 0);
    const el = /** @type {HTMLElement} */ (document.createElement("div"));
    el.className = `view view-${key.replace(/[^a-z0-9-]/g, "-")}`;
    deps.outlet.appendChild(el);
    const handle = mod.mount(el, { params, query: url.searchParams, ctx });
    current = { key, route, handle, layout: ctx.layout };
    if (pop) restoreScroll();
  }

  function restoreScroll() {
    const y = scrolls.get(entryKey);
    if (y != null) requestAnimationFrame(() => globalThis.scrollTo(0, y));
  }

  /** @param {MouseEvent} e */
  function onClick(e) {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    const t = e.target instanceof Element ? e.target.closest("a[data-route]") : null;
    if (!(t instanceof HTMLAnchorElement) || t.target === "_blank") return;
    const url = new URL(t.href);
    if (url.origin !== location.origin) return;
    e.preventDefault();
    nav(url.pathname + url.search);
  }

  function start() {
    if (!history.state || typeof history.state.key !== "string") {
      history.replaceState({ key: "k0", idx: 0 }, "", location.href);
    }
    document.addEventListener("click", onClick);
    globalThis.addEventListener("popstate", () => {
      saveScroll();
      void render(true);
    });
    if ("scrollRestoration" in history) history.scrollRestoration = "manual";
    void render(false);
  }

  /** The layout class changed (phone / medium / wide): mount the route again. */
  function relayout() {
    if (current) {
      try {
        current.handle.unmount();
      } catch (e) {
        console.error(e);
      }
      current = null;
    }
    void render(true);
  }

  return { start, nav, back, relayout, refresh: () => render(true) };
}

/** @typedef {ReturnType<typeof createRouter>} Router */
