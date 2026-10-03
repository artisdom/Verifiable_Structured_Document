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
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![],
    }));
    let blob_id = builder.add_object(blob.to_value()).unwrap();

    let root = Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
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
            ..Default::default()
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
        ..Default::default()
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
            via,
        } => {
            assert_eq!(pages_read, 1);
            assert_eq!(
                via, "vsd-pdf/geometry-recovery",
                "untagged with text → geometry recovery"
            );
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

/// Geometry recovery of an untagged PDF: a large-font line becomes a
/// heading, and body lines group into paragraphs by their vertical gaps.
#[test]
fn untagged_pdf_recovers_headings_and_paragraphs_by_geometry() {
    let pdf = geometry_foreign_pdf();
    let ImportOutcome::Recovered { document, via, .. } = import_pdf(&pdf, &TextRecovery).unwrap()
    else {
        panic!("geometry recovery expected");
    };
    assert_eq!(via, "vsd-pdf/geometry-recovery");

    let Node::Doc(d) = document.root_node().unwrap() else {
        panic!("doc root");
    };
    // First block: a heading recovered from the 24pt line.
    match &d.children[0] {
        Node::Heading(h) => assert!(
            inline_to_string(&h.children).contains("Big Title"),
            "heading text"
        ),
        other => panic!("expected a heading, got {other:?}"),
    }
    // Then two paragraphs; the first joins the two closely-spaced lines.
    let paras: Vec<String> = d
        .children
        .iter()
        .filter_map(|n| match n {
            Node::Para(p) => Some(inline_to_string(&p.children)),
            _ => None,
        })
        .collect();
    assert_eq!(paras.len(), 2, "two paragraphs by vertical gap");
    assert!(paras[0].contains("First body line") && paras[0].contains("Second line"));
    assert!(paras[1].contains("new paragraph"));
    assert!(vsd_core::validate::validate(&document).is_valid());
}

fn inline_to_string(inls: &[Inline]) -> String {
    inls.iter()
        .map(|i| match i {
            Inline::Text(t) => t.clone(),
            _ => String::new(),
        })
        .collect()
}

/// A foreign PDF (no structure tree) with a 24pt title and three 12pt
/// body lines positioned absolutely — exercises geometry clustering.
fn geometry_foreign_pdf() -> Vec<u8> {
    positioned_foreign_pdf(&[vec![
        (24, 72, 720, "Big Title"),
        (12, 72, 690, "First body line of text."),
        (12, 72, 676, "Second line same paragraph."),
        (12, 72, 640, "A new paragraph after a gap."),
    ]])
}

/// Deliberately untagged PDFs with content-stream order independent of layout.
fn positioned_foreign_pdf(pages: &[Vec<(i64, i64, i64, &str)>]) -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Object, Stream};

    let mut doc = lopdf::Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let mut kids = Vec::new();
    for lines in pages {
        let mut ops = vec![Operation::new("BT", vec![])];
        for &(size, x, y, text) in lines {
            ops.push(Operation::new("Tf", vec!["F1".into(), size.into()]));
            ops.push(Operation::new(
                "Tm",
                vec![1.into(), 0.into(), 0.into(), 1.into(), x.into(), y.into()],
            ));
            ops.push(Operation::new("Tj", vec![Object::string_literal(text)]));
        }
        ops.push(Operation::new("ET", vec![]));
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            Content { operations: ops }.encode().unwrap(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        kids.push(Object::Reference(page_id));
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => kids, "Count" => pages.len() as i64,
        }),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

fn recovered_blocks(bytes: &[u8]) -> Vec<Node> {
    let ImportOutcome::Recovered { document, via, .. } = import_pdf(bytes, &TextRecovery).unwrap()
    else {
        panic!("expected geometry recovery")
    };
    assert_eq!(via, "vsd-pdf/geometry-recovery");
    assert!(vsd_core::validate::validate(&document).is_valid());
    let Node::Doc(root) = document.root_node().unwrap() else {
        panic!("doc root")
    };
    root.children
}

