package app.magnetar.ui.detail

import app.magnetar.api.ProbeAudioTrack
import app.magnetar.api.ProbeFileInfo
import app.magnetar.api.ProbeGame
import app.magnetar.api.ProbeInfo
import app.magnetar.api.ProbeSubTrack
import app.magnetar.api.ProbeVideo
import app.magnetar.api.ProbeVideoTrack
import app.magnetar.api.TorrentFile
import app.magnetar.ui.common.fileName
import app.magnetar.ui.common.formatAgo
import app.magnetar.ui.common.formatBytes
import java.util.Locale

/*
 * The detail's "Detected" section (D121): what a worker's header probe read from the files
 * themselves (D117), as plain text lines. Pure functions, so the wording is unit-tested.
 */

/** ISO 639-2 (bibliographic, plus the terminology codes that differ) → English name. */
private val ISO639_2: Map<String, String> = mapOf(
    "eng" to "English", "fre" to "French", "fra" to "French", "ger" to "German", "deu" to "German",
    "spa" to "Spanish", "ita" to "Italian", "por" to "Portuguese", "rus" to "Russian", "jpn" to "Japanese",
    "chi" to "Chinese", "zho" to "Chinese", "kor" to "Korean", "ara" to "Arabic", "dut" to "Dutch", "nld" to "Dutch",
    "swe" to "Swedish", "nor" to "Norwegian", "nob" to "Norwegian Bokmål", "nno" to "Norwegian Nynorsk",
    "dan" to "Danish", "fin" to "Finnish", "pol" to "Polish", "cze" to "Czech", "ces" to "Czech",
    "slo" to "Slovak", "slk" to "Slovak", "hun" to "Hungarian", "rum" to "Romanian", "ron" to "Romanian",
    "bul" to "Bulgarian", "gre" to "Greek", "ell" to "Greek", "tur" to "Turkish", "heb" to "Hebrew",
    "hin" to "Hindi", "tha" to "Thai", "vie" to "Vietnamese", "ind" to "Indonesian", "may" to "Malay",
    "msa" to "Malay", "ukr" to "Ukrainian", "hrv" to "Croatian", "srp" to "Serbian", "slv" to "Slovenian",
    "est" to "Estonian", "lav" to "Latvian", "lit" to "Lithuanian", "per" to "Persian", "fas" to "Persian",
    "ice" to "Icelandic", "isl" to "Icelandic", "cat" to "Catalan", "baq" to "Basque", "eus" to "Basque",
    "glg" to "Galician", "tam" to "Tamil", "tel" to "Telugu", "ben" to "Bengali", "urd" to "Urdu",
    "alb" to "Albanian", "sqi" to "Albanian", "mac" to "Macedonian", "mkd" to "Macedonian",
    "arm" to "Armenian", "hye" to "Armenian", "geo" to "Georgian", "kat" to "Georgian", "wel" to "Welsh",
    "cym" to "Welsh", "gle" to "Irish", "lat" to "Latin", "tgl" to "Tagalog", "fil" to "Filipino",
    "mal" to "Malayalam", "kan" to "Kannada", "mar" to "Marathi", "pan" to "Punjabi", "guj" to "Gujarati",
    "bur" to "Burmese", "mya" to "Burmese", "tib" to "Tibetan", "bod" to "Tibetan", "khm" to "Khmer",
    "lao" to "Lao", "mon" to "Mongolian", "nep" to "Nepali", "sin" to "Sinhala", "swa" to "Swahili",
    "afr" to "Afrikaans", "amh" to "Amharic", "aze" to "Azerbaijani", "bel" to "Belarusian", "bos" to "Bosnian",
    "kaz" to "Kazakh", "uzb" to "Uzbek", "yue" to "Cantonese", "cmn" to "Mandarin", "epo" to "Esperanto",
    "haw" to "Hawaiian", "mao" to "Maori", "mri" to "Maori", "som" to "Somali", "yid" to "Yiddish",
    "zul" to "Zulu", "xho" to "Xhosa", "kur" to "Kurdish", "pus" to "Pashto", "tat" to "Tatar",
)

