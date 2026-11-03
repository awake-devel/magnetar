// Values from android/app/src/test/.../ProbeCorrectionTest.kt (D128–D131).
import assert from "node:assert/strict";
import {
  changed,
  checkAllItems,
  cleanTrackName,
  draftOf,
  editorTitle,
  itemOf,
  kindChoices,
  languageChoices,
  languageLabel,
  primaryAction,
  TRACK_AUDIO,
  TRACK_SUB,
  trackRows,
  unplacedText,
} from "../js/probe-correction.js";
import { probeViews } from "../js/probe-format.js";

/** The `probe` of docs/API.md / the D130 spec §4, verbatim. */
const detail = JSON.parse(`{"summary":{"infohash":"${"c".repeat(40)}","title":"Movie"},
  "probe": {
    "version": 2, "probed_at": 1793500000, "status": "ok", "verified": true,
    "files": [{
      "index": 0, "dht_index": 0, "path": "Movie.mkv", "size": 6000000000, "label": "video.mkv",
      "description": "Matroska video", "confidence": 97, "verified": true,
      "video": {"container": "matroska",
        "audio": [
          {"codec":"eac3","channels":6,"lang":"fre","atmos":true,"pos":0,
           "fix":{"id":17,"fields":[],"at":1793500100,"by":"android"}},
          {"codec":"ac3","channels":2,"lang":"fre","kinds":["commentary"],"name":"Commentaire","pos":1,
           "fix":{"id":18,"fields":["lang","name"],"detected":{"lang":"eng","kinds":["commentary"],"name":"Commentary"},
                  "at":1793500200,"by":"android"}},
          {"codec":"aac","channels":2,"lang":"eng","pos":2,
           "fix":{"id":19,"fields":[],"removed":true,"detected":{"lang":"eng"},"at":1793500300,"by":"android"}}],
        "subs": [
          {"codec":"subrip","lang":"fre","kinds":["forced"],"pos":0},
          {"codec":"","lang":"spa","kinds":["forced"],"name":"Español (forzados)",
           "fix":{"id":42,"fields":["lang","kinds","name"],"added":true,"review":true,"at":1793500400,"by":"android"}}]}
    }],
    "unplaced": [
      {"id":9,"file":0,"path":"Movie.mkv","track":"sub","pos":3,"reason":"changed",
       "was":{"codec":"hdmv_pgs_subtitle","lang":"eng","kinds":["sdh"],"name":"English SDH"},
       "fields":["kinds"],"kinds":["cc"],"at":1793400000}]
  }
}`);
const file = detail.probe.files[0];

/** @param {unknown} v */
const json = (v) => JSON.stringify({ device: "web", items: [v] });

Deno.test("track rows number detected tracks as VLC does and mark corrections", () => {
  const rows = trackRows(file);
  assert.deepEqual(rows.map((r) => r.text), [
    "Track 1 · French · E-AC-3 5.1 · Dolby Atmos",
    "Track 2 · French · AC-3 2.0 · commentary",
    "Track 3 · English · AAC 2.0",
    "Track 1 · French · forced · SRT",
    "Added · Spanish · forced",
  ]);
  assert.deepEqual(rows.map((r) => r.marker), ["✓ matches VLC", "corrected", "not in the file", null, "added by you"]);
  assert.deepEqual(rows.map((r) => r.editLabel), [
    "Edit audio track 1",
    "Edit audio track 2",
    "Edit audio track 3",
    "Edit subtitle track 1",
    "Edit added subtitle track",
  ]);
  assert.deepEqual([...new Set(rows.map((r) => r.file))], [0]);
  assert.deepEqual(rows.map((r) => r.name), [null, "Commentaire", null, null, "Español (forzados)"]);
  const corrected = rows[1];
  assert.ok(corrected.corrected);
  assert.equal(corrected.detectedText, "English · AC-3 2.0 · commentary");
  assert.equal(corrected.detectedName, "Commentary");
  assert.ok(!rows[0].corrected);
  assert.ok(rows[0].checked);
  assert.equal(rows[0].detectedText, "French · E-AC-3 5.1 · Dolby Atmos");
  const captions = { dht_index: 4, video: { subs: [{ codec: "subrip", lang: "eng" }, { codec: "eia_608", kinds: ["cc"] }] } };
  assert.deepEqual(trackRows(captions).map((r) => [r.text, r.pos]), [
    ["Track 1 · English · SRT", 0],
    ["Track 2 · unknown language · CC · CEA-608", 1],
  ]);
  assert.deepEqual([...new Set(trackRows(captions).map((r) => r.file))], [4]);
  assert.deepEqual(trackRows({}), []);
  assert.deepEqual(checkAllItems(file), [{ file: 0, track: TRACK_SUB, pos: 0, set: [], kinds: [], removed: false }]);
  assert.deepEqual(checkAllItems(captions).map((i) => i.pos), [0, 1]);
  const view = /** @type {NonNullable<ReturnType<typeof probeViews>>} */ (probeViews(detail.probe, []))[0];
  assert.deepEqual(view.videoLines, ["Matroska"]);
  assert.equal(view.file, file);
});

