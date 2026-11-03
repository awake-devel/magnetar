// "Save search" (SaveSearchDialog.kt): the current text, filters and sort, stored on the server.

import { h } from "../dom.js";
import { openDialog } from "../ui/dialog.js";

/**
 * @param {{initialName: string, filterCount: number, phone: boolean,
 *   onSave: (name: string, notify: boolean) => Promise<boolean>}} opts onSave resolves false to keep the dialog open
 */
export function openSaveDialog(opts) {
  const name = /** @type {HTMLInputElement} */ (h("input", {
    type: "text",
    id: "save-name",
    class: "field-input",
    value: opts.initialName.slice(0, 200),
    maxlength: "200",
    autocomplete: "off",
  }));
  const notify = /** @type {HTMLInputElement} */ (h("input", {
    type: "checkbox",
    id: "save-notify",
    role: "switch",
    class: "switch",
    checked: true,
  }));
  const err = h("p", { class: "field-error", role: "alert" });
  const d = openDialog({
    title: "Save search",
    cls: opts.phone ? "full" : undefined,
    body: [
      h("label", { for: "save-name", class: "field-label", text: "Name" }),
      name,
      opts.filterCount > 0
        ? h("p", {
          class: "field-hint",
          text: opts.filterCount === 1 ? "With the active filter" : `With the ${opts.filterCount} active filters`,
        })
        : null,
      h("label", { class: "switch-row", for: "save-notify" }, h("span", { text: "Notify on new results" }), notify),
      err,
    ],
    actions: [
      { label: "Cancel", kind: "text" },
      {
        label: "Save",
        kind: "primary",
        onClick: async (dlg) => {
          const n = name.value.trim();
          if (n === "") {
            err.textContent = "Give it a name";
            return false;
          }
          dlg.setBusy(true);
          const ok = await opts.onSave(n, notify.checked);
          dlg.setBusy(false);
          return ok;
        },
      },
    ],
  });
  const save = /** @type {HTMLButtonElement | null} */ (d.footer.lastElementChild);
  name.addEventListener("input", () => {
    if (save) save.disabled = name.value.trim() === "";
  });
  name.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      save?.click();
    }
  });
  name.focus();
  name.select();
  return d;
}
