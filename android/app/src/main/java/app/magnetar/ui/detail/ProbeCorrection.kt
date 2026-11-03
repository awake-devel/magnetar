package app.magnetar.ui.detail

import app.magnetar.api.ProbeCorrectionItem
import app.magnetar.api.ProbeFileInfo
import app.magnetar.api.ProbeTrackFix
import app.magnetar.api.ProbeUnplacedFix
import app.magnetar.api.ProbeUnplacedWas
import java.util.Locale

/*
 * Track corrections in the "Detected" card (D131, docs/APP.md §12 "Detail"): the user checks
 * each audio and subtitle track against VLC's Audio Track / Sub Track menus and corrects the
 * language, the kinds or the name, says a track does not exist, or adds one the probe missed.
 * The server keeps the corrections apart from the detections (D128) and answers the effective
 * tracks (D129). Pure functions, so the wording and the request items are unit-tested.
 */

const val TRACK_AUDIO = "audio"
const val TRACK_SUB = "sub"

/** Field names of `ProbeCorrectionItem.set`, in the order the server lists them. */
const val FIELD_LANG = "lang"
const val FIELD_KINDS = "kinds"
const val FIELD_NAME = "name"

/** Most bytes of a track name (the server's limit, D130). */
const val MAX_TRACK_NAME_BYTES = 64

/** Subtitle rows a file shows before "Show all N subtitle tracks". */
const val MAX_SUBTITLE_ROWS = 10

/** The values a correction can change. */
data class TrackValues(val lang: String? = null, val kinds: List<String> = emptyList(), val name: String? = null)

/** One audio or subtitle track of a probed file, as the editable card shows it. */
data class TrackRow(
    /** `ProbeFileInfo.dhtIndex`: what a correction names as `file`. */
    val file: Int,
    /** [TRACK_AUDIO] or [TRACK_SUB]. */
    val track: String,
    /** 0-based detected position; null on an added track. */
    val pos: Int?,
    /** "Track 2 · French · AC-3 2.0 · commentary", "Added · Spanish · forced". */
    val text: String,
    /** The effective name, cleaned; null without one. */
    val name: String?,
    val fix: ProbeTrackFix?,
    /** The current detection as the same formatter tells it; null on an added track. */
    val detectedText: String?,
    /** The detected name (cleaned), for the editor's "Name in the file". */
    val detectedName: String?,
    /** What the track is now (corrections applied). */
    val effective: TrackValues,
    /** What the probe says now. */
    val detected: TrackValues,
    /** Error-coloured warning when the probe changed since the correction. */
    val review: String?,
    /** Content description of the row's edit button. */
    val editLabel: String,
) {
    val added: Boolean get() = fix?.added == true
    val removed: Boolean get() = fix?.removed == true
    val corrected: Boolean get() = fix != null && !added && !removed && fix.fields.isNotEmpty()
    val checked: Boolean get() = fix != null && !added && !removed && fix.fields.isEmpty()

    /** "corrected", "✓ matches VLC", "not in the file", "added by you", or null. */
    val marker: String?
        get() = when {
            added -> "added by you"
            removed -> "not in the file"
            corrected -> "corrected"
            checked -> "✓ matches VLC"
            else -> null
        }
}

private fun typeWord(track: String): String = if (track == TRACK_AUDIO) "audio" else "subtitle"

/** "French · AC-3 2.0 · Dolby Atmos · commentary": a track's description, without its number. */
private fun audioBody(lang: String?, codec: String, channels: Int?, atmos: Boolean, dtsX: Boolean, kinds: List<String>): String =
    (
        listOfNotNull(
            lang?.let(::languageName) ?: "unknown language",
            codec.takeIf { it.isNotBlank() }?.let { codecName(it) + (channels?.let { c -> " " + channelsText(c) } ?: "") },
            if (atmos) "Dolby Atmos" else null,
            if (dtsX) "DTS:X" else null,
        ) + audioKindLabels(kinds)
        ).joinToString(" · ")

/** "English · SDH · PGS": codec last, as [subTrackText] tells it. */
private fun subBody(lang: String?, codec: String, kinds: List<String>): String =
    (
        listOf(lang?.let(::languageName) ?: "unknown language") + subKindLabels(kinds) +
            listOfNotNull(codec.takeIf { it.isNotBlank() }?.let(::codecName))
        ).joinToString(" · ")