Deno.test("removed, added and review rows", () => {
  const rows = trackRows(file);
  const removed = rows[2];
  assert.ok(removed.removed);
  assert.ok(!removed.corrected);
  assert.equal(removed.review, null);
  const added = rows[4];
  assert.ok(added.added);
  assert.equal(added.pos, null);
  assert.equal(added.detectedText, null);
  assert.equal(added.review, "⚠ The probe now lists a different number of tracks: check this added track");
  assert.equal(editorTitle(draftOf(added)), "Added subtitle track");
  assert.equal(editorTitle(draftOf(rows[1])), "Audio track 2");
  assert.equal(editorTitle(draftOf(rows[3])), "Subtitle track 1");
  assert.equal(editorTitle(draftOf(0)), "Add a track the probe missed");
  const r = trackRows({
    video: { subs: [{ codec: "subrip", lang: "eng", pos: 0, fix: { id: 3, review: true, detected: { lang: "eng" } } }] },
  })[0];
  assert.equal(r.review, "⚠ The probe changed since you checked this track");
  assert.equal(r.marker, "✓ matches VLC");
});

Deno.test("language choices: one bibliographic code per language, search by name or code", () => {
  const all = languageChoices("");
  assert.equal(all[0].code, null);
  assert.equal(all[0].label, "Unknown / none");
  const langs = all.slice(1);
  const terminology = new Set([
    "fra", "deu", "zho", "ces", "slk", "ron", "ell", "nld", "fas", "isl", "eus", "sqi", "mkd", "hye", "kat",
    "cym", "msa", "mya", "bod", "mri",
  ]);
  assert.ok(langs.every((l) => !terminology.has(/** @type {string} */ (l.code))));
  assert.equal(new Set(langs.map((l) => l.name)).size, langs.length);
  assert.deepEqual(langs.map((l) => l.name), [...langs.map((l) => l.name)].sort());
  assert.ok(langs.some((l) => l.code === "fre" && l.name === "French"));
  assert.ok(langs.some((l) => l.code === "mao" && l.name === "Maori"));
  assert.equal(langs.find((l) => l.code === "fre")?.label, "French (fre)");
  assert.deepEqual(languageChoices("fREn").map((l) => l.code), [null, "fre"]);
  assert.ok(languageChoices("ger").some((l) => l.code === "ger"));
  assert.ok(languageChoices("fra").some((l) => l.code === "fre"));
  assert.ok(languageChoices("mri").some((l) => l.code === "mao"));
  assert.deepEqual(languageChoices("zzzz").map((l) => l.code), [null]);
  assert.equal(languageLabel("fre"), "French (fre)");
  assert.equal(languageLabel(null), "Unknown / none");
});

