// Values from CrawlMapTest.kt ("World map asset"). The web asset must be a byte copy of the app's.
import assert from "node:assert/strict";
import { countryAt, countryByCode, parseWorldMap, project } from "../js/worldmap.js";

const WEB = new URL("../assets/world_110m.bin", import.meta.url);
const ANDROID = new URL("../../../../android/app/src/main/assets/world_110m.bin", import.meta.url);

/** @param {Uint8Array<ArrayBuffer>} b */
async function sha256(b) {
  const d = await crypto.subtle.digest("SHA-256", b);
  return [...new Uint8Array(d)].map((x) => x.toString(16).padStart(2, "0")).join("");
}

Deno.test("the web asset is the app's asset, byte for byte", async () => {
  const web = await Deno.readFile(WEB);
  const app = await Deno.readFile(ANDROID);
  assert.equal(await sha256(web), await sha256(app));
});

Deno.test("the world asset parses with the known countries", async () => {
  const bytes = await Deno.readFile(WEB);
  assert.ok(bytes.length < 100 * 1024);
  const m = parseWorldMap(bytes);
  assert.equal(m.scale, 10);
  assert.equal(m.west, -180);
  assert.equal(m.north, 84);
  assert.equal(m.width, 3600);
  assert.equal(m.height, 1420);
  assert.ok(m.countries.length >= 150 && m.countries.length <= 200);
  for (const c of ["FR", "US", "NO", "DE", "BR", "CN", "RU", "AU", "JP", "XK", "TW"]) assert.ok(countryByCode(m, c), c);
  assert.equal(countryByCode(m, "AQ"), null);
  assert.equal(new Set(m.countries.map((c) => c.code)).size, m.countries.length);
  for (const c of m.countries) {
    assert.match(c.code, /^[A-Z]{2}$/);
    assert.ok(c.rings.length > 0 && c.rings.length < 40);
    for (const r of c.rings) {
      assert.ok(r.length >= 6 && r.length % 2 === 0);
      for (let i = 0; i < r.length; i++) assert.ok(r[i] <= (i % 2 === 0 ? m.width : m.height));
    }
  }
  assert.ok(/** @type {NonNullable<ReturnType<typeof countryByCode>>} */ (countryByCode(m, "US")).rings.length >= 3);
  assert.ok(/** @type {NonNullable<ReturnType<typeof countryByCode>>} */ (countryByCode(m, "FR")).rings.length >= 2);
  const points = m.countries.reduce((a, c) => a + c.rings.reduce((b, r) => b + r.length / 2, 0), 0);
  assert.ok(points >= 3000 && points <= 20000);
});

Deno.test("a point finds its country", async () => {
  const m = parseWorldMap(await Deno.readFile(WEB));
  /** @param {number} lon @param {number} lat */
  const at = (lon, lat) => countryAt(m, ...project(m, lon, lat))?.code ?? null;
  assert.equal(at(2.35, 48.85), "FR");
  assert.equal(at(-100, 40), "US");
  assert.equal(at(10.75, 59.9), "NO");
  assert.equal(at(13.4, 52.5), "DE");
  assert.equal(at(-47.9, -15.8), "BR");
  assert.equal(at(139.7, 35.7), "JP");
  assert.equal(at(-30, 30), null);
});

Deno.test("a broken asset is refused", async () => {
  const bytes = await Deno.readFile(WEB);
  assert.throws(() => parseWorldMap(bytes.slice(0, bytes.length - 3)), RangeError);
  const longer = new Uint8Array(bytes.length + 1);
  longer.set(bytes);
  assert.throws(() => parseWorldMap(longer), RangeError);
  assert.throws(() => parseWorldMap(new TextEncoder().encode("nope")), RangeError);
  assert.throws(() => parseWorldMap(new Uint8Array(0)), RangeError);
});
