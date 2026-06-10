//! Writing `.vsd` files (spec §2).
//!
//! Chunk order is normative: HEADER, MNFST, INDEX, OBJS…, SIGS?, TRAILR.
//! The INDEX precedes the OBJS chunks it describes (streaming readers see
//! the map before the territory), so the writer lays out object offsets
//! before emitting bytes.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use vsd_core::cbor::{MapBuilder, Value};
use vsd_core::object::ObjectId;
use vsd_core::Document;

use crate::chunk::{encode_chunk, ChunkFlags, ChunkType, CHUNK_OVERHEAD};
use crate::error::{ContainerError, Result};
use crate::sig::Signature;
use crate::{profile_flags, FORMAT_MAJOR, FORMAT_MINOR, HEADER_LEN, MAGIC};

#[derive(Clone, Debug)]
pub struct WriteOptions {
    /// zstd-compress the OBJS chunk when it helps.
    pub compress: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            compress: cfg!(feature = "zstd"),
        }
    }
}

/// Serialize a document (plus detached signatures) to `.vsd` bytes.
pub fn write_document(
    doc: &Document,
    signatures: &[Signature],
    opts: &WriteOptions,
) -> Result<Vec<u8>> {
    let manifest_bytes = doc.manifest.to_value().encode()?;

    // --- Plan the object store layout ------------------------------------
    // One OBJS chunk; objects concatenated in id order (deterministic
    // file bytes for a given document and options). The INDEX maps
    // object_id → (chunk_offset, intra_offset, length, codec); intra
    // offsets refer to the *decompressed* chunk payload.
    let mut objs_payload = Vec::with_capacity(doc.store.total_bytes() as usize);
    let mut placements: BTreeMap<ObjectId, (u64, u64)> = BTreeMap::new(); // id → (intra, len)
    for (id, bytes) in doc.store.iter() {
        placements.insert(*id, (objs_payload.len() as u64, bytes.len() as u64));
        objs_payload.extend_from_slice(bytes);
    }

    // --- Encode chunks in order, computing offsets ------------------------
    let mnfst_chunk = encode_chunk(
        ChunkType::MNFST,
        ChunkFlags(ChunkFlags::CRITICAL),
        &manifest_bytes,
        false, // the manifest is small and its bytes are the document id's preimage; keep raw
    )?;

    // OBJS chunk must be encoded before INDEX (compression changes
    // nothing for the index — intra offsets are decompressed-relative —
    // but we need its final size only after INDEX, whose own size we
    // need first… so compute INDEX size with the OBJS offset as unknown
    // by laying out in two passes).
    let objs_chunk = encode_chunk(
        ChunkType::OBJS,
        ChunkFlags(ChunkFlags::CRITICAL),
        &objs_payload,
        opts.compress,
    )?;

    // INDEX payload references the absolute file offset of the OBJS
    // chunk, which depends on the INDEX chunk's own length. The entry
    // values are fixed-width by construction (u64s in CBOR vary, so we
    // resolve by fixpoint: encode with a candidate offset, recompute).
    let header_len = HEADER_LEN;
    let mut objs_offset_guess = header_len + mnfst_chunk.len() as u64; // + index len, iterate
    let index_payload = loop {
        let payload = index_payload(&placements, objs_offset_guess)?;
        let index_len = payload.len() as u64 + CHUNK_OVERHEAD;
        let resolved = header_len + mnfst_chunk.len() as u64 + index_len;
        if resolved == objs_offset_guess {
            break payload;
        }
        objs_offset_guess = resolved;
    };
    let index_chunk = encode_chunk(
        ChunkType::INDEX,
        ChunkFlags(ChunkFlags::CRITICAL),
        &index_payload,
        false,
    )?;

    let mnfst_offset = header_len;
    let index_offset = mnfst_offset + mnfst_chunk.len() as u64;
    let objs_offset = index_offset + index_chunk.len() as u64;
    debug_assert_eq!(objs_offset, objs_offset_guess);

    let sigs_chunk = if signatures.is_empty() {
        None
    } else {
        Some(encode_chunk(
            ChunkType::SIGS,
            ChunkFlags(ChunkFlags::CRITICAL),
            &Signature::block_to_payload(signatures)?,
            false,
        )?)
    };

    let trailer_offset = objs_offset
        + objs_chunk.len() as u64
        + sigs_chunk.as_ref().map(|c| c.len() as u64).unwrap_or(0);

    let doc_id = doc.document_id()?;
    let trailer_payload = MapBuilder::new()
        .put("index-offset", Value::Unsigned(index_offset))
        .put("mnfst-offset", Value::Unsigned(mnfst_offset))
        .put("doc-id", Value::Bytes(doc_id.as_slice().to_vec()))
        .build()
        .encode()?;
    let trailer_chunk = encode_chunk(
        ChunkType::TRAILR,
        ChunkFlags(ChunkFlags::CRITICAL),
        &trailer_payload,
        false,
    )?;

    let total_size = trailer_offset + trailer_chunk.len() as u64;

    // --- Header (spec §2.1) ------------------------------------------------
    let mut out = Vec::with_capacity(total_size as usize);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_MAJOR.to_le_bytes());
    out.extend_from_slice(&FORMAT_MINOR.to_le_bytes());
    let pflags = match doc.manifest.profile {
        vsd_core::Profile::Core => profile_flags::CORE,
        vsd_core::Profile::Archive => profile_flags::CORE | profile_flags::ARCHIVE,
        vsd_core::Profile::Form => profile_flags::CORE | profile_flags::FORM,
        vsd_core::Profile::Stream => profile_flags::CORE | profile_flags::STREAM,
    };
    out.extend_from_slice(&pflags.to_le_bytes());
    out.extend_from_slice(&total_size.to_le_bytes());
    out.extend_from_slice(&trailer_offset.to_le_bytes());
    debug_assert_eq!(out.len() as u64, HEADER_LEN);

    out.extend_from_slice(&mnfst_chunk);
    out.extend_from_slice(&index_chunk);
    out.extend_from_slice(&objs_chunk);
    if let Some(s) = &sigs_chunk {
        out.extend_from_slice(s);
    }
    out.extend_from_slice(&trailer_chunk);
    debug_assert_eq!(out.len() as u64, total_size);
    Ok(out)
}

