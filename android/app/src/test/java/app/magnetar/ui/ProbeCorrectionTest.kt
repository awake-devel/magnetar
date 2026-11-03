package app.magnetar.ui

import app.magnetar.api.MagnetarApi
import app.magnetar.api.ProbeCorrectionItem
import app.magnetar.api.ProbeCorrectionRequest
import app.magnetar.api.ProbeFileInfo
import app.magnetar.api.ProbeSubTrack
import app.magnetar.api.ProbeTrackDetected
import app.magnetar.api.ProbeTrackFix
import app.magnetar.api.ProbeUnplacedFix
import app.magnetar.api.ProbeUnplacedWas
import app.magnetar.api.ProbeVideo
import app.magnetar.api.TorrentDetail
import app.magnetar.ui.detail.LanguageChoice
import app.magnetar.ui.detail.TRACK_AUDIO
import app.magnetar.ui.detail.TRACK_SUB
import app.magnetar.ui.detail.checkAllItems
import app.magnetar.ui.detail.cleanTrackName
import app.magnetar.ui.detail.draftOf
import app.magnetar.ui.detail.editorTitle
import app.magnetar.ui.detail.itemOf
import app.magnetar.ui.detail.languageChoices
import app.magnetar.ui.detail.languageLabel
import app.magnetar.ui.detail.primaryAction
import app.magnetar.ui.detail.probeViews
import app.magnetar.ui.detail.trackRows
import app.magnetar.ui.detail.unplacedText
import org.testng.Assert.assertEquals
import org.testng.Assert.assertFalse
import org.testng.Assert.assertNull
import org.testng.Assert.assertTrue
import org.testng.annotations.Test

/** Track corrections in the "Detected" card (D128–D131). */
class ProbeCorrectionTest {
    /** The `probe` of docs/API.md / the D130 spec §4, verbatim. */
    private val specJson = """{"summary":{"infohash":"${"c".repeat(40)}","title":"Movie"},
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
}"""

    private val detail: TorrentDetail = MagnetarApi.JSON.decodeFromString(TorrentDetail.serializer(), specJson)
    private val file: ProbeFileInfo = detail.probe!!.files.single()

    @Test
    fun `corrections decode with fix, unplaced and dht index`() {
        val p = detail.probe!!
        assertEquals(file.dhtIndex, 0)
        val v = file.video!!
        assertEquals(v.audio.map { it.pos }, listOf(0, 1, 2))
        assertEquals(v.audio[0].fix, ProbeTrackFix(id = 17, at = 1793500100, by = "android"))
        assertEquals(
            v.audio[1].fix,
            ProbeTrackFix(
                id = 18, fields = listOf("lang", "name"),
                detected = ProbeTrackDetected(lang = "eng", kinds = listOf("commentary"), name = "Commentary"),
                at = 1793500200, by = "android",
            ),
        )
        assertTrue(v.audio[2].fix!!.removed)
        assertEquals(v.audio[2].fix!!.detected, ProbeTrackDetected(lang = "eng"))
        assertNull(v.subs[0].fix)
        assertEquals(v.subs[0].pos, 0)
        val added = v.subs[1]
        assertNull(added.pos)
        assertEquals(added.codec, "")
        assertEquals(added.name, "Español (forzados)")
        assertTrue(added.fix!!.added)
        assertTrue(added.fix.review)
        assertEquals(
            p.unplaced.single(),
            ProbeUnplacedFix(
                id = 9, file = 0, path = "Movie.mkv", track = "sub", pos = 3, reason = "changed",
                was = ProbeUnplacedWas(codec = "hdmv_pgs_subtitle", lang = "eng", kinds = listOf("sdh"), name = "English SDH"),
                fields = listOf("kinds"), kinds = listOf("cc"), at = 1793400000,
            ),
        )
        // dht_index is the file's stored DHT index, not its place in the shown list.
        val moved = MagnetarApi.JSON.decodeFromString(
            TorrentDetail.serializer(),
            """{"probe":{"files":[{"index":2,"dht_index":5,"path":"a.mkv"}]}}""",
        ).probe!!.files.single()
        assertEquals(moved.index to moved.dhtIndex, 2 to 5)
        // An older server's answer still parses: no pos, no fix, no unplaced, dht_index 0.
        val old = MagnetarApi.JSON.decodeFromString(
            TorrentDetail.serializer(),
            """{"probe":{"files":[{"index":1,"video":{"subs":[{"codec":"subrip","lang":"eng"}]}}]}}""",
        ).probe!!
        assertTrue(old.unplaced.isEmpty())
        assertEquals(old.files.single().dhtIndex, 0)
        assertNull(old.files.single().video!!.subs.single().pos)
        assertNull(old.files.single().video!!.subs.single().fix)
    }

