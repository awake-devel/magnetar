// The forbidden-sink lint (docs/WEB.md "Security model"): no markup parser, no eval, no native
// dialogs, no inline handlers or styles, anywhere in the web JavaScript. Nothing is allow-listed:
// a comment that needs one of these words is phrased differently.

import assert from "node:assert/strict";

const FORBIDDEN = [
  /innerHTML/,
  /outerHTML/,
  /insertAdjacentHTML/,
  /document\.write/,
  /DOMParser/,
  /createContextualFragment/,
  /\beval\s*\(/,
  /new\s+Function\b/,
  /setTimeout\s*\(\s*["'`]/,
  /setInterval\s*\(\s*["'`]/,
  /setAttribute\s*\(\s*["'`]on/i,
  /setAttribute\s*\(\s*["'`]style/i,
  /\balert\s*\(/,
  /\bconfirm\s*\(/,
  /\bprompt\s*\(/,
  /javascript:/i,
  /\.srcdoc/,
  /\.cssText/,
  /import\s*\(\s*[^"'`\s]/,
];

const ROOT = new URL("../", import.meta.url);

/**
 * @param {URL} dir
 * @returns {AsyncGenerator<URL>}
 */
async function* jsFiles(dir) {
  for await (const e of Deno.readDir(dir)) {
    const u = new URL(e.name + (e.isDirectory ? "/" : ""), dir);
    if (e.isDirectory) yield* jsFiles(u);
    else if (e.name.endsWith(".js")) yield u;
  }
}

Deno.test("no forbidden sink in the web JavaScript", async () => {
  const files = [new URL("sw.js", ROOT)];
  for await (const f of jsFiles(new URL("js/", ROOT))) files.push(f);
  assert.ok(files.length > 20, "the walk found the modules");
  /** @type {string[]} */
  const hits = [];
  for (const f of files) {
    const text = await Deno.readTextFile(f);
    text.split("\n").forEach((line, i) => {
      for (const re of FORBIDDEN) {
        if (re.test(line)) hits.push(`${f.pathname.split("/web/")[1]}:${i + 1}: ${re} in ${line.trim()}`);
      }
    });
  }
  assert.deepEqual(hits, []);
});

Deno.test("index.html has no inline script, style or handler", async () => {
  const html = await Deno.readTextFile(new URL("index.html", ROOT));
  assert.doesNotMatch(html, /<script(?![^>]*\bsrc=)[^>]*>/i);
  assert.doesNotMatch(html, /<style/i);
  assert.doesNotMatch(html, /\son[a-z]+\s*=/i);
  assert.doesNotMatch(html, /\sstyle\s*=/i);
  assert.match(html, /<script type="module" src="\/_app\/__V__\/js\/main\.js"><\/script>/);
});

Deno.test("the lint itself catches each sink", () => {
  const samples = [
    "el.innerHTML = x",
    "el.outerHTML",
    "el.insertAdjacentHTML('beforeend', x)",
    "document.write(x)",
    "new DOMParser()",
    "range.createContextualFragment(x)",
    "eval(x)",
    "new Function('a')",
    "setTimeout(\"x()\", 1)",
    "el.setAttribute('onclick', x)",
    'el.setAttribute("style", x)',
    "alert(1)",
    "window.confirm(x)",
    "prompt(x)",
    "a.href = 'javascript:x'",
    "f.srcdoc = x",
    "el.style.cssText = x",
    "import(name)",
  ];
  for (const s of samples) assert.ok(FORBIDDEN.some((re) => re.test(s)), s);
  assert.ok(!FORBIDDEN.some((re) => re.test('import("./views/search.js")')));
});
