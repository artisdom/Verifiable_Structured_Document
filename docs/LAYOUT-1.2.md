# vsd-layout/1.2 — Layout Engine Contract (delta over 1.1)

**Status:** Normative for engine id `vsd-layout` version `1.2.0`.
This document specifies only the differences from
[LAYOUT-1.1.md](LAYOUT-1.1.md) (and transitively
[LAYOUT-1.0.md](LAYOUT-1.0.md)); everything not mentioned here is
**identical to 1.1**, including the integer-micrometer arithmetic, the
`muldiv` primitive, greedy line breaking, pagination, constants,
bold/italic face resolution, and back-reference semantics.

Version pinning is load-bearing: a render cache records the engine
version it was produced with, and verifiers MUST recompute under that
exact version's contract. 1.0 and 1.1 caches remain verifiable forever;
this document changes nothing about them.

Engine 1.2 emits display lists that use the format **0.3** `rtl` flag
on text runs (additive minor field; pre-0.3 readers of LTR documents
are unaffected because the flag is omitted when false).

## 1. New pinned faces

Two faces join the family, identified by display-list font index. Each
binary is part of the engine version — its `cmap`/`hmtx` tables are the
sole source of glyph mapping and metrics, scaled with the same `muldiv`
rule as 1.0:

| Index | Face | Size (bytes) | SHA-256 |
|---|---|---|---|
| 4 | NotoSansMono-Regular | 596,428 | `65b5e2b2c4a1fba9ae8be1f026cb35b03dcb8886d9b2a4147054fde12f7e767d` |
| 5 | NotoSansHebrew-Regular | 26,860 | `cdefaf8efd47045f6820928eba84db5bed7557539328952b5f828315485e02ee` |

Indices 0–3 are unchanged from 1.1.

## 2. Monospace

- **Code blocks** (and MathML source fallback) are set in face 4.
  Advances and tab stops (4 × the *mono* space advance) are measured in
  the mono face; baselines keep the Regular face's ascent and the
  `muldiv(size, 7, 5)` line height — faces never change vertical
  rhythm.
- **`mono` style spans**: the style table's `mono` flag is honored and
  **overrides** `b`/`i` (the pinned mono family ships one face in this
  version): `mono → 4`, else `pick(bold, italic)` as in 1.1.

## 3. Underline

The style table's `u` flag is honored. Each maximal underlined byte
range on a line produces a filled rect per emitted run: top edge at the
baseline, height `RULE` (100 µm — the same hairline as table rules and
blank field lines), in the run's color (link blue inside links, black
otherwise). Runs split at underline boundaries in addition to link and
face boundaries.

## 4. Justification

Body paragraphs (`para` blocks; not headings, captions, list labels,
fields, or code) are **fully justified**:

- Applies to every line of the paragraph except the last, and only
  when the line contains at least one word gap and the line's measured
  width is less than the content width.
- The slack `width − line_width` (µm) is distributed across the line's
  word gaps (the collapsed single spaces): each gap receives
  `slack / gaps`, and the leftmost `slack mod gaps` gaps receive one
  extra micrometer. Integer arithmetic; no floating point.
- Runs additionally split after each word gap so the bonus shifts the
  remainder of the line; an underline spanning a stretched gap stays
  solid (the rect runs to the segment's visual end).
- Lines ordered by the bidi algorithm (§5) are **never justified**;
  they stay ragged.

## 5. RTL and bidi (non-joining scripts)

### 5.1 Script fallback

Codepoints in the Hebrew blocks (U+0590–U+05FF, U+FB1D–U+FB4F) are
mapped and measured in face 5 regardless of span styling, and runs
split at every script transition. All other codepoints keep their
styled face.

### 5.2 Ordering

If the document's base direction is `rtl`, or a block's layout text
contains any Hebrew-block codepoint, each broken line is reordered by
**UAX #9** (the Unicode Bidirectional Algorithm; the reference
implementation pins `unicode-bidi` 0.3.18) with the paragraph level
forced to the document direction:

- Line breaking itself is unchanged (logical order, direction-blind —
  widths are direction-independent sums).
- A `dir=rtl` document right-aligns each line box: the line starts at
  `x_left + (width − line_width)`. LTR documents keep `x_left`.
- The line's visual runs are placed left to right; within an RTL run,
  segments (split at link/face/underline/script boundaries) are placed
  in reversed logical order.
- An RTL run is emitted with the format-0.3 `rtl: true` flag, its
  `text` in **logical order**, and `x` at the run's left edge.
  Consumers draw its glyphs in reversed logical order starting at `x`.
  Back-references (`node_path`, `char_range`) stay logical and
  byte-accurate, so selection, search, and disclosure are unaffected
  by visual reordering.

`dir=rtl` remains refused by engines 1.0/1.1 (their frozen contracts).

### 5.3 Refused scripts

1.2 claims script awareness, so it **refuses** (a layout error, never a
mis-render) any text containing scripts it cannot set faithfully:

| Range(s) | Why refused |
|---|---|
| Arabic (U+0600–U+06FF, U+0750–U+077F, U+08A0–U+08FF, U+FB50–U+FDFF, U+FE70–U+FEFF) | joining script: needs a real shaper |
| Syriac (U+0700–U+074F) | joining script |
| Indic (U+0900–U+0DFF) | conjuncts/reordering: needs a shaper |
| Thai/Lao (U+0E00–U+0EFF) | dictionary line breaking |
| CJK (U+1100–U+11FF, U+3040–U+30FF, U+3400–U+4DBF, U+4E00–U+9FFF, U+AC00–U+D7AF, U+F900–U+FAFF) | needs CJK fonts and breaking rules |

This applies to paragraph/heading/caption text, code blocks, and field
values. Engines 1.0/1.1 keep their frozen behavior (`.notdef` boxes) —
documents that lay out under an old pinned version keep verifying.

Known limitations (deliberate, documented): no bracket mirroring
(UAX #9 L4) and no Arabic-Indic digit shaping; Hebrew cantillation
marks render as combining glyphs without mark positioning.

## 6. Determinism

Unchanged in kind: all arithmetic is integer micrometers; six faces'
metric tables and the UAX #9 implementation are pinned; the same
document and version produce byte-identical page objects on every
platform. The conformance corpus carries golden hashes for 1.0
(`valid/laid-out.vsd`), 1.1 (`valid/laid-out-1.1.vsd`), and 1.2
(`valid/laid-out-1.2.vsd`); a conforming implementation MUST reproduce
all three.

## 7. Still deliberately absent (future versions)

Shaped scripts (Arabic, Indic — require a pinned shaper, the next major
engine effort), CJK and vertical text, hyphenation, multi-column,
widow/orphan control, bracket mirroring, mark positioning.
