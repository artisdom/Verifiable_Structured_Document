# VSD — Verifiable Structured Document

## Format Specification, version 0.4 (1.0-track draft)

**License:** This specification is published under [CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/).
The reference implementation is Apache-2.0.
**Status:** Implementation-synced draft. Everything specified here is
implemented by the reference implementation in this repository and
exercised by the public conformance corpus (`testdata/`,
[CONFORMANCE.md](CONFORMANCE.md)). The layout engine has its own
normative contract: [docs/LAYOUT-1.0.md](../docs/LAYOUT-1.0.md).
**Supersedes:** `vsd-spec-draft.md` (the 0.1 concept draft).

Design goals: layout fidelity of PDF · parseability of HTML · integrity
model of Git · attack surface of a JPEG.

---

## 1. Design invariants

| # | Invariant | Consequence |
|---|-----------|-------------|
| I1 | **Structure is canonical, pixels are cache.** | The typed content tree is the document. Fixed layout is a deterministic, recomputable projection of it. |
| I2 | **Zero executable content.** | No scripting engine anywhere. Interactivity is a total, terminating declarative expression language. |
| I3 | **Every object is content-addressed.** | `object_id = BLAKE3-256(canonical_encoding(object))`. Dedup, diff, partial fetch, and integrity verification are structural facts. |
| I4 | **One reference implementation, conformance-tested.** | "Behaves identically everywhere" is enforced by the public test vectors, not hoped for. |
| I5 | **Cryptography over meaning, not bytes.** | Signatures cover Merkle roots of content; they survive recompression, repacking, and chunk reordering. |

Readers MUST be strict: reject, never repair. Every "be liberal in what
you accept" decision in a document format eventually becomes a CVE or a
polyglot.

## 2. Deterministic CBOR profile

All objects are encoded in CBOR (RFC 8949) restricted to its Core
Deterministic Encoding (§4.2), further restricted:

- integers, lengths, and float arguments use the shortest possible form;
- indefinite-length items are forbidden;
- map keys MUST be strictly increasing in bytewise encoded order
  (forbids duplicates);
- floats use the shortest of binary16/32/64 that preserves the value;
  the only admissible NaN is `0xf97e00`;
- tags and simple values other than `false`/`true`/`null` are forbidden;
- a decoder MUST reject any non-canonical input, including trailing
  bytes, and MUST enforce a nesting-depth bound (reference: 256).

Consequence: every logical value has exactly one byte representation,
so every object has exactly one hash, so a `.vsd` cannot be a polyglot
and two conforming parsers cannot disagree about content.

## 3. Container

A `.vsd` file is a 32-byte header followed by length-prefixed chunks,
little-endian, in this order:

```
HEADER · MNFS · INDX · OBJS (1..n) · SIGS (0..n) · TRLR
```

### 3.1 Header (32 bytes)

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | Magic `89 56 53 44 0D 0A 1A 0A` (`\x89VSD\r\n\x1a\n`) |
| 8 | 2 | Major version (u16 LE) — readers MUST refuse unknown majors |
| 10 | 2 | Minor version (u16 LE) — additive; see §15 |
| 12 | 4 | Profile flags: bit0 core, bit1 archive, bit2 form, bit3 stream |
| 16 | 8 | Total file size (u64 LE) — truncation/append detection |
| 24 | 8 | Offset of the TRLR chunk (u64 LE) |

### 3.2 Chunk framing

```
u64 LE   payload length (as stored)
u32 LE   chunk type (FourCC: "MNFS" | "INDX" | "OBJS" | "SIGS" | "TRLR")
u32 LE   flags — bit0 critical, bit1 zstd-compressed
[..]     payload
u64 LE   checksum: first 8 bytes of BLAKE3-256(stored payload)
```

Unknown chunk types with the critical flag are fatal; without it they
are skippable only when the reader explicitly opts in. Compressed
payloads are zstd frames; readers MUST bound decompressed size (the
reference cap is 64 GiB) — decompression bombs are a parser attack.

### 3.3 Chunk payloads

- **MNFS**: the manifest's canonical CBOR bytes, exactly once,
  uncompressed. **The document identity is `BLAKE3-256(MNFS payload)`.**
  Readers MUST verify the payload is canonical (re-encode equals input).
