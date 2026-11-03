// Service worker (docs/WEB.md "Caching and offline"): caches the app shell only, so the UI
// loads without the server. It never stores an API answer: recent answers live in the page's
// IndexedDB cache, cleared at sign-out. The server fills in __V__ and the asset list.
/// <reference lib="webworker" />

const VERSION = "__V__";
/** @type {string[]} */
const ASSETS = /*__ASSETS__*/[];
const CACHE = `magnetar-shell-${VERSION}`;
const SHELL = "/";
const NAV_TIMEOUT_MS = 4000;

/** SPA routes a navigation may fall back to the cached shell for. */
const SPA = [
  /^\/$/,
  /^\/search$/,
  /^\/saved$/,
  /^\/stats(\/graphs|\/status)?$/,
  /^\/settings$/,
  /^\/login$/,
  /^\/crawl-map$/,
  /^\/graph$/,
  /^\/torrent\/[0-9a-fA-F]{40}$/,
  /^\/title\/\d{1,18}$/,
];

const sw = /** @type {ServiceWorkerGlobalScope} */ (/** @type {unknown} */ (self));

sw.addEventListener("install", (event) => {
  event.waitUntil(
    caches.open(CACHE).then((c) => c.addAll(ASSETS.includes(SHELL) ? ASSETS : [...ASSETS, SHELL])),
  );
});

// A new version waits until the page asks for it ("Updated: reload"), so an open tab never
// runs a mix of old page code and new shell files.
sw.addEventListener("message", (event) => {
  const data = /** @type {unknown} */ (event.data);
  if (data && typeof data === "object" && /** @type {{type?: unknown}} */ (data).type === "SKIP_WAITING") {
    void sw.skipWaiting();
  }
});

sw.addEventListener("activate", (event) => {
  event.waitUntil((async () => {
    for (const k of await caches.keys()) {
      if (k.startsWith("magnetar-shell-") && k !== CACHE) await caches.delete(k);
    }
    await sw.clients.claim();
  })());
});

/** @param {Request} req */
async function navigate(req) {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), NAV_TIMEOUT_MS);
  try {
    const res = await fetch(req, { signal: ctl.signal });
    clearTimeout(timer);
    return res;
  } catch {
    clearTimeout(timer);
    const cached = await caches.match(SHELL, { cacheName: CACHE });
    return cached ?? Response.error();
  }
}

/** @param {Request} req */
async function cacheFirst(req) {
  const hit = await caches.match(req, { cacheName: CACHE });
  if (hit) return hit;
  return fetch(req);
}

sw.addEventListener("fetch", (event) => {
  const req = event.request;
  if (req.method !== "GET") return;
  const url = new URL(req.url);
  if (url.origin !== sw.location.origin) return;
  if (url.pathname.startsWith(`/_app/${VERSION}/`)) {
    event.respondWith(cacheFirst(req));
    return;
  }
  if (req.mode === "navigate" && SPA.some((re) => re.test(url.pathname))) {
    event.respondWith(navigate(req));
  }
  // Everything else (/api/*, /health, /sw.js, the manifest) goes to the network untouched.
});
