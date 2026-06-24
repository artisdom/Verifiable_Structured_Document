# vsd-layout/1.8 — Layout Engine Contract (delta over 1.7)

**Status:** Normative for engine id `vsd-layout` version `1.8.0`.
This document specifies only the differences from
[LAYOUT-1.7.md](LAYOUT-1.7.md) (and transitively 1.6 … 1.0); everything
not mentioned here is **identical to 1.7**. A **horizontal** document
(`writing_mode = horizontal-tb`, the default) is laid out byte-for-byte
as in 1.7.

Engine 1.8 adds **vertical writing mode** (`vertical-rl`). It is the
first engine to consume the format **0.5** `wm` attribute on the doc
node; horizontal page output is unchanged, so the 1.0–1.7 golden layout
hashes are **unchanged** by this version (the conformance corpus proves
it — only the documents' `doc_id`s move, because the doc node gained the
`wm` key).

## 1. The `wm` attribute (format 0.5)

The doc node carries `wm`: `"htb"` (horizontal-tb, default) or `"vrl"`
(vertical-rl). A document whose `wm` is `"vrl"` requires engine **1.8 or
later**; earlier engines refuse it (as they refuse `dir=rtl` before 1.2).
No display-list change: vertical layout emits ordinary `text` runs at
computed positions.

## 2. Vertical-rl layout

When `wm = vrl`:

- **Inline axis is vertical, block axis is horizontal-right-to-left.**
  Characters stack top-to-bottom within a *column*; columns advance
  right-to-left across the page; when columns reach the left margin a new
  page begins.
- **One positioned run per character.** Each character is emitted as its
  own `text` run at an absolute `(x, y)`: `x` is its column (constant
  within a column, decreasing as columns advance left), `y` is its
  baseline (increasing down the column). Because runs are emitted in
  reading order — top-to-bottom, then right-to-left — **op order is
  reading order**, so extraction, search, and back-references recover the
  logical text directly. Each run keeps its `node_path` and `char_range`.
- **Metrics.** A character advances **one em** (`size`) down the column
  (CJK is square; this is the standard vertical rhythm). It is centered
  horizontally in its column: column width is the same line-height
  constant as horizontal (`muldiv(size, 7, 5)`), and the glyph is placed
  at `col_left + (col_w − advance)/2`. The baseline uses the character's
  face ascent, as horizontal does. Faces are resolved per character
  exactly as in horizontal mode (CJK → the pan-CJK face, Latin → Regular,
  etc.).
- **Blocks.** Each paragraph or heading begins a fresh column; headings
  use their larger size (wider column). Blocks after the first leave an
  inter-block gap (`SPACE_AFTER`) in the horizontal block-flow axis.

## 3. Scope (refused rather than mis-rendered)

Engine 1.8 vertical mode supports **paragraphs and headings** (and
transparently sections, subtree refs, and salt wrappers). Other block
types — tables, figures, lists, fields, code, math — are **refused** in
vertical mode (faithful vertical tables/floats are a future version).
**Shaped scripts** (Arabic, Indic, Thai/Lao) are also refused in vertical
mode: they would need vertical shaping, which is out of scope; placing
unshaped glyphs would mis-render, so the engine refuses instead. CJK,
Latin, and other simple scripts set upright.

Horizontal documents are entirely unaffected by any of the above.

## 4. Determinism

Unchanged in kind: integer micrometers throughout; per-character advances
and the column/page flow are pure functions of the text and the pinned
fonts. The conformance corpus carries a golden hash for 1.8
(`valid/laid-out-1.8.vsd`, a vertical Japanese document) alongside the
unchanged 1.0–1.7 hashes; a conforming implementation MUST reproduce all
nine.

## 5. Still deliberately absent (future versions)

Vertical tables / figures / lists / code; tate-chu-yoko (short
horizontal runs inside vertical text) and vertical glyph alternates
(`vert`/`vrt2` features, rotated punctuation); vertical bidi; CJK
punctuation/fullwidth face routing; and the viewer's vertical
highlight geometry (selection still extracts correct text). These remain
documented gaps.
