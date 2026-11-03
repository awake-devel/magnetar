// Port of android/.../ui/common/NumberFormat.kt: the Stats numbers in the browser's locale
// (docs/STATS.md "Colours and numbers"). Every function takes the locale explicitly in tests.

/** @returns {string} the browser's locale, or "en". */
export function defaultLocale() {
  try {
    return navigator.language || "en";
  } catch {
    return "en";
  }
}

/** @param {string} locale */
function lang(locale) {
  return locale.toLowerCase().split(/[-_]/)[0];
}

/**
 * Grouped integer: "1,234,567" (en), "1 234 567" (fr).
 * @param {number} n
 * @param {string} [locale]
 */
export function formatNumber(n, locale = defaultLocale()) {
  return new Intl.NumberFormat(locale, { maximumFractionDigits: 0 }).format(Math.round(n));
}

/**
 * Short form for chart labels: "950", "12.3k", "1.2M", "3.4B"; French "12,3 k".
 * @param {number} v
 * @param {string} [locale]
 * @returns {string}
 */
export function formatCompact(v, locale = defaultLocale()) {
  const a = Math.abs(v);
  if (a < 1000) return formatNumber(v, locale);
  let div, unit;
  if (a < 1e6) [div, unit] = [1e3, "k"];
  else if (a < 1e9) [div, unit] = [1e6, "M"];
  else [div, unit] = [1e9, "B"];
  const x = v / div;
  const digits = Math.abs(x) < 100 ? 1 : 0;
  const rounded = digits === 1 ? Math.round(x * 10) / 10 : Math.round(x);
  // 999 950 rounds to "1000k": say "1M" instead.
  if (unit !== "B" && Math.abs(rounded) >= 1000) {
    return formatCompact(v < 0 ? -div * 1000 : div * 1000, locale);
  }
  const text = new Intl.NumberFormat(locale, { maximumFractionDigits: digits, minimumFractionDigits: 0 })
    .format(rounded);
  return text + (lang(locale) === "fr" ? " " : "") + unit;
}

/**
 * "42%" ("42 %" in French), no decimals.
 * @param {number} share
 * @param {string} [locale]
 */
export function formatPercent(share, locale = defaultLocale()) {
  return new Intl.NumberFormat(locale, { style: "percent", maximumFractionDigits: 0 }).format(share);
}

/**
 * Share of n in total: "42%", "<1%" for a small non-zero share, "–" without a total.
 * @param {number} n
 * @param {number} total
 * @param {string} [locale]
 */
export function formatShare(n, total, locale = defaultLocale()) {
  if (total <= 0) return "–";
  const share = n / total;
  if (n > 0 && share < 0.005) return "<" + formatPercent(0.01, locale);
  return formatPercent(share, locale);
}

/**
 * "42 s", "12 min", "1 h 05 min", "3 d 4 h".
 * @param {number} secs
 */
export function formatUptime(secs) {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.floor(s / 60)} min`;
  if (s < 86_400) return `${Math.floor(s / 3600)} h ${String(Math.floor((s % 3600) / 60)).padStart(2, "0")} min`;
  return `${Math.floor(s / 86_400)} d ${Math.floor((s % 86_400) / 3600)} h`;
}

/**
 * "4.5/min" (one decimal under 10), "18,146/min".
 * @param {number} perUnit
 * @param {string} [unit]
 * @param {string} [locale]
 */
export function formatRate(perUnit, unit = "min", locale = defaultLocale()) {
  const n = perUnit < 10
    ? new Intl.NumberFormat(locale, { maximumFractionDigits: 1, minimumFractionDigits: 1 }).format(perUnit)
    : formatNumber(perUnit, locale);
  return `${n}/${unit}`;
}

/**
 * Time to clear what is due at the last hour's pace (D45); null when nothing is due.
 * @param {number} due
 * @param {number} checksPerHour
 * @returns {string | null}
 */
export function backlogEta(due, checksPerHour) {
  if (due <= 0 || checksPerHour <= 0) return null;
  const hours = due / checksPerHour;
  if (hours < 1) return "under an hour";
  if (hours < 48) return `≈ ${Math.round(hours)} h`;
  return `≈ ${Math.round(hours / 24)} d`;
}

/**
 * Bucket start → "Oct 12" / "12 oct." (days, UTC) or "14:00" (hours, the browser's zone).
 * @param {number} at
 * @param {boolean} hourly
 * @param {string} [locale]
 * @param {string} [timeZone]
 */
export function bucketLabel(at, hourly, locale = defaultLocale(), timeZone) {
  const d = new Date(at * 1000);
  if (hourly) {
    return new Intl.DateTimeFormat(locale, { hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZone })
      .format(d);
  }
  return new Intl.DateTimeFormat(locale, { month: "short", day: "numeric", timeZone: timeZone ?? "UTC" }).format(d);
}

/**
 * A chart axis end: as bucketLabel, but an hour also names its day ("Mon 17:00"), so the two
 * ends of a 24-hour window are not both "17:00".
 * @param {number} at
 * @param {boolean} hourly
 * @param {string} [locale]
 * @param {string} [timeZone]
 */
export function axisLabel(at, hourly, locale = defaultLocale(), timeZone) {
  if (!hourly) return bucketLabel(at, false, locale, timeZone);
  const d = new Date(at * 1000);
  const day = new Intl.DateTimeFormat(locale, { weekday: "short", timeZone }).format(d);
  return `${day} ${bucketLabel(at, true, locale, timeZone)}`;
}