    @Test
    fun `track rows number detected tracks as VLC does and mark corrections`() {
        val rows = trackRows(file)
        assertEquals(
            rows.map { it.text },
            listOf(
                "Track 1 · French · E-AC-3 5.1 · Dolby Atmos",
                "Track 2 · French · AC-3 2.0 · commentary",
                "Track 3 · English · AAC 2.0",
                "Track 1 · French · forced · SRT",
                "Added · Spanish · forced",
            ),
        )
        assertEquals(rows.map { it.marker }, listOf("✓ matches VLC", "corrected", "not in the file", null, "added by you"))
        assertEquals(
            rows.map { it.editLabel },
            listOf("Edit audio track 1", "Edit audio track 2", "Edit audio track 3", "Edit subtitle track 1", "Edit added subtitle track"),
        )
        assertEquals(rows.map { it.file }.distinct(), listOf(0))
        // Names are shown since D131 (VLC shows them).
        assertEquals(rows.map { it.name }, listOf(null, "Commentaire", null, null, "Español (forzados)"))
        // The corrected track tells what the probe said, with the same formatter.
        val corrected = rows[1]
        assertTrue(corrected.corrected)
        assertEquals(corrected.detectedText, "English · AC-3 2.0 · commentary")
        assertEquals(corrected.detectedName, "Commentary")
        assertFalse(rows[0].corrected)
        assertTrue(rows[0].checked)
        assertEquals(rows[0].detectedText, "French · E-AC-3 5.1 · Dolby Atmos")
        // Without a language: "unknown language"; codec last on subtitles.
        val captions = ProbeFileInfo(
            dhtIndex = 4,
            video = ProbeVideo(subs = listOf(ProbeSubTrack(codec = "subrip", lang = "eng"), ProbeSubTrack(codec = "eia_608", kinds = listOf("cc")))),
        )
        assertEquals(
            trackRows(captions).map { it.text to it.pos },
            listOf("Track 1 · English · SRT" to 0, "Track 2 · unknown language · CC · CEA-608" to 1),
        )
        assertEquals(trackRows(captions).map { it.file }.distinct(), listOf(4))
        assertTrue(trackRows(ProbeFileInfo()).isEmpty())
        // "All tracks match VLC" checks only the detected tracks without a correction.
        assertEquals(checkAllItems(file), listOf(ProbeCorrectionItem(file = 0, track = TRACK_SUB, pos = 0)))
        assertEquals(checkAllItems(captions).map { it.pos }, listOf(0, 1))
        // The card keeps the non-track lines apart from the tracks.
        val view = probeViews(detail.probe, emptyList())!!.single()
        assertEquals(view.videoLines, listOf("Matroska"))
        assertEquals(view.file, file)
    }

