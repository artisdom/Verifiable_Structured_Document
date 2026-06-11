# VSD — Verifiable Structured Document
## Container & Format Specification — Draft 0.1

> **SUPERSEDED.** This is the original 0.1 concept draft, kept for
> history. The normative, implementation-synced specification is
> **[spec/SPEC.md](spec/SPEC.md)** (version 0.2, CC-BY 4.0), with the
> layout engine contract in
> [docs/LAYOUT-1.0.md](docs/LAYOUT-1.0.md) and the conformance program
> in [spec/CONFORMANCE.md](spec/CONFORMANCE.md). Where this draft and
> the implementation disagree (chunk FourCC spellings, the manifest's
> `field-layer` key, the `salted` node, hybrid signatures), SPEC.md is
> authoritative.

**Status:** Concept draft
**Design goals:** layout fidelity of PDF · parseability of HTML · integrity model of Git · attack surface of a JPEG

---

## 1. Design invariants

Every decision below derives from five non-negotiable invariants:

| # | Invariant | Consequence |
|---|-----------|-------------|
| I1 | **Structure is canonical, pixels are cache.** | The typed content tree is the document. The fixed layout is a deterministic, reproducible projection of it. |
| I2 | **Zero executable content in core.** | No scripting engine. Interactivity is a declarative finite state machine with bounded semantics. |
| I3 | **Every object is content-addressed.** | Dedup, diff, partial fetch, and integrity verification fall out for free. |
| I4 | **One reference renderer, conformance-tested.** | "Renders identically everywhere" is enforced by test vectors, not hoped for. |
| I5 | **Cryptography over meaning, not bytes.** | Signatures cover the Merkle root of the content tree, surviving recompression and container repacking. |

---

## 2. Container layout

A `.vsd` file is a sequence of length-prefixed chunks, little-endian, in the spirit of PNG/RIFF but with 64-bit lengths and mandatory ordering rules.

```
┌──────────────────────────────────────────────┐
│ HEADER (32 bytes, fixed)                     │
├──────────────────────────────────────────────┤
│ MNFST  — manifest chunk (exactly one)        │
├──────────────────────────────────────────────┤
│ INDEX  — object index (exactly one)          │
├──────────────────────────────────────────────┤
│ OBJS   — object store (1..n chunks)          │
├──────────────────────────────────────────────┤
│ SIGS   — signature block (0..n)              │
├──────────────────────────────────────────────┤
│ TRAILR — trailer (offset of INDEX, CRC)      │
└──────────────────────────────────────────────┘
```

### 2.1 Header (32 bytes)

```
Offset  Size  Field
0       8     Magic: 0x89 'V' 'S' 'D' 0x0D 0x0A 0x1A 0x0A
              (PNG-style: catches FTP/text-mode corruption)
8       2     Major version (u16 LE)
10      2     Minor version (u16 LE)
12      4     Profile flags (bitfield: Core, Archive, Form, Stream)
16      8     Total file size (u64 LE) — truncation detection
24      8     Offset of TRAILR chunk (u64 LE)
```

Major version bumps are breaking; readers MUST refuse unknown majors. Minor bumps are additive; unknown chunk types within a known major are skippable if flagged non-critical.

### 2.2 Chunk framing

```
u64   length of payload
u32   chunk type (FourCC)
u32   flags (bit 0: critical, bit 1: compressed-zstd)
[..]  payload
u64   BLAKE3-64 truncated checksum of payload
```

### 2.3 Object store (`OBJS`)

The object store is the heart of the format. Every resource — content-tree nodes, images, font subsets, page render caches, metadata — is an **object**:

```
object_id = BLAKE3-256( canonical_encoding(object) )
```

- Encoding is **deterministic CBOR** (RFC 8949 §4.2 core deterministic encoding). Same logical object → same bytes → same hash, always.
- Objects are immutable. "Editing" a document produces new objects and a new manifest; unchanged objects are shared. A 50-page revision that changes one paragraph reuses ~all prior objects — version diffs are object-set diffs, exactly like Git trees.
- The `INDEX` chunk maps `object_id → (chunk_offset, intra_chunk_offset, length, codec)` so any object is one seek away.

