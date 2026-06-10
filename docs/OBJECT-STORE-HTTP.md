# VSD Object Store over HTTP — Protocol Conventions

**Status:** Draft conventions (ROADMAP 5e). A reference server is future
work; any static file host or CDN can already implement this, which is
the point.

VSD objects are immutable and content-addressed (`id = BLAKE3-256` of
canonical bytes). That makes the transport story almost embarrassingly
simple — the protocol below mostly consists of *not doing things*.

## 1. Resource layout

```
GET /vsd/o/{id}          one object, raw canonical CBOR bytes
GET /vsd/d/{doc-id}      a document: either a .vsd container, or 302 →
                         a manifest-driven assembly (see §3)
GET /vsd/d/{doc-id}.sth  optional: latest signed tree head of the
                         operator's transparency log (vsd-tlog format)
```

`{id}` and `{doc-id}` are 64 lowercase hex characters. Servers MUST
reject other forms (no traversal surface).

## 2. The rules (all of them)

1. **Objects are immutable.** Serve `/vsd/o/{id}` with
   `Cache-Control: public, max-age=31536000, immutable` and a strong
   `ETag` equal to the id. A correct client never revalidates an object.
2. **Verification is the client's job and the client's power.** The
   client hashes every received object and compares to the id it asked
   for. A lying or compromised CDN can deny service, never substitute
   content. (This is exactly the `StreamReader` model — see
   `vsd-container`'s `RangeSource`; an HTTP implementation of that trait
   plus this layout is a complete remote reader.)
3. **Range requests** (`Accept-Ranges: bytes`) SHOULD be supported on
   `.vsd` container files, so `StreamReader`-style clients can fetch
   header → trailer → INDEX → exactly the objects for page 47 (spec §9).
4. **No negotiation.** Objects have one representation (canonical CBOR).
   `Content-Type: application/vsd-object`; containers:
   `application/vsd`. Compression: objects are already small and often
   zstd-compressed inside containers; servers MAY apply HTTP gzip, and
   clients MUST verify the *decoded* bytes.
5. **Corpus-level dedup.** Because ids are global, one `/vsd/o/`
   namespace can back any number of documents: the org logo and the
   shared font subset exist once for a million invoices. `vsd migrate`'s
   dedup report estimates the saving for a given corpus.

## 3. Document assembly

A client resolving `{doc-id}` with only `/vsd/o/` access:

1. `GET /vsd/o/{doc-id}` — by convention the *manifest object* is also
   published under its own hash (the document id is `BLAKE3(manifest)`,
   so the manifest is its own object).
2. Read the manifest's refs (root, resources, metadata, render-cache,
   page-index) and fetch the closure breadth-first, verifying each
   object on arrival.
3. Strict-decode and validate as usual. Page-index-driven clients fetch
   only the pages they display.

## 4. Trust integration

- Operators SHOULD run a transparency log (`vsd-tlog`) of every
  document id they publish and serve signed tree heads; clients can then
  demand inclusion proofs out of band.
- Signatures travel inside containers (SIGS chunk); object-level
  fetching gets signature blocks from the container path or a
  `/vsd/d/{doc-id}.sigs` convenience resource (CBOR signature array).

## 5. Explicit non-goals

- **No server-side rendering.** Rendering is the client's job
  (`vsd-render`, `vsd-web`); a server that renders for you is a server
  you must trust about pixels — the exact failure mode VSD removes.
- **No mutation API.** A "changed document" is a new document with a
  `predecessor` link. Publishing is `PUT`-shaped at most, and out of
  scope here.
- **No authentication design.** This layer serves public immutable
  bytes; access control is an ordinary HTTP concern in front of it.
