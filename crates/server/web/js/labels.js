// Port of android/.../ui/common/Labels.kt (and the row helpers of ResultRow.kt / MediaCard.kt):
// display labels of filter values and release attributes. Labels only: nothing here decides
// what is shown or hidden.

import { formatAge, formatBytes, formatRating, peersText } from "./format.js";

/** @typedef {import("./types.js").ReleaseAttrs} ReleaseAttrs */
/** @typedef {import("./types.js").TorrentSummary} TorrentSummary */
/** @typedef {import("./types.js").MediaInfo} MediaInfo */

/** Values offered for a filter when no counts are available (index building, no facet_set). */
export const FILTER_OPTIONS = Object.freeze(/** @type {Record<string, readonly string[]>} */ ({
  language: [
    "fr", "fr-fr", "fr-ca", "original", "multi", "en", "ja", "es", "es-419", "de", "it",
    "pt", "pt-br", "ko", "zh", "ru", "hi",
  ],
  subtitles: ["fr", "en", "multi", "es", "de", "it", "pt", "ja", "zh", "ru"],
  resolution: ["4320p", "2160p", "1440p", "1080p", "720p", "576p", "480p", "sd"],
  source: ["remux", "bluray", "web-dl", "webrip", "hdrip", "hdtv", "dvd", "vhs", "screener", "telesync", "cam"],
  codec: ["h265", "h264", "av1", "vp9", "xvid", "mpeg2", "vc1"],
  hdr: ["dv", "hdr10+", "hdr10", "hlg"],
  audio: [
    "truehd", "atmos", "dts-x", "dts-hd", "dts", "eac3", "ac3", "aac", "flac", "opus", "mp3",
    "pcm", "alac", "vorbis", "dsd",
  ],
  media_kind: ["movie", "series", "anime"],
  probe_language: ["fre", "eng", "jpn", "spa", "ger", "ita"],
  probe_subtitles: ["fre", "eng", "spa", "ger"],
  probe_resolution: ["2160p", "1080p", "720p", "576p", "480p", "sd"],
  probe_codec: ["hevc", "h264", "av1", "vp9"],
  probe_hdr: ["any", "dolby_vision", "hdr10_plus", "hdr10", "hlg"],
  probe_audio: ["atmos", "dts_x", "truehd", "dts", "eac3", "ac3", "aac", "flac", "opus"],
  probe_audio_kind: [
    "original", "dub", "voiceover", "description", "dialogue", "commentary", "music", "karaoke", "alternate",
  ],
  probe_sub_kind: [
    "sdh", "cc", "forced", "signs", "commentary", "description", "honorifics", "bilingual", "easy_reader",
  ],
  probe_sub_format: ["text", "image", "cc"],
}));

