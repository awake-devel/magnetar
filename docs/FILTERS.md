# Filters: language, quality and more (ARCHITECTURE.md D44)

Magnetar reads what a release name says about the release (resolution, source, codecs, HDR,
audio, audio and subtitle languages, group, season/episode, year, editions, music and book
formats), stores it per infohash and lets the app filter on it, with live counts next to every
choice, the way private trackers do. A per-title page lists every release of one movie or
series grouped by season/episode, quality and language.

| What | Where |
|---|---|
| Parser (no dependencies, server and phone) | `crates/core/src/release.rs` (`PARSER_VERSION`) |
| Hand-checked sample + regression test | `crates/core/tests/data/release_sample.tsv`, `crates/core/tests/release_sample.rs` |
| Try names by hand | `printf 'video\tSome.Title.2024.MULTi.1080p.WEB.x264-GRP\n' \| cargo run -q -p magnetar-core --example release_attrs` (`category<TAB>title[<TAB>files JSON]` per line, JSON out) |
| Postgres table | `torrent_attrs` (migration `20261029000000_release_attrs.sql`) |
| Backfill, `magnetar-server attrs` | `crates/server/src/attrs.rs` |
| Index facets/filters | `crates/server/src/search_index/{schema,collect}.rs` (`SCHEMA_VERSION` 2) |
| SQL fallback filters | `crates/server/src/search.rs` (`push_attr_filters`) |
| Phone cache | SQLite V4 `torrent_attrs`, `crates/ffi/src/store.rs` (`push_attr_filters`, `backfill_attrs`) |
| App | `ui/search/FilterSheet.kt`, `ui/media/MediaScreen.kt` |

## Parsing

`release::parse_release(title, files, category)` cuts the name into tokens at separators
(`.` `_` space brackets `-` `+` `/` …; `DD+5.1`, `WEB-DL`, `DTS-HD`, `HDR10+`, `Blu-ray`,
`Dual-Audio`, `Multi-Subs`, `VOST-FR`, … are folded into one token first).

**Tag region.** Many tags are also title words (`French Kiss`, `The Italian Job`, `IT`, `DE`,
`VO`, `DV`, `TS`, `Web`). Those only count in the tag region, which starts at the first
unambiguous tag after the first word: a year, `S01E02`/`1x02`, `1080p`, `WEB-DL`, `x264`,
`VOSTFR`, `MULTi`, `TRUEFRENCH`, … An ambiguous language word also needs to be uppercase or
next to another tag (`S01E05.The.French.Connection.1080p` has no language; `….Cowboys.French.HDTV`
is French), or inside a bracket holding only language words (`[ENG]`, `[Hindi+English]`).
Scene tags that are never title words (`VFF`, `VFQ`, `VOSTFR`, `MULTi`, …) count anywhere but
on the first word.

**Files.** When the name has no resolution, source or codec (DHT-style names, bare folder
names), the largest video file's name is parsed and fills what is missing. Subtitle files
(`.srt .ass .ssa .sub .idx .vtt .sup .smi`) add subtitle languages from their last words
(`Movie.fr.srt`, `Subs/2_English.srt`, `Movie.eng.forced.srt`, `Movie.pt-BR.srt`); audio track
files next to a video (`.mka .ac3 .eac3 .dts .thd .aac`) add audio languages when the name has
none. Music torrents without a codec in the name take the most common audio file type; book
torrents list every book format among their files.

**Stored once per infohash.** `PgStore::put_torrent` parses the first event of an infohash
(same files for every publisher; their titles differ little) into `torrent_attrs`. A row of an
older `PARSER_VERSION` is re-parsed: by `serve` at startup (one pass over `torrent_infohashes`
in chunks of 2000, parsing on blocking threads, 20 ms between chunks) or by
`magnetar-server attrs [--force]`; a failed startup pass is retried every 5 minutes until it
completes. The phone does the same in its SQLite cache (V4), at ingest and in the background
after `Magnetar::open`, 500 infohashes per lock, walking the infohash index with a cursor.

## Values

### Languages

`languages` lists audio languages, `subtitles` subtitle languages, both as ISO 639-1 codes plus
regional variants and two pseudo-languages. `french` keeps the French scene tags as written, for
the per-title grouping.

