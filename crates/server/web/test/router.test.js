import assert from "node:assert/strict";
import { safeNext } from "../js/dom.js";
import { endedLoginUrl, loginUrl, matchPath, matchRoute, nextTarget } from "../js/router.js";
import { ROUTES } from "../js/routes.js";

const IH = "0123456789abcdef0123456789abcdef01234567";

/** @param {string} p */
const nameOf = (p) => matchRoute(ROUTES, p)?.route.name ?? null;

Deno.test("every SPA route matches, with validated parameters", () => {
  assert.equal(nameOf("/"), "search");
  assert.equal(nameOf("/search"), "search-redirect");
  assert.equal(nameOf("/saved"), "saved");
  assert.equal(nameOf("/saved/"), "saved");
  assert.equal(nameOf("/stats"), "stats");
  assert.equal(nameOf("/stats/graphs"), "stats-graphs");
  assert.equal(nameOf("/stats/status"), "stats-status");
  assert.equal(nameOf("/settings"), "settings");
  assert.equal(nameOf("/login"), "login");
  assert.equal(nameOf("/graph"), "graph");
  assert.equal(nameOf("/crawl-map"), "crawl-map");
  assert.deepEqual(matchRoute(ROUTES, `/torrent/${IH}`)?.params, { ih: IH });
  assert.deepEqual(matchRoute(ROUTES, `/torrent/${IH.toUpperCase()}`)?.params, { ih: IH });
  assert.deepEqual(matchRoute(ROUTES, "/title/7")?.params, { id: "7" });
  assert.deepEqual(matchRoute(ROUTES, "/title/007")?.params, { id: "7" });
});

Deno.test("bad parameters and unknown paths do not match", () => {
  assert.equal(nameOf("/torrent/xyz"), null);
  assert.equal(nameOf(`/torrent/${IH}0`), null);
  assert.equal(nameOf("/title/-1"), null);
  assert.equal(nameOf("/title/99999999999999999999"), null);
  assert.equal(nameOf("/title/1e3"), null);
  assert.equal(nameOf("/nope"), null);
  assert.equal(nameOf("/api/v1/nope"), null);
  assert.equal(matchPath("/title/:id", "/title/%ZZ"), null);
});

Deno.test("routes share a view where they should", () => {
  const stats = ROUTES.filter((r) => r.path.startsWith("/stats"));
  assert.ok(stats.every((r) => r.view === "stats"));
  assert.equal(ROUTES.find((r) => r.name === "torrent")?.paneOf, "search");
  assert.ok(ROUTES.every((r) => !r.public || r.name === "login"));
});

Deno.test("next after sign-in is a same-origin path only", () => {
  assert.equal(nextTarget("/saved"), "/saved");
  assert.equal(nextTarget("/?q=dune"), "/?q=dune");
  assert.equal(nextTarget("//evil.example/"), "/");
  assert.equal(nextTarget("/\\evil.example"), "/");
  assert.equal(nextTarget("https://evil.example/"), "/");
  assert.equal(nextTarget("javascript:x"), "/");
  assert.equal(nextTarget("/login?next=/x"), "/");
  assert.equal(nextTarget(null), "/");
  assert.equal(safeNext("/a\nb"), false);
  assert.equal(loginUrl("/"), "/login?next=%2F");
  assert.equal(loginUrl("/saved"), "/login?next=%2Fsaved");
  assert.equal(loginUrl("//evil"), "/login?next=%2F");
  // A 401 mid-session: the sign-in page says the session ended (C-AUTH-10).
  assert.equal(loginUrl("/saved", true), "/login?next=%2Fsaved&ended=1");
  assert.equal(loginUrl("/login?next=%2Fx", true), "/login?next=%2F&ended=1");
  assert.equal(new URL(loginUrl("/stats?x=1", true), "http://h").searchParams.get("next"), "/stats?x=1");
});

Deno.test("a session that ended keeps where the page was, or the next of a sign-in page", () => {
  assert.equal(endedLoginUrl("/saved", ""), "/login?next=%2Fsaved&ended=1");
  assert.equal(endedLoginUrl("/stats/graphs", "?x=1"), "/login?next=%2Fstats%2Fgraphs%3Fx%3D1&ended=1");
  // A full page load: the router already showed /login?next=… before the session answer.
  assert.equal(endedLoginUrl("/login", "?next=%2Fsaved"), "/login?next=%2Fsaved&ended=1");
  assert.equal(endedLoginUrl("/login", ""), "/login?next=%2F&ended=1");
  assert.equal(endedLoginUrl("/login", "?next=%2F%2Fevil"), "/login?next=%2F&ended=1");
});
