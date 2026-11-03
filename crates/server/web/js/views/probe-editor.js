// The track editor of the "Detected" card (ProbeCard.kt TrackEditorSheet, D131): language
// picker, kind chips, the name as VLC shows it, "This track doesn't exist", and Save / "Matches
// VLC" / "Still correct" / "Undo correction". While a request is in flight the buttons are off,
// a progress bar shows and Esc or the backdrop do not close it; a failure keeps the draft.

import { h, untrusted } from "../dom.js";
import {
  cleanTrackName,
  editorTitle,
  itemOf,
  kindChoices,
  kindSet,
  languageChoices,
  languageLabel,
  primaryAction,
  TRACK_AUDIO,
  TRACK_SUB,
} from "../probe-correction.js";
import { chip, chipRow } from "../ui/chips.js";
import { openDialog } from "../ui/dialog.js";

let counter = 0;

/**
 * @param {{
 *   draft: import("../probe-correction.js").TrackDraft,
 *   row: import("../probe-correction.js").TrackRow | null,
 *   phone: boolean,
 *   onSave: (item: import("../types-stats.js").ProbeCorrectionItem) => Promise<boolean>,
 *   onUndo: (id: number) => Promise<boolean>,
 * }} o
 */
export function openTrackEditor(o) {
  const id = `ed-${++counter}`;
  let d = { ...o.draft };
  let picking = false;
  let sending = false;

  const typeRow = h("div", { class: "segmented", role: "group", "aria-label": "Track type" });
  const langValue = h("span", { class: "lang-value" });
  const langBtn = h("button", { type: "button", class: "btn text", text: "Change", "aria-expanded": "false", "aria-controls": `${id}-picker` });
  const search = /** @type {HTMLInputElement} */ (h("input", {
    type: "search",
    class: "field-input",
    id: `${id}-search`,
    placeholder: "Search languages",
    "aria-label": "Search languages",
    autocomplete: "off",
    spellcheck: "false",
  }));
  const choices = h("ul", { class: "lang-list", "aria-label": "Languages" });
  const picker = h("div", { class: "lang-picker", id: `${id}-picker`, hidden: true }, search, choices);
  const kinds = h("div", { class: "kind-chips" });
  const name = /** @type {HTMLInputElement} */ (h("input", {
    type: "text",
    class: "field-input",
    id: `${id}-name`,
    autocomplete: "off",
    spellcheck: "false",
    value: d.name,
  }));
  const removed = /** @type {HTMLInputElement} */ (h("input", { type: "checkbox", class: "switch", role: "switch", id: `${id}-rm` }));
  const undoBtn = h("button", { type: "button", class: "btn text", text: "Undo correction" });
  const cancelBtn = h("button", { type: "button", class: "btn text", text: "Cancel" });
  const saveBtn = h("button", { type: "button", class: "btn primary" });

  const body = [
    h("p", { class: "muted small", text: "Compare with VLC's Audio Track / Sub Track menu: tracks are listed in the same order." }),
    o.row && !o.row.added && o.row.detectedText != null ? h("p", { class: "small" }, "Detected: ", untrusted(o.row.detectedText)) : null,
    o.row && !o.row.added && o.row.detectedName != null
      ? h("p", { class: "small" }, "Name in the file: “", untrusted(o.row.detectedName), "”")
      : null,
    d.start == null ? typeRow : null,
    h("div", { class: "field" }, h("span", { class: "field-label", id: `${id}-lang-l`, text: "Language" }), h("div", { class: "lang-row" }, langValue, langBtn)),
    picker,
    h("div", { class: "field" }, h("span", { class: "field-label", text: "Kind" }), kinds),
    h(
      "div",
      { class: "field" },
      h("label", { class: "field-label", for: `${id}-name`, text: "Track name (as VLC shows it)" }),
      name,
      h("span", { class: "field-hint", text: "Optional" }),
    ),
    d.pos != null
      ? h(
        "label",
        { class: "switch-row", for: `${id}-rm` },
        h(
          "span",
          null,
          h("span", { class: "switch-title", text: "This track doesn't exist" }),
          h("span", { class: "muted small", text: "VLC does not list it. It is left out of search." }),
        ),
        removed,
      )
      : null,
  ];

  const dlg = openDialog({
    title: editorTitle(d),
    body,
    actions: [],
    cls: o.phone ? "full editor" : "editor",
    closeButton: true,
  });
  dlg.footer.append(undoBtn, cancelBtn, saveBtn);

  function renderChoices() {
    const list = languageChoices(search.value);
    choices.replaceChildren(...list.slice(0, 200).map((c) =>
      h("li", null, h("button", {
        "aria-pressed": String(c.code === d.lang),
        type: "button",
        class: "lang-choice",
        text: c.label,
        "on:click": () => {
          d = { ...d, lang: c.code };
          picking = false;
          sync();
          langBtn.focus();
        },
      }))
    ));
  }

  function sync() {
    const fieldsOn = !d.removed && !sending;
    typeRow.replaceChildren(
      ...[[TRACK_AUDIO, "Audio"], [TRACK_SUB, "Subtitle"]].map(([t, label]) =>
        h("button", {
          type: "button",
          class: ["btn", "seg", d.track === t && "on"],
          "aria-pressed": String(d.track === t),
          text: label,
          disabled: sending,
          "on:click": () => {
            if (d.track !== t) d = { ...d, track: t, kinds: [] };
            sync();
          },
        })
      ),
    );
    langValue.textContent = languageLabel(d.lang);
    /** @type {HTMLButtonElement} */ (langBtn).disabled = !fieldsOn;
    langBtn.setAttribute("aria-expanded", String(picking));
    picker.hidden = !picking || !fieldsOn;
    kinds.replaceChildren(chipRow(kindChoices(d.track, d.kinds).map((k) =>
      chip({
        label: k.label,
        selected: d.kinds.includes(k.kind),
        disabled: !fieldsOn,
        onToggle: () => {
          d = { ...d, kinds: d.kinds.includes(k.kind) ? d.kinds.filter((x) => x !== k.kind) : kindSet([...d.kinds, k.kind]) };
          sync();
        },
      })
    ), "Kind"));
    name.disabled = !fieldsOn;
    removed.checked = d.removed;
    removed.disabled = sending;
    undoBtn.hidden = d.fixId == null;
    /** @type {HTMLButtonElement} */ (undoBtn).disabled = sending;
    /** @type {HTMLButtonElement} */ (cancelBtn).disabled = sending;
    const [label, ok] = primaryAction(d);
    saveBtn.textContent = label;
    /** @type {HTMLButtonElement} */ (saveBtn).disabled = !ok || sending;
  }

  /** @param {() => Promise<boolean>} call */
  async function send(call) {
    if (sending) return;
    sending = true;
    dlg.setBusy(true);
    sync();
    let ok = false;
    try {
      ok = await call();
    } finally {
      sending = false;
      if (ok) dlg.close();
      else {
        dlg.setBusy(false);
        sync();
      }
    }
  }

  langBtn.addEventListener("click", () => {
    picking = !picking;
    sync();
    if (picking) {
      renderChoices();
      search.focus();
    }
  });
  search.addEventListener("input", renderChoices);
  name.addEventListener("input", () => {
    const clean = cleanTrackName(name.value);
    if (clean !== name.value) name.value = clean;
    d = { ...d, name: clean };
    sync();
  });
  removed.addEventListener("change", () => {
    d = { ...d, removed: removed.checked };
    sync();
  });
  cancelBtn.addEventListener("click", () => {
    if (!sending) dlg.close();
  });
  undoBtn.addEventListener("click", () => {
    const fixId = d.fixId;
    if (fixId != null) void send(() => o.onUndo(fixId));
  });
  saveBtn.addEventListener("click", () => {
    const item = itemOf(d);
    if (item) void send(() => o.onSave(item));
  });
  sync();
  return dlg;
}
