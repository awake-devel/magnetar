# sniff

Content-based file type detection and video recon on **sparse input**: the byte ranges of a
file that a torrent probe fetched from peers. Pure Rust, `#![forbid(unsafe_code)]`, no I/O in
the library, no network, no ffprobe. Does not depend on `magnetar-core`.

The extension never decides. The name only breaks ties between equally confident candidates
and raises the `extension_mismatch` / `suspicious_executable` flags.

## API

```rust
pub struct Range { pub offset: u64, pub len: u64 }
pub struct SparseFile { /* len, optional name, sorted coalesced (offset, bytes) ranges */ }
impl SparseFile {
    pub fn new(len: u64) -> SparseFile;
    pub fn from_bytes(bytes: Vec<u8>) -> SparseFile;
    pub fn with_name(self, name: impl Into<String>) -> SparseFile;
    pub fn insert(&mut self, offset: u64, bytes: Vec<u8>);            // verified bytes
    pub fn insert_unverified(&mut self, offset: u64, bytes: Vec<u8>); // sets Flags::unverified when read
    pub fn get(&self, offset: u64, len: usize) -> Option<&[u8]>;
    pub fn from_offset(&self, offset: u64) -> &[u8];
    pub fn missing(&self, r: Range) -> Vec<Range>;
}

pub enum Outcome { Detected(Detection), NeedMore(Vec<Range>), Unknown }
pub struct Detection {
    pub label: String,            // "game.ps5.pkg", "video.mkv", "archive.zip"
    pub mime: String, pub description: String,
    pub confidence: u8,           // 95+ signature + structural check, 80+ partial, <= 79 a short
                                  // magic alone (shown, not stored); store >= 80
    pub fields: Vec<Field>,       // typed key/values (Value::{Bool,U64,Str,List})
    pub game: Option<GameInfo>,   // content_id, title_id, title, version, required_fw, region, ...
    pub video: Option<VideoInfo>, // video recon for MKV/WebM, MP4/MOV, AVI, TS/M2TS
    pub flags: Flags,             // extension_mismatch, suspicious_executable, unverified, incomplete
    pub extensions: Vec<String>,  // reported only
    pub candidates: Vec<Candidate>,
}

pub fn detect(file: &SparseFile) -> Outcome;                       // one pass, built-in rules
pub fn detect_with<F, E>(fetch: F, len: u64, name: Option<&str>, caps: &Caps) -> Result<Report, E>
where F: FnMut(Range) -> Result<Vec<u8>, E>;                       // fetch driver
pub struct Detector<'s> { .. }  // new(), with_signatures(&Signatures), caps(Caps),
                                // detect(), finish() (never NeedMore), analyze() -> Analysis, detect_with()
pub struct Caps { head, tail, rounds, max_bytes, max_range, max_ranges, inner_bytes, max_depth }
pub struct Signatures { .. }    // builtin(), try_builtin(), from_toml(src, text), with_user(src, text)
pub struct Report { outcome, analysis, rounds, bytes_fetched, fetched, stop: Stop }
pub enum Stop { Done, Rounds, Budget, NoProgress }               // why the driver stopped
pub fn video::analyze(file: &SparseFile, caps: &Caps) -> Recon; // { info, need, unverified }
pub fn video::probe(file: &SparseFile, caps: &Caps) -> VideoOutcome;  // Info | NeedMore | Unsupported
pub fn video::recommended_caps() -> Caps;                        // 64 KiB head, 10 rounds
pub fn listing::classify(files: impl IntoIterator<Item = (&str, u64)>) -> Vec<Listing>;
pub fn listing::split_range(part_sizes: &[u64], offset: u64, len: u64) -> Vec<(usize, u64, u64)>;
pub fn db_id_valid(platform: Platform, id: &str) -> bool;
```

Contract details enforced by the engine:

- The driver keeps only the bytes of the range it asked for: a fetch that answers more is
  cut to the request (budget and planted offsets).
- Every `NeedMore` (from `Detector::detect`, `analyze`, `video::analyze`) is held to
  `max_ranges`, `max_range` and `max_bytes`.
- `flags.unverified` is set when any byte read by the type check or the video recon came from
  `insert_unverified`; `flags.incomplete` when ranges were still wanted at the end.
- 95+ needs a passing structural check or, for signature rules, 64 bits of matched evidence
  (a magic of 8 bytes or more); 80-94 a short magic plus a passing check.