    @Test
    fun `removed added and review rows`() {
        val rows = trackRows(file)
        val removed = rows[2]
        assertTrue(removed.removed)
        assertFalse(removed.corrected)
        assertNull(removed.review)
        val added = rows[4]
        assertTrue(added.added)
        assertNull(added.pos)
        assertNull(added.detectedText)
        assertEquals(added.review, "⚠ The probe now lists a different number of tracks: check this added track")
        assertEquals(editorTitle(draftOf(added)), "Added subtitle track")
        assertEquals(editorTitle(draftOf(rows[1])), "Audio track 2")
        assertEquals(editorTitle(draftOf(rows[3])), "Subtitle track 1")
        assertEquals(editorTitle(draftOf(0)), "Add a track the probe missed")
        // An anchored correction whose detection changed since.
        val changed = ProbeFileInfo(
            video = ProbeVideo(
                subs = listOf(
                    ProbeSubTrack(
                        codec = "subrip", lang = "eng", pos = 0,
                        fix = ProbeTrackFix(id = 3, review = true, detected = ProbeTrackDetected(lang = "eng")),
                    ),
                ),
            ),
        )
        val r = trackRows(changed).single()
        assertEquals(r.review, "⚠ The probe changed since you checked this track")
        assertEquals(r.marker, "✓ matches VLC")
    }

    @Test
    fun `language choices offer one bibliographic code per language and search by name or code`() {
        val all = languageChoices("")
        assertEquals(all.first(), LanguageChoice(null, "Unknown / none"))
        assertEquals(all.first().label, "Unknown / none")
        val langs = all.drop(1)
        val terminology = setOf(
            "fra", "deu", "zho", "ces", "slk", "ron", "ell", "nld", "fas", "isl", "eus", "sqi", "mkd", "hye", "kat",
            "cym", "msa", "mya", "bod", "mri",
        )
        assertTrue(langs.none { it.code in terminology }, langs.toString())
        assertEquals(langs.map { it.name }.distinct().size, langs.size, "one code per name")
        assertEquals(langs.map { it.name }, langs.map { it.name }.sorted())
        assertTrue(LanguageChoice("fre", "French") in langs)
        assertTrue(LanguageChoice("mao", "Maori") in langs)
        assertEquals(LanguageChoice("fre", "French").label, "French (fre)")
        assertEquals(languageChoices("fREn"), listOf(LanguageChoice(null, "Unknown / none"), LanguageChoice("fre", "French")))
        assertTrue(LanguageChoice("ger", "German") in languageChoices("ger"))
        // A terminology code finds its bibliographic language.
        assertTrue(LanguageChoice("fre", "French") in languageChoices("fra"))
        assertTrue(LanguageChoice("mao", "Maori") in languageChoices("mri"))
        assertEquals(languageChoices("zzzz"), listOf(LanguageChoice(null, "Unknown / none")))
        assertEquals(languageLabel("fre"), "French (fre)")
        assertEquals(languageLabel(null), "Unknown / none")
    }

