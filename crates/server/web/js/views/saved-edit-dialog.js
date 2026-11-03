// "Edit saved search" (a web addition, PUT /api/v1/saved-searches/{id}): name, Notify, and
// "Replace its query with the current search". The dialog stays open on a failure.

import { h } from "../dom.js";
import { openDialog } from "../ui/dialog.js";

let counter = 0;

/**
 * @param {{
 *   search: import("../types.js").SavedSearch,
 *   canNotify: boolean,
 *   currentSummary: string,
 *   phone: boolean,
 *   onSave: (name: string, notify: boolean, replaceQuery: boolean) => Promise<boolean>,
 * }} o
 */
export function openSavedEditDialog(o) {
  const id = `se-${++counter}`;
  const name = /** @type {HTMLInputElement} */ (h("input", {
    type: "text",
    class: "field-input",
    id: `${id}-name`,
    value: o.search.name ?? "",
    maxlength: "200",
    autocomplete: "off",
    spellcheck: "false",
  }));
  const notify = /** @type {HTMLInputElement} */ (h("input", {
    type: "checkbox",
    class: "switch",
    role: "switch",
    id: `${id}-notify`,
    checked: o.search.notify === true,
    disabled: !o.canNotify,
  }));
  const replace = /** @type {HTMLInputElement} */ (h("input", { type: "checkbox", id: `${id}-replace` }));
  const error = h("p", { class: "field-error", role: "alert" });
  const dlg = openDialog({
    title: "Edit saved search",
    cls: o.phone ? "full" : undefined,
    body: [
      h("label", { class: "field-label", for: `${id}-name`, text: "Name" }),
      name,
      h("label", { class: "switch-row", for: `${id}-notify` }, h("span", { class: "switch-title", text: "Notify on new results" }), notify),
      h(
        "label",
        { class: "check-row", for: `${id}-replace` },
        replace,
        h("span", null, h("span", { class: "switch-title", text: "Replace its query with the current search" }), h("span", { class: "muted small", text: o.currentSummary })),
      ),
      error,
    ],
    actions: [
      { label: "Cancel", kind: "text" },
      {
        label: "Save",
        kind: "primary",
        onClick: async (d) => {
          const n = name.value.trim();
          if (n === "") {
            error.textContent = "Give it a name";
            return false;
          }
          error.textContent = "";
          d.setBusy(true);
          const ok = await o.onSave(n, notify.checked, replace.checked);
          d.setBusy(false);
          return ok;
        },
      },
    ],
  });
  name.focus();
  return dlg;
}
