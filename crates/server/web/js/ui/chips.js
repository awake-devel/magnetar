// Filter chips: toggle buttons with aria-pressed, a count in parentheses.

import { formatCount } from "../format.js";
import { h } from "../dom.js";

/**
 * "Video (1,234)".
 * @param {string} label
 * @param {number | null | undefined} count
 */
export function chipText(label, count) {
  return label + (count != null ? ` (${formatCount(count)})` : "");
}

/**
 * @param {{label: string, count?: number | null, selected: boolean, disabled?: boolean,
 *   onToggle: () => void, dataset?: Record<string, string>}} opts
 */
export function chip(opts) {
  return h(
    "button",
    {
      type: "button",
      class: ["chip", opts.selected && "on"],
      "aria-pressed": opts.selected ? "true" : "false",
      disabled: opts.disabled ?? false,
      dataset: opts.dataset,
      "on:click": opts.onToggle,
    },
    opts.selected ? h("span", { class: "chip-check", "aria-hidden": "true", text: "✓" }) : null,
    h("span", { class: "chip-label", text: chipText(opts.label, opts.count) }),
  );
}

/**
 * @param {Node[]} chips
 * @param {string} [label] an accessible group name
 */
export function chipRow(chips, label) {
  return h("div", { class: "chips", role: "group", "aria-label": label }, ...chips);
}
