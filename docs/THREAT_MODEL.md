# VSD threat model

Written adversarial analysis of the reference implementation, maintained
alongside the code. Spec §12 gives the format-level threat table; this
document maps it to concrete code, tests, and residual risks.

## Assets

1. **Document integrity** — what a reader displays/extracts is what the
   author committed to (`doc_id = BLAKE3(manifest)`).
2. **Signature meaning** — a signature endorses exactly one content
   Merkle root, never "whatever bytes happen to be in the file".
3. **Redacted content** — once redacted, content is unrecoverable from
   the artifact.
4. **Reader availability** — hostile input cannot crash or exhaust the
   reading process.
5. **Form semantics** — constraints/computed values evaluate identically
   everywhere and always terminate.

## Adversaries

- **A1 Malicious document author** — crafts a `.vsd` to exploit readers
  or to show different content to different consumers.
- **A2 Man-in-the-middle / storage tamperer** — modifies a file after
  signing, including with checksum fix-ups.
- **A3 Malicious server** (streaming) — serves substituted or corrupted
  ranges to a `StreamReader`.
- **A4 Careless or hostile tool** — third-party software that tries to
  "redact" by overlaying, or re-encodes objects non-canonically.

## Attack surfaces and mitigations

### 1. CBOR decoder (`vsd-core::cbor`) — A1

The single grammar everything else stands on.

| Threat | Mitigation | Evidence |
|---|---|---|
| Polyglot / dual-representation input | Strict deterministic decoding: minimal-length integers, sorted unique map keys, definite lengths only, shortest-form floats, canonical NaN, no tags, no trailing bytes | `accepted_bytes_are_canonical_fixpoint` property test; `cbor_decode` fuzz target asserts decode∘encode = identity |
| Stack exhaustion via nesting | `MAX_DEPTH = 256` enforced at encode *and* decode | `rejects_*` unit tests; fuzzing |
| Memory exhaustion via length fields | Lengths bounds-checked against remaining input before allocation; collection capacity clamped | `decode_never_panics` property test |

### 2. Container parser (`vsd-container`) — A1, A2

| Threat | Mitigation | Evidence |
|---|---|---|
| Truncation / size lies | Header declares total size; mismatch is a hard error | `tampering_is_detected` test; `invalid/truncated.vsd` vector |
| Bit-flips | BLAKE3-64 chunk checksums **and** per-object BLAKE3-256 verification (`put_verified`) | `invalid/corrupt-objs.vsd` vector |
| Checksum fix-up tampering | Identity cross-checks: trailer doc-id must equal BLAKE3(MNFST); objects must hash to their index ids | `invalid/trailer-docid-mismatch.vsd` vector — checksums deliberately repaired, must still be rejected |
| Non-canonical manifest | MNFST payload must re-encode to itself | `invalid/noncanonical-mnfst.vsd` vector |
| Decompression bomb | Streaming zstd decode behind a hard 64 GiB read cap; chunk lengths sanity-capped before allocation | regression: the original `bulk::decompress` preallocation was caught by test and replaced |
| Unknown chunk smuggling | Unknown *critical* chunks are fatal; unknown non-critical chunks require an explicit opt-in flag | `invalid/unknown-critical.vsd` vector |
| Duplicate/misplaced chunks | Duplicate MNFST/INDEX/TRAILR rejected; trailer position cross-checked against header | reader unit logic; fuzzing |

### 3. Streaming reader (`StreamReader`) — A3

A lying range server can serve anything. Every object fetched is
verified (canonical form + BLAKE3 = requested id) *before* use, so
substitution is detected at the first touched object; the manifest is
verified against the trailer doc-id at open. Whole-chunk checksums are
not verified in ranged mode — per-object content addressing is the
stronger guarantee and is always applied.
Evidence: `stream_reader_rejects_substituted_object` test; `container_read`
fuzz target drives the lazy path.

### 4. Signatures (`vsd-sign`) — A1, A2

| Threat | Mitigation |
|---|---|
| Cross-protocol replay | Domain separation prefix `VSD-SIG-v1\0` + scope byte in the signed message |
| Scope confusion (subtree sig presented as document sig) | Scope byte is part of the signed message; unit-tested both directions |
| Shadow attack (signature from revision A presented on revision B) | Target binding: verification distinguishes `Valid` from `ValidForOtherTarget`; amendment chains are explicit via `predecessor` |
| Algorithm confusion | `alg` is part of the signature object; only implemented algorithms verify, others error |

