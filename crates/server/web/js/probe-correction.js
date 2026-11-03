// Port of android/.../ui/detail/ProbeCorrection.kt: track corrections in the "Detected" card
// (D131, docs/APP.md "Track corrections"). The user checks each audio and subtitle track against
// VLC's Audio Track / Sub Track menus and corrects the language, the kinds or the name, says a
// track does not exist, or adds one the probe missed. Pure: the wording and the request items
// are unit-tested.

import {
  AUDIO_KIND_LABELS,
  audioKindLabels,
  channelsText,
  codecName,
  ISO639_2,
  languageName,
  SUB_KIND_LABELS,
  subKindLabels,
} from "./probe-format.js";

/** @typedef {import("./types-stats.js").ProbeFileInfo} ProbeFileInfo */
/** @typedef {import("./types-stats.js").ProbeTrackFix} ProbeTrackFix */
/** @typedef {import("./types-stats.js").ProbeUnplacedFix} ProbeUnplacedFix */
/** @typedef {import("./types-stats.js").ProbeUnplacedWas} ProbeUnplacedWas */
/** @typedef {import("./types-stats.js").ProbeCorrectionItem} ProbeCorrectionItem */

export const TRACK_AUDIO = "audio";
export const TRACK_SUB = "sub";
export const FIELD_LANG = "lang";
export const FIELD_KINDS = "kinds";
export const FIELD_NAME = "name";

/** Most bytes of a track name (the server's limit, D130). */
export const MAX_TRACK_NAME_BYTES = 64;

/** Subtitle rows a file shows before "Show all N subtitle tracks". */
export const MAX_SUBTITLE_ROWS = 10;

/**
 * @typedef {object} TrackValues
 * @property {string | null} lang
 * @property {readonly string[]} kinds
 * @property {string | null} name
 */

/**
 * One audio or subtitle track of a probed file, as the editable card shows it.
 * @typedef {object} TrackRow
 * @property {number} file `ProbeFileInfo.dht_index`
 * @property {string} track TRACK_AUDIO or TRACK_SUB
 * @property {number | null} pos 0-based detected position; null on an added track
 * @property {string} text "Track 2 · French · AC-3 2.0 · commentary", "Added · Spanish · forced"
 * @property {string | null} name the effective name, cleaned
 * @property {ProbeTrackFix | null} fix
 * @property {string | null} detectedText
 * @property {string | null} detectedName
 * @property {TrackValues} effective
 * @property {TrackValues} detected
 * @property {string | null} review
 * @property {string} editLabel
 * @property {boolean} added
 * @property {boolean} removed
 * @property {boolean} corrected
 * @property {boolean} checked
 * @property {string | null} marker "corrected", "✓ matches VLC", "not in the file", "added by you"
 */

/** @param {string} track */
function typeWord(track) {
  return track === TRACK_AUDIO ? "audio" : "subtitle";
}

/**
 * @param {string | null | undefined} lang
 * @param {string} codec
 * @param {number | null | undefined} channels
 * @param {boolean} atmos
 * @param {boolean} dtsX
 * @param {readonly string[]} kinds
 */
function audioBody(lang, codec, channels, atmos, dtsX, kinds) {
  /** @type {string[]} */
  const parts = [lang != null ? languageName(lang) : "unknown language"];
  if (codec.trim() !== "") parts.push(codecName(codec) + (channels != null ? ` ${channelsText(channels)}` : ""));
  if (atmos) parts.push("Dolby Atmos");
  if (dtsX) parts.push("DTS:X");
  return [...parts, ...audioKindLabels(kinds)].join(" · ");
}

/**
 * @param {string | null | undefined} lang
 * @param {string} codec
 * @param {readonly string[]} kinds
 */
function subBody(lang, codec, kinds) {
  const parts = [lang != null ? languageName(lang) : "unknown language", ...subKindLabels(kinds)];
  if (codec.trim() !== "") parts.push(codecName(codec));
  return parts.join(" · ");
}

