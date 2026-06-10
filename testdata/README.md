# VSD conformance vectors

A public corpus of `.vsd` files with declared expected outcomes, so that
**a second implementation can be tested without reading this repository's
source code**. This is the seed of design invariant I4 ("one reference
renderer, conformance-tested").

- [`vectors.json`](vectors.json) — the expectation manifest. For every
  file under `valid/`, a conforming reader MUST accept it, derive exactly
  the recorded `doc_id`, and report it valid. For every file under
  `invalid/`, a conforming reader MUST reject it at read or validation
  time (the `reject_reason` documents what property the vector probes).
- Regenerate deterministically: `cargo run -p xtask -- gen-vectors`.
  The signing key for `signed.vsd` is a fixed public test seed
  (`0x2a × 32`); Ed25519 signing is deterministic, so regeneration is
  byte-stable (except `minimal-compressed.vsd`, whose bytes may vary
  with the zstd version — its *document id* must not).
- The runner lives at `crates/vsd-cli/tests/conformance.rs`.

Highlights worth understanding:

| Vector | What it proves |
|---|---|
| `valid/minimal.vsd` vs `valid/minimal-compressed.vsd` | Different bytes, same `doc_id`: identity is BLAKE3(manifest), not file bytes (spec §2.4) |
| `valid/redacted.vsd` | Destructive redaction: the recorded secret appears in no byte of the file; predecessor + proof recorded (spec §7.2) |
| `invalid/trailer-docid-mismatch.vsd` | Checksums deliberately *fixed up* after tampering — only the identity cross-check can catch it, and it must |
| `invalid/noncanonical-mnfst.vsd` | A manifest that decodes but is not canonical CBOR cannot be a document identity preimage |
