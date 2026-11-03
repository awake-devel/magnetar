// Values from android/app/src/test/.../ProbeFormatTest.kt.
import assert from "node:assert/strict";
import {
  audioTrackText,
  channelsText,
  codecName,
  containerText,
  durationText,
  flagNotes,
  fpsText,
  gameLines,
  hdrNames,
  languageName,
  probeSummary,
  probeViews,
  subFormatClass,
  subtitleLines,
  subTrackText,
  videoTrackText,
} from "../js/probe-format.js";

const withProbe = JSON.parse(`{"summary":{"infohash":"${"a".repeat(40)}","title":"Dune"},
  "files":[{"path":"Dune/Dune.2024.2160p.mkv","size":6000000000},{"path":"Dune/sample.mkv","size":5},
           {"path":"Dune/game.pkg","size":900}],
  "probe":{"version":1,"probed_at":1793500000,"status":"partial","verified":false,"future":[1],
    "files":[
      {"index":2,"path":"Dune/game.pkg","size":900,"label":"game.ps5.pkg","description":"PS5 package",
       "confidence":90,"verified":false,"flags":["extension_mismatch","suspicious_executable","incomplete"],
       "game":{"platform":"ps5","kind":"package","format":"PS5 FIH","title_id":"PPSA01234",
               "content_id":"EP0000-PPSA01234_00-GAME000000000000","title":"Some Game","region":"EU","version":"01.000"}},
      {"index":0,"path":"Movie.2024.2160p.mkv","size":6000000000,"label":"video.mkv",
       "description":"Matroska video","confidence":97,"verified":true,"flags":["agreed"],
       "video":{"container":"matroska","duration_ms":7200000,"bitrate_kbps":15200,"new_field":true,
                "video":[{"codec":"hevc","profile":"Main 10","width":3840,"height":2160,"fps_milli":23976,
                          "bit_depth":10,"hdr":["dolby_vision","hdr10"],"dv_profile":8,"dv_compat":1}],
                "audio":[{"codec":"eac3","channels":6,"lang":"fre","atmos":true},
                         {"codec":"dts","channels":8,"lang":"eng","dts_x":true},
                         {"codec":"aac","channels":2}],
                "subs":[{"codec":"subrip","lang":"eng"},{"codec":"hdmv_pgs_subtitle","lang":"fre"},
                        {"codec":"subrip","lang":"eng"},{"codec":"ass"}]}}]}}`);

Deno.test("no section without a probe or a probed file", () => {
  assert.equal(probeViews(undefined, []), null);
  assert.equal(probeViews({ status: "ok" }, []), null);
});

Deno.test("files are mapped by index into the shown list, in list order", () => {
  const views = /** @type {NonNullable<ReturnType<typeof probeViews>>} */ (probeViews(withProbe.probe, withProbe.files));
  assert.deepEqual(views.map((v) => v.name), ["Dune.2024.2160p.mkv", "game.pkg"]);
  const movie = views[0];
  assert.equal(movie.headline, "Matroska video · 97% · verified · 5.59 GiB");
  assert.ok(movie.verified);
  assert.deepEqual(movie.lines, [
    "Matroska · 2h 00m · 15.2 Mb/s",
    "Video: HEVC Main 10 · 3840×2160 · 23.976 fps · 10-bit · Dolby Vision 8.1, HDR10",
    "Audio: French · E-AC-3 5.1 · Dolby Atmos",
    "Audio: English · DTS 7.1 · DTS:X",
    "Audio: AAC 2.0",
    "Subtitle: English · SRT ×2",
    "Subtitle: French · PGS",
    "Subtitle: unknown · ASS",
    "Two peers agreed",
  ]);
  assert.deepEqual(movie.warnings, []);
  const game = views[1];
  assert.equal(game.headline, "PS5 package · 90% · unverified · 900 B");
  assert.deepEqual(game.warnings, ["Suspicious executable", "The file name's extension does not match its contents"]);
  assert.deepEqual(game.lines, [
    "PS5 package (PS5 FIH)",
    "Title: Some Game",
    "Title ID PPSA01234 · Content ID EP0000-PPSA01234_00-GAME000000000000",
    "Region EU · Version 01.000",
    "Read partly",
  ]);
  const outside = /** @type {NonNullable<ReturnType<typeof probeViews>>} */ (probeViews(withProbe.probe, []));
  assert.equal(outside[0].name, "Movie.2024.2160p.mkv");
});

Deno.test("summary says status, verification and age", () => {
  const now = 1793500000 + 3 * 86_400;
  assert.equal(probeSummary({ probed_at: 1793500000, status: "ok", verified: true }, now), "Complete · all hash-checked · probed 3d ago");
  assert.equal(probeSummary({ probed_at: 0, status: "partial", verified: false }, now), "Partial · not all hash-checked");
});