- **INDX**: a CBOR map `object_id (bytes32) → [chunk_file_offset,
  intra_offset, length, codec]` (all uints). Intra offsets refer to the
  decompressed chunk payload. Codec 0 = raw canonical CBOR; other
  values are reserved and fatal.
- **OBJS**: concatenated object bytes. On load, every indexed object
  MUST strictly decode, re-encode to itself, and hash to its claimed id.
- **SIGS**: a CBOR array of signature maps (§10).
- **TRLR**: CBOR map `{"doc-id": bytes32, "index-offset": uint,
  "mnfst-offset": uint}`. `doc-id` MUST equal BLAKE3(MNFS payload).

Identity is independent of the container: two files with different
compression or chunking but the same manifest hash are the same
document.

## 4. Manifest

```cddl
manifest = {
  "vsd-version": [major: uint, minor: uint],
  "root":        object-ref,          ; content tree root (§5)
  "resources":   object-ref,          ; resource table (§6)
  "metadata":    object-ref,          ; metadata object (§6.3)
  ? "render-cache": object-ref,       ; layout projection (§7)
  ? "page-index":   object-ref,       ; streaming index (§13)
  ? "field-layer":  object-ref,       ; filled form values (§9.2)
  ? "provenance":   object-ref,       ; assertion chain (§12)
  ? "predecessor":  object-ref,       ; amendment chain (§10.3)
  "profile":     "core" / "archive" / "form" / "stream",
}
object-ref = bytes .size 32
```

Optional keys are **omitted** when absent, never null. Unknown keys are
rejected within a major version. "Editing" produces new objects and a
new manifest; unchanged objects are shared.

## 5. Content tree

The canonical semantic layer. Node maps carry a `t` discriminator;
optional fields are omitted when absent; unknown keys are rejected.

| `t` | Fields | Notes |
|---|---|---|
| `doc` | `lang` tstr, `dir` "ltr"/"rtl", `children` [node] | root only |
| `sec` | `role` tstr, `children` [node] | semantic section |
| `h` | `level` 1..6, `children` [inline] | heading |
| `p` | `children` [inline] | paragraph |
| `table` | `cols` [{?width: float}], `head`/`body`/`foot` [row] | row = `{cells:[cell]}`; cell = `{?span:[r,c], ?scope:"row"/"col", children:[node]}`; per-row col-span sum must equal the column count; body non-empty |
| `fig` | `res` ref, `alt` tstr, `?decorative` true, `caption` [inline] | **alt is a validity condition**: empty alt without `decorative:true` is malformed |
| `list` | `ordered` bool, `items` [[node]] | |
| `code` | `?lang` tstr, `text` tstr | verbatim |
| `math` | `mathml` tstr, `?fallback` ref | MathML Core |
| `field` | `id`, `kind`, `?label`, `required`, `?constraint`, `?computed` | §9 |
| `pagebreak` | — | hint |
| `redacted` | `?reason` tstr, `?proof` bytes32 | §11 |
| `ref` | `ref` object-ref | subtree stored as separate object — the tree is a Merkle structure |
| `salted` | `salt` bytes(16..32), `child` node | added in 0.2; §11.2 |

Inlines: bare text strings, `span` (`?style` uint into the style table,
`children`), `link` (`href`, `children`), `math`, `fnref` (`id`).
Reading order is tree order. Field ids must be unique; computed-field
dependency graphs must be acyclic.

## 6. Resources

- **Resource table object**: `{"entries": {name → {kind, mime, data:
  object-ref}}, "styles": [style]}`; kinds: `image`, `vector`, `font`,
  `icc`, `attachment`; style = `{?b,?i,?u,?mono: true}`.
- **Blob object**: `{"t":"blob", "mime": tstr, "data": bytes}`.

## 7. Render layer

The render cache is a deterministic projection of the content tree
through a versioned layout engine:

```cddl
render-cache = {
  "layout-engine": {"name": tstr, "version": tstr},
  "geometry": {"w": float, "h": float, "unit": "mm"},
  "pages": [+object-ref],            ; display-list objects
  "layout-hash": bytes .size 32,     ; BLAKE3(canonical CBOR of pages array)
}
```

