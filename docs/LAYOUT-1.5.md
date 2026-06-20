# vsd-layout/1.5 — Layout Engine Contract (delta over 1.4)

**Status:** Normative for engine id `vsd-layout` version `1.5.0`.
This document specifies only the differences from
[LAYOUT-1.4.md](LAYOUT-1.4.md) (and transitively 1.3 / 1.2 / 1.1 / 1.0);
everything not mentioned here is **identical to 1.4**, including the
integer-micrometer arithmetic, the `muldiv` primitive, greedy line
breaking, hyphenation, widow/orphan control, justification, faces, the
format-0.4 `glyphs` display op, and back-reference semantics.

Version pinning is load-bearing: a render cache records the engine
version it was produced with, and verifiers MUST recompute under that
exact version's contract. 1.0–1.4 caches remain verifiable forever; the
conformance corpus proves it — the golden layout hashes of the 1.0–1.4
vectors are **unchanged** by this engine version, and because 1.5
introduces **no format change** (it reuses the format-0.4 `glyphs` op),
every prior `.vsd` vector is byte-identical too.

Engine 1.5 adds two things: shaping for the **remaining major Brahmic
scripts**, and **bidi mirroring** of `Bidi_Mirrored` characters in
right-to-left runs.

## 1. New pinned dependencies

Nine additional faces, each part of engine 1.5's identity exactly like
every other pinned face, all shaped by the **same** pinned `rustybuzz`
(`=0.14.1`) already used since 1.4 — no new shaper or tool.

| Face index | Font | SHA-256 |
|---|---|---|
| 8  | NotoSansBengali-Regular   | `b55c62ee531e3214da6c0701daecea89a52ba42db7d8206b92e6b51f397a3193` |
| 9  | NotoSansGurmukhi-Regular  | `658d0207da305a1411c539a8b0bbeda64d4146e54fb4827facddb890b6b90d74` |
| 10 | NotoSansGujarati-Regular  | `9b5a7aaeeb649a2e75a49d8b006a1f87db1b61c0df3b001609f4e0725d88dbf6` |
| 11 | NotoSansOriya-Regular     | `a16645d056017927406546aa78e4ce15e782fd8783467267b75450453d007415` |
| 12 | NotoSansTamil-Regular     | `3c0a186feb3c63c7f6d63e1511dcdc144e745ae09b98e217c83f3e317974f6f9` |
| 13 | NotoSansTelugu-Regular    | `b274780b69d1d23fe84b55e809a152cb2ac5306d33864b1f87622f6971871aae` |
| 14 | NotoSansKannada-Regular   | `9ad74dc64838c6855b96f671fc08e425a58921b9d0c71712ea79c328a27e6e38` |
| 15 | NotoSansMalayalam-Regular | `c08de7fa8d032a5d6a4d120fb82c78cec60b362a4e73fa26360d89759ff2a7f9` |
| 16 | NotoSansSinhala-Regular   | `9e32612d47004552f3125e78648a9e2e7899a216ccd3cefbb93a9b5f4c809feb` |

The mirroring table (§3) is the Unicode Character Database
`Bidi_Mirroring_Glyph` property, pinned to **Unicode 17.0.0**
(BidiMirroring.txt, 2025-08-01), 428 pairs, embedded by value.

## 2. Shaping the remaining Brahmic scripts

Codepoints in these blocks are routed per-character to their face (like
the 1.2 Hebrew fallback and the 1.4 Arabic/Devanagari routing) and
**shaped** by the pinned shaper:

| Block | Range | Face |
|---|---|---|
| Bengali   | U+0980–09FF | 8 |
| Gurmukhi  | U+0A00–0A7F | 9 |
| Gujarati  | U+0A80–0AFF | 10 |
| Oriya     | U+0B00–0B7F | 11 |
| Tamil     | U+0B80–0BFF | 12 |
| Telugu    | U+0C00–0C7F | 13 |
| Kannada   | U+0C80–0CFF | 14 |
| Malayalam | U+0D00–0D7F | 15 |
| Sinhala   | U+0D80–0DFF | 16 |

The mechanics are exactly those of 1.4 §2: each maximal same-face run is
shaped (script/direction/language from `guess_segment_properties`),
never crosses a space, is measured during line breaking exactly as it is
shaped at emission, is not hyphenated, and is emitted as a format-0.4
`glyphs` op with logical `text`/`range`/`src` preserved. These scripts
are written left-to-right and use the normal LTR path.

**Freeze gate.** `Face::shaped_for` is a forward-growing codepoint→face
map, but *which* scripts an engine version shapes is gated per version
(`EngineVersion::shaped_face`): engine 1.4 shapes Arabic + Devanagari
**only** and still refuses Bengali…Sinhala; engine 1.5 shapes all of the
above. This keeps 1.4 (and earlier) byte-identical.

## 3. Bidi mirroring (UAX #9 HL6)

Within a **right-to-left run**, a character with the Unicode
`Bidi_Mirrored=Yes` property is drawn with its mirror-image glyph (an
opening `(` in a Hebrew or Arabic clause is drawn as `)`). This is a
glyph-selection step that does **not** change the logical text.

- Mirroring applies only to characters resolved at an odd (RTL)
  embedding level (UAX #9). LTR runs never mirror.
- A right-to-left **non-shaped** segment (e.g. a Regular-face neutral
  run such as `" ( "` between Hebrew words) that contains any mirrored
  character is emitted as a `glyphs` op: each character maps to its own
  glyph, except mirrored characters which map to their **mirror's**
  glyph, and the glyphs are emitted in **visual order** (the logical
  order reversed). `cluster` is the source byte offset, so `text` stays
  the original (`(` is still `(` for search and extraction).
- A right-to-left non-shaped segment with **no** mirrored character is
  still emitted as a format-0.3 `text` op with the `rtl` flag, exactly
  as in 1.2 — so Hebrew letter runs are unchanged; only segments that
  actually contain a mirrored character convert to `glyphs`.
- Shaped right-to-left runs (Arabic) are unaffected by this clause: the
  brackets adjacent to them always resolve to their own Regular-face
  segment (script fallback splits them out), so the shaper never sees a
  bracket and there is no double-mirroring.

No new display op and no new field: mirroring rides the format-0.4
`glyphs` op introduced in 1.4.

## 4. Determinism

Unchanged in kind: integer micrometers throughout; the fonts, shaper,
and mirroring table are pinned by hash / exact version; mirroring is a
deterministic binary-search lookup. The conformance corpus carries a
golden hash for 1.5 (`valid/laid-out-1.5.vsd`) alongside the unchanged
1.0–1.4 hashes; a conforming implementation MUST reproduce all six.

## 5. Still deliberately absent (future versions)

CJK and vertical text (need CJK fonts — ~8–16 MB — and ideographic /
vertical breaking rules), Thai/Lao dictionary line breaking (needs a
pinned segmentation dictionary), other complex scripts beyond the major
Brahmic set (Myanmar, Khmer, Tibetan, Ethiopic, …), multi-column
layout, and MathML layout.