### 2.4 Manifest (`MNFST`)

The manifest is the root object, by definition the document identity:

```cddl
manifest = {
  vsd-version:    [major: uint, minor: uint],
  root:           object-ref,        ; content tree root
  render-cache:   object-ref / null, ; layout projection (§5)
  resources:      object-ref,        ; resource table
  metadata:       object-ref,        ; Dublin Core-ish + custom
  provenance:     object-ref / null, ; C2PA-compatible chain (§8)
  page-index:     object-ref / null, ; streaming index (§9)
  profile:        "core" / "archive" / "form",
}
object-ref = bytes .size 32          ; BLAKE3-256
```

**The document's identity is `BLAKE3(manifest)`** — a single 32-byte value that commits, Merkle-style, to every byte of content. Two files with different compression, chunk order, or padding but the same manifest hash are *the same document*. This is what signatures sign (§7).

---

## 3. Content tree — the canonical layer

A typed tree of semantic nodes. This is what gets signed, extracted, diffed, indexed, and read by assistive tech and machines. CDDL sketch:

```cddl
node = doc / section / para / heading / table / figure /
       math / list / code / field / link / span / pagebreak-hint

doc     = { t: "doc",  lang: tstr, dir: "ltr"/"rtl", children: [+node] }
section = { t: "sec",  role: tstr, children: [+node] }
heading = { t: "h",    level: 1..6, children: [+inline] }
para    = { t: "p",    children: [+inline] }

table   = { t: "table",
            cols: [+colspec],
            head: [*row], body: [+row], foot: [*row] }
row     = { cells: [+cell] }
cell    = { span: [rows: uint, cols: uint] / null,
            scope: "row"/"col"/null,        ; real header semantics
            children: [+node] }

figure  = { t: "fig",  res: object-ref,     ; image or vector object
            alt: tstr,                       ; REQUIRED, non-empty
            caption: [*inline] }

math    = { t: "math", mathml: tstr,         ; MathML Core
            fallback: object-ref / null }    ; rendered vector

field   = { t: "field", id: tstr, kind: field-kind, ... }  ; see §6

inline  = text / span / link / math / footnote-ref
span    = { t: "span", style-ref: uint, children: [+inline] }
```

Key departures from PDF:

- **Tables are tables.** Cell topology, header scope, and spans are structural facts, not inferred from line positions. PDF table extraction is a research field; here it's a tree walk.
- **Alt text is mandatory** on figures (empty string permitted only with `decorative: true`). Accessibility is a validity condition, not an afterthought — a VSD that fails it is malformed, full stop.
- **Reading order is tree order.** There is no separate "logical structure" bolted onto draw commands, because there are no draw commands in the canonical layer.
- Large subtrees are stored as separate objects referenced by hash, so the tree itself is a Merkle structure — partial verification and lazy loading come free.

---

## 4. Resources

| Type | Codec | Notes |
|------|-------|-------|
| Raster images | JPEG XL (primary), AVIF | JXL losslessly recompresses legacy JPEG ~20% smaller — important for PDF migration |
| Vector graphics | **VSD-V**: a closed SVG subset | Paths, shapes, gradients, clips, text-as-path. **No** `<script>`, `<foreignObject>`, external refs, animation, CSS. Grammar is finite and fuzzable. |
| Fonts | WOFF2, subset per document | Subsetting is normative: the embedded subset MUST cover every codepoint used, verified at validation |
| Color | ICC profiles as objects; default sRGB / Display-P3 | Archive profile requires explicit ICC for print intent |

Every resource is an object in the store → identical logos/fonts across 1,000 invoices in an archive are stored once when files are packed into a VSD bundle.

---

## 5. Render layer — pixels as cache

The render cache is a **deterministic projection** of the content tree through a versioned layout engine:

```cddl
render-cache = {
  layout-engine: { name: tstr, version: tstr },  ; e.g. "vsd-layout/1.0"
  geometry:      { w: float, h: float, unit: "mm" },
  pages:         [+object-ref],                  ; display lists
  layout-hash:   bytes .size 32,
}
```

Each page is a **display list**: a flat array of positioned glyph runs, vector ops, and image placements — deliberately dumber than PDF content streams (no inline state machine, no arbitrary transform nesting, no Type 4 PostScript functions).

The determinism contract:

1. The layout engine spec is normative and versioned. Text shaping pins a HarfBuzz revision + bug-compatible behavior list; line breaking is Unicode UAX #14 with an explicitly enumerated tailoring table; float arithmetic uses round-to-nearest-even f64 with a defined operation order.
2. `layout-hash = BLAKE3(pages)` lets any validator **re-run layout and compare**. A mismatch means the cache lies about the content — the document is invalid.
3. A reader trusts the cache for speed; an auditor recomputes it for truth. This kills the classic PDF attack where visible pixels and extracted text disagree (shadow-text phishing, ATS keyword stuffing, contract-swap exploits).

Every glyph run carries a back-reference `(node-path, char-range)` into the content tree — so selection, search, copy/paste, and screen-reader sync are exact, not heuristic.

---

## 6. Interactivity — declarative state machine, no scripts

PDF forms run JavaScript; XFA was so bad ISO deprecated it. VSD replaces both with a bounded declarative layer:

```cddl
field-kind = "text" / "number" / "date" / "choice" /
             "checkbox" / "signature" / "attachment"

field = { t: "field", id: tstr, kind: field-kind,
          required: bool,
          constraint: expr / null,    ; validation
          computed:   expr / null }   ; derived value

; Total, terminating expression language. No loops, no I/O,
; no string-eval, no network, no clock beyond document date fields.
expr = literal / field-ref /
       ["+" / "-" / "*" / "/" / "min" / "max" / "round", *expr] /
       ["if", expr, expr, expr] /
       ["match", field-ref, *[pattern, expr]] /
       ["regex-valid", field-ref, anchored-re2-pattern]
```

Properties worth having: every expression provably terminates; evaluation is O(fields × expr-size); the whole form layer can be model-checked; regexes are RE2-class (no catastrophic backtracking). Filled values are stored as a separate object layer over the immutable base document — the blank form and each filled instance share all structural objects, and "flattening" is a defined merge, not a print-to-new-file.

---

## 7. Signing & redaction

### 7.1 Signatures

```cddl
signature = {
  scope:     "document" / "subtree" / "field-layer",
  target:    object-ref,            ; manifest hash or subtree root
  alg:       "ed25519" / "ecdsa-p256" / "ml-dsa-65",  ; PQ-ready
  cert:      bytes,                 ; X.509 chain
  timestamp: rfc3161-token / null,
  sig:       bytes,
}
```

Because the target is a Merkle root over *meaning*:

- Signatures survive container repacking, recompression, and chunk reordering — none of which PDF byte-range signatures tolerate.
- **Subtree signatures**: party A signs §3 (the schedule of fees), party B signs the whole document; both verifiable independently. PDF needs awkward incremental-update gymnastics for this.
- **Incremental amendment**: an addendum adds objects + a new manifest; the original signature still verifies against the original manifest, which the new manifest references as `predecessor`. You get an authenticated version chain.

### 7.2 Redaction — destructive by construction

The redaction operation is defined in the spec, not left to tools:

1. Target subtree is **replaced** by `{ t: "redacted", reason: tstr / null }`.
2. Objects no longer referenced by any manifest are **purged** from the store.
3. The render cache is **recomputed** (mandatory — stale caches are where PDF redaction leaks live).
4. A `redaction-proof` records `BLAKE3(removed-subtree)` so a court can later verify *what* was removed matches an escrowed original, without the file containing it.

Drawing a black box over text is not representable in this format. The "Manning/Snowden-era PDF redaction failure" class of bug is structurally impossible.

---

## 8. Provenance