private fun reviewText(fix: ProbeTrackFix?): String? = when {
    fix == null || !fix.review -> null
    fix.added -> "⚠ The probe now lists a different number of tracks: check this added track"
    else -> "⚠ The probe changed since you checked this track"
}

private fun editLabel(track: String, pos: Int?): String =
    if (pos == null) "Edit added ${typeWord(track)} track" else "Edit ${typeWord(track)} track ${pos + 1}"

private fun rowPrefix(pos: Int?): String = if (pos == null) "Added" else "Track ${pos + 1}"

private fun cleanName(name: String?): String? = name?.let { cleanTrackName(it).trim() }?.takeIf { it.isNotEmpty() }

/**
 * The file's audio rows, then its subtitle rows, in stored order: "Track {pos+1}" is VLC's
 * numbering (same container order, docs/PROBES.md §12), added tracks last of their type.
 */
fun trackRows(f: ProbeFileInfo): List<TrackRow> {
    val v = f.video ?: return emptyList()
    val audio = v.audio.mapIndexed { i, a ->
        val added = a.fix?.added == true
        val pos = if (added) null else (a.pos ?: i)
        val eff = TrackValues(a.lang, a.kinds, a.name)
        val det = a.fix?.detected?.let { TrackValues(it.lang, it.kinds, it.name) } ?: eff
        TrackRow(
            file = f.dhtIndex, track = TRACK_AUDIO, pos = pos,
            text = rowPrefix(pos) + " · " + audioBody(a.lang, a.codec, a.channels, a.atmos, a.dtsX, a.kinds),
            name = cleanName(a.name), fix = a.fix,
            detectedText = if (added) null else audioBody(det.lang, a.codec, a.channels, a.atmos, a.dtsX, det.kinds),
            detectedName = if (added) null else cleanName(det.name),
            effective = eff, detected = det, review = reviewText(a.fix), editLabel = editLabel(TRACK_AUDIO, pos),
        )
    }
    val subs = v.subs.mapIndexed { i, s ->
        val added = s.fix?.added == true
        val pos = if (added) null else (s.pos ?: i)
        val eff = TrackValues(s.lang, s.kinds, s.name)
        val det = s.fix?.detected?.let { TrackValues(it.lang, it.kinds, it.name) } ?: eff
        TrackRow(
            file = f.dhtIndex, track = TRACK_SUB, pos = pos,
            text = rowPrefix(pos) + " · " + subBody(s.lang, s.codec, s.kinds),
            name = cleanName(s.name), fix = s.fix,
            detectedText = if (added) null else subBody(det.lang, s.codec, det.kinds),
            detectedName = if (added) null else cleanName(det.name),
            effective = eff, detected = det, review = reviewText(s.fix), editLabel = editLabel(TRACK_SUB, pos),
        )
    }
    return audio + subs
}

/** What the editor holds. [name] is the text field's value (cleaned, not yet trimmed). */
data class TrackDraft(
    val file: Int,
    val track: String,
    /** Detected track: its position. */
    val pos: Int? = null,
    /** Added track: its correction id (null while adding a new one). */
    val id: Long? = null,
    val lang: String? = null,
    val kinds: Set<String> = emptySet(),
    val name: String = "",
    val removed: Boolean = false,
    /** Detected track: the current detection, against which the changed fields are told. */
    val detected: TrackValues? = null,
    /** What the track was when the editor opened (null while adding). */
    val start: TrackValues? = null,
    val startRemoved: Boolean = false,
    /** The correction on this track, for "Undo correction". */
    val fixId: Long? = null,
    /** The probe changed since the correction: saving it unchanged confirms it ("Still correct"). */
    val review: Boolean = false,
)

/** The editor for [row]: its effective values. */
fun draftOf(row: TrackRow): TrackDraft = TrackDraft(
    file = row.file,
    track = row.track,
    pos = row.pos,
    id = if (row.added) row.fix?.id else null,
    lang = row.effective.lang,
    kinds = row.effective.kinds.toSet(),
    name = cleanName(row.effective.name).orEmpty(),
    removed = row.removed,
    detected = if (row.added) null else row.detected,
    start = row.effective,
    startRemoved = row.removed,
    fixId = row.fix?.id,
    review = row.fix?.review == true,
)