/** "fre" → "French"; an unknown code stays as it came ("tlh"). */
fun languageName(code: String): String = ISO639_2[code.lowercase(Locale.ROOT)] ?: code

private val CODECS: Map<String, String> = mapOf(
    "hevc" to "HEVC", "h264" to "H.264", "av1" to "AV1", "vp9" to "VP9", "vp8" to "VP8", "vvc" to "VVC",
    "mpeg2video" to "MPEG-2", "mpeg1video" to "MPEG-1", "mpeg4" to "MPEG-4 Part 2", "vc1" to "VC-1",
    "wmv3" to "WMV 9", "prores" to "ProRes", "theora" to "Theora",
    "aac" to "AAC", "ac3" to "AC-3", "eac3" to "E-AC-3", "truehd" to "TrueHD", "mlp" to "MLP", "dts" to "DTS",
    "dts_hd" to "DTS-HD", "dts_hd_ma" to "DTS-HD MA", "flac" to "FLAC", "opus" to "Opus", "vorbis" to "Vorbis",
    "mp3" to "MP3", "mp2" to "MP2", "pcm" to "PCM", "alac" to "ALAC", "wmav2" to "WMA",
    "subrip" to "SRT", "ass" to "ASS", "ssa" to "SSA", "webvtt" to "WebVTT", "mov_text" to "MP4 text",
    "hdmv_pgs_subtitle" to "PGS", "dvd_subtitle" to "VobSub", "dvb_subtitle" to "DVB", "eia_608" to "CEA-608",
    "eia_708" to "CEA-708", "ttml" to "TTML", "arib_caption" to "ARIB", "dvb_teletext" to "Teletext",
    "hdmv_text_subtitle" to "TextST", "xsub" to "XSUB", "kate" to "Kate", "usf" to "USF",
)

/** FFmpeg codec name → what people call it ("eac3" → "E-AC-3"). */
fun codecName(codec: String): String = CODECS[codec] ?: codec.uppercase(Locale.ROOT)

private val CONTAINERS: Map<String, String> = mapOf(
    "matroska" to "Matroska", "mkv" to "Matroska", "webm" to "WebM", "mp4" to "MP4", "mov" to "QuickTime",
    "avi" to "AVI", "mpegts" to "MPEG-TS", "ts" to "MPEG-TS", "m2ts" to "Blu-ray M2TS", "flv" to "FLV",
    "ogg" to "Ogg", "asf" to "ASF/WMV", "mpegps" to "MPEG-PS",
)

fun containerName(c: String): String = CONTAINERS[c] ?: c.uppercase(Locale.ROOT)

private val PLATFORMS: Map<String, String> = mapOf(
    "ps5" to "PS5", "ps4" to "PS4", "ps3" to "PS3", "psp" to "PSP", "ps_vita" to "PS Vita",
    "psp_or_vita" to "PSP or PS Vita", "ps1_or_ps2" to "PS1 or PS2", "ps1" to "PS1", "ps2" to "PS2",
    "switch" to "Nintendo Switch", "xbox" to "Xbox", "xbox360" to "Xbox 360", "xbox_one" to "Xbox One",
    "wii" to "Wii", "wii_u" to "Wii U", "game_cube" to "GameCube", "wii_or_game_cube" to "Wii or GameCube",
    "n3ds" to "Nintendo 3DS", "nds" to "Nintendo DS", "gba" to "Game Boy Advance", "gb" to "Game Boy",
    "gbc" to "Game Boy Color", "nes" to "NES", "fds" to "Famicom Disk System", "snes" to "SNES",
    "n64" to "Nintendo 64", "master_system" to "Master System", "game_gear" to "Game Gear",
    "genesis" to "Mega Drive / Genesis", "sega32x" to "32X", "sega_cd" to "Sega CD", "saturn" to "Saturn",
    "dreamcast" to "Dreamcast", "lynx" to "Atari Lynx", "atari7800" to "Atari 7800",
    "neo_geo_pocket" to "Neo Geo Pocket",
)

