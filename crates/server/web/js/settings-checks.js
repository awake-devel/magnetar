// Port of android/.../settings/SettingsChecks.kt and the root-key input rules of
// SettingsViewModel.setRootKey, plus the web's page-transport check (D39 for a browser).

import { isPrivateKey, pubkeyHex } from "./nip19.js";

export { isPrivateKey };

/** Shown when an nsec is pasted into the trust root field; the field is cleared at once. */
export const PRIVATE_KEY_ERROR =
  "That is a private key (nsec): never paste it into an app. Use your public key (npub1…). The field was cleared.";

export const NOT_A_PUBLIC_KEY = "Not a public key: paste an npub1…, an nprofile1… or 64 hex digits";

export const TOKEN_CHARS_ERROR = "The token has a space, line break or non-ASCII character";

/**
 * A field check: an error blocks saving, a warning does not.
 * @typedef {object} Check
 * @property {string | null} error
 * @property {string | null} warning
 */

/**
 * @param {string | null} [error]
 * @param {string | null} [warning]
 * @returns {Check}
 */
function check(error = null, warning = null) {
  return { error, warning };
}

/**
 * The trust root field: empty (no root), or an npub / nprofile / 64-digit hex public key.
 * @param {string} key
 */
export function checkRootKey(key) {
  const k = key.trim();
  if (k === "") return check();
  if (isPrivateKey(k)) return check(PRIVATE_KEY_ERROR);
  if (pubkeyHex(k) == null) return check(NOT_A_PUBLIC_KEY);
  return check();
}

/**
 * The API token: printable ASCII only.
 * @param {string} token
 */
export function checkToken(token) {
  const t = token.trim();
  for (let i = 0; i < t.length; i++) {
    const c = t.charCodeAt(i);
    if (c < 0x21 || c > 0x7e) return check(TOKEN_CHARS_ERROR);
  }
  return check();
}

/**
 * @param {string} host
 * @returns {number[] | null}
 */
function ipv4(host) {
  const parts = host.split(".");
  if (parts.length !== 4) return null;
  /** @type {number[]} */
  const out = [];
  for (const p of parts) {
    if (!/^\d{1,3}$/.test(p)) return null;
    const n = Number(p);
    if (n > 255) return null;
    out.push(n);
  }
  return out;
}

/**
 * Hosts where plain http:// is not exposed on the wire (D39): loopback, MagicDNS *.ts.net and
 * the Tailscale IPv6 range. Tailscale IPv4 (100.64.0.0/10) is not: it is also carrier-grade NAT.
 * @param {string} host
 */
export function isPrivateTransport(host) {
  const h = host.toLowerCase().replace(/^\[/, "").replace(/\]$/, "");
  if (h === "localhost" || h === "::1" || h.endsWith(".ts.net")) return true;
  const v4 = ipv4(h);
  if (v4 != null) return v4[0] === 127;
  return h.startsWith("fd7a:115c:a1e0:");
}

/**
 * 100.64.0.0/10: a Tailscale address, or carrier-grade NAT when the tunnel is down.
 * @param {string} host
 */
export function isTailscaleCgnat(host) {
  const v4 = ipv4(host);
  return v4 != null && v4[0] === 100 && v4[1] >= 64 && v4[1] <= 127;
}

/**
 * The web's D39 check: this page itself on plain http:// to a host that is neither loopback nor
 * reachable only through Tailscale. Returns a warning (never an error).
 * @param {{protocol: string, hostname: string}} location
 * @returns {Check}
 */
export function checkPageTransport(location) {
  if (location.protocol !== "http:" || isPrivateTransport(location.hostname)) return check();
  if (isTailscaleCgnat(location.hostname)) {
    return check(
      null,
      "Plain HTTP to a 100.x address: encrypted only while Tailscale is connected on this device. " +
        "Without it, 100.64.0.0/10 is carrier-grade NAT space and your session cookie travels readable. " +
        "Prefer the *.ts.net name or https://.",
    );
  }
  return check(
    null,
    "Plain HTTP: your session cookie and results can be read by anyone on this network. " +
      "Prefer https:// (tailscale serve or a reverse proxy).",
  );
}

const BECH32_CHARS = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const NSEC_DATA_LEN = 58;

/**
 * State of the trust root field's private-key guard.
 * @typedef {object} RootInput
 * @property {string} value what the field shows
 * @property {Check} check
 * @property {boolean} dropping a private key was just cleared: its tail is dropped too
 * @property {string} dropped the last dropped characters (to see "nsec1" typed again)
 */

/** @returns {RootInput} */
export function rootInputOf(/** @type {string} */ value) {
  return { value, check: checkRootKey(value), dropping: false, dropped: "" };
}

/**
 * The field's next state for what the browser reports as its new value (SettingsViewModel
 * setRootKey): a pasted or typed nsec never stays in the field, not even unsaved; while
 * dropping, the rest of the key typed one character at a time is dropped as well.
 * @param {RootInput} state
 * @param {string} v
 * @returns {RootInput}
 */
export function rootKeyInput(state, v) {
  /** @returns {RootInput} */
  const cleared = (/** @type {string} */ dropped) => ({
    value: "",
    check: check(PRIVATE_KEY_ERROR),
    dropping: true,
    dropped,
  });
  if (isPrivateKey(v)) return cleared("");
  if (state.dropping) {
    const l = v.toLowerCase();
    if (v === "") return cleared(state.dropped);
    if ("npub1".startsWith(l) || "nprofile1".startsWith(l)) {
      return { value: v, check: check(), dropping: true, dropped: state.dropped };
    }
    const joined = state.dropped + l;
    if (joined.includes("nsec1") || joined.includes("ncryptsec1")) return cleared("");
    if (v.length <= NSEC_DATA_LEN && [...v].every((c) => BECH32_CHARS.includes(c))) {
      return cleared((state.dropped + v).slice(-10));
    }
  }
  return { value: v, check: checkRootKey(v), dropping: false, dropped: "" };
}
