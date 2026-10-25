package app.magnetar.ui.detail

import app.magnetar.ffi.FfiFile

class FileNode(val name: String, val path: String) {
    val children = sortedMapOf<String, FileNode>(String.CASE_INSENSITIVE_ORDER)
    var fileSize: ULong? = null
    var isFile = false

    /** Sum of known sizes below this node; null when nothing below has a size. */
    val totalSize: ULong? by lazy {
        if (isFile) fileSize else children.values.mapNotNull { it.totalSize }.takeIf { it.isNotEmpty() }?.sum()
    }
    val fileCount: Int by lazy { if (isFile) 1 else children.values.sumOf { it.fileCount } }
}

fun buildFileTree(files: List<FfiFile>): FileNode {
    val root = FileNode("", "")
    for (f in files) {
        var node = root
        val parts = f.path.split('/').filter { it.isNotEmpty() }.ifEmpty { listOf(f.path) }
        for ((i, part) in parts.withIndex()) {
            val path = if (node.path.isEmpty()) part else node.path + "/" + part
            node = node.children.getOrPut(part) { FileNode(part, path) }
            if (i == parts.lastIndex) {
                node.isFile = true
                node.fileSize = f.size
            }
        }
    }
    return root
}

data class TreeRow(val depth: Int, val node: FileNode, val expanded: Boolean)

/** Visible rows: directories first, then files; children only of [expanded] directory paths. */
fun flattenTree(root: FileNode, expanded: Set<String>): List<TreeRow> {
    val out = mutableListOf<TreeRow>()
    fun walk(n: FileNode, depth: Int) {
        val (dirs, files) = n.children.values.partition { !it.isFile }
        for (c in dirs + files) {
            val open = !c.isFile && c.path in expanded
            out += TreeRow(depth, c, open)
            if (open) walk(c, depth + 1)
        }
    }
    walk(root, 0)
    return out
}
