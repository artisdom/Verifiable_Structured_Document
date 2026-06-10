# vsd-layout/1.0 — Layout Engine Determinism Contract

**Status:** Normative for engine id `vsd-layout` version `1.0.0`
**Scope:** the minimal profile — single-column, left-to-right, Latin/Greek/
Cyrillic block layout. Wider coverage arrives in later engine versions;
documents pin the version they were laid out with, and old caches stay
verifiable against their pinned engine forever.

This document is the heart of spec §5: the render cache is a
**deterministic projection** of the content tree, and any validator can
re-run the engine and compare. Two conforming implementations of this
contract, on any platform, MUST produce byte-identical page objects for
the same document. Every "MUST" below is conformance-testable; the
public vectors (`testdata/`) carry golden layout hashes.

---

## 1. The determinism strategy: integer micrometers

All layout arithmetic is performed in **signed 64-bit integer
micrometers** (µm; 1000 µm = 1 mm). There is no floating-point
arithmetic anywhere in layout — which makes "defined f64 operation
order" unnecessary: integer arithmetic has no rounding modes, no
platform variance, no reassociation hazards.

The only defined float operations occur at **emission**, converting
final integer results into the display-list fields:

- millimetres: `f64(x_µm) / 1000.0`
- points: `f64(size_µm) * 72.0 / 25400.0`

Each is a single IEEE 754 binary64 operation on an exactly-representable
integer input (all magnitudes here are far below 2⁵³), so the result is
bit-identical on every IEEE 754 platform.

### 1.1 The rounding primitive

Every scaled quantity uses one primitive, evaluated in 128-bit
intermediate precision:

```text
muldiv(a, b, d) = floor((a × b + floor(d / 2)) / d)        d > 0
```

i.e. multiply, then divide with round-half-up. No other rounding
operation exists in this engine.

## 2. The pinned font

| Property | Value |
|---|---|
| Family | Noto Sans Regular |
| Release | notofonts/latin-greek-cyrillic **v2.015**, hinted TTF |
| File size | 621,572 bytes |
| SHA-256 | `478c558ea716033cd60c03438f628dfa75694dcf6b5f6d505a2f05fd2b4f3823` |
| License | SIL OFL 1.1 (embedded alongside) |

The font binary is embedded in the engine and is **part of the engine
version**: its `cmap`, `hmtx`, `hhea` and `head` tables are the sole
source of glyph mapping and metrics. A different font file (even a
different Noto release) is a different engine version.

- `upem` — units per em, from `head` (Noto Sans: 1000).
- `ascent`, `descent` (negative), `line_gap` — from `hhea`.
- Glyph lookup: Unicode codepoint → glyph id via `cmap`. Codepoints with
  no `cmap` entry map to glyph 0 (`.notdef`) and use its advance.
- Advance width: `hmtx` for the glyph id.

Scaling font units to µm at font size `s` µm:
`adv_µm = muldiv(adv_units, s, upem)` — and identically for ascent,
descent, and line gap.

## 3. Shaping (deliberately minimal in 1.0)

Per-character horizontal advance accumulation. Specifically:

1. Text is processed codepoint by codepoint in document order.
2. Each codepoint maps to exactly one glyph (no ligatures, no kerning,
   no contextual forms, no combining-mark positioning, no bidi).
3. The run advance is the sum of scaled glyph advances; **summation is
   exact** (integer µm).
4. `dir` MUST be `ltr`; the engine refuses `rtl` documents
   (`UnsupportedByEngine`). RTL arrives in a later engine version.
5. C0 control characters other than those given meaning below
   contribute no glyph and no advance.

## 4. Text preparation

For every block, the engine derives its **layout text**:

- Inline content is concatenated in tree order (span and link
  boundaries do not affect text, only attribution and color).
- Whitespace runs (`U+0020`, `U+0009`, `U+000A`, `U+000D`) collapse to a
  single `U+0020`; leading and trailing whitespace is stripped.
- **Exception — code blocks:** text is verbatim; `U+000A` is a mandatory
  line break; `U+0009` advances to the next multiple of 4 space-widths
  from the line start; `U+000D` is dropped.

`TextRun.char_range` values are **byte ranges into the block's layout
text** (`node_path` identifies the block; see §9).

## 5. Line breaking

A tailored subset of UAX #14, enumerated exhaustively — there are
exactly two break classes in 1.0:

1. **Break opportunities** exist only after a collapsed space. The space
   itself is dropped when a break is taken there (it contributes no
   advance at line end).
2. **Mandatory breaks** exist only at `U+000A` in code blocks.

Lines are filled **greedily** (first-fit): take the longest prefix of
unbroken segments that fits the available width. A segment longer than
the line is force-broken before the first glyph that would overflow,
with a minimum of one glyph per line. There is no hyphenation and no
justification in 1.0; all text is set ragged-right, left-aligned.

## 6. Page model and normative constants

Geometry is an engine input (default **A4**: 210000 × 297000 µm).
Margins are fixed: **20000 µm** on all four sides. Content width
`CW = page_width − 40000`.

All constants are normative; pt equivalents are informative.