| In the name | `languages` | `subtitles` | `french` |
|---|---|---|---|
| `VFF`, `TRUEFRENCH` | fr, fr-fr | | vff / truefrench |
| `VFQ`, `VQ` | fr, fr-ca | | vfq |
| `VF2` (France + Québec dubs) | fr, fr-fr, fr-ca | | vf2 |
| `VFI` (international French) | fr | | vfi |
| `VOF` (French original) | fr, original | | vof |
| `FRENCH`, `VF`, `FRA`, `FRE`, `Français` | fr | | french |
| `MULTi` (films, series, anime, adult) | multi, fr, original | | multi |
| `MULTi` (software, games, books, music), `MULTi12` | multi | | |
| `MULTi.VFF`, `MULTi.VFQ`, … | the union of both rows | | multi, vff, … |
| `VOSTFR`, `VOSTF`, `VOST-FR` | original | fr | vostfr |
| `SUBFRENCH`, `SUBFR`, `SUB.FRENCH` | original | fr | subfrench |
| `VOFSTFR` / `VFFSTFR` / `VFSTFR` | as VOF / VFF / VF | fr | |
| `STFR` | | fr | |
| `VOSTA`, `VOSTEN` | original | en | |
| `VO` | original | | vo |
| `Dual Audio`, `DUAL` | multi | | |
| `MultiSub`, `Multi-Subs` | | multi | |
| `ENG`/`ENGLISH`/`EN`, `GER`/`GERMAN`/`DEUTSCH`/`DE`, `ITA`/`ITALIAN`/`IT`, `SPA`/`ESP`/`SPANISH`/`CASTELLANO`/`ES`, `JPN`/`JAP`/`JAPANESE`/`JP`, `KOR`/`KOREAN`/`KR`, `RUS`/`RUSSIAN`/`RU`, `POR`/`PORTUGUESE`/`PT`, `CHI`/`CHINESE`/`MANDARIN`/`CANTONESE`/`CN`, `NLD`/`DUTCH`/`NL`, `POL`/`POLISH`/`PL`, `HINDI`, `TURKISH`, `ARABIC`, `SWEDISH`, `NORWEGIAN`, `DANISH`, `FINNISH`, `CZECH`, `HUNGARIAN`, `UKRAINIAN`, `THAI`, `VIETNAMESE`, `GREEK`, `HEBREW`, `TAMIL`, `TELUGU`, `ROMANIAN`; French names `ANGLAIS`, `ALLEMAND`, `ITALIEN`, `ESPAGNOL`, `JAPONAIS`, … | en, de, it, es, ja, ko, ru, pt, zh, nl, pl, hi, tr, ar, sv, no, da, fi, cs, hu, uk, th, vi, el, he, ta, te, ro | | french (for fr) |
| `LATINO`, `LAT` | es-419, es | | |
| `PT-BR`, `BRAZILIAN` | pt-br, pt | | |
| a language + `SUB`/`SUBS`/`SUBBED` (`ENG-SUB`, `[JPN_SUB]`, `Sub_Esp`, `ESub`, `subita`) | | that language | |
| a language + `Dub` (`[English Dub]`) | that language | | |
| `CHS`, `CHT`, `BIG5`, `[GB]`, `简`/`繁`/`字幕`/`中字`/`内封`/`内嵌` | | zh | |
| `中配`, `国语`/`國語`, `粤语`/`粵語`, `国漫`; Erai-raws `(CA)` | zh | | |
| `英配` | en | | |

Two-letter codes count only uppercase in the tag region or next to an unambiguous tag (`EN -
PDF`); lowercase `fr` only next to a book format (`2024.fr.epub`).

**Anime.** In the anime category a release whose name names no audio language, or only
`original` (VOSTFR, SUBFRENCH) or `multi`, gets `ja`; dual-audio fansubs (`[Dual-Audio]`,
`DUAL` with nothing else) get `ja` and `en`. A French dub only (`VFF` alone) or an English
dub (`[English Dub]`) does not. Chinese animation in the anime category is only recognised by
the markers above (`国漫`, `中配`, `(CA)`); otherwise it gets `ja` too (the known miss below).

The per-title view groups by `ReleaseAttrs::language_key`: `multi-vff`, `multi-vfq`,
`multi-vf2`, `multi-vfi`, `multi`, `vff`, `vfq`, `vf2`, `vfi`, `vof`, `vostfr`, `french`, else
the first language code, `original` or `unknown`; labels such as "MULTi (VFF)", "VFQ (Québec)"
come from `release::language_label`.

### Quality and the rest

