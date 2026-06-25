# vsd-layout/1.10 — Layout Engine Contract (delta over 1.9)

**Status:** Normative for engine id `vsd-layout` version `1.10.0`.
This document specifies only the differences from
[LAYOUT-1.9.md](LAYOUT-1.9.md) (and transitively 1.8 … 1.0); everything
not mentioned here is **identical to 1.9**.

1.0–1.9 caches remain verifiable forever. Engine 1.10 adds **section-level
multi-column layout**. It uses the **additive format-0.6 `cols`
attribute** on the section node; documents that contain no multi-column
section are byte-identical to 1.9 (the 1.0–1.9 golden layout hashes are
**unchanged**, proven by the conformance vectors — only `doc_id`s move
under the format-version bump, as at every additive minor).

## 1. New format surface (format 0.6)

A section node MAY carry an integer `cols` attribute:

| Node | Key | Type | Meaning |
|---|---|---|---|
| `sec` | `cols` | uint `2..=64` | flow this section's content into `cols` equal columns |

`cols == 1` is the default single column and is **omitted** from the
encoding, so every pre-0.6 section is byte-identical. `cols` of `0` or
`1` in the wire form is a decode error (single canonical encoding: a
single-column section never carries the key). The format minor version
becomes **0.6**; readers still gate on the major version only.

No new display op: column layout reuses the existing positioned
`text`/`glyphs`/`rect`/`image` ops. The only engine-internal change is
that an op's horizontal origin may be offset by a column's left edge;
single-column flow supplies a zero offset and is therefore byte-identical.

## 2. Geometry (normative, integer micrometers)

For a section with `n` columns laid out across the content width
`W = page_width − 2·MARGIN`:

- gutter between columns: `COLUMN_GUTTER = 5000` µm (5 mm);
- column width: `col_w = (W − (n−1)·COLUMN_GUTTER) / n` (integer division);
- left edge of column `i` (0-based): `MARGIN + i·(col_w + COLUMN_GUTTER)`.

If `col_w < MIN_COLUMN_WIDTH = 20000` µm (20 mm) the section is
**refused** (`Unsupported`) rather than rendered unreadably narrow.

## 3. Column flow (`column-fill: auto`)

Columns are filled **sequentially**, never balanced:

1. The column band begins at the section's current position on its first
   page — just below any preceding content (its `space_after` gap), or at
   the top margin on a fresh page — and runs to the bottom margin.
2. Content fills the current column top-to-bottom. When the next atom
   (a text line, table row, figure, …) would cross the bottom margin, the
   next column to the right is started at the band top.
3. When the rightmost column fills, a **new page** begins and the band
   resets to the top margin (`column-fill: auto`: the final page is **not**
   balanced — a short section may leave its rightmost columns empty).
4. After the section, full-width flow resumes just below the **deepest**
   column used on the section's final page.

Edge rules, all deterministic:

- An atom taller than a whole column is placed anyway (it cannot fit
  anywhere), exactly as single-column flow over-runs a too-tall atom.
- If the section starts so low on a page that an empty column cannot fit
  the next atom, the whole band is pulled to a fresh page rather than
  overflowing the bottom margin.
- A `pagebreak` hint inside the section flushes all columns and starts a
  new page.
- Heading **keep-with-next** is honored at the column level: a heading
  that would strand at a column bottom (no room for it plus one body
  line) moves to the next column / page.

Reading order = op order: column 0 top-to-bottom, then column 1, …, then
the next page. Back-references (`node_path`, `char_range`) are unchanged,
so search, extraction, selection, and disclosure are unaffected by the
column geometry.

## 4. Refusals and limits (honest scope)

- **Earlier engines** (≤ 1.9) **refuse** a section with `cols > 1` with
  `Unsupported`, rather than silently collapsing it to one column. The
  capability is version-gated; frozen engines are byte-identical.
- **Nested** multi-column sections (a `cols > 1` section inside another)
  are refused in 1.10 rather than mis-rendered.
- **Vertical writing mode** (`vertical-rl`) is independent of `cols`;
  multi-column is a horizontal-flow feature.
- **Widow/orphan control** (engine 1.3) is a page-flow rule and is **not**
  applied at column breaks in 1.10: paragraph lines flow naturally across
  a column boundary. This is a quality refinement, not a fidelity issue —
  lines are correctly laid out and broken only at legitimate
  opportunities — and is left to a later engine version.
- Multi-column section content is **not** consulted through the
  incremental fragment cache (`LayoutSession`) in 1.10; it is always
  fragmented directly. The result is identical with or without a session
  (the cache is an optimization, never an oracle); column caching is a
  possible future refinement.

## 5. Determinism

All column arithmetic is integer micrometers with the existing `muldiv`
rounding; there are no floats until op emission. Column assignment is a
pure function of content height and page geometry. Two machines, two
OSes, one document → byte-identical render cache, as for every prior
engine. The conformance corpus gains `valid/laid-out-1.10.vsd` (a
two-column article that breaks across columns and pages); the 1.0–1.9
golden layout hashes are unchanged.
