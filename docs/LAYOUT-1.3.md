# vsd-layout/1.3 — Layout Engine Contract (delta over 1.2)

**Status:** Normative for engine id `vsd-layout` version `1.3.0`.
This document specifies only the differences from
[LAYOUT-1.2.md](LAYOUT-1.2.md) (and transitively 1.1 / 1.0);
everything not mentioned here is **identical to 1.2**, including the
integer-micrometer arithmetic, the `muldiv` primitive, greedy
first-fit line breaking, face resolution, justification, bidi, and
back-reference semantics.

Version pinning is load-bearing: a render cache records the engine
version it was produced with, and verifiers MUST recompute under that
exact version's contract. 1.0/1.1/1.2 caches remain verifiable forever;
this document changes nothing about them, and the conformance corpus
proves it — the golden layout hashes of `valid/laid-out.vsd` (1.0),
`valid/laid-out-1.1.vsd`, and `valid/laid-out-1.2.vsd` are **unchanged**
by this engine version.

Engine 1.3 introduces **no display-list format change**: it emits only
ops already defined through format 0.3. The inserted hyphen (below) is
an ordinary `text` op.

Engine 1.3 is "page furniture": two pagination/line-breaking
refinements, both pure deterministic integer work, layered on the 1.2
profile.

## 1. New pinned asset

| Asset | Bytes | SHA-256 |
|---|---|---|
| `hyph-en-us.pat.txt` | 31,489 | `0f57318b878b132547ae92db39a6e1d1cf2a05d9008874955d6ecb910007a463` |

The TeX `hyph-en-us` Knuth–Liang pattern set (Liang's original en-US
patterns; freely redistributable). Embedded at build time and part of
engine version 1.3's identity, exactly like the pinned font binaries.
No fonts are added in 1.3.

## 2. Hyphenation

Engine 1.3 hyphenates **English body paragraphs** using the pinned
patterns and the classic Knuth–Liang algorithm.

### 2.1 Eligibility (all must hold)

- The block is a body paragraph (the same class that 1.2 justifies —
  not headings, captions, list labels, fields, or code).
- The document language (`doc.lang`) begins with `en` (case-insensitive).
  Other languages are laid out exactly as in 1.2 (no hyphenation), so
  the engine never invents a break it has no patterns for.
- The line is not bidi-reordered (a line containing RTL characters, or
  any line of a `dir=rtl` document, is never hyphenated in 1.3 — a
  hyphen on a reordered line is deferred with the rest of complex-script
  work).

### 2.2 Break points

A word is hyphenated only if it is a pure ASCII-alphabetic token of at
least `LEFT_MIN + RIGHT_MIN` letters. The dotted word `.word.` is scored
by every matching pattern, taking the maximum value at each
inter-letter point; a break is permitted where the value is odd. The
TeX defaults apply: at least **2** letters before and **3** after a
break (`\lefthyphenmin=2`, `\righthyphenmin=3`). All of this is integer
and string work — identical on every platform.

### 2.3 Line breaking with hyphenation

Greedy first-fit is unchanged for words that fit. When a word overflows
the current line, the engine takes the **longest** hyphenation prefix
whose width *plus the hyphen* still fits, emits the line ending at that
break, and continues with the remainder (which may hyphenate again, fit
a full line, or — only when unhyphenatable — force-break by glyph, as
in 1.0). With no hyphenation eligible, breaking is byte-identical to
1.2.

The hyphen advance is measured as `-` (U+002D) in the **style face of
the last character before the break** (script fallback never applies),
using the same `muldiv` scaling as all advances. Line breaking and
emission compute it identically, so the honored width limit and the
drawn position always agree.

### 2.4 Hyphen emission

A hyphenated line emits, after its content, one `text` op for `-` at the
line's right end:

- It is **layout decoration, not source content**, so it carries an
  **empty `char_range`** (`start == end`, at the break point) — exactly
  the convention already used for list bullets and field labels.
  Consumers MAY drop empty-range runs from copy/extraction.
- Its width is already included in the line's measured width, so on a
  justified line (every line of a body paragraph except the last) the
  hyphen lands flush at the right margin.
- It is never an RTL run.

## 3. Widow and orphan control

When paginating the lines of a single paragraph (engine 1.3 fragments a
paragraph into one atom per line), a page break MUST keep **at least two
lines on each side**:

- A paragraph that does not fit in the remaining space but **fits on a
  page by itself** is moved whole to the next page rather than split
  (this also subsumes 2- and 3-line paragraphs, which cannot be split
  without stranding a single line).
- A paragraph **taller than a full page** is split, but every break
  still leaves ≥ 2 lines on the page above and ≥ 2 lines on the page
  below, choosing the split point closest to the page bottom that
  satisfies both. The procedure always makes forward progress (at least
  one line per page), so pagination terminates.

Other blocks (headings — which already use keep-with-next — tables,
lists, figures, code) paginate exactly as in 1.2.

## 4. Determinism

Unchanged in kind: all arithmetic is integer micrometers; the patterns
are pinned by hash; pattern lookups never depend on map iteration
order; the same document and version produce byte-identical page
objects on every platform. The conformance corpus carries a golden hash
for 1.3 (`valid/laid-out-1.3.vsd`) alongside the unchanged 1.0/1.1/1.2
hashes; a conforming implementation MUST reproduce all four.

## 5. Still deliberately absent (future versions)

Shaped scripts (Arabic, Indic — require a pinned shaper plus their
fonts, the next major engine effort), CJK and vertical text, Thai/Lao
dictionary line breaking, multi-column layout, MathML layout,
non-English hyphenation dictionaries, and per-language hyphenation
minimums. Engine 1.3 continues to **refuse** the scripts 1.2 refuses,
never mis-rendering them.
