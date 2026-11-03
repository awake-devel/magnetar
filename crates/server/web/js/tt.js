// The page's one Trusted Types policy (docs/WEB.md "Security model"). The CSP enforces Trusted
// Types for scripts, which also covers the URL given to serviceWorker.register() and to
// new Worker(). This policy turns exactly those same-origin script URLs into TrustedScriptURL
// values and nothing else: it has no createHTML and no createScript, so every markup sink and
// every eval-like sink keeps throwing.

export const POLICY_NAME = "magnetar";

/** Script URLs the page may start: the service worker and its own module workers. */
const ALLOWED = [/^\/sw\.js$/, /^\/_app\/[0-9a-z]{1,32}\/js\/[a-z0-9-]{1,40}-worker\.js$/];

/**
 * @param {string} url
 */
export function allowedScriptUrl(url) {
  return ALLOWED.some((re) => re.test(url));
}

/** @type {{createScriptURL: (u: string) => unknown} | null | undefined} */
let policy;

/**
 * A script URL for serviceWorker.register() or new Worker(): a TrustedScriptURL where the
 * browser has Trusted Types, the checked string elsewhere. Throws for any other URL, and when
 * the CSP does not allow the policy (then the caller skips the feature).
 * @param {string} url same-origin path
 * @returns {string} typed as string for the DOM signatures; a TrustedScriptURL at run time
 */
export function scriptUrl(url) {
  if (!allowedScriptUrl(url)) throw new TypeError("tt: script URL not allowed");
  const tt = /** @type {any} */ (globalThis).trustedTypes;
  if (!tt || typeof tt.createPolicy !== "function") return url;
  if (policy === undefined) {
    policy = tt.createPolicy(POLICY_NAME, {
      /** @param {string} u */
      createScriptURL: (u) => {
        if (!allowedScriptUrl(u)) throw new TypeError("tt: script URL not allowed");
        return u;
      },
    });
  }
  return /** @type {string} */ (/** @type {any} */ (policy).createScriptURL(url));
}

/**
 * Whether the page's CSP names this policy (the server sends `trusted-types magnetar`): read
 * from the page's own response headers, so a server whose CSP allows no policy at all
 * (`trusted-types 'none'`) gets no attempt and no CSP violation in the console.
 * "unknown" when the server did not answer (offline: an installed worker keeps working).
 * @returns {Promise<"yes" | "no" | "unknown">}
 */
export async function policyAllowed() {
  const tt = /** @type {any} */ (globalThis).trustedTypes;
  if (!tt) return "yes";
  try {
    const res = await fetch("/", { method: "HEAD", cache: "no-store", credentials: "same-origin" });
    const csp = res.headers.get("content-security-policy") ?? "";
    const m = /(?:^|;)\s*trusted-types\s+([^;]*)/i.exec(csp);
    if (!m) return "yes";
    return m[1].split(/\s+/).includes(POLICY_NAME) ? "yes" : "no";
  } catch {
    return "unknown";
  }
}
