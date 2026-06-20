# VSD — Verifiable Structured Document

> Layout fidelity of PDF · parseability of HTML · integrity model of Git · attack surface of a JPEG

A document format in which **structure is canonical and pixels are cache**. The
typed content tree is the document; every object is content-addressed
(BLAKE3-256 over deterministic CBOR); the document's identity is a single
32-byte Merkle commitment to every byte of content; and signatures cover
*meaning*, not byte ranges — so they survive recompression, repacking,
and container reordering.

This repository is the reference implementation in Rust. The normative,
implementation-synced specification lives in **[spec/SPEC.md](spec/SPEC.md)**
(CC-BY 4.0), with the layout contract in
[docs/LAYOUT-1.0.md](docs/LAYOUT-1.0.md) and the conformance program in
[spec/CONFORMANCE.md](spec/CONFORMANCE.md). Governance:
[GOVERNANCE.md](GOVERNANCE.md). Want to build a second implementation?
You should never need to read this source — see the conformance
program; where you do, that's a spec bug we want filed.

## Why

PDF has survived 30 years because it nails pixel-faithful, self-contained,
offline, archivable layout. It is also a bag of draw commands with semantics
bolted on, a scripting host, and a parser-ambiguity playground. VSD keeps the
four properties that matter and removes the failure modes by construction:

| PDF attack / failure class | VSD answer |
|---|---|
| Embedded JavaScript, launch actions | No executable content exists in the format |
| Parser ambiguity, polyglot files | One deterministic CBOR grammar; strict decoding; chunk checksums |
| Visible pixels ≠ extracted text | Render cache is a verifiable projection of the content tree |
| Failed redaction (black box over live text) | Redaction replaces the subtree, purges objects, and proves what was removed — a black box is unrepresentable |
| Signature shadow attacks | Signatures over Merkle roots; amendments form an explicit predecessor chain |
| ReDoS in form validation | Total expression language, RE2-class regexes only |
| Table extraction as a research field | Tables are tables: topology, header scope, spans are structural facts |
| Accessibility as an afterthought | Missing alt text is a *validation error*; reading order is tree order |

## Crates

| Crate | Contents |
|---|---|
| [`vsd-core`](crates/vsd-core) | Deterministic CBOR (RFC 8949 §4.2, strict both ways) · content-addressed object store · content tree · manifest & profiles · validation · destructive redaction · forms + fill/flatten · object-set diff · render-layer types. `no_std + alloc` capable. |
| [`vsd-container`](crates/vsd-container) | The `.vsd` chunk container: 32-byte header, BLAKE3-checksummed chunks, object index, signature blocks, trailer; zstd optional; lazy `StreamReader` for ranged access |
| [`vsd-sign`](crates/vsd-sign) | Ed25519 signatures over document/subtree Merkle roots, with domain separation (wire format reserves `ecdsa-p256`, `ml-dsa-65`) |
| [`vsd-layout`](crates/vsd-layout) | The reference layout engine (versions **1.0–1.4**, each an immutable contract): a deterministic projection from content tree to display lists — integer-µm arithmetic, eight pinned Noto faces, pinned hyphenation patterns + pinned shaper (rustybuzz), normative contracts in [docs/LAYOUT-1.0.md](docs/LAYOUT-1.0.md)…[1.4.md](docs/LAYOUT-1.4.md) |
| [`vsd-render`](crates/vsd-render) | Rasterizer: display-list pages → PNG via tiny-skia, drawing with the same pinned font the engine measured with |
| [`vsd-pdf`](crates/vsd-pdf) | PDF interop: deterministic **tagged** PDF export with the canonical `.vsd` embedded (hybrid PDF — round trips losslessly, verifiable by document id); import with hybrid recovery + pluggable structure recovery for foreign PDFs |
| [`vsd-tlog`](crates/vsd-tlog) | Transparency log: RFC 6962-style Merkle tree over document ids — inclusion + consistency proofs, signed tree heads ("this contract existed, in exactly this form, at this time") |
| [`vsd-view`](crates/vsd-view) | Native viewer: page nav, zoom, exact search with highlights, copy — and the **verification banner** (validate + signatures + layout recomputation) as the first thing on screen |
| [`vsd-web`](crates/vsd-web) | The browser viewer: the full verify+layout+render stack as WASM behind a tiny C ABI, consumed by a dependency-free `<vsd-doc>` web component — no plugin, no install |
| [`vsd-cli`](crates/vsd-cli) | The `vsd` tool: `pack` (JSON/Markdown), `info`, `validate`, `extract`, `objects`, `keygen`, `sign`, `verify [--recompute]`, `redact`, `diff [--html]`, `fill [--interactive]`, `flatten`, `layout`, `render`, `export`, `import`, `migrate` |

