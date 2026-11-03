// Port of android/.../ui/detail/ProbeFormat.kt: the detail's "Detected" section (D121), what a
// worker's header probe read from the files themselves (D117), as plain text lines. Pure.

import { fileName, formatAgo, formatBytes, nowSec } from "./format.js";

/** @typedef {import("./types-stats.js").ProbeFileInfo} ProbeFileInfo */
/** @typedef {import("./types-stats.js").ProbeVideo} ProbeVideo */
/** @typedef {import("./types-stats.js").ProbeVideoTrack} ProbeVideoTrack */
/** @typedef {import("./types-stats.js").ProbeAudioTrack} ProbeAudioTrack */
/** @typedef {import("./types-stats.js").ProbeSubTrack} ProbeSubTrack */
/** @typedef {import("./types-stats.js").ProbeGame} ProbeGame */
/** @typedef {import("./types-stats.js").ProbeInfo} ProbeInfo */
/** @typedef {import("./types-stats.js").TorrentFile} TorrentFile */

/** ISO 639-2 (bibliographic, plus the terminology codes that differ) → English name. */
export const ISO639_2 = Object.freeze(/** @type {Record<string, string>} */ ({
  eng: "English", fre: "French", fra: "French", ger: "German", deu: "German",
  spa: "Spanish", ita: "Italian", por: "Portuguese", rus: "Russian", jpn: "Japanese",
  chi: "Chinese", zho: "Chinese", kor: "Korean", ara: "Arabic", dut: "Dutch", nld: "Dutch",
  swe: "Swedish", nor: "Norwegian", nob: "Norwegian Bokmål", nno: "Norwegian Nynorsk",
  dan: "Danish", fin: "Finnish", pol: "Polish", cze: "Czech", ces: "Czech",
  slo: "Slovak", slk: "Slovak", hun: "Hungarian", rum: "Romanian", ron: "Romanian",
  bul: "Bulgarian", gre: "Greek", ell: "Greek", tur: "Turkish", heb: "Hebrew",
  hin: "Hindi", tha: "Thai", vie: "Vietnamese", ind: "Indonesian", may: "Malay",
  msa: "Malay", ukr: "Ukrainian", hrv: "Croatian", srp: "Serbian", slv: "Slovenian",
  est: "Estonian", lav: "Latvian", lit: "Lithuanian", per: "Persian", fas: "Persian",
  ice: "Icelandic", isl: "Icelandic", cat: "Catalan", baq: "Basque", eus: "Basque",
  glg: "Galician", tam: "Tamil", tel: "Telugu", ben: "Bengali", urd: "Urdu",
  alb: "Albanian", sqi: "Albanian", mac: "Macedonian", mkd: "Macedonian",
  arm: "Armenian", hye: "Armenian", geo: "Georgian", kat: "Georgian", wel: "Welsh",
  cym: "Welsh", gle: "Irish", lat: "Latin", tgl: "Tagalog", fil: "Filipino",
  mal: "Malayalam", kan: "Kannada", mar: "Marathi", pan: "Punjabi", guj: "Gujarati",
  bur: "Burmese", mya: "Burmese", tib: "Tibetan", bod: "Tibetan", khm: "Khmer",
  lao: "Lao", mon: "Mongolian", nep: "Nepali", sin: "Sinhala", swa: "Swahili",
  afr: "Afrikaans", amh: "Amharic", aze: "Azerbaijani", bel: "Belarusian", bos: "Bosnian",
  kaz: "Kazakh", uzb: "Uzbek", yue: "Cantonese", cmn: "Mandarin", epo: "Esperanto",
  haw: "Hawaiian", mao: "Maori", mri: "Maori", som: "Somali", yid: "Yiddish",
  zul: "Zulu", xho: "Xhosa", kur: "Kurdish", pus: "Pashto", tat: "Tatar",
}));

/**
 * @param {Record<string, string>} map
 * @param {string} key
 * @returns {string | undefined}
 */
function lookup(map, key) {
  return Object.hasOwn(map, key) ? map[key] : undefined;
}

/**
 * "fre" → "French"; an unknown code stays as it came ("tlh").
 * @param {string} code
 */
export function languageName(code) {
  return lookup(ISO639_2, code.toLowerCase()) ?? code;
}