Optional C2PA-compatible chain: ordered list of signed assertions (`created-by`, `derived-from: manifest-hash`, `ai-generated: {model, params-hash}`, `scanned-from-physical`, `format-migrated: {source: "pdf", tool, lossy: bool}`). Each assertion signs the manifest hash at that point in history, producing a verifiable custody chain — increasingly a legal requirement for AI-touched documents, and the kind of thing 2026 procurement checklists have started asking for.

---

## 9. Streaming & partial access

The `page-index` object maps `page-number → required object-id closure`. A ranged-HTTP client can:

1. Fetch header + trailer (one small range) → locate INDEX.
2. Fetch INDEX → locate page-index → fetch it.
3. Fetch exactly the objects for page 47 of a 900-page manual.

Object immutability makes caching trivial (`Cache-Control: immutable` by hash), and a CDN can serve object stores shared across an entire document corpus.

---

## 10. Conformance profiles

| Profile | Adds / restricts |
|---------|------------------|
| **VSD/Core** | Everything in §2–§5. Render cache optional. |
| **VSD/Archive** | Render cache mandatory; all fonts embedded; ICC explicit; no field layer; provenance mandatory. (The PDF/A analogue, minus PDF/A's 200 pages of exceptions.) |
| **VSD/Form** | Core + §6 field layer + §7 signatures. |
| **VSD/Stream** | Core + mandatory page-index + chunk-ordering constraints for single-pass HTTP. |

Conformance = passing the public test-vector suite against the reference implementation (validator + layout engine + rasterizer, permissively licensed). The spec budget target is under 150 pages including the layout engine — versus PDF 2.0's ~1,000.

---

## 11. PDF interop — the adoption wedge

A format without a migration story is a hobby. Normative converters ship with the reference implementation:

**PDF → VSD.** Tagged PDFs map structure directly. Untagged PDFs (the majority) go through structure recovery — ironically, 2026-era document-understanding models are good enough that the conversion *adds* the semantics PDF never had. Output is marked `provenance: format-migrated { lossy: true }` when recovery was heuristic; the original PDF may be embedded as an attachment object for legal continuity.

**VSD → PDF.** Trivially lossless for visuals: the display lists (§5) are a strict subset of PDF's imaging model, so export is mechanical and the result is a *better-tagged* PDF than most native ones. This matters: organizations can adopt VSD internally with zero external-compatibility risk, which is the only adoption posture that has ever worked.

---

## 12. Threat-model summary

| PDF attack class | VSD answer |
|---|---|
| Embedded JS / launch actions | No executable content exists (I2) |
| Parser ambiguity / polyglot files | One deterministic CBOR grammar; chunk checksums; magic catches corruption |
| Shadow text ≠ visible text | layout-hash recomputation (§5.3) |
| Failed redaction | Destructive redaction is the only redaction (§7.2) |
| Signature shadow attacks (incremental-update abuse) | Signatures over Merkle roots; amendments form an explicit chain (§7.1) |
| Font-parser RCEs | WOFF2 only, validated subsets, finite grammar |
| ReDoS in form validation | RE2-class patterns, total expression language (§6) |

---

## 13. Open problems (honest list)

1. **Layout determinism across decades** is the hardest engineering problem here — it's why the engine is versioned and the cache is authoritative-but-verifiable rather than regenerate-always.
2. **Long-document pagination cost**: incremental relayout for 10k-page documents needs a chunked layout protocol (sketch: per-section layout fences).
3. **Post-quantum signature size** (ML-DSA ≈ 3.3 KB/sig) is noticeable on tiny documents; hybrid-by-default is the current lean.
4. **Governance**: this dies if proprietary. Plausible path: incubate openly, standardize via ISO SC34 or W3C once two independent implementations exist.
5. **The real moat is political, not technical** — PDF/A is written into statute in many jurisdictions. The wedge is regulation that PDF satisfies poorly: accessibility law (EU EAA, in force since mid-2025), machine-readability mandates, and AI-provenance requirements.

---

*Draft 0.1 — a concept specification, not an implemented standard. Numbers (header sizes, codec choices) are defensible defaults, not settled law.*
