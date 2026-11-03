// "Detected" (ProbeCard.kt, D117, D121, D131): what a worker read from the files themselves.
// With `probe_corrections` every audio and subtitle track is an editable row; without it (an
// older server) the D126 lines. Every name read from a file is untrusted text, cleaned of bidi
// and control characters before it is shown.

import { h, untrusted } from "../dom.js";
import { fileName } from "../format.js";
import { checkAllItems, draftOf, MAX_SUBTITLE_ROWS, TRACK_AUDIO, TRACK_SUB, trackRows, unplacedText } from "../probe-correction.js";
import { probeSummary } from "../probe-format.js";
import { openTrackEditor } from "./probe-editor.js";

/**
 * Per-detail UI state kept across renders.
 * @typedef {object} ProbeUiState
 * @property {Set<number>} allSubs files whose subtitle list is expanded
 * @property {Set<string>} showDetected rows showing "Detected: …"
 */

/** @returns {ProbeUiState} */
export function probeUiState() {
  return { allSubs: new Set(), showDetected: new Set() };
}

/** @param {import("../probe-correction.js").TrackRow} r */
function rowKey(r) {
  return `${r.file}:${r.track}:${r.pos ?? `added${r.fix?.id ?? 0}`}`;
}

/**
 * @param {{
 *   probe: import("../types-stats.js").ProbeInfo,
 *   views: import("../probe-format.js").ProbeFileView[],
 *   canCorrect: boolean,
 *   correcting: boolean,
 *   phone: boolean,
 *   ui: ProbeUiState,
 *   rerender: () => void,
 *   onSave: (items: import("../types-stats.js").ProbeCorrectionItem[], message: string) => Promise<boolean>,
 *   onUndo: (id: number) => Promise<boolean>,
 * }} o
 */
export function probeCard(o) {
  /** @param {import("../probe-correction.js").TrackDraft} draft @param {import("../probe-correction.js").TrackRow | null} row */
  const edit = (draft, row) =>
    openTrackEditor({
      draft,
      row,
      phone: o.phone,
      onSave: (item) => o.onSave([item], "Correction saved"),
      onUndo: (id) => o.onUndo(id),
    });

  /** @param {import("../probe-correction.js").TrackRow} r */
  const trackRow = (r) => {
    const key = rowKey(r);
    const shown = o.ui.showDetected.has(key);
    const main = h(
      "div",
      { class: "track-main" },
      h("p", { class: ["track-text", r.removed && "removed"] }, untrusted(r.text)),
      r.name != null ? h("p", { class: "muted small" }, "“", untrusted(r.name), "”") : null,
      r.marker != null ? h("p", { class: ["track-marker", r.removed && "marker-removed"], text: r.marker }) : null,
      r.review != null ? h("p", { class: "track-review", text: r.review }) : null,
      r.corrected && r.detectedText != null && shown ? h("p", { class: "muted small" }, "Detected: ", untrusted(r.detectedText)) : null,
    );
    const toggle = r.corrected && r.detectedText != null
      ? h("button", {
        type: "button",
        class: "btn text small-btn",
        "aria-expanded": String(shown),
        text: shown ? "Hide detected" : "Show detected",
        "on:click": () => {
          if (shown) o.ui.showDetected.delete(key);
          else o.ui.showDetected.add(key);
          o.rerender();
        },
      })
      : null;
    const editBtn = h("button", {
      type: "button",
      class: "btn text",
      text: "Edit",
      "aria-label": r.editLabel,
      title: r.editLabel,
      disabled: o.correcting,
      "on:click": () => edit(draftOf(r), r),
    });
    const li = h("li", { class: ["track-row", o.correcting && "busy"], dataset: { track: key } }, h("div", { class: "track-col" }, main, toggle), editBtn);
    main.addEventListener("click", () => {
      if (!o.correcting) edit(draftOf(r), r);
    });
    return li;
  };

  /** @type {Node[]} */
  const files = [];
  o.views.forEach((f, i) => {
    const info = f.file;
    /** @type {Node[]} */
    const parts = [
      h("h4", { class: "probe-file" }, untrusted(f.name)),
      h("p", { class: ["small", f.verified ? "verified" : "muted"] }, untrusted(f.headline)),
      ...f.warnings.map((w) => h("p", { class: "probe-warning", text: `⚠ ${w}` })),
    ];
    if (o.canCorrect && info.video) {
      parts.push(...f.videoLines.map((l) => h("p", { class: "small" }, untrusted(l))));
      const rows = trackRows(info);
      const subs = rows.filter((r) => r.track === TRACK_SUB);
      const file = info.dht_index ?? 0;
      const all = o.ui.allSubs.has(file) || subs.length <= MAX_SUBTITLE_ROWS;
      const shownSubs = all ? subs : subs.slice(0, MAX_SUBTITLE_ROWS);
      parts.push(h(
        "ul",
        { class: "track-list", "aria-label": `Tracks of ${f.name}` },
        ...rows.filter((r) => r.track === TRACK_AUDIO).map(trackRow),
        ...shownSubs.map(trackRow),
      ));
      if (shownSubs.length < subs.length) {
        parts.push(h("button", {
          type: "button",
          class: "btn text",
          text: `Show all ${subs.length} subtitle tracks`,
          "on:click": () => {
            o.ui.allSubs.add(file);
            o.rerender();
          },
        }));
      }
      const unchecked = checkAllItems(info);
      parts.push(h(
        "div",
        { class: "row-actions" },
        h("button", {
          type: "button",
          class: "btn text",
          text: "Add a track the probe missed",
          disabled: o.correcting,
          "on:click": () => edit(draftOf(file), null),
        }),
        unchecked.length > 0
          ? h("button", {
            type: "button",
            class: "btn text",
            text: "All tracks match VLC",
            disabled: o.correcting,
            "on:click": () => void o.onSave(unchecked, `Marked ${unchecked.length} tracks as checked`),
          })
          : null,
      ));
      parts.push(...f.otherLines.map((l) => h("p", { class: "small" }, untrusted(l))));
    } else {
      parts.push(...f.lines.map((l) => h("p", { class: "small" }, untrusted(l))));
    }
    files.push(h("div", { class: ["probe-file-block", i > 0 && "sep"] }, ...parts));
  });
  const unplaced = o.canCorrect ? o.probe.unplaced ?? [] : [];
  return h(
    "section",
    { class: "card probe-card", "aria-label": "Detected" },
    h("h3", { class: "card-title", text: "Detected" }),
    h("p", { class: "small", text: "Read from the first blocks of each file, not from its name." }),
    h("p", { class: "muted small", text: probeSummary(o.probe) }),
    ...files,
    unplaced.length > 0
      ? h(
        "div",
        { class: "probe-file-block sep" },
        h("h4", { class: "probe-file", text: "Corrections that no longer match the probe" }),
        h(
          "ul",
          { class: "plain-list" },
          ...unplaced.map((u) => {
            const path = u.path ?? "";
            const fname = fileName(path).trim() !== "" ? fileName(path) : path;
            return h(
              "li",
              { class: "unplaced-row" },
              h("p", { class: "small" }, untrusted(unplacedText(u, fname))),
              h("button", {
                type: "button",
                class: "btn text",
                text: "Remove",
                disabled: o.correcting || u.id == null,
                "on:click": () => {
                  if (u.id != null) void o.onUndo(u.id);
                },
              }),
            );
          }),
        ),
      )
      : null,
  );
}