const CODECS = Object.freeze(/** @type {Record<string, string>} */ ({
  hevc: "HEVC", h264: "H.264", av1: "AV1", vp9: "VP9", vp8: "VP8", vvc: "VVC",
  mpeg2video: "MPEG-2", mpeg1video: "MPEG-1", mpeg4: "MPEG-4 Part 2", vc1: "VC-1",
  wmv3: "WMV 9", prores: "ProRes", theora: "Theora",
  aac: "AAC", ac3: "AC-3", eac3: "E-AC-3", truehd: "TrueHD", mlp: "MLP", dts: "DTS",
  dts_hd: "DTS-HD", dts_hd_ma: "DTS-HD MA", flac: "FLAC", opus: "Opus", vorbis: "Vorbis",
  mp3: "MP3", mp2: "MP2", pcm: "PCM", alac: "ALAC", wmav2: "WMA",
  subrip: "SRT", ass: "ASS", ssa: "SSA", webvtt: "WebVTT", mov_text: "MP4 text",
  hdmv_pgs_subtitle: "PGS", dvd_subtitle: "VobSub", dvb_subtitle: "DVB", eia_608: "CEA-608",
  eia_708: "CEA-708", ttml: "TTML", arib_caption: "ARIB", dvb_teletext: "Teletext",
  hdmv_text_subtitle: "TextST", xsub: "XSUB", kate: "Kate", usf: "USF",
}));

/**
 * FFmpeg codec name → what people call it ("eac3" → "E-AC-3").
 * @param {string} codec
 */
export function codecName(codec) {
  return lookup(CODECS, codec) ?? codec.toUpperCase();
}

const CONTAINERS = Object.freeze(/** @type {Record<string, string>} */ ({
  matroska: "Matroska", mkv: "Matroska", webm: "WebM", mp4: "MP4", mov: "QuickTime",
  avi: "AVI", mpegts: "MPEG-TS", ts: "MPEG-TS", m2ts: "Blu-ray M2TS", flv: "FLV",
  ogg: "Ogg", asf: "ASF/WMV", mpegps: "MPEG-PS",
}));

/** @param {string} c */
export function containerName(c) {
  return lookup(CONTAINERS, c) ?? c.toUpperCase();
}

const PLATFORMS = Object.freeze(/** @type {Record<string, string>} */ ({
  ps5: "PS5", ps4: "PS4", ps3: "PS3", psp: "PSP", ps_vita: "PS Vita",
  psp_or_vita: "PSP or PS Vita", ps1_or_ps2: "PS1 or PS2", ps1: "PS1", ps2: "PS2",
  switch: "Nintendo Switch", xbox: "Xbox", xbox360: "Xbox 360", xbox_one: "Xbox One",
  wii: "Wii", wii_u: "Wii U", game_cube: "GameCube", wii_or_game_cube: "Wii or GameCube",
  n3ds: "Nintendo 3DS", nds: "Nintendo DS", gba: "Game Boy Advance", gb: "Game Boy",
  gbc: "Game Boy Color", nes: "NES", fds: "Famicom Disk System", snes: "SNES",
  n64: "Nintendo 64", master_system: "Master System", game_gear: "Game Gear",
  genesis: "Mega Drive / Genesis", sega32x: "32X", sega_cd: "Sega CD", saturn: "Saturn",
  dreamcast: "Dreamcast", lynx: "Atari Lynx", atari7800: "Atari 7800",
  neo_geo_pocket: "Neo Geo Pocket",
}));

/**
 * sniff platform ("ps_vita") → "PS Vita"; an unknown one reads with spaces.
 * @param {string} p
 */
export function platformName(p) {
  return lookup(PLATFORMS, p) ?? p.replaceAll("_", " ");
}

/** @param {string} k */
function gameKindName(k) {
  switch (k) {
    case "package":
      return "package";
    case "image":
      return "disc image";
    case "compressed_image":
      return "compressed image";
    case "metadata":
      return "metadata file";
    default:
      return k.replaceAll("_", " ");
  }
}

/**
 * 2 → "2.0", 6 → "5.1", 8 → "7.1", 1 → "mono".
 * @param {number} n
 */
export function channelsText(n) {
  switch (n) {
    case 1:
      return "mono";
    case 2:
      return "2.0";
    case 3:
      return "2.1";
    case 6:
      return "5.1";
    case 7:
      return "6.1";
    case 8:
      return "7.1";
    default:
      return `${n} ch`;
  }
}

/**
 * 23976 → "23.976 fps", 25000 → "25 fps".
 * @param {number} milli
 */
export function fpsText(milli) {
  const s = milli % 1000 === 0
    ? String(milli / 1000)
    : (milli / 1000).toFixed(3).replace(/0+$/, "").replace(/\.$/, "");
  return `${s} fps`;
}

