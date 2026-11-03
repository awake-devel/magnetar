// Values from CrawlMapTest.kt ("Sources helpers") and MagnetarApiTest.kt (redaction).
import assert from "node:assert/strict";
import {
  formatConversion,
  hourRoutes,
  machineLines,
  machineTitle,
  orderedRoutes,
  redactIpHost,
  redactSources,
  relayPlace,
  relaySourceLabel,
  routeLabel,
  shortRelayUrl,
} from "../js/sources-model.js";

const en = "en-US";
const fr = "fr-FR";

Deno.test("routes, conversion and relay labels", () => {
  assert.equal(routeLabel("get_peers"), "Lookups (get_peers)");
  assert.equal(routeLabel("sample"), "Sampling (BEP 51)");
  assert.equal(routeLabel("announce"), "Announces");
  assert.deepEqual(
    orderedRoutes([{ via: "x" }, { via: "announce" }, { via: "sample" }, { via: "get_peers" }]).map((r) => r.via),
    ["sample", "get_peers", "announce", "x"],
  );
  assert.deepEqual(hourRoutes([{ bucket: 0, seen: { announce: 1, zz: 2 }, stored: { sample: 1 } }]), ["sample", "announce", "zz"]);
  assert.equal(formatConversion(1_009_417, 4_256_462, en), "24%");
  assert.equal(formatConversion(143_237, 2_156_244, en), "6.6%");
  assert.ok(formatConversion(143_237, 2_156_244, fr).startsWith("6,6"));
  assert.equal(formatConversion(1, 1_000_000, en), "<0.1%");
  assert.equal(formatConversion(0, 100, en), "0%");
  assert.equal(formatConversion(5, 0, en), "–");
  assert.equal(relaySourceLabel("nip66"), "NIP-66 monitors");
  assert.equal(relaySourceLabel("other"), "other");
  assert.equal(relaySourceLabel(""), "Unknown");
  assert.equal(relayPlace({ country: "de", asn: 24940, as_org: "Hetzner Online GmbH" }), "DE · Hetzner Online GmbH (AS24940)");
  assert.equal(relayPlace({ asn: 3320 }), "AS3320");
  assert.equal(relayPlace({}), null);
  assert.equal(shortRelayUrl("wss://relay.example/"), "relay.example");
});

Deno.test("machine lines", () => {
  const m = { name: "eee", kind: "worker", connected: true, dht_triaged: 120_000, dht_stored: 8_000, seed_checks: 30_000, stored_per_min: 80.5 };
  assert.equal(machineTitle(m), "eee (worker)");
  const lines = new Map(machineLines(m, en));
  assert.equal(lines.get("Triaged"), "120,000");
  assert.equal(lines.get("Stored"), "8,000 · 81/min");
  assert.equal(lines.get("Seed checks"), "30,000");
  assert.ok(!lines.has("Queued"));
  assert.equal(machineTitle({ name: "", kind: "" }), "?");
});

Deno.test("IP literal hosts are redacted, names kept", () => {
  assert.equal(redactIpHost("wss://relay.damus.io"), "wss://relay.damus.io");
  assert.equal(redactIpHost("wss://nos.lol:443/path"), "wss://nos.lol:443/path");
  assert.equal(redactIpHost("wss://1.2.3.4"), "wss://(IP address)");
  assert.equal(redactIpHost("wss://203.0.113.9"), "wss://(IP address)");
  assert.equal(redactIpHost("wss://user@10.0.0.1:7000/x"), "wss://(IP address)");
  assert.equal(redactIpHost("wss://[::1]:7000"), "wss://(IP address)");
  assert.equal(redactIpHost("2001:db8::1"), "(IP address)");
  assert.equal(redactIpHost("wss://1.2.3.4.example.com"), "wss://1.2.3.4.example.com");
  const s = redactSources({
    nostr: { top_relays: [{ url: "wss://203.0.113.9" }, { url: "wss://relay.example" }] },
    machines: [{ name: "192.168.1.4" }, { name: "eee" }],
  });
  assert.deepEqual(s.nostr?.top_relays?.map((r) => r.url), ["wss://(IP address)", "wss://relay.example"]);
  assert.deepEqual(s.machines?.map((m) => m.name), ["(IP address)", "eee"]);
});
