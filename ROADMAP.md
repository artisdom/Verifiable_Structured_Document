# VSD Roadmap

> **Mission:** replace PDF as the default format for final-form documents — by
> keeping the four properties that made PDF unkillable (pixel-faithful,
> self-contained, offline, archivable) and removing its failure modes *by
> construction* rather than by convention.
>
> Layout fidelity of PDF · parseability of HTML · integrity model of Git ·
> attack surface of a JPEG.

This file is the single source of truth for where the project is and where it
is going. The [draft specification](vsd-spec-draft.md) says what the format
*is*; this file says what we *build, in what order, and why*.

---

## 1. Design recap — the five invariants

Everything in the spec and this codebase derives from five non-negotiables
(spec §1). Any proposed feature that violates one is rejected, no matter how
attractive:

| # | Invariant | Practical consequence |
|---|-----------|----------------------|
| **I1** | Structure is canonical, pixels are cache | The typed content tree *is* the document. Layout is a deterministic, verifiable projection — never the other way around. |
| **I2** | Zero executable content in core | No scripting engine, ever. Interactivity is a total, terminating declarative layer. "Just add a small JS hook" is the death of the format. |
| **I3** | Every object is content-addressed | `id = BLAKE3-256(canonical bytes)`. Dedup, diff, partial fetch, and integrity verification are emergent properties, not features. |
| **I4** | One reference renderer, conformance-tested | "Renders identically everywhere" is enforced by public test vectors against this repository, not hoped for. |
| **I5** | Cryptography over meaning, not bytes | Signatures cover the Merkle root of content. Recompression, repacking, and chunk reordering can never invalidate a signature. |

**Architecture that follows:** a strict deterministic-CBOR grammar at the
bottom; a content-addressed object store above it; a semantic tree as the
canonical layer; manifest hash as document identity; layout/render strictly
derived; container as dumb transport. Each layer is independently verifiable.

---

## 2. Status at a glance

```
Phase 0  Foundations (canonical layer)        ████████████████████  SHIPPED (v0.1)
Phase 1  Hardening & ecosystem hygiene        ███████████████████░  SHIPPED (v0.3) — crates.io publish awaits public repo
Phase 2  The render layer (vsd-layout)        ████████████████░░░░  SHIPPED (v0.5) — minimal profile; widening (2f) + incremental (2g) open
Phase 3  PDF interop (the adoption wedge)     ██████████████░░░░░░  SHIPPED (v0.7) — export + hybrid round-trip + md on-ramp; rich import (3b/3c) + JXL (3e) open
Phase 4  Viewing & authoring experience       ████████████░░░░░░░░  SHIPPED (v0.8) — vsd-view, <vsd-doc> WASM viewer, compose API, diff --html, interactive fill; bindings (4c) + Pandoc (4d) open
Phase 5  Trust infrastructure at scale        █████████████░░░░░░░  SHIPPED (v0.9) — hybrid PQ, tlog, selective disclosure, cert binding; full PKI (5a) + C2PA serialization (5c) + salting (5f) open
Phase 6  Standardization & governance         ░░░░░░░░░░░░░░░░░░░░  next major effort
Moonshots                                     see §10
```

---

## 3. Phase 0 — Foundations ✅ *shipped as v0.1*

The complete canonical layer, working end to end. What exists today:

### vsd-core
- [x] **Deterministic CBOR** (RFC 8949 §4.2): hand-written encoder *and*
      strict decoder. Non-minimal integers, unsorted/duplicate map keys,
      indefinite lengths, overlong floats, non-canonical NaN, tags, trailing
      bytes — all hard errors. One grammar, no polyglots.
- [x] **Content-addressed object store** — BLAKE3-256 ids, immutability,
      canonical-form verification on every load, orphan purging.
- [x] **Content tree** (spec §3) — doc/section/heading/para/table/figure/
      list/code/math/field/redacted/subtree-ref. Tables carry real topology
      (spans, header scope). Alt text is mandatory. Reading order is tree
      order. Merkle structure via subtree refs.