/**
 * 7_200_000 → "2h 00m", 2_700_000 → "45m", 42_000 → "42s".
 * @param {number} ms
 */
export function durationText(ms) {
  const s = Math.floor(ms / 1000);
  if (s >= 3600) return `${Math.floor(s / 3600)}h ${String(Math.floor((s % 3600) / 60)).padStart(2, "0")}m`;
  if (s >= 60) return `${Math.floor(s / 60)}m`;
  return `${s}s`;
}

/**
 * 15_200 → "15.2 Mb/s", 640 → "640 kb/s".
 * @param {number} kbps
 */
export function bitrateText(kbps) {
  return kbps >= 1000 ? `${(kbps / 1000).toFixed(1)} Mb/s` : `${kbps} kb/s`;
}

/**
 * `dolby_vision` (profile 8, base layer 1) → "Dolby Vision 8.1"; `hdr10_plus` → "HDR10+".
 * @param {ProbeVideoTrack} t
 * @returns {string[]}
 */
export function hdrNames(t) {
  return (t.hdr ?? []).map((h) => {
    switch (h) {
      case "dolby_vision": {
        const p = t.dv_profile;
        const c = t.dv_compat;
        // Compatibility id 0 means no compatible base layer (profile 5): no suffix.
        if (p != null && c != null && c !== 0) return `Dolby Vision ${p}.${c}`;
        if (p != null) return `Dolby Vision profile ${p}`;
        return "Dolby Vision";
      }
      case "hdr10":
        return "HDR10";
      case "hdr10_plus":
        return "HDR10+";
      case "hlg":
        return "HLG";
      default:
        return h.toUpperCase();
    }
  });
}

/**
 * "HEVC Main 10 · 3840×2160 · 23.976 fps · 10-bit · Dolby Vision 8.1, HDR10".
 * @param {ProbeVideoTrack} t
 */
export function videoTrackText(t) {
  /** @type {string[]} */
  const parts = [codecName(t.codec ?? "") + (t.profile != null ? ` ${t.profile}` : "")];
  if (t.width != null && t.height != null) parts.push(`${t.width}×${t.height}`);
  else if (t.height != null) parts.push(`${t.height}p`);
  if (t.fps_milli != null) parts.push(fpsText(t.fps_milli));
  if (t.bit_depth != null) parts.push(`${t.bit_depth}-bit`);
  const hdr = hdrNames(t);
  if (hdr.length > 0) parts.push(hdr.join(", "));
  return parts.join(" · ");
}

/** Audio track kinds (D123) in the order they are told. */
export const AUDIO_KIND_ORDER = Object.freeze([
  "original", "dub", "voiceover", "description", "dialogue", "commentary", "music", "karaoke", "alternate",
]);

export const AUDIO_KIND_LABELS = Object.freeze(/** @type {Record<string, string>} */ ({
  original: "original", dub: "dub", voiceover: "voice-over", description: "audio description",
  dialogue: "clear dialogue", commentary: "commentary", music: "music only", karaoke: "karaoke",
  alternate: "alternate mix",
}));

/** Subtitle track kinds (D123) in the order they are told. */
export const SUB_KIND_ORDER = Object.freeze([
  "forced", "sdh", "cc", "signs", "commentary", "description", "honorifics", "bilingual", "easy_reader",
]);

export const SUB_KIND_LABELS = Object.freeze(/** @type {Record<string, string>} */ ({
  forced: "forced", sdh: "SDH", cc: "CC", signs: "signs & songs", commentary: "commentary",
  description: "descriptions", honorifics: "honorifics", bilingual: "bilingual", easy_reader: "easy reader",
}));

/**
 * Known kinds in `order`, then unknown ones as they came, with `_` read as a space.
 * @param {readonly string[]} kinds
 * @param {readonly string[]} order
 * @param {Record<string, string>} labels
 */
function kindLabels(kinds, order, labels) {
  const set = new Set(kinds);
  const known = order.filter((k) => set.has(k)).map((k) => labels[k]);
  const unknown = [...new Set(kinds.filter((k) => !Object.hasOwn(labels, k)))].map((k) => k.replaceAll("_", " "));
  return [...known, ...unknown];
}

/**
 * ["description", "original"] → ["original", "audio description"].
 * @param {readonly string[]} kinds
 */
export function audioKindLabels(kinds) {
  return kindLabels(kinds, AUDIO_KIND_ORDER, AUDIO_KIND_LABELS);
}

