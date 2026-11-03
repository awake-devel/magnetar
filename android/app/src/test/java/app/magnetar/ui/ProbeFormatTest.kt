package app.magnetar.ui

import app.magnetar.api.MagnetarApi
import app.magnetar.api.ProbeAudioTrack
import app.magnetar.api.ProbeGame
import app.magnetar.api.ProbeInfo
import app.magnetar.api.ProbeVideo
import app.magnetar.api.ProbeVideoTrack
import app.magnetar.api.ProbeSubTrack
import app.magnetar.api.TorrentDetail
import app.magnetar.ui.detail.audioTrackText
import app.magnetar.ui.detail.channelsText
import app.magnetar.ui.detail.codecName
import app.magnetar.ui.detail.containerText
import app.magnetar.ui.detail.durationText
import app.magnetar.ui.detail.flagNotes
import app.magnetar.ui.detail.fpsText
import app.magnetar.ui.detail.gameLines
import app.magnetar.ui.detail.hdrNames
import app.magnetar.ui.detail.languageName
import app.magnetar.ui.detail.probeSummary
import app.magnetar.ui.detail.probeViews
import app.magnetar.ui.detail.subFormatClass
import app.magnetar.ui.detail.subTrackText
import app.magnetar.ui.detail.subtitleLines
import app.magnetar.ui.detail.videoTrackText
import org.testng.Assert.assertEquals
import org.testng.Assert.assertNull
import org.testng.Assert.assertTrue
import org.testng.annotations.Test

class ProbeFormatTest {
    private fun detail(json: String) = MagnetarApi.JSON.decodeFromString(TorrentDetail.serializer(), json)

    /** docs/API.md "Header probes" example, a console file and fields this app does not know yet. */
    private val withProbe = """{"summary":{"infohash":"${"a".repeat(40)}","title":"Dune"},
        "files":[{"path":"Dune/Dune.2024.2160p.mkv","size":6000000000},{"path":"Dune/sample.mkv","size":5},
                 {"path":"Dune/game.pkg","size":900}],
        "probe":{"version":1,"probed_at":1793500000,"status":"partial","verified":false,"future":[1],
          "files":[
            {"index":2,"path":"Dune/game.pkg","size":900,"label":"game.ps5.pkg","description":"PS5 package",
             "confidence":90,"verified":false,"flags":["extension_mismatch","suspicious_executable","incomplete"],
             "game":{"platform":"ps5","kind":"package","format":"PS5 FIH","title_id":"PPSA01234",
                     "content_id":"EP0000-PPSA01234_00-GAME000000000000","title":"Some Game","region":"EU","version":"01.000"}},
            {"index":0,"path":"Movie.2024.2160p.mkv","size":6000000000,"label":"video.mkv",
             "description":"Matroska video","confidence":97,"verified":true,"flags":["agreed"],
             "video":{"container":"matroska","duration_ms":7200000,"bitrate_kbps":15200,"new_field":true,
                      "video":[{"codec":"hevc","profile":"Main 10","width":3840,"height":2160,"fps_milli":23976,
                                "bit_depth":10,"hdr":["dolby_vision","hdr10"],"dv_profile":8,"dv_compat":1}],
                      "audio":[{"codec":"eac3","channels":6,"lang":"fre","atmos":true},
                               {"codec":"dts","channels":8,"lang":"eng","dts_x":true},
                               {"codec":"aac","channels":2}],
                      "subs":[{"codec":"subrip","lang":"eng"},{"codec":"hdmv_pgs_subtitle","lang":"fre"},
                              {"codec":"subrip","lang":"eng"},{"codec":"ass"}]}}]}}"""

    @Test
    fun `probe decodes with unknown fields and older servers send none`() {
        val p = detail(withProbe).probe!!
        assertEquals(p.status, "partial")
        assertEquals(p.probedAt, 1793500000L)
        assertEquals(p.files.size, 2)
        val v = p.files[1].video!!
        assertEquals(v.video.single().dvProfile, 8)
        assertEquals(v.video.single().fpsMilli, 23976)
        assertEquals(v.audio[1].dtsX, true)
        assertEquals(v.audio[2].lang, null)
        assertEquals(p.files[0].game!!.contentId, "EP0000-PPSA01234_00-GAME000000000000")
        val old = detail("""{"summary":{"title":"Old"},"files":[]}""")
        assertNull(old.probe)
        assertNull(probeViews(old.probe, old.files), "no section without a probe")
        assertNull(probeViews(ProbeInfo(status = "ok"), old.files), "no section without a probed file")
    }