Deno.test("languages, codecs and numbers read like people say them", () => {
  assert.equal(languageName("fre"), "French");
  assert.equal(languageName("deu"), "German");
  assert.equal(languageName("JPN"), "Japanese");
  assert.equal(languageName("tlh"), "tlh");
  assert.equal(languageName("constructor"), "constructor");
  assert.equal(codecName("truehd"), "TrueHD");
  assert.equal(codecName("newcodec"), "NEWCODEC");
  assert.equal(channelsText(1), "mono");
  assert.equal(channelsText(6), "5.1");
  assert.equal(channelsText(12), "12 ch");
  assert.equal(fpsText(25000), "25 fps");
  assert.equal(fpsText(29970), "29.97 fps");
  assert.equal(fpsText(23976), "23.976 fps");
  assert.equal(durationText(2_700_000), "45m");
  assert.equal(durationText(5_430_000), "1h 30m");
  assert.equal(durationText(42_000), "42s");
});

Deno.test("hdr names with and without a Dolby Vision profile", () => {
  assert.deepEqual(hdrNames({ hdr: ["dolby_vision"] }), ["Dolby Vision"]);
  assert.deepEqual(hdrNames({ hdr: ["dolby_vision"], dv_profile: 5 }), ["Dolby Vision profile 5"]);
  assert.deepEqual(hdrNames({ hdr: ["dolby_vision"], dv_profile: 5, dv_compat: 0 }), ["Dolby Vision profile 5"]);
  assert.deepEqual(hdrNames({ hdr: ["dolby_vision", "hdr10_plus", "hlg"], dv_profile: 8, dv_compat: 4 }), [
    "Dolby Vision 8.4",
    "HDR10+",
    "HLG",
  ]);
  assert.equal(videoTrackText({ codec: "h264", height: 720 }), "H.264 · 720p");
  assert.equal(videoTrackText({ codec: "av1" }), "AV1");
});

Deno.test("sparse tracks leave missing parts out", () => {
  assert.equal(audioTrackText({ codec: "opus" }), "Opus");
  assert.equal(containerText({ container: "avi" }), "AVI");
  assert.deepEqual(subtitleLines({}), []);
  assert.deepEqual(subtitleLines({ subs: [{ codec: "subrip", lang: "ita" }] }), ["Subtitle: Italian · SRT"]);
  assert.deepEqual(gameLines({ platform: "wii_or_game_cube" }), ["Wii or GameCube"]);
  assert.deepEqual(
    gameLines({ platform: "switch", kind: "compressed_image", title_id: "0100000000010000", db_id: "0100000000010000" }),
    ["Nintendo Switch compressed image", "Title ID 0100000000010000"],
  );
});

Deno.test("every surprise note is shown", () => {
  const [warnings, notes] = flagNotes(["surprise_verified", "surprise_changed", "surprise_unverified"]);
  assert.deepEqual(warnings, []);
  assert.deepEqual(notes, [
    "surprising result, confirmed on hash-checked pieces",
    "first guess corrected on hash-checked pieces",
    "surprising result, not confirmed on hash-checked pieces",
  ]);
});

Deno.test("kinds are listed, names are never shown in the plain lines", () => {
  const d = JSON.parse(`{"files":[{"path":"Movie.mkv","size":1}],
    "probe":{"version":2,"status":"ok","verified":true,"files":[{"index":0,"path":"Movie.mkv","size":1,
      "description":"Matroska video","confidence":97,"verified":true,
      "video":{"container":"matroska","duration_ms":7200000,
       "video":[{"codec":"h264","width":1920,"height":1080}],
       "audio":[{"codec":"eac3","channels":6,"lang":"fre"},
                {"codec":"ac3","channels":2,"lang":"eng","kinds":["commentary"],"name":"Director's Commentary","future":1},
                {"codec":"aac","channels":2,"lang":"fre","kinds":["description"],"name":"Audiodescription"}],
       "subs":[{"codec":"hdmv_pgs_subtitle","lang":"eng","kinds":["sdh"],"name":"English (SDH)"},
               {"codec":"subrip","lang":"fre","kinds":["forced"],"name":"Français (forcés)","role":"x"},
               {"codec":"subrip","lang":"fre"},
               {"codec":"eia_608","kinds":["cc"]},
               {"codec":"ass","lang":"jpn","kinds":["signs"],"name":"Signs & Songs"}]}}]}}`);
  const lines = /** @type {NonNullable<ReturnType<typeof probeViews>>} */ (probeViews(d.probe, d.files))[0].lines;
  assert.deepEqual(lines, [
    "Matroska · 2h 00m",
    "Video: H.264 · 1920×1080",
    "Audio: French · E-AC-3 5.1",
    "Audio: English · AC-3 2.0 · commentary",
    "Audio: French · AAC 2.0 · audio description",
    "Subtitle: English · SDH · PGS",
    "Subtitle: French · forced · SRT",
    "Subtitle: French · SRT",
    "Subtitle: unknown · CC · CEA-608",
    "Subtitle: Japanese · signs & songs · ASS",
  ]);
  assert.ok(lines.every((l) => !l.includes("Director") && !l.includes("Audiodescription") && !l.includes("forcés")));
});

