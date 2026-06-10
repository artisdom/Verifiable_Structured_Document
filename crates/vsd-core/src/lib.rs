//! # vsd-core
//!
//! Core data model for **VSD — Verifiable Structured Document**: a
//! document format with the layout fidelity of PDF, the parseability of
//! HTML, the integrity model of Git, and the attack surface of a JPEG.
//!
//! This crate is container-agnostic (see `vsd-container` for the `.vsd`
//! file format) and implements:
//!
//! - **Deterministic CBOR** ([`cbor`]) — RFC 8949 §4.2 core deterministic
//!   encoding, with a strict decoder that rejects every non-canonical form
//! - **Content-addressed objects** ([`object`]) — `id = BLAKE3-256(bytes)`,
//!   immutable, deduplicated, Merkle-verifiable
//! - **The content tree** ([`tree`]) — the canonical semantic layer:
//!   real tables, mandatory alt text, reading order = tree order
//! - **Manifest & profiles** ([`manifest`]) — the document identity is
//!   `BLAKE3(manifest)`, a 32-byte commitment to every byte of content
//! - **Validation** ([`validate`]) — accessibility and structural
//!   integrity as *validity conditions*
//! - **Destructive redaction** ([`redact`]) — replace, purge, prove;
//!   black boxes over live text are unrepresentable
//! - **Forms** ([`forms`]) — a total, terminating expression language;
//!   no scripts, RE2-class regexes only
//! - **Diff** ([`diff`]) — version diffs as object-set diffs
//! - **Render layer types** ([`layout`]) — display lists and the
//!   versioned layout-engine contract
//!
//! ## Design invariants (spec §1)
//!
//! | # | Invariant |
//! |---|-----------|
//! | I1 | Structure is canonical, pixels are cache |
//! | I2 | Zero executable content in core |
//! | I3 | Every object is content-addressed |
//! | I4 | One reference renderer, conformance-tested |
//! | I5 | Cryptography over meaning, not bytes |

#![forbid(unsafe_code)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod cbor;
pub mod compose;
pub mod diff;
pub mod disclose;
pub mod document;
pub mod error;
pub mod extract;
pub mod fill;
pub mod forms;
pub mod layout;
pub mod manifest;
pub mod object;
pub mod redact;
pub mod tree;
pub mod validate;

pub use document::{Document, DocumentBuilder};
pub use error::{Error, Result};
pub use manifest::{Manifest, Metadata, Profile, ResourceTable};
pub use object::{ObjectId, ObjectStore};
pub use tree::Node;
