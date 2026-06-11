# vsd-layout/1.1 — Layout Engine Contract (delta over 1.0)

**Status:** Normative for engine id `vsd-layout` version `1.1.0`.
This document specifies only the differences from
[LAYOUT-1.0.md](LAYOUT-1.0.md); everything not mentioned here is
**identical to 1.0**, including the integer-micrometer arithmetic, the
`muldiv` primitive, line breaking, pagination, constants, and
back-reference semantics.

Version pinning is load-bearing: a render cache records the engine
version it was produced with, and verifiers MUST recompute under that
exact version's contract. 1.0 caches remain verifiable forever; this
document changes nothing about them.

## 1. What 1.1 adds: real bold and italic faces

Engine 1.0 has a single face; style spans affect nothing. Engine 1.1
honors the `b` (bold) and `i` (italic) flags of the document's style
table (spec §6) with **real font faces** — never synthetic emboldening
or shearing, which would have no metric truth.

### 1.1 The pinned faces

All from the same Noto Sans release as 1.0
(notofonts/latin-greek-cyrillic **v2.015**, hinted TTF, SIL OFL 1.1),
identified by display-list font index:

| Index | Face | Size (bytes) | SHA-256 |
|---|---|---|---|
| 0 | NotoSans-Regular | 621,572 | `478c558ea716033cd60c03438f628dfa75694dcf6b5f6d505a2f05fd2b4f3823` |
| 1 | NotoSans-Bold | 631,484 | `1df075a380fc7cb898acf64c1f7b3b4dd780de3caa860178bf929de35817a913` |
| 2 | NotoSans-Italic | 639,124 | `467e3f89eeca4108bb8710a2b9e0cf2281ac56d5b0609211a83776d0505eecb5` |
| 3 | NotoSans-BoldItalic | 646,092 | `1b602a9d6353be42c91df097a4857b69fa2696f26703d7a33b54a15d87c2622c` |

Each face binary is part of the engine version. Advances come from the
attributed face's `hmtx` via the same `muldiv` scaling as 1.0.

### 1.2 Face resolution

Walking a block's inline content, each text fragment carries the
OR-combination of the `b`/`i` flags of every enclosing span's style
(nested spans combine; a missing or out-of-range style index
contributes nothing):

```
face = pick(bold, italic):  (false,false)→0  (true,false)→1
                            (false,true)→2   (true,true)→3
```

`u` (underline) and `mono` still affect nothing in 1.1. Headings,
code, list labels, field labels/values, and figure structure are
unchanged (they do not pass through span styling).

### 1.3 Measurement and emission

- Line breaking measures every character in its attributed face;
  separator spaces are measured in their own attributed face. The
  algorithm itself (greedy first-fit, force-break rules) is unchanged.
- Text runs split at face boundaries in addition to link boundaries;
  each run's `font` field carries the face index (1.0 always emits 0).
- **Vertical rhythm never changes with faces:** line height remains
  `muldiv(size, 7, 5)` and baselines use the *Regular* face's ascent,
  regardless of the faces on the line. Faces change advances and
  glyphs, not geometry rows.

## 2. Determinism

Unchanged in kind: all arithmetic is integer micrometers; the four
faces' metric tables are pinned by hash; the same document and version
produce byte-identical page objects on every platform. The conformance
corpus carries golden hashes for both 1.0 (`valid/laid-out.vsd`) and
1.1 (`valid/laid-out-1.1.vsd`); a conforming implementation MUST
reproduce both.

## 3. Still deliberately absent (future versions)

RTL/bidi, CJK and vertical text, complex scripts (these require a
pinned shaper — the next major engine effort), justification and
hyphenation, multi-column, underline decoration, monospace face,
widow/orphan control. Engine 1.1 continues to refuse `dir=rtl` rather
than mis-render it.
