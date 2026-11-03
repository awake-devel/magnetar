// Settings (SettingsScreen.kt, adapted to the web; docs/WEB.md): the server and this browser's
// session, the trust root and threshold (stored on the server), the adult switch (this
// browser), saved-search alerts (the server's ntfy notifier), and the attributions.

import { ApiError, userMessage } from "../api.js";
import { externalUrl, h } from "../dom.js";
import { formatTimestamp, shortKey } from "../format.js";
import { npub, pubkeyHex } from "../nip19.js";
import * as prefs from "../prefs.js";
import { rootInputOf, rootKeyInput } from "../settings-checks.js";
import { patch } from "../store.js";
import { confirmDialog } from "../ui/dialog.js";
import { pageHeader } from "../ui/shell.js";
import { slider } from "../ui/slider.js";

/** TMDB API Terms of Use §3 notice (TmdbAttribution.kt). */
export const TMDB_NOTICE =
  "This product uses TMDB and the TMDB APIs but is not endorsed, certified, or otherwise approved by TMDB.";
export const IMDB_NOTICE = "Information courtesy of IMDb (https://www.imdb.com). Used with permission.";

/**
 * "Connected: Magnetar X" and what an older server lacks (SettingsViewModel.runTest).
 * @param {{version?: string, features?: string[]}} health
 */
export function connectedText(health) {
  const f = new Set(health.features ?? []);
  const missing = [
    f.has("saved_search_poll") ? null : "saved-search alerts",
    f.has("stats") ? null : "charts",
    f.has("graphs") ? null : "graphs",
  ].filter(Boolean);
  let s = `Connected: Magnetar ${health.version || "?"}`;
  if (missing.length > 0) s += `. This server is older than the app: no ${missing.join(", ")}`;
  return s;
}

/**
 * The web asset version from this module's own URL (/_app/<VERSION>/js/...).
 */
function webVersion() {
  try {
    const parts = new URL(import.meta.url).pathname.split("/");
    return parts[1] === "_app" ? parts[2] : "dev";
  } catch {
    return "dev";
  }
}

/**
 * @param {HTMLElement} el
 * @param {import("../types.js").ViewArgs} args
 * @returns {import("../types.js").ViewHandle}
 */
