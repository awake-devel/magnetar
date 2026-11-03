import assert from "node:assert/strict";
import {
  attrBadges,
  attrDetails,
  facetLabel,
  languageKey,
  languageLabel,
  mediaFacts,
  mediaHeadline,
  mediaLine,
  seasonEpisodeText,
  sourceLabel,
  statsLine,
} from "../js/labels.js";

// android/app/src/test/resources/release_attrs.jsonl: the Rust parser's output (LabelsTest).
const RUST = [
  {
    attrs: {
      french: ["multi", "vf2"],
      group: "FW",
      languages: ["fr", "fr-ca", "fr-fr", "original", "multi"],
      resolution: "1080p",
      source: "web-dl",
      video_codec: "h264",
      year: 2023,
    },
    language_key: "multi-vf2",
    language_label: "MULTi (VF2)",
    title: "Haunted.Mansion.2023.MULTi.VF2.1080p.WEB.H264-FW (Le manoir hanté)",
  },
  {
    attrs: {
      french: ["vfq"],
      group: "Slay3R",
      languages: ["fr", "fr-ca"],
      resolution: "720p",
      source: "web-dl",
      video_codec: "h264",
      year: 2024,
    },
    language_key: "vfq",
    language_label: "VFQ (Québec)",
    title: "Shook.2024.VFQ.720p.WEB-DL.H264-Slay3R (Choc)",
  },
  {
    attrs: {
      audio: ["eac3"],
      channels: "5.1",
      french: ["truefrench"],
      group: "FCK",
      languages: ["fr", "fr-fr"],
      resolution: "720p",
      source: "web-dl",
      video_codec: "h264",
      year: 2023,
    },
    language_key: "vff",
    language_label: "VFF (France)",
    title: "White.Men.Cant.Jump.2023.TRUEFRENCH.720p.DSNP.WEB-DL.DDP5.1.H.264-FCK",
  },
  {
    attrs: {
      audio: ["aac"],
      episode: 7,
      french: ["vostfr"],
      group: "ShC23",
      languages: ["ja", "original"],
      resolution: "1080p",
      season: 2,
      source: "webrip",
      subtitles: ["fr"],
      video_codec: "h265",
      year: 2018,
    },
    language_key: "vostfr",
    language_label: "VOSTFR",
    title: "Captain.Tsubasa.(2018).S02E07.VOSTFR.1080p.WEBRip.HEVC.AAC-ShC23",
  },
  {
    attrs: {
      audio: ["truehd", "atmos"],
      channels: "7.1",
      group: "GRP",
      hdr: ["dv", "hdr10"],
      resolution: "2160p",
      source: "remux",
      video_codec: "h265",
      year: 2024,
    },
    language_key: "unknown",
    language_label: "Unknown",
    title: "Dune.Part.Two.2024.2160p.UHD.BluRay.REMUX.DV.HDR10.HEVC.TrueHD.Atmos.7.1-GRP",
  },
  {
    attrs: { episode: 2, group: "GRP", resolution: "1080p", season: 1, source: "web-dl", video_codec: "h264" },
    language_key: "unknown",
    language_label: "Unknown",
    title: "Some.Show.S01E02.1080p.WEB.h264-GRP",
  },
  {
    attrs: { audio: ["flac"], bit_depth: 24, sample_rate: "96", year: 2020 },
    language_key: "unknown",
    language_label: "Unknown",
    title: "Artist - Album (2020) [FLAC 24bit 96kHz]",
  },
  {
    attrs: { formats: ["epub"], french: ["french"], languages: ["fr"], year: 2024 },
    language_key: "french",
    language_label: "French",
    title: "Some Book 2024.fr.epub",
  },
  {
    attrs: {
      audio: ["ac3"],
      french: ["multi", "vff"],
      group: "GRP",
      languages: ["fr", "fr-fr", "original", "multi"],
      resolution: "1080p",
      source: "bluray",
      video_codec: "h264",
      year: 2019,
    },
    language_key: "multi-vff",
    language_label: "MULTi (VFF)",
    title: "Film.2019.MULTi.VFF.1080p.BluRay.x264.AC3-GRP",
  },
  { attrs: {}, language_key: "unknown", language_label: "Unknown", title: "Plain name without tags" },
  {
    attrs: { languages: ["es", "es-419"], resolution: "1080p", source: "web-dl", video_codec: "h264", year: 2022 },
    language_key: "es",
    language_label: "Spanish",
    title: "Latino.Movie.2022.LATINO.1080p.WEB-DL.x264",
  },
  {
    attrs: { audio: ["mp3"], bitrate: "320" },
    language_key: "unknown",
    language_label: "Unknown",
    title: "Artist - Song [MP3 320kbps]",
  },
];

/** @param {string} title */
const attrsOf = (title) => {
  const r = RUST.find((x) => x.title === title);
  if (!r) throw new Error(title);
  return r.attrs;
};

