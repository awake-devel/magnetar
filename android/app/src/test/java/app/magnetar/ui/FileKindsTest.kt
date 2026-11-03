package app.magnetar.ui

import app.magnetar.api.FacetCount
import app.magnetar.api.FacetCounts
import app.magnetar.api.FileKindCatalogue
import app.magnetar.api.FileKindGroup
import app.magnetar.api.FileKindLabel
import app.magnetar.ui.search.ActiveFilter
import app.magnetar.ui.search.ChipOrder
import app.magnetar.ui.search.GroupState
import app.magnetar.ui.search.SearchFilters
import app.magnetar.ui.search.activeFilters
import app.magnetar.ui.search.arrangeFileKinds
import app.magnetar.ui.search.dropdownSummary
import app.magnetar.ui.search.fileKindChecked
import app.magnetar.ui.search.fileKindChips
import app.magnetar.ui.search.fileKindGroups
import app.magnetar.ui.search.fileKindLabelsOf
import app.magnetar.ui.search.fileKindName
import app.magnetar.ui.search.fileKindOrder
import app.magnetar.ui.search.fileKindState
import app.magnetar.ui.search.fileKindSummary
import app.magnetar.ui.search.filterFileKindGroups
import app.magnetar.ui.search.normalizeFileKinds
import app.magnetar.ui.search.optionMatches
import app.magnetar.ui.search.toggleFileKind
import app.magnetar.ui.search.without
import app.magnetar.ui.stats.statChipItems
import org.testng.Assert.assertEquals
import org.testng.Assert.assertFalse
import org.testng.Assert.assertTrue
import org.testng.annotations.Test

/** D140's shared test vectors (the same as the web client's `test/file-kinds.test.js`). */
class FileKindsTest {
    private val catalogue = FileKindCatalogue(
        listOf(
            FileKindGroup(
                "video", "Video",
                listOf(
                    FileKindLabel("video.mkv", "Matroska video", listOf("mkv", "mk3d")),
                    FileKindLabel("video.mp4", "MPEG-4 video", listOf("mp4")),
                    FileKindLabel("video.avi", "AVI video", listOf("avi")),
                ),
            ),
            FileKindGroup("doc", "Documents and e-books", listOf(FileKindLabel("doc.epub", "EPUB e-book", listOf("epub")))),
            FileKindGroup(
                "exe", "Executables",
                listOf(
                    FileKindLabel("exe.pe", "Windows PE executable", listOf("exe", "dll")),
                    FileKindLabel("exe.pe.dotnet", ".NET assembly"),
                    FileKindLabel("exe.elf", "ELF executable"),
                ),
            ),
            FileKindGroup("archive", "Archives", listOf(FileKindLabel("archive.zip", "ZIP archive", listOf("zip")), FileKindLabel("archive.rar", "RAR archive", listOf("rar")))),
        ),
    )
    private val labelsOf = fileKindLabelsOf(catalogue)

    @Test
    fun `toggle vectors`() {
        val vectors = listOf(
            Triple(listOf<String>(), "video", listOf("video")),
            Triple(listOf("video.mkv"), "video", listOf("video")),
            Triple(listOf("video"), "video", listOf()),
            Triple(listOf("video"), "video.mkv", listOf("video.avi", "video.mp4")),
            Triple(listOf("video.avi", "video.mp4"), "video.mkv", listOf("video")),
            Triple(listOf("video.mkv"), "video.mkv", listOf()),
            Triple(listOf("archive.zip"), "video.mkv", listOf("archive.zip", "video.mkv")),
            Triple(listOf("exe.pe"), "exe.pe.dotnet", listOf("exe.pe", "exe.pe.dotnet")),
        )
        for ((i, v) in vectors.withIndex()) {
            val (sel, toggle, want) = v
            assertEquals(toggleFileKind(sel, toggle, labelsOf), want, "vector ${i + 1}")
        }
    }

    @Test
    fun `normalize lowercases, trims, dedupes and folds labels into their group`() {
        assertEquals(normalizeFileKinds(listOf(" VIDEO.MKV", "video"), labelsOf), listOf("video"))
        assertEquals(normalizeFileKinds(listOf("video.mkv", "video.MKV ", ""), labelsOf), listOf("video.mkv"))
        assertEquals(normalizeFileKinds(listOf("video.mp4", "video.avi", "video.mkv", "exe.pe"), labelsOf), listOf("exe.pe", "video"))
        // A value of the right shape the catalogue lacks is kept (it matches nothing on the server).
        assertEquals(normalizeFileKinds(listOf("weird.kind"), labelsOf), listOf("weird.kind"))
    }