### 5. Redaction (`vsd-core::redact`) — A4

The format has no overlay construct, so "black box over live text" is
unrepresentable. The implemented operation replaces the subtree, purges
unreferenced objects, drops the render cache, page index, **and filled
form layer** (filled values may quote redacted content). Validation
flags orphan objects as a leak smell.
Evidence: `redaction_never_leaks` property test scans every byte of
container output *and* every store object on generated documents;
`valid/redacted.vsd` vector records a `must_not_contain` string.

### 6. Forms (`vsd-core::forms`) — A1

Total language: no loops, no eval, no I/O; expression depth/size caps at
decode; regexes are RE2-class (Rust `regex` — linear time, no
backtracking) with pattern length and compiled-size limits, fully
anchored. Computed-field dependency cycles are rejected at validation
(`E_FIELD_CYCLE`). Evidence: `expr_decode` fuzz target evaluates every
accepted expression; `backreferences_rejected`, `depth_bounded` tests.

### 7. Layout engine (`vsd-layout`) — A1, A4

| Threat | Mitigation |
|---|---|
| Cache shows different content than the tree (shadow text, contract swap) | Recomputation verification: byte-identical page objects required; integer-µm arithmetic makes the result platform-invariant, so a verifier on any OS reaches the same verdict (golden-hash conformance vector, CI-enforced cross-platform) |
| Hostile document drives pathological layout cost | Cost is linear in content size; content size is bounded by the container's chunk caps; tree depth is bounded by the CBOR `MAX_DEPTH` |
| Arithmetic overflow | All scaled products go through one 128-bit-intermediate primitive; µm magnitudes for any physical document are ≤ 10⁹, far inside i64 |
| Engine-version confusion | The cache pins engine name+version; verification of an unknown engine reports `UnknownEngine` rather than silently passing or guessing |

### 8. PDF interop (`vsd-pdf`) — A1

| Threat | Mitigation |
|---|---|
| Hostile PDF exploits the importer | The PDF parser (`lopdf`) handles untrusted bytes, but everything it yields is *re-validated*: an embedded `source.vsd` goes through the full strict container/object verification (a tampered embed fails like any tampered `.vsd`); heuristically recovered text becomes ordinary tree nodes subject to validation. The PDF parser can lie about content, never about a VSD identity. |
| Hybrid substitution (attacker swaps the embedded `.vsd`) | The embedded source is a complete signed container: its document id is recomputed from bytes on import and signatures verify against it. Swapping the embed changes the id and breaks signatures — and the visible PDF pages then disagree with an *honest* re-export, which is diffable. |
| Export-side injection via document text | PDF string/name contexts are escaped; text is emitted as hex-encoded glyph ids (no literal-string parsing ambiguity); the writer emits no JavaScript, launch actions, or embedded files other than the declared source attachment. |
| Importer resource exhaustion | Page text extraction is bounded by the PDF's own page count; recovered documents pass through the standard tree/size limits. `lopdf` parsing of pathological PDFs is fuzz-worthy — listed as future fuzz target. |

### 9. Supply chain & implementation

- `#![forbid(unsafe_code)]` in all four crates (compiler-enforced).
- Dependency surface kept deliberately small: blake3, zstd,
  ed25519-dalek, regex, thiserror, hex (+ CLI-only: clap, serde_json,
  anyhow). The CBOR codec is in-tree precisely to avoid a parser
  dependency with its own ideas about leniency.
- CI builds with `-D warnings`, runs clippy, fuzz smoke passes, and
  checks `no_std`/wasm targets.

## Residual risks (known, accepted for 0.x)

1. **No X.509/PKI validation yet** — signatures verify against raw keys;
   trust distribution is the caller's problem until Phase 5a.
2. **Render-cache faithfulness is verified on demand, not on load** —
   `vsd verify --recompute` re-runs `vsd-layout/1.0` and requires
   byte-identical page objects (a grafted cache passes structural checks
   and is caught only by recomputation; tested). Recomputation is
   opt-in because it costs a full layout pass; readers that display a
   cache without recomputing trust the cache for speed, per spec §5.3.
   Caches from unknown engine versions cannot be recomputed by this
   build and verify as such, explicitly.
3. **`no_std` builds do not evaluate regex constraints** (documented;
   embedded verifiers check structure and signatures, not forms).
4. **Timing side channels** are not considered: verification operates on
   public data; signing keys are handled by ed25519-dalek (which is
   constant-time for secret material).
5. **External security audit** has not yet happened; planned before 1.0.