    @Test
    fun `a draft becomes the exact request item`() {
        val rows = trackRows(file)
        // A corrected track: the fields that differ from the detection, values from the draft.
        val commentary = draftOf(rows[1])
        assertEquals(
            itemOf(commentary),
            ProbeCorrectionItem(file = 0, track = TRACK_AUDIO, pos = 1, set = listOf("lang", "name"), lang = "fre", name = "Commentaire"),
        )
        assertEquals(
            itemOf(commentary.copy(kinds = setOf("dub", "commentary"))),
            ProbeCorrectionItem(
                file = 0, track = TRACK_AUDIO, pos = 1, set = listOf("lang", "kinds", "name"),
                lang = "fre", kinds = listOf("commentary", "dub"), name = "Commentaire",
            ),
        )
        // Back to the detected language: only the name stays overridden.
        assertEquals(
            itemOf(commentary.copy(lang = "eng")),
            ProbeCorrectionItem(file = 0, track = TRACK_AUDIO, pos = 1, set = listOf("name"), name = "Commentaire"),
        )
        // "No language" is a null in set: left out of the JSON, which the server reads as none.
        val noLang = itemOf(commentary.copy(lang = null, name = "Commentary"))!!
        assertEquals(noLang, ProbeCorrectionItem(file = 0, track = TRACK_AUDIO, pos = 1, set = listOf("lang")))
        assertEquals(
            MagnetarApi.JSON.encodeToString(ProbeCorrectionRequest.serializer(), ProbeCorrectionRequest(items = listOf(noLang))),
            """{"device":"android","items":[{"file":0,"track":"audio","pos":1,"set":["lang"],"kinds":[],"removed":false}]}""",
        )
        // "This track doesn't exist": removed only.
        assertEquals(
            itemOf(draftOf(rows[3]).copy(removed = true, lang = "eng")),
            ProbeCorrectionItem(file = 0, track = TRACK_SUB, pos = 0, removed = true),
        )
        // An added track is edited by id, with all three fields (the server keeps a field left
        // out of `set`).
        assertEquals(
            itemOf(draftOf(rows[4]).copy(lang = "cat")),
            ProbeCorrectionItem(
                id = 42, file = 0, track = TRACK_SUB, set = listOf("lang", "kinds", "name"),
                lang = "cat", kinds = listOf("forced"), name = "Español (forzados)",
            ),
        )
        // Clearing its name sends "name" in set without a value: none.
        val cleared = itemOf(draftOf(rows[4]).copy(name = ""))!!
        assertEquals(
            cleared,
            ProbeCorrectionItem(id = 42, file = 0, track = TRACK_SUB, set = listOf("lang", "kinds", "name"), lang = "spa", kinds = listOf("forced")),
        )
        assertEquals(
            MagnetarApi.JSON.encodeToString(ProbeCorrectionRequest.serializer(), ProbeCorrectionRequest(items = listOf(cleared))),
            """{"device":"android","items":[{"id":42,"file":0,"track":"sub","set":["lang","kinds","name"],"lang":"spa","kinds":["forced"],"removed":false}]}""",
        )
        // Clearing everything is no item: the editor offers "Undo correction" instead.
        assertNull(itemOf(draftOf(rows[4]).copy(lang = null, kinds = emptySet(), name = "")))
        // A new track: neither id nor pos, its non-empty fields (the name trimmed).
        val new = draftOf(3, TRACK_SUB).copy(lang = "spa", kinds = setOf("forced"), name = "  Español‏ (forzados) ")
        assertEquals(
            itemOf(new),
            ProbeCorrectionItem(file = 3, track = TRACK_SUB, set = listOf("lang", "kinds", "name"), lang = "spa", kinds = listOf("forced"), name = "Español (forzados)"),
        )
        assertEquals(
            itemOf(draftOf(3, TRACK_AUDIO).copy(kinds = setOf("commentary"))),
            ProbeCorrectionItem(file = 3, track = TRACK_AUDIO, set = listOf("kinds"), kinds = listOf("commentary")),
        )
        // Nothing to add: no item, and Save stays off.
        assertNull(itemOf(draftOf(3, TRACK_SUB).copy(name = "   ")))
        assertEquals(primaryAction(draftOf(3, TRACK_SUB)), "Save" to false)
        assertEquals(primaryAction(new), "Save" to true)
    }

