// The filter dropdown (D140, docs/WEB.md "Filters"): a trigger with the title and the current
// selection, a per-group clear, and an in-flow panel (never a popover the sheet could clip) with
// an optional search box and native checkboxes or radios. Stateless: the filter panel owns which
// one is open, the search text and the expanded groups, and renders again on every change.

import { h, untrusted } from "../dom.js";
import { formatCount } from "../format.js";
import { icon } from "../icons.js";

/**
 * @typedef {object} DropdownOpts
 * @property {string} uid
 * @property {string} field
 * @property {string} title
 * @property {string} summary "Any", "French", "French, +2"
 * @property {boolean} open
 * @property {boolean} selection
 * @property {string | null} note a line under the trigger
 * @property {string | null} search the search text, null: no search box
 * @property {Node[]} rows
 * @property {(byKeyboard: boolean) => void} onOpen
 * @property {(focusTrigger: boolean) => void} onClose
 * @property {() => void} onClear
 * @property {(text: string) => void} onSearch
 * @property {(group: string, expand: boolean) => void} onExpand
 */

/** @param {DropdownOpts} o */
export function dropdown(o) {
  const id = `${o.uid}-${o.field}`;
  const fk = (/** @type {string} */ p) => ({ dd: o.field, fkey: `${p}:${o.field}` });
  const trigger = h(
    "button",
    {
      type: "button",
      class: ["dd-trigger", o.selection && "on"],
      "aria-expanded": String(o.open),
      "aria-controls": id,
      "aria-label": `${o.title}: ${o.summary}`,
      dataset: fk("dd"),
      // detail 0: Enter or Space, which also focus the first item.
      "on:click": (/** @type {MouseEvent} */ e) => (o.open ? o.onClose(false) : o.onOpen(e.detail === 0)),
    },
    h("span", { class: "dd-title", text: o.title }),
    h("span", { class: "dd-summary" }, untrusted(o.summary)),
    icon("chevron", { cls: "dd-arrow" }),
  );
  const panel = h("div", { class: "dd-panel", id, role: "group", "aria-label": o.title, hidden: !o.open, dataset: { dd: o.field } });
  if (o.open) {
    if (o.search != null) {
      const input = /** @type {HTMLInputElement} */ (h("input", {
        type: "search",
        class: "field-input dd-search",
        placeholder: "Search",
        "aria-label": `Search ${o.title}`,
        "aria-controls": `${id}-list`,
        autocomplete: "off",
        value: o.search,
        dataset: fk("dds"),
        "on:input": () => o.onSearch(input.value),
      }));
      panel.append(input);
    }
    panel.append(h("div", { class: "dd-list", id: `${id}-list` }, ...o.rows));
    // A press on padding or a row's text keeps the focus inside (a blur would close the panel).
    panel.addEventListener("mousedown", (e) => {
      if (e.target instanceof Element && !e.target.closest("input, button")) e.preventDefault();
    });
  }
  const wrap = h(
    "div",
    { class: ["dd", o.open && "open"], dataset: { ddWrap: o.field } },
    h(
      "div",
      { class: "dd-head" },
      trigger,
      o.selection
        ? h("button", { type: "button", class: "btn icon-only dd-clear", "aria-label": `Clear ${o.title}`, title: `Clear ${o.title}`, dataset: fk("ddc"), "on:click": o.onClear }, icon("close"))
        : null,
    ),
    o.note ? h("p", { class: "field-hint dd-note", text: o.note }) : null,
    panel,
  );
  wrap.addEventListener("keydown", (e) => onKey(e, o, trigger, panel));
  return wrap;
}

/**
 * The search box, then each enabled input, in order.
 * @param {Element} panel
 */
export const panelItems = (panel) => [...panel.querySelectorAll("input")].filter((x) => !x.disabled);

/**
 * @param {KeyboardEvent} e
 * @param {DropdownOpts} o
 * @param {HTMLElement} trigger
 * @param {HTMLElement} panel
 */
function onKey(e, o, trigger, panel) {
  const t = e.target;
  /** @type {HTMLElement | undefined} */
  let next;
  if (e.key === "Escape" && o.open) {
    // Inside the filter sheet, Esc closes the dropdown, not the sheet.
    e.stopPropagation();
    o.onClose(true);
  } else if (t === trigger) {
    if (e.key !== "ArrowDown") return;
    if (!o.open) o.onOpen(true);
    else next = panelItems(panel)[0];
  } else if (t instanceof HTMLInputElement && panel.contains(t)) {
    const items = panelItems(panel);
    const i = items.indexOf(t);
    const text = t.type === "search";
    if (e.key === "ArrowDown") next = items[Math.min(items.length - 1, i + 1)];
    else if (e.key === "ArrowUp") next = i <= 0 ? trigger : items[i - 1];
    else if (e.key === "Home" && !text) next = items[0];
    else if (e.key === "End" && !text) next = items[items.length - 1];
    else if ((e.key === "ArrowRight" || e.key === "ArrowLeft") && t.dataset.group && t.dataset.group === t.dataset.value) {
      o.onExpand(t.dataset.group, e.key === "ArrowRight");
    } else return;
  } else return;
  e.preventDefault();
  next?.focus();
}

/**
 * One option: a native checkbox (or radio) inside its label, the name and the count.
 * @param {{field: string, value: string, label: string, count?: number | null, checked: boolean, disabled?: boolean,
 *   radio?: string, hint?: string, group?: string, mixed?: boolean, cls?: string, onChange: () => void}} o
 */
export function optionRow(o) {
  const input = /** @type {HTMLInputElement} */ (h("input", {
    type: o.radio ? "radio" : "checkbox",
    name: o.radio,
    checked: o.checked,
    disabled: o.disabled ?? false,
    "aria-checked": o.mixed ? "mixed" : null,
    dataset: { field: o.field, value: o.value, group: o.group, fkey: `opt:${o.field}:${o.value}` },
    "on:change": o.onChange,
  }));
  input.indeterminate = o.mixed ?? false;
  return h(
    "label",
    { class: ["dd-opt", o.disabled && "off", o.cls] },
    input,
    h("span", { class: "dd-opt-text" }, untrusted(o.label, "dd-opt-label"), o.hint ? h("span", { class: "dd-opt-hint", text: o.hint }) : null),
    o.count != null ? h("span", { class: "dd-opt-count", text: formatCount(o.count) }) : null,
  );
}