| Field | Values | From |
|---|---|---|
| resolution | 4320p, 2160p, 1440p, 1080p, 720p, 576p, 480p, sd | `1080p`/`1080i`, `1920x1080`, `HD1080`, `BD720`, `4K`/`UHD`/`4KLight` (2160p, unless an explicit one is given), `FHD`; bare `1080` next to a source tag or alone in brackets; heights are bucketed (`816p` → 1080p); DVD sources → sd |
| source | remux, bluray, web-dl, webrip, hdrip, hdtv, dvd, vhs, screener, telesync, cam | `REMUX`/`BDRemux`; `BluRay`/`BDRip`/`BRRip`/`BD`/`BDMV`; `WEB-DL`/`WEB`/`WEB.DL`, or a streaming tag alone (`AMZN`, `NF`, `DSNP`, `HMAX`, `ATVP`, `PCOK`, `PMTP`, `CR`, `ADN`, …); `WEBRip`/`WEB.Rip`; `HDRip`; `HDTV`/`PDTV`/`DSR`/`TVRip`/`SATRip`/`DVB`; `DVDRip`/`DVD`/`DVD5`/`DVD9`/`FullDVD`; `VHSRip`; `DVDSCR`/`SCR`/`Screener`/`R5`; `TS`/`HDTS`/`TELESYNC`/`TC`/`TELECINE`; `CAM`/`HDCAM`. Remux beats the others, then CAM/TS/screener, BluRay, WEBRip, WEB-DL |
| video_codec | h265, h264, av1, vp9, xvid, mpeg2, vc1 | `x265`/`H.265`/`HEVC`, `x264`/`H.264`/`AVC`, `AV1`, `VP9`, `XviD`/`DivX`, `MPEG2`, `VC-1` |
| hdr | dv, hdr10+, hdr10, hlg | `DV`/`DoVi`/`Dolby Vision`, `HDR10+`/`HDR10Plus`, `HDR`/`HDR10`, `HLG` |
| bit_depth | 8, 10, 12 (video), 16, 24 (audio) | `10bit`, `Hi10P`, `10 bits`, `24 Bits`, `16b`, `FLAC16BITS` |
| audio | truehd, atmos, dts-x, dts-hd, dts, eac3, ac3, aac, flac, opus, mp3, pcm, alac, vorbis, dsd | `TrueHD`, `Atmos`, `DTS:X`, `DTS-HD MA`/`HDMA`/`DTSHDMA`, `DTS`, `DDP`/`DD+`/`EAC3`, `DD`/`AC3`, `AAC`, `FLAC`, `Opus`, `MP3`, `LPCM`/`PCM`, `ALAC`, `OGG`, `DSD` |
| channels | 1.0, 2.0, 5.1, 7.1 | `5.1`, `DDP5.1`, `AAC2.0`, `DD 5 1`, `6CH`/`8CH`/`2CH` |
| group | as written | `…-GROUP` at the end (before a trailing `(alt title)`, `[hash]`, `[site]`), `-Tsundere-Raws`; a leading `[Group]` for anime and fansub-style names; `NoTag`/`NOGRP` = none |
| season, season_end, episode, episode_end, complete | numbers, bool | `S01E02`, `S01E02E03`, `S01E01-E03`, `S01-S05`, `S1+S2+S3`, `1x02`, `E08`/`EP06`/`Episode 6`, `Season 1-5`, `Saison 1 à 3`, `2nd Season - 01`, anime `Title - 07`, `(01-12)`, `13v2`, `[05]`, `第26话`; `COMPLETE`/`INTEGRALE`/`Batch` (not `COMPLETE.BLURAY`, a full disc) |
| year | 1900–2099 | the first year followed by a tag or alone in brackets (`Blade.Runner.2049.2017.1080p` → 2017, `L'Armée des Ombres 1969 REMASTERISÉ 2024` → 1969), a range's start (`2008-2013`), else the last before the first tag |
| editions | repack, proper, extended, uncut, unrated, directors-cut, imax, remastered, theatrical, criterion, limited, internal, 3d, open-matte, hybrid, custom, light, fansub, uncensored | `HDLight`/`mHD`/`4KLight`/`1080p Light` are `light` |
| bitrate | 320, 256, …, v0, v2 | `320kbps`, `320 Kb/s`, `MP3.320`, `V0` |
| sample_rate | 44.1, 48, 96, 192, … | `44.1kHz`, `44,1 kHz`, `44100Hz` |
| formats | epub, pdf, mobi, azw3, cbz, cbr, djvu, fb2, m4b | name tokens and file extensions |