/** @param {ProbeTrackFix | null | undefined} fix */
function reviewText(fix) {
  if (fix == null || !fix.review) return null;
  if (fix.added) return "⚠ The probe now lists a different number of tracks: check this added track";
  return "⚠ The probe changed since you checked this track";
}

/**
 * @param {string} track
 * @param {number | null} pos
 */
function editLabel(track, pos) {
  return pos == null ? `Edit added ${typeWord(track)} track` : `Edit ${typeWord(track)} track ${pos + 1}`;
}

/** @param {number | null} pos */
function rowPrefix(pos) {
  return pos == null ? "Added" : `Track ${pos + 1}`;
}

/**
 * @param {string | null | undefined} name
 * @returns {string | null}
 */
function cleanName(name) {
  if (name == null) return null;
  const c = cleanTrackName(name).trim();
  return c === "" ? null : c;
}

/**
 * @param {Omit<TrackRow, "added" | "removed" | "corrected" | "checked" | "marker">} r
 * @returns {TrackRow}
 */
function withFlags(r) {
  const fix = r.fix;
  const added = fix?.added === true;
  const removed = fix?.removed === true;
  const corrected = fix != null && !added && !removed && (fix.fields ?? []).length > 0;
  const checked = fix != null && !added && !removed && (fix.fields ?? []).length === 0;
  const marker = added
    ? "added by you"
    : removed
    ? "not in the file"
    : corrected
    ? "corrected"
    : checked
    ? "✓ matches VLC"
    : null;
  return { ...r, added, removed, corrected, checked, marker };
}

/**
 * The file's audio rows, then its subtitle rows, in stored order: "Track {pos+1}" is VLC's
 * numbering, added tracks last of their type.
 * @param {ProbeFileInfo} f
 * @returns {TrackRow[]}
 */
export function trackRows(f) {
  const v = f.video;
  if (!v) return [];
  const file = f.dht_index ?? 0;
  /** @type {TrackRow[]} */
  const out = [];
  (v.audio ?? []).forEach((a, i) => {
    const fix = a.fix ?? null;
    const added = fix?.added === true;
    const pos = added ? null : (a.pos ?? i);
    /** @type {TrackValues} */
    const eff = { lang: a.lang ?? null, kinds: a.kinds ?? [], name: a.name ?? null };
    const d = fix?.detected;
    /** @type {TrackValues} */
    const det = d ? { lang: d.lang ?? null, kinds: d.kinds ?? [], name: d.name ?? null } : eff;
    const codec = a.codec ?? "";
    out.push(withFlags({
      file,
      track: TRACK_AUDIO,
      pos,
      text: rowPrefix(pos) + " · " + audioBody(a.lang, codec, a.channels, a.atmos === true, a.dts_x === true, a.kinds ?? []),
      name: cleanName(a.name),
      fix,
      detectedText: added ? null : audioBody(det.lang, codec, a.channels, a.atmos === true, a.dts_x === true, det.kinds),
      detectedName: added ? null : cleanName(det.name),
      effective: eff,
      detected: det,
      review: reviewText(fix),
      editLabel: editLabel(TRACK_AUDIO, pos),
    }));
  });
  (v.subs ?? []).forEach((s, i) => {
    const fix = s.fix ?? null;
    const added = fix?.added === true;
    const pos = added ? null : (s.pos ?? i);
    /** @type {TrackValues} */
    const eff = { lang: s.lang ?? null, kinds: s.kinds ?? [], name: s.name ?? null };
    const d = fix?.detected;
    /** @type {TrackValues} */
    const det = d ? { lang: d.lang ?? null, kinds: d.kinds ?? [], name: d.name ?? null } : eff;
    const codec = s.codec ?? "";
    out.push(withFlags({
      file,
      track: TRACK_SUB,
      pos,
      text: rowPrefix(pos) + " · " + subBody(s.lang, codec, s.kinds ?? []),
      name: cleanName(s.name),
      fix,
      detectedText: added ? null : subBody(det.lang, codec, det.kinds),
      detectedName: added ? null : cleanName(det.name),
      effective: eff,
      detected: det,
      review: reviewText(fix),
      editLabel: editLabel(TRACK_SUB, pos),
    }));
  });
  return out;
}