- A magic shorter than 8 bytes with no passing structural check scores at most 79
  (`types::WEAK`): shown as a candidate, below the 80 that callers store. Signature rules get
  it from the engine (`confidence_cap`); Rust checks for short magics (TIFF IFD, GIF block
  after the colour table, DTS and AMR frame chains, the MOD sample table, N64 boot address,
  NES size, GB header checksum, NCCH size, MPEG-PS pack header, MPEG-2 sequence header, AV1
  OBU chain, DV DIF sequence, shebang interpreter path) return 79 or nothing when their check
  fails. A check that needs bytes past the head asks for them (it outranks the bare signature
  rule at the same 79).
- A user signature file is capped (4096 rules, 32k tests, 256 alternatives per search,
  64 MiB of search work per pass).

Defaults (`Caps::default()`): 16 KiB first read, no tail, 3 follow-up rounds, 8 MiB per file,
4 MiB per range, 16 ranges per round, 64 KiB decompressed peeks, 2 nested peeks.

## CLI

```
sniff <file> [--explain] [--json] [--head N] [--tail N] [--rounds N] [--max-bytes N] [--signatures FILE]
```

Reads only the ranges the detector asks for (a local simulation of the sparse fetches).
`--explain` lists the candidates, the ranges fetched per run, what was still wanted and why
the rounds stopped (round cap, byte budget, no new bytes). `--json` carries `stop` and the
`incomplete` flag.

## Module layout and ownership

Three areas work on this crate; each keeps to its files so they do not edit the same ones.

| Path | What | Area |
|---|---|---|
| `src/lib.rs` | public re-exports, crate docs | core (shared, edit rarely) |
| `src/sparse.rs` | `SparseFile`, `Range` | core |
| `src/types.rs` | `Outcome`, `Detection`, `Value`, `Flags`, `Caps`, internal `Hit` | core |
| `src/engine.rs` | ranking, NeedMore decisions, name flags, video hand-off, fetch driver | core |
| `src/view.rs`, `src/util.rs`, `src/name.rs` | bounds-checked readers, extension rules | core |
| `src/label.rs` | label registry (id, legacy id, MIME, description, extensions) | shared: append rows in your section |
| `src/magic/` | TOML signature engine (parser `parse.rs`, evaluator `eval.rs`) | core |
| `data/signatures/*.toml` | built-in signature rules (syntax: `data/signatures/README.md`) | per file: `game.toml` console, `media.toml` video, others core |
| `src/formats/{media,image,doc,archive,exe,disc,text}.rs` | Rust structural parsers | core |
| `src/formats/console/` (`mod.rs`, `sony.rs`, `nintendo.rs`, `xbox.rs`, `iso.rs`, `retro.rs`) | console packages, disc images and cartridges, `GameInfo` | **console** |
| `src/formats/console/listing.rs` | game / disc folders and split files from a torrent file list (no bytes) | **console** |
| `src/video/` (`mod`, `src`, `bits`, `codec`, `audio`, `mkv`, `mp4`, `ts`, `avi`) | video recon: `analyze()` / `probe()`, `VideoInfo` and track types | **video** |
| `tests/formats.rs`, `tests/driver.rs`, `tests/signatures.rs`, `tests/props.rs` | per-format fixtures, driver, engine, property tests | core |
| `tests/console*.rs`, `tests/ps5_samples.rs`, `tests/signatures_{fixtures,fp}.rs` | console fixtures and ids, listings, the two real PS5 packages (skipped when absent), per-rule fixtures and false positives | **console** |
| `tests/video.rs`, `tests/video_props.rs`, `tests/video_corpus.rs`, `tests/video_fixtures/` | hand-built containers, hostile-input properties, ffprobe comparison (skipped without corpora or ffprobe; `SNIFF_VIDEO_CORPUS`) | **video** |
| `tests/corpus_eval.rs`, `tests/support/eval.rs`, `examples/eval.rs`, `examples/fp.rs` | corpus scoring (skipped when the manifests are absent), misplaced-window false positives | core |
| `tests/real_samples.rs`, `tests/sample_fixes.rs` | real demo media and console homebrew from `~/sniff-samples` (`SNIFF_SAMPLES`; skipped when absent), and the same regressions rebuilt from those files' header values | shared |
| `src/bin/sniff.rs` | CLI | core |

Integration points:

- **Video** (`src/video/mod.rs`): the engine calls `video::analyze` for every label in
  `video::is_video_container` once the type is final, attaches the partial or complete
  `VideoInfo` to `Detection::video` (with `incomplete` when ranges are still wanted), and
  merges the recon's ranges into the detection's own requests (same rounds and budget). The
  recon's track handlers also settle MP4 vs `.m4a` (no picture track), so the type check
  never fetches a whole `moov`. Audio-only containers (`.mka`, `.m4a`) and raw AC-3 / E-AC-3,
  DTS and TrueHD / MLP streams get the same recon (one audio track for a raw stream).