/** @type {Readonly<Record<string, string>>} */
export const FIXED_LABELS = Object.freeze({
  "resolution/4320p": "4320p (8K)",
  "resolution/2160p": "2160p (4K)",
  "resolution/sd": "SD",
  "source/remux": "Remux",
  "source/bluray": "BluRay",
  "source/web-dl": "WEB-DL",
  "source/webrip": "WEBRip",
  "source/hdrip": "HDRip",
  "source/hdtv": "HDTV",
  "source/dvd": "DVD",
  "source/vhs": "VHS",
  "source/screener": "Screener",
  "source/telesync": "TS/TC",
  "source/cam": "CAM",
  "codec/h265": "H.265/HEVC",
  "codec/h264": "H.264/AVC",
  "codec/av1": "AV1",
  "codec/vp9": "VP9",
  "codec/xvid": "XviD/DivX",
  "codec/mpeg2": "MPEG-2",
  "codec/vc1": "VC-1",
  "hdr/dv": "Dolby Vision",
  "hdr/hdr10+": "HDR10+",
  "hdr/hdr10": "HDR10",
  "hdr/hlg": "HLG",
  "audio/truehd": "TrueHD",
  "audio/atmos": "Atmos",
  "audio/dts-x": "DTS:X",
  "audio/dts-hd": "DTS-HD MA",
  "audio/dts": "DTS",
  "audio/eac3": "DD+ (E-AC-3)",
  "audio/ac3": "DD (AC-3)",
  "audio/aac": "AAC",
  "audio/flac": "FLAC",
  "audio/opus": "Opus",
  "audio/mp3": "MP3",
  "audio/pcm": "PCM",
  "audio/alac": "ALAC",
  "audio/vorbis": "Vorbis",
  "audio/dsd": "DSD",
  "media_kind/movie": "Movie",
  "media_kind/series": "Series",
  "media_kind/anime": "Anime",
  "subtitles/multi": "Multi-subs",
  "origin/nostr": "Nostr",
  "origin/dht": "DHT",
  "probe_resolution/2160p": "2160p (4K and up)",
  "probe_resolution/sd": "SD",
  "probe_codec/hevc": "H.265/HEVC",
  "probe_codec/h264": "H.264/AVC",
  "probe_codec/av1": "AV1",
  "probe_codec/vp9": "VP9",
  "probe_codec/vp8": "VP8",
  "probe_codec/mpeg4": "MPEG-4 (XviD/DivX)",
  "probe_codec/mpeg2video": "MPEG-2",
  "probe_codec/vc1": "VC-1",
  "probe_hdr/any": "Any HDR",
  "probe_hdr/dolby_vision": "Dolby Vision",
  "probe_hdr/hdr10_plus": "HDR10+",
  "probe_hdr/hdr10": "HDR10",
  "probe_hdr/hlg": "HLG",
  "probe_audio/atmos": "Atmos",
  "probe_audio/dts_x": "DTS:X",
  "probe_audio/truehd": "TrueHD",
  "probe_audio/mlp": "MLP",
  "probe_audio/dts": "DTS",
  "probe_audio/dtsuhd": "DTS-UHD",
  "probe_audio/eac3": "DD+ (E-AC-3)",
  "probe_audio/ac3": "DD (AC-3)",
  "probe_audio/aac": "AAC",
  "probe_audio/flac": "FLAC",
  "probe_audio/opus": "Opus",
  "probe_audio/mp3": "MP3",
  "probe_audio/mp2": "MP2",
  "probe_audio/vorbis": "Vorbis",
  "probe_audio/pcm": "PCM",
  "probe_audio_kind/original": "Original",
  "probe_audio_kind/dub": "Dubbed",
  "probe_audio_kind/voiceover": "Voice-over",
  "probe_audio_kind/description": "Audio description",
  "probe_audio_kind/dialogue": "Clear dialogue",
  "probe_audio_kind/commentary": "Commentary",
  "probe_audio_kind/music": "Music only",
  "probe_audio_kind/karaoke": "Karaoke",
  "probe_audio_kind/alternate": "Alternate mix",
  "probe_sub_kind/sdh": "SDH",
  "probe_sub_kind/cc": "Closed captions",
  "probe_sub_kind/forced": "Forced",
  "probe_sub_kind/signs": "Signs & songs",
  "probe_sub_kind/commentary": "Commentary",
  "probe_sub_kind/description": "Descriptions",
  "probe_sub_kind/honorifics": "Honorifics",
  "probe_sub_kind/bilingual": "Bilingual",
  "probe_sub_kind/easy_reader": "Easy reader",
  "probe_sub_format/text": "Text (SRT, ASS…)",
  "probe_sub_format/image": "Image (PGS, VobSub…)",
  "probe_sub_format/cc": "Closed captions (608/708)",
});

/** ISO 639-2/B codes the probes store (D117) → names; others show upper-cased. */
/** @type {Readonly<Record<string, string>>} */
export const ISO639B_LABELS = Object.freeze({
  eng: "English", fre: "French", ger: "German", spa: "Spanish", ita: "Italian",
  por: "Portuguese", rus: "Russian", jpn: "Japanese", chi: "Chinese", kor: "Korean",
  ara: "Arabic", dut: "Dutch", swe: "Swedish", nor: "Norwegian", nob: "Norwegian Bokmål",
  dan: "Danish", fin: "Finnish", pol: "Polish", cze: "Czech", slo: "Slovak",
  hun: "Hungarian", rum: "Romanian", bul: "Bulgarian", gre: "Greek", tur: "Turkish",
  heb: "Hebrew", hin: "Hindi", tha: "Thai", vie: "Vietnamese", ind: "Indonesian",
  may: "Malay", ukr: "Ukrainian", hrv: "Croatian", srp: "Serbian", slv: "Slovenian",
  est: "Estonian", lav: "Latvian", lit: "Lithuanian", per: "Persian", ice: "Icelandic",
  cat: "Catalan", baq: "Basque", glg: "Galician", tam: "Tamil", tel: "Telugu",
  ben: "Bengali", urd: "Urdu", fil: "Filipino", tgl: "Tagalog", mal: "Malayalam",
  kan: "Kannada", lat: "Latin",
});

/**
 * A probe's track language (ISO 639-2/B): "fre" → "French".
 * @param {string} code
 */
export function iso639bLabel(code) {
  return Object.hasOwn(ISO639B_LABELS, code) ? ISO639B_LABELS[code] : code.toUpperCase();
}

