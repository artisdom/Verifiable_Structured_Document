//! # vsd-container
//!
//! The `.vsd` on-disk container (spec §2): a sequence of length-prefixed
//! chunks with 64-bit lengths, BLAKE3 payload checksums, and mandatory
//! ordering:
//!
//! ```text
//! HEADER (32 bytes) · MNFST · INDEX · OBJS… · SIGS? · TRAILR
//! ```
//!
//! The container is *transport*: repacking, recompressing, or reordering
//! chunks never changes the document identity, because identity is
//! `BLAKE3(manifest)` over the canonical content (spec §2.4) — that is
//! what signatures cover, not byte ranges.

#![forbid(unsafe_code)]

mod chunk;
mod error;
mod reader;
mod sig;
mod stream;
mod writer;

pub use chunk::{ChunkFlags, ChunkType, CHUNK_OVERHEAD};
pub use error::{ContainerError, Result};
pub use reader::{read_document, read_file, ReadOptions, VsdFile};
pub use sig::{SigAlg, SigScope, Signature};
pub use stream::{RangeSource, StreamReader};
pub use writer::{write_document, write_file, WriteOptions};

/// PNG-style magic: catches FTP/text-mode corruption (spec §2.1).
pub const MAGIC: [u8; 8] = [0x89, b'V', b'S', b'D', 0x0d, 0x0a, 0x1a, 0x0a];

pub const FORMAT_MAJOR: u16 = 0;
pub const FORMAT_MINOR: u16 = 1;

/// Fixed header size (spec §2.1).
pub const HEADER_LEN: u64 = 32;

/// Profile flag bits in the header bitfield.
pub mod profile_flags {
    pub const CORE: u32 = 1 << 0;
    pub const ARCHIVE: u32 = 1 << 1;
    pub const FORM: u32 = 1 << 2;
    pub const STREAM: u32 = 1 << 3;
}
