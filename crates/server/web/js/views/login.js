// Sign in (docs/WEB.md "Security model"): the API token is sent once to open a session cookie
// for this browser, then cleared from the field. It is never stored by this page.

import { ApiError, userMessage } from "../api.js";
import { h } from "../dom.js";
import { logo } from "../icons.js";
import { nextTarget } from "../router.js";
import { loadTrust } from "../session.js";
import { checkPageTransport, checkToken } from "../settings-checks.js";
import { banner } from "../ui/banner.js";

/**
 * @param {unknown} e
 * @returns {string}
 */
export function loginError(e) {
  if (e instanceof ApiError) {
    switch (e.kind) {
      case "unauthorized":
        return "Token rejected";
      case "rate_limited":
        return `Too many attempts: try again in ${e.retryAfter ?? 60} s`;
      case "network":
        return "Server unreachable";
      case "timeout":
        return "The server did not answer in time";
      case "not_found":
        return "This server has no web sign-in (MAGNETAR_WEB_UI is off)";
      default:
        return userMessage(e);
    }
  }
  return "Sign-in failed";
}

/**
 * @param {HTMLElement} el
 * @param {import("../types.js").ViewArgs} args
 * @returns {import("../types.js").ViewHandle}
 */
export function mount(el, { query, ctx }) {
  // Set by a 401 mid-session (main.js): kept in the URL until the next sign-in.
  const ended = query.get("ended") === "1";
  const input = /** @type {HTMLInputElement} */ (h("input", {
    type: "password",
    id: "login-token",
    name: "token",
    autocomplete: "off",
    spellcheck: "false",
    autocapitalize: null,
    class: "field-input",
    "aria-describedby": "login-hint login-error",
    required: true,
  }));
  input.setAttribute("autocapitalize", "off");
  const error = h("p", { id: "login-error", class: "field-error", role: "alert" });
  const button = /** @type {HTMLButtonElement} */ (h("button", { type: "submit", class: "btn primary", text: "Sign in" }));
  let sending = false;

  /** @param {SubmitEvent} e */
  async function submit(e) {
    e.preventDefault();
    if (sending) return;
    const token = input.value.trim();
    const c = checkToken(input.value);
    if (c.error) {
      error.textContent = c.error;
      return;
    }
    if (token === "") {
      error.textContent = "Enter the server's API token";
      return;
    }
    sending = true;
    button.disabled = true;
    button.textContent = "Signing in…";
    error.textContent = "";
    try {
      await ctx.session.login(token);
      input.value = "";
      void ctx.health.refresh({ force: true });
      ctx.health.startAutoRefresh();
      void loadTrust(ctx);
      ctx.nav(nextTarget(query.get("next")), { replace: true });
    } catch (err) {
      error.textContent = loginError(err);
      input.focus();
    } finally {
      sending = false;
      button.disabled = false;
      button.textContent = "Sign in";
    }
  }

  const transport = checkPageTransport(location);
  const form = h(
    "form",
    { class: "login-form", "on:submit": submit, autocomplete: "off" },
    h("label", { for: "login-token", class: "field-label", text: "API token" }),
    input,
    h("p", {
      id: "login-hint",
      class: "field-hint",
      text:
        "The server's MAGNETAR_API_TOKEN. It is sent once to open a session cookie for this browser and never stored by this page.",
    }),
    error,
    h("div", { class: "form-actions" }, button),
  );
  el.append(
    h(
      "section",
      { class: "login card" },
      h("div", { class: "login-brand" }, logo()),
      h("h1", { class: "page-title", text: "Sign in to Magnetar" }),
      ended ? banner("Your session ended: sign in again", { kind: "warn" }) : null,
      transport.warning ? banner(transport.warning, { kind: "warn" }) : null,
      form,
    ),
  );
  input.focus();
  return {
    unmount() {
      input.value = "";
    },
  };
}
