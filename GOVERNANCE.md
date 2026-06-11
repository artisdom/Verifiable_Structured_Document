# Governance

VSD dies if proprietary (ROADMAP §13.4 / spec history §13.4). This
document is the open-incubation plan and the current decision process.

## Licensing

- **Specification** (`spec/`): CC-BY 4.0. Anyone may implement it,
  commercially or otherwise, without permission.
- **Reference implementation** (everything else): Apache-2.0
  ([LICENSE](LICENSE)). Patent grant included by design.
- **Embedded font**: Noto Sans, SIL OFL 1.1
  (`crates/vsd-layout/assets/OFL.txt`).

## Current phase: open incubation

While there is a single implementation, the maintainers of this
repository are the spec editors. Process:

1. **Format changes land as spec + code + vectors in one change.** A
   format change without a conformance vector does not exist.
2. **The invariants (spec §1) are frozen.** Changes that violate I1–I5
   are rejected regardless of merit; "just a small scripting hook" is
   the canonical example.
3. **Strictness is a one-way door.** Parsers may become stricter, never
   looser, within a major version.
4. **Versioning:** additive changes bump the minor version and are
   recorded in spec §15. Breaking changes require a major bump and a
   migration story, and are expected to be ~never.

## Path to shared governance

| Milestone | Trigger | Change |
|---|---|---|
| Second implementation passes the corpus | external team, any language | Spec moves to its own repository (`vsd-format/spec`); editors from both implementations; changes by documented consensus |
| Two interoperable implementations + audit | pre-1.0 gate | Submit to a standards venue — W3C Community Group first (low ceremony), ISO/IEC JTC 1/SC 34 when adoption warrants |
| Standardization underway | — | Format mark and conformance corpus move to a neutral foundation; this repository remains *a* reference implementation, not *the* authority |

## Security

Vulnerabilities follow [SECURITY.md](SECURITY.md) (private disclosure),
including spec-level issues (canonicality breaks, verification
bypasses) — a spec bug that permits two readings of one document is
treated as severity-critical.
