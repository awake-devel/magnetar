// Port of android/.../ui/stats/SourcesModel.kt: "Where discoveries come from" (feature
// `crawl_sources`), plus the app's IP-literal redaction of relay URLs and machine names
// (MagnetarApi.redactIpHost): the page never shows, caches or sends an IP address. Pure.

import { defaultLocale, formatNumber, formatRate } from "./numfmt.js";

/** @typedef {import("./types-stats.js").RouteCount} RouteCount */
/** @typedef {import("./types-stats.js").RouteHour} RouteHour */
/** @typedef {import("./types-stats.js").TopRelay} TopRelay */
/** @typedef {import("./types-stats.js").CrawlMachine} CrawlMachine */
/** @typedef {import("./types-stats.js").CrawlSources} CrawlSources */

/** DHT routes in a fixed order, so a route keeps its colour slot. */
export const ROUTE_ORDER = Object.freeze(["sample", "get_peers", "announce"]);

/** @param {string} via */
export function routeLabel(via) {
  switch (via) {
    case "sample":
      return "Sampling (BEP 51)";
    case "get_peers":
      return "Lookups (get_peers)";
    case "announce":
      return "Announces";
    default:
      return via;
  }
}

/** @param {string} via */
function routeRank(via) {
  const i = ROUTE_ORDER.indexOf(via);
  return i < 0 ? ROUTE_ORDER.length : i;
}

/**
 * Routes in ROUTE_ORDER, unknown routes after them (stable).
 * @param {readonly RouteCount[]} routes
 */
export function orderedRoutes(routes) {
  return routes.map((r, i) => ({ r, i }))
    .sort((a, b) => routeRank(a.r.via) - routeRank(b.r.via) || a.i - b.i)
    .map((x) => x.r);
}

/**
 * Route names of a series of hours, ROUTE_ORDER first, then the others sorted.
 * @param {readonly RouteHour[]} hours
 */
export function hourRoutes(hours) {
  /** @type {Set<string>} */
  const seen = new Set();
  for (const h of hours) {
    for (const k of Object.keys(h.seen ?? {})) seen.add(k);
    for (const k of Object.keys(h.stored ?? {})) seen.add(k);
  }
  return [...ROUTE_ORDER.filter((r) => seen.has(r)), ...[...seen].filter((r) => !ROUTE_ORDER.includes(r)).sort()];
}

/**
 * Stored / seen as a percent: one decimal under 10 % ("6.6%"), "<0.1%" for a tiny non-zero
 * share, "–" when nothing was seen.
 * @param {number} stored
 * @param {number} seen
 * @param {string} [locale]
 */
export function formatConversion(stored, seen, locale = defaultLocale()) {
  if (seen <= 0) return "–";
  const share = stored / seen;
  /** @param {number} digits */
  const f = (digits) => new Intl.NumberFormat(locale, { style: "percent", maximumFractionDigits: digits });
  if (stored > 0 && share < 0.001) return "<" + f(1).format(0.001);
  return f(share < 0.1 ? 1 : 0).format(share);
}

/** @param {string} source */
export function relaySourceLabel(source) {
  switch (source) {
    case "nip66":
      return "NIP-66 monitors";
    case "nip65":
      return "NIP-65 relay lists";
    case "seed":
      return "Seed list";
    case "hint":
      return "Relay hints in events";
    case "kind3":
      return "Contact lists (kind 3)";
    case "manual":
      return "Added by hand";
    default:
      return source.trim() === "" ? "Unknown" : source;
  }
}

/**
 * "DE · Hetzner Online GmbH (AS24940)", or null when nothing is known.
 * @param {TopRelay} r
 * @returns {string | null}
 */
export function relayPlace(r) {
  const org = r.as_org != null && r.as_org.trim() !== "" ? r.as_org : null;
  let net = null;
  if (org != null && r.asn != null) net = `${org} (AS${r.asn})`;
  else if (org != null) net = org;
  else if (r.asn != null) net = `AS${r.asn}`;
  const cc = r.country != null && r.country.trim() !== "" ? r.country.toUpperCase() : null;
  const s = [cc, net].filter((x) => x != null).join(" · ");
  return s === "" ? null : s;
}

/**
 * A relay URL without its `wss://` for a list row.
 * @param {string} url
 */
export function shortRelayUrl(url) {
  let u = url;
  if (u.startsWith("wss://")) u = u.slice(6);
  else if (u.startsWith("ws://")) u = u.slice(5);
  return u.replace(/\/+$/, "");
}

/**
 * One machine (server or worker) as label → value lines.
 * @param {CrawlMachine} m
 * @param {string} [locale]
 * @returns {[string, string][]}
 */
export function machineLines(m, locale = defaultLocale()) {
  /** @type {[string, string][]} */
  const out = [["Triaged", formatNumber(m.dht_triaged ?? 0, locale)]];
  if ((m.dht_queued ?? 0) > 0) out.push(["Queued", formatNumber(m.dht_queued ?? 0, locale)]);
  out.push(["Metadata fetched", formatNumber(m.dht_metadata ?? 0, locale)]);
  out.push(["Stored", formatNumber(m.dht_stored ?? 0, locale) + " · " + formatRate(m.stored_per_min ?? 0, "min", locale)]);
  out.push(["Seed checks", formatNumber(m.seed_checks ?? 0, locale)]);
  return out;
}

/** @param {CrawlMachine} m */
export function machineTitle(m) {
  const name = (m.name ?? "").trim() === "" ? "?" : m.name ?? "?";
  const kind = m.kind ?? "";
  if (kind === "server") return `${name} (server)`;
  if (kind === "worker") return `${name} (worker)`;
  return kind.trim() !== "" ? `${name} (${kind})` : name;
}

const IPV4 = /^\d{1,3}(\.\d{1,3}){3}$/;

/**
 * `url` with an IP-literal host replaced (`wss://203.0.113.5:7447/x` → `wss://(IP address)`).
 * Host names are kept.
 * @param {string} url
 */
export function redactIpHost(url) {
  const sep = url.indexOf("://");
  const scheme = sep >= 0 ? url.slice(0, sep) : "";
  const rest = sep >= 0 ? url.slice(sep + 3) : url;
  let authority = rest.split("/")[0].split("?")[0].split("#")[0];
  const at = authority.lastIndexOf("@");
  if (at >= 0) authority = authority.slice(at + 1);
  const host = authority.startsWith("[") ? authority.slice(0, authority.indexOf("]") + 1) : authority.split(":")[0];
  const colons = [...authority].filter((c) => c === ":").length;
  const ip = host.startsWith("[") || IPV4.test(host) || colons > 1;
  if (!ip) return url;
  return (scheme === "" ? "" : `${scheme}://`) + "(IP address)";
}

/**
 * The sources answer with every IP-literal relay host and machine name redacted, before
 * anything shows or caches it.
 * @param {CrawlSources} s
 * @returns {CrawlSources}
 */
export function redactSources(s) {
  const nostr = s.nostr
    ? { ...s.nostr, top_relays: (s.nostr.top_relays ?? []).map((r) => ({ ...r, url: redactIpHost(r.url ?? "") })) }
    : s.nostr;
  return { ...s, nostr, machines: (s.machines ?? []).map((m) => ({ ...m, name: redactIpHost(m.name ?? "") })) };
}
