import assert from "node:assert/strict";
import { decodeBech32, encodeBech32, isPrivateKey, npub, pubkeyHex } from "../js/nip19.js";

// NIP-19's own example (nips/19.md), as SettingsChecksTest.
const hex = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
const NPUB = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6";
const NPROFILE =
  "nprofile1qqsrhuxx8l9ex335q7he0f09aej04zpazpl0ne2cgukyawd24mayt8gpp4mhxue69uhhytnc9e3k7mgpz4mhxue69uhkg6nzv9ejuumpv34kytnrdaksjlyr9p";

Deno.test("npub and nprofile decode to the NIP-19 example key, and back", () => {
  assert.equal(pubkeyHex(NPUB), hex);
  assert.equal(pubkeyHex(` ${NPUB} `), hex);
  assert.equal(pubkeyHex(NPUB.toUpperCase()), hex);
  assert.equal(pubkeyHex(NPROFILE), hex);
  assert.equal(pubkeyHex(hex.toUpperCase()), hex);
  assert.equal(npub(hex), NPUB);
  assert.equal(npub("not hex"), "not hex");
});

Deno.test("bad keys are refused, private keys never decoded", () => {
  assert.equal(pubkeyHex(NPUB.slice(0, -1) + "7"), null);
  assert.equal(pubkeyHex("npub1"), null);
  assert.equal(pubkeyHex(NPUB.slice(0, 20) + NPUB.slice(20).toUpperCase()), null);
  assert.equal(pubkeyHex(hex.slice(0, -2)), null);
  const dec = decodeBech32(NPUB);
  assert.ok(dec);
  assert.equal(pubkeyHex(encodeBech32("note", dec[1])), null);
  const nsec = encodeBech32("nsec", dec[1]);
  assert.equal(isPrivateKey(nsec), true);
  assert.equal(pubkeyHex(nsec), null);
  assert.equal(isPrivateKey("  NCRYPTSEC1abc"), true);
  assert.equal(isPrivateKey(NPUB), false);
});

Deno.test("bech32 matches BIP-173 test vectors", () => {
  for (
    const v of [
      "A12UEL5L",
      "a12uel5l",
      "abcdef1qpzry9x8gf2tvdw0s3jn54khce6mua7lmqqqxw",
      "split1checkupstagehandshakeupstreamerranterredcaperred2y9e3w",
    ]
  ) {
    assert.notEqual(decodeBech32(v), null, v);
  }
  for (const v of ["pzry9x0s0muk", "1pzry9x0s0muk", "x1b4n0q5v", "li1dgmt3", "A1G7SGD8", "10a06t8"]) {
    assert.equal(decodeBech32(v), null, v);
  }
  const d = decodeBech32("abcdef1qpzry9x8gf2tvdw0s3jn54khce6mua7lmqqqxw");
  assert.ok(d);
  assert.equal(encodeBech32(d[0], d[1]), "abcdef1qpzry9x8gf2tvdw0s3jn54khce6mua7lmqqqxw");
});