- **Console** (`src/formats/console/`): every parser is listed in `console::CHECKS`; labels and
  confidences follow the detector prototype (tuned on the corpora), `GameInfo` follows the
  console prototype. Labels produced by signature rules only (`game.toml`) get a minimal
  `GameInfo` from `console::game_from_label`.

## Layers

1. Rust checks (`formats::checks()`): every check sees the known bytes and returns at most one
   candidate, possibly with ranges it wants (ZIP central directory at the tail, MP4 `moov`,
   Matroska Tracks via the SeekHead, ISO directory and SYSTEM.CNF, PE header and overlay,
   PS5 CNT block, CFB directory sector, zstd blocks).
2. Signature rules (`magic`): libmagic-style trees. Missing top-level bytes are remembered and
   asked for only when the file is otherwise unidentified (the second chance: from 0x7FC0,
   covering both SNES header locations and the 0x8000 ISO descriptor, plus 0x10000 and the
   last 512 bytes).
3. Text rules (only when no binary candidate reaches 60), then the fallback (empty, zeros,
   unknown).

Candidates rank by confidence, then by extension match, then by signature strength.

## Accuracy (prototype corpora)

| Corpus | Default caps (16 KiB, 3 rounds) | Prototype caps (64 KiB + 64 KiB tail, 6 rounds) |
|---|---|---|
| Holdout, 356 files (94% real) | 356/356 (100%) | 356/356 (100%) |
| Dev, 307 files (80% synthetic) | 306/307 (99.7%) | 306/307 (99.7%) |

False positives (`examples/fp.rs`, 2,100 random 64 KiB windows from the middle of 105 local
files): 1 confident (>= 80) wrong label, a window in the middle of `Dolby_Amaze_ATMOS.m2ts`
read as `video.h264` (85): raw H.264 with a TS packet boundary cut away. The prototype gave 3 there (`text/nfo` on binary
data behind an `FF FE` "BOM"); the UTF-16 decode now has to read as text.

The dev miss is a `.cb7`: its 7z header is LZMA-encoded and there is no LZMA decoder in the
offline registry. Flags (mismatch / suspicious) are identical to the prototype's on every file.

## Video recon

Fields: container, brand, duration, overall bitrate, fragmented; per video track codec,
profile, level, size, fps, bit depth, chroma, H.273 codes, HDR (HDR10, HDR10+, HLG, Dolby
Vision profile / level / compatibility / RPU / layers), mastering display, MaxCLL/MaxFALL;
per audio track codec, profile, channels, layout, rate, bit depth, bitrate, language, Atmos
(TrueHD 16-channel presentation, E-AC-3 JOC, also behind a Blu-ray AC-3 core), DTS-HD MA /
HRA / Express, DTS:X and DTS-UHD (MP4 `dtsx` / `dtsy`, codec `dtsuhd`, from the sample entry
only); subtitle tracks. `atmos` / `dts_x` are `None` unless the parsed bytes decide them (AC-4
stays `None`: its `dac4` is not parsed). Encrypted MP4 tracks (`encv` / `enca`) report the
`frma` original format and are never frame-parsed.

| Corpus (31 local files, `tests/video_corpus.rs`) | Field checks vs ffprobe on the whole file |
|---|---|
| `video::recommended_caps()` (64 KiB head, 10 rounds) | 649/649 |
| Detector defaults (16 KiB head, 3 rounds) | 625/639; the misses are fields of MP4s with the `moov` at the end and of TS files whose audio starts MBs in, kept partial and flagged `incomplete` |

Bytes per file through the driver, recommended caps: MKV/WebM 64-208 KiB, MP4 128-208 KiB
(faststart, `moov` at the end, fragmented), AVI 64 KiB, TS/M2TS 204 KiB to 4.2 MiB (when
the muxer starts the audio several MB in).

## Checked on real samples

All of these were run through the fetch driver and compared with ffprobe or the format's
header values (sources, licences and ground truth are in `~/sniff-samples/SOURCES.tsv`; no
file is checked in). Only part of the list is automated: `tests/real_samples.rs` re-runs the
samples behind this round's fixes when `~/sniff-samples` exists, and `tests/sample_fixes.rs`
rebuilds the same cases from their header values. The rest was checked by hand and in a
sweep of all 864 sample files.

- **Atmos**: TrueHD + Atmos in Blu-ray M2TS with interleaved AC-3 (Dolby "Amaze",
  "Helicopter", TrueHD 7.1 Channel Check) and in MKV / MP4 remuxes (`mlpa`, `moov` at the
  end); E-AC-3 JOC in MP4 and TS (Dolby delivery-kit ChID / AVSync files) and Blu-ray style
  E-AC-3 (AC-3 core + dependent substream, 7.1) in M2TS, MKV (DDP 7.1 Channel Check) and MP4
  (`ec-3`); raw `.thd`.
