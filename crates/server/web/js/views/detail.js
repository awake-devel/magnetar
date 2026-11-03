// The torrent detail, `/torrent/:ih` (DetailScreen.kt, DetailViewModel.kt, docs/APP.md §1):
// mounted full page on phones and in the search workspace's pane on medium and wide layouts
// (ctx.pane). Media card, release attributes, magnet, swarm with "Check now" and the seed
// history, DHT or trust card, the "Detected" card with track corrections, files, trackers, tags,
// description, comments. Every string from the API is untrusted text.

import { ApiError, ihSegment, isAborted, isOffline, userMessage } from "../api.js";
import { h, magnetLink, untrusted } from "../dom.js";
import { cachedAgo, formatAge, formatBytes, formatTimestamp, peersText, shortKey, stripHtml } from "../format.js";
import { icon } from "../icons.js";
import { attrDetails, categoryLabel, sourceLabel } from "../labels.js";
import { npub } from "../nip19.js";
import { probeViews } from "../probe-format.js";
import { banner, centered, progressBar } from "../ui/banner.js";
import { iconButton, pageHeader } from "../ui/shell.js";
import { dhtCard } from "./dht-card.js";
import { fileTree } from "./file-tree.js";
import { mediaCard } from "./media-card.js";
import { probeCard, probeUiState } from "./probe-card.js";
import { swarmCard } from "./swarm-card.js";
import { trustCard } from "./trust-card.js";

/** @typedef {import("../types-stats.js").TorrentDetail} TorrentDetail */

/** Wait before asking once more for a poster or overview the server is still fetching (D42). */
export const MEDIA_RETRY_MS = 4_000;

/**
 * Copies text to the clipboard and says so.
 * @param {import("../types.js").Ctx} ctx
 * @param {string} label
 * @param {string} text
 */
export async function copyText(ctx, label, text) {
  const refused = `Couldn't copy the ${label.toLowerCase()}: the browser refused`;
  // Without a user gesture or focus some browsers leave the write pending: say so after a while
  // instead of nothing (a late success still says "copied").
  let toldRefused = false;
  const timer = setTimeout(() => {
    toldRefused = true;
    ctx.snackbar.show(refused);
  }, COPY_TIMEOUT_MS);
  try {
    if (!navigator.clipboard) throw new Error("no clipboard");
    await navigator.clipboard.writeText(text);
    ctx.snackbar.show(`${label} copied`);
  } catch {
    if (!toldRefused) ctx.snackbar.show(refused);
  } finally {
    clearTimeout(timer);
  }
}

/** How long a clipboard write may stay pending before the page says it failed. */
const COPY_TIMEOUT_MS = 3_000;

/**
 * @param {HTMLElement} el
 * @param {import("../types.js").ViewArgs} args
 * @returns {import("../types.js").ViewHandle}
 */
