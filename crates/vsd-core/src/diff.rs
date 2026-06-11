//! Version diffs are object-set diffs, exactly like Git trees (spec §2.3).
//!
//! Because every object is content-addressed and unchanged subtrees share
//! ids, comparing two revisions is set arithmetic on object ids, plus a
//! structural walk to name *where* the trees diverge.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::document::Document;
use crate::error::Result;
use crate::object::ObjectId;
use crate::tree::Node;

#[derive(Debug, Default)]
pub struct Diff {
    /// Objects only in the new document.
    pub added: Vec<ObjectId>,
    /// Objects only in the old document.
    pub removed: Vec<ObjectId>,
    /// Objects shared by both (the dedup payoff).
    pub shared: usize,
    /// Bytes in added / removed objects.
    pub added_bytes: u64,
    pub removed_bytes: u64,
    /// Tree paths (child indices from the root) where content diverges.
    pub changed_paths: Vec<String>,
    pub same_document: bool,
}

pub fn diff(old: &Document, new: &Document) -> Result<Diff> {
    let mut d = Diff {
        same_document: old.document_id()? == new.document_id()?,
        ..Diff::default()
    };
    if d.same_document {
        d.shared = old.closure()?.len();
        return Ok(d);
    }

    let old_set = old.closure()?;
    let new_set = new.closure()?;
    d.shared = old_set.intersection(&new_set).count();
    for id in new_set.difference(&old_set) {
        d.added.push(*id);
        d.added_bytes += new.store.get_bytes(id).map(|b| b.len() as u64).unwrap_or(0);
    }
    for id in old_set.difference(&new_set) {
        d.removed.push(*id);
        d.removed_bytes += old.store.get_bytes(id).map(|b| b.len() as u64).unwrap_or(0);
    }

    // Structural walk to name divergent paths.
    let old_root = old.root_node()?;
    let new_root = new.root_node()?;
    let mut path = Vec::new();
    walk_diff(
        &old_root,
        &new_root,
        old,
        new,
        &mut path,
        &mut d.changed_paths,
    )?;
    Ok(d)
}

fn fmt_path(path: &[usize]) -> String {
    if path.is_empty() {
        "(root)".to_owned()
    } else {
        path.iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }
}

fn resolve(node: &Node, doc: &Document) -> Result<Node> {
    match node {
        Node::SubtreeRef(id) => Node::from_value(&doc.store.get_value(id)?),
        other => Ok(other.clone()),
    }
}

fn walk_diff(
    a: &Node,
    b: &Node,
    da: &Document,
    db: &Document,
    path: &mut Vec<usize>,
    out: &mut Vec<String>,
) -> Result<()> {
    // Bound the report size; past a point, path lists stop being useful.
    if out.len() >= 200 {
        return Ok(());
    }
    let a = resolve(a, da)?;
    let b = resolve(b, db)?;
    if a == b {
        return Ok(());
    }
    let (ac, bc) = (children_of(&a), children_of(&b));
    match (ac, bc) {
        (Some(ac), Some(bc)) if same_kind(&a, &b) => {
            if ac.len() != bc.len() {
                out.push(format!(
                    "{}: child count {} → {}",
                    fmt_path(path),
                    ac.len(),
                    bc.len()
                ));
                return Ok(());
            }
            for (i, (ca, cb)) in ac.iter().zip(bc.iter()).enumerate() {
                path.push(i);
                walk_diff(ca, cb, da, db, path, out)?;
                path.pop();
            }
        }
        _ => out.push(format!("{}: {} → {}", fmt_path(path), kind(&a), kind(&b))),
    }
    Ok(())
}

fn same_kind(a: &Node, b: &Node) -> bool {
    kind(a) == kind(b)
}

fn kind(n: &Node) -> &'static str {
    match n {
        Node::Doc(_) => "doc",
        Node::Section(_) => "section",
        Node::Heading(_) => "heading",
        Node::Para(_) => "paragraph",
        Node::Table(_) => "table",
        Node::Figure(_) => "figure",
        Node::List(_) => "list",
        Node::Code(_) => "code",
        Node::Math(_) => "math",
        Node::Field(_) => "field",
        Node::PageBreakHint => "pagebreak",
        Node::Redacted(_) => "redacted",
        Node::SubtreeRef(_) => "ref",
        Node::Salted(_) => "salted",
    }
}

fn children_of(n: &Node) -> Option<Vec<Node>> {
    match n {
        Node::Doc(d) => Some(d.children.clone()),
        Node::Section(s) => Some(s.children.clone()),
        // The salt wrapper is transparent: compare its child.
        Node::Salted(s) => Some(vec![(*s.child).clone()]),
        _ => None,
    }
}

/// Walk an amendment chain: list of (document id, predecessor id) pairs
/// from the given document backwards. The chain itself lives across
/// files; this reports what the manifest at hand asserts.
pub fn amendment_chain(doc: &Document) -> Result<Vec<(ObjectId, Option<ObjectId>)>> {
    Ok(vec![(doc.document_id()?, doc.manifest.predecessor)])
}

/// Convenience: ids shared between two stores (dedup measurement).
pub fn shared_objects(a: &Document, b: &Document) -> Result<BTreeSet<ObjectId>> {
    Ok(a.closure()?.intersection(&b.closure()?).copied().collect())
}
