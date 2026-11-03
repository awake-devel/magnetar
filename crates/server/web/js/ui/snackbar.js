// Short messages at the bottom (an aria-live region), with an optional action.

import { h } from "../dom.js";

/**
 * @typedef {object} SnackOpts
 * @property {{label: string, onClick: () => void}} [action]
 * @property {number} [duration] ms; 0 keeps it until dismissed
 */

/** @param {HTMLElement} host */
export function createSnackbar(host) {
  host.classList.add("snackbar-host");
  host.setAttribute("aria-live", "polite");
  host.setAttribute("role", "status");
  /** @type {ReturnType<typeof setTimeout> | null} */
  let timer = null;

  function dismiss() {
    if (timer) clearTimeout(timer);
    timer = null;
    host.replaceChildren();
  }

  /**
   * @param {string} text
   * @param {SnackOpts} [opts]
   */
  function show(text, opts = {}) {
    dismiss();
    const bar = h(
      "div",
      { class: "snackbar" },
      h("span", { class: "snackbar-text", text }),
      opts.action
        ? h("button", {
          type: "button",
          class: "btn text snackbar-action",
          text: opts.action.label,
          "on:click": () => {
            dismiss();
            opts.action?.onClick();
          },
        })
        : null,
      h("button", {
        type: "button",
        class: "btn icon-only snackbar-close",
        "aria-label": "Dismiss",
        text: "×",
        "on:click": dismiss,
      }),
    );
    host.appendChild(bar);
    const d = opts.duration ?? (opts.action ? 10_000 : 4_000);
    if (d > 0) timer = setTimeout(dismiss, d);
  }

  return { show, dismiss };
}

/** @typedef {ReturnType<typeof createSnackbar>} Snackbar */
