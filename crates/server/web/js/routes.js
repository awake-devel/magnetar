// The route table (docs/WEB.md "Routing and URL parameters"). Views load on first use.
// U2 owns the imports and entries of detail, saved, stats, graph and crawl-map.

/** @type {readonly import("./router.js").RouteDef[]} */
export const ROUTES = Object.freeze([
  { name: "search", path: "/", title: "Search", nav: "search", load: () => import("./views/search.js") },
  { name: "search-redirect", path: "/search", title: "Search", redirect: "/", load: () => import("./views/search.js") },
  { name: "login", path: "/login", title: "Sign in", public: true, load: () => import("./views/login.js") },
  { name: "settings", path: "/settings", title: "Settings", nav: "settings", load: () => import("./views/settings.js") },
  { name: "title", path: "/title/:id", title: "Title", nav: "search", load: () => import("./views/title.js") },
  // U2: detail, saved, stats, graph, crawl map.
  {
    name: "torrent",
    path: "/torrent/:ih",
    title: "Torrent",
    nav: "search",
    paneOf: "search",
    load: () => import("./views/detail.js"),
  },
  { name: "saved", path: "/saved", title: "Saved", nav: "saved", load: () => import("./views/saved.js") },
  { name: "stats", path: "/stats", title: "Stats", nav: "stats", view: "stats", load: () => import("./views/stats.js") },
  { name: "stats-graphs", path: "/stats/graphs", title: "Stats", nav: "stats", view: "stats", load: () => import("./views/stats.js") },
  { name: "stats-status", path: "/stats/status", title: "Stats", nav: "stats", view: "stats", load: () => import("./views/stats.js") },
  { name: "graph", path: "/graph", title: "Graph", nav: "stats", load: () => import("./views/graph.js") },
  { name: "crawl-map", path: "/crawl-map", title: "Crawl map", nav: "stats", load: () => import("./views/crawl-map.js") },
]);

/** @type {import("./router.js").RouteDef} */
export const NOT_FOUND = Object.freeze({
  name: "not-found",
  path: "",
  title: "Not found",
  public: true,
  load: () => import("./views/not-found.js"),
});