/**
 * What the editor holds. `name` is the text field's value (cleaned, not yet trimmed).
 * @typedef {object} TrackDraft
 * @property {number} file
 * @property {string} track
 * @property {number | null} pos detected track: its position
 * @property {number | null} id added track: its correction id (null while adding a new one)
 * @property {string | null} lang
 * @property {readonly string[]} kinds sorted, distinct
 * @property {string} name
 * @property {boolean} removed
 * @property {TrackValues | null} detected
 * @property {TrackValues | null} start what the track was when the editor opened (null while adding)
 * @property {boolean} startRemoved
 * @property {number | null} fixId the correction on this track, for "Undo correction"
 * @property {boolean} review
 */

/**
 * @param {Iterable<string>} kinds
 * @returns {string[]}
 */
export function kindSet(kinds) {
  return [...new Set(kinds)].sort();
}

/**
 * The editor for `row` (its effective values), or for a track the probe missed in file `row`.
 * @param {TrackRow | number} row
 * @param {string} [track]
 * @returns {TrackDraft}
 */
export function draftOf(row, track = TRACK_SUB) {
  if (typeof row === "number") {
    return {
      file: row,
      track,
      pos: null,
      id: null,
      lang: null,
      kinds: [],
      name: "",
      removed: false,
      detected: null,
      start: null,
      startRemoved: false,
      fixId: null,
      review: false,
    };
  }
  return {
    file: row.file,
    track: row.track,
    pos: row.pos,
    id: row.added ? (row.fix?.id ?? null) : null,
    lang: row.effective.lang,
    kinds: kindSet(row.effective.kinds),
    name: cleanName(row.effective.name) ?? "",
    removed: row.removed,
    detected: row.added ? null : row.detected,
    start: row.effective,
    startRemoved: row.removed,
    fixId: row.fix?.id ?? null,
    review: row.fix?.review === true,
  };
}

/**
 * @param {string | null | undefined} a
 * @param {string | null | undefined} b
 */
function sameName(a, b) {
  return (cleanName(a) ?? "") === (cleanName(b) ?? "");
}

/**
 * @param {readonly string[]} a
 * @param {readonly string[]} b
 */
function sameSet(a, b) {
  const x = kindSet(a);
  const y = kindSet(b);
  return x.length === y.length && x.every((v, i) => v === y[i]);
}

/**
 * The draft differs from the track as it was when the editor opened.
 * @param {TrackDraft} d
 */
export function changed(d) {
  const s = d.start;
  if (s == null) return d.lang != null || d.kinds.length > 0 || cleanName(d.name) != null;
  return d.removed !== d.startRemoved ||
    (!d.removed && (d.lang !== s.lang || !sameSet(d.kinds, s.kinds) || !sameName(d.name, s.name)));
}

/**
 * The request item for `d` (D130), or null when there is nothing to send. Fields that are null
 * are left out of the JSON (the server reads them as none), as the app's `explicitNulls = false`.
 * @param {TrackDraft} d
 * @returns {ProbeCorrectionItem | null}
 */
export function itemOf(d) {
  const name = cleanName(d.name);
  const kinds = kindSet(d.kinds);
  if (d.pos != null) {
    if (d.removed) return { file: d.file, track: d.track, pos: d.pos, set: [], kinds: [], removed: true };
    const det = d.detected ?? { lang: null, kinds: [], name: null };
    /** @type {string[]} */
    const set = [];
    if (d.lang !== det.lang) set.push(FIELD_LANG);
    if (!sameSet(d.kinds, det.kinds)) set.push(FIELD_KINDS);
    if (!sameName(name, det.name)) set.push(FIELD_NAME);
    /** @type {ProbeCorrectionItem} */
    const item = { file: d.file, track: d.track, pos: d.pos, set, kinds: set.includes(FIELD_KINDS) ? kinds : [], removed: false };
    if (set.includes(FIELD_LANG) && d.lang != null) item.lang = d.lang;
    if (set.includes(FIELD_NAME) && name != null) item.name = name;
    return orderItem(item);
  }
  /** @type {string[]} */
  const filled = [];
  if (d.lang != null) filled.push(FIELD_LANG);
  if (kinds.length > 0) filled.push(FIELD_KINDS);
  if (name != null) filled.push(FIELD_NAME);
  if (filled.length === 0) return null;
  const set = d.id != null ? [FIELD_LANG, FIELD_KINDS, FIELD_NAME] : filled;
  /** @type {ProbeCorrectionItem} */
  const item = { file: d.file, track: d.track, set, kinds, removed: false };
  if (d.id != null) item.id = d.id;
  if (d.lang != null) item.lang = d.lang;
  if (name != null) item.name = name;
  return orderItem(item);
}

