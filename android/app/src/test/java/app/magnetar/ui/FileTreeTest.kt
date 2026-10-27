package app.magnetar.ui

import app.magnetar.api.TorrentFile
import app.magnetar.ui.detail.allDirs
import app.magnetar.ui.detail.buildFileTree
import app.magnetar.ui.detail.flattenTree
import app.magnetar.ui.detail.seederSeries
import app.magnetar.api.LivenessPoint
import org.testng.Assert.assertEquals
import org.testng.annotations.Test

class FileTreeTest {
    @Test
    fun `directories first, sizes summed, collapsed until opened`() {
        val root = buildFileTree(
            listOf(
                TorrentFile("Show/S01/E02.mkv", 200),
                TorrentFile("Show/S01/E01.mkv", 100),
                TorrentFile("Show/readme.txt", null),
                TorrentFile("Show/Subs/en.srt", 5),
            ),
        )
        val top = flattenTree(root, emptySet())
        assertEquals(top.map { it.node.name }, listOf("Show"))
        assertEquals(top.single().node.totalSize, 305L)
        assertEquals(top.single().node.fileCount, 4)
        val open = flattenTree(root, allDirs(root))
        assertEquals(open.map { it.depth to it.node.name }, listOf(0 to "Show", 1 to "S01", 2 to "E01.mkv", 2 to "E02.mkv", 1 to "Subs", 2 to "en.srt", 1 to "readme.txt"))
    }

    @Test
    fun `seed history skips unanswered points and takes the best of trackers and DHT`() {
        val (s, l) = seederSeries(
            listOf(
                LivenessPoint(at = 1, seeders = 3, leechers = 1, peersEst = 10),
                LivenessPoint(at = 2),
                LivenessPoint(at = 3, seeders = 4),
            ),
        )
        assertEquals(s.points, listOf(1L to 10, 3L to 4))
        assertEquals(l.points, listOf(1L to 1))
        assertEquals(s.max, 10)
    }
}