- [x] **Manifest & identity** (§2.4) — document id = BLAKE3(manifest),
      a 32-byte commitment to every byte of content. Amendment chains via
      `predecessor`.
- [x] **Validation** (§3, §10) — accessibility as a *validity condition*,
      table shape checks, reference resolution, profile conformance
      (core/archive/form/stream), orphan detection.
- [x] **Destructive redaction** (§7.2) — replace subtree → purge orphans →
      invalidate render cache → record BLAKE3 proof of what was removed.
      A black box over live text is unrepresentable.
- [x] **Forms** (§6) — total, terminating expression language: arithmetic,
      comparisons, if/match, regex-valid. RE2-class only (linear-time,
      ReDoS-immune by construction), depth/size capped, model-checkable.
- [x] **Diff** — version diffs as object-set arithmetic plus structural
      divergence paths.
- [x] **Text extraction** — exact, reading-order, a tree walk.
- [x] **Render-layer types** (§5) — display-list format (flat positioned ops,
      back-references `(node_path, char_range)` into the tree), render-cache
      object with `layout-hash`, the versioned `LayoutEngine` trait.

### vsd-container
- [x] `.vsd` chunk container (§2): 32-byte header with PNG-style magic,
      64-bit lengths, BLAKE3-64 chunk checksums, MNFST/INDEX/OBJS/SIGS/TRLR,
      truncation detection, duplicate-chunk rejection, unknown-critical-chunk
      rejection.
- [x] zstd chunk compression (feature-gated) with decompression-bomb guard.
- [x] Deterministic file output: same document + same options → identical bytes.
- [x] Strict reader: every chunk checksum *and* every object hash verified;
      identity survives recompression (proven by test).

### vsd-sign
- [x] Ed25519 over Merkle targets with domain separation (`VSD-SIG-v1`).
- [x] Scopes: whole-document, subtree, field-layer. Wire format reserves
      `ecdsa-p256`, `ml-dsa-65`, X.509 `cert`, RFC 3161 `timestamp`.
- [x] Verdict model distinguishes *invalid* from *valid-for-predecessor*
      (kills signature shadow attacks).

### vsd-cli
- [x] `pack` (JSON authoring dialect) · `info` · `validate` · `extract`
      (text/JSON) · `objects` · `keygen` · `sign` · `verify` · `redact` ·
      `diff`.

### Quality
- [x] 33 tests including end-to-end conformance-style vectors (determinism,
      tamper rejection, redaction byte-level destruction, signature
      lifecycle, profile enforcement).
- [x] `#![forbid(unsafe_code)]` across all crates; clippy-clean.

---

## 4. Phase 1 — Hardening & ecosystem hygiene ✅ *shipped as v0.3*

Make what exists trustworthy enough that other people can bet on it.

- [x] **Fuzzing**: `cargo-fuzz` targets (`fuzz/`) for the CBOR decoder,
      container parser (eager + streaming), tree decoder, and expression
      parser — each asserting its bijection/fixpoint property, not just
      "no panic". Seeded from the conformance vectors; CI smoke-runs every
      target per push. (OSS-Fuzz application once the repo is public.)
- [x] **Public conformance test vectors**: `testdata/` corpus of 5 valid +
      6 invalid `.vsd` files with a machine-readable expectation manifest
      (`vectors.json`), regenerated deterministically by
      `cargo run -p xtask -- gen-vectors` (fixed Ed25519 test seed; RFC 8032
      signing is deterministic). Includes adversarial vectors with
      *checksum-fixed-up* tampering that only the identity cross-checks can
      catch. Runner: `crates/vsd-cli/tests/conformance.rs`. The seed of I4.
- [x] **CI matrix** (`.github/workflows/ci.yml`): Linux/macOS/Windows tests,
      MSRV (1.85), fmt + clippy (`-D warnings`), rustdoc with broken-link
      errors, vector-reproducibility gate, `wasm32-unknown-unknown` *and*
      bare-metal `thumbv7em-none-eabihf` builds of vsd-core, fuzz smoke job,
      `cargo-semver-checks` (advisory until first publish).
