package app.magnetar.ui.common

import app.magnetar.api.Category
import app.magnetar.api.MediaKind
import app.magnetar.api.Origin
import app.magnetar.api.ReleaseAttrs

/*
 * Display labels of filter values and release attributes. A Kotlin copy of
 * `magnetar_core::release::{facet_label, language_label, ReleaseAttrs::language_key}` and the
 * old ffi `badges` (the app has no Rust since D79); `LabelsTest` pins them against the Rust
 * outputs. Labels only: nothing here decides what is shown or hidden.
 */

/** Values offered for a filter when no counts are available (index building, no `facet_set`). */
val FILTER_OPTIONS: Map<String, List<String>> = mapOf(
    "language" to listOf(
        "fr", "fr-fr", "fr-ca", "original", "multi", "en", "ja", "es", "es-419", "de", "it",
        "pt", "pt-br", "ko", "zh", "ru", "hi",
    ),
    "subtitles" to listOf("fr", "en", "multi", "es", "de", "it", "pt", "ja", "zh", "ru"),
    "resolution" to listOf("4320p", "2160p", "1440p", "1080p", "720p", "576p", "480p", "sd"),
    "source" to listOf(
        "remux", "bluray", "web-dl", "webrip", "hdrip", "hdtv", "dvd", "vhs", "screener", "telesync", "cam",
    ),
    "codec" to listOf("h265", "h264", "av1", "vp9", "xvid", "mpeg2", "vc1"),
    "hdr" to listOf("dv", "hdr10+", "hdr10", "hlg"),
    "audio" to listOf(
        "truehd", "atmos", "dts-x", "dts-hd", "dts", "eac3", "ac3", "aac", "flac", "opus", "mp3",
        "pcm", "alac", "vorbis", "dsd",
    ),
    "media_kind" to listOf("movie", "series", "anime"),
)

private val FIXED_LABELS: Map<String, String> = mapOf(
    "resolution/4320p" to "4320p (8K)",
    "resolution/2160p" to "2160p (4K)",
    "resolution/sd" to "SD",
    "source/remux" to "Remux",
    "source/bluray" to "BluRay",
    "source/web-dl" to "WEB-DL",
    "source/webrip" to "WEBRip",
    "source/hdrip" to "HDRip",
    "source/hdtv" to "HDTV",
    "source/dvd" to "DVD",
    "source/vhs" to "VHS",
    "source/screener" to "Screener",
    "source/telesync" to "TS/TC",
    "source/cam" to "CAM",
    "codec/h265" to "H.265/HEVC",
    "codec/h264" to "H.264/AVC",
    "codec/av1" to "AV1",
    "codec/vp9" to "VP9",
    "codec/xvid" to "XviD/DivX",
    "codec/mpeg2" to "MPEG-2",
    "codec/vc1" to "VC-1",
    "hdr/dv" to "Dolby Vision",
    "hdr/hdr10+" to "HDR10+",
    "hdr/hdr10" to "HDR10",
    "hdr/hlg" to "HLG",
    "audio/truehd" to "TrueHD",
    "audio/atmos" to "Atmos",
    "audio/dts-x" to "DTS:X",
    "audio/dts-hd" to "DTS-HD MA",
    "audio/dts" to "DTS",
    "audio/eac3" to "DD+ (E-AC-3)",
    "audio/ac3" to "DD (AC-3)",
    "audio/aac" to "AAC",
    "audio/flac" to "FLAC",
    "audio/opus" to "Opus",
    "audio/mp3" to "MP3",
    "audio/pcm" to "PCM",
    "audio/alac" to "ALAC",
    "audio/vorbis" to "Vorbis",
    "audio/dsd" to "DSD",
    "media_kind/movie" to "Movie",
    "media_kind/series" to "Series",
    "media_kind/anime" to "Anime",
    "subtitles/multi" to "Multi-subs",
    "origin/nostr" to "Nostr",
    "origin/dht" to "DHT",
)

private val LANGUAGE_LABELS: Map<String, String> = mapOf(
    "multi-vff" to "MULTi (VFF)",
    "multi-vfq" to "MULTi (VFQ)",
    "multi-vf2" to "MULTi (VF2)",
    "multi-vfi" to "MULTi (VFI)",
    "multi" to "MULTi",
    "vff" to "VFF (France)",
    "vfq" to "VFQ (Québec)",
    "vf2" to "VF2 (France + Québec)",
    "vfi" to "VFI (international French)",
    "vof" to "VOF (French original)",
    "french" to "French",
    "fr" to "French",
    "fr-fr" to "French (France)",
    "fr-ca" to "French (Québec)",
    "vostfr" to "VOSTFR",
    "original" to "Original (VO)",
    "unknown" to "Unknown",
    "en" to "English",
    "de" to "German",
    "it" to "Italian",
    "es" to "Spanish",
    "es-419" to "Spanish (Latino)",
    "ja" to "Japanese",
    "ko" to "Korean",
    "ru" to "Russian",
    "pt" to "Portuguese",
    "pt-br" to "Portuguese (Brazil)",
    "zh" to "Chinese",
    "nl" to "Dutch",
    "pl" to "Polish",
    "hi" to "Hindi",
    "tr" to "Turkish",
    "ar" to "Arabic",
    "sv" to "Swedish",
    "no" to "Norwegian",
    "da" to "Danish",
    "fi" to "Finnish",
    "cs" to "Czech",
    "hu" to "Hungarian",
    "uk" to "Ukrainian",
    "th" to "Thai",
    "vi" to "Vietnamese",
    "el" to "Greek",
    "he" to "Hebrew",
    "ta" to "Tamil",
    "te" to "Telugu",
    "ro" to "Romanian",
)

