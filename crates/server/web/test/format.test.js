import assert from "node:assert/strict";
import {
  cachedAgo,
  fileName,
  formatAge,
  formatAgo,
  formatBytes,
  formatCount,
  formatRating,
  peersText,
  resultsText,
  shortKey,
  stripHtml,
  trustLevel,
} from "../js/format.js";
import { axisLabel, backlogEta, bucketLabel, formatCompact, formatNumber, formatShare, formatUptime } from "../js/numfmt.js";

/** @param {string} s */
const plain = (s) => s.replace(/[  ]/g, " ");

Deno.test("sizes, counts and results", () => {
  assert.equal(formatBytes(null), "?");
  assert.equal(formatBytes(-1), "?");
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(1536), "1.50 KiB");
  assert.equal(formatBytes(4.37 * 1024 ** 3), "4.37 GiB");
  assert.equal(formatBytes(123.4 * 1024 ** 2), "123.4 MiB");
  assert.equal(formatCount(1234567), "1,234,567");
  assert.equal(formatCount(1000, true), "1,000+");
  assert.equal(resultsText(1, false), "1 result");
  assert.equal(resultsText(1, true), "1+ results");
  assert.equal(resultsText(12, false), "12 results");
  assert.equal(resultsText(1000, true), "1,000+ results");
});

Deno.test("ages", () => {
  const now = 1_800_000_000;
  assert.equal(formatAge(now - 30, now), "now");
  assert.equal(formatAge(now - 180, now), "3m");
  assert.equal(formatAge(now - 5 * 3600, now), "5h");
  assert.equal(formatAge(now - 12 * 86400, now), "12d");
  assert.equal(formatAge(now - 800 * 86400, now), "2y");
  assert.equal(formatAge(now + 100, now), "now");
  assert.equal(formatAgo(now - 10, now), "just now");
  assert.equal(formatAgo(now - 300, now), "5m ago");
  assert.equal(cachedAgo(1000, 1000 + 30_000), "cached · just now");
  assert.equal(cachedAgo(0, 5 * 60_000), "cached · 5 min ago");
  assert.equal(cachedAgo(0, 3 * 3600_000), "cached · 3 h ago");
  assert.equal(cachedAgo(0, 2 * 86400_000), "cached · 2 d ago");
});

Deno.test("peers, names, keys, ratings", () => {
  assert.equal(peersText(12, 3), "12 S / 3 L");
  assert.equal(peersText(null, null, 40), "≈40 peers");
  assert.equal(peersText(null, null), "peers unknown");
  assert.equal(peersText(5, null), "5 S / ? L");
  assert.equal(fileName("Season 1/E01.mkv"), "E01.mkv");
  assert.equal(fileName("dir/"), "dir");
  assert.equal(shortKey("npub1abcdefghijklmnopqrstuvwxyz"), "npub1abcdefg…uvwxyz");
  assert.equal(shortKey("short"), "short");
  assert.equal(formatRating(7.44), "★ 7.4");
  assert.equal(trustLevel(30, 20), "ok");
  assert.equal(trustLevel(10, 20), "low");
  assert.equal(trustLevel(10, 20, [], true), "negative");
  assert.equal(trustLevel(90, 20, ["BLOCKED"]), "negative");
});

Deno.test("stripHtml keeps text only, entities decoded once", () => {
  assert.equal(stripHtml("<p>a</p><p>b</p>"), "a\nb");
  assert.equal(stripHtml("x<br>y<br/>z"), "x\ny\nz");
  assert.equal(stripHtml("&lt;b&gt; &amp;amp; &#65;&#x42; &nbsp;"), "<b> &amp; AB");
  assert.equal(stripHtml("<script>console.error('XSS-FIRED')</script>t"), "console.error('XSS-FIRED')t");
  assert.equal(stripHtml('<a href="javascript:x">link</a> &amp; <p>para</p>'), "link & para");
  assert.equal(stripHtml("a\n\n\n\nb   \n"), "a\n\nb");
  assert.equal(stripHtml("&#1F;"), "&#1F;");
  assert.equal(stripHtml("&#99999999;"), "&#99999999;");
});

Deno.test("stats numbers (StatsFormatTest)", () => {
  assert.equal(formatNumber(1_234_567, "en"), "1,234,567");
  assert.equal(plain(formatNumber(1_234_567, "fr")), "1 234 567");
  assert.equal(formatCompact(950, "en"), "950");
  assert.equal(formatCompact(12_345, "en"), "12.3k");
  assert.equal(plain(formatCompact(12_345, "fr")), "12,3 k");
  assert.equal(formatCompact(958_106, "en"), "958k");
  assert.equal(formatCompact(1_234_567, "en"), "1.2M");
  assert.equal(formatCompact(11_510_920_895, "en"), "11.5B");
  assert.equal(formatCompact(999_950, "en"), "1M");
  assert.equal(formatCompact(-12_345, "en"), "-12.3k");
  assert.equal(formatShare(1, 4, "en"), "25%");
  assert.equal(formatShare(1, 1000, "en"), "<1%");
  assert.equal(formatShare(0, 1000, "en"), "0%");
  assert.equal(formatShare(5, 0, "en"), "–");
  assert.equal(formatUptime(42), "42 s");
  assert.equal(formatUptime(508), "8 min");
  assert.equal(formatUptime(3900), "1 h 05 min");
  assert.equal(formatUptime(3 * 86_400 + 4 * 3600 + 5), "3 d 4 h");
  assert.equal(backlogEta(0, 100), null);
  assert.equal(backlogEta(100, 0), null);
  assert.equal(backlogEta(50, 100), "under an hour");
  assert.equal(backlogEta(431_893, 30_000), "≈ 14 h");
  assert.equal(backlogEta(10_000_000, 30_000), "≈ 14 d");
  const at = 1_793_059_200 - 86_400;
  assert.equal(bucketLabel(at, false, "en"), "Oct 26");
  assert.equal(bucketLabel(at, false, "fr"), "26 oct.");
  assert.equal(bucketLabel(at, true, "en", "America/New_York"), "20:00");
  assert.equal(bucketLabel(at, true, "en", "UTC"), "00:00");
  // Axis ends of a 24-hour window name their day (second browser check: both read "17:00").
  assert.equal(axisLabel(at, true, "en", "UTC"), "Mon 00:00");
  assert.equal(axisLabel(at + 86_400, true, "en", "UTC"), "Tue 00:00");
  assert.equal(axisLabel(at, false, "en"), "Oct 26");
});
