//! PDF interop tests: tagged export structure, the verifiable hybrid
//! round trip, heuristic import of foreign PDFs, and determinism.

use vsd_container::{write_document, WriteOptions};
use vsd_core::document::DocumentBuilder;
use vsd_core::manifest::{Blob, Metadata, ResourceEntry, ResourceKind};
use vsd_core::tree::{Direction, Doc, Figure, Heading, Inline, Link, Node, Para};
use vsd_core::{Document, ResourceTable};
use vsd_pdf::{export_pdf, import_pdf, ExportOptions, ImportOutcome, TextRecovery};

/// A 1×1 red PNG.
fn tiny_png() -> Vec<u8> {
    let mut pixmap = tiny_skia::Pixmap::new(1, 1).unwrap();
    pixmap.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
    pixmap.encode_png().unwrap()
}

fn sample_document() -> Document {
    let blob = Blob {
        mime: "image/png".into(),
        data: tiny_png(),
    };
    let mut builder = DocumentBuilder::new(Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        children: vec![],
    }));
    let blob_id = builder.add_object(blob.to_value()).unwrap();

    let root = Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        children: vec![
            Node::Heading(Heading {
                level: 1,
                children: vec![Inline::Text("Export Test".into())],
            }),
            Node::Para(Para {
                children: vec![
                    Inline::Text("Body with a ".into()),
                    Inline::Link(Link {
                        href: "https://example.com".into(),
                        children: vec![Inline::Text("link".into())],
                    }),
                    Inline::Text(".".into()),
                ],
            }),
            Node::Figure(Figure {
                res: blob_id,
                alt: "A red square".into(),
                decorative: false,
                caption: vec![Inline::Text("Fig 1".into())],
            }),
        ],
    });
    let mut builder = DocumentBuilder::new(root).metadata(Metadata {
        title: Some("Export Test".into()),
        authors: vec!["VSD".into()],
        ..Default::default()
    });
    builder.add_object(blob.to_value()).unwrap();
    builder
        .resources(ResourceTable {
            entries: vec![(
                "img".into(),
                ResourceEntry {
                    kind: ResourceKind::Image,
                    mime: "image/png".into(),
                    data: blob_id,
                },
            )],
            styles: vec![],
        })
        .build()
        .unwrap()
}

#[test]
fn export_is_valid_tagged_pdf() {
    let doc = sample_document();
    let pdf_bytes = export_pdf(
        &doc,
        None,
        &ExportOptions {
            embed_source: false,
        },
    )
    .unwrap();
    assert!(pdf_bytes.starts_with(b"%PDF-1.7"));

    // A real PDF parser must accept it.
    let pdf = lopdf::Document::load_mem(&pdf_bytes).unwrap();
    assert_eq!(pdf.get_pages().len(), 1);

    let catalog = pdf.catalog().unwrap();
    assert!(catalog.has(b"StructTreeRoot"), "must be tagged");
    assert!(catalog.has(b"MarkInfo"));

    let text = String::from_utf8_lossy(&pdf_bytes);
    assert!(text.contains("/H1"), "heading struct type present");
    assert!(text.contains("/Caption"), "figure caption tagged");
    assert!(
        text.contains("(A red square)"),
        "figure alt text rides along"
    );
    assert!(text.contains("/Subtype /Type0"), "CID font embedded");
    assert!(text.contains("/ToUnicode"), "text extraction supported");
    assert!(
        text.contains("vsd-doc-id:") && text.contains(&doc.document_id().unwrap().to_hex()),
        "document identity recorded in metadata"
    );
}

#[test]
fn hybrid_round_trip_is_the_identity_function() {
    let doc = sample_document();
    let key = vsd_sign::SigningKey::from_seed(&[7u8; 32]).unwrap();
    let sig = key.sign_document(&doc).unwrap();
    let vsd_bytes = write_document(&doc, &[sig], &WriteOptions::default()).unwrap();

    let pdf_bytes = export_pdf(&doc, Some(&vsd_bytes), &ExportOptions::default()).unwrap();

    match import_pdf(&pdf_bytes, &TextRecovery).unwrap() {
        ImportOutcome::Lossless {
            document,
            signatures,
            document_id,
        } => {
            assert_eq!(document_id, doc.document_id().unwrap());
            assert_eq!(document.manifest, doc.manifest);
            // The signature survived the trip through PDF…
            assert_eq!(signatures.len(), 1);
            // …and still verifies.
            assert_eq!(
                vsd_sign::verify(&document, &signatures[0]).unwrap(),
                vsd_sign::Verdict::Valid
            );
        }
        ImportOutcome::Recovered { .. } => panic!("hybrid PDF must recover losslessly"),
    }
}

#[test]
fn export_is_deterministic() {
    let doc = sample_document();
    let opts = ExportOptions {
        embed_source: false,
    };
    assert_eq!(
        export_pdf(&doc, None, &opts).unwrap(),
        export_pdf(&doc, None, &opts).unwrap(),
        "same document must export to identical PDF bytes"
    );
}

#[test]
fn foreign_pdf_recovers_heuristically_with_provenance() {
    // Build a foreign PDF (simple Type1 font) with lopdf.
    let pdf_bytes = simple_foreign_pdf("Hello from a foreign PDF.");

    match import_pdf(&pdf_bytes, &TextRecovery).unwrap() {
        ImportOutcome::Lossless { .. } => panic!("no embedded source in a foreign PDF"),
        ImportOutcome::Recovered {
            document,
            pages_read,
        } => {
            assert_eq!(pages_read, 1);
            let report = vsd_core::validate::validate(&document);
            assert!(report.is_valid(), "findings: {:?}", report.findings);

            // Recovered text present.
            let text = vsd_core::extract::extract_text(&document).unwrap();
            assert!(text.contains("Hello from a foreign PDF."), "text: {text}");

            // Provenance says lossy, and the original rides along.
            let prov = document.provenance().unwrap().expect("provenance");
            let a = &prov.assertions[0];
            assert_eq!(a.kind, "format-migrated");
            assert!(a.claims.iter().any(|(k, v)| k == "lossy" && v == "true"));
            let resources = document.resources().unwrap();
            let original = resources
                .entries
                .iter()
                .find(|(name, _)| name == "original-pdf")
                .expect("original embedded");
            let blob =
                Blob::from_value(&document.store.get_value(&original.1.data).unwrap()).unwrap();
            assert_eq!(blob.data, pdf_bytes, "original preserved byte-exact");
        }
    }
}

#[test]
fn vsd_to_pdf_to_vsd_preserves_content_tree() {
    // The Phase 3 exit criterion, hybrid path: tree in == tree out.
    let doc = sample_document();
    let vsd_bytes = write_document(&doc, &[], &WriteOptions::default()).unwrap();
    let pdf = export_pdf(&doc, Some(&vsd_bytes), &ExportOptions::default()).unwrap();
    let ImportOutcome::Lossless { document, .. } = import_pdf(&pdf, &TextRecovery).unwrap() else {
        panic!("hybrid expected");
    };
    assert_eq!(document.root_node().unwrap(), doc.root_node().unwrap());
    assert_eq!(
        vsd_core::extract::extract_text(&document).unwrap(),
        vsd_core::extract::extract_text(&doc).unwrap()
    );
}

/// Minimal foreign PDF via lopdf (Helvetica, uncompressed content).
fn simple_foreign_pdf(text: &str) -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Object, Stream};

    let mut doc = lopdf::Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![72.into(), 700.into()]),
            Operation::new("Tj", vec![Object::string_literal(text)]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}