/** sniff platform ("ps_vita") → "PS Vita"; an unknown one reads with spaces. */
fun platformName(p: String): String = PLATFORMS[p] ?: p.replace('_', ' ')

private fun gameKindName(k: String): String = when (k) {
    "package" -> "package"
    "image" -> "disc image"
    "compressed_image" -> "compressed image"
    "metadata" -> "metadata file"
    else -> k.replace('_', ' ')
}

/** 2 → "2.0", 6 → "5.1", 8 → "7.1", 1 → "mono". */
fun channelsText(n: Int): String = when (n) {
    1 -> "mono"
    2 -> "2.0"
    3 -> "2.1"
    6 -> "5.1"
    7 -> "6.1"
    8 -> "7.1"
    else -> "$n ch"
}

/** 23976 → "23.976 fps", 25000 → "25 fps". */
fun fpsText(milli: Int): String {
    val s = if (milli % 1000 == 0) (milli / 1000).toString()
    else String.format(Locale.ROOT, "%.3f", milli / 1000.0).trimEnd('0').trimEnd('.')
    return "$s fps"
}

/** 7_200_000 → "2h 00m", 2_700_000 → "45m", 42_000 → "42s". */
fun durationText(ms: Long): String {
    val s = ms / 1000
    return when {
        s >= 3600 -> String.format(Locale.ROOT, "%dh %02dm", s / 3600, s % 3600 / 60)
        s >= 60 -> "${s / 60}m"
        else -> "${s}s"
    }
}

/** 15_200 → "15.2 Mb/s", 640 → "640 kb/s". */
fun bitrateText(kbps: Long): String =
    if (kbps >= 1000) String.format(Locale.ROOT, "%.1f Mb/s", kbps / 1000.0) else "$kbps kb/s"

/** `dolby_vision` (profile 8, base layer 1) → "Dolby Vision 8.1"; `hdr10_plus` → "HDR10+". */
fun hdrNames(t: ProbeVideoTrack): List<String> = t.hdr.map { h ->
    when (h) {
        "dolby_vision" -> "Dolby Vision" + when {
            // Compatibility id 0 means no compatible base layer (profile 5): no suffix.
            t.dvProfile != null && t.dvCompat != null && t.dvCompat != 0 -> " ${t.dvProfile}.${t.dvCompat}"
            t.dvProfile != null -> " profile ${t.dvProfile}"
            else -> ""
        }
        "hdr10" -> "HDR10"
        "hdr10_plus" -> "HDR10+"
        "hlg" -> "HLG"
        else -> h.uppercase(Locale.ROOT)
    }
}

/** "HEVC Main 10 · 3840×2160 · 23.976 fps · 10-bit · Dolby Vision 8.1, HDR10". */
fun videoTrackText(t: ProbeVideoTrack): String = listOfNotNull(
    codecName(t.codec) + (t.profile?.let { " $it" } ?: ""),
    when {
        t.width != null && t.height != null -> "${t.width}×${t.height}"
        t.height != null -> "${t.height}p"
        else -> null
    },
    t.fpsMilli?.let(::fpsText),
    t.bitDepth?.let { "$it-bit" },
    hdrNames(t).takeIf { it.isNotEmpty() }?.joinToString(", "),
).joinToString(" · ")

/** Audio track kinds (D123) in the order they are told, with their labels. */
val AUDIO_KIND_ORDER = listOf("original", "dub", "voiceover", "description", "dialogue", "commentary", "music", "karaoke", "alternate")

private val AUDIO_KIND_LABELS = mapOf(
    "original" to "original", "dub" to "dub", "voiceover" to "voice-over", "description" to "audio description",
    "dialogue" to "clear dialogue", "commentary" to "commentary", "music" to "music only", "karaoke" to "karaoke",
    "alternate" to "alternate mix",
)

/** Subtitle track kinds (D123) in the order they are told, with their labels. */
val SUB_KIND_ORDER = listOf("forced", "sdh", "cc", "signs", "commentary", "description", "honorifics", "bilingual", "easy_reader")