#[test]
fn untagged_tables_preserve_cells_and_surrounding_text() {
    let pdf = positioned_foreign_pdf(&[vec![
        (24, 72, 740, "Inventory"),
        (12, 72, 710, "Available items are listed below."),
        (12, 300, 660, "Qty"),
        (12, 72, 660, "Name"),
        (12, 300, 642, "7"),
        (12, 72, 642, "Red"),
        (12, 96, 643, "apples"),
        (12, 72, 624, "Oranges"),
        (12, 300, 624, "12"),
        (12, 72, 590, "End of inventory."),
    ]]);
    let blocks = recovered_blocks(&pdf);
    assert_eq!(blocks.len(), 4, "{blocks:?}");
    assert!(matches!(&blocks[0], Node::Heading(h) if inline_text(&h.children) == "Inventory"));
    assert_eq!(
        inline_text_of_block(&blocks[1]),
        "Available items are listed below."
    );
    let Node::Table(table) = &blocks[2] else {
        panic!("table: {blocks:?}")
    };
    assert_eq!(table.cols.len(), 2);
    assert!(table.head.is_empty() && table.foot.is_empty());
    let texts: Vec<Vec<String>> = table
        .body
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| {
                    assert!(c.scope.is_none() && c.span.is_none());
                    inline_text_of_block(&c.children[0])
                })
                .collect()
        })
        .collect();
    assert_eq!(
        texts,
        vec![
            vec!["Name", "Qty"],
            vec!["Red apples", "7"],
            vec!["Oranges", "12"]
        ]
    );
    assert_eq!(inline_text_of_block(&blocks[3]), "End of inventory.");

    let ImportOutcome::Recovered {
        document,
        pages_read,
        ..
    } = import_pdf(&pdf, &TextRecovery).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(pages_read, 1);
    let prov = document.provenance().unwrap().unwrap();
    assert!(prov.assertions[0]
        .claims
        .iter()
        .any(|(k, v)| k == "lossy" && v == "true"));
    // The recovered table survives canonical serialization and container reading.
    let packed = write_document(&document, &[], &WriteOptions::default()).unwrap();
    let read = vsd_container::read_document(&packed, &Default::default()).unwrap();
    assert_eq!(
        document.root_node().unwrap(),
        read.document.root_node().unwrap()
    );
}

#[test]
fn tj_positioning_gaps_recover_cells_without_splitting_kerning() {
    use lopdf::content::{Content, Operation};
    use lopdf::Object;
    let bytes = positioned_foreign_pdf(&[vec![]]);
    let mut pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let page = *pdf.get_pages().get(&1).unwrap();
    let content_id = pdf
        .get_dictionary(page)
        .unwrap()
        .get(b"Contents")
        .unwrap()
        .as_reference()
        .unwrap();
    let mut ops = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 12.into()]),
    ];
    for (y, label, qty) in [
        (700, "Name", "Qty"),
        (682, "Apples", "7"),
        (664, "Oranges", "12"),
    ] {
        ops.push(Operation::new(
            "Tm",
            vec![1.into(), 0.into(), 0.into(), 1.into(), 72.into(), y.into()],
        ));
        let gap = -((300 - 72 - label.len() as i64 * 6) * 1000 / 12);
        // Tiny kerning between the first character and the rest must not add a space.
        ops.push(Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal(&label[..1]),
                5.into(),
                Object::string_literal(&label[1..]),
                gap.into(),
                Object::string_literal(qty),
            ])],
        ));
    }
    ops.push(Operation::new("ET", vec![]));
    pdf.get_object_mut(content_id)
        .unwrap()
        .as_stream_mut()
        .unwrap()
        .set_content(Content { operations: ops }.encode().unwrap());
    let mut out = Vec::new();
    pdf.save_to(&mut out).unwrap();
    let blocks = recovered_blocks(&out);
    let [Node::Table(table)] = blocks.as_slice() else {
        panic!("{blocks:?}")
    };
    assert_eq!(table.body.len(), 3);
    assert_eq!(
        inline_text_of_block(&table.body[1].cells[0].children[0]),
        "Apples"
    );
    assert_eq!(
        inline_text_of_block(&table.body[2].cells[1].children[0]),
        "12"
    );
}