/**
 * ["sdh", "forced"] → ["forced", "SDH"].
 * @param {readonly string[]} kinds
 */
export function subKindLabels(kinds) {
  return kindLabels(kinds, SUB_KIND_ORDER, SUB_KIND_LABELS);
}

/**
 * Subtitle format class of a codec (D125): `text`, `image`, `cc`, or null.
 * @param {string} codec
 * @returns {"text" | "image" | "cc" | null}
 */
export function subFormatClass(codec) {
  switch (codec) {
    case "subrip":
    case "ass":
    case "ssa":
    case "webvtt":
    case "mov_text":
    case "ttml":
    case "hdmv_text_subtitle":
    case "dvb_teletext":
    case "arib_caption":
    case "kate":
    case "usf":
      return "text";
    case "hdmv_pgs_subtitle":
    case "dvd_subtitle":
    case "dvb_subtitle":
    case "xsub":
      return "image";
    case "eia_608":
    case "eia_708":
      return "cc";
    default:
      return null;
  }
}

/**
 * "French · E-AC-3 5.1 · Dolby Atmos · audio description". The track name is never shown.
 * @param {ProbeAudioTrack} a
 */
export function audioTrackText(a) {
  /** @type {string[]} */
  const parts = [];
  if (a.lang != null) parts.push(languageName(a.lang));
  parts.push(codecName(a.codec ?? "") + (a.channels != null ? ` ${channelsText(a.channels)}` : ""));
  if (a.atmos) parts.push("Dolby Atmos");
  if (a.dts_x) parts.push("DTS:X");
  return [...parts, ...audioKindLabels(a.kinds ?? [])].join(" · ");
}

/**
 * "English · SDH · PGS", "unknown · CC · CEA-608". The track name is never shown.
 * @param {ProbeSubTrack} s
 */
export function subTrackText(s) {
  return [s.lang != null ? languageName(s.lang) : "unknown", ...subKindLabels(s.kinds ?? []), codecName(s.codec ?? "")]
    .join(" · ");
}

/** Most subtitle lines one file shows; the rest are counted on one more line. */
export const MAX_SUBTITLE_LINES = 10;

/**
 * One line per distinct subtitle track (D126), identical ones grouped in first-seen order.
 * @param {ProbeVideo} v
 * @returns {string[]}
 */
export function subtitleLines(v) {
  const subs = v.subs ?? [];
  /** @type {Map<string, number>} */
  const groups = new Map();
  for (const s of subs) {
    const t = subTrackText(s);
    groups.set(t, (groups.get(t) ?? 0) + 1);
  }
  const shown = [...groups.entries()].slice(0, MAX_SUBTITLE_LINES);
  const rest = subs.length - shown.reduce((a, [, n]) => a + n, 0);
  const out = shown.map(([text, n]) => `Subtitle: ${text}` + (n > 1 ? ` ×${n}` : ""));
  if (rest > 0) out.push(`Subtitles: ${rest} more ` + (rest === 1 ? "track" : "tracks"));
  return out;
}

/**
 * "Matroska · 2h 00m · 15.2 Mb/s".
 * @param {ProbeVideo} v
 */
export function containerText(v) {
  /** @type {string[]} */
  const parts = [];
  const c = v.container ?? "";
  if (c.trim() !== "") parts.push(containerName(c));
  if (v.duration_ms != null) parts.push(durationText(v.duration_ms));
  if (v.bitrate_kbps != null) parts.push(bitrateText(v.bitrate_kbps));
  return parts.join(" · ");
}

/**
 * "PS5 package (PS5 FIH)", then title, ids, region and version.
 * @param {ProbeGame} g
 * @returns {string[]}
 */
export function gameLines(g) {
  /** @type {string[]} */
  const out = [];
  const head = [platformName(g.platform ?? ""), g.kind != null ? gameKindName(g.kind) : null]
    .filter((x) => x != null).join(" ");
  out.push(head + (g.format != null ? ` (${g.format})` : ""));
  if (g.title != null) out.push(`Title: ${g.title}`);
  const ids = [
    g.title_id != null ? `Title ID ${g.title_id}` : null,
    g.content_id != null ? `Content ID ${g.content_id}` : null,
    g.db_id != null && g.db_id !== g.title_id ? `Database ID ${g.db_id}` : null,
  ].filter((x) => x != null);
  if (ids.length > 0) out.push(ids.join(" · "));
  const rv = [g.region != null ? `Region ${g.region}` : null, g.version != null ? `Version ${g.version}` : null]
    .filter((x) => x != null);
  if (rv.length > 0) out.push(rv.join(" · "));
  return out;
}

