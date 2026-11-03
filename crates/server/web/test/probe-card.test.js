// The "Detected" card against a minimal fake document (C-CORR-10, C-PROBE-*): an older server
// (no `probe_corrections`) gets the D126 lines and no edit action; a newer one the track rows,
// "Add a track the probe missed", "All tracks match VLC" and the unplaced corrections. Track
// names stay text.

import assert from "node:assert/strict";

class Node0 {
  /** @param {number} t */
  constructor(t) {
    this.nodeType = t;
    /** @type {any[]} */
    this.childNodes = [];
    this.data = "";
  }

  /** @param {any} c */
  appendChild(c) {
    this.childNodes.push(c);
    return c;
  }

  /** @param {...any} c */
  append(...c) {
    for (const x of c) this.appendChild(typeof x === "string" ? txt(x) : x);
  }

  /** @param {...any} c */
  replaceChildren(...c) {
    this.childNodes = [];
    this.append(...c);
  }

  get textContent() {
    return this.nodeType === 3 ? this.data : this.childNodes.map((c) => c.textContent).join("");
  }

  set textContent(v) {
    if (this.nodeType === 3) this.data = v;
    else this.childNodes = [txt(String(v))];
  }
}

class El extends Node0 {
  /** @param {string} tag */
  constructor(tag) {
    super(1);
    this.tagName = tag.toUpperCase();
    /** @type {Map<string, string>} */
    this.attributes = new Map();
    /** @type {Record<string, string>} */
    this.dataset = {};
    /** @type {Record<string, Function[]>} */
    this.listeners = {};
    this.className = "";
    this.disabled = false;
    this.hidden = false;
    this.tabIndex = -1;
  }

  /** @param {string} k @param {string} v */
  setAttribute(k, v) {
    this.attributes.set(k, v);
  }

  /** @param {string} k */
  getAttribute(k) {
    return this.attributes.get(k) ?? null;
  }

  /** @param {string} t @param {Function} fn */
  addEventListener(t, fn) {
    (this.listeners[t] ??= []).push(fn);
  }
}

/** @param {string} s */
function txt(s) {
  const n = new Node0(3);
  n.data = s;
  return n;
}

/** @type {any} */ (globalThis).document ??= {
  /** @param {string} t */
  createElement: (t) => new El(t),
  /** @param {string} _ns @param {string} t */
  createElementNS: (_ns, t) => new El(t),
  createTextNode: txt,
};

const { probeCard, probeUiState } = await import("../js/views/probe-card.js");
const { probeViews } = await import("../js/probe-format.js");

const XSS = "<img src=x onerror=console.error('XSS-FIRED')>";
const probe = {
  version: 2,
  probed_at: 1793500000,
  status: "ok",
  verified: true,
  files: [{
    index: 0,
    dht_index: 0,
    path: "Movie.mkv",
    size: 6e9,
    description: "Matroska video",
    confidence: 97,
    verified: true,
    video: {
      container: "matroska",
      audio: [
        { codec: "eac3", channels: 6, lang: "fre", atmos: true, pos: 0, fix: { id: 17, fields: [] } },
        { codec: "ac3", channels: 2, lang: "eng", kinds: ["commentary"], name: "‮" + XSS, pos: 1 },
      ],
      subs: Array.from({ length: 12 }, (_, i) => ({ codec: "subrip", lang: "fre", pos: i })),
    },
  }],
  unplaced: [{ id: 9, file: 0, path: "Movie.mkv", track: "sub", pos: 3, reason: "changed", fields: ["kinds"], kinds: ["cc"] }],
};

/**
 * @param {any} n
 * @param {(e: El) => boolean} pred
 * @returns {El[]}
 */
function all(n, pred) {
  /** @type {El[]} */
  const out = [];
  /** @param {any} x */
  const walk = (x) => {
    if (x.nodeType === 1 && pred(x)) out.push(x);
    for (const c of x.childNodes ?? []) walk(c);
  };
  walk(n);
  return out;
}

/** @param {boolean} canCorrect */
function render(canCorrect) {
  return probeCard({
    probe,
    views: /** @type {any} */ (probeViews(probe, [])),
    canCorrect,
    correcting: false,
    phone: false,
    ui: probeUiState(),
    rerender: () => {},
    onSave: () => Promise.resolve(true),
    onUndo: () => Promise.resolve(true),
  });
}

Deno.test("an older server gets the D126 lines and no edit action", () => {
  const card = render(false);
  const text = card.textContent ?? "";
  assert.ok(text.includes("Audio: French · E-AC-3 5.1 · Dolby Atmos"));
  assert.ok(text.includes("Subtitle: French · SRT ×12"));
  assert.ok(text.includes("Read from the first blocks of each file, not from its name."));
  const buttons = all(card, (e) => e.tagName === "BUTTON");
  assert.deepEqual(buttons.map((b) => b.textContent), []);
  assert.ok(!text.includes("Corrections that no longer match the probe"));
  assert.ok(!text.includes("XSS-FIRED"), "the plain lines never show a track name");
});

Deno.test("a newer server gets editable rows, the add and check-all actions and unplaced corrections", () => {
  const card = render(true);
  const text = card.textContent ?? "";
  const labels = all(card, (e) => e.tagName === "BUTTON").map((b) => b.getAttribute("aria-label") ?? b.textContent);
  assert.ok(labels.includes("Edit audio track 1"));
  assert.ok(labels.includes("Edit audio track 2"));
  assert.ok(labels.includes("Edit subtitle track 10"));
  assert.ok(!labels.includes("Edit subtitle track 11"), "ten subtitle rows before Show all");
  assert.ok(labels.includes("Show all 12 subtitle tracks"));
  assert.ok(labels.includes("Add a track the probe missed"));
  assert.ok(labels.includes("All tracks match VLC"));
  assert.ok(labels.includes("Remove"));
  assert.ok(text.includes("✓ matches VLC"));
  assert.ok(text.includes("Movie.mkv: subtitle track 4: kinds CC"));
  // The name is cleaned of the override and shown as text inside an isolated span.
  const names = all(card, (e) => e.className === "u" && (e.textContent ?? "").includes("XSS-FIRED"));
  assert.equal(names.length, 1);
  assert.equal(names[0].getAttribute("dir"), "auto");
  assert.ok(!(names[0].textContent ?? "").includes("‮"));
  assert.equal(all(card, (e) => e.tagName === "IMG" || e.tagName === "SCRIPT").length, 0);
});