/**
 * The item with its keys in the server's order (id, file, track, pos, set, lang, kinds, name,
 * removed), so the JSON reads as the app's.
 * @param {ProbeCorrectionItem} i
 * @returns {ProbeCorrectionItem}
 */
function orderItem(i) {
  /** @type {Record<string, unknown>} */
  const o = {};
  for (const k of ["id", "file", "track", "pos", "set", "lang", "kinds", "name", "removed"]) {
    const v = /** @type {Record<string, unknown>} */ (i)[k];
    if (v !== undefined) o[k] = v;
  }
  return /** @type {ProbeCorrectionItem} */ (o);
}

/**
 * The editor's primary button and whether it is enabled: "Save"; "Matches VLC" on an unchanged
 * track without a correction; "Still correct" on an unchanged correction under review.
 * @param {TrackDraft} d
 * @returns {[string, boolean]}
 */
export function primaryAction(d) {
  if (d.start == null) return ["Save", itemOf(d) != null];
  if (changed(d)) return ["Save", itemOf(d) != null];
  if (d.fixId == null && d.pos != null) return ["Matches VLC", true];
  if (d.review) return ["Still correct", itemOf(d) != null];
  return ["Save", false];
}

/**
 * "Audio track 2", "Added subtitle track", "Add a track the probe missed".
 * @param {TrackDraft} d
 */
export function editorTitle(d) {
  if (d.pos != null) return (d.track === TRACK_AUDIO ? "Audio" : "Subtitle") + ` track ${d.pos + 1}`;
  if (d.id != null) return `Added ${typeWord(d.track)} track`;
  return "Add a track the probe missed";
}

/**
 * "All tracks match VLC": a check for every detected track of `f` without a correction.
 * @param {ProbeFileInfo} f
 * @returns {ProbeCorrectionItem[]}
 */
export function checkAllItems(f) {
  return trackRows(f).filter((r) => r.fix == null && r.pos != null)
    .map((r) => orderItem({ file: r.file, track: r.track, pos: /** @type {number} */ (r.pos), set: [], kinds: [], removed: false }));
}

/**
 * One entry of the language picker; a null code is "Unknown / none".
 * @typedef {object} LanguageChoice
 * @property {string | null} code
 * @property {string} name
 * @property {string} label
 */

/** ISO 639-2/T codes (and `mri`, whose /B is `mao`): the picker offers the /B one only. */
const TERMINOLOGY_CODES = new Set([
  "fra", "deu", "zho", "ces", "slk", "ron", "ell", "nld", "fas", "isl", "eus", "sqi", "mkd", "hye", "kat",
  "cym", "msa", "mya", "bod", "mri",
]);

/**
 * @param {string | null} code
 * @param {string} name
 * @returns {LanguageChoice}
 */
function choice(code, name) {
  return Object.freeze({ code, name, label: code != null ? `${name} (${code})` : "Unknown / none" });
}

const NO_LANGUAGE = choice(null, "Unknown / none");

/** @type {LanguageChoice[] | null} */
let languages = null;

function allLanguages() {
  languages ??= Object.entries(ISO639_2).filter(([c]) => !TERMINOLOGY_CODES.has(c))
    .map(([c, n]) => choice(c, n))
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  return languages;
}

/**
 * "Unknown / none", then the languages by English name, one bibliographic code each. A query
 * keeps those whose name contains it (any case) or whose code is it (a /T code finds its /B
 * language).
 * @param {string} [query]
 * @returns {LanguageChoice[]}
 */