    @Test
    fun `files are mapped by index into the shown list, in list order`() {
        val d = detail(withProbe)
        val views = probeViews(d.probe, d.files)!!
        assertEquals(views.map { it.name }, listOf("Dune.2024.2160p.mkv", "game.pkg"), "shown path, not the probe's")
        val movie = views[0]
        assertEquals(movie.headline, "Matroska video · 97% · verified · 5.59 GiB")
        assertTrue(movie.verified)
        assertEquals(
            movie.lines,
            listOf(
                "Matroska · 2h 00m · 15.2 Mb/s",
                "Video: HEVC Main 10 · 3840×2160 · 23.976 fps · 10-bit · Dolby Vision 8.1, HDR10",
                "Audio: French · E-AC-3 5.1 · Dolby Atmos",
                "Audio: English · DTS 7.1 · DTS:X",
                "Audio: AAC 2.0",
                "Subtitle: English · SRT ×2",
                "Subtitle: French · PGS",
                "Subtitle: unknown · ASS",
                "Two peers agreed",
            ),
        )
        assertEquals(movie.warnings, emptyList<String>())
        val game = views[1]
        assertEquals(game.headline, "PS5 package · 90% · unverified · 900 B")
        assertEquals(
            game.warnings,
            listOf("Suspicious executable", "The file name's extension does not match its contents"),
        )
        assertEquals(
            game.lines,
            listOf(
                "PS5 package (PS5 FIH)",
                "Title: Some Game",
                "Title ID PPSA01234 · Content ID EP0000-PPSA01234_00-GAME000000000000",
                "Region EU · Version 01.000",
                "Read partly",
            ),
        )
        val outside = probeViews(d.probe, emptyList())!!
        assertEquals(outside[0].name, "Movie.2024.2160p.mkv", "falls back to the probe's own path")
    }

    @Test
    fun `summary says status, verification and age`() {
        val now = 1793500000L + 3 * 86_400
        assertEquals(
            probeSummary(ProbeInfo(probedAt = 1793500000, status = "ok", verified = true), now),
            "Complete · all hash-checked · probed 3d ago",
        )
        assertEquals(
            probeSummary(ProbeInfo(probedAt = 0, status = "partial", verified = false), now),
            "Partial · not all hash-checked",
        )
    }

    @Test
    fun `languages, codecs and numbers read like people say them`() {
        assertEquals(languageName("fre"), "French")
        assertEquals(languageName("ger"), "German")
        assertEquals(languageName("deu"), "German")
        assertEquals(languageName("JPN"), "Japanese")
        assertEquals(languageName("tlh"), "tlh", "unknown codes stay as they came")
        assertEquals(codecName("truehd"), "TrueHD")
        assertEquals(codecName("newcodec"), "NEWCODEC")
        assertEquals(channelsText(1), "mono")
        assertEquals(channelsText(6), "5.1")
        assertEquals(channelsText(12), "12 ch")
        assertEquals(fpsText(25000), "25 fps")
        assertEquals(fpsText(29970), "29.97 fps")
        assertEquals(fpsText(23976), "23.976 fps")
        assertEquals(durationText(2_700_000), "45m")
        assertEquals(durationText(5_430_000), "1h 30m")
        assertEquals(durationText(42_000), "42s")
    }

    @Test
    fun `hdr names with and without a Dolby Vision profile`() {
        assertEquals(hdrNames(ProbeVideoTrack(hdr = listOf("dolby_vision"))), listOf("Dolby Vision"))
        assertEquals(hdrNames(ProbeVideoTrack(hdr = listOf("dolby_vision"), dvProfile = 5)), listOf("Dolby Vision profile 5"))
        // A real profile 5 record carries compatibility id 0 (no compatible base layer).
        assertEquals(
            hdrNames(ProbeVideoTrack(hdr = listOf("dolby_vision"), dvProfile = 5, dvCompat = 0)),
            listOf("Dolby Vision profile 5"),
        )
        assertEquals(
            hdrNames(ProbeVideoTrack(hdr = listOf("dolby_vision", "hdr10_plus", "hlg"), dvProfile = 8, dvCompat = 4)),
            listOf("Dolby Vision 8.4", "HDR10+", "HLG"),
        )
        assertEquals(videoTrackText(ProbeVideoTrack(codec = "h264", height = 720)), "H.264 · 720p")
        assertEquals(videoTrackText(ProbeVideoTrack(codec = "av1")), "AV1")
    }

