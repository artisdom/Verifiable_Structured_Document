# vsd-layout/1.6 — Layout Engine Contract (delta over 1.5)

**Status:** Normative for engine id `vsd-layout` version `1.6.0`.
This document specifies only the differences from
[LAYOUT-1.5.md](LAYOUT-1.5.md) (and transitively 1.4 … 1.0); everything
not mentioned here is **identical to 1.5**, including the
integer-micrometer arithmetic, the `muldiv` primitive, faces, shaping,
bidi mirroring, justification, hyphenation, widow/orphan control, the
format-0.4 `glyphs` display op, and back-reference semantics.

1.0–1.5 caches remain verifiable forever; the conformance corpus proves
it. 1.6 introduces **no format change** (it reuses the format-0.4
`glyphs` op), so every prior `.vsd` vector is byte-identical and the
golden layout hashes of the 1.0–1.5 vectors are **unchanged**.

Engine 1.6 adds **Thai** and **Lao**: shaping (as for any other complex
script) plus **dictionary-based line breaking**, because these scripts
are written without spaces between words.

## 1. New pinned dependencies

| Kind | Item | SHA-256 |
|---|---|---|
| Font (index 17) | NotoSansThai-Regular | `61cf814eec46b294d6ea4401ac295d0cecd5207bd2331dcc5a15e7301d30ee44` |
| Font (index 18) | NotoSansLao-Regular  | `0a86e5e1ccfe34ca78c43fac6829dc751b42bcc469272a9a55325aae587bfbe7` |
| Dictionary | ICU `thaidict.txt` | `3166abde40c0f44ab91c28f5ce96d7d1472cb7882e1c0bda0a72f8f69dba4274` |
| Dictionary | ICU `laodict.txt`  | `3c876934a3fa81031d2333525eafaca6a7c9f842e3b98f18c38880420afb5d36` |

Both fonts shape via the **same** pinned `rustybuzz` (`=0.14.1`) used
since 1.4 — no new shaper. The dictionaries are the ICU `brkitr` word
lists, embedded by value and frozen with the engine version like every
other table. The segmenter does only integer/string work with hashed
lookups, so its output is byte-identical on every platform.

## 2. Shaping

Codepoints in the **Thai** block (U+0E00–0E7F) and **Lao** block
(U+0E80–0EFF) are routed to face 17 / face 18 and shaped exactly as in
1.4 §2 (per-character fallback, one shaped run per breakable unit,
measured during line breaking exactly as shaped at emission, emitted as
a `glyphs` op with logical `text`/`range`/`src` preserved). Both scripts
are left-to-right.

**Freeze gate.** As with 1.5, `Face::shaped_for` is a forward-growing
map but the per-version gate `EngineVersion::shaped_face` decides what is
shaped vs. refused: engine 1.5 shaped every Brahmic script **except**
Thai/Lao and still refused them (it had no dictionary); engine 1.6 adds
Thai + Lao. 1.0–1.5 are byte-identical.

## 3. Dictionary line breaking

Thai and Lao have **no inter-word spaces**, so line-break opportunities
cannot come from spaces. The engine discovers them by segmenting each
maximal Thai (or Lao) run against the script's pinned dictionary with a
deterministic **forward longest-match** algorithm:

- Starting at the run's first character, consume the **longest**
  dictionary word that is a prefix there; the byte boundary immediately
  after it is a permitted break point. Continue from that boundary.
- A character that begins no dictionary word advances by one scalar and
  introduces **no** break, so out-of-dictionary spans (names, numerals,
  novel words) are never split mid-cluster.

These break points feed the same greedy first-fit line breaker as
spaces and hyphenation, with two differences from a hyphenation break:
they are **zero-width** (no separator consumed) and carry **no hyphen
glyph**. The breaker prefers a dictionary boundary, then a hyphenation
point (for any Latin in the run), then — only for an unbreakable
overflow wider than the line — a by-glyph force break. A break point is
always a whole-word boundary, hence cluster-safe.

Line breaking is otherwise unchanged: greedy, integer-µm widths measured
by the same shaper used at emission, so the measured width of a wrapped
prefix equals the width of the run actually emitted on that line.

## 4. Determinism

Unchanged in kind: integer micrometers throughout; fonts, shaper, and
dictionaries pinned by hash / exact version; the segmenter is a pure
function of the run and the pinned word list. The conformance corpus
carries a golden hash for 1.6 (`valid/laid-out-1.6.vsd`, a Thai and a
Lao paragraph wrapped on a narrow page) alongside the unchanged 1.0–1.5
hashes; a conforming implementation MUST reproduce all seven.

## 5. Still deliberately absent (future versions)

CJK and vertical text (need CJK fonts and ideographic / vertical
breaking rules), complex scripts beyond the major Brahmic + Thai/Lao set
(Myanmar, Khmer, Tibetan, Ethiopic, …), multi-column layout, and MathML
layout. The dictionary segmenter here is a forward longest-match
baseline; a future version could refine it (e.g. minimal-word-count or
frequency-weighted segmentation) as its own frozen engine version.
