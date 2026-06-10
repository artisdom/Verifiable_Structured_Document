//! Streaming / ranged access (spec §9).
//!
//! A [`StreamReader`] opens a `.vsd` without reading the whole file:
//!
//! 1. header (32 bytes) → locate the trailer
//! 2. trailer → locate INDEX and MNFST
//! 3. INDEX + MNFST → document identity and the object map
//! 4. objects load lazily, one range read each, verified by hash on load
//!
//! [`RangeSource`] abstracts "a thing that serves byte ranges": a local
//! file, an HTTP server with `Range:` support, a CDN of immutable
//! objects. Implement it for your transport; the verification logic is
//! identical everywhere. Note that object-level BLAKE3 verification
//! replaces whole-chunk checksum verification in this mode — each object
//! is still individually tamper-evident, which is the stronger of the
//! two guarantees.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

use vsd_core::cbor::Value;
use vsd_core::manifest::Manifest;
use vsd_core::object::ObjectId;

use crate::chunk::{ChunkFlags, ChunkType, MAX_CHUNK_LEN};
use crate::error::{ContainerError, Result};
use crate::writer::parse_index;
use crate::{FORMAT_MAJOR, HEADER_LEN, MAGIC};

/// A byte-range server. `read_at` must fill `buf` completely from
/// `offset` or fail.
pub trait RangeSource {
    fn len(&mut self) -> Result<u64>;
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()>;

