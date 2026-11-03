import assert from "node:assert/strict";
import { allowedScriptUrl, scriptUrl } from "../js/tt.js";

Deno.test("the Trusted Types policy passes the service worker and module workers only", () => {
  assert.equal(allowedScriptUrl("/sw.js"), true);
  assert.equal(allowedScriptUrl("/_app/0123456789abcdef/js/graph-worker.js"), true);
  for (
    const u of [
      "/sw.js?x=1",
      "//evil.example/sw.js",
      "https://evil.example/sw.js",
      "/_app/v/js/main.js",
      "/_app/v/js/../../sw.js",
      "data:text/javascript,1",
      "blob:http://127.0.0.1/x",
      "/api/v1/x-worker.js",
    ]
  ) {
    assert.equal(allowedScriptUrl(u), false, u);
    assert.throws(() => scriptUrl(u), TypeError, u);
  }
  // No trustedTypes in Deno: the checked string itself.
  assert.equal(scriptUrl("/sw.js"), "/sw.js");
});
