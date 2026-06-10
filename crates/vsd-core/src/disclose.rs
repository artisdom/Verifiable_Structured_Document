//! Selective disclosure (ROADMAP 5f, initial): reveal one subtree of a
//! document — to an auditor, a bank, a court — while proving it belongs
//! to the signed whole, *without revealing the siblings*.
//!
//! No novel cryptography: the content tree is already a Merkle
//! structure. [`seal`] materializes that by hoisting every top-level
//! block into its own object behind a `SubtreeRef`; a disclosure bundle
//! then contains only the manifest, the root skeleton (in which
//! siblings appear as 32-byte hashes), and the disclosed subtree's
//! object. Verification recomputes the hash chain up to the document id
//! — the same id a signature or a transparency-log entry commits to.
//!
//! **Privacy caveat (open, by design):** sibling hashes are unsalted in
//! this version. A sibling whose content is guessable (low entropy) can
//! be confirmed by hashing the guess. Salted subtree hashing is a
//! format addition tracked in the ROADMAP; do not use unsalted
//! disclosure where sibling *confirmation* is itself a leak.

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use crate::cbor::{MapBuilder, Value};
use crate::document::Document;
use crate::error::{Error, Result};
use crate::manifest::Manifest;
use crate::object::{ObjectId, ObjectStore};
use crate::tree::Node;

/// Hoist every top-level block of the root `doc` node into its own
/// object behind a `SubtreeRef`, producing a successor document whose
/// tree is explicitly Merkle-ized (sealing changes the root object and
/// therefore the document id; the original is the `predecessor`).
pub fn seal(doc: &Document) -> Result<Document> {
    let root = doc.root_node()?;
    let Node::Doc(mut d) = root else {
        return Err(Error::Schema("root must be a doc node".into()));
    };
    let mut store = doc.store.clone();
    for child in &mut d.children {
        if !matches!(child, Node::SubtreeRef(_)) {
            let id = store.put_value(&child.to_value()?)?;
            *child = Node::SubtreeRef(id);
        }
    }
    let new_root = store.put_value(&Node::Doc(d).to_value()?)?;
    let manifest = Manifest {
        root: new_root,
        // Layout/pagination cover the whole document; a sealed revision
        // must re-derive them.
        render_cache: None,
        page_index: None,
        predecessor: Some(doc.manifest.document_id()?),
        ..doc.manifest.clone()
    };
    let mut sealed = Document { manifest, store };
    let keep = sealed.closure()?;
    sealed.store.retain_only(&keep);
    Ok(sealed)
}

/// A disclosure bundle: everything needed to verify one subtree against
/// a document id, and nothing else.
#[derive(Debug, Clone)]
pub struct Disclosure {
    /// Document id the bundle claims membership of.
    pub doc_id: ObjectId,
    /// Child index of the disclosed block under the root doc node.
    pub index: u64,
    /// Canonical bytes: manifest, root object, disclosed subtree object.
    pub manifest_bytes: Vec<u8>,
    pub root_bytes: Vec<u8>,
    pub subtree_bytes: Vec<u8>,
}

impl Disclosure {
    pub fn to_value(&self) -> Value {
        MapBuilder::new()
            .put("t", Value::text("disclosure"))
            .put("doc-id", self.doc_id.to_value())
            .put("index", Value::Unsigned(self.index))
            .put("manifest", Value::Bytes(self.manifest_bytes.clone()))
            .put("root", Value::Bytes(self.root_bytes.clone()))
            .put("subtree", Value::Bytes(self.subtree_bytes.clone()))
            .build()
    }

    pub fn from_value(v: &Value) -> Result<Disclosure> {
        if v.get("t").and_then(Value::as_text) != Some("disclosure") {
            return Err(Error::Schema("not a disclosure bundle".into()));
        }
        let bytes = |key: &str| -> Result<Vec<u8>> {
            v.get(key)
                .and_then(Value::as_bytes)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| Error::Schema(format!("disclosure: missing {key}")))
        };
        Ok(Disclosure {
            doc_id: ObjectId::from_value(
                v.get("doc-id")
                    .ok_or_else(|| Error::Schema("disclosure: missing doc-id".into()))?,
            )?,
            index: v
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| Error::Schema("disclosure: missing index".into()))?,
            manifest_bytes: bytes("manifest")?,
            root_bytes: bytes("root")?,
            subtree_bytes: bytes("subtree")?,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.to_value().encode()
    }

    pub fn decode(bytes: &[u8]) -> Result<Disclosure> {
        Disclosure::from_value(&Value::decode(bytes)?)
    }
}

/// Create a disclosure for the top-level block at `index`. The document
/// must be sealed (the target child must be a `SubtreeRef`) — otherwise
/// the root object would reveal the siblings it inlines.
pub fn disclose(doc: &Document, index: u64) -> Result<Disclosure> {
    let root = doc.root_node()?;
    let Node::Doc(d) = &root else {
        return Err(Error::Schema("root must be a doc node".into()));
    };
    let child = d
        .children
        .get(index as usize)
        .ok_or_else(|| Error::BadNodePath(vec![index as usize]))?;
    let Node::SubtreeRef(subtree_id) = child else {
        return Err(Error::Schema(
            "block is inlined in the root; run seal() first so siblings stay hidden".into(),
        ));
    };
    Ok(Disclosure {
        doc_id: doc.manifest.document_id()?,
        index,
        manifest_bytes: doc.manifest.to_value().encode()?,
        root_bytes: doc.store.get_bytes(&doc.manifest.root)?.to_vec(),
        subtree_bytes: doc.store.get_bytes(subtree_id)?.to_vec(),
    })
}