#[test]
fn untagged_text_table_recovers_three_columns_and_page_boundaries() {
    let mut rows = vec![];
    for (y, cells) in [
        (700, ["Name", "Colour", "Origin"]),
        (682, ["Apples", "Red", "NZ"]),
        (664, ["Oranges", "Orange", "AU"]),
    ] {
        for (x, text) in [72, 240, 420].into_iter().zip(cells) {
            rows.push((12, x, y, text));
        }
    }
    let blocks = recovered_blocks(&positioned_foreign_pdf(&[
        rows.clone(),
        vec![],
        rows,
        vec![],
    ]));
    assert!(matches!(
        blocks.as_slice(),
        [
            Node::Table(_),
            Node::PageBreakHint,
            Node::PageBreakHint,
            Node::Table(_)
        ]
    ));
    let Node::Table(table) = &blocks[0] else {
        unreachable!()
    };
    assert_eq!(table.body.len(), 3);
    assert_eq!(table.cols.len(), 3);
    assert_eq!(
        inline_text_of_block(&table.body[2].cells[2].children[0]),
        "AU"
    );
}

#[test]
fn uneven_columns_read_in_order_around_spanning_sections() {
    let mut rows = vec![(24, 72, 740, "Report")];
    for (y, left, right) in [
        (
            700,
            "Left first sentence.",
            "Right first sentence is considerably longer.",
        ),
        (
            686,
            "Left second sentence.",
            "Right second sentence is considerably longer.",
        ),
        (
            672,
            "Left third sentence.",
            "Right third sentence is considerably longer.",
        ),
        (
            600,
            "Left fourth sentence.",
            "Right fourth sentence is considerably longer.",
        ),
        (
            586,
            "Left fifth sentence.",
            "Right fifth sentence is considerably longer.",
        ),
        (
            572,
            "Left sixth sentence.",
            "Right sixth sentence is considerably longer.",
        ),
    ] {
        rows.push((12, 240, y, right));
        rows.push((12, 72, y, left));
    }
    rows.push((18, 72, 640, "Section"));
    rows.push((
        12,
        72,
        530,
        "A final full-width sentence spanning both recovered columns.",
    ));
    let blocks = recovered_blocks(&positioned_foreign_pdf(&[rows]));
    assert_eq!(blocks.len(), 7, "{blocks:?}");
    assert!(matches!(&blocks[0], Node::Heading(h) if inline_text(&h.children) == "Report"));
    assert_eq!(
        inline_text_of_block(&blocks[1]),
        "Left first sentence. Left second sentence. Left third sentence."
    );
    assert!(inline_text_of_block(&blocks[2]).starts_with("Right first"));
    assert!(matches!(&blocks[3], Node::Heading(h) if inline_text(&h.children) == "Section"));
    assert!(inline_text_of_block(&blocks[4]).starts_with("Left fourth"));
    assert!(inline_text_of_block(&blocks[5]).starts_with("Right fourth"));
    assert!(inline_text_of_block(&blocks[6]).starts_with("A final full-width"));
}

#[test]
fn short_parallel_prose_is_columns_not_a_table() {
    let rows = vec![
        (12, 72, 700, "Left one."),
        (12, 300, 700, "Right one."),
        (12, 72, 686, "Left two."),
        (12, 300, 686, "Right two."),
        (12, 72, 672, "Left three."),
        (12, 300, 672, "Right three."),
    ];
    let blocks = recovered_blocks(&positioned_foreign_pdf(&[rows]));
    assert_eq!(blocks.len(), 2, "{blocks:?}");
    assert_eq!(
        inline_text_of_block(&blocks[0]),
        "Left one. Left two. Left three."
    );
    assert_eq!(
        inline_text_of_block(&blocks[1]),
        "Right one. Right two. Right three."
    );
}

#[test]
fn staggered_columns_do_not_require_shared_baselines() {
    let rows = vec![
        (12, 72, 700, "Left one."),
        (12, 300, 693, "Right one."),
        (12, 72, 685, "Left two."),
        (12, 300, 678, "Right two."),
        (12, 72, 670, "Left three."),
        (12, 300, 663, "Right three."),
    ];
    let blocks = recovered_blocks(&positioned_foreign_pdf(&[rows]));
    assert_eq!(blocks.len(), 2, "{blocks:?}");
    assert_eq!(
        inline_text_of_block(&blocks[0]),
        "Left one. Left two. Left three."
    );
    assert_eq!(
        inline_text_of_block(&blocks[1]),
        "Right one. Right two. Right three."
    );
}