Pages are flat display lists: `{"t":"page","w","h","ops":[op]}` with
ops `text` (x, y baseline, font uint, size pt, color RGBA bytes4, text,
`src` node path, `range` byte range into the block's layout text, and —
added in 0.3 — an optional `rtl` bool: the run's `text` is stored in
logical order and consumers draw its glyphs right-to-left starting at
`x`, the run's left edge; omitted when false, so pre-0.3 pages decode
unchanged), `image` (x,y,w,h,res), `rect` (x,y,w,h,fill), and — added in 0.4 —
`glyphs`: a pre-shaped complex-script run carrying positioned glyphs
`g` (each `[gid, x_advance, x_offset, y_offset, cluster]`) in visual
order plus the logical `text`, `src`, and `range`. Consumers draw the
glyphs by id (needing no shaper) while selection/search/extraction use
the logical `text`; `cluster` is the source byte offset of each glyph.
Positions are mm floats produced by exact integer→float conversion (see
the layout contract).

Verification levels: (1) structural — `layout-hash` matches the page
list, pages decode; (2) **recomputation** — re-run the named engine *at
the version the cache pins* and require identical page object ids. A
cache that displays anything other than the tree's content cannot
survive (2). Engine versions are immutable contracts: once shipped,
their output never changes, so old caches stay verifiable forever.
Reference contracts: `vsd-layout/1.0.0`
([docs/LAYOUT-1.0.md](../docs/LAYOUT-1.0.md)), `vsd-layout/1.1.0`
([docs/LAYOUT-1.1.md](../docs/LAYOUT-1.1.md), adds bold/italic faces),
`vsd-layout/1.2.0` ([docs/LAYOUT-1.2.md](../docs/LAYOUT-1.2.md),
adds monospace, underline, justification, and Hebrew bidi; refuses
scripts it cannot set faithfully), `vsd-layout/1.3.0`
([docs/LAYOUT-1.3.md](../docs/LAYOUT-1.3.md), adds Knuth–Liang
hyphenation of English body text and widow/orphan control — no
display-list format change), and `vsd-layout/1.4.0`
([docs/LAYOUT-1.4.md](../docs/LAYOUT-1.4.md), adds Arabic + Devanagari
shaping via a pinned pure-Rust HarfBuzz port, emitting the `glyphs` op;
still refuses CJK / Thai / other complex scripts). Each engine version
is frozen: the conformance corpus pins one golden vector per version and
a conforming reader MUST reproduce all of them.

## 8. (reserved)

Numbering reserved to keep §-references from the 0.1 draft stable where
they appear in commit history and docs.

## 9. Forms

### 9.1 Expression language

Total and terminating: literals, `{"$": field-id}` references, arrays
`[op, …]` with ops `+ - * / min max round`, comparisons
`= != < <= > >=`, `and or not if match regex-valid`. No loops, no
string-eval, no I/O, no clock. Decoders MUST bound depth (64) and node
count (10 000). Regex patterns MUST be RE2-class (no backtracking),
matched fully anchored, with pattern-length and compiled-size limits.
`round` is round-half-to-even.

### 9.2 Filled layer

Filled values live in a separate object over the immutable base form:
`{"t":"filled","values":{field-id → text/float/bool/null}}`, referenced
from the manifest's `field-layer`. A filled instance is a successor
document (`predecessor` = the blank form). Constraint violations make a
document *warnable*, not malformed — partially filled forms are
legitimate saved states. **Flattening** is the defined merge: computed
fields evaluate in dependency order, every constraint must hold, field
nodes are replaced by their final values, the layer is dropped.

## 10. Signing

### 10.1 Signature object (SIGS payload entries)

```cddl
signature = {
  "scope": "document" / "subtree" / "field-layer",
  "target": object-ref,      ; manifest hash, or an object id
  "alg": "ed25519" / "hybrid-ed25519-ml-dsa-65"
       / "ecdsa-p256" / "ml-dsa-65",            ; last two reserved
  "pubkey": bytes, ? "cert": bytes, ? "timestamp": bytes,
  "sig": bytes,
}
```

### 10.2 Signed message

`"VSD-SIG-v1\0" ‖ scope-byte ‖ target`, where scope-byte is 0
(document), 1 (subtree), 2 (field-layer). Domain separation is
mandatory.

- `ed25519`: 32-byte key, 64-byte signature.
- `hybrid-ed25519-ml-dsa-65`: `pubkey = ed25519_pk(32) ‖ mldsa65_pk(1952)`,
  `sig = ed25519_sig(64) ‖ mldsa65_sig(3309)`; ML-DSA context is empty.
  **Both components MUST verify.**

Verification MUST additionally bind the target to the document at hand:
a cryptographically valid signature whose target is not this document
(e.g. a predecessor) is reported as such, never as valid-for-this.

