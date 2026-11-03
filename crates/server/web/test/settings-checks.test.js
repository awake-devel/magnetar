import assert from "node:assert/strict";
import { decodeBech32, encodeBech32 } from "../js/nip19.js";
import {
  checkPageTransport,
  checkRootKey,
  checkToken,
  isPrivateTransport,
  isTailscaleCgnat,
  NOT_A_PUBLIC_KEY,
  PRIVATE_KEY_ERROR,
  rootInputOf,
  rootKeyInput,
  TOKEN_CHARS_ERROR,
} from "../js/settings-checks.js";

const hex = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
const NPUB = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6";
const dec = decodeBech32(NPUB);
if (!dec) throw new Error("vector");
const NSEC = encodeBech32("nsec", dec[1]);

Deno.test("root key field", () => {
  assert.equal(checkRootKey("").error, null);
  assert.equal(checkRootKey(NPUB).error, null);
  assert.equal(checkRootKey(hex).error, null);
  assert.equal(checkRootKey("nsec1abc").error, PRIVATE_KEY_ERROR);
  assert.equal(checkRootKey("NCRYPTSEC1abc").error, PRIVATE_KEY_ERROR);
  assert.equal(checkRootKey("hello").error, NOT_A_PUBLIC_KEY);
  assert.equal(checkRootKey("abc").error, NOT_A_PUBLIC_KEY);
});

Deno.test("token field", () => {
  assert.equal(checkToken("").error, null);
  assert.equal(checkToken("  abc123-XYZ_.~  ").error, null);
  assert.equal(checkToken("abc\ndef").error, TOKEN_CHARS_ERROR);
  assert.equal(checkToken("abc def").error, TOKEN_CHARS_ERROR);
  assert.equal(checkToken("tökén").error, TOKEN_CHARS_ERROR);
});

Deno.test("transport: private hosts and the page warning (D39)", () => {
  assert.equal(isPrivateTransport("127.0.0.1"), true);
  assert.equal(isPrivateTransport("localhost"), true);
  assert.equal(isPrivateTransport("[::1]"), true);
  assert.equal(isPrivateTransport("ryoku-1.tail26814a.ts.net"), true);
  assert.equal(isPrivateTransport("[fd7a:115c:a1e0::1]"), true);
  assert.equal(isPrivateTransport("100.81.1.2"), false);
  assert.equal(isPrivateTransport("192.168.1.20"), false);
  assert.equal(isTailscaleCgnat("100.64.0.1"), true);
  assert.equal(isTailscaleCgnat("100.128.0.1"), false);
  assert.equal(checkPageTransport({ protocol: "https:", hostname: "example.org" }).warning, null);
  assert.equal(checkPageTransport({ protocol: "http:", hostname: "127.0.0.1" }).warning, null);
  assert.equal(checkPageTransport({ protocol: "http:", hostname: "host.tail1.ts.net" }).warning, null);
  const lan = checkPageTransport({ protocol: "http:", hostname: "192.168.1.20" });
  assert.equal(lan.error, null);
  assert.ok(lan.warning?.includes("session cookie"));
  assert.ok(checkPageTransport({ protocol: "http:", hostname: "100.81.1.2" }).warning?.includes("100.x"));
});

Deno.test("a pasted nsec is cleared at once and never comes back (C-SET-04)", () => {
  let s = rootInputOf("");
  s = rootKeyInput(s, NSEC);
  assert.equal(s.value, "");
  assert.equal(s.check.error, PRIVATE_KEY_ERROR);
  // A keyboard that sends the rest of the paste one piece at a time: dropped too.
  s = rootKeyInput(s, "qq");
  assert.equal(s.value, "");
  assert.equal(s.check.error, PRIVATE_KEY_ERROR);
  // The field's own echo of the cleared value keeps the warning.
  s = rootKeyInput(s, "");
  assert.equal(s.check.error, PRIVATE_KEY_ERROR);
  // The start of a public key typed by hand shows.
  s = rootKeyInput(s, "np");
  assert.equal(s.value, "np");
  // A full npub is accepted.
  s = rootKeyInput(s, NPUB);
  assert.equal(s.value, NPUB);
  assert.equal(s.check.error, null);
  assert.equal(s.dropping, false);
  // nsec typed one character at a time.
  let t = rootInputOf("");
  for (const c of "nsec1") t = rootKeyInput(t, t.value + c);
  assert.equal(t.value, "");
  assert.equal(t.check.error, PRIVATE_KEY_ERROR);
  // Uppercase or with spaces.
  assert.equal(rootKeyInput(rootInputOf(""), `  ${NSEC.toUpperCase()}`).value, "");
  // Something else ends the drop.
  t = rootKeyInput(t, "B");
  assert.equal(t.value, "B");
  assert.equal(t.check.error, NOT_A_PUBLIC_KEY);
});