#[test]
fn three_prose_columns_are_independent_of_stream_order() {
    let mut rows = Vec::new();
    for y in [700, 686, 672] {
        for (x, text) in [
            (40, "First column has a long line."),
            (240, "Middle column has a long line."),
            (440, "Last column has a long line."),
        ] {
            rows.push((10, x, y, text));
        }
    }
    let blocks = recovered_blocks(&positioned_foreign_pdf(&[rows.clone()]));
    rows.reverse();
    assert_eq!(blocks, recovered_blocks(&positioned_foreign_pdf(&[rows])));
    assert_eq!(blocks.len(), 3, "{blocks:?}");
    for (block, expected) in blocks.iter().zip(["First", "Middle", "Last"]) {
        let text = inline_text_of_block(block);
        assert!(text.starts_with(expected), "{text}");
        assert_eq!(text.matches("long line.").count(), 3);
    }
}

#[test]
fn insufficient_or_misaligned_rows_do_not_invent_a_table() {
    for rows in [
        vec![
            (12, 72, 700, "Name"),
            (12, 300, 700, "Qty"),
            (12, 72, 682, "Apples"),
            (12, 300, 682, "7"),
        ],
        vec![
            (12, 72, 700, "Name"),
            (12, 300, 700, "Qty"),
            (12, 72, 682, "Apples"),
            (12, 300, 682, "7"),
            (12, 72, 664, "Oranges"),
            (12, 340, 664, "12"),
        ],
    ] {
        let blocks = recovered_blocks(&positioned_foreign_pdf(&[rows]));
        assert!(!blocks.iter().any(|n| matches!(n, Node::Table(_))));
        let text = blocks
            .iter()
            .map(inline_text_of_block)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("Apples") && text.contains('7'), "{text}");
    }
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

