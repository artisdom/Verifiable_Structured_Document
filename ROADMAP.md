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
Phase 2  The render layer (vsd-layout)        ░░░░░░░░░░░░░░░░░░░░  next major effort
Phase 3  PDF interop (the adoption wedge)     ░░░░░░░░░░░░░░░░░░░░
Phase 4  Viewing & authoring experience       ░░░░░░░░░░░░░░░░░░░░
Phase 5  Trust infrastructure at scale        ░░░░░░░░░░░░░░░░░░░░
Phase 6  Standardization & governance         ░░░░░░░░░░░░░░░░░░░░
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

## 5. Phase 2 — The render layer: `vsd-layout` *(the hard one)*

Spec §5 and §13.1: **deterministic layout is the format's hardest problem and
its deepest moat.** When this ships, "what you see ≠ what the text says"
becomes a *cryptographically detectable* condition — the one thing PDF can
never offer.

Strategy: don't boil the ocean. Ship determinism for a constrained profile
first, widen coverage version by version. The engine is versioned
(`vsd-layout/1.0`); documents pin the version; old caches stay verifiable
forever against their pinned engine.

- [ ] **2a. Determinism contract document** — the normative text that makes
      or breaks I4:
      - pinned text-shaping behavior (vendored HarfBuzz revision via
        `harfruzz`/pure-Rust shaper, with a bug-compatibility list)
      - Unicode line breaking (UAX #14) with an explicitly enumerated
        tailoring table
      - f64 round-to-nearest-even with a *defined operation order* (no FMA,
        no reassociation; written as scalar evaluation order in the spec)
      - hyphenation: none in 1.0 (deterministic ≫ pretty, initially)
- [ ] **2b. `vsd-layout/1.0` minimal profile**: single-column block layout,
      pinned default font family (embedded Noto subset), paragraphs/headings/
      lists/code/tables (fixed algorithm), figures as boxes, page breaking.
      Latin + common European scripts first.
- [ ] **2c. Layout-hash verification** end to end: `vsd verify --recompute`
      re-runs layout and compares against the cache. *This is the killer
      demo:* a tampered cache that shows different pixels than the signed
      content fails verification, mechanically.
- [ ] **2d. Rasterizer** (`vsd-render`): display lists → PNG/SVG via
      `tiny-skia`. Needed for visual conformance tests ("golden image" suite)
      and the viewer.
- [ ] **2e. Cross-platform determinism CI**: byte-identical page objects on
      x86-64 Linux/Windows/macOS and ARM64 — the proof I4 holds in practice.
- [ ] **2f. Widening, versioned**: RTL + bidi (1.1), CJK + vertical text
      (1.2), complex scripts/Indic (1.3), floats & multi-column (1.4),
      math layout from MathML Core (1.5), justification + hyphenation (1.6).
- [ ] **2g. Incremental relayout** (§13.2): per-section layout fences so a
      one-paragraph edit in a 10k-page manual doesn't re-paginate the world.

**Exit criteria:** two machines, two OSes, one document → bit-identical
render cache; the conformance suite contains pixel-exact golden vectors.

---

## 6. Phase 3 — PDF interop: the adoption wedge

Spec §11: *a format without a migration story is a hobby.* Organizations must
be able to adopt VSD internally with **zero external-compatibility risk**.

- [ ] **3a. VSD → PDF export** (`vsd-pdf-out`): mechanical and lossless by
      construction — display lists are a strict subset of PDF's imaging
      model. Emit *tagged* PDF (the structure tree maps directly), making
      VSD-exported PDFs more accessible than most native ones. PDF/A-2b
      output mode for statutory archival.
- [ ] **3b. Tagged-PDF → VSD importer**: structure tree → content tree
      directly; mark provenance `format-migrated { lossy: false }`.
- [ ] **3c. Untagged-PDF → VSD structure recovery**: heuristic text/column/
      table reconstruction (the majority of real PDFs). Always marked
      `format-migrated { lossy: true }`; optionally embed the source PDF as
      an attachment object for legal continuity. Design the recovery stage
      as a pluggable trait so document-understanding models can slot in
      without entering the trusted core.
- [ ] **3d. HTML/Markdown → VSD** (`vsd pack --from md`): the cheap on-ramp
      for the developer ecosystem — every README, invoice template, and
      static-site pipeline becomes a VSD producer.
- [ ] **3e. JPEG → JXL lossless recompression** in the importer (~20%
      smaller, per spec §4) and WOFF2 font subsetting with the normative
      coverage check.
- [ ] **3f. `vsd migrate` batch tool**: directory/archive-scale conversion
      with a dedup report ("your 10,000 invoices share 94% of their objects;
      archive shrank 11×") — the CFO-legible feature.

**Exit criteria:** `vsd export doc.vsd -o doc.pdf` produces a PDF that opens
pixel-correct in Acrobat; round-trip VSD→PDF→VSD preserves the content tree.

---

## 7. Phase 4 — Viewing & authoring experience

Formats win when reading them is frictionless and producing them is one line.

- [ ] **4a. `vsd-view`**: minimal cross-platform viewer (render cache → GPU
      via wgpu/softbuffer). Selection, search, copy — all *exact* thanks to
      display-list back-references. Verification status as a first-class UI
      element: green "content matches pixels, signed by X" banner.
- [ ] **4b. WASM viewer** (`vsd-web`): vsd-core+render compiled to WASM; a
      `<vsd-doc>` web component. View a signed document in any browser with
      no plugin — *this* is the distribution hack PDF never had: the viewer
      travels as 200 KB of WASM, not a 200 MB install.
- [ ] **4c. Authoring libraries**: high-level builder APIs in Rust, then
      Python (`pyo3`) and TypeScript (napi/WASM) bindings — invoice
      generators and report pipelines are the highest-volume document
      producers on earth.
- [ ] **4d. Typst/LaTeX/Pandoc backends**: emit VSD from existing authoring
      ecosystems (a Pandoc writer alone unlocks dozens of input formats).
- [ ] **4e. Form filling UX**: `vsd fill` CLI + viewer-integrated forms with
      live constraint evaluation (the evaluator already exists and provably
      terminates).
- [ ] **4f. Diff/review UI**: `vsd diff --html` producing a redline view from
      the structural diff — contract negotiation without "compare in Word".

---

## 8. Phase 5 — Trust infrastructure at scale

The features that make institutions — not individuals — switch.

- [ ] **5a. X.509 chain validation** + RFC 3161 timestamps (wire format
      already reserves both); eIDAS-friendly profile for EU qualified
      signatures.
- [ ] **5b. Post-quantum signatures**: ML-DSA-65, hybrid-by-default
      (Ed25519+ML-DSA in one signature block). Documents signed today must
      verify in 2050 — archival is the one domain where PQ is not optional.
- [ ] **5c. C2PA provenance interop**: map §8 assertions onto C2PA claims;
      `ai-generated {model, params-hash}` assertions ride the regulatory
      wave (EU AI Act transparency, agency procurement checklists).
- [ ] **5d. Transparency log** (RFC 6962-style): optional append-only log of
      document ids; "this contract existed, in exactly this form, at this
      time" without escrowing content. Redaction proofs anchor to it.
- [ ] **5e. Object-store CDN protocol**: content-addressed objects are
      `Cache-Control: immutable` by hash; a corpus-wide shared store means an
      enterprise serves a million invoices from one deduplicated object pool.
      Define the (trivial) HTTP conventions; ship a reference server.
- [ ] **5f. Selective disclosure (research → spec)**: because the tree is a
      Merkle structure, "reveal §3 to the auditor, prove it belongs to the
      signed whole, disclose nothing else" is a Merkle-path proof away.
      Salted subtree hashing to prevent sibling-hash content guessing.

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
| 0.4 | `vsd-layout/1.0` | Deterministic layout (minimal profile), `verify --recompute` |
| 0.5 | Rendering | Rasterizer, golden-image conformance, `vsd-view` alpha |
| 0.6 | PDF export | Lossless VSD→PDF (tagged, PDF/A mode) |
| 0.7 | PDF import | Tagged import + heuristic recovery + `vsd migrate` |
| 0.8 | Web + bindings | WASM viewer, Python/TS authoring |
| 0.9 | Trust at scale | X.509, timestamps, hybrid PQ, C2PA |
| **1.0** | Freeze | Spec 1.0, two implementations, audit complete, ISO/W3C track |

*Versioning policy:* the format major version and the crate versions decouple
at 1.0; the format freezes hard (I-numbered invariants are already frozen),
crates keep evolving.

---

## 12. How to contribute / what to pick up

- **Want maximum leverage now?** Grow the conformance corpus (`testdata/`)
  with adversarial vectors, or run long fuzz campaigns against the targets
  in `fuzz/` — the harnesses exist; depth is what's wanted.
- **Want the hard problem?** Phase 2a, the determinism contract. It is a
  document, not code, and it is the heart of the entire format.
- **Want adoption?** Phase 3a (PDF export) is mechanical, demoable, and the
  single most persuasive artifact for skeptics.
- **Want a moonshot?** §10.2 (`vsd-mcp`) is genuinely small — vsd-core
  already does verified extract/diff; it needs an MCP wrapper and a README
  that explains *why agents should refuse unsigned PDFs*.

Every contribution gate: invariants hold (§1) · no `unsafe` · strict-decode
discipline (reject, never repair) · a test that would fail without the change.