private val SUB_KIND_LABELS = mapOf(
    "forced" to "forced", "sdh" to "SDH", "cc" to "CC", "signs" to "signs & songs", "commentary" to "commentary",
    "description" to "descriptions", "honorifics" to "honorifics", "bilingual" to "bilingual", "easy_reader" to "easy reader",
)

/** Known kinds in [order], then unknown ones as they came, with `_` read as a space. */
private fun kindLabels(kinds: List<String>, order: List<String>, labels: Map<String, String>): List<String> {
    val set = kinds.toSet()
    return order.filter { it in set }.map { labels.getValue(it) } +
        kinds.filter { it !in labels }.distinct().map { it.replace('_', ' ') }
}

/** ["description", "original"] → ["original", "audio description"]. */
fun audioKindLabels(kinds: List<String>): List<String> = kindLabels(kinds, AUDIO_KIND_ORDER, AUDIO_KIND_LABELS)

/** ["sdh", "forced"] → ["forced", "SDH"]. */
fun subKindLabels(kinds: List<String>): List<String> = kindLabels(kinds, SUB_KIND_ORDER, SUB_KIND_LABELS)

/**
 * Subtitle format class of a codec (D125, the server's `probes::sub_format`): `text`, `image`,
 * `cc`, or null for a codec outside the table.
 */
fun subFormatClass(codec: String): String? = when (codec) {
    "subrip", "ass", "ssa", "webvtt", "mov_text", "ttml", "hdmv_text_subtitle", "dvb_teletext", "arib_caption",
    "kate", "usf",
    -> "text"
    "hdmv_pgs_subtitle", "dvd_subtitle", "dvb_subtitle", "xsub" -> "image"
    "eia_608", "eia_708" -> "cc"
    else -> null
}

/** "French · E-AC-3 5.1 · Dolby Atmos · audio description". The track name is never shown. */
fun audioTrackText(a: ProbeAudioTrack): String = (
    listOfNotNull(
        a.lang?.let(::languageName),
        codecName(a.codec) + (a.channels?.let { " " + channelsText(it) } ?: ""),
        if (a.atmos) "Dolby Atmos" else null,
        if (a.dtsX) "DTS:X" else null,
    ) + audioKindLabels(a.kinds)
    ).joinToString(" · ")

/** "English · SDH · PGS", "unknown · CC · CEA-608". The track name is never shown. */
fun subTrackText(s: ProbeSubTrack): String =
    (listOf(s.lang?.let(::languageName) ?: "unknown") + subKindLabels(s.kinds) + codecName(s.codec)).joinToString(" · ")

/** Most subtitle lines one file shows; the rest are counted on one more line. */
const val MAX_SUBTITLE_LINES = 10

/**
 * One line per distinct subtitle track (D126), identical ones grouped in first-seen order:
 * "Subtitle: French · SRT ×2". At most [MAX_SUBTITLE_LINES], then "Subtitles: N more tracks".
 */
fun subtitleLines(v: ProbeVideo): List<String> {
    val groups = LinkedHashMap<String, Int>()
    v.subs.forEach { groups.merge(subTrackText(it), 1, Int::plus) }
    val shown = groups.entries.take(MAX_SUBTITLE_LINES)
    val rest = v.subs.size - shown.sumOf { it.value }
    return shown.map { (text, n) -> "Subtitle: $text" + if (n > 1) " ×$n" else "" } +
        listOfNotNull(rest.takeIf { it > 0 }?.let { "Subtitles: $it more " + if (it == 1) "track" else "tracks" })
}

/** "Matroska · 2h 00m · 15.2 Mb/s". */
fun containerText(v: ProbeVideo): String = listOfNotNull(
    containerName(v.container).takeIf { v.container.isNotBlank() },
    v.durationMs?.let(::durationText),
    v.bitrateKbps?.let(::bitrateText),
).joinToString(" · ")

