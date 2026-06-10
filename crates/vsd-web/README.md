# vsd-web — VSD in the browser

The viewer travels as WASM, not as an install. All parsing, strict
verification, layout recomputation, and rasterization run client-side
in pure Rust; the `<vsd-doc>` web component is ~150 lines of
dependency-free JavaScript (no bundler, no npm, no wasm-bindgen
toolchain — the WASM boundary is a tiny hand-written C ABI). The build
is ~1.1 MB gzipped, more than half of which is the pinned Noto Sans the
layout contract requires.

## Build

```console
$ rustup target add wasm32-unknown-unknown
$ cargo build -p vsd-web --release --target wasm32-unknown-unknown
$ cp ../../target/wasm32-unknown-unknown/release/vsd_web.wasm www/
```

## Use

Serve `www/` from any static host (WASM requires http(s), not file://):

```console
$ python -m http.server -d www
```

```html
<script type="module" src="vsd-doc.js"></script>
<vsd-doc src="agreement.vsd"></vsd-doc>
```

The component renders every page and shows a verification badge:

| Badge | Meaning |
|---|---|
| ✓ verified — pixels match content (recomputed) | The render cache was re-derived from the content tree inside the browser and matched byte-for-byte |
| ✓ verified — laid out from content | Structure-only document; pages were produced from the tree directly |
| ⚠ RENDER CACHE LIES ABOUT CONTENT | Recomputation did not match: what this document displays is not what it says |
| ✗ … | The strict reader refused the file (corrupt, tampered, not VSD) |

Signature *presence* is reported; in-browser Ed25519 verification is a
follow-up (it needs a getrandom-free verify path wired for wasm).

## Safety posture

`vsd-web` is `#![deny(unsafe_code)]` with one `#[allow]`-scoped FFI
module containing the minimal pointer-crossing any WASM ABI requires
(alloc/free/slice-from-parts). Every byte of document handling happens
in the `forbid(unsafe_code)` core crates.
