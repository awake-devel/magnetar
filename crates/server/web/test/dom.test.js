// dom.js against a minimal fake document: text stays text, URLs only from the helpers.

import assert from "node:assert/strict";

class FakeNode {
  /** @param {number} nodeType */
  constructor(nodeType) {
    this.nodeType = nodeType;
    /** @type {any[]} */
    this.childNodes = [];
    /** @type {string} */
    this.data = "";
  }

  /** @param {FakeNode} c */
  appendChild(c) {
    this.childNodes.push(c);
    return c;
  }

  get textContent() {
    return this.nodeType === 3 ? this.data : this.childNodes.map((c) => c.textContent).join("");
  }

  set textContent(v) {
    if (this.nodeType === 3) this.data = v;
    else this.childNodes = [text(String(v))];
  }
}

class FakeElement extends FakeNode {
  /**
   * @param {string} tag
   * @param {string | null} ns
   */
  constructor(tag, ns = null) {
    super(1);
    this.tagName = tag.toUpperCase();
    this.namespaceURI = ns;
    /** @type {Map<string, string>} */
    this.attributes = new Map();
    /** @type {Record<string, string>} */
    this.dataset = {};
    /** @type {Record<string, unknown[]>} */
    this.listeners = {};
    this.className = "";
    this.value = "";
    this.disabled = false;
    this.src = "";
    this.referrerPolicy = "";
    this.decoding = "";
    this.loading = "";
    this.alt = "";
  }

  /**
   * @param {string} k
   * @param {string} v
   */
  setAttribute(k, v) {
    this.attributes.set(k, v);
  }

  /** @param {string} k */
  getAttribute(k) {
    return this.attributes.get(k) ?? null;
  }

  /**
   * @param {string} t
   * @param {unknown} fn
   */
  addEventListener(t, fn) {
    (this.listeners[t] ??= []).push(fn);
  }

  /** @param {...FakeNode} nodes */
  replaceChildren(...nodes) {
    this.childNodes = nodes;
  }
}

/** @param {string} s */
function text(s) {
  const n = new FakeNode(3);
  n.data = s;
  return n;
}

/** @type {any} */ (globalThis).document = {
  /** @param {string} t */
  createElement: (t) => new FakeElement(t),
  /**
   * @param {string} ns
   * @param {string} t
   */
  createElementNS: (ns, t) => new FakeElement(t, ns),
  createTextNode: text,
};

const { externalUrl, h, link, magnetLink, posterImg, SafeUrl, svg, untrusted } = await import("../js/dom.js");

const IH = "0123456789abcdef0123456789abcdef01234567";
const HOSTILE = `<img src=x onerror=console.error('XSS-FIRED')> "q" & <b>b</b> ‮gpj.exe`;

/** @param {unknown} n */
const el = (n) => /** @type {FakeElement} */ (/** @type {unknown} */ (n));

Deno.test("strings become text nodes, never markup", () => {
  const p = el(h("p", { class: "x", text: HOSTILE }));
  assert.equal(p.childNodes.length, 1);
  assert.equal(p.childNodes[0].nodeType, 3);
  assert.equal(p.textContent, HOSTILE);
  const d = el(h("div", null, "a", HOSTILE, 3, null, false, ["b"]));
  assert.equal(d.childNodes.length, 4);
  assert.ok(d.childNodes.every((c) => c.nodeType === 3));
  assert.equal(d.textContent, `a${HOSTILE}3b`);
});

Deno.test("untrusted text is bidi-isolated text", () => {
  const s = el(untrusted(HOSTILE));
  assert.equal(s.tagName, "SPAN");
  assert.equal(s.className, "u");
  assert.equal(s.getAttribute("dir"), "auto");
  assert.equal(s.textContent, HOSTILE);
  assert.equal(el(untrusted(null)).textContent, "");
});

Deno.test("dangerous properties and tags throw", () => {
  for (const k of ["innerHTML", "outerHTML", "style", "srcdoc", "onclick", "onerror", "src", "formaction"]) {
    assert.throws(() => h("div", { [k]: "x" }), TypeError, k);
  }
  assert.throws(() => h("a", { href: "https://evil.example/" }), TypeError);
  assert.throws(() => h("a", { href: "javascript:alert(1)" }), TypeError);
  for (const t of ["script", "iframe", "object", "embed", "img", "style", "base", "svg", "a b"]) {
    assert.throws(() => h(t), TypeError, t);
  }
  assert.throws(() => h("div", { "on:click": "code" }), TypeError);
  assert.throws(() => new SafeUrl(Symbol("x"), "/", false), TypeError);
});

Deno.test("events go through addEventListener", () => {
  const fn = () => {};
  const b = el(h("button", { "on:click": fn }));
  assert.deepEqual(b.listeners.click, [fn]);
  assert.equal(b.getAttribute("onclick"), null);
});