/** The editor for a track the probe missed, in [file]. */
fun draftOf(file: Int, track: String = TRACK_SUB): TrackDraft = TrackDraft(file = file, track = track)

private fun sameName(a: String?, b: String?): Boolean = cleanName(a).orEmpty() == cleanName(b).orEmpty()

/** The draft differs from the track as it was when the editor opened. */
fun TrackDraft.changed(): Boolean {
    val s = start ?: return lang != null || kinds.isNotEmpty() || cleanName(name) != null
    return removed != startRemoved ||
        (!removed && (lang != s.lang || kinds != s.kinds.toSet() || !sameName(name, s.name)))
}

/**
 * The request item for [d] (D130), or null when there is nothing to send (an added track
 * without any value). A detected track sends `pos` and, in `set`, the fields that differ from
 * the detection (none: "matches VLC"); a removed one sends `removed` only (the server replaces
 * the row, so other overrides go too). A new added track sends neither id nor pos and its
 * non-empty fields. An edit of an added track sends its id and all three fields: the server
 * keeps a field left out of `set`, so a cleared language, kinds or name must be in it (an
 * absent value is none).
 */
fun itemOf(d: TrackDraft): ProbeCorrectionItem? {
    val name = cleanName(d.name)
    val kinds = d.kinds.sorted()
    if (d.pos != null) {
        if (d.removed) return ProbeCorrectionItem(file = d.file, track = d.track, pos = d.pos, removed = true)
        val det = d.detected ?: TrackValues()
        val set = buildList {
            if (d.lang != det.lang) add(FIELD_LANG)
            if (d.kinds != det.kinds.toSet()) add(FIELD_KINDS)
            if (!sameName(name, det.name)) add(FIELD_NAME)
        }
        return ProbeCorrectionItem(
            file = d.file, track = d.track, pos = d.pos, set = set,
            lang = d.lang.takeIf { FIELD_LANG in set },
            kinds = if (FIELD_KINDS in set) kinds else emptyList(),
            name = name.takeIf { FIELD_NAME in set },
        )
    }
    val filled = buildList {
        if (d.lang != null) add(FIELD_LANG)
        if (kinds.isNotEmpty()) add(FIELD_KINDS)
        if (name != null) add(FIELD_NAME)
    }
    if (filled.isEmpty()) return null
    val set = if (d.id != null) listOf(FIELD_LANG, FIELD_KINDS, FIELD_NAME) else filled
    return ProbeCorrectionItem(id = d.id, file = d.file, track = d.track, set = set, lang = d.lang, kinds = kinds, name = name)
}

/**
 * The editor's primary button: "Save"; "Matches VLC" on an unchanged track without a
 * correction; "Still correct" on an unchanged correction the probe changed under (review): it
 * sends the correction again with a fresh snapshot, which clears the warning.
 */
fun primaryAction(d: TrackDraft): Pair<String, Boolean> = when {
    d.start == null -> "Save" to (itemOf(d) != null)
    d.changed() -> "Save" to (itemOf(d) != null)
    d.fixId == null && d.pos != null -> "Matches VLC" to true
    d.review -> "Still correct" to (itemOf(d) != null)
    else -> "Save" to false
}

/** "Audio track 2", "Added subtitle track", "Add a track the probe missed". */
fun editorTitle(d: TrackDraft): String = when {
    d.pos != null -> (if (d.track == TRACK_AUDIO) "Audio" else "Subtitle") + " track ${d.pos + 1}"
    d.id != null -> "Added ${typeWord(d.track)} track"
    else -> "Add a track the probe missed"
}

/** "All tracks match VLC": a check for every detected track of [f] without a correction. */
fun checkAllItems(f: ProbeFileInfo): List<ProbeCorrectionItem> =
    trackRows(f).filter { it.fix == null && it.pos != null }
        .map { ProbeCorrectionItem(file = it.file, track = it.track, pos = it.pos, set = emptyList()) }