Deno.test("a draft becomes the exact request item", () => {
  const rows = trackRows(file);
  const commentary = draftOf(rows[1]);
  assert.deepEqual(itemOf(commentary), {
    file: 0, track: TRACK_AUDIO, pos: 1, set: ["lang", "name"], lang: "fre", kinds: [], name: "Commentaire", removed: false,
  });
  assert.deepEqual(itemOf({ ...commentary, kinds: ["dub", "commentary"] }), {
    file: 0, track: TRACK_AUDIO, pos: 1, set: ["lang", "kinds", "name"], lang: "fre", kinds: ["commentary", "dub"], name: "Commentaire",
    removed: false,
  });
  assert.deepEqual(itemOf({ ...commentary, lang: "eng" }), {
    file: 0, track: TRACK_AUDIO, pos: 1, set: ["name"], kinds: [], name: "Commentaire", removed: false,
  });
  const noLang = itemOf({ ...commentary, lang: null, name: "Commentary" });
  assert.equal(json(noLang), `{"device":"web","items":[{"file":0,"track":"audio","pos":1,"set":["lang"],"kinds":[],"removed":false}]}`);
  assert.deepEqual(itemOf({ ...draftOf(rows[3]), removed: true, lang: "eng" }), {
    file: 0, track: TRACK_SUB, pos: 0, set: [], kinds: [], removed: true,
  });
  assert.deepEqual(itemOf({ ...draftOf(rows[4]), lang: "cat" }), {
    id: 42, file: 0, track: TRACK_SUB, set: ["lang", "kinds", "name"], lang: "cat", kinds: ["forced"], name: "Español (forzados)",
    removed: false,
  });
  const cleared = itemOf({ ...draftOf(rows[4]), name: "" });
  assert.equal(
    json(cleared),
    `{"device":"web","items":[{"id":42,"file":0,"track":"sub","set":["lang","kinds","name"],"lang":"spa","kinds":["forced"],"removed":false}]}`,
  );
  assert.equal(itemOf({ ...draftOf(rows[4]), lang: null, kinds: [], name: "" }), null);
  const fresh = { ...draftOf(3, TRACK_SUB), lang: "spa", kinds: ["forced"], name: "  Español‏ (forzados) " };
  assert.deepEqual(itemOf(fresh), {
    file: 3, track: TRACK_SUB, set: ["lang", "kinds", "name"], lang: "spa", kinds: ["forced"], name: "Español (forzados)", removed: false,
  });
  assert.deepEqual(itemOf({ ...draftOf(3, TRACK_AUDIO), kinds: ["commentary"] }), {
    file: 3, track: TRACK_AUDIO, set: ["kinds"], kinds: ["commentary"], removed: false,
  });
  assert.equal(itemOf({ ...draftOf(3, TRACK_SUB), name: "   " }), null);
  assert.deepEqual(primaryAction(draftOf(3, TRACK_SUB)), ["Save", false]);
  assert.deepEqual(primaryAction(fresh), ["Save", true]);
});

Deno.test("an unchanged draft saves as matches VLC", () => {
  const rows = trackRows(file);
  const plain = draftOf(rows[3]);
  assert.deepEqual(primaryAction(plain), ["Matches VLC", true]);
  assert.deepEqual(itemOf(plain), { file: 0, track: TRACK_SUB, pos: 0, set: [], kinds: [], removed: false });
  assert.deepEqual(itemOf({ ...plain, lang: "fre", kinds: ["forced"], name: "" })?.set, []);
  assert.deepEqual(primaryAction(draftOf(rows[0])), ["Save", false]);
  assert.deepEqual(primaryAction(draftOf(rows[1])), ["Save", false]);
  assert.deepEqual(primaryAction(draftOf(rows[2])), ["Save", false]);
  assert.deepEqual(primaryAction({ ...plain, lang: "eng" }), ["Save", true]);
  assert.deepEqual(primaryAction({ ...draftOf(rows[2]), removed: false }), ["Save", true]);
  assert.deepEqual(itemOf({ ...draftOf(rows[2]), removed: false }), { file: 0, track: TRACK_AUDIO, pos: 2, set: [], kinds: [], removed: false });
  assert.deepEqual(primaryAction({ ...draftOf(rows[1]), name: "Commentaire‎ " }), ["Save", false]);
  assert.ok(!changed({ ...draftOf(rows[1]), name: "Commentaire‎ " }));
  assert.deepEqual(primaryAction(draftOf(rows[4])), ["Still correct", true]);
  assert.equal(itemOf(draftOf(rows[4]))?.id, 42);
  const reviewed = trackRows({
    video: {
      subs: [{
        codec: "subrip",
        lang: "ger",
        pos: 0,
        fix: { id: 3, fields: ["lang"], review: true, detected: { lang: "ita" } },
      }],
    },
  })[0];
  assert.deepEqual(primaryAction(draftOf(reviewed)), ["Still correct", true]);
  assert.deepEqual(itemOf(draftOf(reviewed)), { file: 0, track: TRACK_SUB, pos: 0, set: ["lang"], lang: "ger", kinds: [], removed: false });
});