## Filters

`SearchQuery` (all `serde(default)`, so older clients send nothing and older servers ignore
them) gains `languages`, `subtitles`, `resolutions`, `sources`, `video_codecs`, `hdr`, `audio`,
`genres`, `media_kinds`, `min_rating`, `min_year`, `max_year`, and `Sort::Rating`. Within one
field any value matches, fields combine with AND. Values are the codes above (lowercased by the
server). `genres`, `media_kinds` and `min_rating` look at the linked media (D40, links of
confidence ≥ 0.5); the year range at the media's year, else the year in the name; unrated or
undated results fail those bounds. Saved searches store the whole query, so the notifier and
the phone's local notifications apply the filters too.

| `SearchQuery` field | query string | facet |
|---|---|---|
| languages | `lang` | `language` |
| subtitles | `sub` | `subtitles` |
| resolutions | `res` | `resolution` |
| sources | `source` | `source` |
| video_codecs | `codec` | `codec` |
| hdr | `hdr` | `hdr` |
| audio | `audio` | `audio` |
| genres | `genre` | `genre` |
| media_kinds | `kind` | `media_kind` |
| min_rating, min_year, max_year | `min_rating`, `min_year`, `max_year` | `year` |
| sort | `sort=rating` | |
| include_adult (D52) | `adult=1` | |

Further facets (counted, not filterable yet): `channels`, `edition`, `format`, `group`,
plus D43's `category`, `category_path`.