    @Test
    fun `unchanged draft saves as matches VLC`() {
        val rows = trackRows(file)
        val plain = draftOf(rows[3])
        assertEquals(primaryAction(plain), "Matches VLC" to true)
        assertEquals(itemOf(plain), ProbeCorrectionItem(file = 0, track = TRACK_SUB, pos = 0, set = emptyList()))
        // Saving the detected value is a check too.
        val same = plain.copy(lang = "fre", kinds = setOf("forced"), name = "")
        assertEquals(itemOf(same)!!.set, emptyList<String>())
        // A track already checked or corrected and unchanged: nothing to save.
        assertEquals(primaryAction(draftOf(rows[0])), "Save" to false)
        assertEquals(primaryAction(draftOf(rows[1])), "Save" to false)
        assertEquals(primaryAction(draftOf(rows[2])), "Save" to false)
        // Changing it is a save; switching "doesn't exist" off on a removed track too.
        assertEquals(primaryAction(plain.copy(lang = "eng")), "Save" to true)
        assertEquals(primaryAction(draftOf(rows[2]).copy(removed = false)), "Save" to true)
        assertEquals(itemOf(draftOf(rows[2]).copy(removed = false)), ProbeCorrectionItem(file = 0, track = TRACK_AUDIO, pos = 2))
        // A name that only differs by bidi controls or spaces is not a change.
        assertEquals(primaryAction(draftOf(rows[1]).copy(name = "Commentaire‎ ")), "Save" to false)
        // A correction the probe changed under (review): "Still correct" sends it again (a
        // fresh snapshot clears the warning), for an added track and a detected one.
        assertEquals(primaryAction(draftOf(rows[4])), "Still correct" to true)
        assertEquals(itemOf(draftOf(rows[4]))?.id, 42L)
        val reviewed = trackRows(
            ProbeFileInfo(
                video = ProbeVideo(
                    subs = listOf(
                        ProbeSubTrack(
                            codec = "subrip", lang = "ger", pos = 0,
                            fix = ProbeTrackFix(id = 3, fields = listOf("lang"), review = true, detected = ProbeTrackDetected(lang = "ita")),
                        ),
                    ),
                ),
            ),
        ).single()
        assertEquals(primaryAction(draftOf(reviewed)), "Still correct" to true)
        assertEquals(itemOf(draftOf(reviewed)), ProbeCorrectionItem(file = 0, track = TRACK_SUB, pos = 0, set = listOf("lang"), lang = "ger"))
    }

    @Test
    fun `track names are cut to 64 bytes and lose bidi controls`() {
        assertEquals(cleanTrackName("‏English‪ SDH‬⁩‎⁦"), "English SDH")
        assertEquals(cleanTrackName("a\u0007b\nc"), "abc")
        assertEquals(cleanTrackName("a".repeat(70)), "a".repeat(64))
        assertEquals(cleanTrackName("é".repeat(40)), "é".repeat(32))
        assertEquals(cleanTrackName("a" + "€".repeat(30)), "a" + "€".repeat(21))
        // A 4-byte code point is never split.
        val film = "🎬"
        val cut = cleanTrackName("aaa" + film.repeat(20))
        assertEquals(cut, "aaa" + film.repeat(15))
        assertTrue(cut.toByteArray(Charsets.UTF_8).size <= 64)
        // Not trimmed while typing: a space being typed stays.
        assertEquals(cleanTrackName("English "), "English ")
    }

    @Test
    fun `unplaced corrections are described`() {
        val u = detail.probe!!.unplaced.single()
        assertEquals(unplacedText(u, "Movie.mkv"), "Movie.mkv: subtitle track 4 (was English · SDH · PGS): kinds CC")
        val audio = ProbeUnplacedFix(
            id = 1, track = "audio", pos = 0, reason = "changed",
            was = ProbeUnplacedWas(codec = "ac3", channels = 6),
            fields = listOf("lang", "kinds", "name"), lang = "fre", kinds = emptyList(), name = "VFF",
        )
        assertEquals(unplacedText(audio, "a.mkv"), "a.mkv: audio track 1 (was unknown language · AC-3 5.1): language French, kinds none, name “VFF”")
        assertEquals(
            unplacedText(ProbeUnplacedFix(track = "sub", pos = 1, reason = "changed", fields = listOf("lang")), "b.mkv"),
            "b.mkv: subtitle track 2: language none",
        )
        assertEquals(unplacedText(ProbeUnplacedFix(track = "sub", pos = 2, reason = "changed", removed = true), "b.mkv"), "b.mkv: subtitle track 3: not in the file")
        assertEquals(unplacedText(ProbeUnplacedFix(track = "audio", pos = 0, reason = "changed"), "b.mkv"), "b.mkv: audio track 1: checked")
        assertEquals(unplacedText(ProbeUnplacedFix(track = "audio", pos = 0, reason = "file"), "c.mkv"), "c.mkv: the file is no longer probed")
    }
}
