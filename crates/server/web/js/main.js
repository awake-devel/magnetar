// Boot (docs/WEB.md "Module map"): shell, session, /health, router, service worker.

import { createApi } from "./api.js";
import { createHealth } from "./health.js";
import { createPageCache } from "./pagecache.js";
import * as prefs from "./prefs.js";
import { createRouter, endedLoginUrl } from "./router.js";
import { NOT_FOUND, ROUTES } from "./routes.js";
import { createSession, loadTrust } from "./session.js";
import { createAppStore, patch } from "./store.js";
import { checkPageTransport } from "./settings-checks.js";
import { banner } from "./ui/banner.js";
import * as dialog from "./ui/dialog.js";
import { createShell } from "./ui/shell.js";
import { createSnackbar } from "./ui/snackbar.js";
import { policyAllowed, scriptUrl } from "./tt.js";
import { lastSearchHref } from "./views/search.js";

/** The asset version this page was served with (/_app/<VERSION>/js/main.js). */
export const WEB_VERSION = (() => {
  try {
    const parts = new URL(import.meta.url).pathname.split("/");
    return parts[1] === "_app" && /^[0-9a-z]{1,32}$/.test(parts[2]) ? parts[2] : "dev";
  } catch {
    return "dev";
  }
})();

/** @returns {"phone" | "medium" | "wide"} */
function layoutNow() {
  if (matchMedia("(min-width: 1200px)").matches) return "wide";
  if (matchMedia("(min-width: 768px)").matches) return "medium";
  return "phone";
}

function boot() {
  const root = document.getElementById("app");
  if (!root) return;
  const store = createAppStore();
  const api = createApi();
  const cache = createPageCache();
  const health = createHealth(api);
  const shell = createShell(root, { searchHref: lastSearchHref });
  const snackbar = createSnackbar(shell.snackHost);
  let layout = layoutNow();
  shell.setLayout(layout);

  /** @type {import("./router.js").Router | null} */
  let router = null;

  const session = createSession({
    api,
    cache,
    store,
    onSignedOut(reason) {
      if (reason === "ended") {
        // The router may already have sent a signed-out page to /login: keep its `next`.
        router?.nav(endedLoginUrl(location.pathname, location.search), { replace: true });
      } else {
        router?.nav("/login", { replace: true });
      }
    },
  });

  /** @type {import("./types.js").Ctx} */
  const ctx = {
    api,
    store,
    health,
    prefs,
    nav: (to, opts) => router?.nav(to, opts),
    back: () => router?.back(),
    snackbar,
    dialog,
    get layout() {
      return layout;
    },
    session,
    cache,
  };

  router = createRouter({
    routes: ROUTES,
    notFound: NOT_FOUND,
    outlet: shell.outlet,
    ctx: () => ctx,
    signedIn: () => store.get().signedIn,
    onRoute(route) {
      shell.setActive(route.nav);
      shell.setNavVisible(route.name !== "login" && store.get().signedIn);
    },
  });

  const media = [matchMedia("(min-width: 1200px)"), matchMedia("(min-width: 768px)")];
  for (const m of media) {
    m.addEventListener("change", () => {
      const next = layoutNow();
      if (next === layout) return;
      layout = next;
      shell.setLayout(layout);
      router?.relayout();
    });
  }

  // Global banners: plain HTTP, offline, a new version.
  const transport = checkPageTransport(location);
  function renderBanners() {
    /** @type {Node[]} */
    const list = [];
    if (transport.warning) list.push(banner(transport.warning, { kind: "warn" }));
    const s = store.get();
    if (s.signedIn && health.get().ok === false) {
      list.push(banner("Offline: the server cannot be reached. Pages show what this browser cached.", { kind: "error" }));
    }
    if (s.swUpdate) {
      list.push(banner("Updated: reload to use the new version", {
        kind: "info",
        action: { label: "Reload", onClick: () => swReload() },
      }));
    }
    shell.setBanners(list);
  }
  store.subscribe(renderBanners);
  health.subscribe(renderBanners);
  renderBanners();

  void (async () => {
    const state = await session.boot();
    if (state === "ended") {
      // A full page load with a session the server no longer knows: the sign-in page says so.
      history.replaceState(history.state, "", endedLoginUrl(location.pathname, location.search));
    } else if (state !== "signed_out") {
      void health.refresh();
      health.startAutoRefresh();
      void loadTrust(ctx);
    }
    router?.start();
  })();

  registerServiceWorker(store);
}

/**
 * @param {import("./store.js").AppStore} store
 */
function registerServiceWorker(store) {
  if (!("serviceWorker" in navigator) || !globalThis.isSecureContext) return;
  const sw = navigator.serviceWorker;
  let reloading = false;
  /** @type {ServiceWorker | null} */
  let waiting = null;
  sw.addEventListener("controllerchange", () => {
    // Only after the user asked for the new version (the first install also takes control).
    if (!waiting || reloading) return;
    reloading = true;
    location.reload();
  });
  /** @param {ServiceWorker} w */
  function offer(w) {
    // A new version is installed and waiting while this page runs the old one.
    if (!sw.controller || waiting === w) return;
    waiting = w;
    const reload = () => w.postMessage({ type: "SKIP_WAITING" });
    swReload = reload;
    // The global banner says it (renderBanners); no snackbar as well.
    patch(store, { swUpdate: "1" });
  }
  void (async () => {
    // Trusted Types covers the worker URL too: register only when the CSP allows our policy.
    const allowed = await policyAllowed();
    if (allowed === "no") {
      console.info("Magnetar: no offline shell (the server's CSP allows no Trusted Types policy named magnetar)");
      return;
    }
    if (allowed === "unknown") return;
    try {
      const reg = await sw.register(scriptUrl("/sw.js"), { scope: "/", updateViaCache: "none" });
      if (reg.waiting) offer(reg.waiting);
      reg.addEventListener("updatefound", () => {
        const w = reg.installing;
        if (!w) return;
        w.addEventListener("statechange", () => {
          if (w.state === "installed") offer(w);
        });
      });
      // Look for a new version when the page comes back to the foreground.
      document.addEventListener("visibilitychange", () => {
        if (document.visibilityState === "visible") reg.update().catch(() => {});
      });
    } catch {
      // No offline shell: the app still works online.
    }
  })();
}

/** The "Updated: reload" action: activate the waiting worker (the page then reloads). */
let swReload = () => location.reload();

boot();