- [x] **API documentation pass** + `docs.rs` all-features metadata +
      semver-checks in CI.
- [x] **`no_std + alloc` support for vsd-core** — verified against a
      Cortex-M target. Float plumbing rewritten as bit-exact integer ops
      (f16↔f64 conversion, round-half-even) — better for cross-platform
      determinism even on std. Caveat: regex-valid expressions parse but
      don't evaluate without `std` (embedded *verifiers* check structure
      and signatures, not forms).
- [x] **Property-based tests** (`proptest`): CBOR roundtrip, map-order
      independence, decode-never-panics, **accepted-bytes-are-canonical-
      fixpoint** (the no-polyglot property), float shortest-form roundtrip,
      container roundtrip on generated documents, and redaction-never-leaks
      (byte-scan of container output *and* every store object).
- [x] **Streaming/ranged reader** (§9): `RangeSource` trait (any
      `Read + Seek` works out of the box; HTTP transports implement the
      trait), lazy `StreamReader` with per-object hash verification on
      fetch, `PageIndex` object type, `page_closure()` for
      fetch-exactly-page-47 access. A lying range server is caught at the
      first touched object.
- [x] **Filled-form value layer**: `field-layer` manifest slot,
      `FilledLayer` object, `fill()` (overlay semantics, kind checking,
      computed-field evaluation in topological order with cycle rejection
      `E_FIELD_CYCLE`), `flatten()` as the defined merge (§6) gated on all
      constraints passing, CLI `vsd fill --set id=value` / `vsd flatten`.
      Redaction drops the layer (filled values may quote redacted content).
- [x] **Threat-model doc** ([docs/THREAT_MODEL.md](docs/THREAT_MODEL.md)):
      assets, adversaries, per-surface mitigations mapped to tests and
      vectors, honest residual-risk list. Plus [SECURITY.md](SECURITY.md)
      reporting policy. External audit remains a pre-1.0 gate (§11).
- [ ] **crates.io release** — code and metadata are release-ready;
      publishing is blocked only on the repository going public and an
      owner account. First publish flips semver-checks from advisory to
      hard gate.

**Exit criteria — met:** a hostile-input bug bounty would be boring (every
parser fuzzed with property assertions, every bound tested); a second
implementer needs only the spec + `testdata/`.

---

## 5. Phase 2 — The render layer: `vsd-layout` ✅ *minimal profile shipped as v0.5*

Spec §5 and §13.1: **deterministic layout is the format's hardest problem and
its deepest moat.** As of v0.5, "what you see ≠ what the text says" is a
*mechanically detectable* condition (`vsd verify --recompute`) — the one
thing PDF can never offer.

Strategy held: don't boil the ocean. Determinism shipped for a constrained
profile first; coverage widens version by version. The engine is versioned
(`vsd-layout/1.0`); documents pin the version; old caches stay verifiable
forever against their pinned engine.

- [x] **2a. Determinism contract document**
      ([docs/LAYOUT-1.0.md](docs/LAYOUT-1.0.md)) — and one strategy upgrade
      over the original plan: instead of "defined f64 operation order", all
      layout arithmetic is **integer micrometers** with a single rounding
      primitive (`muldiv`, 128-bit intermediate). No float order to define;
      platform variance is impossible by construction. Floats appear only at
      display-list emission as single exact conversions. Line breaking is an
      exhaustively enumerated UAX #14 subset (two break classes); shaping is
      pinned to the embedded font's cmap/hmtx (no ligatures/kerning in 1.0);
      no hyphenation. Pinned font: **Noto Sans Regular v2.015** (OFL,
      SHA-256 in the contract); pinned metrics parser (`ttf-parser`, exact
      version). HarfBuzz-class shaping is deferred to the engine version
      that introduces complex scripts (2f).