Deno.test("audio kinds are listed after the format", () => {
  assert.equal(
    audioTrackText({ codec: "eac3", channels: 6, lang: "fre", atmos: true, kinds: ["description"] }),
    "French · E-AC-3 5.1 · Dolby Atmos · audio description",
  );
  assert.equal(
    audioTrackText({ codec: "aac", kinds: ["future_kind", "music", "original", "voiceover", "dialogue", "alternate", "karaoke", "dub"] }),
    "AAC · original · dub · voice-over · clear dialogue · music only · karaoke · alternate mix · future kind",
  );
  assert.equal(audioTrackText({ codec: "aac", name: "Director's Commentary" }), "AAC");
});

Deno.test("subtitle lines group identical tracks and cap at ten", () => {
  const v = {
    subs: [
      { codec: "hdmv_pgs_subtitle", lang: "eng", kinds: ["sdh"] },
      { codec: "subrip", lang: "fre", kinds: ["forced"] },
      { codec: "subrip", lang: "fre" },
      { codec: "eia_608", lang: null, kinds: ["cc"] },
      { codec: "subrip", lang: "fre", name: "another name, same line" },
      { codec: "ass", lang: "jpn", kinds: ["signs"] },
    ],
  };
  assert.deepEqual(subtitleLines(v), [
    "Subtitle: English · SDH · PGS",
    "Subtitle: French · forced · SRT",
    "Subtitle: French · SRT ×2",
    "Subtitle: unknown · CC · CEA-608",
    "Subtitle: Japanese · signs & songs · ASS",
  ]);
  assert.equal(
    subTrackText({
      codec: "subrip",
      lang: "eng",
      kinds: ["easy_reader", "bilingual", "honorifics", "description", "commentary", "signs", "cc", "sdh", "forced", "new_one"],
    }),
    "English · forced · SDH · CC · signs & songs · commentary · descriptions · honorifics · bilingual · easy reader · new one · SRT",
  );
  const codes = ["eng", "fre", "ger", "spa", "ita", "por", "rus", "jpn", "chi", "kor", "ara", "dut", "swe", "nor", "dan"];
  const lines = subtitleLines({ subs: [...codes.map((c) => ({ codec: "subrip", lang: c })), { codec: "subrip", lang: "dan" }] });
  assert.equal(lines.length, 11);
  assert.equal(lines[0], "Subtitle: English · SRT");
  assert.equal(lines[9], "Subtitle: Korean · SRT");
  assert.equal(lines[10], "Subtitles: 6 more tracks");
  assert.equal(subtitleLines({ subs: codes.map((c) => ({ codec: "subrip", lang: c })) })[10], "Subtitles: 5 more tracks");
  assert.equal(subtitleLines({ subs: codes.slice(0, 11).map((c) => ({ codec: "subrip", lang: c })) })[10], "Subtitles: 1 more track");
  assert.deepEqual(subtitleLines({ subs: Array.from({ length: 30 }, () => ({ codec: "subrip", lang: "eng" })) }), [
    "Subtitle: English · SRT ×30",
  ]);
});

Deno.test("new subtitle codec names and the format classes", () => {
  assert.deepEqual(
    ["eia_708", "ttml", "arib_caption", "dvb_teletext", "hdmv_text_subtitle", "xsub", "kate", "usf"].map(codecName),
    ["CEA-708", "TTML", "ARIB", "Teletext", "TextST", "XSUB", "Kate", "USF"],
  );
  for (const c of ["subrip", "ass", "ssa", "webvtt", "mov_text", "ttml", "hdmv_text_subtitle", "dvb_teletext", "arib_caption", "kate", "usf"]) {
    assert.equal(subFormatClass(c), "text", c);
  }
  for (const c of ["hdmv_pgs_subtitle", "dvd_subtitle", "dvb_subtitle", "xsub"]) assert.equal(subFormatClass(c), "image", c);
  for (const c of ["eia_608", "eia_708"]) assert.equal(subFormatClass(c), "cc", c);
  assert.equal(subFormatClass("unknown_codec"), null);
});
