//! Destructive redaction (spec §7.2).
//!
//! Redaction is an operation *defined by the format*, not left to tools:
//!
//! 1. the target subtree is **replaced** by a `redacted` node,
//! 2. objects no longer referenced by the manifest are **purged**,
//! 3. the render cache is **invalidated** (it must be recomputed by a
//!    layout engine before the document can claim a cache again — a
//!    stale cache is exactly where PDF redaction leaks live),
//! 4. a redaction proof — BLAKE3 of the removed subtree's canonical
//!    encoding — is recorded in the replacement node, so an escrowed
//!    original can later be matched against what was removed.
//!
//! Drawing a black box over text is not representable in this format.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::document::Document;
use crate::error::{Error, Result};
use crate::manifest::Manifest;
use crate::object::ObjectId;
use crate::tree::{Node, Redacted};

/// Outcome of a redaction.
pub struct Redaction {
    /// The new document (new manifest, purged store).
    pub document: Document,
    /// BLAKE3 of the removed subtree's canonical encoding.
    pub proof: [u8; 32],
    /// Object ids purged from the store.
    pub purged: Vec<ObjectId>,
    /// True if a render cache existed and was invalidated.
    pub cache_invalidated: bool,
}

/// Redact the node at `path` (child indices from the root `doc` node,
/// descending through section children, list items, and table cells in
/// reading order). Returns a new document; the input is untouched —
/// objects are immutable, so "editing" produces new objects and a new
/// manifest that records the original as its `predecessor`.
pub fn redact(doc: &Document, path: &[usize], reason: Option<String>) -> Result<Redaction> {
    if path.is_empty() {
        return Err(Error::BadNodePath(vec![]));
    }
    let mut root = doc.root_node()?;
    let removed = replace_at(&mut root, doc, path, path, &reason)?;
    let proof: [u8; 32] = *blake3::hash(&removed.to_value()?.encode()?).as_bytes();

    // Rebuild: new root object, same resources/metadata, render cache
    // dropped (mandatory recompute), predecessor chain recorded.
    let mut store = doc.store.clone();
    let new_root = store.put_value(&root.to_value()?)?;
    let cache_invalidated = doc.manifest.render_cache.is_some();
    let manifest = Manifest {
        root: new_root,
        render_cache: None,
        page_index: None, // derived from the cache; dropped with it
        // Filled values may quote the redacted content; they do not
        // survive a redaction. Re-fill from the redacted base if needed.
        field_layer: None,
        predecessor: Some(doc.manifest.document_id()?),
        ..doc.manifest.clone()
    };
    let mut new_doc = Document { manifest, store };

    // Purge everything outside the new closure. This is what makes the
    // removed content *gone* rather than merely unreferenced.
    let keep = new_doc.closure()?;
    let purged = new_doc.store.retain_only(&keep);

    Ok(Redaction {
        document: new_doc,
        proof,
        purged,
        cache_invalidated,
    })
}