    @Test
    fun `sparse tracks leave missing parts out`() {
        assertEquals(audioTrackText(ProbeAudioTrack(codec = "opus")), "Opus")
        assertEquals(containerText(ProbeVideo(container = "avi")), "AVI")
        assertEquals(subtitleLines(ProbeVideo()), emptyList<String>())
        assertEquals(subtitleLines(ProbeVideo(subs = listOf(ProbeSubTrack(codec = "subrip", lang = "ita")))), listOf("Subtitle: Italian · SRT"))
        assertEquals(gameLines(ProbeGame(platform = "wii_or_game_cube")), listOf("Wii or GameCube"))
        assertEquals(
            gameLines(ProbeGame(platform = "switch", kind = "compressed_image", titleId = "0100000000010000", dbId = "0100000000010000")),
            listOf("Nintendo Switch compressed image", "Title ID 0100000000010000"),
        )
    }

    @Test
    fun `every surprise note is shown`() {
        val (warnings, notes) = flagNotes(listOf("surprise_verified", "surprise_changed", "surprise_unverified"))
        assertTrue(warnings.isEmpty())
        assertEquals(
            notes,
            listOf(
                "surprising result, confirmed on hash-checked pieces",
                "first guess corrected on hash-checked pieces",
                "surprising result, not confirmed on hash-checked pieces",
            ),
        )
    }

    /** docs/PROBES.md §6 / the D123 spec's §3 example: a v2 probe with kinds and names. */
    private val withKinds = """{"summary":{"title":"Kinds"},"files":[{"path":"Movie.mkv","size":1}],
        "probe":{"version":2,"status":"ok","verified":true,"files":[{"index":0,"path":"Movie.mkv","size":1,
          "description":"Matroska video","confidence":97,"verified":true,
          "video":{"container":"matroska","duration_ms":7200000,
           "video":[{"codec":"h264","width":1920,"height":1080}],
           "audio":[{"codec":"eac3","channels":6,"lang":"fre"},
                    {"codec":"ac3","channels":2,"lang":"eng","kinds":["commentary"],"name":"Director's Commentary","future":1},
                    {"codec":"aac","channels":2,"lang":"fre","kinds":["description"],"name":"Audiodescription"}],
           "subs":[{"codec":"hdmv_pgs_subtitle","lang":"eng","kinds":["sdh"],"name":"English (SDH)"},
                   {"codec":"subrip","lang":"fre","kinds":["forced"],"name":"Français (forcés)","role":"x"},
                   {"codec":"subrip","lang":"fre"},
                   {"codec":"eia_608","kinds":["cc"]},
                   {"codec":"ass","lang":"jpn","kinds":["signs"],"name":"Signs & Songs"}]}}]}}"""

    @Test
    fun `kinds and names decode, unknown fields ignored`() {
        val d = detail(withKinds)
        val v = d.probe!!.files.single().video!!
        assertEquals(v.audio[0].kinds, emptyList<String>())
        assertNull(v.audio[0].name)
        assertEquals(v.audio[1].kinds, listOf("commentary"))
        assertEquals(v.audio[1].name, "Director's Commentary")
        assertEquals(v.subs.map { it.kinds }, listOf(listOf("sdh"), listOf("forced"), emptyList(), listOf("cc"), listOf("signs")))
        assertEquals(v.subs[1].name, "Français (forcés)")
        assertNull(v.subs[3].lang)
        val lines = probeViews(d.probe, d.files)!!.single().lines
        assertEquals(
            lines,
            listOf(
                "Matroska · 2h 00m",
                "Video: H.264 · 1920×1080",
                "Audio: French · E-AC-3 5.1",
                "Audio: English · AC-3 2.0 · commentary",
                "Audio: French · AAC 2.0 · audio description",
                "Subtitle: English · SDH · PGS",
                "Subtitle: French · forced · SRT",
                "Subtitle: French · SRT",
                "Subtitle: unknown · CC · CEA-608",
                "Subtitle: Japanese · signs & songs · ASS",
            ),
        )
        // Names are free text from peers: never shown (D123).
        assertTrue(lines.none { "Director" in it || "Audiodescription" in it || "forcés" in it }, lines.toString())
    }

