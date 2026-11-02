# Signature files

Rule trees in TOML, modelled on libmagic's magic(5). The built-in files here are embedded at
compile time; a user file can be added at runtime (`Signatures::with_user`, CLI
`--signatures FILE`). Rules that need real parsing (sizes checked against the file, chains of
frames, container walks) belong in `src/formats/` instead.

## A rule

```toml
[[rule]]
id = "gif"                    # optional; shown by --explain (default "<file>#<index>")
label = "image.gif"           # lowercase dotted id; register it in src/label.rs
mime = "image/gif"            # optional, overrides the registry (for user labels)
description = "GIF image"     # optional, idem
confidence = 98               # 0-100 when the rule matches
strength = 98                 # optional ordering among equal confidences (default = confidence)
fetch = true                  # ask for the top-level bytes when missing (default true)
head = 16                     # optional override of the computed head need
tail = 0                      # optional override of the computed tail need
offset = 0                    # see Offsets
type = "string"               # see Types
value = ["GIF87a", "GIF89a"]  # any of these
  [[rule.then]]               # continuation (level 1); [[rule.then.then]] is level 2, ...
  offset = 6
  type = "u16le"
  field = "width"             # capture the value as a field
```

A node runs its test, then its children in order. A child is **required** unless
`optional = true`: a required child that fails fails its parent (and drops what the parent and
its children captured). Children may override `label` and `confidence`, add a `note`, or `set`
string fields (`set = { fs = "ext4" }`); the last matching override wins, so put the most
specific child last. At most 8 levels.

Missing bytes: when the top-level test's bytes are not known the rule is undecided (and its
range is asked for only if the file is otherwise unidentified). When a required child's bytes
are missing, the rule matches at `confidence - 20` and asks for the range.

## Offsets

- `offset = 0x100`: from the start of the file.
- `offset = -512`: bytes before the end of the file.
- `offset = 4` with `relative = true`: after the end of the parent's match (search: after the
  found bytes).
- `offset = { at = 0x3C, type = "u32le", add = 4, mul = 1 }`: indirect, the value read at `at`
  (negative = from the end) times `mul` plus `add`.

## Types

| type | test | captured |
|---|---|---|
| `u8`, `u16le`, `u16be`, `u32le`, `u32be`, `u64le`, `u64be` (libmagic names `byte`, `leshort`, `belong`, ... also accepted) | `test = "..."` after `mask` and `shift` | number |
| `string` / `bytes` | equals any of `value` (escapes) or `hex`; `nocase = true` | the bytes (lossy UTF-8) |
| `search` | any of `value`/`hex` within `range` bytes from the offset (at most 1 MiB) | position found |
| `ascii` | exactly `len` bytes, optional `class` | trimmed text |
| `cstring` | up to `len` bytes, NUL-terminated, optional `class` | trimmed text |
| `utf16le`, `utf16be` | `len` bytes | trimmed text |

Numeric tests: `x` (any, default), `N` or `=N`, `!=N`, `<N`, `<=N`, `>N`, `>=N`, `&N` (all bits
set), `^N` (none set), `A..B` (B excluded), `A..=B`, alternatives `1|2|0x10`. `$size` is the
file length (`test = "=$size"`).

Classes: `printable`, `alnum`, `upper_alnum`, `digit`, `hex`, `graphic`, `whitespace`.

String escapes (use TOML literal strings `'...'`): `\xNN`, `\0`, `\n`, `\r`, `\t`, `\\`, `\'`.
Beware `'\r'` in a literal like `{\rtf`: use `hex` there.

## Adding a signature: from a hex dump to a rule