    @Test
    fun `group header state and label checks`() {
        assertEquals(fileKindState(listOf("video"), "video"), GroupState.CHECKED)
        assertEquals(fileKindState(listOf("video.mkv"), "video"), GroupState.MIXED)
        assertEquals(fileKindState(listOf("exe.pe"), "video"), GroupState.UNCHECKED)
        assertTrue(fileKindChecked(listOf("video"), "video.mp4"))
        assertFalse(fileKindChecked(listOf("exe.pe"), "exe.pe.dotnet"))
    }

    @Test
    fun `summary follows the option order`() {
        val groups = fileKindGroups(catalogue, null, emptyList())
        val order = fileKindOrder(groups)
        val name = { v: String -> fileKindName(catalogue, v) }
        assertEquals(dropdownSummary(emptyList(), order, name), "Any")
        assertEquals(dropdownSummary(listOf("video"), order, name), "Video")
        assertEquals(dropdownSummary(listOf("archive.zip", "video"), order, name), "Video, +1")
        assertEquals(dropdownSummary(listOf("video.mp4", "video.mkv"), order, name), "Matroska video, +1")
        val langs = listOf("en", "fr", "de")
        assertEquals(dropdownSummary(listOf("de", "fr", "it"), langs) { mapOf("fr" to "French", "de" to "German")[it] ?: it }, "French, +2")
    }

    @Test
    fun `names fall back to the ids without a catalogue`() {
        assertEquals(fileKindName(catalogue, "video.mkv"), "Matroska video")
        assertEquals(fileKindName(catalogue, "exe"), "Executables")
        assertEquals(fileKindName(null, "video"), "Video")
        assertEquals(fileKindName(null, "video.mkv"), "video.mkv")
        assertEquals(fileKindName(catalogue, "game.ps5.pkg"), "game.ps5.pkg")
    }

    @Test
    fun `labels of a group come from the counts without a catalogue`() {
        val counts = FacetCounts("probe_file_kind", 10, listOf(FacetCount("video", 5), FacetCount("video.mkv", 5)))
        val fromCounts = fileKindLabelsOf(null, counts)
        assertEquals(fromCounts("video"), listOf("video.mkv"))
        // Every known label picked = the group.
        assertEquals(toggleFileKind(emptyList(), "video.mkv", fromCounts), listOf("video"))
        assertEquals(fileKindLabelsOf(catalogue, counts)("video"), listOf("video.mkv", "video.mp4", "video.avi"))
    }

    @Test
    fun `rows by count, labels with a count or a selection, catalogue order without counts`() {
        val counts = FacetCounts(
            "probe_file_kind", 100,
            listOf(
                FacetCount("exe", 9), FacetCount("video", 60), FacetCount("video.mp4", 20), FacetCount("video.mkv", 40),
                FacetCount("exe.pe", 9), FacetCount("doc", 0),
            ),
        )
        val groups = fileKindGroups(catalogue, counts, listOf("archive.zip"))
        assertEquals(groups.map { it.group.value }, listOf("video", "exe", "archive"))
        assertEquals(groups[0].labels.map { it.value }, listOf("video.mkv", "video.mp4"))
        assertEquals(groups[2].labels.single().count, 0L)
        assertEquals(groups[0].labels[0].label, "Matroska video")
        val all = fileKindGroups(catalogue, null, emptyList())
        assertEquals(all.map { it.group.value }, listOf("video", "doc", "exe", "archive"))
        assertEquals(all[0].labels.map { it.value }, listOf("video.mkv", "video.mp4", "video.avi"))
        // No catalogue, no counts: a selected value still shows, so it can be cleared.
        val bare = fileKindGroups(null, null, listOf("game.ps5.pkg"))
        assertEquals(bare.single().group.label, "Game")
        assertEquals(bare.single().labels.single().value, "game.ps5.pkg")
    }

    @Test
    fun `rows keep their place while the sheet is open`() {
        val order = ChipOrder()
        val first = FacetCounts("probe_file_kind", 10, listOf(FacetCount("video", 6), FacetCount("video.mkv", 6), FacetCount("exe", 4)))
        assertEquals(order.arrangeFileKinds(fileKindGroups(catalogue, first, emptyList()), catalogue).map { it.group.value }, listOf("video", "exe"))
        val later = FacetCounts("probe_file_kind", 10, listOf(FacetCount("exe", 4), FacetCount("doc", 1)))
        val arranged = order.arrangeFileKinds(fileKindGroups(catalogue, later, emptyList()), catalogue)
        assertEquals(arranged.map { it.group.value }, listOf("video", "exe", "doc"))
        assertEquals(arranged[0].group.count, 0L)
        assertEquals(arranged[0].labels.single().value, "video.mkv")
    }

