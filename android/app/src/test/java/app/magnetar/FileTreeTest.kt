package app.magnetar

import app.magnetar.ffi.FfiFile
import app.magnetar.ui.detail.buildFileTree
import app.magnetar.ui.detail.flattenTree
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class FileTreeTest {
    private val files = listOf(
        FfiFile("Show/S01/e02.mkv", 200u),
        FfiFile("Show/S01/e01.mkv", 100u),
        FfiFile("Show/readme.txt", null),
        FfiFile("Show/Extras/a.mkv", 5u),
    )

    @Test
    fun sizesAndCountsRollUp() {
        val root = buildFileTree(files)
        val show = root.children.getValue("Show")
        assertEquals(305uL, show.totalSize)
        assertEquals(4, show.fileCount)
        assertNull(show.children.getValue("readme.txt").totalSize)
    }

    @Test
    fun collapsedShowsTopLevelOnly() {
        val rows = flattenTree(buildFileTree(files), emptySet())
        assertEquals(listOf("Show"), rows.map { it.node.path })
    }

    @Test
    fun expandedListsDirsBeforeFiles() {
        val rows = flattenTree(buildFileTree(files), setOf("Show", "Show/S01"))
        assertEquals(
            listOf("Show", "Show/Extras", "Show/S01", "Show/S01/e01.mkv", "Show/S01/e02.mkv", "Show/readme.txt"),
            rows.map { it.node.path },
        )
        assertEquals(listOf(0, 1, 1, 2, 2, 1), rows.map { it.depth })
    }
}