- **DTS**: DTS-HD MA + DTS:X in MKV (mkvmerge "DTS:X Emulator", ffmpeg remux), M2TS (DTS:X
  Demo 2016), MKA and raw `.dts`; DTS-HD MA 5.1 (THX Deep Note); DTS-HD HRA 7.1 96 kHz
  (Orchestra). DTS-UHD from the MP4 sample entry (DTS-UHD_DTSX_P2.mp4).
- **Dolby Vision**: profiles 5 (Jellyfin, Chromium fragmented), 8.1 (Jellyfin, DV8_TEST with
  in-band parameter sets), 8.4 HLG (Jellyfin, iPhone 13 Pro MOV) and profile 4 dual layer in
  TS (LG demo).
- **HDR**: HDR10+ through HEVC SEI (ISS 4K MP4) and VP9 BlockAdditional in WebM (WebM project
  sample); HLG; AV1 PQ; VP9 profile 2 (The Redwoods, Chromium).
- **Containers**: mkvmerge / libmatroska MKV (THX, DTS:X Emulator, DDP 7.1, HRA, Matroska
  test suite incl. junk between top-level elements), Google's WebM muxer, Apple MOV, MP4 with
  two `stsd` entries (Chromium CENC: `encv` + clear), fragmented MP4, OpenDML AVI over 2 GB
  (2.36 GB rawvideo), MPEG-2 video in TS (Chromium bear.m2ts), ProRes in MKV.
- **Installers**: NSIS (Notepad++ 8.9.8.1, VLC 3.0.21), Inno Setup 7.1.1, 7-Zip SFX, MSI,
  DMG, macOS flat pkg, APK.
- **Consoles (homebrew)**: PS4 fpkg CNT (ezRemote Client, Store), PS3 debug PKG (Apollo,
  prepISO, PSP Minis Launcher), Vita VPK / SELF / PARAM.SFO (VitaShell, VITAlbum), PSP PBP
  (Apollo, CM File Manager), PS1 raw BIN (tonyhax), PS-X EXE, Switch NRO with NACP (ftpd,
  Goldleaf, nxdumptool, hbmenu), NSO, ExeFS PFS0 (uLaunch), 3DS CIA / 3DSX (ftpd,
  Checkpoint), DSiWare CIA, NDS with and without the logo (ftpd, hbmenu builds), GB / GBC
  (acid2), GBA, N64 (Julia), SNES LoROM / HiROM test ROMs, NES (240p test suite), Mega Drive
  (SGDK, incl. "SEGA MEGAWIFI"), GameCube ISO (Swiss, bi2 region), GameCube / Wii DOL (Swiss,
  USB Loader GX, WiiFlow, Snes9x GX), WUHB, XISO + XBE (NevolutionX), XEX2 (XeUnshackle).
- **Listings**: Vita VPK trees, the PSP Memory Stick layout (`PSP/GAME/<name>/EBOOT.PBP`) from
  two homebrew zips, split 7z parts (RetroDECK).

## UNVERIFIED and not ported

- QuickTime AAC in `wave`; `mfra` with several `tfra`; Macintosh language codes other than
  0, 1, 2, 4, 10, 11, 23, 128; Matroska header stripping and zlib frames; DTS Express; DV
  profile 7 and the RPU-only path (no legally clean profile 7 sample found); VC-1 in TS
  (no sequence header parse); the DTS-UHD frame (channels come from the sample entry).
- Folder and split-file layouts in `listing.rs` other than the ones above come from dump
  tools' docs, not real torrents. Wii Homebrew Channel (`apps/<name>/boot.dol`), Switch
  `switch/<name>/*.nro` and 3DS `3ds/<name>/*.3dsx` folders have no listing label.
- Console formats without a real sample: Wii ISO / WAD / WBFS / RVZ / GCZ / CISO, Wii U RPX /
  WUD, Xbox 360 STFS / GoD, PSP ISO / CSO, PS2 ISO, retail PS3 / PSP / Vita PKG, 3DS CCI,
  installable NSP / NSZ / XCI. PS5 is checked on two local packages only (both carry
  `PPRPLAIN-NOAUTH!`, possibly fake-signed). PS3 debug packages' PARAM.SFO is not decoded
  (SHA-1 keystream, no key needed).
- RAR / ZIP / CAB self-extractors have never seen a real installer.
- Blu-ray UDF/BDMV directory walking is not implemented (`bluray_likely` heuristic only).
- xz/7z/cb7 file names need an LZMA decoder; bzip2 has no inner peek.
