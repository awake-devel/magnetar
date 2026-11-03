// A one-line notice across a view: offline, cached, plain HTTP, older server.

import { h } from "../dom.js";

/**
 * @param {import("../dom.js").Children} text
 * @param {{kind?: "info" | "error" | "warn", action?: {label: string, onClick: () => void}}} [opts]
 */
export function banner(text, opts = {}) {
  const kind = opts.kind ?? "info";
  return h(
    "div",
    { class: ["banner", `banner-${kind}`], role: kind === "error" ? "alert" : "status" },
    h("span", { class: "banner-text" }, text),
    opts.action
      ? h("button", { type: "button", class: "btn text", text: opts.action.label, "on:click": opts.action.onClick })
      : null,
  );
}

/**
 * A centred message with an optional button (empty and error states).
 * @param {import("../dom.js").Children} text
 * @param {{label: string, onClick: () => void}} [action]
 */
export function centered(text, action) {
  return h(
    "div",
    { class: "centered" },
    h("p", null, text),
    action ? h("button", { type: "button", class: "btn tonal", text: action.label, "on:click": action.onClick }) : null,
  );
}

/** A thin indeterminate progress bar. */
export function progressBar() {
  return h("div", { class: "progress", role: "progressbar", "aria-label": "Loading" });
}