/// Replace the node at `rel` (relative path) under `node`, returning the
/// removed subtree. The replacement node carries the proof of what it
/// replaced, computed by the caller — so we splice a placeholder first
/// and patch the proof after hashing.
fn replace_at(
    node: &mut Node,
    doc: &Document,
    full: &[usize],
    rel: &[usize],
    reason: &Option<String>,
) -> Result<Node> {
    let idx = rel[0];
    let last = rel.len() == 1;

    // Resolve subtree refs transparently: materialize, descend, and let
    // the caller re-store the modified tree (the ref's parent now holds
    // the materialized + modified subtree inline; the old subtree object
    // becomes an orphan and is purged).
    if let Node::SubtreeRef(id) = node {
        let mut sub = Node::from_value(&doc.store.get_value(id)?)?;
        let removed = replace_at(&mut sub, doc, full, rel, reason)?;
        *node = sub;
        return Ok(removed);
    }
    // Salt wrappers are path-transparent; descending through one keeps
    // the wrapper (its salt) around the modified child.
    if let Node::Salted(s) = node {
        return replace_at(&mut s.child, doc, full, rel, reason);
    }

    let children: &mut Vec<Node> = match node {
        Node::Doc(d) => &mut d.children,
        Node::Section(s) => &mut s.children,
        Node::List(l) => {
            // A list child index selects an item (a block sequence).
            let item = l
                .items
                .get_mut(idx)
                .ok_or_else(|| Error::BadNodePath(full.to_vec()))?;
            if last {
                let removed = Node::Section(crate::tree::Section {
                    role: "list-item".into(),
                    columns: 1,
                    children: core::mem::take(item),
                });
                let proof = *blake3::hash(&removed.to_value()?.encode()?).as_bytes();
                *item = vec![Node::Redacted(Redacted {
                    reason: reason.clone(),
                    proof: Some(proof),
                })];
                return Ok(removed);
            }
            // Descend into the item's blocks: next index selects the block.
            return descend_blocks(item, doc, full, &rel[1..], reason);
        }
        Node::Table(t) => {
            // A table child index selects a cell in reading order
            // (head rows, then body, then foot, row-major).
            let mut cells: Vec<&mut crate::tree::Cell> = t
                .head
                .iter_mut()
                .chain(t.body.iter_mut())
                .chain(t.foot.iter_mut())
                .flat_map(|r| r.cells.iter_mut())
                .collect();
            let cell = cells
                .get_mut(idx)
                .ok_or_else(|| Error::BadNodePath(full.to_vec()))?;
            if last {
                let removed = Node::Section(crate::tree::Section {
                    role: "table-cell".into(),
                    columns: 1,
                    children: core::mem::take(&mut cell.children),
                });
                let proof = *blake3::hash(&removed.to_value()?.encode()?).as_bytes();
                cell.children = vec![Node::Redacted(Redacted {
                    reason: reason.clone(),
                    proof: Some(proof),
                })];
                return Ok(removed);
            }
            return descend_blocks(&mut cell.children, doc, full, &rel[1..], reason);
        }
        _ => return Err(Error::BadNodePath(full.to_vec())),
    };

    if last {
        let target = children
            .get_mut(idx)
            .ok_or_else(|| Error::BadNodePath(full.to_vec()))?;
        let removed = core::mem::replace(
            target,
            Node::Redacted(Redacted {
                reason: reason.clone(),
                proof: None,
            }),
        );
        let proof = *blake3::hash(&removed.to_value()?.encode()?).as_bytes();
        if let Node::Redacted(r) = target {
            r.proof = Some(proof);
        }
        Ok(removed)
    } else {
        let child = children
            .get_mut(idx)
            .ok_or_else(|| Error::BadNodePath(full.to_vec()))?;
        replace_at(child, doc, full, &rel[1..], reason)
    }
}

fn descend_blocks(
    blocks: &mut [Node],
    doc: &Document,
    full: &[usize],
    rel: &[usize],
    reason: &Option<String>,
) -> Result<Node> {
    let idx = rel[0];
    let target = blocks
        .get_mut(idx)
        .ok_or_else(|| Error::BadNodePath(full.to_vec()))?;
    if rel.len() == 1 {
        let removed = core::mem::replace(
            target,
            Node::Redacted(Redacted {
                reason: reason.clone(),
                proof: None,
            }),
        );
        let proof = *blake3::hash(&removed.to_value()?.encode()?).as_bytes();
        if let Node::Redacted(r) = target {
            r.proof = Some(proof);
        }
        Ok(removed)
    } else {
        replace_at(target, doc, full, &rel[1..], reason)
    }
}

/// Verify a redaction proof against a candidate original subtree
/// (e.g. one produced from an escrowed pre-redaction document).
pub fn verify_proof(proof: &[u8; 32], candidate: &Node) -> Result<bool> {
    Ok(*blake3::hash(&candidate.to_value()?.encode()?).as_bytes() == *proof)
}