/// PDF/A export: with the embedded source it targets PDF/A-3b (which
/// permits the attachment), carrying the XMP `pdfaid` identification, an
/// sRGB OutputIntent, a trailer `/ID`, and subset-tagged fonts with a
/// `/CIDSet`. Without the source it is PDF/A-2b.
#[test]
fn pdfa_export_has_archival_scaffolding() {
    let doc = sample_document();
    let vsd = write_document(&doc, &[], &WriteOptions::default()).unwrap();

    // PDF/A-3b (source embedded).
    let pdf = export_pdf(
        &doc,
        Some(&vsd),
        &ExportOptions {
            embed_source: true,
            pdfa: true,
        },
    )
    .unwrap();
    let parsed = lopdf::Document::load_mem(&pdf).unwrap();
    let catalog = parsed.catalog().unwrap();
    assert!(catalog.has(b"Metadata"), "XMP metadata stream");
    assert!(catalog.has(b"OutputIntents"), "OutputIntent present");
    assert!(catalog.has(b"AF"), "associated file (PDF/A-3)");

    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/ID [<"), "trailer /ID required by PDF/A");
    assert!(
        text.contains("pdfaid:part>3"),
        "PDF/A part 3 with attachment"
    );
    assert!(text.contains("pdfaid:conformance>B"));
    assert!(text.contains("/GTS_PDFA1"), "OutputIntent subtype");
    assert!(text.contains("/DestOutputProfile"), "embedded ICC profile");
    assert!(text.contains("/CIDSet"), "CIDFont subset CIDSet");
    // Subset-tagged font name: six uppercase letters + '+'.
    let tagged = pdf
        .windows(7)
        .any(|w| w[..6].iter().all(|b| b.is_ascii_uppercase()) && w[6] == b'+');
    assert!(tagged, "subset tag prefix on the embedded font");
    assert!(
        text.contains("<pdf:Producer>vsd-pdf"),
        "XMP/Info consistency"
    );

    // Still a hybrid PDF: the round trip back is lossless.
    let ImportOutcome::Lossless { document, .. } = import_pdf(&pdf, &TextRecovery).unwrap() else {
        panic!("PDF/A-3b must still carry its embedded source");
    };
    assert_eq!(document.root_node().unwrap(), doc.root_node().unwrap());

    // Without the embedded source → PDF/A-2b, and deterministic.
    let opts2 = ExportOptions {
        embed_source: false,
        pdfa: true,
    };
    let a = export_pdf(&doc, None, &opts2).unwrap();
    let b = export_pdf(&doc, None, &opts2).unwrap();
    assert_eq!(a, b, "PDF/A export is deterministic");
    let atext = String::from_utf8_lossy(&a);
    assert!(atext.contains("pdfaid:part>2"), "part 2 without attachment");
    assert!(
        !atext.contains("/AF ["),
        "no associated file without source"
    );
}

/// A *foreign tagged* PDF made by our own exporter with the embedded
/// source switched off: its StructTreeRoot must drive recovery (path 2),
/// reconstructing headings (with levels) and paragraphs from the marked
/// content — not the naive text path.
#[test]
fn foreign_tagged_pdf_recovers_structure() {
    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![
            Node::Heading(Heading {
                level: 1,
                children: vec![Inline::Text("Annual Report".into())],
            }),
            Node::Para(Para {
                children: vec![Inline::Text(
                    "This is the opening paragraph of the report.".into(),
                )],
            }),
            Node::Heading(Heading {
                level: 2,
                children: vec![Inline::Text("Summary".into())],
            }),
            Node::Para(Para {
                children: vec![Inline::Text("A short summary follows here.".into())],
            }),
        ],
    }))
    .build()
    .unwrap();

    let pdf = export_pdf(
        &doc,
        None,
        &ExportOptions {
            embed_source: false,
            ..Default::default()
        },
    )
    .unwrap();

    let ImportOutcome::Recovered { document, via, .. } = import_pdf(&pdf, &TextRecovery).unwrap()
    else {
        panic!("a foreign tagged PDF must take the recovery path, not lossless");
    };
    assert_eq!(via, "vsd-pdf/tagged-recovery", "tagged walker must run");

    let Node::Doc(d) = document.root_node().unwrap() else {
        panic!("doc root");
    };
    // Headings recovered with their levels, in order, interleaved with
    // paragraphs carrying the source text.
    let headings: Vec<(u8, String)> = d
        .children
        .iter()
        .filter_map(|n| match n {
            Node::Heading(h) => Some((h.level, inline_text(&h.children))),
            _ => None,
        })
        .collect();
    assert_eq!(
        headings,
        vec![(1, "Annual Report".to_string()), (2, "Summary".to_string())]
    );
    let text = vsd_core::extract::extract_text(&document).unwrap();
    assert!(
        text.contains("opening paragraph of the report"),
        "text: {text}"
    );
    assert!(text.contains("short summary follows"), "text: {text}");

    // Still honestly marked lossy, original attached.
    let prov = document.provenance().unwrap().unwrap();
    assert!(prov.assertions[0]
        .claims
        .iter()
        .any(|(k, v)| k == "tool" && v == "vsd-pdf/tagged-recovery"));
    assert!(vsd_core::validate::validate(&document).is_valid());
}

/// A synthetic foreign tagged PDF with real `L`/`LI` and
/// `Table`/`TR`/`TH`/`TD` structure (which our own exporter does not
/// emit) — exercises list and table reconstruction from marked content.
#[test]
fn foreign_tagged_pdf_recovers_lists_and_tables() {
    let pdf = tagged_list_table_pdf();

    let ImportOutcome::Recovered { document, via, .. } = import_pdf(&pdf, &TextRecovery).unwrap()
    else {
        panic!("tagged recovery expected");
    };
    assert_eq!(via, "vsd-pdf/tagged-recovery");

    let Node::Doc(d) = document.root_node().unwrap() else {
        panic!("doc root");
    };
    let list = d
        .children
        .iter()
        .find_map(|n| match n {
            Node::List(l) => Some(l),
            _ => None,
        })
        .expect("a list was recovered");
    assert_eq!(list.items.len(), 2, "two list items");
    assert!(!list.ordered, "Disc numbering → unordered");
    assert!(inline_text_of_block(&list.items[0][0]).contains("Apples"));
    assert!(inline_text_of_block(&list.items[1][0]).contains("Oranges"));

    let table = d
        .children
        .iter()
        .find_map(|n| match n {
            Node::Table(t) => Some(t),
            _ => None,
        })
        .expect("a table was recovered");
    assert_eq!(table.body.len(), 2, "two rows");
    assert_eq!(table.body[0].cells.len(), 2, "two columns");
    // The header row's cells carry header scope.
    assert!(table.body[0].cells[0].scope.is_some(), "TH → scope");
    assert!(table.body[1].cells[0].scope.is_none(), "TD → no scope");
    let cell_text = inline_text_of_block(&table.body[0].cells[0].children[0]);
    assert!(cell_text.contains("Name"), "header cell text: {cell_text}");
    let body_text = inline_text_of_block(&table.body[1].cells[1].children[0]);
    assert!(body_text.contains('7'), "body cell text: {body_text}");
}

