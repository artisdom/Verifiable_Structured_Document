# Security policy

VSD's reason to exist is that documents are an attack surface. We treat
parser bugs, verification bypasses, and redaction leaks as the most
serious class of defect this project can have.

## Reporting a vulnerability

Please report suspected vulnerabilities privately via GitHub Security
Advisories ("Report a vulnerability" on the repository's Security tab).
Do not open public issues for security reports.

You can expect an acknowledgement within 7 days. Coordinated disclosure
is preferred; we will credit reporters unless asked not to.

## In scope (the things we most want to hear about)

- **Verification bypass**: a tampered container, object, or signature
  that `vsd verify` / the reader APIs accept.
- **Canonicality break**: two distinct byte sequences that decode to the
  same value (or one value with two accepted encodings) — this would
  break the one-document-one-hash property.
- **Redaction leak**: any byte of redacted content recoverable from a
  post-redaction file.
- **Resource exhaustion**: input that causes unbounded memory, stack, or
  CPU (decompression bombs, deep nesting, pathological expressions).
- Panics in any parser reachable from untrusted bytes (we ship fuzz
  targets — `fuzz/` — and treat fuzz findings as bugs even when "just" a
  panic).

## Out of scope

- Vulnerabilities requiring a malicious *writer* of the local key files
  or a compromised host.
- The `examples/` and `xtask/` developer tooling.
- The fixed conformance-vector signing key (`testdata/`) — it is public
  by design and authorizes nothing.

## Design commitments relevant to security review

See [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md) for the full analysis.
Headlines: no executable content in the format; strict (reject, never
repair) parsing; `#![forbid(unsafe_code)]` across all crates; all
allocation and nesting bounded on hostile input; signatures over content
Merkle roots with domain separation.