    @Test
    fun `search matches name, id, extension and group, diacritics folded`() {
        val groups = fileKindGroups(catalogue, null, emptyList())
        assertEquals(filterFileKindGroups(groups, "epub").map { g -> g.group.value to g.labels.map { it.value } }, listOf("doc" to listOf("doc.epub")))
        assertEquals(filterFileKindGroups(groups, "Matroska").single().labels.single().value, "video.mkv")
        assertEquals(filterFileKindGroups(groups, "dll").single().labels.single().value, "exe.pe")
        // A group name match keeps all its labels.
        assertEquals(filterFileKindGroups(groups, "executables").single().labels.size, 3)
        assertTrue(filterFileKindGroups(groups, "nothing like it").isEmpty())
        assertEquals(filterFileKindGroups(groups, " ").size, 4)
        assertTrue(optionMatches("FRAN", "fr", "Français"))
        assertTrue(optionMatches("fre", "fre", "French"))
        assertFalse(optionMatches("ger", "fr", "French"))
    }

    @Test
    fun `active filters list every value and remove one`() {
        val f = SearchFilters(
            categories = setOf("video"), languages = setOf("fr"), probeFileKinds = setOf("video", "archive.zip"),
            date = app.magnetar.ui.search.DatePreset.WEEK, probeSubKinds = setOf("sdh"),
        )
        val active = f.activeFilters(catalogue)
        assertEquals(
            active.map { "${it.field}:${it.value}" },
            listOf("category:video", "language:fr", "date:week", "probe_file_kind:archive.zip", "probe_file_kind:video", "probe_sub_kind:sdh"),
        )
        assertEquals(active.first { it.field == "probe_file_kind" && it.value == "video" }.label, "Video")
        assertEquals(f.without(ActiveFilter("probe_file_kind", "video", "Video")).probeFileKinds, setOf("archive.zip"))
        assertEquals(f.without(ActiveFilter("date", "week", "7 days")).date, app.magnetar.ui.search.DatePreset.ANY)
        assertEquals(f.clear("probe_file_kind").probeFileKinds, emptySet<String>())
        assertEquals(f.toggleFileKind("video.mkv", labelsOf).probeFileKinds, setOf("archive.zip", "video.avi", "video.mp4"))
    }

    @Test
    fun `many labels of one group are one chip, not one per catalogue label (D140 C3)`() {
        // A group of 21 labels, all but one left after unticking PDF from the checked group.
        val ids = (0 until 21).map { "doc.k%02d".format(it) }
        val big = FileKindCatalogue(
            listOf(
                FileKindGroup("doc", "Documents and e-books", (ids + "doc.pdf").map { FileKindLabel(it, if (it == "doc.pdf") "PDF document" else it) }),
            ) + catalogue.groups.filter { it.id == "video" || it.id == "exe" },
        )
        val blo = fileKindLabelsOf(big)
        val name = { v: String -> fileKindName(big, v) }
        val sel = toggleFileKind(listOf("doc"), "doc.pdf", blo)
        assertEquals(sel.size, 21)
        assertEquals(fileKindChips(sel, blo, name).map { it.label to it.values }, listOf("Documents and e-books except PDF document" to sel))
        val order = fileKindOrder(fileKindGroups(big, null, sel))
        assertEquals(fileKindSummary(sel, order, blo, name), "Documents and e-books except PDF document")
        val mixed = sel + "exe.pe" + "video"
        assertEquals(fileKindChips(mixed, blo, name).map { it.label }, listOf("Documents and e-books except PDF document", "Windows PE executable", "Video"))
        // Without counts the option order is the catalogue's: Documents first.
        assertEquals(fileKindSummary(mixed, fileKindOrder(fileKindGroups(big, null, mixed)), blo, name), "Documents and e-books except PDF document, +2")
        assertEquals(fileKindChips(sel.take(10), blo, name).single().label, "Documents and e-books: 10 kinds")
        assertEquals(fileKindChips(listOf("doc.k00", "doc.k01", "doc.pdf"), blo, name).map { it.values }, listOf(listOf("doc.k00"), listOf("doc.k01"), listOf("doc.pdf")))
        assertEquals(fileKindSummary(emptyList(), emptyList(), blo, name), "Any")

        // The sheet's active row: one chip, and removing it removes every one of its labels.
        val f = SearchFilters(categories = setOf("book"), probeFileKinds = sel.toSet())
        val active = f.activeFilters(big)
        assertEquals(active.map { it.label }, listOf("Book", "Documents and e-books except PDF document"))
        assertEquals(f.without(active.last()).probeFileKinds, emptySet<String>())
        // The Stats filter row too.
        val stat = f.statChipItems(big, blo)
        val kinds = stat.filter { it.fileKinds.isNotEmpty() }
        assertEquals(kinds.map { it.label to it.fileKinds }, listOf("Documents and e-books except PDF document" to sel))
        assertEquals(stat.size, kinds.size + stat.count { it.field == "category" })
    }
}
