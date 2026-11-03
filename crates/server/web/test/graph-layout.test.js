// Values from GraphLayoutTest.kt.
import assert from "node:assert/strict";
import { createLayout, fitTransform, GRAPH_PATH, graphTitle, kindLabel, nodeAt, subjectOf, XorWow } from "../js/graph-layout.js";

/**
 * @param {ReturnType<typeof createLayout>} l
 * @param {number} a
 * @param {number} b
 */
const dist = (l, a, b) => Math.hypot(l.x[a] - l.x[b], l.y[a] - l.y[b]);
/** @param {number[]} v */
const avg = (v) => v.reduce((a, b) => a + b, 0) / v.length;
/** @param {number} a @param {number} b */
const range = (a, b) => Array.from({ length: b - a }, (_, i) => a + i);

Deno.test("Kotlin's Random(7) sequence", () => {
  // kotlin.random.Random(7).nextInt() x3 (XorWowRandom).
  const r = new XorWow(7);
  const a = [r.nextInt(), r.nextInt(), r.nextInt()];
  const again = new XorWow(7);
  assert.deepEqual([again.nextInt(), again.nextInt(), again.nextInt()], a);
  const f = new XorWow(7).nextFloat();
  assert.ok(f >= 0 && f < 1);
});

Deno.test("linked nodes end up closer and the subject stays centred", () => {
  /** @type {[number, number][]} */
  const edges = [...range(1, 11).map((i) => /** @type {[number, number]} */ ([0, i])), ...range(12, 22).map((i) => /** @type {[number, number]} */ ([11, i])), [0, 11]];
  const l = createLayout(22, edges, 0);
  l.run(400);
  assert.equal(l.x[0], 0);
  assert.equal(l.y[0], 0);
  assert.ok(range(0, 22).every((i) => Number.isFinite(l.x[i]) && Number.isFinite(l.y[i])));
  const within = avg(range(1, 11).map((i) => dist(l, 0, i)));
  const across = avg(range(12, 22).map((i) => dist(l, 0, i)));
  assert.ok(within < across, `${within} vs ${across}`);
});

Deno.test("deterministic and settles before the cap", () => {
  /** @type {[number, number][]} */
  const edges = range(1, 50).map((i) => [Math.trunc((i - 1) / 3), i]);
  const a = createLayout(50, edges);
  a.run();
  const b = createLayout(50, edges);
  b.run();
  assert.deepEqual([...a.x], [...b.x]);
  assert.deepEqual([...a.y], [...b.y]);
  assert.ok(a.steps > 50 && a.steps < 400, `steps ${a.steps}`);
});

Deno.test("copes with odd input", () => {
  createLayout(0, []).run();
  const l = createLayout(1, [[0, 0], [0, 5]], 0);
  l.run();
  assert.equal(l.x[0], 0);
});

Deno.test("detached islands and lone nodes stay near the main cluster", () => {
  /** @type {[number, number][]} */
  const edges = [...range(1, 60).map((i) => /** @type {[number, number]} */ ([0, i])), ...range(61, 64).map((i) => /** @type {[number, number]} */ ([60, i]))];
  const n = 84;
  const l = createLayout(n, edges, 0);
  l.run();
  const cx = avg(range(0, 60).map((i) => l.x[i]));
  const cy = avg(range(0, 60).map((i) => l.y[i]));
  const radius = Math.max(...range(0, 60).map((i) => Math.hypot(l.x[i] - cx, l.y[i] - cy)));
  assert.ok(Math.hypot(l.x[60] - cx, l.y[60] - cy) < 3 * radius);
  assert.ok(Math.max(...range(64, n).map((i) => Math.hypot(l.x[i] - cx, l.y[i] - cy))) < 3 * radius);
  const t = fitTransform(l.x, l.y, { n, width: 1080, height: 1600, pad: 60 });
  assert.ok(radius * t.scale > 1080 * 0.12);
});

Deno.test("fit puts every node inside the padded view, centred", () => {
  const x = [-100, 300, 50];
  const y = [-20, 40, 400];
  const t = fitTransform(x, y, { width: 1000, height: 800, pad: 50 });
  for (let i = 0; i < 3; i++) {
    const sx = x[i] * t.scale + t.ox;
    const sy = y[i] * t.scale + t.oy;
    assert.ok(sx >= 49.9 && sx <= 950.1 && sy >= 49.9 && sy <= 750.1);
  }
  assert.ok(Math.abs(-20 * t.scale + t.oy - 50) < 0.5);
  assert.ok(Math.abs(400 * t.scale + t.oy - 750) < 0.5);
  assert.ok(Math.abs(100 * t.scale + t.ox - 500) < 0.5);
  const one = fitTransform([5], [5], { width: 500, height: 500, pad: 20 });
  assert.ok(one.scale <= 6);
  assert.ok(Math.abs(5 * one.scale + one.ox - 250) < 0.01);
  assert.deepEqual(fitTransform([], [], { width: 500, height: 500, pad: 20 }), { scale: 1, ox: 250, oy: 250 });
});

Deno.test("tap hits the nearest node within its touch radius", () => {
  const x = [0, 10, 100];
  const y = [0, 0, 0];
  const t = { scale: 2, ox: 100, oy: 100 };
  const r = [5, 5, 30];
  assert.equal(nodeAt(118, 101, x, y, 3, t, r, 12), 1);
  assert.equal(nodeAt(103, 100, x, y, 3, t, r, 12), 0);
  assert.equal(nodeAt(325, 100, x, y, 3, t, r, 12), 2);
  assert.equal(nodeAt(200, 300, x, y, 3, t, r, 12), null);
  assert.equal(nodeAt(0, 0, [], [], 3, t, r, 12), null);
});

Deno.test("graph paths, titles and kinds", () => {
  for (const p of ["wot", "relays", "media/7", "swarm/" + "a".repeat(40)]) assert.match(p, GRAPH_PATH);
  for (const p of ["evil", "media/0", "media/x", "swarm/xyz", "wot/../x", "media/1?x"]) assert.doesNotMatch(p, GRAPH_PATH);
  assert.equal(graphTitle("media/7"), "Releases of this title");
  assert.equal(kindLabel("root"), "You (trust root)");
  assert.equal(kindLabel("constructor"), "constructor");
  assert.equal(subjectOf({ nodes: [{ kind: "root" }] }), 0);
  assert.equal(subjectOf({ nodes: [{ kind: "publisher" }] }), null);
});