| Constant | µm | ≈ pt |
|---|---|---|
| `SIZE_BODY` | 3881 | 11 |
| `SIZE_H1` / `H2` / `H3` | 8467 / 6350 / 4939 | 24 / 18 / 14 |
| `SIZE_H4` / `H5` / `H6` | 4233 / 3881 / 3528 | 12 / 11 / 10 |
| `SIZE_CODE` | 3528 | 10 |
| `SIZE_CAPTION` | 3175 | 9 |
| Line height | `muldiv(size, 7, 5)` | 1.4 × |
| Space after block (default) | 2117 | 6 |
| Space before heading | 4233 (h1: 6350) | 12 / 18 |
| List indent | 7000 | — |
| List item gap | 1058 | 3 |
| Table cell padding | 1000 | — |
| Table rule thickness | 100 | — |
| Redaction bar height | one body line height | — |
| Field blank width | 30000 | — |

Vertical stacking: blocks are placed top to bottom; the gap between two
blocks is `max(space_after(prev), space_before(next))`. A line of text
at cursor `y` places its baseline at `y + ascent_µm`.

Colors are RGBA8: text black `#000000FF`; link text `#1A0DABFF`; table
rules black; table header cell background `#F0F0F0FF`; redaction bar
black.

## 7. Block layout algorithms

**Paragraph / heading**: layout text → lines (§5) at the element's size;
emit one `TextRun` per line segment (segments split where color
attribution changes, e.g. link spans).

**List**: items indent by 7000 µm. Each item's label — `•` followed by
`U+0020` for unordered, `<n>.` followed by `U+0020` (1-based decimal)
for ordered — is a `TextRun` at the indent's left edge attributed to the
list node with `char_range (0, 0)`; item content lays out in
`CW − 7000`.

**Code**: verbatim lines (§4), `SIZE_CODE`, no wrapping discipline
beyond force-breaking (§5 rule applies if a line exceeds width).

**Table**: column weights are the colspec widths (absent → 1.0),
converted once per colspec via `wᵢ = round_ties_away(width × 1000)` (the
single sanctioned f64→int conversion; inputs are author data). Column
widths: `colᵢ = muldiv(CW, wᵢ, Σw)` for all but the last column, which
takes the remainder (so columns always tile `CW` exactly). Cells lay out
recursively at `colᵢ − 2 × padding`; row height = tallest cell + 2 ×
padding. Header-row cells get the background rect. Grid rules: one
horizontal rule above the first row, one below every row, and vertical
rules at every column boundary, thickness 100 µm. Rows are atomic for
pagination; a row taller than a full page is placed anyway and may
overflow (flagged by tooling, not an error).

**Figure**: if the resource blob is PNG (`mime image/png`), intrinsic
size comes from the IHDR fields, converted at 96 dpi:
`µm = muldiv(px, 25400, 96)`. Any other resource uses a 40000 × 30000 µm
box. If wider than `CW`, scale to `CW` preserving aspect ratio
(`h = muldiv(h₀, CW, w₀)`). Emits one `Image` op; the caption (if any)
lays out below at `SIZE_CAPTION` after a 1058 µm gap.

**Field**: renders `<label>: ` (label, else id) at `SIZE_BODY`. If the
document's field layer (with computed fields evaluated) yields a value,
the value's text renders after it; otherwise a blank: a rect 30000 ×
100 µm sitting on the baseline.

**Math**: 1.0 lays out the MathML source verbatim as a code block
(normative limitation); a `fallback` resource, when present, renders as
a figure-style box instead.

**Redacted**: a black rect, full content width, one body line height —
a visible bar with, by construction, nothing underneath it.

**Page-break hint**: forces a page break unless already at the top of a
fresh page.

**Subtree refs** resolve transparently through the object store.

## 8. Pagination

Greedy, top-to-bottom:

- Paragraphs, headings, and code split **at line boundaries**: lines are
  placed until one does not fit, then a new page begins. (No
  widow/orphan control in 1.0.)
- Tables split at **row boundaries**.
- Figures and redaction bars are atomic.
- **Keep-with-next**: a heading is only placed if the remaining height
  after it fits at least one body line; otherwise it moves to the next
  page.
- An empty trailing page is never emitted; an empty document produces
  exactly one empty page.

## 9. Back-references

Every `TextRun` carries `node_path` and `char_range` (spec §5):

- `node_path` is the sequence of child indices from the root `doc` node:
  container children by index; list items by item index; table cells by
  reading-order cell index (head rows, body, foot, row-major). Figure
  captions carry the figure's path; field and list-label runs carry the
  field/list node's path.
- `char_range` is the UTF-8 byte range into the block's layout text
  (§4).

## 10. Output and the layout hash

Pages are emitted as display-list objects (spec §5) in deterministic
CBOR; ops appear in placement order: background rects, then rules, then
text/images, per block, in document order. The render-cache object
records `{engine name, version, geometry, pages, layout-hash}` where
`layout-hash = BLAKE3(canonical CBOR of the pages object-id array)`.

Verification levels:

1. **Structural** (cheap, always): `layout-hash` matches the page list;
   page objects decode; geometry positive.
2. **Recomputation** (`vsd verify --recompute`): re-run this engine on
   the content tree and require the identical page object ids. A cache
   that renders anything other than the tree's content cannot survive
   this — that is the property PDF structurally lacks.

A render cache also implies a **page index** (spec §9): for each page,
the page object id followed by the ids of resources placed on that
page, sorted, deduplicated.

## 11. What 1.0 deliberately does not do

No RTL/bidi, no CJK or complex-script shaping, no ligatures/kerning, no
hyphenation or justification, no bold/italic faces (style spans affect
attribution and color only, not metrics), no widow/orphan control, no
row-splitting, no incremental relayout. Each lands in a future engine
version (`1.1`+, ROADMAP §5 Phase 2f/2g); none of them can change what
`vsd-layout/1.0` produces, ever.
