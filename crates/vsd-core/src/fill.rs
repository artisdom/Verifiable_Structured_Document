//! Filling and flattening forms (spec §6).
//!
//! Filled values are a separate object layer over the immutable base
//! document: the blank form and each filled instance share all
//! structural objects, and a filled instance records the base as its
//! `predecessor`. Flattening is a *defined merge* — field nodes are
//! replaced by their final values in the content tree — not a
//! print-to-new-file.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::document::Document;
use crate::error::{Error, Result};
use crate::forms::{check_constraints, evaluate_computed, FieldValue, FilledLayer, Violation};
use crate::manifest::Manifest;
use crate::tree::{FieldKind, Inline, Node, Para};

/// Outcome of a fill operation. Violations do not prevent saving a
/// partially filled form; they prevent *flattening* it.
pub struct FillResult {
    pub document: Document,
    pub violations: Vec<Violation>,
}

/// Apply `inputs` on top of any existing filled layer, producing a new
/// document. Input ids must name fields; values must match field kinds.
pub fn fill(doc: &Document, inputs: &BTreeMap<String, FieldValue>) -> Result<FillResult> {
    let fields = doc.fields()?;
    let kinds: BTreeMap<&str, FieldKind> = fields.iter().map(|f| (f.id.as_str(), f.kind)).collect();

    for (id, value) in inputs {
        let kind = kinds
            .get(id.as_str())
            .ok_or_else(|| Error::Schema(format!("no field with id {id:?}")))?;
        let ok = matches!(
            (kind, value),
            (FieldKind::Number, FieldValue::Num(_))
                | (FieldKind::Checkbox, FieldValue::Bool(_))
                | (
                    FieldKind::Text
                        | FieldKind::Date
                        | FieldKind::Choice
                        | FieldKind::Signature
                        | FieldKind::Attachment,
                    FieldValue::Str(_)
                )
        ) || *value == FieldValue::Empty;
        if !ok {
            return Err(Error::Schema(format!(
                "field {id:?} has kind {}, value type does not match",
                kind.as_str()
            )));
        }
    }

    // Overlay on the existing layer; Empty erases a previous value.
    let mut merged: BTreeMap<String, FieldValue> = match doc.manifest.field_layer {
        Some(id) => FilledLayer::from_value(&doc.store.get_value(&id)?)?.env(),
        None => BTreeMap::new(),
    };
    for (id, value) in inputs {
        if *value == FieldValue::Empty {
            merged.remove(id);
        } else {
            merged.insert(id.clone(), value.clone());
        }
    }

    // Only explicit inputs are persisted; computed values are derived on
    // read (a stored computed value would be one more cache that can lie).
    let env = evaluate_computed(&fields, &merged)?;
    let violations = check_constraints(&fields, &env);

    let layer = FilledLayer {
        values: merged.into_iter().collect(),
    };
    let mut store = doc.store.clone();
    let layer_id = store.put_value(&layer.to_value())?;
    let manifest = Manifest {
        field_layer: Some(layer_id),
        predecessor: Some(doc.manifest.document_id()?),
        ..doc.manifest.clone()
    };
    let mut document = Document { manifest, store };
    let keep = document.closure()?;
    document.store.retain_only(&keep); // drop a superseded layer object

    Ok(FillResult {
        document,
        violations,
    })
}

/// Replace every field node with its final value, producing a plain
/// (form-free) document. Fails if any required field is empty or any
/// constraint is violated — a flattened document is final-form.
pub fn flatten(doc: &Document) -> Result<Document> {
    let fields = doc.fields()?;
    let inputs = match doc.manifest.field_layer {
        Some(id) => FilledLayer::from_value(&doc.store.get_value(&id)?)?.env(),
        None => BTreeMap::new(),
    };
    let env = evaluate_computed(&fields, &inputs)?;
    let violations = check_constraints(&fields, &env);
    if !violations.is_empty() {
        let mut msg = String::from("cannot flatten: ");
        for (i, v) in violations.iter().enumerate() {
            if i > 0 {
                msg.push_str("; ");
            }
            msg.push_str(&format!("{}: {}", v.field, v.message));
        }
        return Err(Error::Validation(msg));
    }

    let mut root = doc.root_node()?;
    replace_fields(&mut root, doc, &env)?;

    let mut store = doc.store.clone();
    let new_root = store.put_value(&root.to_value()?)?;
    let manifest = Manifest {
        root: new_root,
        field_layer: None,
        // Layout depends on field rendering; any cache must be recomputed.
        render_cache: None,
        page_index: None,
        predecessor: Some(doc.manifest.document_id()?),
        ..doc.manifest.clone()
    };
    let mut flat = Document { manifest, store };
    let keep = flat.closure()?;
    flat.store.retain_only(&keep);
    Ok(flat)
}

fn replace_fields(
    node: &mut Node,
    doc: &Document,
    env: &BTreeMap<String, FieldValue>,
) -> Result<()> {
    match node {
        Node::Field(f) => {
            let value = env.get(&f.id).cloned().unwrap_or(FieldValue::Empty);
            *node = Node::Para(Para {
                children: vec![Inline::Text(value.to_text())],
            });
        }
        Node::Doc(d) => {
            for c in &mut d.children {
                replace_fields(c, doc, env)?;
            }
        }
        Node::Section(s) => {
            for c in &mut s.children {
                replace_fields(c, doc, env)?;
            }
        }
        Node::Table(t) => {
            for row in t.head.iter_mut().chain(&mut t.body).chain(&mut t.foot) {
                for cell in &mut row.cells {
                    for c in &mut cell.children {
                        replace_fields(c, doc, env)?;
                    }
                }
            }
        }
        Node::List(l) => {
            for item in &mut l.items {
                for c in item {
                    replace_fields(c, doc, env)?;
                }
            }
        }
        Node::SubtreeRef(id) => {
            // Materialize: the flattened tree must not share a subtree
            // object with the form version if that subtree held fields.
            let mut sub = Node::from_value(&doc.store.get_value(id)?)?;
            replace_fields(&mut sub, doc, env)?;
            *node = sub;
        }
        _ => {}
    }
    Ok(())
}
