# Contributing to VSD

Thanks for considering it. Start with [ROADMAP.md](ROADMAP.md) §12 for
high-leverage open work, and [GOVERNANCE.md](GOVERNANCE.md) for how
format decisions are made.

## Ground rules (every contribution)

1. **The invariants hold** (spec §1). No executable content, no
   leniency in parsers, content-addressing everywhere, cryptography
   over meaning. PRs that trade an invariant for a feature are closed
   with a pointer to this line.
2. **No `unsafe`.** All format/parsing crates are
   `#![forbid(unsafe_code)]`. The single exception is `vsd-web`'s
   WASM-boundary FFI module (`deny` + one scoped `allow`).
3. **Reject, never repair.** A parser change that accepts previously
   rejected bytes is a format change and needs spec + vectors, not just
   code.
4. **Tests that would fail without the change.** Format changes
   additionally need conformance vectors (`xtask gen-vectors`) in the
   same PR.

## Mechanics

```console
$ cargo test --workspace          # all green before and after
$ cargo fmt --all
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo check -p vsd-core --no-default-features   # no_std stays intact
$ cargo run -p xtask -- gen-vectors               # if you touched the format
```

CI mirrors exactly this plus cross-OS runs, wasm/embedded targets, and
fuzz smoke (`fuzz/README.md` — long campaigns very welcome).

- Commit messages: explain *why*; format changes cite spec sections.
- New dependencies need justification in the PR description — the
  small-attack-surface story is a feature.
- Security issues: [SECURITY.md](SECURITY.md), privately.

## Writing a second implementation?

Best possible contribution. See
[spec/CONFORMANCE.md](spec/CONFORMANCE.md) — you should never need to
read this repo's source; where you do, file the spec gap as a bug.