fn index_payload(placements: &BTreeMap<ObjectId, (u64, u64)>, objs_offset: u64) -> Result<Vec<u8>> {
    let entries: Vec<(Value, Value)> = placements
        .iter()
        .map(|(id, (intra, len))| {
            (
                Value::Bytes(id.as_slice().to_vec()),
                Value::Array(vec![
                    Value::Unsigned(objs_offset),
                    Value::Unsigned(*intra),
                    Value::Unsigned(*len),
                    Value::Unsigned(0), // codec 0: raw canonical CBOR
                ]),
            )
        })
        .collect();
    Ok(Value::Map(entries).encode()?)
}

/// Write a document to a file path.
pub fn write_file(
    path: impl AsRef<Path>,
    doc: &Document,
    signatures: &[Signature],
    opts: &WriteOptions,
) -> Result<()> {
    let bytes = write_document(doc, signatures, opts)?;
    let mut f = std::fs::File::create(path)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    Ok(())
}

// Re-exported for reader-side use.
pub(crate) fn parse_index(payload: &[u8]) -> Result<BTreeMap<ObjectId, (u64, u64, u64, u64)>> {
    let v = Value::decode(payload)?;
    let mut out = BTreeMap::new();
    for (k, entry) in v
        .as_map()
        .ok_or_else(|| ContainerError::Structure("INDEX payload must be a map".into()))?
    {
        let id = ObjectId::from_value(k)?;
        let a = entry
            .as_array()
            .filter(|a| a.len() == 4)
            .ok_or_else(|| ContainerError::Structure("INDEX entry must be a 4-tuple".into()))?;
        let nums: Vec<u64> = a
            .iter()
            .map(|x| {
                x.as_u64()
                    .ok_or_else(|| ContainerError::Structure("INDEX entry must be uints".into()))
            })
            .collect::<Result<_>>()?;
        out.insert(id, (nums[0], nums[1], nums[2], nums[3]));
    }
    Ok(out)
}