1. **Dump the header** of two or three real files of the type (more if the header has
   variants) and look for bytes that are the same in all of them (illustrative dump):

   ```console
   $ xxd -l 64 song1.it
   00000000: 494d 504d 4d79 2053 6f6e 6700 0000 0000  IMPMMy Song.....
   00000010: 0000 0000 0000 0000 0000 0000 0000 0410  ................
   00000020: 1000 0400 0800 0200 1402 0002 0d00 0600  ................
   00000030: 8030 0696 8000 0000 0000 0000 0000 0000  .0..............
   ```

   `IMPM` at 0 is the magic. Four bytes alone fire on text and on the middle of other
   files, so look for fields with a narrow range: the tracker versions at 0x28 / 0x2A
   (`0x0214`, `0x0200`, little-endian) and the global volume at 0x30 (at most 128).

2. **Write the rule** in the file of its area (`media.toml` here). The magic is the top-level
   test; each constraining field is a required child:

   ```toml
   [[rule]]
   id = "it"
   label = "audio.it"
   confidence = 90
   type = "string"
   value = "IMPM"
     [[rule.then]]          # created-with tracker version
     offset = 0x28
     type = "u16le"
     test = "0x0100..0x8000"
     [[rule.then]]          # compatible-with version (0x0200 / 0x0214)
     offset = 0x2A
     type = "u16le"
     test = "0x0100..=0x0300"
     [[rule.then]]          # global volume 0-128
     offset = 0x30
     type = "u8"
     test = "<=128"
   ```

   Pick the confidence from what the rule checks: 95+ only when a structural field is
   verified (a size equal to the file length, a version list, a checksum in a Rust check),
   80-94 for a magic plus constraints, below 80 for weak hints (never stored as facts).
   The engine enforces the bands from the evidence of the matched tests (-log2 of the chance
   they pass on random bytes; a 4-byte magic is 32 bits, `n` alternatives cost `log2 n`),
   whatever `confidence` says (`confidence_cap` in `src/magic/eval.rs`):

   | Matched evidence | Cap |
   |---|---|
   | 64 bits or more (a magic of 8+ bytes, or a magic plus structural fields) | 100 |
   | under 64, but the children add 8 bits or more, or the magic itself is 60+ bits | 94 |
   | a magic under 60 bits (shorter than 8 bytes) whose children add under 8 bits | **79** |

   So a short magic alone (`'NES\x1a'`, `"wOFF"`, the MOD tag at 1080) is shown at 79 and
   never stored. Give it a structural child that real files pass: a version, a header size,
   a power-of-two block size, a size `<=$size`, a second marker. Make it `optional = true`
   when the field is not certain on every real file: a file that fails it keeps the rule's
   label at 79 instead of losing the rule; a required child that fails drops the rule. A
   child whose bytes may lie past the head (a footer pointer, an indirect offset) must be
   required to be fetched: optional children never ask for bytes. When the check needs
   arithmetic a rule cannot express (a frame chain, a table walk), write it as a Rust check
   in `src/formats/` that returns 95+ when the structure checks out and lets the rule show
   the bare magic at 79 (`tests/formats.rs`,
   `short_magic_behind_random_bytes_stays_below_80`).
   Capture what a database needs with `field = "..."`; for consoles the names `title`,
   `title_id`, `product_code`, `version`, `region_code` and `maker` fill the detection's game
   record.

3. **Register the label** in `src/label.rs` (id, legacy name, MIME type, description, usual
   extensions). `tests/signatures.rs` fails on an unregistered label.

4. **Add a positive fixture** to `tests/signatures_fixtures.rs` (the header bytes from the
   dump, zero-padded): `every_rule_has_a_positive_fixture` fails for a rule without one.

5. **Run the false-positive check**: `cargo test -p sniff --test signatures_fp` evaluates every
   rule on 2000 synthetic windows (random bytes, text with magic-looking words, UTF-16 text,
   integer tables, sparse data) and on 2000 windows from the middle of the evaluation corpus
   when it is present. No rule may reach 80 there. A hit means the rule needs another
   constraint (the first draft of the IT rule above, without the 0x2A / 0x30 children, fired
   14 times on text windows).

A user file (`--signatures FILE`, `Signatures::with_user`) takes the same rules; its labels need
not be registered (give `mime` and `description` in the rule).
