// The detail's file list as a tree (FileTree.kt): directories first, sizes summed, small torrents
// open fully, big ones collapsed. Every name is untrusted text.

import { h, untrusted } from "../dom.js";
import { formatBytes } from "../format.js";

/** Most rows shown; past it "… N more". */
export const MAX_FILE_ROWS = 300;

/**
 * @typedef {object} FileNode
 * @property {string} name
 * @property {string} path
 * @property {Map<string, FileNode>} children
 * @property {boolean} isFile
 * @property {number | null} fileSize
 * @property {number | null} totalSize sum of known sizes below; null when nothing has one
 * @property {number} fileCount
 */

/**
 * @param {string} name
 * @param {string} path
 * @returns {FileNode}
 */
function node(name, path) {
  return { name, path, children: new Map(), isFile: false, fileSize: null, totalSize: null, fileCount: 0 };
}

/**
 * @param {readonly import("../types-stats.js").TorrentFile[]} files
 * @returns {FileNode}
 */
export function buildFileTree(files) {
  const root = node("", "");
  for (const f of files) {
    const path = typeof f.path === "string" ? f.path : "";
    let parts = path.split("/").filter((p) => p !== "");
    if (parts.length === 0) parts = [path];
    let n = root;
    parts.forEach((part, i) => {
      const last = i === parts.length - 1;
      const p = n.path === "" ? part : `${n.path}/${part}`;
      // A file and a directory of the same name (malformed list) stay apart.
      const key = last ? part : `${part}/`;
      let c = n.children.get(key);
      if (!c) {
        c = node(part, p);
        n.children.set(key, c);
      }
      n = c;
      if (last) {
        n.isFile = true;
        n.fileSize = typeof f.size === "number" ? f.size : null;
      }
    });
  }
  /** @param {FileNode} n */
  const sum = (n) => {
    if (n.isFile) {
      n.totalSize = n.fileSize;
      n.fileCount = 1;
      return;
    }
    let total = null;
    let count = 0;
    for (const c of n.children.values()) {
      sum(c);
      if (c.totalSize != null) total = (total ?? 0) + c.totalSize;
      count += c.fileCount;
    }
    n.totalSize = total;
    n.fileCount = count;
  };
  sum(root);
  return root;
}

/** @param {FileNode} n */
function sortedChildren(n) {
  const all = [...n.children.values()].sort((a, b) => {
    const x = a.name.toLowerCase();
    const y = b.name.toLowerCase();
    return x < y ? -1 : x > y ? 1 : 0;
  });
  return [...all.filter((c) => !c.isFile), ...all.filter((c) => c.isFile)];
}

/**
 * Visible rows: directories first, then files; children only of expanded directories.
 * @param {FileNode} root
 * @param {ReadonlySet<string>} expanded
 * @returns {{depth: number, node: FileNode, expanded: boolean}[]}
 */
export function flattenTree(root, expanded) {
  /** @type {{depth: number, node: FileNode, expanded: boolean}[]} */
  const out = [];
  /** @param {FileNode} n @param {number} depth */
  const walk = (n, depth) => {
    for (const c of sortedChildren(n)) {
      const open = !c.isFile && expanded.has(c.path);
      out.push({ depth, node: c, expanded: open });
      if (open) walk(c, depth + 1);
    }
  };
  walk(root, 0);
  return out;
}

/** @param {FileNode} root */
export function allDirs(root) {
  /** @type {Set<string>} */
  const out = new Set();
  /** @param {FileNode} n */
  const walk = (n) => {
    for (const c of n.children.values()) {
      if (!c.isFile) {
        out.add(c.path);
        walk(c);
      }
    }
  };
  walk(root);
  return out;
}

/**
 * The tree section: rows are buttons for folders (expand/collapse), plain rows for files.
 * @param {readonly import("../types-stats.js").TorrentFile[]} files
 * @param {{expanded: Set<string> | null}} state kept by the caller across renders
 */
export function fileTree(files, state) {
  const root = buildFileTree(files);
  state.expanded ??= files.length <= 30 ? allDirs(root) : new Set();
  const list = h("ul", { class: "file-tree" });
  const more = h("p", { class: "muted" });
  const render = () => {
    const rows = flattenTree(root, /** @type {Set<string>} */ (state.expanded));
    list.replaceChildren(...rows.slice(0, MAX_FILE_ROWS).map((r) => {
      const n = r.node;
      const size = h("span", { class: "file-size muted", text: n.totalSize != null ? formatBytes(n.totalSize) : "?" });
      const label = n.isFile
        ? h("span", { class: "file-name" }, h("span", { class: "file-icon", "aria-hidden": "true", text: "▫" }), untrusted(n.name))
        : h(
          "button",
          {
            type: "button",
            class: "file-name folder",
            "aria-expanded": String(r.expanded),
            "on:click": () => {
              const e = /** @type {Set<string>} */ (state.expanded);
              if (e.has(n.path)) e.delete(n.path);
              else e.add(n.path);
              render();
            },
          },
          h("span", { class: "file-icon", "aria-hidden": "true", text: r.expanded ? "▾" : "▸" }),
          untrusted(n.name),
          ` (${n.fileCount})`,
        );
      const li = h("li", { class: "file-row" }, label, size);
      li.style.setProperty("--depth", String(Math.min(r.depth, 12)));
      return li;
    }));
    more.textContent = rows.length > MAX_FILE_ROWS ? `… ${rows.length - MAX_FILE_ROWS} more` : "";
    more.hidden = rows.length <= MAX_FILE_ROWS;
  };
  render();
  return h("div", null, list, more);
}
