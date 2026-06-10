//! PDF → VSD import (spec §11).
//!
//! Two paths, tried in order:
//!
//! 1. **Hybrid recovery (lossless, verifiable).** If the PDF carries an
//!    embedded `source.vsd` attachment (as `vsd-pdf` exports do), the
//!    canonical document is extracted and read back — bit-exact, with
//!    its identity (and any signatures) intact. The round trip
//!    VSD → PDF → VSD is the identity function, cryptographically
//!    checkable.
//!
//! 2. **Structure recovery (lossy, honest about it).** Foreign PDFs go
//!    through a [`StructureRecovery`] strategy. The built-in
//!    [`TextRecovery`] extracts page text and rebuilds paragraphs —
//!    deliberately naive; richer recoverers (tagged-structure walkers,
//!    document-understanding models) plug in via the trait without
//!    entering the trusted core. The output is marked
//!    `format-migrated { lossy: true }` in its provenance chain and the
//!    original PDF rides along as an attachment resource for legal
//!    continuity.

use vsd_core::document::{Document, DocumentBuilder};
use vsd_core::manifest::{
    Assertion, Blob, Metadata, Profile, Provenance, ResourceEntry, ResourceKind,
};
use vsd_core::tree::{Direction, Doc, Inline, Node, Para};
use vsd_core::ResourceTable;

use crate::{PdfError, Result};

/// Outcome of an import.
pub enum ImportOutcome {
    /// The PDF carried its canonical VSD source; this *is* the original
    /// document, identity verified during container reading.
    Lossless {
        document: Document,
        signatures: Vec<vsd_container::Signature>,
        document_id: vsd_core::ObjectId,
    },
    /// Heuristic structure recovery was applied.
    Recovered {
        document: Document,
        pages_read: usize,
    },
}

/// A structure-recovery strategy for foreign PDFs. Implementations map
/// extracted page texts to block nodes; the import pipeline handles
/// provenance, attachment of the original, and document assembly.
pub trait StructureRecovery {
    /// A short identifier recorded in provenance (`tool` claim).
    fn name(&self) -> &str;

    /// `page_texts[i]` is the extracted text of page i+1 (possibly
    /// empty). Returns the recovered block nodes.
    fn recover(&self, page_texts: &[String]) -> Vec<Node>;
}

/// The built-in naive recoverer: blank-line-separated line groups
/// become paragraphs; page boundaries become page-break hints.
pub struct TextRecovery;

impl StructureRecovery for TextRecovery {
    fn name(&self) -> &str {
        "vsd-pdf/text-recovery"
    }

    fn recover(&self, page_texts: &[String]) -> Vec<Node> {
        let mut blocks = Vec::new();
        for (i, text) in page_texts.iter().enumerate() {
            if i > 0 {
                blocks.push(Node::PageBreakHint);
            }
            let mut para = String::new();
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    flush_para(&mut para, &mut blocks);
                } else {
                    if !para.is_empty() {
                        para.push(' ');
                    }
                    para.push_str(line);
                }
            }
            flush_para(&mut para, &mut blocks);
        }
        blocks
    }
}

fn flush_para(para: &mut String, blocks: &mut Vec<Node>) {
    if !para.is_empty() {
        blocks.push(Node::Para(Para {
            children: vec![Inline::Text(std::mem::take(para))],
        }));
    }
}

