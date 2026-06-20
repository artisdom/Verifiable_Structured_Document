# vsd-layout/1.7 — Layout Engine Contract (delta over 1.6)

**Status:** Normative for engine id `vsd-layout` version `1.7.0`.
This document specifies only the differences from
[LAYOUT-1.6.md](LAYOUT-1.6.md) (and transitively 1.5 … 1.0); everything
not mentioned here is **identical to 1.6**.

1.0–1.6 caches remain verifiable forever. 1.7 introduces **no format
change** (CJK is emitted with the existing `text` display op), so every
prior `.vsd` vector is byte-identical and the golden layout hashes of the
1.0–1.6 vectors are **unchanged**.

Engine 1.7 adds **CJK** — Han, Hiragana, Katakana, and Hangul — in
**horizontal** writing mode. (Vertical writing mode, `vertical-rl`, is a
separate future engine version: it requires an additive format change.)

## 1. New pinned dependency

| Kind | Item | SHA-256 |
|---|---|---|
| Font (index 19) | NotoSansCJKsc-Regular.otf | `2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b` |

This is a **CFF/OpenType** font (≈16 MB), unlike every prior pinned face
(TrueType `glyf`). It is CID-keyed with the **Adobe-Identity-0** ROS, so
its CFF charset is the identity map: **CID == GID**. That single property
makes both rendering and PDF embedding straightforward (below). `upem` is
1000, as for every other pinned face, so the integer-µm arithmetic is
unchanged.

## 2. Layout

Codepoints in the CJK ranges — Hangul Jamo (U+1100–11FF), Hiragana +
Katakana (U+3040–30FF), CJK Unified Ideographs and Extension A
(U+3400–4DBF, U+4E00–9FFF), Hangul Syllables (U+AC00–D7AF), and CJK
Compatibility Ideographs (U+F900–FAFF) — are routed per character to face
19 (like the 1.2 Hebrew fallback) and laid out **per glyph**:

- CJK is **not shaped**: each character maps to its glyph via the font's
  `cmap` and advances by its `hmtx` width, exactly like Latin. It is
  emitted as an ordinary `text` run, not a `glyphs` run. (Han, kana, and
  Hangul need no contextual shaping for faithful horizontal setting.)
- Baselines stay on the Regular rhythm (faces never change vertical
  metrics — the 1.1 rule).

These ranges are **exactly** the set every pre-1.7 engine already refused
(`refused_script`), which is the freeze-safety invariant: routing them to
the CJK face only affects an engine version that allows CJK (1.7), so no
frozen engine's output changes. CJK symbols/punctuation (U+3000–303F) and
the fullwidth forms are outside the historical refusal set and so remain
on the Regular path — a documented gap addressable only by a future
engine version.

## 3. Inter-ideograph line breaking

CJK has no inter-word spaces; a line may break between almost any two
adjacent characters. The engine adds a break opportunity at the boundary
between two characters when at least one is CJK, subject to simple
**kinsoku**:

- never break **after** an opening bracket/quote
  (`(` `[` `{` `〈` `《` `「` `『` `【` `〔` `〖` `（` `［` `｛`);
- never break **before** a closing bracket/quote or trailing punctuation
  (`)` `]` `}` `、` `。` `〉` `》` `」` `』` `】` `〕` `〗` `！` `）` `，` `．`
  `：` `；` `？` `］` `｝`);
- boundaries adjacent to whitespace are left to the normal space-based
  breaker.

These zero-width opportunities feed the **same** greedy first-fit breaker
as spaces, hyphenation, and Thai/Lao dictionary breaks (1.6). CJK / Latin
transitions are break points; runs of Latin (CJK on neither side) are
not. Justification still applies only over real spaces.

## 4. Rendering and PDF embedding

- **Raster** (`vsd-render`): draws each CJK glyph by id, outlined from
  the same pinned face — `ttf-parser` reads the CFF outlines directly.
- **PDF** (`vsd-pdf`): the CFF face embeds as **`FontFile3`** with
  `/Subtype /OpenType` under a **`/CIDFontType0`** descendant (every
  TrueType face still uses `FontFile2` + `CIDFontType2`). The content
  stream addresses glyphs by GID through `Identity-H`; because the CFF
  charset is identity (CID == GID), this selects the correct glyph
  without any remapping. `ToUnicode` is built from the same
  glyph→character usage map as every other face, so extraction and
  copy/paste keep yielding the source text. The whole pinned font is
  embedded (no subsetting yet), so a CJK PDF is large — a deliberate
  fidelity-over-size choice consistent with the existing exporter.

## 5. Determinism

Unchanged in kind: integer micrometers throughout; the font is pinned by
hash; per-glyph advances come from `hmtx`; the break rules are a pure
function of the text. The conformance corpus carries a golden hash for
1.7 (`valid/laid-out-1.7.vsd`, Chinese + Japanese + Korean wrapped on a
narrow page) alongside the unchanged 1.0–1.6 hashes; a conforming
implementation MUST reproduce all eight.

## 6. Still deliberately absent (future versions)

**Vertical writing mode** (`vertical-rl`: right-to-left columns, vertical
metrics, glyph orientation) — needs an additive `writing-mode` format
change and a column-flow geometry, so it is its own future engine
version. Also: CJK punctuation/fullwidth routing, font subsetting in PDF
export, complex scripts beyond the major Brahmic + Thai/Lao + CJK set
(Myanmar, Khmer, Tibetan, Ethiopic, …), multi-column layout, and MathML
layout.
