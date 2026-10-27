package app.magnetar.ui

import app.magnetar.api.MagnetarApi
import app.magnetar.api.ReleaseAttrs
import app.magnetar.ui.common.attrBadges
import app.magnetar.ui.common.facetLabel
import app.magnetar.ui.common.languageKey
import app.magnetar.ui.common.languageLabel
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.testng.Assert.assertEquals
import org.testng.annotations.Test

class LabelsTest {
    /** `release_attrs.jsonl`: `cargo run -p magnetar-core --example release_attrs` output (Rust parser). */
    private val rust = javaClass.classLoader!!.getResource("release_attrs.jsonl")!!.readText().lines().filter { it.isNotBlank() }
        .map { MagnetarApi.JSON.parseToJsonElement(it).jsonObject }

    private fun attrsOf(title: String): ReleaseAttrs = rust.first { it["title"]!!.jsonPrimitive.content == title }
        .let { MagnetarApi.JSON.decodeFromJsonElement(ReleaseAttrs.serializer(), it["attrs"]!!) }

    @Test
    fun `language key and label match the Rust parser on every sample`() {
        assertEquals(rust.size, 12)
        for (o in rust) {
            val a = MagnetarApi.JSON.decodeFromJsonElement(ReleaseAttrs.serializer(), o["attrs"]!!)
            val title = o["title"]!!.jsonPrimitive.content
            assertEquals(languageKey(a), o["language_key"]!!.jsonPrimitive.content, title)
            assertEquals(languageLabel(languageKey(a)), o["language_label"]!!.jsonPrimitive.content, title)
        }
    }

    @Test
    fun `row badges as the old app showed them`() {
        assertEquals(
            attrBadges(attrsOf("Dune.Part.Two.2024.2160p.UHD.BluRay.REMUX.DV.HDR10.HEVC.TrueHD.Atmos.7.1-GRP")),
            listOf("2160p", "Remux", "H.265/HEVC", "Dolby Vision", "TrueHD 7.1"),
        )
        assertEquals(
            attrBadges(attrsOf("White.Men.Cant.Jump.2023.TRUEFRENCH.720p.DSNP.WEB-DL.DDP5.1.H.264-FCK")),
            listOf("720p", "WEB-DL", "H.264/AVC", "VFF (France)", "DD+ (E-AC-3) 5.1"),
        )
        assertEquals(attrBadges(attrsOf("Artist - Album (2020) [FLAC 24bit 96kHz]")), listOf("FLAC", "24 bit"))
        assertEquals(attrBadges(attrsOf("Artist - Song [MP3 320kbps]")), listOf("MP3", "320 kbps"))
        assertEquals(attrBadges(attrsOf("Some Book 2024.fr.epub")), listOf("French", "EPUB"))
        assertEquals(attrBadges(attrsOf("Plain name without tags")), emptyList<String>())
    }

    @Test
    fun `facet labels`() {
        assertEquals(facetLabel("language", "fr-ca"), "French (Québec)")
        assertEquals(facetLabel("language", "multi"), "Multi-language")
        assertEquals(facetLabel("subtitles", "multi"), "Multi-subs")
        assertEquals(facetLabel("source", "web-dl"), "WEB-DL")
        assertEquals(facetLabel("resolution", "2160p"), "2160p (4K)")
        assertEquals(facetLabel("resolution", "1080p"), "1080p")
        assertEquals(facetLabel("genre", "science fiction"), "Science fiction")
        assertEquals(facetLabel("origin", "dht"), "DHT")
        assertEquals(facetLabel("category", "video"), "Video")
        assertEquals(facetLabel("language", "xx"), "XX")
    }
}