/**
 * Flags worth telling the reader, warnings first.
 * @param {readonly string[]} flags
 * @returns {[string[], string[]]}
 */
export function flagNotes(flags) {
  /** @type {string[]} */
  const warnings = [];
  if (flags.includes("suspicious_executable")) warnings.push("Suspicious executable");
  if (flags.includes("extension_mismatch")) warnings.push("The file name's extension does not match its contents");
  /** @type {string[]} */
  const notes = [];
  if (flags.includes("incomplete")) notes.push("read partly");
  if (flags.includes("agreed")) notes.push("two peers agreed");
  if (flags.includes("disagree")) notes.push("peers disagreed");
  if (flags.includes("surprise_verified")) notes.push("surprising result, confirmed on hash-checked pieces");
  if (flags.includes("surprise_changed")) notes.push("first guess corrected on hash-checked pieces");
  if (flags.includes("surprise_unverified")) notes.push("surprising result, not confirmed on hash-checked pieces");
  return [warnings, notes];
}

/** @param {string} s */
function capFirst(s) {
  return s === "" ? s : s[0].toUpperCase() + s.slice(1);
}

/**
 * One probed file as the "Detected" section shows it.
 * @typedef {object} ProbeFileView
 * @property {string} name
 * @property {string} headline
 * @property {string[]} lines
 * @property {string[]} warnings
 * @property {boolean} verified
 * @property {ProbeFileInfo} file
 * @property {string[]} videoLines
 * @property {string[]} otherLines
 */

/**
 * @param {ProbeFileInfo} f
 * @param {readonly TorrentFile[]} shown
 * @returns {ProbeFileView}
 */
export function probeFileView(f, shown) {
  const idx = f.index ?? 0;
  const path = (idx >= 0 && idx < shown.length ? shown[idx].path : undefined) ?? f.path ?? "";
  const [warnings, notes] = flagNotes(f.flags ?? []);
  const desc = (f.description ?? "").trim() !== "" ? f.description ?? "" : f.label ?? "";
  const head = [desc];
  if ((f.confidence ?? 0) > 0) head.push(`${f.confidence}%`);
  head.push(f.verified ? "verified" : "unverified");
  /** @type {string[]} */
  const videoLines = [];
  if (f.video) {
    const ct = containerText(f.video);
    if (ct !== "") videoLines.push(ct);
    for (const t of f.video.video ?? []) videoLines.push("Video: " + videoTrackText(t));
  }
  /** @type {string[]} */
  const otherLines = [];
  if (f.game) otherLines.push(...gameLines(f.game));
  if (notes.length > 0) otherLines.push(capFirst(notes.join(" · ")));
  /** @type {string[]} */
  const lines = [...videoLines];
  if (f.video) {
    for (const a of f.video.audio ?? []) lines.push("Audio: " + audioTrackText(a));
    lines.push(...subtitleLines(f.video));
  }
  lines.push(...otherLines);
  const size = f.size ?? 0;
  const name = fileName(path);
  return {
    name: name.trim() !== "" ? name : path,
    headline: head.join(" · ") + (size > 0 ? ` · ${formatBytes(size)}` : ""),
    lines,
    warnings,
    verified: f.verified === true,
    file: f,
    videoLines,
    otherLines,
  };
}

/**
 * "Complete · all hash-checked · probed 3d ago".
 * @param {ProbeInfo} p
 * @param {number} [now]
 */
export function probeSummary(p, now = nowSec()) {
  /** @type {string[]} */
  const parts = [];
  const st = p.status ?? "";
  if (st === "ok") parts.push("Complete");
  else if (st === "partial") parts.push("Partial");
  else if (st !== "") parts.push(capFirst(st));
  parts.push(p.verified ? "all hash-checked" : "not all hash-checked");
  if ((p.probed_at ?? 0) > 0) parts.push("probed " + formatAgo(/** @type {number} */ (p.probed_at), now));
  return parts.join(" · ");
}

/**
 * What to show, or null to hide the section (never probed, an older server, nothing kept).
 * @param {ProbeInfo | null | undefined} p
 * @param {readonly TorrentFile[]} shown
 * @returns {ProbeFileView[] | null}
 */
export function probeViews(p, shown) {
  const files = p?.files ?? [];
  if (files.length === 0) return null;
  return [...files].sort((a, b) => (a.index ?? 0) - (b.index ?? 0)).map((f) => probeFileView(f, shown));
}
