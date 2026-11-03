// The graph page's layout worker (docs/WEB.md "Stats"): runs graph-layout.js off the page's
// thread and posts positions while it settles. Same origin, a module worker started through the
// page's Trusted Types policy (js/tt.js). Messages carry numbers only.
/// <reference lib="webworker" />

import { createLayout } from "./graph-layout.js";

const scope = /** @type {DedicatedWorkerGlobalScope} */ (/** @type {unknown} */ (self));

scope.addEventListener("message", (e) => {
  const m = e.data;
  if (!m || typeof m !== "object" || typeof m.n !== "number" || !Array.isArray(m.edges)) return;
  const n = Math.max(0, Math.min(5000, Math.trunc(m.n)));
  /** @type {[number, number][]} */
  const edges = m.edges.filter((/** @type {unknown} */ p) => Array.isArray(p) && p.length === 2)
    .map((/** @type {unknown[]} */ p) => [Number(p[0]) | 0, Number(p[1]) | 0]);
  const pinned = typeof m.pinned === "number" ? m.pinned : null;
  const id = m.id;
  const l = createLayout(n, edges, pinned);
  l.run(400, 25, (x, y) => scope.postMessage({ id, x, y, settled: false }));
  scope.postMessage({ id, x: l.x, y: l.y, settled: true });
});