    fn is_empty(&mut self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// Any `Read + Seek` (e.g. `std::fs::File`) is a range source.
impl<T: Read + Seek> RangeSource for T {
    fn len(&mut self) -> Result<u64> {
        Ok(self.seek(SeekFrom::End(0))?)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.seek(SeekFrom::Start(offset))?;
        self.read_exact(buf)?;
        Ok(())
    }
}

struct ChunkLocation {
    payload_offset: u64,
    payload_len: u64,
    flags: ChunkFlags,
}

/// Lazy, verifying reader over a range source.
pub struct StreamReader<S: RangeSource> {
    source: S,
    manifest: Manifest,
    document_id: ObjectId,
    /// object id → (chunk file offset, intra offset, length, codec)
    placements: BTreeMap<ObjectId, (u64, u64, u64, u64)>,
    /// chunk file offset → location of its payload
    chunks: BTreeMap<u64, ChunkLocation>,
    /// Decompressed payload cache for compressed chunks (a compressed
    /// chunk cannot be sub-ranged; it is fetched once on first touch).
    decompressed: BTreeMap<u64, Vec<u8>>,
}

impl<S: RangeSource> StreamReader<S> {
    pub fn open(mut source: S) -> Result<Self> {
        let file_len = source.len()?;
        if file_len < HEADER_LEN {
            return Err(ContainerError::Structure("file shorter than header".into()));
        }
        let mut header = [0u8; HEADER_LEN as usize];
        source.read_at(0, &mut header)?;
        if header[..8] != MAGIC {
            return Err(ContainerError::BadMagic);
        }
        let major = u16::from_le_bytes(header[8..10].try_into().unwrap());
        if major != FORMAT_MAJOR {
            return Err(ContainerError::UnsupportedMajor(major, FORMAT_MAJOR));
        }
        let declared = u64::from_le_bytes(header[16..24].try_into().unwrap());
        if declared != file_len {
            return Err(ContainerError::SizeMismatch {
                expected: declared,
                actual: file_len,
            });
        }
        let trailer_offset = u64::from_le_bytes(header[24..32].try_into().unwrap());

        // Trailer → index & manifest offsets.
        let trailer = read_chunk_at(&mut source, trailer_offset, file_len)?;
        if trailer.0 != ChunkType::TRAILR {
            return Err(ContainerError::Structure(
                "header trailer offset does not point at a TRLR chunk".into(),
            ));
        }
        let tv = Value::decode(&trailer.1)?;
        let need = |key: &str| -> Result<u64> {
            tv.get(key)
                .and_then(Value::as_u64)
                .ok_or_else(|| ContainerError::Structure(format!("trailer: missing {key}")))
        };
        let index_offset = need("index-offset")?;
        let mnfst_offset = need("mnfst-offset")?;

        // Manifest: canonical-form enforced, identity derived.
        let mnfst = read_chunk_at(&mut source, mnfst_offset, file_len)?;
        if mnfst.0 != ChunkType::MNFST {
            return Err(ContainerError::Structure("expected MNFST chunk".into()));
        }
        let manifest_value = Value::decode(&mnfst.1)?;
        if manifest_value.encode()? != mnfst.1 {
            return Err(ContainerError::Structure(
                "MNFST payload is not canonical CBOR".into(),
            ));
        }
        let manifest = Manifest::from_value(&manifest_value)?;
        let document_id = ObjectId::of_bytes(&mnfst.1);
        if let Some(claimed) = tv.get("doc-id").and_then(Value::as_bytes) {
            if claimed != document_id.as_slice() {
                return Err(ContainerError::Structure(
                    "trailer doc-id does not match BLAKE3(MNFST)".into(),
                ));
            }
        }

        // Index → object placements; chunk headers resolve lazily.
        let index = read_chunk_at(&mut source, index_offset, file_len)?;
        if index.0 != ChunkType::INDEX {
            return Err(ContainerError::Structure("expected INDEX chunk".into()));
        }
        let placements = parse_index(&index.1)?;

        Ok(StreamReader {
            source,
            manifest,
            document_id,
            placements,
            chunks: BTreeMap::new(),
            decompressed: BTreeMap::new(),
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn document_id(&self) -> ObjectId {
        self.document_id
    }

    pub fn object_count(&self) -> usize {
        self.placements.len()
    }

    pub fn contains(&self, id: &ObjectId) -> bool {
        self.placements.contains_key(id)
    }

    /// Fetch one object by content address: a single range read for
    /// uncompressed chunks. Bytes are verified (canonical CBOR, BLAKE3
    /// = id) before being returned — a lying server cannot substitute
    /// content.
    pub fn object(&mut self, id: &ObjectId) -> Result<Value> {
        let (chunk_off, intra, len, codec) = *self
            .placements
            .get(id)
            .ok_or(vsd_core::Error::ObjectNotFound(*id))?;
        if codec != 0 {
            return Err(ContainerError::Structure(format!(
                "object {id}: unknown codec {codec}"
            )));
        }
        let bytes = self.object_bytes(chunk_off, intra, len, id)?;
        // Hash + canonical-form verification, exactly as the eager reader.
        let mut probe = vsd_core::object::ObjectStore::new();
        probe.put_verified(bytes, Some(*id))?;
        probe.get_value(id).map_err(Into::into)
    }

    fn object_bytes(
        &mut self,
        chunk_off: u64,
        intra: u64,
        len: u64,
        id: &ObjectId,
    ) -> Result<Vec<u8>> {
        let file_len = self.source.len()?;
        if !self.chunks.contains_key(&chunk_off) {
            let loc = read_chunk_header(&mut self.source, chunk_off, file_len)?;
            if loc.0 != ChunkType::OBJS {
                return Err(ContainerError::Structure(format!(
                    "object {id}: chunk at {chunk_off} is not OBJS"
                )));
            }
            self.chunks.insert(chunk_off, loc.1);
        }
        let loc = &self.chunks[&chunk_off];

        if loc.flags.compressed() {
            if !self.decompressed.contains_key(&chunk_off) {
                let mut stored = vec![0u8; loc.payload_len as usize];
                self.source.read_at(loc.payload_offset, &mut stored)?;
                let payload = crate::chunk::decompress_payload(&stored)?;
                self.decompressed.insert(chunk_off, payload);
            }
            let payload = &self.decompressed[&chunk_off];
            let start = usize::try_from(intra)
                .ok()
                .filter(|&s| s <= payload.len())
                .ok_or_else(|| ContainerError::Structure(format!("object {id}: bad offset")))?;
            let end = start
                .checked_add(len as usize)
                .filter(|&e| e <= payload.len())
                .ok_or_else(|| ContainerError::Structure(format!("object {id}: bad length")))?;
            Ok(payload[start..end].to_vec())
        } else {
            let start = loc
                .payload_offset
                .checked_add(intra)
                .filter(|&s| s + len <= loc.payload_offset + loc.payload_len)
                .ok_or_else(|| ContainerError::Structure(format!("object {id}: bad offset")))?;
            let mut buf = vec![0u8; len as usize];
            self.source.read_at(start, &mut buf)?;
            Ok(buf)
        }
    }

    /// All object ids needed for page `page` (0-based), per the
    /// page-index object. The closure can then be fetched one object at
    /// a time — or batched by a smarter transport.
    pub fn page_closure(&mut self, page: usize) -> Result<Vec<ObjectId>> {
        let pi_id = self
            .manifest
            .page_index
            .ok_or_else(|| ContainerError::Structure("document has no page-index object".into()))?;
        let pi = vsd_core::manifest::PageIndex::from_value(&self.object(&pi_id)?)?;
        pi.pages
            .get(page)
            .cloned()
            .ok_or_else(|| ContainerError::Structure(format!("page {page} out of range")))
    }
}

type ChunkHeader = (ChunkType, ChunkLocation);

fn read_chunk_header<S: RangeSource>(
    source: &mut S,
    offset: u64,
    file_len: u64,
) -> Result<ChunkHeader> {
    if offset + 16 > file_len {
        return Err(ContainerError::Structure("chunk header past EOF".into()));
    }
    let mut head = [0u8; 16];
    source.read_at(offset, &mut head)?;
    let len = u64::from_le_bytes(head[0..8].try_into().unwrap());
    if len > MAX_CHUNK_LEN || offset + 16 + len + 8 > file_len {
        return Err(ContainerError::Structure("chunk extends past EOF".into()));
    }
    let ctype = ChunkType(head[8..12].try_into().unwrap());
    let flags = ChunkFlags(u32::from_le_bytes(head[12..16].try_into().unwrap()));
    Ok((
        ctype,
        ChunkLocation {
            payload_offset: offset + 16,
            payload_len: len,
            flags,
        },
    ))
}

/// Read a full small chunk (trailer, manifest, index) with checksum
/// verification.
fn read_chunk_at<S: RangeSource>(
    source: &mut S,
    offset: u64,
    file_len: u64,
) -> Result<(ChunkType, Vec<u8>)> {
    let (ctype, loc) = read_chunk_header(source, offset, file_len)?;
    let mut stored = vec![0u8; loc.payload_len as usize];
    source.read_at(loc.payload_offset, &mut stored)?;
    let mut check = [0u8; 8];
    source.read_at(loc.payload_offset + loc.payload_len, &mut check)?;
    if crate::chunk::checksum(&stored) != u64::from_le_bytes(check) {
        return Err(ContainerError::ChecksumMismatch {
            fourcc: ctype.name(),
        });
    }
    let payload = if loc.flags.compressed() {
        crate::chunk::decompress_payload(&stored)?
    } else {
        stored
    };
    Ok((ctype, payload))
}
