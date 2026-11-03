// In-page dialogs on <dialog> with showModal(): the browser traps focus and Esc closes. Never
// window.alert, confirm or prompt.

import { h } from "../dom.js";

/**
 * @typedef {object} DialogAction
 * @property {string} label
 * @property {"primary" | "danger" | "text"} [kind]
 * @property {(d: DialogHandle) => void | boolean | Promise<void | boolean>} [onClick] return false to keep it open
 * @property {boolean} [disabled]
 */

/**
 * @typedef {object} DialogOpts
 * @property {string} title never API text unless passed through untrusted() as `titleNode`
 * @property {Node} [titleNode]
 * @property {import("../dom.js").Children} [body]
 * @property {DialogAction[]} [actions]
 * @property {string} [cls] "sheet" (bottom sheet), "drawer" (side), "full" (full screen on phones)
 * @property {() => void} [onClose] called once, however it closed
 * @property {boolean} [lightDismiss] a click on the backdrop closes it (default true)
 * @property {boolean} [closeButton] an "×" Close button next to the title
 */

/**
 * @typedef {object} DialogHandle
 * @property {HTMLDialogElement} el
 * @property {HTMLElement} body
 * @property {HTMLElement} footer
 * @property {() => void} close
 * @property {(busy: boolean) => void} setBusy while busy: buttons disabled, a progress bar, Esc and the backdrop do nothing
 * @property {() => boolean} busy
 */

let counter = 0;

/**
 * @param {DialogOpts} opts
 * @returns {DialogHandle}
 */
export function openDialog(opts) {
  const id = `dlg-${++counter}`;
  const progress = h("div", { class: "progress", hidden: true, role: "progressbar", "aria-label": "Working" });
  const body = h("div", { class: "dialog-body" });
  if (opts.body != null) {
    const parts = Array.isArray(opts.body) ? opts.body : [opts.body];
    for (const p of parts) {
      if (p == null || p === false) continue;
      body.append(typeof p === "string" || typeof p === "number" ? String(p) : /** @type {Node} */ (p));
    }
  }
  const footer = h("div", { class: "dialog-actions" });
  const heading = h("h2", { class: "dialog-title", id: `${id}-t` }, opts.titleNode ?? opts.title);
  let isBusy = false;
  let closed = false;
  const head = h(
    "div",
    { class: "dialog-head" },
    heading,
    opts.closeButton
      ? h("button", {
        type: "button",
        class: "btn icon-only dialog-close",
        "aria-label": "Close",
        text: "×",
        "on:click": () => {
          if (!isBusy) handle.close();
        },
      })
      : null,
  );
  const el = /** @type {HTMLDialogElement} */ (h(
    "dialog",
    { class: ["dialog", opts.cls], "aria-labelledby": `${id}-t` },
    h("div", { class: "dialog-surface" }, progress, head, body, footer),
  ));

  /** @type {DialogHandle} */
  const handle = {
    el,
    body,
    footer,
    close() {
      if (closed) return;
      closed = true;
      if (el.open) el.close();
      el.remove();
      opts.onClose?.();
    },
    setBusy(b) {
      isBusy = b;
      progress.hidden = !b;
      el.classList.toggle("busy", b);
      for (const btn of el.querySelectorAll("button, input, select, textarea")) {
        const c = /** @type {HTMLButtonElement} */ (btn);
        if (b) {
          if (!c.disabled) {
            c.dataset.busyDisabled = "1";
            c.disabled = true;
          }
        } else if (c.dataset.busyDisabled) {
          delete c.dataset.busyDisabled;
          c.disabled = false;
        }
      }
    },
    busy: () => isBusy,
  };

  for (const a of opts.actions ?? []) {
    const btn = h("button", {
      type: "button",
      class: ["btn", a.kind ?? "text"],
      text: a.label,
      disabled: a.disabled ?? false,
      "on:click": async () => {
        if (isBusy) return;
        const r = a.onClick ? await a.onClick(handle) : undefined;
        if (r !== false) handle.close();
      },
    });
    footer.appendChild(btn);
  }

  el.addEventListener("cancel", (e) => {
    if (isBusy) e.preventDefault();
  });
  el.addEventListener("close", () => handle.close());
  if (opts.lightDismiss !== false) {
    el.addEventListener("click", (e) => {
      if (e.target === el && !isBusy) handle.close();
    });
  }
  document.body.appendChild(el);
  el.showModal();
  return handle;
}

/**
 * A yes/no question in a dialog.
 * @param {{title: string, text?: import("../dom.js").Children, confirm: string, cancel?: string, danger?: boolean}} opts
 * @returns {Promise<boolean>}
 */
export function confirmDialog(opts) {
  return new Promise((resolve) => {
    let answer = false;
    openDialog({
      title: opts.title,
      body: opts.text != null ? h("p", null, opts.text) : null,
      actions: [
        { label: opts.cancel ?? "Cancel", kind: "text" },
        {
          label: opts.confirm,
          kind: opts.danger ? "danger" : "primary",
          onClick: () => {
            answer = true;
          },
        },
      ],
      onClose: () => resolve(answer),
    });
  });
}