/** One entry of the language picker; a null [code] is "Unknown / none". */
data class LanguageChoice(val code: String?, val name: String) {
    val label: String get() = code?.let { "$name ($it)" } ?: "Unknown / none"
}

/** ISO 639-2/T codes (and `mri`, whose /B is `mao`): the picker offers the /B one only. */
private val TERMINOLOGY_CODES = setOf(
    "fra", "deu", "zho", "ces", "slk", "ron", "ell", "nld", "fas", "isl", "eus", "sqi", "mkd", "hye", "kat",
    "cym", "msa", "mya", "bod", "mri",
)

private val NO_LANGUAGE = LanguageChoice(null, "Unknown / none")

private val LANGUAGES: List<LanguageChoice> by lazy {
    ISO639_2.filterKeys { it !in TERMINOLOGY_CODES }.map { (c, n) -> LanguageChoice(c, n) }.sortedBy { it.name }
}

/**
 * "Unknown / none", then the languages by English name, one bibliographic code each. A
 * [query] keeps those whose name contains it (any case) or whose code is it (a /T code finds
 * its /B language).
 */
fun languageChoices(query: String = ""): List<LanguageChoice> {
    val q = query.trim().lowercase(Locale.ROOT)
    if (q.isEmpty()) return listOf(NO_LANGUAGE) + LANGUAGES
    val byCode = ISO639_2[q]
    return listOf(NO_LANGUAGE) + LANGUAGES.filter { it.name.lowercase(Locale.ROOT).contains(q) || it.code == q || it.name == byCode }
}

/** The language field: "French (fre)", "Unknown / none". */
fun languageLabel(code: String?): String = code?.let { "${languageName(it)} ($it)" } ?: "Unknown / none"

private fun isBidiControl(cp: Int): Boolean =
    cp == 0x200E || cp == 0x200F || cp in 0x202A..0x202E || cp in 0x2066..0x2069

/**
 * A track name as the server takes it: without bidi controls (U+200E, U+200F, U+202A–U+202E,
 * U+2066–U+2069) or control characters, at most 64 UTF-8 bytes cut on a code-point boundary.
 * Not trimmed (the text field keeps a space being typed); items are trimmed.
 */
fun cleanTrackName(s: String): String {
    val out = StringBuilder()
    var bytes = 0
    var i = 0
    while (i < s.length) {
        val cp = s.codePointAt(i)
        i += Character.charCount(cp)
        if (isBidiControl(cp) || Character.isISOControl(cp)) continue
        val n = when {
            cp < 0x80 -> 1
            cp < 0x800 -> 2
            cp < 0x10000 -> 3
            else -> 4
        }
        if (bytes + n > MAX_TRACK_NAME_BYTES) break
        bytes += n
        out.appendCodePoint(cp)
    }
    return out.toString()
}

private fun wasText(track: String, w: ProbeUnplacedWas): String =
    if (track == TRACK_AUDIO) audioBody(w.lang, w.codec, w.channels, atmos = false, dtsX = false, kinds = w.kinds)
    else subBody(w.lang, w.codec, w.kinds)

/**
 * "Movie.mkv: subtitle track 4 (was English · SDH · PGS): kinds CC", or "Movie.mkv: the file
 * is no longer probed".
 */
fun unplacedText(u: ProbeUnplacedFix, fileName: String): String {
    if (u.reason == "file") return "$fileName: the file is no longer probed"
    val which = u.pos?.let { "${typeWord(u.track)} track ${it + 1}" } ?: "added ${typeWord(u.track)} track"
    val was = u.was?.let { " (was ${wasText(u.track, it)})" }.orEmpty()
    val labels = if (u.track == TRACK_AUDIO) audioKindLabels(u.kinds) else subKindLabels(u.kinds)
    val change = buildList {
        if (u.removed) add("not in the file")
        if (FIELD_LANG in u.fields) add("language " + (u.lang?.let(::languageName) ?: "none"))
        if (FIELD_KINDS in u.fields) add("kinds " + labels.ifEmpty { listOf("none") }.joinToString(" + "))
        if (FIELD_NAME in u.fields) add(cleanName(u.name)?.let { "name “$it”" } ?: "no name")
    }.ifEmpty { listOf("checked") }
    return "$fileName: $which$was: ${change.joinToString(", ")}"
}
