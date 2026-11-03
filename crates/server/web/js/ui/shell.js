// The app frame (docs/WEB.md "Layout"): a left rail (medium, wide) or a bottom tab bar (phone)
// with Search, Saved, Stats and Settings; global banners; the view outlet; the snackbar host.

import { h, link, routeLink } from "../dom.js";
import { icon, logo } from "../icons.js";

export const TABS = Object.freeze([
  { nav: "search", label: "Search", icon: "search", route: "search" },
  { nav: "saved", label: "Saved", icon: "saved", route: "saved" },
  { nav: "stats", label: "Stats", icon: "stats", route: "stats" },
  { nav: "settings", label: "Settings", icon: "settings", route: "settings" },
]);

/**
 * @param {HTMLElement} root
 * @param {{searchHref: () => string}} deps the Search tab returns to the last search
 */
export function createShell(root, deps) {
  /** @type {Map<string, HTMLAnchorElement>} */
  const tabLinks = new Map();
  const items = TABS.map((t) => {
    const a = /** @type {HTMLAnchorElement} */ (routeLink(
      link(t.route),
      { class: "nav-item", dataset: { nav: t.nav } },
      icon(t.icon),
      h("span", { class: "nav-label", text: t.label }),
    ));
    if (t.nav === "search") {
      // Keep the search text and filters when coming back to the tab.
      a.addEventListener("click", () => {
        const href = deps.searchHref();
        if (href.startsWith("/") && !href.startsWith("//")) a.setAttribute("href", href);
      }, { capture: true });
    }
    tabLinks.set(t.nav, a);
    return a;
  });
  const nav = h(
    "nav",
    { class: "nav", "aria-label": "Main" },
    h("div", { class: "nav-brand", "aria-hidden": "true" }, logo()),
    ...items,
  );
  const banners = h("div", { class: "global-banners" });
  const outlet = h("main", { id: "outlet", class: "outlet" });
  const snackHost = h("div", { class: "snackbar-host" });
  const frame = h("div", { class: "shell" }, nav, h("div", { class: "shell-main" }, banners, outlet), snackHost);
  root.replaceChildren(frame);

  return {
    outlet,
    snackHost,
    /** @param {string | undefined} active */
    setActive(active) {
      for (const [k, a] of tabLinks) {
        if (k === active) a.setAttribute("aria-current", "page");
        else a.removeAttribute("aria-current");
      }
    },
    /** @param {boolean} on */
    setNavVisible(on) {
      frame.classList.toggle("no-nav", !on);
    },
    /** @param {"phone" | "medium" | "wide"} layout */
    setLayout(layout) {
      frame.dataset.layout = layout;
      document.documentElement.dataset.layout = layout;
    },
    /** @param {Node[]} list */
    setBanners(list) {
      banners.replaceChildren(...list);
    },
  };
}

/** @typedef {ReturnType<typeof createShell>} Shell */

/**
 * A view's top bar: optional Back button, a heading (app text, or an untrusted() node), actions.
 * @param {import("../types.js").Ctx} ctx
 * @param {{title: string | Node, back?: boolean, actions?: (Node | null)[]}} opts
 */
export function pageHeader(ctx, opts) {
  return h(
    "header",
    { class: "page-header" },
    opts.back
      ? h("button", {
        type: "button",
        class: "btn icon-only",
        "aria-label": "Back",
        "on:click": () => ctx.back(),
      }, icon("back"))
      : null,
    h("h1", { class: "page-title" }, opts.title),
    h("div", { class: "page-actions" }, ...(opts.actions ?? [])),
  );
}

/**
 * An icon-only button with its accessible name.
 * @param {string} name icon
 * @param {string} label aria-label
 * @param {() => void} onClick
 * @param {{badge?: number, pressed?: boolean, cls?: string}} [opts]
 */
export function iconButton(name, label, onClick, opts = {}) {
  return h(
    "button",
    {
      type: "button",
      class: ["btn", "icon-only", opts.cls],
      "aria-label": opts.badge ? `${label} (${opts.badge} active)` : label,
      title: label,
      "aria-pressed": opts.pressed == null ? null : String(opts.pressed),
      "on:click": onClick,
    },
    icon(name),
    opts.badge ? h("span", { class: "badge", "aria-hidden": "true", text: String(opts.badge) }) : null,
  );
}
