// Unknown paths.

import { h, link, routeLink } from "../dom.js";

/**
 * @param {HTMLElement} el
 * @param {import("../types.js").ViewArgs} _args
 * @returns {import("../types.js").ViewHandle}
 */
export function mount(el, _args) {
  el.append(
    h(
      "section",
      { class: "centered page-pad" },
      h("h1", { class: "page-title", text: "Not found" }),
      h("p", { text: "This page does not exist." }),
      routeLink(link("search"), { class: "btn tonal" }, "Go to Search"),
    ),
  );
  return { unmount() {} };
}