/// The verified content of a disclosure.
pub struct Disclosed {
    pub doc_id: ObjectId,
    pub index: u64,
    pub subtree: Node,
    /// Number of sibling blocks that remain hidden (hashes only).
    pub hidden_siblings: usize,
}

/// Verify the hash chain: subtree → root skeleton → manifest → doc id.
/// On success the disclosed subtree provably belongs to `expect` (if
/// given) at the stated position.
pub fn verify_disclosure(bundle: &Disclosure, expect: Option<ObjectId>) -> Result<Disclosed> {
    // 1. The manifest bytes are the document id's preimage.
    let doc_id = ObjectId::of_bytes(&bundle.manifest_bytes);
    if doc_id != bundle.doc_id {
        return Err(Error::Validation(
            "manifest bytes do not hash to the claimed document id".into(),
        ));
    }
    if let Some(expect) = expect {
        if doc_id != expect {
            return Err(Error::Validation(format!(
                "document id mismatch: bundle proves {doc_id}, expected {expect}"
            )));
        }
    }
    // Strict decoding throughout: canonical form is enforced by the
    // store's verifying insert.
    let mut probe = ObjectStore::new();
    let manifest = Manifest::from_value(&Value::decode(&bundle.manifest_bytes)?)?;

    // 2. The root object is the one the manifest commits to.
    let root_id = probe.put_verified(bundle.root_bytes.clone(), Some(manifest.root))?;
    debug_assert_eq!(root_id, manifest.root);
    let root = Node::from_value(&probe.get_value(&root_id)?)?;
    let Node::Doc(d) = &root else {
        return Err(Error::Validation("root object is not a doc node".into()));
    };

    // 3. The child at `index` is a subtree ref whose hash the subtree
    //    bytes must match.
    let child = d
        .children
        .get(bundle.index as usize)
        .ok_or_else(|| Error::Validation("disclosed index out of range".into()))?;
    let Node::SubtreeRef(subtree_id) = child else {
        return Err(Error::Validation(
            "root child at index is not a subtree ref".into(),
        ));
    };
    probe.put_verified(bundle.subtree_bytes.clone(), Some(*subtree_id))?;
    let subtree = Node::from_value(&probe.get_value(subtree_id)?)?;

    let hidden = d
        .children
        .iter()
        .enumerate()
        .filter(|(i, c)| *i as u64 != bundle.index && matches!(c, Node::SubtreeRef(_)))
        .count();

    Ok(Disclosed {
        doc_id,
        index: bundle.index,
        subtree,
        hidden_siblings: hidden,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::Compose;

    fn doc() -> Document {
        Compose::new("en")
            .h1("Payslip")
            .para("Employee: A. Person, ID 7741.")
            .para("Salary: $123,456 per annum.")
            .para("Bank account: 12-3456-7890-00.")
            .finish()
            .unwrap()
    }

    #[test]
    fn seal_disclose_verify_roundtrip() {
        let original = doc();
        let sealed = seal(&original).unwrap();
        assert_eq!(
            sealed.manifest.predecessor,
            Some(original.document_id().unwrap())
        );
        assert!(crate::validate::validate(&sealed).is_valid());

        // Disclose only the salary line (index 2).
        let bundle = disclose(&sealed, 2).unwrap();
        let sealed_id = sealed.document_id().unwrap();

        let verified = verify_disclosure(&bundle, Some(sealed_id)).unwrap();
        assert_eq!(verified.hidden_siblings, 3);
        let text = match &verified.subtree {
            Node::Para(p) => match &p.children[0] {
                crate::tree::Inline::Text(t) => t.clone(),
                _ => panic!(),
            },
            other => panic!("unexpected node {other:?}"),
        };
        assert_eq!(text, "Salary: $123,456 per annum.");

        // The bundle bytes must NOT contain the hidden bank account.
        let encoded = bundle.encode().unwrap();
        let secret = b"12-3456-7890-00";
        assert!(
            !encoded.windows(secret.len()).any(|w| w == secret),
            "sibling content leaked into the disclosure bundle"
        );
    }

    #[test]
    fn tampered_disclosures_fail() {
        let sealed = seal(&doc()).unwrap();
        let id = sealed.document_id().unwrap();
        let bundle = disclose(&sealed, 2).unwrap();

        // Swapped-in different subtree bytes.
        let other = disclose(&sealed, 3).unwrap();
        let franken = Disclosure {
            subtree_bytes: other.subtree_bytes.clone(),
            ..disclose(&sealed, 2).unwrap()
        };
        assert!(verify_disclosure(&franken, Some(id)).is_err());

        // Lying about the position.
        let moved = Disclosure {
            index: 1,
            ..disclose(&sealed, 2).unwrap()
        };
        assert!(verify_disclosure(&moved, Some(id)).is_err());

        // Wrong expected document id.
        assert!(verify_disclosure(&bundle, Some(ObjectId([0u8; 32]))).is_err());

        // Bit flip in the subtree.
        let mut flipped = disclose(&sealed, 2).unwrap();
        let last = flipped.subtree_bytes.len() - 1;
        flipped.subtree_bytes[last] ^= 1;
        assert!(verify_disclosure(&flipped, Some(id)).is_err());
    }

    #[test]
    fn unsealed_documents_refuse_disclosure() {
        let original = doc();
        let err = disclose(&original, 2).unwrap_err();
        assert!(err.to_string().contains("seal"), "{err}");
    }
}