Deno.test("track names are cut to 64 bytes and lose bidi and control characters", () => {
  assert.equal(cleanTrackName("‏English‪ SDH‬⁩‎⁦"), "English SDH");
  assert.equal(cleanTrackName("a\u0007b\nc"), "abc");
  assert.equal(cleanTrackName("a\u0085b"), "ab");
  assert.equal(cleanTrackName("a".repeat(70)), "a".repeat(64));
  assert.equal(cleanTrackName("é".repeat(40)), "é".repeat(32));
  assert.equal(cleanTrackName("a" + "€".repeat(30)), "a" + "€".repeat(21));
  const film = "🎬";
  const cut = cleanTrackName("aaa" + film.repeat(20));
  assert.equal(cut, "aaa" + film.repeat(15));
  assert.ok(new TextEncoder().encode(cut).length <= 64);
  assert.equal(cleanTrackName("English "), "English ");
  // W4-style payload: overrides and isolates gone, the text itself kept as text.
  assert.equal(cleanTrackName("‮gpj.exe⁦ javascript:x"), "gpj.exe javascript:x");
});

Deno.test("unplaced corrections are described", () => {
  const u = detail.probe.unplaced[0];
  assert.equal(unplacedText(u, "Movie.mkv"), "Movie.mkv: subtitle track 4 (was English · SDH · PGS): kinds CC");
  assert.equal(
    unplacedText({
      id: 1, track: "audio", pos: 0, reason: "changed", was: { codec: "ac3", channels: 6 },
      fields: ["lang", "kinds", "name"], lang: "fre", kinds: [], name: "VFF",
    }, "a.mkv"),
    "a.mkv: audio track 1 (was unknown language · AC-3 5.1): language French, kinds none, name “VFF”",
  );
  assert.equal(unplacedText({ track: "sub", pos: 1, reason: "changed", fields: ["lang"] }, "b.mkv"), "b.mkv: subtitle track 2: language none");
  assert.equal(unplacedText({ track: "sub", pos: 2, reason: "changed", removed: true }, "b.mkv"), "b.mkv: subtitle track 3: not in the file");
  assert.equal(unplacedText({ track: "audio", pos: 0, reason: "changed" }, "b.mkv"), "b.mkv: audio track 1: checked");
  assert.equal(unplacedText({ track: "audio", pos: 0, reason: "file" }, "c.mkv"), "c.mkv: the file is no longer probed");
});

Deno.test("kind chips follow the D126 order, unknown kinds last", () => {
  assert.deepEqual(kindChoices(TRACK_SUB, []).map((k) => k.label), [
    "forced", "SDH", "CC", "signs & songs", "commentary", "descriptions", "honorifics", "bilingual", "easy reader",
  ]);
  assert.deepEqual(kindChoices(TRACK_AUDIO, ["zz_new"]).map((k) => k.kind).slice(-2), ["alternate", "zz_new"]);
  assert.equal(kindChoices(TRACK_AUDIO, ["zz_new"]).at(-1)?.label, "zz new");
});
