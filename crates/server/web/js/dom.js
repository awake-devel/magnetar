// The only way the UI builds DOM (docs/WEB.md "Security model"). Every node comes from
// createElement / createElementNS and every string goes in as a Text node or textContent.
// URLs reach `href` only as SafeUrl values made here from validated parts, and images only
// through posterImg(). Nothing parses markup.

const TOKEN = Symbol("safe-url");

/** A URL this module built from validated parts (internal route, API magnet, fixed external base). */
export class SafeUrl {
  /** @type {string} */
  #url;
  /** @type {boolean} */
  #external;

  /**
   * @param {symbol} token
   * @param {string} url
   * @param {boolean} external
   */
  constructor(token, url, external) {
    if (token !== TOKEN) throw new TypeError("SafeUrl is made by dom.js only");
    this.#url = url;
    this.#external = external;
  }

  get external() {
    return this.#external;
  }

  toString() {
    return this.#url;
  }
}

/** Element properties h() takes. Anything else throws. */
const PROPS = new Set([
  "class", "id", "text", "role", "type", "value", "checked", "disabled", "title", "href", "dataset",
  "name", "placeholder", "autocomplete", "spellcheck", "inputmode", "enterkeyhint", "min", "max", "step",
  "maxlength", "minlength", "pattern", "for", "tabindex", "hidden", "dir", "lang", "rel", "target", "open",
  "selected", "required", "readonly", "alt", "colspan", "scope", "datetime", "autofocus", "list", "size",
  "draggable", "translate", "inert", "label",
]);

/** Set as DOM properties rather than attributes. */
const AS_PROPERTY = new Set(["value", "checked", "disabled", "hidden", "selected", "open", "autofocus", "inert"]);

/** Tags h() never makes (script, frames, embedded documents, forms posting elsewhere). */
const FORBIDDEN_TAGS = new Set([
  "script", "iframe", "frame", "frameset", "object", "embed", "base", "meta", "link", "style", "template",
  "svg", "math", "img",
]);

/**
 * @typedef {Node | string | number | null | undefined | false} Child
 * @typedef {Child | Child[]} Children
 */

/**
 * @param {Node} parent
 * @param {Children[]} children
 */
function appendAll(parent, children) {
  for (const c of children) {
    if (c == null || c === false) continue;
    if (Array.isArray(c)) {
      appendAll(parent, c);
    } else if (typeof c === "string" || typeof c === "number") {
      parent.appendChild(document.createTextNode(String(c)));
    } else if (typeof c === "object" && "nodeType" in c) {
      parent.appendChild(c);
    } else {
      throw new TypeError("h(): a child must be a node or a string");
    }
  }
}

/**
 * Makes an HTML element.
 * @param {string} tag
 * @param {Record<string, unknown> | null} [props]
 * @param {...Children} children
 * @returns {HTMLElement}
 */
export function h(tag, props = null, ...children) {
  const t = tag.toLowerCase();
  if (FORBIDDEN_TAGS.has(t) || !/^[a-z][a-z0-9-]*$/.test(t)) throw new TypeError(`h(): tag ${tag} is not allowed`);
  const el = document.createElement(t);
  if (props) {
    for (const [k, v] of Object.entries(props)) {
      if (v == null || v === false && !AS_PROPERTY.has(k)) continue;
      if (k.startsWith("on:")) {
        if (typeof v !== "function") throw new TypeError(`h(): ${k} must be a function`);
        el.addEventListener(k.slice(3), /** @type {EventListener} */ (v));
        continue;
      }
      if (k.startsWith("aria-")) {
        el.setAttribute(k, String(v));
        continue;
      }
      if (!PROPS.has(k)) throw new TypeError(`h(): property ${k} is not allowed`);
      switch (k) {
        case "text":
          el.textContent = String(v);
          break;
        case "class":
          el.className = Array.isArray(v) ? v.filter(Boolean).join(" ") : String(v);
          break;
        case "dataset":
          for (const [dk, dv] of Object.entries(/** @type {Record<string, unknown>} */ (v))) {
            if (dv != null) el.dataset[dk] = String(dv);
          }
          break;
        case "href":
          if (!(v instanceof SafeUrl)) throw new TypeError("h(): href must be a SafeUrl from link(), magnetLink() or externalUrl()");
          el.setAttribute("href", v.toString());
          if (v.external) {
            el.setAttribute("rel", "noopener noreferrer");
            el.setAttribute("target", "_blank");
          }
          break;
        case "for":
          el.setAttribute("for", String(v));
          break;
        default:
          if (AS_PROPERTY.has(k)) {
            /** @type {Record<string, unknown>} */ (/** @type {unknown} */ (el))[k] = v;
          } else {
            el.setAttribute(k, String(v));
          }
      }
    }
  }
  appendAll(el, children);
  return el;
}