    @Test
    fun `audio kinds are listed after the format`() {
        assertEquals(
            audioTrackText(ProbeAudioTrack(codec = "eac3", channels = 6, lang = "fre", atmos = true, kinds = listOf("description"))),
            "French · E-AC-3 5.1 · Dolby Atmos · audio description",
        )
        assertEquals(
            audioTrackText(ProbeAudioTrack(codec = "ac3", channels = 2, lang = "eng", kinds = listOf("commentary"))),
            "English · AC-3 2.0 · commentary",
        )
        // Known kinds in their fixed order, an unknown one last and readable.
        assertEquals(
            audioTrackText(ProbeAudioTrack(codec = "aac", kinds = listOf("future_kind", "music", "original", "voiceover", "dialogue", "alternate", "karaoke", "dub"))),
            "AAC · original · dub · voice-over · clear dialogue · music only · karaoke · alternate mix · future kind",
        )
        assertEquals(
            audioTrackText(ProbeAudioTrack(codec = "aac", name = "Director's Commentary")),
            "AAC",
            "the name is never shown",
        )
    }

    @Test
    fun `subtitle lines group identical tracks and cap at ten`() {
        val v = ProbeVideo(
            subs = listOf(
                ProbeSubTrack("hdmv_pgs_subtitle", "eng", listOf("sdh")),
                ProbeSubTrack("subrip", "fre", listOf("forced")),
                ProbeSubTrack("subrip", "fre"),
                ProbeSubTrack("eia_608", null, listOf("cc")),
                ProbeSubTrack("subrip", "fre", name = "another name, same line"),
                ProbeSubTrack("ass", "jpn", listOf("signs")),
            ),
        )
        assertEquals(
            subtitleLines(v),
            listOf(
                "Subtitle: English · SDH · PGS",
                "Subtitle: French · forced · SRT",
                "Subtitle: French · SRT ×2",
                "Subtitle: unknown · CC · CEA-608",
                "Subtitle: Japanese · signs & songs · ASS",
            ),
        )
        assertEquals(
            subTrackText(ProbeSubTrack("subrip", "eng", listOf("easy_reader", "bilingual", "honorifics", "description", "commentary", "signs", "cc", "sdh", "forced", "new_one"))),
            "English · forced · SDH · CC · signs & songs · commentary · descriptions · honorifics · bilingual · easy reader · new one · SRT",
        )
        val codes = listOf("eng", "fre", "ger", "spa", "ita", "por", "rus", "jpn", "chi", "kor", "ara", "dut", "swe", "nor", "dan")
        val many = ProbeVideo(subs = codes.map { ProbeSubTrack("subrip", it) } + ProbeSubTrack("subrip", "dan"))
        val lines = subtitleLines(many)
        assertEquals(lines.size, 11)
        assertEquals(lines.first(), "Subtitle: English · SRT")
        assertEquals(lines[9], "Subtitle: Korean · SRT")
        assertEquals(lines.last(), "Subtitles: 6 more tracks", "five languages plus the second Danish track")
        val fifteen = subtitleLines(ProbeVideo(subs = codes.map { ProbeSubTrack("subrip", it) }))
        assertEquals(fifteen.size, 11)
        assertEquals(fifteen.last(), "Subtitles: 5 more tracks")
        val eleven = subtitleLines(ProbeVideo(subs = codes.take(11).map { ProbeSubTrack("subrip", it) }))
        assertEquals(eleven.last(), "Subtitles: 1 more track")
        // A group shown in full counts nothing as "more".
        val grouped = subtitleLines(ProbeVideo(subs = List(30) { ProbeSubTrack("subrip", "eng") }))
        assertEquals(grouped, listOf("Subtitle: English · SRT ×30"))
    }

    @Test
    fun `new subtitle codec names`() {
        assertEquals(
            listOf("eia_708", "ttml", "arib_caption", "dvb_teletext", "hdmv_text_subtitle", "xsub", "kate", "usf").map(::codecName),
            listOf("CEA-708", "TTML", "ARIB", "Teletext", "TextST", "XSUB", "Kate", "USF"),
        )
        // The server's `probes::sub_format` table (D125), kept the same here.
        for (c in listOf("subrip", "ass", "ssa", "webvtt", "mov_text", "ttml", "hdmv_text_subtitle", "dvb_teletext", "arib_caption", "kate", "usf")) {
            assertEquals(subFormatClass(c), "text", c)
        }
        for (c in listOf("hdmv_pgs_subtitle", "dvd_subtitle", "dvb_subtitle", "xsub")) assertEquals(subFormatClass(c), "image", c)
        for (c in listOf("eia_608", "eia_708")) assertEquals(subFormatClass(c), "cc", c)
        assertNull(subFormatClass("unknown_codec"))
    }
}