export function languageChoices(query = "") {
  const q = query.trim().toLowerCase();
  if (q === "") return [NO_LANGUAGE, ...allLanguages()];
  const byCode = Object.hasOwn(ISO639_2, q) ? ISO639_2[q] : undefined;
  return [NO_LANGUAGE, ...allLanguages().filter((l) => l.name.toLowerCase().includes(q) || l.code === q || l.name === byCode)];
}

/**
 * The language field: "French (fre)", "Unknown / none".
 * @param {string | null} code
 */
export function languageLabel(code) {
  return code != null ? `${languageName(code)} (${code})` : "Unknown / none";
}

/** @param {number} cp */
function isBidiControl(cp) {
  return cp === 0x200e || cp === 0x200f || (cp >= 0x202a && cp <= 0x202e) || (cp >= 0x2066 && cp <= 0x2069);
}

/** @param {number} cp */
function isIsoControl(cp) {
  return cp <= 0x1f || (cp >= 0x7f && cp <= 0x9f);
}

/**
 * A track name as the server takes it: without bidi controls or control characters, at most
 * 64 UTF-8 bytes cut on a code-point boundary. Not trimmed (the field keeps a space being typed).
 * @param {string} s
 */
export function cleanTrackName(s) {
  let out = "";
  let bytes = 0;
  for (const ch of s) {
    const cp = /** @type {number} */ (ch.codePointAt(0));
    if (isBidiControl(cp) || isIsoControl(cp)) continue;
    const n = cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
    if (bytes + n > MAX_TRACK_NAME_BYTES) break;
    bytes += n;
    out += ch;
  }
  return out;
}

/**
 * @param {string} track
 * @param {ProbeUnplacedWas} w
 */
function wasText(track, w) {
  return track === TRACK_AUDIO
    ? audioBody(w.lang, w.codec ?? "", w.channels, false, false, w.kinds ?? [])
    : subBody(w.lang, w.codec ?? "", w.kinds ?? []);
}

/**
 * "Movie.mkv: subtitle track 4 (was English · SDH · PGS): kinds CC", or "Movie.mkv: the file
 * is no longer probed".
 * @param {ProbeUnplacedFix} u
 * @param {string} fileName
 */
export function unplacedText(u, fileName) {
  if (u.reason === "file") return `${fileName}: the file is no longer probed`;
  const track = u.track ?? "";
  const which = u.pos != null ? `${typeWord(track)} track ${u.pos + 1}` : `added ${typeWord(track)} track`;
  const was = u.was ? ` (was ${wasText(track, u.was)})` : "";
  const labels = track === TRACK_AUDIO ? audioKindLabels(u.kinds ?? []) : subKindLabels(u.kinds ?? []);
  const fields = u.fields ?? [];
  /** @type {string[]} */
  let change = [];
  if (u.removed) change.push("not in the file");
  if (fields.includes(FIELD_LANG)) change.push("language " + (u.lang != null ? languageName(u.lang) : "none"));
  if (fields.includes(FIELD_KINDS)) change.push("kinds " + (labels.length > 0 ? labels : ["none"]).join(" + "));
  if (fields.includes(FIELD_NAME)) {
    const n = cleanName(u.name);
    change.push(n != null ? `name “${n}”` : "no name");
  }
  if (change.length === 0) change = ["checked"];
  return `${fileName}: ${which}${was}: ${change.join(", ")}`;
}

/**
 * The kind chips of the editor: the type's kinds in D126 order, then any other kind the draft
 * holds, with their labels.
 * @param {string} track
 * @param {readonly string[]} kinds
 * @returns {{kind: string, label: string}[]}
 */
export function kindChoices(track, kinds) {
  const order = track === TRACK_AUDIO ? AUDIO_KIND_LABELS : SUB_KIND_LABELS;
  const known = Object.keys(order);
  const extra = kinds.filter((k) => !known.includes(k)).sort();
  return [...known, ...extra].map((k) => ({ kind: k, label: Object.hasOwn(order, k) ? order[k] : k.replaceAll("_", " ") }));
}