const SVG_NS = "http://www.w3.org/2000/svg";

const SVG_TAGS = new Set([
  "svg", "g", "path", "rect", "circle", "ellipse", "line", "polyline", "polygon", "text", "tspan", "title",
  "desc", "defs", "clipPath", "linearGradient", "stop",
]);

const SVG_ATTRS = new Set([
  "x", "y", "width", "height", "d", "cx", "cy", "r", "rx", "ry", "points", "fill", "stroke", "stroke-width",
  "transform", "viewBox", "class", "role", "aria-label", "aria-hidden", "tabindex", "text-anchor",
  "dominant-baseline", "x1", "y1", "x2", "y2", "stroke-linecap", "stroke-linejoin", "stroke-dasharray",
  "opacity", "fill-opacity", "stroke-opacity", "fill-rule", "clip-rule", "vector-effect",
  "preserveAspectRatio", "focusable", "font-size", "font-weight", "id", "clip-path", "offset", "stop-color",
  "dx", "dy", "aria-describedby",
]);

const PATH_DATA = /^[MmLlHhVvCcSsQqTtAaZz0-9.,\-+eE\s]*$/;
const POINTS = /^[0-9.,\-+eE\s]*$/;

/**
 * Makes an SVG element (charts, icons). `d` and `points` take numbers only; `text` sets
 * textContent; `on:<event>` adds a listener.
 * @param {string} tag
 * @param {Record<string, unknown> | null} [attrs]
 * @param {...Children} children
 * @returns {SVGElement}
 */
export function svg(tag, attrs = null, ...children) {
  if (!SVG_TAGS.has(tag)) throw new TypeError(`svg(): tag ${tag} is not allowed`);
  const el = /** @type {SVGElement} */ (document.createElementNS(SVG_NS, tag));
  if (attrs) {
    for (const [k, v] of Object.entries(attrs)) {
      if (v == null || v === false) continue;
      if (k.startsWith("on:")) {
        if (typeof v !== "function") throw new TypeError(`svg(): ${k} must be a function`);
        el.addEventListener(k.slice(3), /** @type {EventListener} */ (v));
        continue;
      }
      if (k === "text") {
        el.textContent = String(v);
        continue;
      }
      if (!SVG_ATTRS.has(k)) throw new TypeError(`svg(): attribute ${k} is not allowed`);
      const s = String(v);
      if (k === "d" && !PATH_DATA.test(s)) throw new TypeError("svg(): bad path data");
      if (k === "points" && !POINTS.test(s)) throw new TypeError("svg(): bad points");
      el.setAttribute(k, s);
    }
  }
  appendAll(el, children);
  return el;
}

/**
 * Removes every child (never by assigning markup).
 * @param {Element} el
 */
export function clear(el) {
  el.replaceChildren();
}

/**
 * Text from the API (titles, file names, track names, publisher names, relay URLs, comments,
 * saved-search names): a bidi-isolated span, so a right-to-left override cannot flip the
 * surrounding layout.
 * @param {unknown} text
 * @param {string} [cls]
 */
export function untrusted(text, cls) {
  const s = document.createElement("span");
  s.className = cls ? `u ${cls}` : "u";
  s.setAttribute("dir", "auto");
  s.textContent = text == null ? "" : String(text);
  return s;
}

const IH = /^[0-9a-f]{40}$/;
const GRAPH_PATH = /^(wot|relays|media\/\d{1,18}|swarm\/[0-9a-f]{40})$/;

/**
 * @param {Record<string, string | number | null | undefined> | URLSearchParams | string | undefined} q
 */
function queryString(q) {
  if (q == null) return "";
  if (typeof q === "string") return q === "" ? "" : `?${new URLSearchParams(q).toString()}`;
  const p = q instanceof URLSearchParams ? q : new URLSearchParams();
  if (!(q instanceof URLSearchParams)) {
    for (const [k, v] of Object.entries(q)) if (v != null && v !== "") p.set(k, String(v));
  }
  const s = p.toString();
  return s === "" ? "" : `?${s}`;
}

/**
 * Is `next` a same-origin path the app may navigate to after sign-in?
 * @param {string | null | undefined} next
 */
export function safeNext(next) {
  return typeof next === "string" && next.startsWith("/") && !next.startsWith("//") && !next.startsWith("/\\") &&
    !hasControl(next);
}

/**
 * Any C0 control character (or DEL).
 * @param {string} s
 */
function hasControl(s) {
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x20 || c === 0x7f) return true;
  }
  return false;
}

/**
 * An internal route URL from validated parts.
 * @param {string} route search | saved | stats | stats/graphs | stats/status | settings | login |
 *   torrent | title | graph | crawl-map
 * @param {Record<string, unknown>} [params]
 * @returns {SafeUrl}
 */