Deno.test("internal links from validated parts only", () => {
  assert.equal(String(link("torrent", { ih: IH.toUpperCase() })), `/torrent/${IH}`);
  assert.equal(String(link("title", { id: 7 })), "/title/7");
  assert.equal(String(link("graph", { path: "media/7" })), "/graph?path=media%2F7");
  assert.equal(String(link("graph", { path: `swarm/${IH}` })), `/graph?path=swarm%2F${IH}`);
  assert.equal(String(link("search", { query: "q=dune&res=2160p" })), "/?q=dune&res=2160p");
  assert.equal(String(link("search")), "/");
  assert.equal(String(link("login", { next: "//evil" })), "/login");
  assert.equal(String(link("crawl-map", { layer: "dht", window: "<x>" })), "/crawl-map?layer=dht");
  assert.throws(() => link("torrent", { ih: "xyz" }), TypeError);
  assert.throws(() => link("title", { id: "1; drop" }), TypeError);
  assert.throws(() => link("graph", { path: "evil" }), TypeError);
  assert.throws(() => link("graph", { path: "../wot" }), TypeError);
  assert.throws(() => link("nowhere"), TypeError);
  const a = el(h("a", { href: link("saved") }));
  assert.equal(a.getAttribute("href"), "/saved");
  assert.equal(a.getAttribute("target"), null);
});

Deno.test("magnet links only from the API's own magnet for this infohash", () => {
  const good = `magnet:?xt=urn:btih:${IH}&dn=x`;
  assert.equal(String(magnetLink({ magnet: good, summary: { infohash: IH } })), good);
  assert.equal(String(magnetLink({ magnet: good.toUpperCase().replace("MAGNET:", "magnet:"), summary: { infohash: IH } })), good.toUpperCase().replace("MAGNET:", "magnet:"));
  assert.equal(magnetLink({ magnet: "javascript:alert(1)//magnet:?", summary: { infohash: IH } }), null);
  assert.equal(magnetLink({ magnet: `magnet:?xt=urn:btih:${"f".repeat(40)}`, summary: { infohash: IH } }), null);
  assert.equal(magnetLink({ magnet: `magnet:?xt=urn:btih:${IH}" onclick="x`, summary: { infohash: IH } }), null);
  assert.equal(magnetLink({ summary: { infohash: IH } }), null);
  assert.equal(magnetLink(null), null);
});

Deno.test("external links: fixed bases, new tab, no referrer", () => {
  assert.equal(String(externalUrl("imdb", "tt1160419")), "https://www.imdb.com/title/tt1160419/");
  assert.equal(externalUrl("imdb", "tt1/../../evil"), null);
  assert.equal(externalUrl("imdb", "nm123"), null);
  const dbip = externalUrl("dbip");
  assert.ok(dbip);
  const a = el(h("a", { href: dbip }));
  assert.equal(a.getAttribute("href"), "https://db-ip.com");
  assert.equal(a.getAttribute("rel"), "noopener noreferrer");
  assert.equal(a.getAttribute("target"), "_blank");
});

Deno.test("posters: same-origin API path only, else built from the id", () => {
  const f1 = el(posterImg({ media_id: 7, poster: "/api/v1/media/7/poster", title: HOSTILE }));
  const img = f1.childNodes[0];
  assert.equal(img.tagName, "IMG");
  assert.equal(img.src, "/api/v1/media/7/poster");
  assert.equal(img.referrerPolicy, "no-referrer");
  assert.equal(img.alt, `Poster of ${HOSTILE}`);
  const f2 = el(posterImg({ media_id: 7, poster: "https://evil.example/x.png" }));
  assert.equal(f2.childNodes[0].src, "/api/v1/media/7/poster");
  const f3 = el(posterImg({ media_id: 7, poster: null }));
  assert.equal(f3.childNodes[0].className, "poster-empty");
  const f4 = el(posterImg({ poster: "//evil.example/x" }));
  assert.equal(f4.childNodes[0].className, "poster-empty");
});

Deno.test("svg: geometry attributes only, numeric path data", () => {
  const p = el(svg("path", { d: "M0 0L10 10Z", fill: "red" }));
  assert.equal(p.namespaceURI, "http://www.w3.org/2000/svg");
  assert.throws(() => svg("path", { d: "javascript:x" }), TypeError);
  assert.throws(() => svg("path", { onload: "x" }), TypeError);
  assert.throws(() => svg("a", { href: "x" }), TypeError);
  assert.throws(() => svg("foreignObject"), TypeError);
  assert.throws(() => svg("polyline", { points: "1,2 <x>" }), TypeError);
  const t = el(svg("text", { x: 1, text: HOSTILE }));
  assert.equal(t.textContent, HOSTILE);
});
