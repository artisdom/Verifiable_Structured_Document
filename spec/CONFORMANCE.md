# VSD Conformance Program

The route to design invariant I4 — "one reference implementation,
conformance-tested" — and to the standards track (two independent
interoperable implementations is the ISO/W3C entry ticket).

## What conformance means

A **conforming reader** MUST, for every entry in
[`testdata/vectors.json`](../testdata/vectors.json):

- accept every file under `valid/`, derive exactly the recorded
  `doc_id`, and report the document valid;
- reject every file under `invalid/` at read or validation time —
  including the vectors whose checksums were deliberately repaired
  after tampering, which only the identity cross-checks can catch;
- verify recorded signatures and reproduce recorded layout hashes
  (entries carrying `layout_hash` require byte-identical recomputation
  per [docs/LAYOUT-1.0.md](../docs/LAYOUT-1.0.md));
- verify disclosure bundles (`kind: disclosure-bundle`) against their
  `doc_id` and reject any byte modification.

A **conforming writer** MUST produce output that a conforming reader
accepts, with canonical encoding throughout (a writer whose output
re-encodes differently is non-conforming by definition).

## Using the corpus

```console
$ cargo run -p xtask -- gen-vectors    # regenerate (deterministic)
$ cargo test -p vsd-cli --test conformance
```

The corpus is deterministic by construction: fixed test keys (Ed25519
seed `0x2a × 32` — public, authorizes nothing), fixed salts for sealed
vectors, no timestamps. CI regenerates it on a different OS than it was
committed from and diffs `vectors.json` — which is also the
cross-platform layout-determinism gate.

## For a second implementation

You should not need to read the reference source. Sufficient inputs:

1. [SPEC.md](SPEC.md) (format) and
   [docs/LAYOUT-1.0.md](../docs/LAYOUT-1.0.md) (layout contract);
2. the corpus and its expectation manifest;
3. the pinned font binary (committed, hash in the layout contract).

Anything you need beyond these is a spec bug — please file it as one.
When your implementation passes the corpus, file an issue: two
interoperable implementations unlock the standards track
([GOVERNANCE.md](../GOVERNANCE.md)).

## Growing the corpus

New vectors accompany every format addition (the `salted` node shipped
with `sealed-salted.vsd` and `disclosure.vsdp` in the same change).
Adversarial vectors are especially welcome — the most valuable entries
are files that *one* implementation accepts and another rejects.