fn inline_text(inls: &[Inline]) -> String {
    inls.iter()
        .map(|i| match i {
            Inline::Text(t) => t.clone(),
            _ => String::new(),
        })
        .collect()
}

fn inline_text_of_block(n: &Node) -> String {
    match n {
        Node::Para(p) => inline_text(&p.children),
        _ => String::new(),
    }
}

/// Hand-built tagged PDF: a Helvetica page whose six marked-content
/// sequences feed an `L` (two `LI`) and a `Table` (a `TH` header row and
/// a `TD` body row) in the structure tree.
fn tagged_list_table_pdf() -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Object, Stream};

    let mut doc = lopdf::Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    // One marked-content sequence per MCID, each showing a literal.
    let texts = ["Apples", "Oranges", "Name", "Qty", "Widget", "7"];
    let mut ops = Vec::new();
    let mut y = 700;
    for (mcid, t) in texts.iter().enumerate() {
        ops.push(Operation::new(
            "BDC",
            vec![
                Object::Name(b"Span".to_vec()),
                Object::Dictionary(dictionary! { "MCID" => mcid as i64 }),
            ],
        ));
        ops.push(Operation::new("BT", vec![]));
        ops.push(Operation::new("Tf", vec!["F1".into(), 12.into()]));
        ops.push(Operation::new("Td", vec![72.into(), y.into()]));
        ops.push(Operation::new("Tj", vec![Object::string_literal(*t)]));
        ops.push(Operation::new("ET", vec![]));
        ops.push(Operation::new("EMC", vec![]));
        y -= 20;
    }
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations: ops }.encode().unwrap(),
    ));

    // Leaf and grouping structure elements (children created first).
    let mk = |doc: &mut lopdf::Document, s: &str, k: Object, with_pg: bool| {
        let mut d = dictionary! { "Type" => "StructElem", "S" => s, "K" => k };
        if with_pg {
            d.set("Pg", page_id);
        }
        doc.add_object(d)
    };
    let mcid = |n: i64| Object::Integer(n);

    let lb1 = mk(&mut doc, "LBody", mcid(0), true);
    let li1 = mk(&mut doc, "LI", vec![lb1.into()].into(), false);
    let lb2 = mk(&mut doc, "LBody", mcid(1), true);
    let li2 = mk(&mut doc, "LI", vec![lb2.into()].into(), false);
    let mut list_d = dictionary! {
        "Type" => "StructElem", "S" => "L",
        "K" => vec![li1.into(), li2.into()],
    };
    // ListNumbering attribute → unordered (Disc).
    list_d.set(
        "A",
        dictionary! { "O" => "List", "ListNumbering" => "Disc" },
    );
    let list = doc.add_object(list_d);

    let th1 = mk(&mut doc, "TH", mcid(2), true);
    let th2 = mk(&mut doc, "TH", mcid(3), true);
    let tr1 = mk(&mut doc, "TR", vec![th1.into(), th2.into()].into(), false);
    let td1 = mk(&mut doc, "TD", mcid(4), true);
    let td2 = mk(&mut doc, "TD", mcid(5), true);
    let tr2 = mk(&mut doc, "TR", vec![td1.into(), td2.into()].into(), false);
    let tbody = mk(
        &mut doc,
        "TBody",
        vec![tr1.into(), tr2.into()].into(),
        false,
    );
    let table = mk(&mut doc, "Table", vec![tbody.into()].into(), false);

    let docelem = mk(
        &mut doc,
        "Document",
        vec![list.into(), table.into()].into(),
        false,
    );
    let struct_root = doc.add_object(dictionary! {
        "Type" => "StructTreeRoot",
        "K" => vec![docelem.into()],
    });

    doc.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "StructParents" => 0,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }),
    );
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "MarkInfo" => dictionary! { "Marked" => true },
        "StructTreeRoot" => struct_root,
    });
    doc.trailer.set("Root", catalog_id);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}