Deno.test("language key and label match the Rust parser on every sample", () => {
  assert.equal(RUST.length, 12);
  for (const o of RUST) {
    assert.equal(languageKey(o.attrs), o.language_key, o.title);
    assert.equal(languageLabel(languageKey(o.attrs)), o.language_label, o.title);
  }
});

Deno.test("row badges as the app shows them", () => {
  assert.deepEqual(
    attrBadges(attrsOf("Dune.Part.Two.2024.2160p.UHD.BluRay.REMUX.DV.HDR10.HEVC.TrueHD.Atmos.7.1-GRP")),
    ["2160p", "Remux", "H.265/HEVC", "Dolby Vision", "TrueHD 7.1"],
  );
  assert.deepEqual(
    attrBadges(attrsOf("White.Men.Cant.Jump.2023.TRUEFRENCH.720p.DSNP.WEB-DL.DDP5.1.H.264-FCK")),
    ["720p", "WEB-DL", "H.264/AVC", "VFF (France)", "DD+ (E-AC-3) 5.1"],
  );
  assert.deepEqual(attrBadges(attrsOf("Artist - Album (2020) [FLAC 24bit 96kHz]")), ["FLAC", "24 bit"]);
  assert.deepEqual(attrBadges(attrsOf("Artist - Song [MP3 320kbps]")), ["MP3", "320 kbps"]);
  assert.deepEqual(attrBadges(attrsOf("Some Book 2024.fr.epub")), ["French", "EPUB"]);
  assert.deepEqual(attrBadges(attrsOf("Plain name without tags")), []);
  assert.deepEqual(
    attrDetails(attrsOf("Captain.Tsubasa.(2018).S02E07.VOSTFR.1080p.WEBRip.HEVC.AAC-ShC23")),
    ["1080p", "WEBRip", "H.265/HEVC", "VOSTFR", "AAC", "subs French", "group ShC23"],
  );
});

Deno.test("facet labels", () => {
  assert.equal(facetLabel("language", "fr-ca"), "French (Québec)");
  assert.equal(facetLabel("language", "multi"), "Multi-language");
  assert.equal(facetLabel("subtitles", "multi"), "Multi-subs");
  assert.equal(facetLabel("source", "web-dl"), "WEB-DL");
  assert.equal(facetLabel("resolution", "2160p"), "2160p (4K)");
  assert.equal(facetLabel("resolution", "1080p"), "1080p");
  assert.equal(facetLabel("genre", "science fiction"), "Science fiction");
  assert.equal(facetLabel("origin", "dht"), "DHT");
  assert.equal(facetLabel("category", "video"), "Video");
  assert.equal(facetLabel("language", "xx"), "XX");
  assert.equal(facetLabel("probe_sub_kind", "sign_language"), "Sign language");
  assert.equal(facetLabel("language", "constructor"), "CONSTRUCTOR");
});

Deno.test("media lines, facts and source labels", () => {
  const hit = {
    infohash: "a".repeat(40),
    media_title: "Dune: Part Two",
    media_year: 2024,
    media_rating: 8.5,
    total_size: 4.37 * 1024 ** 3,
    seeders: 12,
    leechers: 3,
    dead: true,
    created_at: 1_800_000_000 - 5 * 86400,
  };
  assert.equal(mediaLine(hit), "Dune: Part Two (2024) · ★ 8.5");
  assert.equal(mediaLine({ infohash: "b".repeat(40) }), null);
  assert.equal(statsLine(hit, 1_800_000_000), "4.37 GiB · 12 S / 3 L · dead · 5d");
  assert.equal(statsLine({ infohash: "c".repeat(40), best_peers: 40, created_at: 0 }, 60), "? · ≈40 peers · 1m");
  assert.equal(mediaHeadline("Show", 2011, 2019), "Show (2011–2019)");
  assert.equal(mediaHeadline("Show", 2011, 2011), "Show (2011)");
  assert.equal(mediaHeadline("Show", null), "Show");
  assert.equal(
    mediaFacts({ media_id: 1, kind: "movie", rating: 7.4, votes: 812_000, runtime_min: 155 }),
    "Movie · ★ 7.4 (812k) · 155 min",
  );
  assert.equal(mediaFacts({ media_id: 1, kind: "series", rating: 8, votes: 1_500_000 }), "Series · ★ 8.0 (1.5M)");
  assert.equal(seasonEpisodeText({ media_id: 1, season: 1, episode: 2, episode_title: "The Kingsroad" }), "S01E02 · The Kingsroad");
  assert.equal(seasonEpisodeText({ media_id: 1, season: 1 }), "Season 1");
  assert.equal(seasonEpisodeText({ media_id: 1 }), null);
  assert.equal(sourceLabel("dht"), "DHT (verified metadata, no publisher)");
  assert.equal(sourceLabel("both"), "Nostr and DHT");
  assert.equal(sourceLabel(undefined), "Nostr");
});