## Quick start

```console
$ cargo install --path crates/vsd-cli

# Author a document in JSON, pack it into a .vsd
$ vsd pack examples/agreement.json -o agreement.vsd --profile form
wrote agreement.vsd (1174 bytes, 3 objects)
document id: 34ca96f9e12e92bc…

# Sign and verify
$ vsd keygen -o me.key
$ vsd sign agreement.vsd --key me.key -o agreement-signed.vsd
$ vsd verify agreement-signed.vsd
container   : OK (all chunk checksums and object hashes verified)
validation  : OK
signature 0 : [document] key 9a199e3c51aa5c24… → VALID
VERIFIED

# Destructively redact the paragraph at tree path 2.2
$ vsd redact agreement-signed.vsd --path 2.2 --reason "bank details" -o redacted.vsd
removed-subtree proof: f1df56f6…
purged 1 unreferenced object(s) from the store

# The content is gone from every byte of the file, the document is
# still valid, and it records its predecessor:
$ vsd diff agreement.vsd redacted.vsd
new declares old as its predecessor (amendment chain)
objects: 2 shared, 1 added (+1138 B), 1 removed (-1152 B)
changed: 2.2: paragraph → redacted
```

Text extraction is a tree walk, not OCR-adjacent heuristics:

```console
$ vsd extract agreement.vsd          # exact reading-order plain text
$ vsd extract agreement.vsd --format json   # full content tree
```

Lay out, verify that pixels match meaning, and rasterize:

```console
$ vsd layout agreement.vsd -o agreement-laid.vsd
laid out 1 page(s) with vsd-layout/1.0.0
layout-hash    : 675a4fea43443a15…

# Re-runs the engine and requires byte-identical page objects — the
# check PDF structurally cannot offer. A render cache that lies about
# the content tree dies here:
$ vsd verify agreement-laid.vsd --recompute
recompute   : OK — 1 page(s) re-laid out, byte-identical to the cache;
              pixels and meaning agree
VERIFIED

$ vsd render agreement-laid.vsd --page 1 -o page1.png --dpi 144
```

PDF is the adoption wedge — and with the hybrid trick, just a transport:

```console
# Markdown is a first-class on-ramp:
$ vsd pack README.md -o readme.vsd

# Tagged PDF out; the canonical .vsd rides inside as an attachment:
$ vsd export readme.vsd -o readme.pdf
exported readme.vsd → readme.pdf (tagged PDF, canonical .vsd embedded — round trip is lossless)

# Anyone with the PDF gets the original back, identity verified:
$ vsd import readme.pdf -o readme-back.vsd
hybrid PDF: recovered the canonical VSD losslessly
document id: 2e5f618fba9d8e8b… (verified)

# Foreign PDFs go through structure recovery instead — marked lossy in
# provenance, with the original embedded for legal continuity.

# Batch migration with the content-addressing payoff made visible:
$ vsd migrate ./docs -o ./vsd-docs
migrated 4 document(s), 0 failure(s)
objects: 12 total across documents, 8 unique (33.3% shared)
```

And read them anywhere — natively or in any browser:

```console
$ vsd-view agreement-laid.vsd        # native viewer; verification banner first
$ vsd diff old.vsd new.vsd --html redline.html   # reviewable redline, no JS
```

![vsd-view: the verification banner is the first thing on screen](docs/vsd-view.png)

The same stack compiles to WASM ([crates/vsd-web](crates/vsd-web)): a
`<vsd-doc src="file.vsd">` web component verifies, lays out, and renders
documents entirely client-side, badge included.

## Library use

