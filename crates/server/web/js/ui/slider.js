// A labelled range input.

import { h } from "../dom.js";

let counter = 0;

/**
 * @param {{label: string, min: number, max: number, step: number, value: number,
 *   valueText?: (v: number) => string, onInput: (v: number) => void, onChange?: (v: number) => void}} opts
 */
export function slider(opts) {
  const id = `sl-${++counter}`;
  const input = /** @type {HTMLInputElement} */ (h("input", {
    type: "range",
    id,
    min: String(opts.min),
    max: String(opts.max),
    step: String(opts.step),
    value: String(opts.value),
    class: "slider",
    "aria-label": opts.label,
  }));
  if (opts.valueText) input.setAttribute("aria-valuetext", opts.valueText(opts.value));
  input.addEventListener("input", () => {
    const v = Number(input.value);
    if (opts.valueText) input.setAttribute("aria-valuetext", opts.valueText(v));
    opts.onInput(v);
  });
  if (opts.onChange) {
    const cb = opts.onChange;
    input.addEventListener("change", () => cb(Number(input.value)));
  }
  return {
    el: input,
    /** @param {number} v */
    set(v) {
      input.value = String(v);
      if (opts.valueText) input.setAttribute("aria-valuetext", opts.valueText(v));
    },
  };
}