**Counts.** `POST /api/v1/facets` with `{"query": SearchQuery, "fields": [...], "size": n}`
returns `FacetSet {total, facets: [FacetsResponse]}`. Counts are disjunctive: each facet is
counted over the results of every filter **except its own**, so with `languages=[fr-ca]` the
language chips still show how many results English or VFF would give, while the resolution
chips count only the fr-ca results. One pass over the matches serves every requested facet (a
document failing one attribute filter counts only for that filter's facet). `GET
/api/v1/facets?field=` (D43) uses the same rule. 503 while the index is building.

**Per-title view.** `GET /api/v1/media/{id}/releases?<search parameters>` (or `POST` with a
`SearchQuery`) returns `MediaReleases {media, total, truncated, groups}`: the releases linked to
the media passing the query's filters, best seeded first, at most 1000, grouped by season and
episode (series and anime: complete packs first, then season packs, then episodes), then
resolution (best first), then language key; each group's releases sorted by seeders (unknown
last), then date.

**Adult content (D52, D70).** `include_adult` (default false) hides adult torrents from search,
browse, facet and Stats counts and saved-search notifications: canonical category `porn`, or a
DHT row the classifier tagged `porn`. Selecting the `porn` category shows them (choosing it is
consent). Older apps send nothing and get the default. The app's Settings → "Show adult
content" (`FfiConfig.show_adult`) sets it on every query the phone makes; the phone's own
SQLite search hides an infohash when any of its events is filed under `porn`.

**Capabilities.** `GET /health` now lists `features`: `filters`, `facet_set`,
`media_releases`, and since D70 `adult_filter` (a server without it ignores `include_adult`;
the app then drops `porn` hits itself). The app shows the filters a server cannot apply only while they are set
(so they can be cleared), never offers rating sort to a server without `filters`, and falls
back to the local cache's grouping for the title page.

## Phone, offline and older servers

| | no server / unreachable | server without `filters` (older) | server with `filters` |
|---|---|---|---|
| language, subtitles, resolution, source, codec, HDR, audio, year | local cache (`torrent_attrs`, V4) | hidden | server |
| genre, type, min rating, rating sort | hidden (the cache only knows media of details opened online) | hidden | server |
| counts next to chips | no (built-in value lists) | no | yes (`facet_set`; none while the index builds) |
| title page | cached releases linked to the media | cached releases | server |

A query that needs `filters` (any release/media filter or rating sort, e.g. from a saved search
made against a newer server) is never sent to a server whose `/health` does not list it: that
server would ignore the filters and answer unfiltered results. The ffi asks `/health` once per
configuration and searches the local cache instead; saved-search notifications do the same.

## Performance (2026-10-26, this laptop: 12 threads, NVMe, Postgres 18 in docker)

On a copy of the production database (`scripts/bench-search-db.sh 1`: 471 898 events, 467 400
infohashes; read-only `COPY … TO STDOUT` from production), release build:

| | |
|---|---|
| Backfill (`magnetar-server attrs`, as `serve` does at the first start) | 467 400 infohashes parsed and written in 73.9 s (6 300/s); `torrent_attrs` 180 MB |
| Coverage | audio language 65.7 %, subtitles 17.1 %, resolution 66.9 %, source 61.3 %, video codec 59.0 %, audio codec 40.0 %, group 60.1 %, year 46.4 % of infohashes |
| Most frequent languages | fr 166 409, original 115 967, ja 95 417, multi 79 081, fr-fr 19 277, en 14 189, fr-ca 7 280, it 1 370, zh 1 275 |
| Index build (schema 2) | 467 400 documents in 29.8 s; 387 MiB on disk (the attribute facets and the stored `attrs` add about a fifth) |
| Queries (`tests/search_bench.rs`, 43 queries × 25 runs, D43's set plus 6 filtered ones) | all: p50 8.0 ms, p95 22.4 ms, max 42.7 ms; the filtered ones (`lang fr-ca`, `fr + 1080p + h265`, `fr subs "one piece"`, `remux + dv + atmos`, `drama, rating ≥ 7, 2010–2020`, rating sort over "french"): p50 2.9–16.1 ms, max 21.6 ms |
| Filter sheet counts (9 facets in one pass) | over every document (empty query, no filter): p50 47.9 ms, max 65.2 ms; "french" + fr-ca + 1080p: p50 11.4 ms |
| Throughput | 8 concurrent clients, 344 queries in 1.98 s (174 q/s) |
| Update | a changed torrent is searchable 212 ms after its commit (plus up to 1 s of polling) |

The full-index facet pass grows with the number of documents (each document reads 9 keyword
columns), so at the 3.2 M of D43's benchmark it would take roughly 7× longer; that run was not
repeated here. A first attempt with the index directory on `/tmp` failed with "an index writer
was killed" (an I/O error in a writer thread): `/tmp` on this laptop is a tmpfs with a user quota
that other work had nearly filled; the same build with the index on disk succeeded.

## Accuracy

**Sample.** 320 production torrents (one per infohash, first event), drawn by a fixed hash
order per category: video 120, anime 60, book 40, audio 40, adult 30, other 20, application
5, game 5. 126 of them carry a French scene tag. Each parsed result was read by hand against
the name (and the file list for poor names) on 2026-10-26, before any parser change based on
it (a separate 400-title development sample was used to build the parser). A field counts as
wrong when the name says something the parser missed or contradicted (e.g. `S18.FRENCH.AD`
with no language, `4th Season - 09` read as season 9), or the parser claims something the name
does not say. Things a name cannot tell (the original language behind `VO`/`MULTi`, the
languages of an untagged French book) are not counted against it.

First pass (the parser as it was before looking at the sample):

| Field group | Right | Share |
|---|---|---|
| languages + subtitles | 312 / 320 | 97.5 % |
| French scene tags (titles with one) | 125 / 126 | 99.2 % |
| resolution, source, codec, HDR | 314 / 320 | 98.1 % |
| audio codec, channels, music bit depth / rate | 319 / 320 | 99.7 % |
| release group | 319 / 320 | 99.7 % |
| season / episode / complete | 313 / 320 | 97.8 % |
| year | 319 / 320 | 99.7 % |
| every field right | 298 / 320 | 93.1 % |

The misses were fixed (bare `1080`, `HD1080`, `FullDVD`, `COMPLETE.BLURAY`, ordinal seasons,
first explicit resolution wins, `S18.FRENCH.AD`, `EN - PDF`, `fr.epub` filed as audio,
`FLAC16BITS`, `英配`, Erai-raws `(CA)`, `EP08` as a group, `[1080]` as an episode, PAL games),
except three that need knowledge the name lacks or a rule too risky to add: Chinese animation
from groups that do not mark it (2 titles, parsed `ja`), `[WIN x64 EN]` (software UI
language), and a date in a leading bracket (`[2025.12.24] …`). The sample without the adult
titles (290 rows, files kept where they change the result) is the regression test
`crates/core/tests/release_sample.rs`: after the fixes every field is ≥ 99 % (languages 99.0 %,
year 99.7 %, the rest 100 %) and 286 / 290 titles are right in every field. The floors in the
test (98–99 % per field) make a parser change that breaks titles fail with the list of what
changed. These after-fix numbers are measured on the sample the fixes came from, so they are an
upper bound; the first-pass table is the honest estimate for unseen titles.