/** @type {Readonly<Record<string, string>>} */
export const LANGUAGE_LABELS = Object.freeze({
  "multi-vff": "MULTi (VFF)",
  "multi-vfq": "MULTi (VFQ)",
  "multi-vf2": "MULTi (VF2)",
  "multi-vfi": "MULTi (VFI)",
  "multi": "MULTi",
  "vff": "VFF (France)",
  "vfq": "VFQ (Québec)",
  "vf2": "VF2 (France + Québec)",
  "vfi": "VFI (international French)",
  "vof": "VOF (French original)",
  "french": "French",
  "fr": "French",
  "fr-fr": "French (France)",
  "fr-ca": "French (Québec)",
  "vostfr": "VOSTFR",
  "original": "Original (VO)",
  "unknown": "Unknown",
  "en": "English",
  "de": "German",
  "it": "Italian",
  "es": "Spanish",
  "es-419": "Spanish (Latino)",
  "ja": "Japanese",
  "ko": "Korean",
  "ru": "Russian",
  "pt": "Portuguese",
  "pt-br": "Portuguese (Brazil)",
  "zh": "Chinese",
  "nl": "Dutch",
  "pl": "Polish",
  "hi": "Hindi",
  "tr": "Turkish",
  "ar": "Arabic",
  "sv": "Swedish",
  "no": "Norwegian",
  "da": "Danish",
  "fi": "Finnish",
  "cs": "Czech",
  "hu": "Hungarian",
  "uk": "Ukrainian",
  "th": "Thai",
  "vi": "Vietnamese",
  "el": "Greek",
  "he": "Hebrew",
  "ta": "Tamil",
  "te": "Telugu",
  "ro": "Romanian",
});

/**
 * `release::language_label`: "multi-vff" → "MULTi (VFF)".
 * @param {string} key
 */
export function languageLabel(key) {
  return Object.hasOwn(LANGUAGE_LABELS, key) ? LANGUAGE_LABELS[key] : key.toUpperCase();
}

/** @param {string} s */
function capitalize(s) {
  return s.length === 0 ? s : s[0].toUpperCase() + s.slice(1);
}

/**
 * Category code → label: "video" → "Video".
 * @param {string} code
 */
export function categoryLabel(code) {
  return capitalize(code);
}

/**
 * `release::facet_label`: "language"/"fr-ca" → "French (Québec)".
 * @param {string} field
 * @param {string} value
 * @returns {string}
 */
export function facetLabel(field, value) {
  const fixed = `${field}/${value}`;
  if (Object.hasOwn(FIXED_LABELS, fixed)) return FIXED_LABELS[fixed];
  switch (field) {
    case "language":
    case "subtitles":
      return value === "multi" ? "Multi-language" : languageLabel(value);
    case "genre":
    case "edition":
      return capitalize(value);
    case "format":
      return value.toUpperCase();
    case "category":
      return categoryLabel(value);
    case "probe_language":
    case "probe_subtitles":
      return iso639bLabel(value);
    case "probe_audio_kind":
    case "probe_sub_kind":
    case "probe_sub_format":
      return capitalize(value.replaceAll("_", " "));
    default:
      return value;
  }
}

/**
 * `ReleaseAttrs::language_key`: the language group of the title page.
 * @param {ReleaseAttrs} a
 */
export function languageKey(a) {
  const french = a.french ?? [];
  const languages = a.languages ?? [];
  /** @param {string} t */
  const has = (t) => french.includes(t);
  let variant = null;
  if (has("vf2") || (has("vff") && has("vfq"))) variant = "vf2";
  else if (has("vff") || has("truefrench")) variant = "vff";
  else if (has("vfq")) variant = "vfq";
  else if (has("vfi")) variant = "vfi";
  if (has("multi") || languages.includes("multi")) return variant != null ? `multi-${variant}` : "multi";
  if (variant != null) return variant;
  if (has("vof")) return "vof";
  if (has("vostfr") || has("subfrench")) return "vostfr";
  if (languages.includes("fr")) return "french";
  const first = languages.find((l) => l !== "original");
  if (first != null) return first.split("-")[0];
  if (languages.includes("original")) return "original";
  return "unknown";
}

/**
 * Short labels for a result row: "2160p", "WEB-DL", "H.265/HEVC", "Dolby Vision", …
 * @param {ReleaseAttrs} a
 * @returns {string[]}
 */
export function attrBadges(a) {
  /** @type {string[]} */
  const out = [];
  if (a.resolution != null) out.push(a.resolution);
  if (a.source != null) out.push(facetLabel("source", a.source));
  if (a.video_codec != null) out.push(facetLabel("codec", a.video_codec));
  const hdr = (a.hdr ?? [])[0];
  if (hdr != null) out.push(facetLabel("hdr", hdr));
  const key = languageKey(a);
  if (key !== "unknown") out.push(languageLabel(key));
  const audio = (a.audio ?? [])[0];
  if (audio != null) out.push(facetLabel("audio", audio) + (a.channels != null ? ` ${a.channels}` : ""));
  for (const f of a.formats ?? []) out.push(f.toUpperCase());
  if (a.bit_depth != null && a.bit_depth >= 16) out.push(`${a.bit_depth} bit`);
  if (a.bitrate != null) out.push(`${a.bitrate} kbps`);
  return out;
}