export function mount(el, { ctx }) {
  /** @type {(() => void)[]} */
  const cleanups = [];

  // --- Server -----------------------------------------------------------------------------
  const versionLine = h("p", { class: "settings-version" });
  const testResult = h("p", { class: "test-result", role: "status" });
  const testBtn = /** @type {HTMLButtonElement} */ (h("button", { type: "button", class: "btn tonal", text: "Test connection" }));
  function renderVersion() {
    const v = ctx.health.version();
    versionLine.textContent = `Magnetar ${v || "?"} · web ${webVersion()}`;
  }
  testBtn.addEventListener("click", async () => {
    testBtn.disabled = true;
    testBtn.textContent = "Testing…";
    testResult.textContent = "";
    testResult.className = "test-result";
    try {
      const health = await ctx.api.get("/health", { cls: "DETAIL" });
      try {
        await ctx.api.get("/api/v1/settings/trust", { cls: "DETAIL" });
        testResult.textContent = "✓ " + connectedText(health ?? {});
        testResult.classList.add("ok");
      } catch (e) {
        testResult.textContent = `✗ The server answers (Magnetar ${health?.version ?? "?"}), but: ${userMessage(e)}`;
        testResult.classList.add("bad");
      }
      void ctx.health.refresh({ force: true });
    } catch (e) {
      testResult.textContent = `✗ ${userMessage(e)}`;
      testResult.classList.add("bad");
    } finally {
      testBtn.disabled = false;
      testBtn.textContent = "Test connection";
    }
  });
  renderVersion();
  cleanups.push(ctx.health.subscribe(renderVersion));

  // --- Session ----------------------------------------------------------------------------
  const sessionLine = h("p");
  function renderSession() {
    const i = ctx.session.info();
    const parts = [];
    if (i.createdAt != null) parts.push(`Signed in since ${formatTimestamp(i.createdAt)}`);
    if (i.expiresAt != null) parts.push(`expires ${formatTimestamp(i.expiresAt)}`);
    sessionLine.textContent = parts.length > 0 ? parts.join(", ") : "Signed in";
  }
  renderSession();
  cleanups.push(ctx.session.subscribe(renderSession));
  const signOut = h("button", {
    type: "button",
    class: "btn tonal",
    text: "Sign out",
    "on:click": () => void ctx.session.logout(false),
  });
  const signOutAll = h("button", {
    type: "button",
    class: "btn text danger",
    text: "Sign out of every browser",
    "on:click": async () => {
      const yes = await confirmDialog({
        title: "Sign out of every browser?",
        text: "Every browser signed in to this server with the web page is signed out. The app and the crawl workers are not affected.",
        confirm: "Sign out everywhere",
        danger: true,
      });
      if (yes) await ctx.session.logout(true);
    },
  });

  // --- Trust ------------------------------------------------------------------------------
  /** @type {{threshold: number, root: string | null} | null} */
  let server = null;
  let root = rootInputOf("");
  let threshold = 20;
  let saving = false;
  /** @type {string | null} */
  let serverError = null;
  const rootInput = /** @type {HTMLInputElement} */ (h("input", {
    type: "text",
    id: "trust-root",
    class: "field-input",
    autocomplete: "off",
    spellcheck: "false",
    "aria-describedby": "trust-root-hint",
  }));
  rootInput.setAttribute("autocapitalize", "off");
  const rootHint = h("p", { id: "trust-root-hint", class: "field-hint" });
  const thresholdLabel = h("p", { class: "field-label" });
  const thresholdOf = h("span", { class: "muted" });
  const thresholdSlider = slider({
    label: "Trust threshold",
    min: 0,
    max: 100,
    step: 1,
    value: threshold,
    valueText: (v) => `${v} of 100`,
    onInput: (v) => {
      threshold = Math.round(v);
      renderTrust();
    },
  });
  const serverLine = h("p", { class: "field-hint server-trust" });
  const dirtyLine = h("span", { class: "muted" });
  const saveBtn = /** @type {HTMLButtonElement} */ (h("button", { type: "button", class: "btn primary", text: "Save" }));

  rootInput.addEventListener("input", () => {
    root = rootKeyInput(root, rootInput.value);
    // A private key never stays in the field, not even unsaved.
    if (rootInput.value !== root.value) rootInput.value = root.value;
    renderTrust();
  });

  function savedRootHex() {
    return server?.root ?? null;
  }

  function dirty() {
    if (!server) return false;
    const hex = root.value.trim() === "" ? null : pubkeyHex(root.value);
    return threshold !== server.threshold || hex !== savedRootHex();
  }

  function renderTrust() {
    rootHint.textContent = root.check.error ??
      "npub, nprofile or hex. Never your nsec: trust is computed from the accounts it follows.";
    rootHint.classList.toggle("error", root.check.error != null);
    rootInput.setAttribute("aria-invalid", root.check.error ? "true" : "false");
    thresholdLabel.textContent = `Trust threshold: ${threshold}`;
    thresholdOf.textContent = `${threshold} of 100`;
    if (serverError) serverLine.textContent = `Server trust settings: ${serverError}`;
    else if (server) {
      serverLine.textContent = `On the server: threshold ${server.threshold}, root ` +
        (server.root ? shortKey(npub(server.root)) : "none");
    } else serverLine.textContent = "";
    dirtyLine.textContent = dirty() ? "Unsaved changes" : "";
    saveBtn.textContent = saving ? "Saving…" : "Save";
    saveBtn.disabled = saving || !server || root.check.error != null;
  }

  saveBtn.addEventListener("click", async () => {
    if (root.check.error) return;
    const hex = root.value.trim() === "" ? null : pubkeyHex(root.value);
    if (root.value.trim() !== "" && hex == null) return;
    saving = true;
    renderTrust();
    try {
      const t = await ctx.api.put("/api/v1/settings/trust", { root: hex, threshold }, { cls: "DETAIL" });
      server = { threshold: typeof t?.threshold === "number" ? t.threshold : threshold, root: t?.root ?? hex };
      serverError = null;
      patch(ctx.store, { trust: server });
      ctx.snackbar.show("Saved · trust settings sent to the server");
    } catch (e) {
      ctx.snackbar.show(e instanceof ApiError && e.kind === "http" && e.message ? e.message : `Not saved: ${userMessage(e)}`);
    } finally {
      saving = false;
      renderTrust();
    }
  });

  async function loadServerTrust() {
    try {
      const t = await ctx.api.get("/api/v1/settings/trust", { cls: "DETAIL" });
      server = { threshold: typeof t?.threshold === "number" ? t.threshold : 20, root: typeof t?.root === "string" ? t.root : null };
      serverError = null;
      threshold = server.threshold;
      thresholdSlider.set(threshold);
      root = rootInputOf(server.root ? npub(server.root) : "");
      rootInput.value = root.value;
      patch(ctx.store, { trust: server });
    } catch (e) {
      serverError = userMessage(e);
    }
    renderTrust();
  }
  renderTrust();
  void loadServerTrust();

  // --- Content ----------------------------------------------------------------------------
  const adult = /** @type {HTMLInputElement} */ (h("input", {
    type: "checkbox",
    role: "switch",
    id: "show-adult",
    class: "switch",
    checked: prefs.showAdult(),
  }));
  adult.addEventListener("change", () => prefs.setShowAdult(adult.checked));

  // --- Notifications ----------------------------------------------------------------------
  const notifyText = h("p", { class: "field-hint" });
  function renderNotify() {
    notifyText.textContent = ctx.session.info().ntfy
      ? "Your server sends alerts for saved searches with Notify on through ntfy (checked every 15 minutes). Subscribe to its topic in the ntfy app. This page uses no browser push."
      : "ntfy is not configured on this server (MAGNETAR_NTFY_URL, MAGNETAR_NTFY_TOPIC): Notify switches are stored but nothing is sent.";
  }
  renderNotify();
  cleanups.push(ctx.session.subscribe(renderNotify));

  /**
   * @param {string} title
   * @param {...import("../dom.js").Children} children
   */
  const card = (title, ...children) =>
    h("section", { class: "card settings-card" }, h("h2", { class: "card-title", text: title }), ...children);

  const tmdb = externalUrl("tmdb");
  const dbip = externalUrl("dbip");
  el.append(
    pageHeader(ctx, { title: "Settings" }),
    h(
      "div",
      { class: "settings page-pad" },
      card(
        "Server",
        h("p", { text: `This server: ${location.origin}` }),
        versionLine,
        h("div", { class: "row-actions" }, testBtn),
        testResult,
      ),
      card(
        "Session",
        sessionLine,
        h("p", {
          class: "field-hint",
          text: "This browser holds a session cookie the page cannot read. The API token is not stored here.",
        }),
        h("div", { class: "row-actions" }, signOut, signOutAll),
      ),
      card(
        "Trust",
        h("label", { for: "trust-root", class: "field-label", text: "Trust root (your public key)" }),
        rootInput,
        rootHint,
        h("p", { class: "field-hint", text: "Amber (NIP-55) signing is Android-only: paste your npub here." }),
        h("div", { class: "filter-row" }, thresholdLabel, thresholdOf),
        thresholdSlider.el,
        h("p", {
          class: "field-hint",
          text: "Publishers scoring below it are hidden from search. The server applies it to every client.",
        }),
        serverLine,
        h("div", { class: "row-actions" }, dirtyLine, saveBtn),
      ),
      card(
        "Content",
        h(
          "label",
          { class: "switch-row", for: "show-adult" },
          h(
            "span",
            null,
            h("span", { class: "switch-title", text: "Show adult content" }),
            h("span", {
              class: "field-hint block",
              text:
                "Off: adult torrents stay out of search, browse, title pages, stats and saved-search alerts. They are hidden, not deleted.",
            }),
          ),
          adult,
        ),
      ),
      card("Notifications", h("p", { class: "switch-title", text: "Saved-search alerts" }), notifyText),
      card(
        "About",
        h("p", { text: "Magnetar web client. Torrent names, files and comments come from strangers: they are shown as plain text." }),
        h("p", { class: "field-hint" }, TMDB_NOTICE, " ", tmdb ? h("a", { href: tmdb }, "themoviedb.org") : null),
        h("p", { class: "field-hint", text: IMDB_NOTICE }),
        h(
          "p",
          { class: "field-hint" },
          "IP Geolocation by ",
          dbip ? h("a", { href: dbip }, "DB-IP") : "DB-IP",
          " (CC BY 4.0), on the crawl map.",
        ),
        h("p", { class: "field-hint", text: "Map: Natural Earth (public domain)." }),
      ),
    ),
  );

  return {
    unmount() {
      for (const c of cleanups) c();
    },
  };
}