- [x] **2b. `vsd-layout/1.0` minimal profile**: single-column LTR block
      layout — paragraphs, headings (keep-with-next), lists, code (verbatim
      + tab stops), tables (weighted columns, ruled grid, header background,
      row-atomic pagination), figures (PNG intrinsic sizing + captions),
      fields (rendered with filled/computed values or blanks), redaction
      bars, page-break hints; greedy pagination with line-level splitting.
      Latin/Greek/Cyrillic via the embedded font.
- [x] **2c. Layout-hash verification end to end**: `vsd layout` attaches a
      cache + page index (successor document, predecessor chained);
      `vsd verify --recompute` re-runs the engine and requires byte-identical
      page objects. The killer demo is a test: a document carrying a
      *grafted* cache from different content passes every structural check —
      genuine hashes, consistent layout-hash — and only recomputation
      exposes the lie (`lying_render_cache_is_caught_only_by_recompute`).
- [x] **2d. Rasterizer** (`vsd-render`): display lists → PNG via tiny-skia;
      glyph outlines from the same pinned font the engine measured with;
      PNG resources composited; `vsd render --page N --dpi N`. The display
      list remains the normative artifact; pixels are an informative view.
- [x] **2e. Cross-platform determinism CI**: the conformance corpus gained
      `valid/laid-out.vsd` with a **golden layout hash** — generated on
      Windows, regenerated and diffed on Linux in CI, recomputed by the
      test suite on all three OSes. Byte-identical page objects, proven on
      every push.
- [ ] **2f. Widening, versioned**: RTL + bidi (1.1), CJK + vertical text
      (1.2), complex scripts/Indic via a pinned pure-Rust shaper (1.3),
      floats & multi-column (1.4), math layout from MathML Core (1.5),
      justification + hyphenation (1.6), bold/italic faces. Engine 1.0
      refuses what it cannot lay out (`dir=rtl` errors rather than
      mis-rendering).
- [ ] **2g. Incremental relayout** (§13.2): per-section layout fences so a
      one-paragraph edit in a 10k-page manual doesn't re-paginate the world.

**Exit criteria — met for the minimal profile:** two machines, two OSes, one
document → bit-identical render cache (CI-enforced via the golden vector);
the conformance suite carries layout vectors. Pixel-exact golden *images*
remain deliberately out of scope: the raster is informative, the display
list is normative.

---

## 6. Phase 3 — PDF interop: the adoption wedge ✅ *core shipped as v0.7*

Spec §11: *a format without a migration story is a hobby.* Organizations must
be able to adopt VSD internally with **zero external-compatibility risk**.

- [x] **3a. VSD → PDF export** (`vsd-pdf`): mechanical and visually lossless
      by construction — display lists are a strict subset of PDF's imaging
      model. Ships as a from-scratch deterministic writer (no PDF-library
      dependency in the trusted output path; same document → same bytes).
      Emits **tagged** PDF: the structure tree is rebuilt from display-list
      `node_path` back-references (H1–H6/P/Code/Caption/Formula/Lbl, figure
      `/Alt` from mandatory alt text), decoration marked as artifacts —
      more accessible than most native PDFs. Embedded CIDFontType2 (the
      pinned Noto Sans) with ToUnicode so extraction and copy/paste work;
      PNG (recompressed Flate) and JPEG (DCT passthrough) images; document
      identity recorded in PDF metadata (`vsd-doc-id:`).
      **Plus the hybrid trick**: by default the canonical `.vsd` travels
      inside the PDF as an attachment, making PDF a *transport* for VSD —
      the round trip back is the identity function, signatures included,
      verifiable by document id (tested). ☐ PDF/A-2b output mode (XMP +
      OutputIntent ICC) remains open.
- [x] **3c. Pluggable structure recovery** for foreign PDFs: the
      `StructureRecovery` trait keeps recovery strategies out of the
      trusted core; the built-in `TextRecovery` is deliberately naive
      (page text → paragraphs, page-break hints). Output is always marked
      `format-migrated { lossy: true }` in provenance with the original
      PDF embedded as an attachment for legal continuity and its hash
      recorded. ☐ Richer built-ins (column/table reconstruction,
      document-understanding models) slot into the trait later.
