# vsd-layout/1.4 — Layout Engine Contract (delta over 1.3)

**Status:** Normative for engine id `vsd-layout` version `1.4.0`.
This document specifies only the differences from
[LAYOUT-1.3.md](LAYOUT-1.3.md) (and transitively 1.2 / 1.1 / 1.0);
everything not mentioned here is **identical to 1.3**, including the
integer-micrometer arithmetic, the `muldiv` primitive, greedy line
breaking, hyphenation, widow/orphan control, justification, faces, and
back-reference semantics.

Version pinning is load-bearing: a render cache records the engine
version it was produced with, and verifiers MUST recompute under that
exact version's contract. 1.0/1.1/1.2/1.3 caches remain verifiable
forever; the conformance corpus proves it — the golden layout hashes of
the 1.0–1.3 vectors are **unchanged** by this engine version.

Engine 1.4 adds **complex-script shaping** and is the first engine to
use the format **0.4** `glyphs` display op.

## 1. New pinned dependencies

| Kind | Item | SHA-256 / version |
|---|---|---|
| Font (index 6) | NotoSansArabic-Regular | `bdff3e5659d67e67def05b33f749683b9376ae819d65d3dd62ac4640b3aaef48` |
| Font (index 7) | NotoSansDevanagari-Regular | `306b53ecfb182a504dd8a7446093c316387d2fd8dc350d0792ed1753fe0996cd` |
| Shaper | `rustybuzz` (pure-Rust HarfBuzz port) | `=0.14.1` |

Each is part of engine version 1.4's identity, exactly like the metrics
parser and the other pinned faces. `rustybuzz` operates entirely in
integer font units; with a pinned version and pinned fonts, shaping is
byte-identical on every platform — the property the whole contract
rests on.

## 2. Shaping

Codepoints in the **Arabic** blocks (U+0600–06FF, U+0750–077F,
U+08A0–08FF, U+FB50–FDFF, U+FE70–FEFF) and the **Devanagari** blocks
(U+0900–097F, U+A8E0–A8FF) are routed to face 6 / face 7 respectively
(per-character, like the 1.2 Hebrew fallback) and **shaped**:

- Each maximal same-face run is shaped by the pinned shaper. Script,
  direction, and language are derived deterministically from the run
  content (`guess_segment_properties`): Arabic shapes right-to-left with
  joining and ligatures; Devanagari reorders matras and forms
  conjuncts.
- A run never crosses a space (a space resolves to a simple face,
  splitting the run), so each shaped run is a single word — measured
  during line breaking exactly as it is shaped at emission. Line
  breaking is otherwise unchanged (greedy, at spaces); shaped words are
  not hyphenated.
- Advances and offsets are scaled from font units to micrometers with
  the same `muldiv` rule as every other advance.

Other scripts that 1.3 refused and 1.4 still cannot set faithfully — CJK,
Thai/Lao, Hebrew-is-not-shaped (handled since 1.2), other Indic scripts
(no pinned font) — remain **refused**, never mis-rendered.

## 3. The `glyphs` display op (format 0.4)

A shaped run is emitted as a `glyphs` op, not a `text` op:

```
glyphs = {
  "op": "glyphs", "x", "y",        ; run origin: left edge, baseline (mm)
  "font": uint, "size": float, "color": bytes4,
  "g": [ [gid, x_advance, x_offset, y_offset, cluster], … ],
  "text": tstr,                    ; logical source substring
  "src": [uint], "range": [uint,uint],  ; back-reference, as for text ops
}
```

- **Glyphs are in visual order** (the shaper reorders RTL runs to visual
  order), so consumers draw them left-to-right from `x`, accumulating
  `x_advance`; `x_offset`/`y_offset` (mm) position marks. No per-op
  `rtl` flag is needed.
- `cluster` is the logical UTF-8 byte offset of each glyph within
  `text`. **`text`/`range`/`src` stay logical**, so selection, search,
  extraction, accessibility, and disclosure operate on source text, not
  glyphs — visual reordering and ligatures never corrupt the meaning.
- Positions are exact µm/1000 conversions, so the op encodes
  deterministically and the page object hash is platform-stable.

Consumers: the rasterizer draws each glyph id by outline at its shaped
position (outlines from the same pinned face); the PDF exporter places
each glyph explicitly (the shaped advances differ from the font's
defaults) and builds ToUnicode from the clusters; viewers extract the
logical `text` and highlight shaped runs at run granularity (glyph-level
geometry without the shaper is out of scope).

## 4. Bidi

A line containing Arabic is ordered by UAX #9 (as for Hebrew in 1.2);
each Arabic run is emitted as one or more `glyphs` ops at its visual
position. A `dir=rtl` document right-aligns the line box. Devanagari is
left-to-right and uses the normal LTR path. Justification still does not
apply to bidi lines.

## 5. Determinism

Unchanged in kind: integer micrometers throughout; the fonts and shaper
are pinned by hash / exact version; shaping never depends on iteration
order. The conformance corpus carries a golden hash for 1.4
(`valid/laid-out-1.4.vsd`) alongside the unchanged 1.0–1.3 hashes; a
conforming implementation MUST reproduce all five.

## 6. Still deliberately absent (future versions)

CJK and vertical text (need CJK fonts and breaking rules), Thai/Lao
dictionary line breaking, other Indic and complex scripts (need their
pinned fonts), bracket mirroring (UAX #9 L4) and full mark positioning
refinements, multi-column layout, and MathML layout.