export function mount(el, { params, ctx }) {
  let ih = ihSegment(params.ih);
  /** @type {TorrentDetail | null} */
  let detail = null;
  /** @type {ApiError | null} */
  let error = null;
  let loading = true;
  /** @type {number | null} */
  let cachedAt = null;
  /** @type {import("../types-stats.js").LivenessHistory | null} */
  let history = null;
  let checking = false;
  let correcting = false;
  /** Bumped by every saved correction: an older detail fetch keeps the shown probe. */
  let probeGen = 0;
  let trustOpen = false;
  /** @type {{expanded: Set<string> | null}} */
  let filesState = { expanded: null };
  let probeUi = probeUiState();
  /** @type {AbortController} */
  let life = new AbortController();
  /** @type {ReturnType<typeof setTimeout> | null} */
  let retryTimer = null;

  const header = pageHeader(ctx, {
    title: "Torrent",
    back: !ctx.pane,
    actions: [iconButton("refresh", "Refresh", () => void load())],
  });
  const body = h("div", { class: "detail page-pad" });
  el.append(header, body);

  const cacheEndpoint = () => `detail/${ih}`;
  const threshold = () => ctx.store.get().trust?.threshold ?? 20;

  /**
   * @param {TorrentDetail} fetched
   * @param {number} gen
   */
  function withProbeSince(fetched, gen) {
    const shown = detail?.probe;
    return gen !== probeGen && shown != null ? { ...fetched, probe: shown } : fetched;
  }

  async function load() {
    life.abort();
    life = new AbortController();
    const signal = life.signal;
    if (retryTimer) clearTimeout(retryTimer);
    loading = true;
    error = null;
    render();
    if (detail == null) {
      const c = await ctx.cache.get(cacheEndpoint(), {});
      if (signal.aborted) return;
      if (c?.json && detail == null) {
        detail = c.json;
        cachedAt = c.savedAt;
        render();
      }
    }
    try {
      const gen = probeGen;
      const r = await ctx.api.get(`/api/v1/torrents/${ih}`, { cls: "DETAIL", signal });
      if (isAborted(r) || signal.aborted) return;
      detail = withProbeSince(r, gen);
      cachedAt = null;
      loading = false;
      void ctx.cache.put(cacheEndpoint(), {}, detail);
      render();
      void loadHistory(signal);
      scheduleMediaRetry(signal);
    } catch (e) {
      if (signal.aborted) return;
      error = e instanceof ApiError ? e : new ApiError("network", String(e));
      if (!isOffline(error)) {
        detail = null;
        cachedAt = null;
      }
      loading = false;
      render();
    }
  }

  /** @param {AbortSignal} signal */
  function scheduleMediaRetry(signal) {
    const m = detail?.media;
    if (!m || (m.poster != null && m.overview != null)) return;
    retryTimer = setTimeout(async () => {
      try {
        const gen = probeGen;
        const r = await ctx.api.get(`/api/v1/torrents/${ih}`, { cls: "DETAIL", signal });
        if (isAborted(r) || signal.aborted || !r?.media) return;
        detail = withProbeSince(r, gen);
        void ctx.cache.put(cacheEndpoint(), {}, detail);
        render();
      } catch {
        // The first answer stays.
      }
    }, MEDIA_RETRY_MS);
  }

  /** @param {AbortSignal} signal */
  async function loadHistory(signal) {
    await ctx.health.refresh();
    if (signal.aborted || !ctx.health.has("liveness_history")) {
      render();
      return;
    }
    try {
      const r = await ctx.api.get(`/api/v1/torrents/${ih}/liveness-history?days=90`, { cls: "DETAIL", signal });
      if (isAborted(r) || signal.aborted) return;
      history = r;
      render();
    } catch {
      // The chart stays out.
    }
  }

  async function checkNow() {
    if (checking) return;
    checking = true;
    render();
    const signal = life.signal;
    try {
      const live = await ctx.api.post(`/api/v1/torrents/${ih}/liveness`, undefined, { cls: "CHECK", signal });
      if (isAborted(live) || signal.aborted) return;
      if (detail) {
        const best = live?.seeders == null && live?.dht_peers_est == null ? null : Math.max(live.seeders ?? 0, live.dht_peers_est ?? 0);
        detail = {
          ...detail,
          liveness: live,
          summary: { ...detail.summary, seeders: live?.seeders ?? null, leechers: live?.leechers ?? null, best_peers: best, dead: best != null ? best === 0 : null },
        };
        void ctx.cache.put(cacheEndpoint(), {}, detail);
      }
      checking = false;
      render();
      void loadHistory(signal);
    } catch (e) {
      checking = false;
      render();
      if (signal.aborted) return;
      ctx.snackbar.show(
        e instanceof ApiError && e.kind === "rate_limited"
          ? "Check failed: too many seed checks right now; try again later"
          : `Check failed: ${userMessage(e)}`,
      );
    }
  }

  /**
   * Sends a correction call; the answer replaces the probe and the cached detail.
   * @param {string} message
   * @param {() => Promise<unknown>} call
   * @returns {Promise<boolean>}
   */
  async function correct(message, call) {
    if (correcting) return false;
    correcting = true;
    render();
    try {
      const probe = await call();
      if (isAborted(probe)) return false;
      probeGen++;
      if (detail) {
        detail = { ...detail, probe: /** @type {import("../types-stats.js").ProbeInfo} */ (probe) };
        void ctx.cache.put(cacheEndpoint(), {}, detail);
      }
      ctx.snackbar.show(message);
      return true;
    } catch (e) {
      ctx.snackbar.show(isOffline(e) ? "Offline: the correction was not saved" : `Couldn't save the correction: ${userMessage(e)}`);
      return false;
    } finally {
      correcting = false;
      render();
    }
  }

  /**
   * @param {import("../types-stats.js").ProbeCorrectionItem[]} items
   * @param {string} message
   */
  const saveCorrections = (items, message) =>
    items.length === 0 ? Promise.resolve(true) : correct(
      message,
      () => ctx.api.post(`/api/v1/torrents/${ih}/probe/corrections`, { device: "web", items }, { cls: "DETAIL" }),
    );

  /** @param {number} id */
  const undoCorrection = (id) =>
    Number.isSafeInteger(id)
      ? correct("Correction removed", () => ctx.api.del(`/api/v1/torrents/${ih}/probe/corrections/${id}`, { cls: "DETAIL" }))
      : Promise.resolve(false);

  /**
   * @param {string} title
   * @param {...import("../dom.js").Children} children
   */
  function section(title, ...children) {
    return h("section", { class: "detail-section" }, h("h3", { class: "section-title", text: title }), ...children);
  }

  function render() {
    // Keep focus on a re-rendered control with the same key.
    const active = document.activeElement;
    const focusKey = active instanceof HTMLElement && body.contains(active) ? active.dataset.fkey : undefined;
    /** @type {Node[]} */
    const out = [];
    if (loading && detail) out.push(progressBar());
    if (detail && error) {
      out.push(banner(
        (isOffline(error) ? "Offline: " : "") + userMessage(error) + (cachedAt != null ? ` · ${cachedAgo(cachedAt)}` : "") +
          (isOffline(error) ? ". Showing the cached page." : ""),
        { kind: "error" },
      ));
    } else if (detail && cachedAt != null) {
      out.push(banner(cachedAgo(cachedAt)));
    }
    if (!detail) {
      if (loading) out.push(progressBar());
      else {
        out.push(centered(error ? userMessage(error) : "Not on this server", { label: "Retry", onClick: () => void load() }));
      }
      body.replaceChildren(...out);
      return;
    }
    const d = detail;
    const s = d.summary;
    const magnet = magnetLink(d);
    const fileCount = (s.file_count ?? 0) > 0 ? s.file_count ?? 0 : (d.files ?? []).length;
    const meta = [
      formatBytes(s.total_size),
      fileCount === 1 ? "1 file" : `${fileCount} files`,
      peersText(s.seeders, s.leechers, s.best_peers),
    ];
    if (s.dead === true) meta.push("dead");
    if ((s.created_at ?? 0) > 0) meta.push(formatTimestamp(s.created_at ?? 0));
    const attrs = s.attrs ? attrDetails(s.attrs) : [];
    out.push(h(
      "div",
      { class: "detail-head" },
      h("p", { class: "cat-badge", text: categoryLabel(s.category ?? "other") }),
      h("h2", { class: "detail-title" }, untrusted(s.title ?? "")),
      h("p", { class: "detail-meta", text: meta.join(" · ") }),
      (d.category_path ?? []).length > 0 ? h("p", { class: "muted small" }, untrusted((d.category_path ?? []).join(" › "))) : null,
      attrs.length > 0 ? h("p", { class: "detail-attrs" }, untrusted(attrs.join(" · "))) : null,
      h(
        "div",
        { class: "row-actions magnet-actions" },
        magnet ? h("a", { href: magnet, class: "btn primary", dataset: { fkey: "open-magnet" } }, icon("magnet"), "Open magnet") : null,
        h("button", {
          type: "button",
          class: "btn tonal",
          dataset: { fkey: "copy-magnet" },
          "on:click": () => {
            if (magnet) void copyText(ctx, "Magnet", magnet.toString());
            else ctx.snackbar.show("No magnet in the answer");
          },
        }, icon("copy"), "Copy magnet"),
      ),
      magnet ? h("p", { class: "muted small", text: "Nothing happened? Copy the magnet into your torrent app." }) : null,
    ));
    if (d.media) out.push(mediaCard(d.media, { mediaReleases: ctx.health.has("media_releases") }));
    out.push(swarmCard({
      liveness: d.liveness,
      bestPeers: s.best_peers,
      checking,
      history,
      showHistory: ctx.health.has("liveness_history"),
      graphs: ctx.health.has("graphs"),
      infohash: ih,
      onCheck: () => void checkNow(),
    }));
    if (d.dht) out.push(dhtCard(d.dht, s.title ?? ""));
    if (s.origin !== "dht") {
      out.push(trustCard({
        detail: d,
        threshold: threshold(),
        open: trustOpen,
        onToggle: () => {
          trustOpen = !trustOpen;
          render();
        },
        onCopy: (label, text) => void copyText(ctx, label, text),
      }));
    }
    const views = probeViews(d.probe, d.files ?? []);
    if (d.probe && views) {
      out.push(probeCard({
        probe: d.probe,
        views,
        canCorrect: ctx.health.has("probe_corrections"),
        correcting,
        phone: ctx.layout === "phone",
        ui: probeUi,
        rerender: render,
        onSave: saveCorrections,
        onUndo: undoCorrection,
      }));
    }
    out.push(section(
      "Infohash",
      h(
        "div",
        { class: "copy-row" },
        h("code", { class: "mono", text: s.infohash }),
        h("button", {
          type: "button",
          class: "btn text",
          text: "Copy",
          "aria-label": "Copy the infohash",
          dataset: { fkey: "copy-ih" },
          "on:click": () => void copyText(ctx, "Infohash", s.infohash),
        }),
      ),
    ));
    out.push(section("Source", h("p", { text: sourceLabel(s.origin) })));
    const files = d.files ?? [];
    if (files.length > 0) out.push(section(`Files (${files.length})`, fileTree(files, filesState)));
    const trackers = d.trackers ?? [];
    if (trackers.length > 0) {
      out.push(section(
        `Trackers (${trackers.length})`,
        h("ul", { class: "plain-list mono small" }, ...trackers.slice(0, 50).map((t) => h("li", null, untrusted(t)))),
        trackers.length > 50 ? h("p", { class: "muted small", text: `… ${trackers.length - 50} more` }) : null,
      ));
    }
    const tags = d.hashtags ?? [];
    const ext = (d.external_ids ?? []).filter((p) => Array.isArray(p) && p.length === 2);
    if (tags.length > 0 || ext.length > 0) {
      out.push(section(
        "Tags",
        h(
          "div",
          { class: "tag-row" },
          ...tags.map((t) => h("span", { class: "tag" }, untrusted(`#${t}`))),
          ...ext.map(([k, v]) =>
            h("button", {
              type: "button",
              class: "tag tag-btn",
              title: "Copy",
              "on:click": () => void copyText(ctx, String(k), String(v)),
            }, untrusted(`${k}: ${v}`))
          ),
        ),
      ));
    }
    const description = stripHtml(d.description ?? "");
    if (description.trim() !== "") out.push(section("Description", h("div", { class: "description" }, untrusted(description))));
    const comments = d.comments ?? [];
    if (comments.length > 0) {
      out.push(section(
        `Comments (${comments.length})`,
        h(
          "ul",
          { class: "plain-list comments" },
          ...comments.map((c) =>
            h(
              "li",
              { class: "comment" },
              h("p", { class: "muted small" }, untrusted(shortKey(npub(c.pubkey ?? ""))), ` · ${formatAge(c.created_at ?? 0)}`),
              h("div", { class: "description" }, untrusted(stripHtml(c.content ?? ""))),
            )
          ),
        ),
      ));
    }
    body.replaceChildren(...out);
    if (focusKey) {
      const again = body.querySelector(`[data-fkey="${CSS.escape(focusKey)}"]`);
      if (again instanceof HTMLElement) again.focus();
    }
  }

  void load();
  return {
    update(p) {
      const next = ihSegment(p.ih);
      if (next === ih) return;
      life.abort();
      ih = next;
      detail = null;
      error = null;
      cachedAt = null;
      history = null;
      checking = false;
      trustOpen = false;
      filesState = { expanded: null };
      probeUi = probeUiState();
      void load();
    },
    unmount() {
      life.abort();
      if (retryTimer) clearTimeout(retryTimer);
    },
  };
}