/// Import PDF bytes, attempting hybrid recovery first, then falling
/// back to the given recovery strategy.
pub fn import_pdf(bytes: &[u8], recovery: &dyn StructureRecovery) -> Result<ImportOutcome> {
    let pdf = lopdf::Document::load_mem(bytes).map_err(|e| PdfError::Parse(e.to_string()))?;

    // --- Path 1: embedded canonical source --------------------------------
    if let Some(vsd_bytes) = find_embedded_vsd(&pdf) {
        let file = vsd_container::read_document(&vsd_bytes, &vsd_container::ReadOptions::default())
            .map_err(|e| PdfError::EmbeddedSource(e.to_string()))?;
        return Ok(ImportOutcome::Lossless {
            document: file.document,
            signatures: file.signatures,
            document_id: file.document_id,
        });
    }

    // --- Path 2: structure recovery ----------------------------------------
    let pages = pdf.get_pages();
    let mut page_texts = Vec::with_capacity(pages.len());
    for &num in pages.keys() {
        page_texts.push(pdf.extract_text(&[num]).unwrap_or_default());
    }
    let pages_read = page_texts.len();
    let blocks = recovery.recover(&page_texts);

    let root = Node::Doc(Doc {
        lang: "und".into(), // language is unknowable from a foreign PDF
        dir: Direction::Ltr,
        children: blocks,
    });
    let root_id_preview = vsd_core::ObjectId::of_value(&root.to_value()?)?;

    // The original travels along as an attachment (spec §11): legal
    // continuity for documents whose recovery was heuristic.
    let original = Blob {
        mime: "application/pdf".into(),
        data: bytes.to_vec(),
    };

    let title = pdf_info_string(&pdf, b"Title");
    let author = pdf_info_string(&pdf, b"Author");

    let mut builder = DocumentBuilder::new(root);
    let original_id = builder.add_object(original.to_value())?;
    let document = builder
        .metadata(Metadata {
            title,
            authors: author.into_iter().collect(),
            ..Default::default()
        })
        .resources(ResourceTable {
            entries: vec![(
                "original-pdf".into(),
                ResourceEntry {
                    kind: ResourceKind::Attachment,
                    mime: "application/pdf".into(),
                    data: original_id,
                },
            )],
            styles: vec![],
        })
        .provenance(Provenance {
            assertions: vec![Assertion {
                kind: "format-migrated".into(),
                claims: vec![
                    ("source".into(), "pdf".into()),
                    ("tool".into(), recovery.name().into()),
                    ("lossy".into(), "true".into()),
                    (
                        "original-sha256-blake3".into(),
                        vsd_core::ObjectId::of_bytes(bytes).to_hex(),
                    ),
                ],
                // No prior manifest exists at migration time; the
                // assertion anchors to the recovered content root.
                manifest_hash: root_id_preview,
            }],
        })
        .profile(Profile::Core)
        .build()?;

    Ok(ImportOutcome::Recovered {
        document,
        pages_read,
    })
}

/// Locate an embedded `.vsd` attachment via the catalog's
/// EmbeddedFiles name tree.
fn find_embedded_vsd(pdf: &lopdf::Document) -> Option<Vec<u8>> {
    let catalog = pdf.catalog().ok()?;
    let names = deref_dict(pdf, catalog.get(b"Names").ok()?)?;
    let embedded = deref_dict(pdf, names.get(b"EmbeddedFiles").ok()?)?;
    let names_arr = deref_array(pdf, embedded.get(b"Names").ok()?)?;

    // Pairs of (name, filespec).
    for pair in names_arr.chunks(2) {
        let [name_obj, spec_obj] = pair else { continue };
        let name = match resolve(pdf, name_obj)? {
            lopdf::Object::String(s, _) => String::from_utf8_lossy(&s).into_owned(),
            _ => continue,
        };
        if !name.to_lowercase().ends_with(".vsd") {
            continue;
        }
        let spec = deref_dict(pdf, spec_obj)?;
        let ef = deref_dict(pdf, spec.get(b"EF").ok()?)?;
        let stream_ref = ef.get(b"F").ok()?;
        if let lopdf::Object::Stream(s) = resolve(pdf, stream_ref)? {
            return s.decompressed_content().ok().or(Some(s.content.clone()));
        }
    }
    None
}

fn resolve(pdf: &lopdf::Document, obj: &lopdf::Object) -> Option<lopdf::Object> {
    match obj {
        lopdf::Object::Reference(id) => pdf.get_object(*id).ok().cloned(),
        other => Some(other.clone()),
    }
}

fn deref_dict(pdf: &lopdf::Document, obj: &lopdf::Object) -> Option<lopdf::Dictionary> {
    match resolve(pdf, obj)? {
        lopdf::Object::Dictionary(d) => Some(d),
        _ => None,
    }
}

fn deref_array(pdf: &lopdf::Document, obj: &lopdf::Object) -> Option<Vec<lopdf::Object>> {
    match resolve(pdf, obj)? {
        lopdf::Object::Array(a) => Some(a),
        _ => None,
    }
}

fn pdf_info_string(pdf: &lopdf::Document, key: &[u8]) -> Option<String> {
    let info_ref = pdf.trailer.get(b"Info").ok()?;
    let info = deref_dict(pdf, info_ref)?;
    match resolve(pdf, info.get(key).ok()?)? {
        lopdf::Object::String(s, _) => {
            let text = String::from_utf8_lossy(&s).trim().to_owned();
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}