- [◐] **3b. Tagged-PDF → VSD importer**: *hybrid* PDFs (ours) import
      losslessly via the embedded source — identity verified, signatures
      intact. ☐ Walking a foreign PDF's structure tree (StructTreeRoot →
      content tree) is still open; foreign tagged PDFs currently take the
      3c recovery path.
- [x] **3d. Markdown → VSD** (`vsd pack notes.md`): CommonMark + tables via
      pulldown-cmark — headings, lists, code, tables, quotes, links,
      emphasis (style table), images (alt text required, enforced). HTML
      passthrough is deliberately dropped (no foreign content). ☐ A direct
      HTML importer remains open.
- [ ] **3e. JPEG → JXL lossless recompression** + WOFF2 subsetting with the
      normative coverage check. Blocked on a production-grade pure-Rust JXL
      *encoder* (jxl-oxide is decode-only); revisit when one exists. JPEG
      already passes through to PDF losslessly via DCTDecode.
- [x] **3f. `vsd migrate`**: batch-converts a directory tree of
      .json/.md/.pdf to .vsd and prints the dedup report — total vs unique
      objects and the bytes a shared object store would save (the
      CFO-legible feature, e.g. "12 objects total, 8 unique, 21.3% saved"
      on the demo corpus).

**Exit criteria — met for the hybrid path:** round-trip VSD→PDF→VSD is the
identity function (same document id, signatures verify; conformance-tested).
Export is structurally verified against a real PDF parser (lopdf: page tree,
StructTreeRoot, fonts); rendering in Acrobat/viewers is visually plausible
but not yet part of automated CI — a poppler/pdfium golden-render job is
future work alongside PDF/A.

---

## 7. Phase 4 — Viewing & authoring experience ✅ *core shipped as v0.8*

Formats win when reading them is frictionless and producing them is one line.

