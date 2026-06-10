# VSD — Verifiable Structured Document

> Layout fidelity of PDF · parseability of HTML · integrity model of Git · attack surface of a JPEG

A document format in which **structure is canonical and pixels are cache**. The
typed content tree is the document; every object is content-addressed
(BLAKE3-256 over deterministic CBOR); the document's identity is a single
32-byte Merkle commitment to every byte of content; and signatures cover
*meaning*, not byte ranges — so they survive recompression, repacking,
and container reordering.

This repository is the reference implementation in Rust, tracking
[the draft specification](vsd-spec-draft.md).

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
| [`vsd-layout`](crates/vsd-layout) | The reference layout engine **vsd-layout/1.0**: a deterministic projection from content tree to display lists — integer-µm arithmetic, pinned Noto Sans, normative contract in [docs/LAYOUT-1.0.md](docs/LAYOUT-1.0.md) |
| [`vsd-render`](crates/vsd-render) | Rasterizer: display-list pages → PNG via tiny-skia, drawing with the same pinned font the engine measured with |
| [`vsd-cli`](crates/vsd-cli) | The `vsd` tool: `pack`, `info`, `validate`, `extract`, `objects`, `keygen`, `sign`, `verify [--recompute]`, `redact`, `diff`, `fill`, `flatten`, `layout`, `render` |

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

**Not yet implemented (the honest list, spec §13):**

- Layout engine widening (engine 1.1+): RTL/bidi, CJK, complex scripts,
  bold/italic faces, justification/hyphenation, incremental relayout.
  Engine 1.0 refuses what it cannot lay out rather than mis-rendering it.
- A viewer (`vsd-view`); the WASM viewer track.
- PDF interop converters (PDF→VSD structure recovery; VSD→PDF export — the
  display lists to export from now exist).
- X.509 chains, RFC 3161 timestamps, and post-quantum (`ml-dsa-65`) signatures
  — wire format reserves all three.
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
