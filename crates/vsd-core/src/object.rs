//! Content-addressed object model (spec §2.3).
//!
//! Every resource in a VSD — content-tree nodes, images, font subsets,
//! render caches, metadata — is an immutable *object*:
//! `object_id = BLAKE3-256(canonical_encoding(object))`.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use crate::cbor::Value;
use crate::error::{Error, Result};

/// BLAKE3-256 of an object's canonical CBOR encoding.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(pub [u8; 32]);

impl ObjectId {
    pub fn of_bytes(bytes: &[u8]) -> ObjectId {
        ObjectId(*blake3::hash(bytes).as_bytes())
    }

    pub fn of_value(value: &Value) -> Result<ObjectId> {
        Ok(ObjectId::of_bytes(&value.encode()?))
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    pub fn to_value(self) -> Value {
        Value::Bytes(self.0.to_vec())
    }

    pub fn from_value(v: &Value) -> Result<ObjectId> {
        let b = v
            .as_bytes()
            .ok_or_else(|| Error::Schema("object-ref must be a byte string".into()))?;
        let arr: [u8; 32] = b
            .try_into()
            .map_err(|_| Error::Schema("object-ref must be exactly 32 bytes".into()))?;
        Ok(ObjectId(arr))
    }

    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({}…)", &hex::encode(self.0)[..12])
    }
}

impl FromStr for ObjectId {
    type Err = Error;

    fn from_str(s: &str) -> Result<ObjectId> {
        let bytes = hex::decode(s).map_err(|_| Error::Schema("invalid hex object id".into()))?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| Error::Schema("object id must be 32 bytes (64 hex chars)".into()))?;
        Ok(ObjectId(arr))
    }
}

/// An in-memory content-addressed store of canonical CBOR objects.
///
/// Objects are immutable: insertion is by content, and the id is derived
/// from the bytes. The store never contains two objects with the same id
/// and different bytes (the id *is* the bytes, cryptographically).
#[derive(Clone, Default)]
pub struct ObjectStore {
    objects: BTreeMap<ObjectId, Vec<u8>>,
}

impl ObjectStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a value, returning its content address.
    pub fn put_value(&mut self, value: &Value) -> Result<ObjectId> {
        let bytes = value.encode()?;
        let id = ObjectId::of_bytes(&bytes);
        self.objects.insert(id, bytes);
        Ok(id)
    }

    /// Insert raw bytes that are *claimed* to be a canonical encoding.
    /// The claim is checked: the bytes must strictly decode, re-encode to
    /// themselves, and hash to `claimed` if provided.
    pub fn put_verified(&mut self, bytes: Vec<u8>, claimed: Option<ObjectId>) -> Result<ObjectId> {
        let value = Value::decode(&bytes)?;
        let reencoded = value.encode()?;
        if reencoded != bytes {
            return Err(Error::Cbor("object bytes are not in canonical form".into()));
        }
        let id = ObjectId::of_bytes(&bytes);
        if let Some(c) = claimed {
            if c != id {
                return Err(Error::HashMismatch { claimed: c, actual: id });
            }
        }
        self.objects.insert(id, bytes);
        Ok(id)
    }

    pub fn get_bytes(&self, id: &ObjectId) -> Result<&[u8]> {
        self.objects
            .get(id)
            .map(Vec::as_slice)
            .ok_or(Error::ObjectNotFound(*id))
    }

    pub fn get_value(&self, id: &ObjectId) -> Result<Value> {
        Value::decode(self.get_bytes(id)?)
    }

    pub fn contains(&self, id: &ObjectId) -> bool {
        self.objects.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn ids(&self) -> impl Iterator<Item = &ObjectId> {
        self.objects.keys()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ObjectId, &[u8])> {
        self.objects.iter().map(|(k, v)| (k, v.as_slice()))
    }

    pub fn total_bytes(&self) -> u64 {
        self.objects.values().map(|v| v.len() as u64).sum()
    }

    /// Remove every object whose id is not in `keep`. Returns removed ids.
    /// This is the purge step of redaction (spec §7.2): objects no longer
    /// referenced by any manifest MUST NOT survive in the store.
    pub fn retain_only(&mut self, keep: &std::collections::BTreeSet<ObjectId>) -> Vec<ObjectId> {
        let doomed: Vec<ObjectId> = self
            .objects
            .keys()
            .filter(|id| !keep.contains(id))
            .copied()
            .collect();
        for id in &doomed {
            self.objects.remove(id);
        }
        doomed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cbor::MapBuilder;

    #[test]
    fn id_is_stable_across_insertion_order() {
        // Same logical map, different builder order → same id.
        let a = MapBuilder::new()
            .put("x", Value::Unsigned(1))
            .put("a", Value::text("hello"))
            .build();
        let b = MapBuilder::new()
            .put("a", Value::text("hello"))
            .put("x", Value::Unsigned(1))
            .build();
        assert_eq!(ObjectId::of_value(&a).unwrap(), ObjectId::of_value(&b).unwrap());
    }

    #[test]
    fn put_verified_rejects_noncanonical() {
        let mut store = ObjectStore::new();
        // Non-minimal integer: 10 with one-byte argument.
        assert!(store.put_verified(vec![0x18, 0x0a], None).is_err());
        // Canonical bytes round-trip fine.
        let id = store.put_verified(vec![0x0a], None).unwrap();
        assert_eq!(store.get_value(&id).unwrap(), Value::Unsigned(10));
    }

    #[test]
    fn put_verified_rejects_wrong_claim() {
        let mut store = ObjectStore::new();
        let wrong = ObjectId([0u8; 32]);
        assert!(matches!(
            store.put_verified(vec![0x0a], Some(wrong)),
            Err(Error::HashMismatch { .. })
        ));
    }
}