/** `release::language_label`: "multi-vff" → "MULTi (VFF)", "fr-ca" → "French (Québec)". */
fun languageLabel(key: String): String = LANGUAGE_LABELS[key] ?: key.uppercase()

/** `release::facet_label`: "language"/"fr-ca" → "French (Québec)", "source"/"web-dl" → "WEB-DL". */
fun facetLabel(field: String, value: String): String {
    FIXED_LABELS["$field/$value"]?.let { return it }
    return when (field) {
        "language", "subtitles" -> if (value == "multi") "Multi-language" else languageLabel(value)
        "genre", "edition" -> value.replaceFirstChar { it.uppercaseChar() }
        "format" -> value.uppercase()
        "category" -> categoryLabel(value)
        else -> value
    }
}

/** `ReleaseAttrs::language_key`: the language group of the title page. */
fun languageKey(a: ReleaseAttrs): String {
    fun has(t: String) = t in a.french
    val variant = when {
        has("vf2") || (has("vff") && has("vfq")) -> "vf2"
        has("vff") || has("truefrench") -> "vff"
        has("vfq") -> "vfq"
        has("vfi") -> "vfi"
        else -> null
    }
    if (has("multi") || "multi" in a.languages) return if (variant != null) "multi-$variant" else "multi"
    if (variant != null) return variant
    if (has("vof")) return "vof"
    if (has("vostfr") || has("subfrench")) return "vostfr"
    if ("fr" in a.languages) return "french"
    val first = a.languages.firstOrNull { it != "original" }
    return when {
        first != null -> first.substringBefore('-')
        "original" in a.languages -> "original"
        else -> "unknown"
    }
}

/**
 * Short labels for a result row (the old ffi `badges`): "2160p", "WEB-DL", "H.265/HEVC",
 * "Dolby Vision", "MULTi (VFF)", "DD+ (E-AC-3) 5.1", "EPUB", "24 bit", "320 kbps".
 */
fun attrBadges(a: ReleaseAttrs): List<String> = buildList {
    a.resolution?.let { add(it) }
    a.source?.let { add(facetLabel("source", it)) }
    a.videoCodec?.let { add(facetLabel("codec", it)) }
    a.hdr.firstOrNull()?.let { add(facetLabel("hdr", it)) }
    val key = languageKey(a)
    if (key != "unknown") add(languageLabel(key))
    a.audio.firstOrNull()?.let { c ->
        add(facetLabel("audio", c) + (a.channels?.let { " $it" } ?: ""))
    }
    a.formats.forEach { add(it.uppercase()) }
    a.bitDepth?.takeIf { it >= 16 }?.let { add("$it bit") }
    a.bitrate?.let { add("$it kbps") }
}

/** The detail screen's longer attribute line: the badges plus subtitles, editions and group. */
fun attrDetails(a: ReleaseAttrs): List<String> = attrBadges(a) + listOfNotNull(
    a.subtitles.takeIf { it.isNotEmpty() }?.let { s -> "subs " + s.joinToString("/") { facetLabel("subtitles", it) } },
    a.editions.takeIf { it.isNotEmpty() }?.joinToString(", ") { facetLabel("edition", it) },
    a.group?.let { "group $it" },
)

fun categoryLabel(code: String): String = code.replaceFirstChar { it.uppercaseChar() }

fun categoryLabel(c: Category): String = categoryLabel(c.code)

val Category.code: String get() = name.lowercase()

fun mediaKindLabel(k: MediaKind): String = facetLabel("media_kind", k.name.lowercase())

/** Where a torrent is known from, for the detail screen (D51). */
fun sourceLabel(origin: Origin): String = when (origin) {
    Origin.NOSTR -> "Nostr"
    Origin.DHT -> "DHT (verified metadata, no publisher)"
    Origin.BOTH -> "Nostr and DHT"
}

/** "Title (2011)" or "Title (2011–2019)". */
fun mediaHeadline(title: String, year: Int?, endYear: Int? = null): String = when {
    year == null -> title
    endYear != null && endYear != year -> "$title ($year–$endYear)"
    else -> "$title ($year)"
}
