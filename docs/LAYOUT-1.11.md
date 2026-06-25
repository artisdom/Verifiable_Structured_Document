# vsd-layout/1.11 — Layout Engine Contract (delta over 1.10)

**Status:** Normative for engine id `vsd-layout` version `1.11.0`.
This document specifies only the differences from
[LAYOUT-1.10.md](LAYOUT-1.10.md) (and transitively 1.9 … 1.0); everything
not mentioned here is **identical to 1.10**.

1.0–1.10 caches remain verifiable forever. Engine 1.11 adds **MathML Core
(subset) layout**. It introduces **no format change** (the `math` node
and its `mathml` string are unchanged): documents without a `math` node
are byte-identical to 1.10, and even the `doc_id`s do not move. The
1.0–1.10 golden layout hashes are **unchanged**, proven by the
conformance vectors (only a new `valid/laid-out-1.11.vsd` is added, with
zero `layout_hash` and zero `doc_id` changes elsewhere).

Before 1.11 a `math` node rendered as its pre-rendered fallback image, or
(absent one) its MathML *source* as a code block. Engine 1.11 typesets
the MathML itself.

## 1. New pinned dependency

| Kind | Item | SHA-256 |
|---|---|---|
| Font (index 24) | STIXTwoMath-Regular | `95bc2729e41faf93b0bcae9e96c4dc4da45855067fd0581e621e30734fe8d90b` |

**STIX Two Math** (SIL OFL 1.1) is a CFF/OpenType font carrying a real
OpenType `MATH` table. It is read **only** by the MathML layout path
(`crate::mathml`); it is **never** in any script-routing map
(`Face::shaped_for`, `for_char`, `extended_for`), so it cannot change any
frozen engine's output. Metrics come from the same pinned `ttf-parser`
the rest of the engine uses; the `MATH` table's `Constants` and vertical
glyph `Variants` are the sole source of math spacing and growable-glyph
selection. Being CFF, it embeds in PDF via the existing
`FontFile3`/`CIDFontType0` path and subsets via the existing CFF
subsetter (self-verified, full-font fallback).

## 2. Supported MathML subset

A block-level `math` node's `mathml` string is parsed by a strict,
dependency-free reader (numeric character references `&#…;`/`&#x…;`
fully; a fixed table of common named entities; **unknown named entities
are an error**). The supported elements are:

`math`, `mrow`, `mstyle`, `mpadded` (grouping); `mi`, `mn`, `mo`,
`mtext` (tokens); `mspace`; `msup`, `msub`, `msubsup` (scripts); `mfrac`
(fractions); `msqrt`, `mroot` (radicals); `munder`, `mover`,
`munderover` (under/over-scripts / limits); `mfenced` (expanded to
open + separated children + close).

Anything else — `mtable`/`mtr`/`mtd` (matrices), `mmultiscripts`,
`menclose`, `maction`, `semantics`, an unknown element, an element with
the wrong child count, a named entity not in the table, or a glyph the
math font lacks — is **refused** (`Unsupported`). When a `math` node also
carries a `fallback` image, an unsupported formula falls back to that
image; otherwise the refusal stands. The engine never mis-renders math
(it never silently drops or approximates an unsupported construct, and
never dumps MathML markup as text).

## 3. Layout model (normative, integer micrometers)

Math is laid out as nested boxes, each with an advance `width` and
`ascent`/`depth` extents measured from its baseline (up positive). All
arithmetic is integer micrometers with the engine's `muldiv` rounding;
the only inputs are the pinned font's `MATH` constants, `hmtx` advances,
`cmap`, and glyph ink bounding boxes (from the outline). Key rules:

- **Tokens.** Glyphs come from the math face by `cmap`. A single-letter
  `mi` is set in **mathematical italic** (its Unicode Math Italic
  codepoint; `h` → U+210E ℎ), per MathML's default `mathvariant`.
  Invisible operators (U+2061–2063) are zero-width.
- **Base size.** The display base size is the body size; scripts shrink
  by the `MATH` `ScriptPercentScaleDown` (and `ScriptScript…` for the
  second level).
- **Operator spacing.** A small built-in operator dictionary (binary and
  relational symbols `+ − = < > × ÷ ± ∓ ⋅ ∗ ≤ ≥ ≠ ≡ ≈ → ← ↔ ∈ ∉ ∪ ∩`)
  takes a medium space (4/18 em) on each side; fences and punctuation do
  not.
- **Scripts** (`msup`/`msub`/`msubsup`) use `SuperscriptShiftUp`,
  `SubscriptShiftDown`, `SubSuperscriptGapMin`, and `SpaceAfterScript`.
- **Fractions** center numerator and denominator over a bar of
  `FractionRuleThickness` centered on `AxisHeight`, honoring the
  numerator/denominator shifts and the minimum gaps.
- **Radicals** select a √ glyph from the `MATH` vertical glyph
  **Variants** tall enough to span the radicand, draw the overbar rule
  (`RadicalRuleThickness`, `RadicalVerticalGap`, `RadicalExtraAscender`),
  and place an optional `mroot` degree raised by
  `RadicalDegreeBottomRaisePercent`.
- **Under/over** (`munder`/`mover`/`munderover`) center the scripts above
  and below the base using the limit gap constants.

The formula becomes one block atom: glyphs emit as positioned
single-glyph `GlyphRun`s in face 24, rules as `Rect`s, and the formula is
**centered** in the available width (column width under multi-column).
The whole node is one back-reference target (`node_path`); math has no
per-character source range, so selection/extraction treat the formula as
a unit (its `mathml`/alt remains the text-layer source).

## 4. Scope and honest limits

- **Block-level math only.** Inline math (`Inline::Math` inside a
  paragraph) is unchanged from prior engines (its `mathml` string is
  surfaced on the text-extraction path); integrating math into line
  breaking and inline baselines is left to a later engine.
- **Non-stretchy fences.** `mfenced`/`mo` brackets are set at base size;
  they do not grow to a tall fenced expression in 1.11 (a documented
  limitation, not a mis-render — the bracket is still the correct glyph).
- **No `displaystyle`/`scriptlevel` attribute control, no line breaking
  inside a formula, no matrices.** These are refused or ignored as noted
  in §2.
- Earlier engines (≤ 1.10) do not lay out MathML; the capability is
  version-gated, so frozen engines are byte-identical.

## 5. Determinism

Box assignment is a pure function of the MathML tree and the pinned
font's tables; there are no floats until op emission. Two machines, two
OSes, one document → byte-identical render cache, as for every prior
engine. The conformance corpus gains `valid/laid-out-1.11.vsd` (the
quadratic formula: a fraction over a radical with a superscript and
binary operators); the 1.0–1.10 golden layout hashes are unchanged.
