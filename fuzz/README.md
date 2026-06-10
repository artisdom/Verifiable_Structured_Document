# VSD fuzz harness

Continuous fuzzing of every parser that touches untrusted bytes. The
strict grammars were designed to be fuzzable — exercise them.

| Target | What it checks |
|---|---|
| `cbor_decode` | Decoder never panics; **anything accepted re-encodes to identical bytes** (canonical fixpoint — the no-polyglot property) |
| `container_read` | Eager + streaming `.vsd` readers on arbitrary bytes: no panic, no OOM (bomb guards), no unbounded loop |
| `tree_decode` | Content-tree decode/encode is a bijection on accepted inputs |
| `expr_decode` | Forms expressions: parsing and evaluation always terminate, never panic |

## Run

```console
$ cargo install cargo-fuzz
$ rustup toolchain install nightly
$ cargo +nightly fuzz run cbor_decode            # libFuzzer (Linux/macOS)
$ cargo +nightly fuzz run container_read -- -max_total_time=300
```

Seed corpora live under `corpus/<target>/`; regenerate richer seeds from
the conformance vectors with `cargo run -p xtask -- gen-vectors` and copy
`testdata/valid/*.vsd` into `corpus/container_read/`.

CI runs a short smoke pass of every target on each push (see
`.github/workflows/ci.yml`); long campaigns run via OSS-Fuzz once the
repository is public.