export function link(route, params = {}) {
  /** @type {string} */
  let path;
  switch (route) {
    case "search":
      path = "/" + queryString(/** @type {any} */ (params.query));
      break;
    case "saved":
    case "stats":
    case "stats/graphs":
    case "stats/status":
    case "settings":
      path = `/${route}`;
      break;
    case "login": {
      const next = params.next;
      path = "/login" + (typeof next === "string" && safeNext(next) ? queryString({ next }) : "");
      break;
    }
    case "torrent": {
      const ih = String(params.ih ?? "").toLowerCase();
      if (!IH.test(ih)) throw new TypeError("link(): bad infohash");
      path = `/torrent/${ih}`;
      break;
    }
    case "title": {
      const id = Number(params.id);
      if (!Number.isSafeInteger(id) || id < 0) throw new TypeError("link(): bad media id");
      path = `/title/${id}`;
      break;
    }
    case "graph": {
      const p = String(params.path ?? "");
      if (!GRAPH_PATH.test(p)) throw new TypeError("link(): bad graph path");
      path = "/graph" + queryString({ path: p });
      break;
    }
    case "crawl-map": {
      /** @type {Record<string, string>} */
      const q = {};
      for (const k of ["layer", "window", "metric"]) {
        const v = params[k];
        if (typeof v === "string" && /^[a-z0-9_]{1,40}$/.test(v)) q[k] = v;
      }
      path = "/crawl-map" + queryString(q);
      break;
    }
    default:
      throw new TypeError(`link(): unknown route ${route}`);
  }
  return new SafeUrl(TOKEN, path, false);
}

/**
 * The detail's magnet, only from the API's own `magnet` field and only when it is a magnet for
 * this infohash. Never built on the client.
 * @param {{magnet?: string, summary?: {infohash?: string}} | null | undefined} detail
 * @returns {SafeUrl | null}
 */
export function magnetLink(detail) {
  const m = detail?.magnet;
  const ih = detail?.summary?.infohash;
  if (typeof m !== "string" || typeof ih !== "string" || !IH.test(ih.toLowerCase())) return null;
  if (!/^magnet:\?/i.test(m) || /[\s"<>\\]/.test(m) || hasControl(m)) return null;
  if (!m.toLowerCase().includes(`urn:btih:${ih.toLowerCase()}`)) return null;
  return new SafeUrl(TOKEN, m, false);
}

/**
 * External links to fixed bases only.
 * @param {"imdb" | "dbip" | "tmdb"} kind
 * @param {string} [value] the IMDb id for "imdb"
 * @returns {SafeUrl | null}
 */
export function externalUrl(kind, value) {
  switch (kind) {
    case "imdb":
      return typeof value === "string" && /^tt\d{7,10}$/.test(value)
        ? new SafeUrl(TOKEN, `https://www.imdb.com/title/${value}/`, true)
        : null;
    case "dbip":
      return new SafeUrl(TOKEN, "https://db-ip.com", true);
    case "tmdb":
      return new SafeUrl(TOKEN, "https://www.themoviedb.org", true);
    default:
      return null;
  }
}

/**
 * A link to an internal route the router handles without a page load.
 * @param {SafeUrl} url
 * @param {Record<string, unknown> | null} props
 * @param {...Children} children
 */
export function routeLink(url, props, ...children) {
  const el = h("a", { ...(props ?? {}), href: url }, ...children);
  el.dataset.route = "";
  return el;
}

const POSTER = /^\/api\/v1\/media\/\d{1,18}\/poster$/;

/**
 * The poster of a title: same-origin only (the session cookie rides along), a placeholder block
 * when there is none or it fails to load.
 * @param {{media_id?: number, poster?: string | null, title?: string} | null | undefined} media
 * @param {{cls?: string}} [opts]
 */
export function posterImg(media, opts = {}) {
  const frame = h("div", { class: ["poster", opts.cls] });
  const placeholder = () => {
    frame.replaceChildren(h("div", { class: "poster-empty", "aria-hidden": "true" }));
  };
  /** @type {string | null} */
  let src = null;
  if (media?.poster != null && POSTER.test(media.poster)) src = media.poster;
  else if (media?.poster != null && media.media_id != null && Number.isSafeInteger(media.media_id)) {
    src = `/api/v1/media/${media.media_id}/poster`;
  }
  if (src == null) {
    placeholder();
    return frame;
  }
  const img = document.createElement("img");
  img.referrerPolicy = "no-referrer";
  img.decoding = "async";
  img.loading = "lazy";
  img.alt = media?.title ? `Poster of ${media.title}` : "Poster";
  img.addEventListener("error", placeholder, { once: true });
  img.src = src;
  frame.appendChild(img);
  return frame;
}
