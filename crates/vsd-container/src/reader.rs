//! Reading and verifying `.vsd` files.
//!
//! The reader is strict by default: every chunk checksum is verified,
//! every object's bytes must be canonical CBOR hashing to its claimed
//! index id, and the declared file size must match reality. "Be liberal
//! in what you accept" is how PDF became a malware format; VSD readers
//! reject, loudly and early.

use std::collections::BTreeMap;
use std::path::Path;

use vsd_core::manifest::Manifest;
use vsd_core::object::{ObjectId, ObjectStore};
use vsd_core::Document;

use crate::chunk::{decode_chunk, Chunk, ChunkType};
use crate::error::{ContainerError, Result};
use crate::sig::Signature;
use crate::writer::parse_index;
use crate::{FORMAT_MAJOR, HEADER_LEN, MAGIC};

#[derive(Clone, Debug, Default)]
pub struct ReadOptions {
    /// Permit unknown non-critical chunk types (forward compatibility
    /// within a major version). Unknown *critical* chunks always fail.
    pub allow_unknown_chunks: bool,
}

/// A parsed `.vsd` file: the document plus container-level artifacts.
pub struct VsdFile {
    pub document: Document,
    pub signatures: Vec<Signature>,
    /// Document id as claimed by the trailer (verified against the
    /// manifest before you ever see this struct).
    pub document_id: ObjectId,
    pub format_version: (u16, u16),
}

pub fn read_file(path: impl AsRef<Path>, opts: &ReadOptions) -> Result<VsdFile> {
    let bytes = std::fs::read(path)?;
    read_document(&bytes, opts)
}

pub fn read_document(buf: &[u8], opts: &ReadOptions) -> Result<VsdFile> {
    // --- Header (spec §2.1) ------------------------------------------------
    if buf.len() < HEADER_LEN as usize {
        return Err(ContainerError::Structure("file shorter than header".into()));
    }
    if buf[..8] != MAGIC {
        return Err(ContainerError::BadMagic);
    }
    let major = u16::from_le_bytes(buf[8..10].try_into().unwrap());
    let minor = u16::from_le_bytes(buf[10..12].try_into().unwrap());
    if major != FORMAT_MAJOR {
        return Err(ContainerError::UnsupportedMajor(major, FORMAT_MAJOR));
    }
    let declared_size = u64::from_le_bytes(buf[16..24].try_into().unwrap());
    if declared_size != buf.len() as u64 {
        return Err(ContainerError::SizeMismatch {
            expected: declared_size,
            actual: buf.len() as u64,
        });
    }
    let trailer_offset = u64::from_le_bytes(buf[24..32].try_into().unwrap());

    // --- Walk all chunks in order, verifying checksums ---------------------
    let mut offset = HEADER_LEN;
    let mut mnfst: Option<Chunk> = None;
    let mut index: Option<Chunk> = None;
    let mut objs: Vec<(u64, Chunk)> = Vec::new();
    let mut sigs: Vec<Chunk> = Vec::new();
    let mut trailer: Option<(u64, Chunk)> = None;

    while offset < buf.len() as u64 {
        let chunk_offset = offset;
        let (chunk, next) = decode_chunk(buf, offset)?;
        match chunk.ctype {
            ChunkType::MNFST => {
                if mnfst.is_some() {
                    return Err(ContainerError::Structure("duplicate MNFST chunk".into()));
                }
                mnfst = Some(chunk);
            }
            ChunkType::INDEX => {
                if index.is_some() {
                    return Err(ContainerError::Structure("duplicate INDEX chunk".into()));
                }
                index = Some(chunk);
            }
            ChunkType::OBJS => objs.push((chunk_offset, chunk)),
            ChunkType::SIGS => sigs.push(chunk),
            ChunkType::TRAILR => {
                if trailer.is_some() {
                    return Err(ContainerError::Structure("duplicate TRAILR chunk".into()));
                }
                trailer = Some((chunk_offset, chunk));
            }
            unknown => {
                if chunk.flags.critical() {
                    return Err(ContainerError::UnknownCritical(unknown.name()));
                }
                if !opts.allow_unknown_chunks {
                    return Err(ContainerError::Structure(format!(
                        "unknown chunk {} (pass allow_unknown_chunks to skip)",
                        unknown.name()
                    )));
                }
            }
        }
        offset = next;
    }

    let mnfst = mnfst.ok_or_else(|| ContainerError::Structure("missing MNFST chunk".into()))?;
    let index = index.ok_or_else(|| ContainerError::Structure("missing INDEX chunk".into()))?;
    let (trailer_pos, trailer) =
        trailer.ok_or_else(|| ContainerError::Structure("missing TRAILR chunk".into()))?;
    if objs.is_empty() {
        return Err(ContainerError::Structure("missing OBJS chunk".into()));
    }
    if trailer_pos != trailer_offset {
        return Err(ContainerError::Structure(format!(
            "header points trailer at {trailer_offset}, found at {trailer_pos}"
        )));
    }

    // --- Manifest & document id --------------------------------------------
    let manifest_value = vsd_core::cbor::Value::decode(&mnfst.payload)?;
    // Canonical-form check: MNFST bytes must be the unique encoding,
    // because the document id is their hash.
    let reencoded = manifest_value.encode()?;
    if reencoded != mnfst.payload {
        return Err(ContainerError::Structure(
            "MNFST payload is not canonical CBOR".into(),
        ));
    }
    let manifest = Manifest::from_value(&manifest_value)?;
    let document_id = ObjectId::of_bytes(&mnfst.payload);

    // Trailer cross-checks.
    let trailer_value = vsd_core::cbor::Value::decode(&trailer.payload)?;
    if let Some(claimed) = trailer_value.get("doc-id").and_then(|v| v.as_bytes()) {
        if claimed != document_id.as_slice() {
            return Err(ContainerError::Structure(
                "trailer doc-id does not match BLAKE3(MNFST)".into(),
            ));
        }
    }

    // --- Object store: load every indexed object, verifying hashes ---------
    let placements = parse_index(&index.payload)?;
    let objs_by_offset: BTreeMap<u64, &Chunk> = objs.iter().map(|(off, c)| (*off, c)).collect();
    let mut store = ObjectStore::new();
    for (id, (chunk_off, intra, len, codec)) in &placements {
        if *codec != 0 {
            return Err(ContainerError::Structure(format!(
                "object {id}: unknown codec {codec}"
            )));
        }
        let chunk = objs_by_offset.get(chunk_off).ok_or_else(|| {
            ContainerError::Structure(format!("object {id}: no OBJS chunk at offset {chunk_off}"))
        })?;
        let start = usize::try_from(*intra)
            .ok()
            .filter(|&s| s <= chunk.payload.len())
            .ok_or_else(|| ContainerError::Structure(format!("object {id}: bad intra offset")))?;
        let end = start
            .checked_add(*len as usize)
            .filter(|&e| e <= chunk.payload.len())
            .ok_or_else(|| ContainerError::Structure(format!("object {id}: bad length")))?;
        // put_verified re-derives the hash and enforces canonical form —
        // a flipped bit anywhere in OBJS dies here even before the chunk
        // checksum is consulted.
        store.put_verified(chunk.payload[start..end].to_vec(), Some(*id))?;
    }

    // --- Signatures ----------------------------------------------------------
    let mut signatures = Vec::new();
    for s in &sigs {
        signatures.extend(Signature::block_from_payload(&s.payload)?);
    }

    Ok(VsdFile {
        document: Document { manifest, store },
        signatures,
        document_id,
        format_version: (major, minor),
    })
}
