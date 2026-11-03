// The Saved tab, `/saved` (SavedScreen.kt, SavedViewModel.kt, D77): the server's saved searches
// by name, a summary of each, Run, the Notify switch (alerts go out through the server's ntfy
// notifier), Edit (a web addition) and Delete. Names are untrusted text.

import { ApiError, isAborted, isOffline, userMessage } from "../api.js";
import { h, untrusted } from "../dom.js";
import { cachedAgo, formatAgo } from "../format.js";
import * as prefs from "../prefs.js";
import { describe, sortByName } from "../saved-model.js";
import { buildQuery, supportedBy, supportedSort } from "../search-model.js";
import { banner, centered, progressBar } from "../ui/banner.js";
import { openDialog } from "../ui/dialog.js";
import { iconButton, pageHeader } from "../ui/shell.js";
import { openSavedEditDialog } from "./saved-edit-dialog.js";
import { runSearch, searchController } from "./search.js";

/** @typedef {import("../types.js").SavedSearch} SavedSearch */

/**
 * @param {HTMLElement} el
 * @param {import("../types.js").ViewArgs} args
 * @returns {import("../types.js").ViewHandle}
 */
export function mount(el, { ctx }) {
  /** @type {SavedSearch[]} */
  let searches = [];
  let loading = true;
  let loaded = false;
  /** @type {string | null} */
  let error = null;
  /** @type {number | null} */
  let cachedAt = null;
  let life = new AbortController();
  /** The list as last shown, for an offline visit. */
  const persist = () => {
    if (loaded) void ctx.cache.put("saved", {}, searches);
  };

  const header = pageHeader(ctx, { title: "Saved searches", actions: [iconButton("refresh", "Refresh", () => void load())] });
  const body = h("div", { class: "saved page-pad" });
  el.append(header, body);

  const canEdit = () => ctx.health.has("saved_search_poll");

  async function load() {
    life.abort();
    life = new AbortController();
    const signal = life.signal;
    loading = true;
    error = null;
    render();
    /** @param {unknown} list */
    const clean = (list) => sortByName(Array.isArray(list) ? list.filter((s) => s && Number.isSafeInteger(s.id)) : []);
    if (!loaded) {
      const c = await ctx.cache.get("saved", {});
      if (signal.aborted) return;
      if (c?.json && !loaded) {
        searches = clean(c.json);
        cachedAt = c.savedAt;
        render();
      }
    }
    try {
      const list = await ctx.api.get("/api/v1/saved-searches", { cls: "DETAIL", signal });
      if (isAborted(list) || signal.aborted) return;
      await ctx.health.refresh();
      searches = clean(list);
      void ctx.cache.put("saved", {}, searches);
      loaded = true;
      cachedAt = null;
    } catch (e) {
      if (signal.aborted) return;
      error = userMessage(e);
      if (!isOffline(e)) {
        searches = [];
        cachedAt = null;
      } else if (cachedAt != null) {
        error = `Offline: ${error}. Showing the cached page.`;
      }
    }
    loading = false;
    render();
  }

  /** @param {SavedSearch} s */
  function replace(s) {
    searches = searches.map((x) => (x.id === s.id ? s : x));
    persist();
  }

  /**
   * @param {SavedSearch} s
   * @param {boolean} notify
   */
  async function setNotify(s, notify) {
    if (!canEdit()) return;
    replace({ ...s, notify });
    render();
    try {
      const r = await ctx.api.put(`/api/v1/saved-searches/${s.id}`, { ...s, notify }, { cls: "DETAIL" });
      if (r && typeof r === "object" && r.id === s.id) replace(r);
    } catch (e) {
      replace(s);
      ctx.snackbar.show(`Not changed: ${userMessage(e)}`);
    }
    render();
  }

  /** The current Search tab state as a SearchQuery. */
  function currentQuery() {
    const st = searchController(ctx).get().state;
    const f = ctx.health.features();
    return buildQuery(st.text, supportedBy(st.filters, f), supportedSort(st.sort, f), st.descending, prefs.showAdult());
  }

  /** @param {SavedSearch} s */
  function edit(s) {
    openSavedEditDialog({
      search: s,
      canNotify: canEdit(),
      currentSummary: describe(currentQuery()),
      phone: ctx.layout === "phone",
      onSave: async (name, notify, replaceQuery) => {
        const body = { ...s, name, notify, query: replaceQuery ? currentQuery() : s.query };
        try {
          const r = await ctx.api.put(`/api/v1/saved-searches/${s.id}`, body, { cls: "DETAIL" });
          replace(r && typeof r === "object" && r.id === s.id ? r : body);
          searches = sortByName(searches);
          render();
          ctx.snackbar.show(`Changed “${name}”`);
          return true;
        } catch (e) {
          ctx.snackbar.show(`Not changed: ${userMessage(e)}`);
          return false;
        }
      },
    });
  }

  /** @param {SavedSearch} s */
  function remove(s) {
    const name = s.name ?? "";
    openDialog({
      title: "Delete saved search",
      titleNode: h("span", null, "Delete “", untrusted(name), "”?"),
      body: h("p", { text: "The search and its Notify setting are removed from the server." }),
      actions: [
        { label: "Cancel", kind: "text" },
        {
          label: "Delete",
          kind: "danger",
          onClick: async () => {
            try {
              await ctx.api.del(`/api/v1/saved-searches/${s.id}`, { cls: "DETAIL" });
              searches = searches.filter((x) => x.id !== s.id);
              persist();
              ctx.snackbar.show(`Deleted “${name}”`);
            } catch (e) {
              if (e instanceof ApiError && e.kind === "not_found") searches = searches.filter((x) => x.id !== s.id);
              else ctx.snackbar.show(`Not deleted: ${userMessage(e)}`);
            }
            render();
          },
        },
      ],
    });
  }

  /** @param {SavedSearch} s */
  function row(s) {
    const id = `saved-${s.id}`;
    const name = s.name ?? "";
    const notify = /** @type {HTMLInputElement} */ (h("input", {
      type: "checkbox",
      class: "switch",
      role: "switch",
      id: `${id}-notify`,
      checked: s.notify === true,
      disabled: !canEdit(),
      "aria-label": `Notify for ${name}`,
      dataset: { fkey: `${id}-notify` },
    }));
    notify.addEventListener("change", () => void setNotify(s, notify.checked));
    const run = h(
      "button",
      {
        type: "button",
        class: "saved-run",
        "aria-describedby": `${id}-sum`,
        "on:click": () => runSearch(ctx, s.query ?? {}),
      },
      h("span", { class: "saved-name" }, untrusted(name)),
      h("span", { class: "saved-summary muted", id: `${id}-sum` }, untrusted(describe(s.query))),
      s.last_checked != null ? h("span", { class: "muted small", text: `checked ${formatAgo(s.last_checked)}` }) : null,
    );
    return h(
      "li",
      { class: "saved-item" },
      run,
      h(
        "div",
        { class: "saved-actions" },
        h("label", { class: "notify-label", for: `${id}-notify` }, h("span", { class: "small", text: "Notify" }), notify),
        canEdit()
          ? h("button", { type: "button", class: "btn text", text: "Edit", "aria-label": `Edit ${name}`, "on:click": () => edit(s) })
          : null,
        h("button", { type: "button", class: "btn text danger", text: "Delete", "aria-label": `Delete ${name}`, "on:click": () => remove(s) }),
      ),
    );
  }

  function render() {
    const active = document.activeElement;
    const focusKey = active instanceof HTMLElement && body.contains(active) ? active.dataset.fkey : undefined;
    /** @type {Node[]} */
    const out = [];
    if (loading) out.push(progressBar());
    if (cachedAt != null && error == null) out.push(banner(cachedAgo(cachedAt)));
    if (loaded && error == null) {
      if (!canEdit()) out.push(banner("Notifications and editing need a newer server (saved_search_poll)."));
      else if (!ctx.session.info().ntfy) out.push(banner("The server has no ntfy topic: Notify sends nothing yet."));
    }
    if (error != null && searches.length === 0) {
      out.push(centered(error, { label: "Retry", onClick: () => void load() }));
    } else if (error != null) {
      out.push(banner(error, { kind: "error", action: { label: "Retry", onClick: () => void load() } }));
    }
    if (loaded && searches.length === 0 && error == null) {
      out.push(centered("No saved searches. Use the star on the Search tab to save one."));
    }
    if (searches.length > 0) {
      out.push(h("p", { class: "muted small", text: "Alerts for searches with Notify on are sent by your server through ntfy. This page uses no browser push." }));
      out.push(h("ul", { class: "saved-list" }, ...searches.map(row)));
    }
    body.replaceChildren(...out);
    if (focusKey) {
      const again = body.querySelector(`[data-fkey="${CSS.escape(focusKey)}"]`);
      if (again instanceof HTMLElement) again.focus();
    }
  }

  void load();
  return {
    unmount() {
      life.abort();
    },
  };
}