When `cert` is present (X.509, DER or PEM), verifiers MUST check the
certificate's SubjectPublicKeyInfo equals the signing key (the Ed25519
component for hybrid) before attributing the signature to the cert's
subject. Chain validation policy is out of scope of this version.

### 10.3 Amendment chains

A revision references its predecessor's document id via `predecessor`.
Old signatures keep verifying against the old manifest; they are
history, not endorsements of the new revision.

## 11. Redaction and disclosure

### 11.1 Redaction (destructive by construction)

1. the target subtree is **replaced** by a `redacted` node whose
   `proof` is BLAKE3-256 of the removed subtree's canonical encoding;
2. objects unreachable from the new manifest are **purged**;
3. `render-cache`, `page-index`, and `field-layer` are dropped (any of
   them can quote removed content);
4. the new manifest records the original as `predecessor`.

A black box over live text is not representable.

### 11.2 Selective disclosure

Sealing hoists each top-level block into its own object behind `ref`;
with salting, each block is first wrapped in `salted` (16–32 random
CSPRNG bytes), so a hidden sibling's object id covers content ‖ salt
and cannot be confirmed by hashing a guess. A **disclosure bundle**
is `{"t":"disclosure","doc-id":bytes32,"index":uint,"manifest":bytes,
"root":bytes,"subtree":bytes}`. Verifiers MUST check: manifest bytes
hash to doc-id; root bytes hash to the manifest's `root`; the root's
child at `index` is a `ref` whose hash the subtree bytes match; all
three byte strings are canonical. Unsalted disclosure MUST be flagged
to users where sibling-guess confirmation is a leak.

## 12. Provenance

`provenance` references a CBOR array of assertions
`{"kind": tstr, "claims": {tstr→tstr}, "manifest-hash": object-ref}`.
Each assertion anchors the manifest hash at its point in history;
appending one produces a successor document. Recommended kinds:
`created-by`, `derived-from`, `ai-generated` (claims `model`,
`params-hash`), `scanned-from-physical`, `format-migrated` (claims
`source`, `tool`, `lossy`).

## 13. Streaming

`page-index` references `{"t":"page-index","pages":[[object-ref…]]}` —
per page, the page object id plus the resources it places. With the
header → trailer → INDX walk, a ranged client fetches exactly the
objects for one page. HTTP serving conventions:
[docs/OBJECT-STORE-HTTP.md](../docs/OBJECT-STORE-HTTP.md).

## 14. Transparency logs

Operators MAY log document ids in an RFC 6962-style Merkle tree:
`leaf = BLAKE3(0x00 ‖ id)`, `node = BLAKE3(0x01 ‖ L ‖ R)`, empty head
`BLAKE3("")`; inclusion and consistency proofs per RFC 6962 §2.1.
Signed tree heads sign `"VSD-TLOG-v1\0" ‖ size_le64 ‖ root` with
Ed25519; serialized as `size_le64 ‖ root32 ‖ pubkey32 ‖ sig64`. Log
file format: `"VSDTLOG1"` magic followed by 32-byte entries.

## 15. Profiles and versioning

| Profile | Requirements |
|---|---|
| `core` | §2–§7; render cache optional |
| `archive` | render cache and provenance mandatory; field layer forbidden |
| `form` | core + §9 + §10 |
| `stream` | core + mandatory `page-index` |

**Versioning:** major bumps are breaking; readers refuse unknown
majors. Minor bumps are additive (new node types, new algorithm
strings); strict readers of an older minor will reject documents using
newer constructs — by design, never mis-render. History: 0.1 initial;
0.2 added `salted` (§11.2) and `field-layer` (§9.2),
`hybrid-ed25519-ml-dsa-65` (§10.2); 0.3 added the `rtl` flag on `text`
display ops (§7); 0.4 added the `glyphs` display op for pre-shaped
complex-script runs (§7).

## 16. Registries

Strings with format-level meaning (chunk FourCCs, node `t` values,
signature algorithms and scopes, resource kinds, profile names,
assertion kinds, domain-separation prefixes) are enumerated by this
specification; additions require a spec change, not just code. The
authoritative lists are §3.2, §5, §6, §10.1, §12, §15 of this document.

---

*Conformance is defined operationally: pass the public corpus. See
[CONFORMANCE.md](CONFORMANCE.md).*