- [x] **4a. `vsd-view`**: native viewer (winit + softbuffer + the project's
      own rasterizer — no GPU stack, no toolkit). Page nav, zoom/fit, exact
      case-insensitive **search with highlights** computed from display-list
      text runs and the engine's own metrics (not raster heuristics),
      Ctrl+C copies real page text. The **verification banner is the first
      thing on screen**: validation + signature verification + layout
      recomputation run on open ("VERIFIED — N page(s) recomputed, pixels
      match content · M signature(s) verified" on green, "RENDER CACHE
      LIES" on red). Visually verified on Windows
      ([screenshot](docs/vsd-view.png)); the view-model is GUI-free and
      unit-tested. ☐ Linux/macOS visual passes, smooth scrolling,
      selection-by-mouse remain open.
- [x] **4b. WASM viewer** (`vsd-web`): the full stack — strict container
      verification, validation, **in-browser layout recomputation**, and
      rasterization — compiled to `wasm32-unknown-unknown` behind a tiny
      hand-written C ABI (no wasm-bindgen toolchain, no bundler, no npm).
      The `<vsd-doc>` web component (~150 lines of dependency-free JS)
      renders every page and shows the verification badge. ~1.1 MB gzipped,
      over half of which is the pinned Noto Sans the layout contract
      requires. Enabled by a new pure-Rust zstd decode feature
      (`vsd-container/zstd-pure`, ruzstd) so compressed containers open in
      browsers. CI builds it for wasm on every push. ☐ In-browser signature
      verification (needs a getrandom-free verify path) and a text layer
      for selection are open.
- [◐] **4c. Authoring libraries**: the high-level Rust builder shipped
      (`vsd_core::compose::Compose` — fluent
      `.h1().para().table().section()` chains, doc-tested). ☐ Python
      (`pyo3`) and TypeScript (napi/WASM) bindings remain open; they need
      their own packaging toolchains.
- [ ] **4d. Typst/LaTeX/Pandoc backends**: emit VSD from existing authoring
      ecosystems (a Pandoc writer alone unlocks dozens of input formats).
      The Markdown on-ramp (3d) covers the most common case meanwhile.
- [◐] **4e. Form filling UX**: `vsd fill --interactive` — terminal prompts
      with **live constraint evaluation** (violations re-prompt with the
      reason, cross-field constraints react immediately, computed fields
      display at the end; the prompt loop is reader/writer-generic and
      unit-tested). ☐ Viewer-integrated form filling remains open.
- [x] **4f. Diff/review UI**: `vsd diff old new --html out.html` — a
      self-contained redline view (old text struck through red, new text
      green) from the structural diff, with the amendment-chain check as a
      badge. Zero JavaScript in the output, by policy.

---

## 8. Phase 5 — Trust infrastructure at scale ✅ *core shipped as v0.9*

The features that make institutions — not individuals — switch.

- [◐] **5a. X.509 + RFC 3161**: certificate **binding** shipped —
      `vsd sign --cert` attaches a PEM/DER certificate, and verification
      checks the cert's SubjectPublicKeyInfo carries exactly the signing
      key (for hybrid signatures: the Ed25519 component, which today's
      PKI can certify) plus the validity window against a
      verifier-supplied time (the format has no clock). This kills the
      cheap lie — presenting someone else's certificate next to your
      key. ☐ Full chain-path validation to trust anchors, revocation,
      RFC 3161 timestamp tokens, and the eIDAS profile remain open.
- [x] **5b. Post-quantum signatures, hybrid-by-default**:
      `vsd keygen --algorithm hybrid` → Ed25519 **and** ML-DSA-65
      (FIPS 204, pure-Rust `fips204`) over the identical
      domain-separated message, concatenated in one signature block
      (`hybrid-ed25519-ml-dsa-65`, ~3.4 KB). **Both components must
      verify** — an attacker needs to break Ed25519 *and* ML-DSA.
      Pure-PQ alone is deliberately not offered: it would inherit
      implementation immaturity without a classical backstop. Tamper
      tests cover each component independently.
- [◐] **5c. C2PA provenance interop**: assertion authoring shipped —
      `vsd provenance add --kind ai-generated --claim model=…` appends
      to the chain (successor document, predecessor-linked, anchoring
      the prior manifest hash per spec §8); `vsd provenance show` lists
      the chain. ☐ Serializing to actual C2PA JUMBF/COSE containers
      remains open.
- [x] **5d. Transparency log** (`vsd-tlog`): RFC 6962 Merkle tree over
      document ids with BLAKE3 — inclusion proofs, consistency proofs
      (append-only verifiable: rewriting history is detected, tested),
      Ed25519-signed tree heads with their own domain separation, and a
      dead-simple append-only file format. CLI: `vsd tlog append | head
      [--key] | prove`. "This contract existed, in exactly this form,
      when head N was signed" — without the log ever holding content.
- [◐] **5e. Object-store CDN protocol**: conventions specified in
      [docs/OBJECT-STORE-HTTP.md](docs/OBJECT-STORE-HTTP.md) — `/vsd/o/{id}`
      immutable-cached objects, client-side verification as the trust
      model (a lying CDN can deny service, never substitute content),
      range-request container access, corpus-level dedup. Any static
      host implements it today. ☐ The reference server binary remains
      open.
- [◐] **5f. Selective disclosure**: shipped on existing Merkle
      mechanics, no novel crypto — `vsd seal` hoists top-level blocks
      into subtree objects; `vsd disclose --index N` emits a bundle
      (manifest + root skeleton + one subtree) in which **siblings
      travel as 32-byte hashes only**; `vsd verify-disclosure` recomputes
      the chain up to the document id — the same id signatures and tlog
      entries commit to. Tested: sibling content provably absent from
      bundle bytes; substituted/moved/flipped subtrees all rejected.
      ☐ **Salted subtree hashing** (so guessable siblings can't be
      confirmed by hashing the guess) is the remaining format addition;
      unsalted disclosure is documented as unsuitable where confirmation
      is itself a leak.

---

## 9. Phase 6 — Standardization & governance

Spec §13.5: **the real moat is political, not technical.** This dies if
proprietary.

- [ ] Publish the spec under an open license (CC-BY) in a separate
      `vsd-format/spec` repository with an issue-driven change process.
- [ ] Spec budget discipline: < 150 pages *including* the layout engine
      (PDF 2.0 is ~1,000).
- [ ] Second independent implementation (encourage; the conformance vectors
      from Phase 1 exist precisely for this). Two interoperable
      implementations is the ISO/W3C entry ticket.
- [ ] Standards track: incubate via W3C Community Group or ISO SC34 once two
      implementations pass the suite.
- [ ] Foundation/working-group governance for the format mark and the
      conformance suite; the reference implementation stays permissive
      (Apache-2.0).
- [ ] **Regulatory wedge dossiers** — where PDF satisfies the law poorly and
      VSD satisfies it by construction:
      - EU Accessibility Act (in force 2025): accessibility is a VSD
        *validity condition*, not a remediation industry
      - machine-readability mandates (e-invoicing: EN 16931 alignment)
      - AI-provenance disclosure requirements
      - court-filing redaction rules (redaction failures are structurally
        impossible — a sentence regulators understand immediately)

---

## 10. Moonshots — the "killer product" list 🚀

High-risk, high-leverage ideas. Each is gated on the invariants (especially
I2 — none of these may introduce executable content) and none may enter the
core spec before a working prototype and an adversarial review.

1. **Verifiable AI-document pipeline.** VSD as the native output of LLM
   document generation: model emits the content tree directly (it's typed
   JSON-shaped data — LLMs are *good* at this), provenance records
   `ai-generated {model, params-hash}`, the layout engine renders it, and the
   signature commits to all of it. The world's "AI slop provenance" problem
   gets a format-level answer: *unsigned/unprovenanced documents become the
   suspicious ones.*

2. **The anti-prompt-injection document.** Because structure is typed and
   there is no hidden layer — no white-on-white text, no off-page content, no
   layers, no JS — a VSD is the first document format an AI agent can ingest
   with a *provable* "what the human saw is what you read" guarantee
   (layout-hash + back-references). Position VSD as the safe input format
   for agentic workflows; ship `vsd-mcp`, an MCP server exposing
   verified-read/extract/diff tools to AI agents.

3. **Selective-disclosure documents** (5f, taken to product): a payslip where
   you reveal the salary line to the bank and nothing else — with the
   issuer's signature still verifying. Merkle proofs, zero novel crypto.
   Pair with mDL/verifiable-credential ecosystems.

4. **Smart-form documents without scripts.** Push the total expression
   language to its ceiling: spreadsheet-grade computed fields, cross-field
   constraints, conditional *visibility* — all declarative, all terminating,
   all model-checkable. Demo: a complete tax form that computes itself,
   provably cannot loop, and runs identically in every conforming viewer.
   ("PDF + JavaScript" minus the CVE feed.)

5. **Git for documents, literally.** `vsd log` / `vsd merge`: amendment
   chains + object stores give three-way merge on *structure* (not lines).
   Contract negotiation as a DAG of signed revisions, each party's signature
   surviving exactly as far as their consent extends. The "track changes"
   killer that lawyers actually trust.

6. **The archival promise: a 100-year file.** VSD/Archive + embedded fonts +
   ICC + provenance + PQ signatures + the pinned-versioned layout engine =
   a file you can verify *and re-render bit-identically* decades later (the
   engine spec is normative; any future implementation must reproduce it).
   Target national archives and digital-preservation programs (Memento/OAIS
   integration) — the institutions that wrote PDF/A into statute.

7. **Live-data documents, honestly bounded.** A declarative "data slot"
   layer: a document may declare typed slots refreshed from a signed feed
   (stock price in a prospectus, exchange rate in an invoice) — but the
   *document* remains the signed snapshot; slots are visibly distinguished,
   their feed is itself signature-verified, and rendering without network
   shows the committed values. The 80% of "we need JS in documents" use
   cases, with 0% of the attack surface.

8. **`vsd-db`: documents as a queryable corpus.** A million VSDs are a
   columnar database wearing trench coats: typed trees + content addressing
   → index every table cell, every field, every figure caption across an
   archive. "SELECT total FROM invoices WHERE supplier = X" over your
   document store, with every result row carrying a Merkle proof back to a
   signed document. BI on documents without ETL — and the answers are
   *auditable*.

9. **Hardware-anchored signing ceremonies.** FIDO2/passkey and HSM-backed
   document signing in the CLI and viewer; the signature UX of "touch your
   key to sign the contract" with the verification UX of a green banner.
   Boring crypto, product-grade ceremony.

10. **The embedded verifier.** `no_std` vsd-core verifying signatures +
    structure on microcontroller-class hardware: boarding passes, customs
    documents, medical device labels, spare-parts certificates — scanned and
    *cryptographically verified offline* at the point of use. (QR code
    carries the manifest hash + signature; the document travels separately.)

---

## 11. Release train

| Version | Theme | Headline |
|---|---|---|
| **0.1** ✅ | Canonical layer | Sign, verify, redact, diff — structure-only |
| **0.2** ✅ | Hardening | Fuzzing, proptests, conformance vectors, CI matrix, `no_std` |
| **0.3** ✅ | Streaming + filled forms | Ranged reads with lazy verification; fill/flatten lifecycle (0.2 + 0.3 shipped together as the Phase 1 release) |
| **0.4** ✅ | `vsd-layout/1.0` | Deterministic layout (minimal profile), `verify --recompute` |
| **0.5** ✅ | Rendering | Rasterizer + golden layout-hash conformance (0.4 + 0.5 shipped together as the Phase 2 release; `vsd-view` alpha moved to Phase 4a where it belongs) |
| **0.6** ✅ | PDF export | Tagged deterministic VSD→PDF with hybrid embedded source (PDF/A mode still open) |
| **0.7** ✅ | PDF import | Hybrid lossless recovery + pluggable heuristic recovery + Markdown on-ramp + `vsd migrate` (0.6 + 0.7 shipped together as the Phase 3 release; foreign tagged-structure import open) |
| **0.8** ✅ | Viewing + web | `vsd-view` native viewer, `<vsd-doc>` WASM viewer, compose API, redline diff, interactive fill (Python/TS bindings moved to the 0.9 cycle) |
| **0.9** ✅ | Trust at scale | Hybrid PQ signatures, transparency log, selective disclosure, cert binding, provenance authoring (full PKI chains + RFC 3161 + C2PA serialization carry into the 1.0 cycle) |
| **1.0** | Freeze | Spec 1.0, two implementations, audit complete, ISO/W3C track |

*Versioning policy:* the format major version and the crate versions decouple
at 1.0; the format freezes hard (I-numbered invariants are already frozen),
crates keep evolving.

---

## 12. How to contribute / what to pick up

- **Want maximum leverage now?** Grow the conformance corpus (`testdata/`)
  with adversarial vectors, or run long fuzz campaigns against the targets
  in `fuzz/` — the harnesses exist; depth is what's wanted.
- **Want the hard problem?** Phase 2f — widening `vsd-layout` to RTL/bidi
  (engine 1.1) while keeping the determinism contract airtight; the 1.0
  contract (docs/LAYOUT-1.0.md) shows the required rigor.
- **Want adoption?** Phase 3b's open half — walking a foreign tagged PDF's
  structure tree into a content tree — or richer `StructureRecovery`
  built-ins (columns, tables); the trait and pipeline already exist. A
  poppler/pdfium golden-render CI job for exported PDFs is also up for
  grabs.
- **Want a moonshot?** §10.2 (`vsd-mcp`) is genuinely small — vsd-core
  already does verified extract/diff; it needs an MCP wrapper and a README
  that explains *why agents should refuse unsigned PDFs*.

Every contribution gate: invariants hold (§1) · no `unsafe` · strict-decode
discipline (reject, never repair) · a test that would fail without the change.
