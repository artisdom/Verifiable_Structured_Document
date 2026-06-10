//! Chunk framing (spec §2.2):
//!
//! ```text
//! u64   payload length (little-endian)
//! u32   chunk type (FourCC)
//! u32   flags (bit 0: critical, bit 1: zstd-compressed)
//! […]   payload
//! u64   BLAKE3-64 truncated checksum of payload (as stored)
//! ```

use crate::error::{ContainerError, Result};

/// Bytes of framing around every payload: 8 (len) + 4 (type) + 4 (flags)
/// + 8 (checksum).
pub const CHUNK_OVERHEAD: u64 = 24;

/// Sanity cap on a single chunk payload (64 GiB) — prevents a hostile
/// length field from driving allocations.
pub const MAX_CHUNK_LEN: u64 = 64 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkType(pub [u8; 4]);

impl ChunkType {
    pub const MNFST: ChunkType = ChunkType(*b"MNFS");
    pub const INDEX: ChunkType = ChunkType(*b"INDX");
    pub const OBJS: ChunkType = ChunkType(*b"OBJS");
    pub const SIGS: ChunkType = ChunkType(*b"SIGS");
    pub const TRAILR: ChunkType = ChunkType(*b"TRLR");

    pub fn as_u32(self) -> u32 {
        u32::from_le_bytes(self.0)
    }

    pub fn name(self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChunkFlags(pub u32);

impl ChunkFlags {
    pub const CRITICAL: u32 = 1 << 0;
    pub const ZSTD: u32 = 1 << 1;

    pub fn critical(self) -> bool {
        self.0 & Self::CRITICAL != 0
    }

    pub fn compressed(self) -> bool {
        self.0 & Self::ZSTD != 0
    }
}

pub struct Chunk {
    pub ctype: ChunkType,
    pub flags: ChunkFlags,
    /// Decompressed payload.
    pub payload: Vec<u8>,
}

/// Streaming zstd decode with a hard read limit: a hostile frame cannot
/// force a giant allocation (decompression-bomb guard).
#[cfg(feature = "zstd")]
pub(crate) fn decompress_payload(stored: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let decoder = zstd::Decoder::new(stored).map_err(|e| ContainerError::Zstd(e.to_string()))?;
    let mut out = Vec::new();
    decoder
        .take(MAX_CHUNK_LEN + 1)
        .read_to_end(&mut out)
        .map_err(|e| ContainerError::Zstd(e.to_string()))?;
    if out.len() as u64 > MAX_CHUNK_LEN {
        return Err(ContainerError::Structure(
            "decompressed chunk exceeds sanity cap".into(),
        ));
    }
    Ok(out)
}

/// Pure-Rust decode path (`zstd-pure`): same bomb guard, no C code —
/// this is what lets browsers and wasm runtimes read compressed `.vsd`.
#[cfg(all(not(feature = "zstd"), feature = "zstd-pure"))]
pub(crate) fn decompress_payload(stored: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let decoder = ruzstd::decoding::StreamingDecoder::new(stored)
        .map_err(|e| ContainerError::Zstd(e.to_string()))?;
    let mut out = Vec::new();
    decoder
        .take(MAX_CHUNK_LEN + 1)
        .read_to_end(&mut out)
        .map_err(|e| ContainerError::Zstd(e.to_string()))?;
    if out.len() as u64 > MAX_CHUNK_LEN {
        return Err(ContainerError::Structure(
            "decompressed chunk exceeds sanity cap".into(),
        ));
    }
    Ok(out)
}

#[cfg(not(any(feature = "zstd", feature = "zstd-pure")))]
pub(crate) fn decompress_payload(_stored: &[u8]) -> Result<Vec<u8>> {
    Err(ContainerError::ZstdUnavailable)
}

pub fn checksum(payload: &[u8]) -> u64 {
    let hash = blake3::hash(payload);
    u64::from_le_bytes(hash.as_bytes()[..8].try_into().unwrap())
}

/// Serialize a chunk, compressing if requested (and beneficial).
/// Compression requires the native `zstd` feature (`zstd-pure` is
/// decode-only).
pub fn encode_chunk(
    ctype: ChunkType,
    flags: ChunkFlags,
    payload: &[u8],
    compress: bool,
) -> Result<Vec<u8>> {
    #[cfg(feature = "zstd")]
    let (stored, flags) = if compress {
        let compressed =
            zstd::bulk::compress(payload, 9).map_err(|e| ContainerError::Zstd(e.to_string()))?;
        if compressed.len() < payload.len() {
            (compressed, ChunkFlags(flags.0 | ChunkFlags::ZSTD))
        } else {
            (payload.to_vec(), flags)
        }
    } else {
        (payload.to_vec(), flags)
    };
    #[cfg(not(feature = "zstd"))]
    let stored = if compress {
        return Err(ContainerError::ZstdUnavailable);
    } else {
        payload.to_vec()
    };

    let mut out = Vec::with_capacity(stored.len() + CHUNK_OVERHEAD as usize);
    out.extend_from_slice(&(stored.len() as u64).to_le_bytes());
    out.extend_from_slice(&ctype.as_u32().to_le_bytes());
    out.extend_from_slice(&flags.0.to_le_bytes());
    out.extend_from_slice(&stored);
    out.extend_from_slice(&checksum(&stored).to_le_bytes());
    Ok(out)
}

/// Parse the chunk at `offset`; returns the chunk and the offset just
/// past it. Verifies the checksum and decompresses.
pub fn decode_chunk(buf: &[u8], offset: u64) -> Result<(Chunk, u64)> {
    let off = offset as usize;
    let head = buf
        .get(off..off + 16)
        .ok_or_else(|| ContainerError::Structure("truncated chunk header".into()))?;
    let len = u64::from_le_bytes(head[0..8].try_into().unwrap());
    if len > MAX_CHUNK_LEN {
        return Err(ContainerError::Structure(format!(
            "chunk length {len} exceeds sanity cap"
        )));
    }
    let ctype = ChunkType(head[8..12].try_into().unwrap());
    let flags = ChunkFlags(u32::from_le_bytes(head[12..16].try_into().unwrap()));

    let body_start = off + 16;
    let body_end = body_start
        .checked_add(len as usize)
        .filter(|&e| e + 8 <= buf.len())
        .ok_or_else(|| {
            ContainerError::Structure(format!("chunk {} extends past EOF", ctype.name()))
        })?;
    let stored = &buf[body_start..body_end];
    let claimed = u64::from_le_bytes(buf[body_end..body_end + 8].try_into().unwrap());
    if checksum(stored) != claimed {
        return Err(ContainerError::ChecksumMismatch {
            fourcc: ctype.name(),
        });
    }

    let payload = if flags.compressed() {
        decompress_payload(stored)?
    } else {
        stored.to_vec()
    };

    Ok((
        Chunk {
            ctype,
            flags,
            payload,
        },
        (body_end + 8) as u64,
    ))
}