/**
 * The detail's longer attribute line: the badges plus subtitles, editions and group.
 * @param {ReleaseAttrs} a
 * @returns {string[]}
 */
export function attrDetails(a) {
  const out = attrBadges(a);
  const subs = a.subtitles ?? [];
  if (subs.length > 0) out.push("subs " + subs.map((s) => facetLabel("subtitles", s)).join("/"));
  const eds = a.editions ?? [];
  if (eds.length > 0) out.push(eds.map((e) => facetLabel("edition", e)).join(", "));
  if (a.group != null) out.push(`group ${a.group}`);
  return out;
}

/**
 * "Movie", "Series", "Anime".
 * @param {string} kind
 */
export function mediaKindLabel(kind) {
  return facetLabel("media_kind", kind);
}

/**
 * Where a torrent is known from, for the detail (D51).
 * @param {string | undefined} origin
 */
export function sourceLabel(origin) {
  switch (origin) {
    case "dht":
      return "DHT (verified metadata, no publisher)";
    case "both":
      return "Nostr and DHT";
    default:
      return "Nostr";
  }
}

/**
 * "Title (2011)" or "Title (2011–2019)".
 * @param {string} title
 * @param {number | null | undefined} year
 * @param {number | null | undefined} [endYear]
 */
export function mediaHeadline(title, year, endYear = null) {
  if (year == null) return title;
  if (endYear != null && endYear !== year) return `${title} (${year}–${endYear})`;
  return `${title} (${year})`;
}

/**
 * "812k", "1.2M".
 * @param {number} v
 */
export function votesText(v) {
  if (v >= 1_000_000) return `${(v / 1_000_000).toFixed(1)}M`;
  if (v >= 1_000) return `${Math.round(v / 1_000)}k`;
  return String(v);
}

/**
 * "S01E02 · The Kingsroad", "Season 1", or null.
 * @param {MediaInfo} m
 * @returns {string | null}
 */
export function seasonEpisodeText(m) {
  const s = m.season;
  if (s == null) return null;
  const e = m.episode;
  const code = e != null && e > 0
    ? `S${String(s).padStart(2, "0")}E${String(e).padStart(2, "0")}`
    : `Season ${s}`;
  return [code, m.episode_title].filter((x) => x != null && x !== "").join(" · ");
}

/**
 * "Movie · ★ 7.4 (812k) · 155 min".
 * @param {MediaInfo} m
 */
export function mediaFacts(m) {
  /** @type {string[]} */
  const parts = [mediaKindLabel(m.kind ?? "movie")];
  if (m.rating != null) parts.push(formatRating(m.rating) + (m.votes != null ? ` (${votesText(m.votes)})` : ""));
  if (m.runtime_min != null && m.runtime_min > 0) parts.push(`${m.runtime_min} min`);
  return parts.join(" · ");
}

/**
 * The part of a headline after the (untrusted) title: " (2011)", " (2011–2019)" or "". Shown in
 * its own text node, so a right-to-left override in the title cannot reverse the years.
 * @param {number | null | undefined} year
 * @param {number | null | undefined} [endYear]
 */
export function headlineSuffix(year, endYear = null) {
  return mediaHeadline("", year, endYear);
}

/**
 * The media line of a hit split in its untrusted title and the app's own suffix
 * (" (2024) · ★ 8.5"), or null.
 * @param {TorrentSummary} hit
 * @returns {{title: string, rest: string} | null}
 */
export function mediaLineParts(hit) {
  const t = hit.media_title;
  if (t == null) return null;
  let rest = headlineSuffix(hit.media_year);
  if (hit.media_rating != null) rest += ` · ${formatRating(hit.media_rating)}`;
  return { title: t, rest };
}

/**
 * "Dune: Part Two (2024) · ★ 8.5" for a hit linked to a title (D40), else null.
 * @param {TorrentSummary} hit
 * @returns {string | null}
 */
export function mediaLine(hit) {
  const p = mediaLineParts(hit);
  return p ? p.title + p.rest : null;
}

/**
 * "4.37 GiB · 12 S / 3 L · dead · 5d".
 * @param {TorrentSummary} hit
 * @param {number} [now]
 */
export function statsLine(hit, now) {
  const parts = [
    formatBytes(hit.total_size),
    peersText(hit.seeders, hit.leechers, hit.best_peers),
  ];
  if (hit.dead === true) parts.push("dead");
  parts.push(formatAge(hit.created_at ?? 0, now));
  return parts.join(" · ");
}