/** "PS5 package (PS5 FIH)", then title, ids, region and version. */
fun gameLines(g: ProbeGame): List<String> = buildList {
    add(
        listOfNotNull(platformName(g.platform), g.kind?.let(::gameKindName)).joinToString(" ") +
            (g.format?.let { " ($it)" } ?: ""),
    )
    g.title?.let { add("Title: $it") }
    listOfNotNull(
        g.titleId?.let { "Title ID $it" },
        g.contentId?.let { "Content ID $it" },
        g.dbId?.takeIf { it != g.titleId }?.let { "Database ID $it" },
    ).takeIf { it.isNotEmpty() }?.let { add(it.joinToString(" · ")) }
    listOfNotNull(g.region?.let { "Region $it" }, g.version?.let { "Version $it" })
        .takeIf { it.isNotEmpty() }?.let { add(it.joinToString(" · ")) }
}

/** Flags worth telling the reader, warnings first. */
fun flagNotes(flags: List<String>): Pair<List<String>, List<String>> {
    val warnings = buildList {
        if ("suspicious_executable" in flags) add("Suspicious executable")
        if ("extension_mismatch" in flags) add("The file name's extension does not match its contents")
    }
    val notes = buildList {
        if ("incomplete" in flags) add("read partly")
        if ("agreed" in flags) add("two peers agreed")
        if ("disagree" in flags) add("peers disagreed")
        if ("surprise_verified" in flags) add("surprising result, confirmed on hash-checked pieces")
        if ("surprise_changed" in flags) add("first guess corrected on hash-checked pieces")
        if ("surprise_unverified" in flags) add("surprising result, not confirmed on hash-checked pieces")
    }
    return warnings to notes
}

/** One probed file as the "Detected" section shows it. */
data class ProbeFileView(
    /** The shown file's name (by `index` into the detail's files), else the probe's path. */
    val name: String,
    /** "Matroska video · 97% · verified". */
    val headline: String,
    val lines: List<String>,
    val warnings: List<String>,
    val verified: Boolean,
)

fun probeFileView(f: ProbeFileInfo, shown: List<TorrentFile>): ProbeFileView {
    val path = shown.getOrNull(f.index)?.path ?: f.path
    val (warnings, notes) = flagNotes(f.flags)
    val headline = listOfNotNull(
        f.description.ifBlank { f.label },
        "${f.confidence}%".takeIf { f.confidence > 0 },
        if (f.verified) "verified" else "unverified",
    ).joinToString(" · ")
    val lines = buildList {
        f.video?.let { v ->
            containerText(v).takeIf { it.isNotEmpty() }?.let(::add)
            v.video.forEach { add("Video: " + videoTrackText(it)) }
            v.audio.forEach { add("Audio: " + audioTrackText(it)) }
            addAll(subtitleLines(v))
        }
        f.game?.let { addAll(gameLines(it)) }
        if (notes.isNotEmpty()) add(notes.joinToString(" · ").replaceFirstChar { it.uppercaseChar() })
    }
    return ProbeFileView(
        name = fileName(path).ifBlank { path },
        headline = headline + (f.size.takeIf { it > 0 }?.let { " · " + formatBytes(it) } ?: ""),
        lines = lines,
        warnings = warnings,
        verified = f.verified,
    )
}

/** "Complete · all hash-checked · probed 3d ago". */
fun probeSummary(p: ProbeInfo, nowSec: Long = System.currentTimeMillis() / 1000): String = listOfNotNull(
    when (p.status) {
        "ok" -> "Complete"
        "partial" -> "Partial"
        "" -> null
        else -> p.status.replaceFirstChar { it.uppercaseChar() }
    },
    if (p.verified) "all hash-checked" else "not all hash-checked",
    p.probedAt.takeIf { it > 0 }?.let { "probed " + formatAgo(it, nowSec) },
).joinToString(" · ")

/** What to show, or null to hide the section (never probed, an older server, nothing kept). */
fun probeViews(p: ProbeInfo?, shown: List<TorrentFile>): List<ProbeFileView>? =
    p?.files?.takeIf { it.isNotEmpty() }?.sortedBy { it.index }?.map { probeFileView(it, shown) }