```rust
use vsd_core::document::DocumentBuilder;
use vsd_core::tree::{Doc, Direction, Heading, Inline, Node, Para};
use vsd_container::{write_document, WriteOptions};

let root = Node::Doc(Doc {
    lang: "en".into(),
    dir: Direction::Ltr,
    children: vec![
        Node::Heading(Heading { level: 1, children: vec![Inline::Text("Hello".into())] }),
        Node::Para(Para { children: vec![Inline::Text("World.".into())] }),
    ],
});
let doc = DocumentBuilder::new(root).build()?;
let id = doc.document_id()?;          // 32-byte identity over all content
let bytes = write_document(&doc, &[], &WriteOptions::default())?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Format in one page

```
.vsd file = HEADER (32 B) · MNFST · INDEX · OBJS… · SIGS? · TRAILR
```

- Every chunk: `u64 length · u32 fourcc · u32 flags · payload · u64 BLAKE3-64 checksum`
- Every object: deterministic CBOR; `object_id = BLAKE3-256(bytes)`; immutable
- The manifest references the content-tree root, resources, metadata, render
  cache, and provenance by object id. **Document identity = BLAKE3(manifest).**
- Editing produces new objects and a new manifest; unchanged objects are
  shared, so revision diffs are object-set diffs, exactly like Git trees.
- The decoder is strict: non-minimal integers, unsorted map keys, indefinite
  lengths, overlong floats, tags, trailing bytes, non-canonical object bytes,
  hash mismatches, and checksum failures are all hard errors.

## Conformance profiles

| Profile | Constraint set |
|---|---|
| `core` | Baseline; render cache optional |
| `archive` | Render cache + provenance mandatory; field layer forbidden (PDF/A analogue) |
| `form` | Core + declarative field layer + signatures |
| `stream` | Core + mandatory page index for ranged-HTTP access |

## Status and roadmap

See **[ROADMAP.md](ROADMAP.md)** for the full design rationale, phased
implementation plan, release train, and the long-horizon feature list.
Summary:

**Implemented (v0.1, the canonical layer):** deterministic encoding, content
addressing, container I/O with full integrity verification, validation with
accessibility as a validity condition, Ed25519 signing with subtree scopes
and amendment chains, spec-defined destructive redaction with proofs, the
total forms expression language, object-set diffs, text extraction, and the
CLI.

**Implemented (v0.3, Phase 1 — hardening & ecosystem hygiene):**

- **Streaming/ranged reads** (§9): `StreamReader` over any `RangeSource`
  (file, HTTP range transport) — open verifies header/trailer/manifest,
  objects load lazily with per-object BLAKE3 verification; `vsd` fetches
  exactly the objects page *n* needs via the page-index object.
- **Filled forms**: `vsd fill --set qty=4` layers values over an immutable
  blank form (shared objects, predecessor chain); `vsd flatten` performs the
  spec-defined merge, gated on all constraints passing; computed fields
  evaluate in dependency order with cycles rejected at validation.
- **`no_std + alloc` vsd-core**, build-verified for bare-metal ARM and wasm32
  — embedded verifiers (passport readers, access hardware) can check
  structure and signatures offline.
- **Public conformance vectors** ([testdata/](testdata/)): valid + invalid
  files with a machine-readable expectation manifest, deterministically
  regenerable — a second implementation can test itself without our source.
- **Fuzz harness** ([fuzz/](fuzz/)): four targets asserting bijection/
  canonical-fixpoint properties, not just absence of panics.
- **Property-based tests**: canonical-fixpoint, roundtrip, and
  redaction-never-leaks over generated documents.
- **CI matrix**: 3 OSes, MSRV, no_std/wasm targets, fmt/clippy/docs gates,
  vector reproducibility, fuzz smoke, semver-checks.
- **Threat model** ([docs/THREAT_MODEL.md](docs/THREAT_MODEL.md)) and
  [security policy](SECURITY.md).

**Implemented (v0.5, Phase 2 — the render layer):**

- **`vsd-layout/1.0`**, the reference layout engine: a deterministic
  projection from content tree to display lists. All layout arithmetic is
  integer micrometers (no float ordering to pin); the font (Noto Sans
  Regular v2.015) and metrics parser are pinned by hash and exact version;
  line breaking is an enumerated UAX #14 subset. Normative contract:
  [docs/LAYOUT-1.0.md](docs/LAYOUT-1.0.md). Coverage: single-column LTR with
  paragraphs, headings, lists, code, ruled tables, figures, filled fields,
  and redaction bars; greedy pagination with keep-with-next.
- **`vsd verify --recompute`** — the spec §5.3 promise made real: re-runs
  the engine and requires byte-identical page objects. A grafted cache from
  different content passes every structural check and is caught *only* by
  recomputation (there's a test proving exactly that).
- **`vsd-render` + `vsd render`** — display lists to PNG via tiny-skia,
  drawing glyph outlines from the same pinned font the engine measured with.
- **Cross-platform determinism, CI-enforced**: the conformance corpus now
  carries a laid-out vector with a golden layout hash, generated on Windows
  and reproduced on Linux/macOS on every push.

**Implemented (v0.7, Phase 3 — PDF interop, the adoption wedge):**

- **Tagged PDF export** (`vsd export`): a from-scratch deterministic PDF
  writer — real structure tree rebuilt from display-list back-references
  (H1–H6/P/Code/Caption, figure alt text), embedded CID font with
  ToUnicode, PNG/JPEG images, document id in PDF metadata. Visually
  lossless by construction: display lists are a strict subset of PDF's
  imaging model.
- **Hybrid PDFs**: the canonical `.vsd` (signatures included) travels
  inside the exported PDF as an attachment, so `vsd import` recovers the
  exact original — same document id, signatures still verify. PDF becomes
  a transport, not a destination; the round trip is the identity function.
- **Foreign-PDF import** via a pluggable `StructureRecovery` trait (naive
  text recovery built in), always marked `format-migrated { lossy: true }`
  in provenance with the original PDF embedded for legal continuity.
- **Markdown on-ramp**: `vsd pack README.md` (CommonMark + tables); alt
  text on images enforced, HTML passthrough deliberately dropped.
- **`vsd migrate`**: batch directory conversion with the dedup report.

**Implemented (v0.8, Phase 4 — viewing & authoring):**

- **`vsd-view`** — native viewer with the verification banner as first-class
  UI (validation + signatures + recomputation on open), exact search with
  metric-true highlights, zoom, copy. Visually verified
  ([screenshot](docs/vsd-view.png)); the view-model is GUI-free and tested.
- **`vsd-web` + `<vsd-doc>`** — the whole verify/layout/render stack in the
  browser via WASM and ~150 lines of dependency-free JS; pure-Rust zstd
  decode (`zstd-pure`) so compressed containers open client-side.
- **`Compose`** — fluent Rust authoring (`.h1().para().table()…`), the
  high-level API invoice generators actually want.
- **`vsd diff --html`** — self-contained redline review pages (no JS).
- **`vsd fill --interactive`** — terminal form filling with live constraint
  evaluation and computed-field display.

**Implemented (v0.9, Phase 5 — trust infrastructure at scale):**

- **Hybrid post-quantum signatures** (`vsd keygen --algorithm hybrid`):
  Ed25519 + ML-DSA-65 (FIPS 204) over the same message in one signature —
  both must verify. Documents signed today still verify in 2050.
- **Transparency log** (`vsd tlog append | head | prove`): RFC 6962-style
  Merkle log of document ids with inclusion/consistency proofs and signed
  tree heads. Rewriting history is mechanically detectable.
- **Selective disclosure** (`vsd seal` / `disclose` / `verify-disclosure`):
  reveal one block to an auditor, prove it belongs to the signed document
  id, siblings travel as hashes only. No novel crypto — the tree already
  is a Merkle structure.
- **X.509 certificate binding** (`vsd sign --cert`): the attached cert must
  certify the signing key (SPKI check) and its validity window is enforced
  against verifier-supplied time.
- **Provenance authoring** (`vsd provenance add/show`), including
  `ai-generated {model, params-hash}` assertions for AI-transparency
  requirements.
- **Object-store HTTP conventions** ([docs/OBJECT-STORE-HTTP.md](docs/OBJECT-STORE-HTTP.md)):
  content-addressed serving any static host can implement; clients verify,
  CDNs can deny service but never substitute content.

**Implemented (v0.10, Phase 6 — standardization prep + completed partials):**

- **The spec, consolidated** ([spec/SPEC.md](spec/SPEC.md), CC-BY 4.0,
  format version 0.2): the format as actually built, in ~30 pages against
  the 150-page budget; conformance program, governance, contributing
  guide, Apache-2.0 LICENSE, and [regulatory wedge
  dossiers](docs/REGULATORY.md) (EAA, e-invoicing, AI provenance, court
  redaction, archival).
- **Salted selective disclosure** (`vsd seal --salted`, format 0.2): hidden
  siblings can no longer be confirmed by hashing a guess — tested by
  running the confirmation attack against both modes.
- **In-browser signature verification**: `vsd-sign` verification is now
  RNG-free (Ed25519 *and* hybrid PQ), so the `<vsd-doc>` badge reports
  signatures verified client-side.
- **Reference object-store server** (`vsd serve`): the
  [OBJECT-STORE-HTTP](docs/OBJECT-STORE-HTTP.md) conventions, served;
  untrusted by design — clients verify every object by hash.

**Implemented (v0.11 — engine 1.1 + incremental relayout):**

- **Engine 1.1: real bold/italic/bold-italic faces** (three more pinned
  Noto Sans binaries, contract in [docs/LAYOUT-1.1.md](docs/LAYOUT-1.1.md)).
  Verification dispatches on the version a cache pins — 1.0 caches
  recompute byte-identically forever, with golden conformance vectors for
  both versions. PDF export embeds every used face.
- **Incremental relayout** (`LayoutSession`): a one-paragraph edit
  re-shapes exactly one fragment, and the output is byte-identical to a
  from-scratch layout — the cache is an optimization, never an oracle.

**Implemented (v0.12 — engine 1.2 typography):**

- **Monospace, underline, justification, Hebrew bidi/RTL** (contract in
  [docs/LAYOUT-1.2.md](docs/LAYOUT-1.2.md)): code and `mono` spans in a
  pinned Noto Sans Mono; underline hairlines; body paragraphs fully
  justified by integer-µm slack distribution; UAX #9 ordering with a
  pinned `unicode-bidi` and a pinned Hebrew face. RTL runs keep their
  text in **logical order** (a format-0.3 `rtl` flag tells consumers to
  draw right-to-left), so back-references, search, selection, and
  disclosure are untouched by visual reordering. The 1.0/1.1 golden
  layout hashes survived the widening unchanged.

**Implemented (v0.13 — engine 1.3 page furniture):**

- **Hyphenation + widow/orphan control** (contract in
  [docs/LAYOUT-1.3.md](docs/LAYOUT-1.3.md)): English body text is
  hyphenated with the pinned Knuth–Liang en-US patterns (pure
  integer/string work, language-gated; the inserted hyphen is
  decoration with an empty `char_range`), and pagination keeps ≥2 lines
  of a paragraph on each side of a page break. No display-list format
  change; the 1.0/1.1/1.2 golden layout hashes are byte-for-byte
  unchanged.

**Implemented (v0.14 — engine 1.4 shaped scripts):**

- **Arabic + Devanagari shaping** (contract in
  [docs/LAYOUT-1.4.md](docs/LAYOUT-1.4.md)): real HarfBuzz-class shaping
  via the pinned pure-Rust `rustybuzz` over two pinned fonts. Arabic
  joins right-to-left; Devanagari reorders matras and forms conjuncts.
  Shaping runs once in the engine and is emitted as the new format-0.4
  `glyphs` display op — positioned glyphs in visual order, with the
  **logical** text/cluster retained so selection, search, extraction,
  and disclosure still operate on source text. Deterministic (pinned
  shaper, integer font units); the 1.0–1.3 golden hashes are unchanged.

**Not yet implemented (the honest list):**

- Layout engine widening (engine 1.5+): CJK + vertical text (needs CJK
  fonts and breaking rules), Thai/Lao dictionary line breaking, other
  Indic/complex scripts, multi-column, MathML layout. The engine refuses
  what it cannot lay out rather than mis-rendering it.
- Python/TypeScript authoring bindings; Pandoc/Typst backends;
  viewer-integrated form filling; a browser text-selection layer.
- PDF/A-2b export mode; foreign tagged-PDF structure-tree import; richer
  recovery strategies (columns/tables); JPEG→JXL recompression (blocked on
  a pure-Rust JXL encoder).
- X.509 chain-path validation, revocation, RFC 3161 timestamps; C2PA
  JUMBF serialization.
- External-by-nature: a second independent implementation, the standards
  track, the security audit — see [GOVERNANCE.md](GOVERNANCE.md).
- crates.io publication (release-ready; awaits the repository going public).

## Security posture

- No `unsafe` in any VSD crate (`#![forbid(unsafe_code)]`, compiler-enforced).
- No executable content in the format; forms are a total, terminating
  expression language with RE2-class regexes (linear-time matching).
- Hostile-input bounds: nesting depth caps, chunk size sanity caps,
  decompression-bomb guard, expression size/depth caps.
- Strict parsing everywhere — unknown keys, unknown node types, and
  non-canonical encodings are rejected, eliminating polyglot ambiguity.
- Every parser fuzzed with property assertions; full written threat model in
  [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md); report vulnerabilities per
  [SECURITY.md](SECURITY.md).

## License

Apache-2.0
