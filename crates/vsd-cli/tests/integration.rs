//! End-to-end tests across the full stack: authoring → container →
//! verification → signing → redaction → diff.
//!
//! These are the format's first conformance vectors: they pin behavior
//! that the spec declares normative (determinism, tamper rejection,
//! destructive redaction, signature survival across repacking).

use vsd_container::{read_document, write_document, ReadOptions, WriteOptions};
use vsd_core::document::DocumentBuilder;
use vsd_core::manifest::{Blob, Metadata, Profile, ResourceEntry, ResourceKind};
use vsd_core::tree::{Direction, Doc, Figure, Heading, Inline, Node, Para, Section};
use vsd_core::{Document, ResourceTable};

fn sample_document() -> Document {
    let blob = Blob {
        mime: "image/png".into(),
        data: vec![0x89, 0x50, 0x4e, 0x47, 1, 2, 3, 4],
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
                children: vec![Inline::Text("Service Agreement".into())],
            }),
            Node::Section(Section {
                role: "terms".into(),
                columns: 1,
                children: vec![
                    Node::Para(Para {
                        children: vec![Inline::Text(
                            "The fee is $400 per month, payable in arrears.".into(),
                        )],
                    }),
                    Node::Para(Para {
                        children: vec![Inline::Text("Confidential: account 12-3456-789.".into())],
                    }),
                ],
            }),
            Node::Figure(Figure {
                res: blob_id,
                alt: "Company letterhead logo".into(),
                decorative: false,
                caption: vec![],
            }),
        ],
    });

    let mut builder = DocumentBuilder::new(root).metadata(Metadata {
        title: Some("Service Agreement".into()),
        authors: vec!["Alice".into()],
        created: Some("2026-06-10T00:00:00Z".into()),
        ..Default::default()
    });
    builder.add_object(blob.to_value()).unwrap();
    let resources = ResourceTable {
        entries: vec![(
            "logo".into(),
            ResourceEntry {
                kind: ResourceKind::Image,
                mime: "image/png".into(),
                data: blob_id,
            },
        )],
        styles: vec![],
    };
    builder
        .resources(resources)
        .profile(Profile::Core)
        .build()
        .unwrap()
}

#[test]
fn container_roundtrip_preserves_identity() {
    let doc = sample_document();
    let id = doc.document_id().unwrap();

    let bytes = write_document(&doc, &[], &WriteOptions::default()).unwrap();
    let back = read_document(&bytes, &ReadOptions::default()).unwrap();

    assert_eq!(back.document_id, id);
    assert_eq!(back.document.manifest, doc.manifest);
    assert_eq!(back.document.store.len(), doc.store.len());
    assert!(vsd_core::validate::validate(&back.document).is_valid());
}

#[test]
fn identity_survives_recompression() {
    // Spec §2.4: two files with different compression but the same
    // manifest hash are the same document.
    let doc = sample_document();
    let compressed = write_document(&doc, &[], &WriteOptions { compress: true }).unwrap();
    let raw = write_document(&doc, &[], &WriteOptions { compress: false }).unwrap();
    let a = read_document(&compressed, &ReadOptions::default()).unwrap();
    let b = read_document(&raw, &ReadOptions::default()).unwrap();
    assert_eq!(a.document_id, b.document_id);
}

#[test]
fn deterministic_output() {
    let doc = sample_document();
    let opts = WriteOptions::default();
    assert_eq!(
        write_document(&doc, &[], &opts).unwrap(),
        write_document(&doc, &[], &opts).unwrap(),
        "same document + same options must produce identical bytes"
    );
}

#[test]
fn tampering_is_detected() {
    let doc = sample_document();
    let bytes = write_document(&doc, &[], &WriteOptions { compress: false }).unwrap();

    // Find the confidential text inside the OBJS payload and flip it.
    let needle = b"12-3456-789";
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("uncompressed payload should contain the text");
    let mut tampered = bytes.clone();
    tampered[pos] ^= 0x01;

    // The reader must reject: chunk checksum, object hash, or both.
    assert!(read_document(&tampered, &ReadOptions::default()).is_err());

    // Truncation must be detected via the header size field.
    let truncated = &bytes[..bytes.len() - 5];
    assert!(read_document(truncated, &ReadOptions::default()).is_err());

    // Magic corruption (e.g. FTP text-mode mangling) must be detected.
    let mut bad_magic = bytes.clone();
    bad_magic[4] = b'\n';
    assert!(read_document(&bad_magic, &ReadOptions::default()).is_err());
}

#[test]
fn signature_lifecycle() {
    let doc = sample_document();
    let key = vsd_sign::SigningKey::generate();
    let sig = key.sign_document(&doc).unwrap();

    // Survives a container roundtrip and recompression.
    let bytes = write_document(&doc, &[sig], &WriteOptions { compress: true }).unwrap();
    let loaded = read_document(&bytes, &ReadOptions::default()).unwrap();
    assert_eq!(loaded.signatures.len(), 1);
    assert_eq!(
        vsd_sign::verify(&loaded.document, &loaded.signatures[0]).unwrap(),
        vsd_sign::Verdict::Valid
    );

    // A signature from a different document must not validate as "this
    // document" even though the cryptography is fine.
    let other = {
        let mut d = sample_document();
        let meta_id = d
            .store
            .put_value(
                &Metadata {
                    title: Some("Different".into()),
                    ..Default::default()
                }
                .to_value(),
            )
            .unwrap();
        d.manifest.metadata = meta_id;
        d
    };
    let foreign_sig = key.sign_document(&other).unwrap();
    assert_eq!(
        vsd_sign::verify(&doc, &foreign_sig).unwrap(),
        vsd_sign::Verdict::ValidForOtherTarget
    );
}

#[test]
fn redaction_destroys_content() {
    let doc = sample_document();
    // Path 1.1 = section → second paragraph (the confidential one).
    let result = vsd_core::redact::redact(&doc, &[1, 1], Some("bank details".into())).unwrap();
    let redacted = result.document;

    // The text is gone from every byte of the new file.
    let bytes = write_document(&redacted, &[], &WriteOptions { compress: false }).unwrap();
    let needle = b"12-3456-789";
    assert!(
        !bytes.windows(needle.len()).any(|w| w == needle),
        "redacted content must not survive anywhere in the container"
    );

    // The unredacted sibling is still present.
    let keep = b"$400 per month";
    assert!(bytes.windows(keep.len()).any(|w| w == keep));

    // The document is still valid, has no orphans, and chains to its
    // predecessor.
    let report = vsd_core::validate::validate(&redacted);
    assert!(report.is_valid(), "findings: {:?}", report.findings);
    assert_eq!(
        redacted.manifest.predecessor,
        Some(doc.document_id().unwrap())
    );

    // The proof matches the removed paragraph.
    let removed = Node::Para(Para {
        children: vec![Inline::Text("Confidential: account 12-3456-789.".into())],
    });
    assert!(vsd_core::redact::verify_proof(&result.proof, &removed).unwrap());

    // Extracted text shows the redaction marker, not the content.
    let text = vsd_core::extract::extract_text(&redacted).unwrap();
    assert!(text.contains("[REDACTED: bank details]"));
    assert!(!text.contains("12-3456-789"));
}

#[test]
fn diff_is_object_set_arithmetic() {
    let doc = sample_document();
    let redacted = vsd_core::redact::redact(&doc, &[1, 1], None)
        .unwrap()
        .document;

    let d = vsd_core::diff::diff(&doc, &redacted).unwrap();
    assert!(!d.same_document);
    // The blob, metadata, and resource table are shared; root, section
    // change. Shared must be substantial.
    assert!(d.shared >= 2, "expected object sharing, got {}", d.shared);
    assert!(!d.changed_paths.is_empty());
}

#[test]
fn text_extraction_is_exact() {
    let doc = sample_document();
    let text = vsd_core::extract::extract_text(&doc).unwrap();
    let expected = "Service Agreement\n\n\
                    The fee is $400 per month, payable in arrears.\n\n\
                    Confidential: account 12-3456-789.\n\n\
                    [Company letterhead logo]\n";
    assert_eq!(text, expected);
}

#[test]
fn alt_text_is_a_validity_condition() {
    let blob = Blob {
        mime: "image/png".into(),
        data: vec![1, 2, 3],
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
        children: vec![Node::Figure(Figure {
            res: blob_id,
            alt: "".into(), // missing alt, not decorative
            decorative: false,
            caption: vec![],
        })],
    });
    let mut builder = DocumentBuilder::new(root);
    builder.add_object(blob.to_value()).unwrap();
    let doc = builder.build().unwrap();

    let report = vsd_core::validate::validate(&doc);
    assert!(
        report.errors().any(|f| f.code == "E_ALT_TEXT"),
        "missing alt text must be a validation error, findings: {:?}",
        report.findings
    );
}

#[test]
fn unknown_critical_chunk_is_rejected() {
    let doc = sample_document();
    let bytes = write_document(&doc, &[], &WriteOptions { compress: false }).unwrap();

    // Append a fake critical chunk before the trailer... simpler: craft a
    // file with an extra unknown critical chunk by splicing one in at the
    // end and fixing sizes is complex; instead verify the flag logic via
    // a direct read of a corrupted type field. Rewrite the OBJS fourcc to
    // an unknown critical type.
    let mut hacked = bytes.clone();
    let pos = hacked
        .windows(4)
        .position(|w| w == b"OBJS")
        .expect("OBJS fourcc present");
    hacked[pos..pos + 4].copy_from_slice(b"EVIL");
    let err = read_document(&hacked, &ReadOptions::default());
    assert!(err.is_err());
}

#[test]
fn forms_profile_and_evaluation() {
    use std::collections::BTreeMap;
    use vsd_core::forms::{Expr, FieldValue};
    use vsd_core::tree::{Field, FieldKind};

    let root = Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![
            Node::Field(Field {
                id: "qty".into(),
                kind: FieldKind::Number,
                label: Some("Quantity".into()),
                required: true,
                constraint: Some(Expr::Cmp(
                    vsd_core::forms::CmpOp::Ge,
                    Box::new(Expr::FieldRef("qty".into())),
                    Box::new(Expr::Num(1.0)),
                )),
                computed: None,
            }),
            Node::Field(Field {
                id: "total".into(),
                kind: FieldKind::Number,
                label: None,
                required: false,
                constraint: None,
                computed: Some(Expr::Arith(
                    vsd_core::forms::ArithOp::Mul,
                    vec![Expr::FieldRef("qty".into()), Expr::Num(9.5)],
                )),
            }),
        ],
    });
    let doc = DocumentBuilder::new(root)
        .profile(Profile::Form)
        .build()
        .unwrap();
    assert!(vsd_core::validate::validate(&doc).is_valid());

    let fields = doc.fields().unwrap();
    assert_eq!(fields.len(), 2);

    let mut env = BTreeMap::new();
    env.insert("qty".to_string(), FieldValue::Num(4.0));
    let computed = fields[1].computed.as_ref().unwrap().eval(&env).unwrap();
    assert_eq!(computed, FieldValue::Num(38.0));

    // Archive profile must reject the field layer.
    let archive_doc = {
        let mut d = doc.clone();
        d.manifest.profile = Profile::Archive;
        d
    };
    let report = vsd_core::validate::validate(&archive_doc);
    assert!(report.errors().any(|f| f.code == "E_ARCHIVE_FIELDS"));
}

#[test]
fn fill_and_flatten_lifecycle() {
    use std::collections::BTreeMap;
    use vsd_core::forms::{ArithOp, CmpOp, Expr, FieldValue};
    use vsd_core::tree::{Field, FieldKind};

    let root = Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![
            Node::Para(Para {
                children: vec![Inline::Text("Order form".into())],
            }),
            Node::Field(Field {
                id: "qty".into(),
                kind: FieldKind::Number,
                label: None,
                required: true,
                constraint: Some(Expr::Cmp(
                    CmpOp::Ge,
                    Box::new(Expr::FieldRef("qty".into())),
                    Box::new(Expr::Num(1.0)),
                )),
                computed: None,
            }),
            Node::Field(Field {
                id: "total".into(),
                kind: FieldKind::Number,
                label: None,
                required: false,
                constraint: None,
                computed: Some(Expr::Arith(
                    ArithOp::Mul,
                    vec![Expr::FieldRef("qty".into()), Expr::Num(9.5)],
                )),
            }),
        ],
    });
    let blank = DocumentBuilder::new(root)
        .profile(Profile::Form)
        .build()
        .unwrap();

    // Flattening an unfilled form with a required field must fail.
    assert!(vsd_core::fill::flatten(&blank).is_err());

    // Fill with a violating value: saved, but flagged.
    let mut bad = BTreeMap::new();
    bad.insert("qty".to_string(), FieldValue::Num(0.0));
    let r = vsd_core::fill::fill(&blank, &bad).unwrap();
    assert_eq!(r.violations.len(), 1);

    // Fill correctly; layer rides the container; validation passes.
    let mut good = BTreeMap::new();
    good.insert("qty".to_string(), FieldValue::Num(4.0));
    let filled = vsd_core::fill::fill(&blank, &good).unwrap();
    assert!(filled.violations.is_empty());
    let filled = filled.document;
    assert_eq!(
        filled.manifest.predecessor,
        Some(blank.document_id().unwrap())
    );
    let bytes = write_document(&filled, &[], &WriteOptions::default()).unwrap();
    let back = read_document(&bytes, &ReadOptions::default()).unwrap();
    assert!(vsd_core::validate::validate(&back.document).is_valid());

    // Flatten: fields become final values, computed fields included.
    let flat = vsd_core::fill::flatten(&back.document).unwrap();
    assert!(flat.manifest.field_layer.is_none());
    assert!(flat.fields().unwrap().is_empty());
    let text = vsd_core::extract::extract_text(&flat).unwrap();
    assert!(text.contains('4'), "filled value missing: {text}");
    assert!(text.contains("38"), "computed total missing: {text}");
    assert!(vsd_core::validate::validate(&flat).is_valid());
}

#[test]
fn computed_cycle_is_rejected() {
    use vsd_core::forms::{ArithOp, Expr};
    use vsd_core::tree::{Field, FieldKind};

    let mk = |id: &str, dep: &str| {
        Node::Field(Field {
            id: id.into(),
            kind: FieldKind::Number,
            label: None,
            required: false,
            constraint: None,
            computed: Some(Expr::Arith(
                ArithOp::Add,
                vec![Expr::FieldRef(dep.into()), Expr::Num(1.0)],
            )),
        })
    };
    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![mk("a", "b"), mk("b", "a")],
    }))
    .profile(Profile::Form)
    .build()
    .unwrap();
    let report = vsd_core::validate::validate(&doc);
    assert!(report.errors().any(|f| f.code == "E_FIELD_CYCLE"));
}

#[test]
fn stream_reader_lazy_access() {
    use std::io::Cursor;
    use vsd_container::StreamReader;

    let doc = sample_document();
    for compress in [false, true] {
        let bytes = write_document(&doc, &[], &WriteOptions { compress }).unwrap();
        let mut reader = StreamReader::open(Cursor::new(bytes)).unwrap();
        assert_eq!(reader.document_id(), doc.document_id().unwrap());
        assert_eq!(reader.object_count(), doc.store.len());

        // Lazily fetch and verify the root object.
        let root_id = reader.manifest().root;
        let root = reader.object(&root_id).unwrap();
        let node = Node::from_value(&root).unwrap();
        assert!(matches!(node, Node::Doc(_)));
    }
}

#[test]
fn stream_reader_rejects_substituted_object() {
    use std::io::Cursor;
    use vsd_container::StreamReader;

    let doc = sample_document();
    let bytes = write_document(&doc, &[], &WriteOptions { compress: false }).unwrap();

    // Flip a byte inside the OBJS region (find the fee text).
    let needle = b"$400 per month";
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap();
    let mut tampered = bytes.clone();
    tampered[pos] ^= 0x01;

    // Opening succeeds (header/trailer/index untouched)…
    let mut reader = StreamReader::open(Cursor::new(tampered)).unwrap();
    let root_id = reader.manifest().root;
    // …but fetching the tampered object fails hash verification.
    assert!(reader.object(&root_id).is_err());
}

#[test]
fn layout_roundtrip_and_recompute() {
    let doc = sample_document();
    let laid = vsd_layout::add_render_cache(&doc, &vsd_layout::LayoutOptions::default()).unwrap();

    // Identity changed (manifest commits to the cache); chain recorded.
    assert_eq!(laid.manifest.predecessor, Some(doc.document_id().unwrap()));
    let cache = laid.render_cache().unwrap().unwrap();
    assert_eq!(cache.engine_name, vsd_layout::ENGINE_NAME);
    assert!(!cache.pages.is_empty());

    // Survives the container; validates; recomputes byte-identically.
    let bytes = write_document(&laid, &[], &WriteOptions::default()).unwrap();
    let back = read_document(&bytes, &ReadOptions::default()).unwrap();
    let report = vsd_core::validate::validate(&back.document);
    assert!(report.is_valid(), "findings: {:?}", report.findings);
    assert_eq!(report.warnings().count(), 0, "no orphans expected");
    assert!(matches!(
        vsd_layout::verify_render_cache(&back.document).unwrap(),
        vsd_layout::RecomputeOutcome::Match { .. }
    ));

    // The page index closure carries the figure's blob for its page.
    let pi_id = back.document.manifest.page_index.unwrap();
    let pi =
        vsd_core::manifest::PageIndex::from_value(&back.document.store.get_value(&pi_id).unwrap())
            .unwrap();
    let all: Vec<_> = pi.pages.iter().flatten().collect();
    let resources = back.document.resources().unwrap();
    let blob_id = resources.entries[0].1.data;
    assert!(
        all.contains(&&blob_id),
        "page closure must include placed resources"
    );

    // Text runs carry back-references into the tree.
    let page = vsd_core::layout::Page::from_value(
        &back.document.store.get_value(&cache.pages[0]).unwrap(),
    )
    .unwrap();
    let has_fee_run = page.ops.iter().any(|op| {
        matches!(op, vsd_core::layout::DisplayOp::TextRun { text, node_path, .. }
            if text.contains("$400") && node_path == &vec![1, 0])
    });
    assert!(
        has_fee_run,
        "fee paragraph must be a text run with path 1.0"
    );
}

#[test]
fn lying_render_cache_is_caught_only_by_recompute() {
    // The §5.3 attack: a render cache whose pixels disagree with the
    // content tree. Build document A, but graft on the cache generated
    // from document B (same shape, different fee).
    let a = sample_document();
    let b = {
        let mut builder = DocumentBuilder::new(Node::Doc(Doc {
            lang: "en".into(),
            dir: Direction::Ltr,
            writing_mode: vsd_core::tree::WritingMode::Horizontal,
            children: vec![Node::Para(Para {
                children: vec![Inline::Text(
                    "The fee is $800 per month, payable in arrears.".into(),
                )],
            })],
        }));
        let _ = &mut builder;
        builder.build().unwrap()
    };
    let laid_b = vsd_layout::add_render_cache(&b, &vsd_layout::LayoutOptions::default()).unwrap();

    // Franken-document: A's content, B's render cache.
    let mut store = a.store.clone();
    for (id, bytes) in laid_b.store.iter() {
        store.put_verified(bytes.to_vec(), Some(*id)).unwrap();
    }
    let manifest = vsd_core::Manifest {
        render_cache: laid_b.manifest.render_cache,
        page_index: laid_b.manifest.page_index,
        ..a.manifest.clone()
    };
    let franken = Document { manifest, store };

    // Every *structural* check passes: object hashes are genuine, the
    // layout-hash is consistent with its own page list…
    let report = vsd_core::validate::validate(&franken);
    assert!(
        report.is_valid(),
        "structural validation cannot catch a grafted cache: {:?}",
        report.findings
    );
    // …only recomputation exposes that the pixels lie about the tree.
    assert!(matches!(
        vsd_layout::verify_render_cache(&franken).unwrap(),
        vsd_layout::RecomputeOutcome::Mismatch { .. }
    ));
}

#[test]
fn long_documents_paginate_deterministically() {
    let children: Vec<Node> = (0..120)
        .map(|i| {
            Node::Para(Para {
                children: vec![Inline::Text(format!(
                    "Paragraph {i}: the quick brown fox jumps over the lazy dog, \
                     repeatedly and at considerable length, to fill the measure."
                ))],
            })
        })
        .collect();
    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children,
    }))
    .build()
    .unwrap();

    let opts = vsd_layout::LayoutOptions::default();
    let pages = vsd_layout::layout_document(&doc, &opts).unwrap();
    assert!(pages.len() > 1, "120 paragraphs must span multiple pages");
    for p in &pages {
        assert!(!p.ops.is_empty(), "no empty pages in the middle");
    }
    // Determinism: laying out twice yields identical page encodings.
    let again = vsd_layout::layout_document(&doc, &opts).unwrap();
    assert_eq!(pages.len(), again.len());
    for (x, y) in pages.iter().zip(&again) {
        assert_eq!(
            x.to_value().encode().unwrap(),
            y.to_value().encode().unwrap()
        );
    }
}

#[test]
fn rtl_is_refused_by_engine_1_0() {
    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "he".into(),
        dir: Direction::Rtl,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![],
    }))
    .build()
    .unwrap();
    // Engines 1.0/1.1 predate bidi: dir=rtl is refused, never guessed.
    for engine in [
        vsd_layout::EngineVersion::V1_0,
        vsd_layout::EngineVersion::V1_1,
    ] {
        assert!(matches!(
            vsd_layout::layout_document(
                &doc,
                &vsd_layout::LayoutOptions::default().with_engine(engine),
            ),
            Err(vsd_layout::LayoutError::Unsupported(_))
        ));
    }
    // Engine 1.2 (the default) lays RTL documents out.
    assert!(vsd_layout::layout_document(&doc, &vsd_layout::LayoutOptions::default()).is_ok());
}

#[test]
fn rasterizer_renders_and_is_stable() {
    let doc = sample_document();
    let laid = vsd_layout::add_render_cache(&doc, &vsd_layout::LayoutOptions::default()).unwrap();
    let cache = laid.render_cache().unwrap().unwrap();
    let page = vsd_core::layout::Page::from_value(&laid.store.get_value(&cache.pages[0]).unwrap())
        .unwrap();

    let png1 = vsd_render::render_page_png(&laid, &page, 96.0).unwrap();
    let png2 = vsd_render::render_page_png(&laid, &page, 96.0).unwrap();
    assert_eq!(png1, png2, "rasterization must be stable");

    // The page is not blank: some non-white pixel exists.
    let pixmap = vsd_render::render_page(&laid, &page, 96.0).unwrap();
    let blank = pixmap
        .pixels()
        .iter()
        .all(|p| p.red() == 255 && p.green() == 255 && p.blue() == 255);
    assert!(!blank, "rendered page must contain ink");
}

#[test]
fn redaction_invalidates_render_cache_and_relayout_recovers() {
    let doc = sample_document();
    let laid = vsd_layout::add_render_cache(&doc, &vsd_layout::LayoutOptions::default()).unwrap();
    let redacted = vsd_core::redact::redact(&laid, &[1, 1], Some("account".into())).unwrap();
    assert!(redacted.cache_invalidated);
    assert!(redacted.document.manifest.render_cache.is_none());

    // Re-layout the redacted document: the redaction bar is in the cache,
    // the secret is not.
    let relaid =
        vsd_layout::add_render_cache(&redacted.document, &vsd_layout::LayoutOptions::default())
            .unwrap();
    assert!(matches!(
        vsd_layout::verify_render_cache(&relaid).unwrap(),
        vsd_layout::RecomputeOutcome::Match { .. }
    ));
    let bytes = write_document(&relaid, &[], &WriteOptions { compress: false }).unwrap();
    let needle = b"12-3456-789";
    assert!(!bytes.windows(needle.len()).any(|w| w == needle));
}

#[test]
fn hybrid_signature_lifecycle_through_container() {
    let doc = sample_document();
    let key = vsd_sign::HybridSigningKey::generate().unwrap();
    let sig = key.sign_document(&doc).unwrap();

    // Survives the container round trip and verifies.
    let bytes = write_document(&doc, &[sig], &WriteOptions::default()).unwrap();
    let loaded = read_document(&bytes, &ReadOptions::default()).unwrap();
    assert_eq!(
        loaded.signatures[0].alg,
        vsd_container::SigAlg::HybridEd25519MlDsa65
    );
    assert_eq!(
        vsd_sign::verify(&loaded.document, &loaded.signatures[0]).unwrap(),
        vsd_sign::Verdict::Valid
    );

    // A signature from different content is bound to its own target.
    let other = {
        let mut d = sample_document();
        let meta = d
            .store
            .put_value(
                &Metadata {
                    title: Some("Other".into()),
                    ..Default::default()
                }
                .to_value(),
            )
            .unwrap();
        d.manifest.metadata = meta;
        d
    };
    let foreign = key.sign_document(&other).unwrap();
    assert_eq!(
        vsd_sign::verify(&doc, &foreign).unwrap(),
        vsd_sign::Verdict::ValidForOtherTarget
    );
}

#[test]
fn transparency_log_anchors_document_history() {
    // Blank → filled → flattened: log each revision, prove inclusion of
    // all three, and prove the log only ever grew.
    let doc = sample_document();
    let redacted = vsd_core::redact::redact(&doc, &[1, 1], None)
        .unwrap()
        .document;

    let mut log = vsd_tlog::Log::new();
    log.append(doc.document_id().unwrap().0);
    let old_size = log.size();
    let old_root = log.root();

    log.append(redacted.document_id().unwrap().0);
    let n = log.size();
    let root = log.root();

    // Inclusion of both revisions.
    for (i, d) in [&doc, &redacted].iter().enumerate() {
        let proof = log.inclusion_proof(i as u64, n).unwrap();
        vsd_tlog::verify_inclusion(&d.document_id().unwrap().0, i as u64, n, &proof, &root)
            .unwrap();
    }
    // Append-only consistency from size 1 to size 2.
    let cproof = log.consistency_proof(old_size, n).unwrap();
    vsd_tlog::verify_consistency(old_size, n, &old_root, &root, &cproof).unwrap();
}

#[test]
fn sealed_disclosure_round_trip_with_signature() {
    // The 5f story end to end: seal, sign the sealed doc, disclose one
    // block; the verifier checks the disclosure against the *signed* id.
    let doc = sample_document();
    let sealed = vsd_core::disclose::seal(&doc).unwrap();
    let key = vsd_sign::SigningKey::generate();
    let sig = key.sign_document(&sealed).unwrap();
    assert_eq!(
        vsd_sign::verify(&sealed, &sig).unwrap(),
        vsd_sign::Verdict::Valid
    );

    let bundle = vsd_core::disclose::disclose(&sealed, 0).unwrap();
    let encoded = bundle.encode().unwrap();

    // Receiver side: decode, verify against the signed document id.
    let decoded = vsd_core::disclose::Disclosure::decode(&encoded).unwrap();
    let verified =
        vsd_core::disclose::verify_disclosure(&decoded, Some(sealed.document_id().unwrap()))
            .unwrap();
    assert!(matches!(verified.subtree, Node::Heading(_)));
    // The confidential paragraph (in a hidden sibling) must not leak.
    let secret = b"12-3456-789";
    assert!(!encoded.windows(secret.len()).any(|w| w == secret));
}

/// A document whose paragraph carries a bold span (style table entry 0).
fn styled_document() -> Document {
    use vsd_core::manifest::Style;
    use vsd_core::tree::Span;

    let root = Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![
            Node::Heading(Heading {
                level: 1,
                children: vec![Inline::Text("Faces".into())],
            }),
            Node::Para(Para {
                children: vec![
                    Inline::Text("plain then ".into()),
                    Inline::Span(Span {
                        style: Some(0),
                        children: vec![Inline::Text("bold words".into())],
                    }),
                    Inline::Text(" then plain again".into()),
                ],
            }),
        ],
    });
    DocumentBuilder::new(root)
        .resources(ResourceTable {
            entries: vec![],
            styles: vec![Style {
                bold: true,
                italic: false,
                underline: false,
                mono: false,
            }],
        })
        .build()
        .unwrap()
}

#[test]
fn engine_versions_are_dispatched_and_both_verifiable() {
    use vsd_layout::{EngineVersion, LayoutOptions};

    let doc = styled_document();

    // Engine 1.0: styles affect nothing; every run is the regular face.
    let opts_10 = LayoutOptions::default().with_engine(EngineVersion::V1_0);
    let pages_10 = vsd_layout::layout_document(&doc, &opts_10).unwrap();
    for page in &pages_10 {
        for op in &page.ops {
            if let vsd_core::layout::DisplayOp::TextRun { font, .. } = op {
                assert_eq!(*font, 0, "engine 1.0 must never emit non-regular faces");
            }
        }
    }

    // Engine 1.1: the bold span gets the bold face and wider advances.
    let opts_11 = LayoutOptions::default().with_engine(EngineVersion::V1_1);
    let pages_11 = vsd_layout::layout_document(&doc, &opts_11).unwrap();
    let bold_runs: Vec<&str> = pages_11
        .iter()
        .flat_map(|p| &p.ops)
        .filter_map(|op| match op {
            vsd_core::layout::DisplayOp::TextRun { font: 1, text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        bold_runs.iter().any(|t| t.contains("bold")),
        "bold span must become a bold-face run, got {bold_runs:?}"
    );

    // Both versions produce caches that recompute byte-identically —
    // and each cache pins its version, forever.
    for opts in [opts_10, opts_11] {
        let laid = vsd_layout::add_render_cache(&doc, &opts).unwrap();
        let cache = laid.render_cache().unwrap().unwrap();
        assert_eq!(cache.engine_version, opts.engine.as_str());
        assert!(matches!(
            vsd_layout::verify_render_cache(&laid).unwrap(),
            vsd_layout::RecomputeOutcome::Match { .. }
        ));
    }

    // The two versions disagree about this document (bold is wider), so
    // their caches must differ — version pinning is load-bearing.
    let laid_10 = vsd_layout::add_render_cache(&doc, &opts_10).unwrap();
    let laid_11 = vsd_layout::add_render_cache(&doc, &opts_11).unwrap();
    assert_ne!(
        laid_10.render_cache().unwrap().unwrap().layout_hash,
        laid_11.render_cache().unwrap().unwrap().layout_hash,
    );
}

/// Engine 1.2 (LAYOUT-1.2.md): mono spans and code blocks, underline
/// rects, Hebrew via per-script fallback with UAX #9 ordering, and
/// refusal of scripts the engine cannot set faithfully.
#[test]
fn engine_1_2_widened_typography() {
    use vsd_core::layout::DisplayOp;
    use vsd_core::manifest::Style;
    use vsd_core::tree::Span;
    use vsd_layout::{EngineVersion, LayoutOptions};

    let root = Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![
            Node::Para(Para {
                children: vec![
                    Inline::Text("call ".into()),
                    Inline::Span(Span {
                        style: Some(0),
                        children: vec![Inline::Text("vsd_verify()".into())],
                    }),
                    Inline::Text(" or ".into()),
                    Inline::Span(Span {
                        style: Some(1),
                        children: vec![Inline::Text("underlined".into())],
                    }),
                    Inline::Text(" then שלום עולם closes it".into()),
                ],
            }),
            Node::Code(vsd_core::tree::Code {
                lang: Some("rust".into()),
                text: "fn main() {}".into(),
            }),
        ],
    });
    let doc = DocumentBuilder::new(root)
        .resources(ResourceTable {
            entries: vec![],
            styles: vec![
                Style {
                    bold: false,
                    italic: false,
                    underline: false,
                    mono: true,
                },
                Style {
                    bold: false,
                    italic: false,
                    underline: true,
                    mono: false,
                },
            ],
        })
        .build()
        .unwrap();

    // This test pins engine 1.2 specifically (the default is newer).
    let opts = LayoutOptions::default().with_engine(EngineVersion::V1_2);
    assert_eq!(opts.engine.as_str(), "1.2.0");
    let pages = vsd_layout::layout_document(&doc, &opts).unwrap();
    let runs: Vec<(&u64, &bool, &String)> = pages
        .iter()
        .flat_map(|p| &p.ops)
        .filter_map(|op| match op {
            DisplayOp::TextRun {
                font, rtl, text, ..
            } => Some((font, rtl, text)),
            _ => None,
        })
        .collect();

    // The mono span and the code block both set in the mono face (4).
    assert!(runs
        .iter()
        .any(|(f, _, t)| **f == 4 && t.contains("vsd_verify")));
    assert!(runs
        .iter()
        .any(|(f, _, t)| **f == 4 && t.contains("fn main")));
    // Hebrew words come from the Hebrew face (5) as RTL runs in logical
    // order — the display list stores text, not reordered glyphs.
    assert!(runs
        .iter()
        .any(|(f, rtl, t)| **f == 5 && **rtl && t.contains("שלום")));
    // LTR words around them never carry the flag.
    assert!(runs.iter().all(|(_, rtl, t)| !t.contains("call") || !**rtl));
    // The underline produced a hairline rect (0.1 mm — same rule width
    // as everywhere else in the contract).
    assert!(pages.iter().flat_map(|p| &p.ops).any(|op| matches!(
        op,
        DisplayOp::Rect { h, .. } if (*h - 0.1).abs() < 1e-9
    )));

    // The cache pins 1.2.0 and recomputes byte-identically.
    let laid = vsd_layout::add_render_cache(&doc, &opts).unwrap();
    let cache = laid.render_cache().unwrap().unwrap();
    assert_eq!(cache.engine_version, "1.2.0");
    assert!(matches!(
        vsd_layout::verify_render_cache(&laid).unwrap(),
        vsd_layout::RecomputeOutcome::Match { .. }
    ));

    // And the page raster + PDF export accept the new faces and flag.
    let page = vsd_core::layout::Page::from_value(&laid.store.get_value(&cache.pages[0]).unwrap())
        .unwrap();
    assert!(!vsd_render::render_page_png(&laid, &page, 96.0)
        .unwrap()
        .is_empty());
    assert!(
        !vsd_pdf::export_pdf(&laid, None, &vsd_pdf::ExportOptions::default())
            .unwrap()
            .is_empty()
    );
}

/// Engine 1.2 justifies body paragraphs: every line but the last ends
/// flush at the right content edge. Frozen engines stay ragged.
#[test]
fn engine_1_2_justifies_body_paragraphs() {
    use vsd_core::layout::DisplayOp;
    use vsd_layout::font::{Face, FontMetrics};
    use vsd_layout::{EngineVersion, LayoutOptions};

    let words = "the quick brown fox jumps over the lazy dog ".repeat(8);
    let root = Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![Node::Para(Para {
            children: vec![Inline::Text(words)],
        })],
    });
    let doc = DocumentBuilder::new(root).build().unwrap();

    let line_extents = |engine: EngineVersion| -> Vec<f64> {
        let opts = LayoutOptions::default().with_engine(engine);
        let pages = vsd_layout::layout_document(&doc, &opts).unwrap();
        // Right edge of each baseline's last run, in mm.
        let mut lines: std::collections::BTreeMap<i64, f64> = Default::default();
        for op in pages.iter().flat_map(|p| &p.ops) {
            if let DisplayOp::TextRun {
                x,
                y,
                size_pt,
                text,
                font,
                ..
            } = op
            {
                let m = FontMetrics::face_metrics(Face::from_index(*font));
                let w_mm = text
                    .chars()
                    .map(|c| m.char_advance_um(c, (size_pt * 25400.0 / 72.0) as i64) as f64)
                    .sum::<f64>()
                    / 1000.0;
                let key = (y * 1000.0) as i64;
                let right = x + w_mm;
                lines
                    .entry(key)
                    .and_modify(|r| *r = r.max(right))
                    .or_insert(right);
            }
        }
        lines.into_values().collect()
    };

    // A4 content right edge: 210 − 20 mm margin.
    let extents_12 = line_extents(EngineVersion::V1_2);
    assert!(extents_12.len() > 2, "paragraph must wrap");
    for (i, right) in extents_12.iter().enumerate() {
        if i + 1 < extents_12.len() {
            assert!(
                (right - 190.0).abs() < 0.05,
                "justified line {i} must end flush at 190mm, got {right}"
            );
        } else {
            assert!(*right < 189.0, "last line stays ragged");
        }
    }
    // Engine 1.1 (frozen contract): ragged right everywhere.
    let extents_11 = line_extents(EngineVersion::V1_1);
    assert!(extents_11
        .iter()
        .take(extents_11.len() - 1)
        .any(|r| (r - 190.0).abs() > 0.05));
}

/// A dir=rtl document right-aligns its line boxes: the visually last
/// run ends flush at the right content edge (190 mm on A4).
#[test]
fn engine_1_2_rtl_documents_right_align() {
    use vsd_core::layout::DisplayOp;
    use vsd_layout::font::{Face, FontMetrics};
    use vsd_layout::{EngineVersion, LayoutOptions};

    let root = Node::Doc(Doc {
        lang: "he".into(),
        dir: Direction::Rtl,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![Node::Para(Para {
            children: vec![Inline::Text("שלום עולם".into())],
        })],
    });
    let doc = DocumentBuilder::new(root).build().unwrap();
    // Pin 1.2 (plain RTL, no mirroring): the default has moved forward.
    let pages = vsd_layout::layout_document(
        &doc,
        &LayoutOptions::default().with_engine(EngineVersion::V1_2),
    )
    .unwrap();
    let mut right_edge = f64::NEG_INFINITY;
    let mut saw_rtl = false;
    for op in pages.iter().flat_map(|p| &p.ops) {
        if let DisplayOp::TextRun {
            x,
            font,
            size_pt,
            rtl,
            text,
            ..
        } = op
        {
            saw_rtl |= rtl;
            let m = FontMetrics::face_metrics(Face::from_index(*font));
            let w_mm = text
                .chars()
                .map(|c| m.char_advance_um(c, (size_pt * 25400.0 / 72.0) as i64) as f64)
                .sum::<f64>()
                / 1000.0;
            right_edge = right_edge.max(x + w_mm);
        }
    }
    assert!(saw_rtl, "Hebrew text must produce rtl runs");
    assert!(
        (right_edge - 190.0).abs() < 0.05,
        "rtl line must end flush at the right margin, got {right_edge}"
    );
}

/// Scripts engine 1.2 cannot set faithfully are refused with a clear
/// error — never silently drawn as .notdef boxes. Engine 1.0's frozen
/// contract (everything maps to .notdef) is unchanged.
#[test]
fn engine_1_2_refuses_unsupported_scripts() {
    use vsd_layout::{EngineVersion, LayoutOptions};

    for sample in ["مرحبا بالعالم", "你好世界", "नमस्ते"] {
        let root = Node::Doc(Doc {
            lang: "en".into(),
            dir: Direction::Ltr,
            writing_mode: vsd_core::tree::WritingMode::Horizontal,
            children: vec![Node::Para(Para {
                children: vec![Inline::Text(format!("mixed {sample} text"))],
            })],
        });
        let doc = DocumentBuilder::new(root).build().unwrap();
        // Pin engine 1.2: it refuses all three (1.4 shapes Arabic and
        // Devanagari, so the default would accept two of them).
        let err = vsd_layout::layout_document(
            &doc,
            &LayoutOptions::default().with_engine(EngineVersion::V1_2),
        );
        assert!(
            matches!(err, Err(vsd_layout::LayoutError::Unsupported(_))),
            "engine 1.2 must refuse {sample:?}"
        );
        // The frozen 1.0 contract still lays it out (as .notdef).
        assert!(vsd_layout::layout_document(
            &doc,
            &LayoutOptions::default().with_engine(EngineVersion::V1_0)
        )
        .is_ok());
    }
}

#[test]
fn incremental_relayout_reuses_fragments_and_matches_from_scratch() {
    use vsd_layout::{layout_document, layout_document_with_session, LayoutOptions, LayoutSession};

    let make_doc = |changed: bool| {
        let children: Vec<Node> = (0..120)
            .map(|i| {
                let text = if changed && i == 60 {
                    "Paragraph 60: EDITED — the only paragraph that changed.".to_string()
                } else {
                    format!(
                        "Paragraph {i}: the quick brown fox jumps over the lazy dog, \
                         repeatedly and at considerable length, to fill the measure."
                    )
                };
                Node::Para(Para {
                    children: vec![Inline::Text(text)],
                })
            })
            .collect();
        DocumentBuilder::new(Node::Doc(Doc {
            lang: "en".into(),
            dir: Direction::Ltr,
            writing_mode: vsd_core::tree::WritingMode::Horizontal,
            children,
        }))
        .build()
        .unwrap()
    };

    let opts = LayoutOptions::default();
    let mut session = LayoutSession::new();

    // First layout: all misses, output identical to the plain path.
    let doc_a = make_doc(false);
    let warm = layout_document_with_session(&doc_a, &opts, Some(&mut session)).unwrap();
    assert_eq!(session.misses, 120);
    assert_eq!(session.hits, 0);
    let fresh = layout_document(&doc_a, &opts).unwrap();
    assert_eq!(warm.len(), fresh.len());
    for (a, b) in warm.iter().zip(&fresh) {
        assert_eq!(
            a.to_value().encode().unwrap(),
            b.to_value().encode().unwrap(),
            "session layout must be byte-identical to from-scratch layout"
        );
    }

    // Identical document again: zero shaping work.
    let again = layout_document_with_session(&doc_a, &opts, Some(&mut session)).unwrap();
    assert_eq!(session.hits, 120);
    assert_eq!(session.misses, 0);
    assert_eq!(again.len(), warm.len());

    // One edited paragraph: exactly one fragment re-shapes (the
    // per-section layout fence of ROADMAP 2g), and the result still
    // matches a from-scratch layout of the edited document.
    let doc_b = make_doc(true);
    let incremental = layout_document_with_session(&doc_b, &opts, Some(&mut session)).unwrap();
    assert_eq!(session.misses, 1, "only the edited paragraph re-shapes");
    assert_eq!(session.hits, 119);
    let fresh_b = layout_document(&doc_b, &opts).unwrap();
    for (a, b) in incremental.iter().zip(&fresh_b) {
        assert_eq!(
            a.to_value().encode().unwrap(),
            b.to_value().encode().unwrap()
        );
    }
}

#[test]
fn subtree_signature_scope() {
    let doc = sample_document();
    let key = vsd_sign::SigningKey::generate();

    // Sign the root subtree object (in a real flow: the fee schedule).
    let subtree_sig = key.sign_subtree(doc.manifest.root);
    assert_eq!(
        vsd_sign::verify(&doc, &subtree_sig).unwrap(),
        vsd_sign::Verdict::Valid
    );

    // After redaction the root object changes; the old subtree signature
    // no longer matches an object in the new document.
    let redacted = vsd_core::redact::redact(&doc, &[1, 1], None)
        .unwrap()
        .document;
    assert_eq!(
        vsd_sign::verify(&redacted, &subtree_sig).unwrap(),
        vsd_sign::Verdict::ValidForOtherTarget
    );
}

/// Engine 1.3 (LAYOUT-1.3.md): English body text is hyphenated with the
/// pinned Knuth–Liang patterns; the inserted hyphen is decoration (an
/// empty char_range), language-gated, and the cache pins 1.3.0 and
/// recomputes byte-identically. The 1.0/1.1/1.2 contracts are untouched.
#[test]
fn engine_1_3_hyphenates_english_body_only() {
    use vsd_core::layout::DisplayOp;
    use vsd_layout::{EngineVersion, LayoutOptions, RecomputeOutcome};

    // A long word-heavy paragraph and a narrow page force breaks.
    let prose = "Internationalization and the establishment of comprehensive \
                 documentation standards require extraordinarily collaborative \
                 organizations working methodically toward interoperability."
        .to_string();
    let make = |lang: &str| {
        DocumentBuilder::new(Node::Doc(Doc {
            lang: lang.into(),
            dir: Direction::Ltr,
            writing_mode: vsd_core::tree::WritingMode::Horizontal,
            children: vec![Node::Para(Para {
                children: vec![Inline::Text(prose.clone())],
            })],
        }))
        .build()
        .unwrap()
    };
    // Narrow A6-ish page so long words must hyphenate. Pin engine 1.3
    // (the default is newer); hyphenation behaves identically there.
    let opts = LayoutOptions {
        page_width_um: 90_000,
        ..LayoutOptions::default().with_engine(EngineVersion::V1_3)
    };
    assert_eq!(opts.engine.as_str(), "1.3.0");

    let en = make("en");
    let pages = vsd_layout::layout_document(&en, &opts).unwrap();
    let hyphens: Vec<&DisplayOp> = pages
        .iter()
        .flat_map(|p| &p.ops)
        .filter(|op| matches!(op, DisplayOp::TextRun { text, .. } if text == "-"))
        .collect();
    assert!(
        !hyphens.is_empty(),
        "English body must hyphenate at this width"
    );
    // Every inserted hyphen carries an empty char_range (decoration, so
    // copy/extraction can drop it) and is not RTL.
    for op in &hyphens {
        if let DisplayOp::TextRun {
            char_range, rtl, ..
        } = op
        {
            assert_eq!(char_range.0, char_range.1, "hyphen range must be empty");
            assert!(!rtl);
        }
    }

    // Language gating: German shares the Latin font but not the en-US
    // patterns, so it is laid out without hyphenation (no invented break).
    let de = make("de");
    let de_pages = vsd_layout::layout_document(&de, &opts).unwrap();
    assert!(
        de_pages
            .iter()
            .flat_map(|p| &p.ops)
            .all(|op| !matches!(op, DisplayOp::TextRun { text, .. } if text == "-")),
        "non-English text must not be hyphenated by the en-US patterns"
    );

    // The cache pins 1.3.0 and recomputes byte-identically.
    let laid = vsd_layout::add_render_cache(&en, &opts).unwrap();
    assert_eq!(
        laid.render_cache().unwrap().unwrap().engine_version,
        "1.3.0"
    );
    assert!(matches!(
        vsd_layout::verify_render_cache(&laid).unwrap(),
        RecomputeOutcome::Match { .. }
    ));

    // The frozen contracts are unchanged: 1.2 lays this document out
    // without hyphenation, so its cache differs from 1.3's — and both
    // still recompute-verify under their pinned versions.
    let laid_12 =
        vsd_layout::add_render_cache(&en, &opts.with_engine(EngineVersion::V1_2)).unwrap();
    assert_ne!(
        laid.render_cache().unwrap().unwrap().layout_hash,
        laid_12.render_cache().unwrap().unwrap().layout_hash,
    );
    assert!(matches!(
        vsd_layout::verify_render_cache(&laid_12).unwrap(),
        RecomputeOutcome::Match { .. }
    ));
}

/// Engine 1.3 widow/orphan control: no page break strands a single line
/// of a paragraph. On a doc whose paragraphs straddle short pages, the
/// 1.3 layout differs from 1.2's (the control acted) and satisfies the
/// "≥2 lines on each side of a break" invariant on every paragraph.
#[test]
fn engine_1_3_widow_orphan_keeps_two_lines_together() {
    use std::collections::BTreeMap;
    use vsd_core::layout::DisplayOp;
    use vsd_layout::{EngineVersion, LayoutOptions};

    // Eight paragraphs that each wrap to exactly two lines at the A4
    // measure (≈120 chars > one 170 mm line, < two). Body line height is
    // 5433 µm and the margin 20000 µm (frozen 1.0 constants), so a
    // 60 mm page (limit 40000 µm) holds the first paragraph's two lines
    // plus only the first line of the next — forcing a 1/1 split under
    // greedy pagination. Language is non-English to isolate widow/orphan
    // from hyphenation.
    let line2 = "This paragraph is written to be long enough that it wraps onto a \
                 second line at the default page measure here.";
    assert!(
        line2.len() > 90 && line2.len() < 170,
        "must wrap to two lines"
    );
    let children: Vec<Node> = (0..8)
        .map(|_| {
            Node::Para(Para {
                children: vec![Inline::Text(line2.into())],
            })
        })
        .collect();
    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "fr".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children,
    }))
    .build()
    .unwrap();
    let opts = LayoutOptions {
        page_height_um: 60_000, // limit = 40000 µm: two lines + one stranded
        ..LayoutOptions::default()
    };

    // Line counts per paragraph (node_path) per page, in page order.
    let per_para = |engine| {
        let pages = vsd_layout::layout_document(&doc, &opts.with_engine(engine)).unwrap();
        // node_path -> page_index -> set of distinct baselines (lines).
        let mut m: BTreeMap<Vec<u64>, BTreeMap<usize, std::collections::BTreeSet<i64>>> =
            BTreeMap::new();
        for (pi, page) in pages.iter().enumerate() {
            for op in &page.ops {
                if let DisplayOp::TextRun { y, node_path, .. } = op {
                    m.entry(node_path.clone())
                        .or_default()
                        .entry(pi)
                        .or_default()
                        .insert((y * 1000.0).round() as i64);
                }
            }
        }
        // Collapse to per-paragraph ordered line counts across pages.
        m.into_iter()
            .map(|(path, by_page)| {
                let counts: Vec<usize> = by_page.values().map(|ys| ys.len()).collect();
                (path, counts)
            })
            .collect::<BTreeMap<_, _>>()
    };

    let v13 = per_para(EngineVersion::V1_3);
    // The invariant: any paragraph split across pages keeps ≥2 lines on
    // both the first and the last page it touches.
    for (path, counts) in &v13 {
        if counts.len() >= 2 {
            assert!(
                *counts.first().unwrap() >= 2 && *counts.last().unwrap() >= 2,
                "widow/orphan violated at {path:?}: {counts:?}"
            );
        }
    }
    // The control actually acted: 1.2 leaves at least one stranded line
    // that 1.3 does not.
    let v12 = per_para(EngineVersion::V1_2);
    let v12_strands = v12
        .values()
        .any(|c| c.len() >= 2 && (*c.first().unwrap() == 1 || *c.last().unwrap() == 1));
    assert!(
        v12_strands,
        "expected engine 1.2 to strand a line so 1.3 can be shown to fix it"
    );
}

/// Engine 1.4 (LAYOUT-1.4.md): Arabic and Devanagari are shaped by the
/// pinned shaper into positioned `GlyphRun`s (format 0.4) rather than
/// refused. Logical text is preserved for extraction; the cache pins
/// 1.4.0 and recomputes byte-identically; engine 1.2 still refuses.
#[test]
fn engine_1_4_shapes_arabic_and_devanagari() {
    use vsd_core::layout::DisplayOp;
    use vsd_layout::{EngineVersion, LayoutOptions, RecomputeOutcome};

    let arabic = "العربية";
    let deva = "नमस्ते";
    // One LTR document mixing English with both complex scripts.
    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![Node::Para(Para {
            children: vec![Inline::Text(format!(
                "Arabic {arabic} and Devanagari {deva}."
            ))],
        })],
    }))
    .build()
    .unwrap();

    // Pin engine 1.4: the default has since moved forward, but the 1.4
    // contract (Arabic + Devanagari shaping) is frozen and tested here.
    let opts = LayoutOptions::default().with_engine(EngineVersion::V1_4);
    assert_eq!(opts.engine.as_str(), "1.4.0");
    let pages = vsd_layout::layout_document(&doc, &opts).unwrap();
    let glyph_runs: Vec<(&u64, &String, usize)> = pages
        .iter()
        .flat_map(|p| &p.ops)
        .filter_map(|op| match op {
            DisplayOp::GlyphRun {
                font, glyphs, text, ..
            } => Some((font, text, glyphs.len())),
            _ => None,
        })
        .collect();
    // The Arabic run is shaped in the Arabic face (6), the Devanagari in
    // the Devanagari face (7); both carry real glyphs and logical text.
    let ar = glyph_runs
        .iter()
        .find(|(f, ..)| **f == 6)
        .expect("arabic glyph run");
    assert!(ar.2 > 0 && ar.1.contains(arabic));
    let dv = glyph_runs
        .iter()
        .find(|(f, ..)| **f == 7)
        .expect("devanagari glyph run");
    assert!(dv.2 > 0 && dv.1.contains(deva));
    // The surrounding English is still simple text runs.
    assert!(pages
        .iter()
        .flat_map(|p| &p.ops)
        .any(|op| matches!(op, DisplayOp::TextRun { text, .. } if text.contains("Arabic"))));
    // Every shaped glyph is real (font + shaper agree) with a valid
    // cluster into its run text.
    for op in pages.iter().flat_map(|p| &p.ops) {
        if let DisplayOp::GlyphRun { glyphs, text, .. } = op {
            assert!(glyphs.iter().all(|g| g.gid != 0));
            assert!(glyphs.iter().all(|g| (g.cluster as usize) < text.len()));
        }
    }

    // The cache pins 1.4.0 and recomputes byte-identically (shaping is
    // deterministic), and round-trips through the container/codec.
    let laid = vsd_layout::add_render_cache(&doc, &opts).unwrap();
    let cache = laid.render_cache().unwrap().unwrap();
    assert_eq!(cache.engine_version, "1.4.0");
    assert!(matches!(
        vsd_layout::verify_render_cache(&laid).unwrap(),
        RecomputeOutcome::Match { .. }
    ));
    // The GlyphRun page object survives a CBOR encode/decode round trip.
    let page = vsd_core::layout::Page::from_value(&laid.store.get_value(&cache.pages[0]).unwrap())
        .unwrap();
    assert_eq!(
        page.to_value().encode().unwrap(),
        vsd_core::layout::Page::from_value(&page.to_value())
            .unwrap()
            .to_value()
            .encode()
            .unwrap()
    );

    // Raster and tagged-PDF export both accept the shaped runs.
    assert!(!vsd_render::render_page_png(&laid, &page, 96.0)
        .unwrap()
        .is_empty());
    assert!(
        !vsd_pdf::export_pdf(&laid, None, &vsd_pdf::ExportOptions::default())
            .unwrap()
            .is_empty()
    );

    // Frozen contract: engine 1.2 still refuses these scripts.
    assert!(matches!(
        vsd_layout::layout_document(&doc, &opts.with_engine(EngineVersion::V1_2)),
        Err(vsd_layout::LayoutError::Unsupported(_))
    ));
}

/// A right-to-left Arabic document right-aligns its shaped line and the
/// glyphs come back in visual order (first glyph maps to a later source
/// cluster than the last — the hallmark of RTL visual reordering).
#[test]
fn engine_1_4_arabic_rtl_is_visually_ordered() {
    use vsd_core::layout::DisplayOp;
    use vsd_layout::font::{Face, FontMetrics};
    use vsd_layout::{EngineVersion, LayoutOptions};

    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "ar".into(),
        dir: Direction::Rtl,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![Node::Para(Para {
            children: vec![Inline::Text("العربية لغة جميلة".into())],
        })],
    }))
    .build()
    .unwrap();
    let pages = vsd_layout::layout_document(
        &doc,
        &LayoutOptions::default().with_engine(EngineVersion::V1_4),
    )
    .unwrap();
    let mut right_edge = f64::NEG_INFINITY;
    let mut saw_visual_reorder = false;
    for op in pages.iter().flat_map(|p| &p.ops) {
        if let DisplayOp::GlyphRun {
            x, font, glyphs, ..
        } = op
        {
            assert_eq!(Face::from_index(*font), Face::Arabic);
            let w: f64 = glyphs.iter().map(|g| g.x_advance).sum();
            right_edge = right_edge.max(x + w);
            // In an RTL run the leftmost (first) glyph is a logically
            // later character than the rightmost (last) glyph.
            if let (Some(first), Some(last)) = (glyphs.first(), glyphs.last()) {
                if first.cluster > last.cluster {
                    saw_visual_reorder = true;
                }
            }
        }
    }
    assert!(saw_visual_reorder, "RTL run must be in visual order");
    // A4 content right edge = 210 − 20 mm margin.
    assert!(
        (right_edge - 190.0).abs() < 0.5,
        "rtl shaped line must sit at the right margin, got {right_edge}"
    );
    // Sanity: the Arabic face actually has the metrics we scaled with.
    assert!(FontMetrics::face_metrics(Face::Arabic).upem > 0);
}

/// Engine 1.5 shapes the remaining major Brahmic scripts via the same
/// pinned shaper as Devanagari, emitting them as positioned GlyphRuns in
/// their own face; engine 1.4 still refuses them (frozen contract).
#[test]
fn engine_1_5_shapes_remaining_brahmic_scripts() {
    use vsd_core::layout::DisplayOp;
    use vsd_layout::{EngineVersion, LayoutOptions, RecomputeOutcome};

    let tamil = "தமிழ்";
    let bengali = "বাংলা";
    let telugu = "తెలుగు";
    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![Node::Para(Para {
            children: vec![Inline::Text(format!(
                "Tamil {tamil}, Bengali {bengali}, Telugu {telugu}."
            ))],
        })],
    }))
    .build()
    .unwrap();

    let opts = LayoutOptions::default().with_engine(EngineVersion::V1_5);
    assert_eq!(opts.engine.as_str(), "1.5.0");
    let pages = vsd_layout::layout_document(&doc, &opts).unwrap();
    let glyph_runs: Vec<(u64, &String)> = pages
        .iter()
        .flat_map(|p| &p.ops)
        .filter_map(|op| match op {
            DisplayOp::GlyphRun { font, text, .. } => Some((*font, text)),
            _ => None,
        })
        .collect();
    // Tamil → face 12, Bengali → 8, Telugu → 13; each carries its word.
    for (face_idx, word) in [(12u64, tamil), (8, bengali), (13, telugu)] {
        let run = glyph_runs
            .iter()
            .find(|(f, t)| *f == face_idx && t.contains(word))
            .unwrap_or_else(|| panic!("missing glyph run for face {face_idx} / {word}"));
        assert!(run.1.contains(word), "logical text preserved");
    }
    // Every shaped glyph is real, with a valid cluster into its run text.
    for op in pages.iter().flat_map(|p| &p.ops) {
        if let DisplayOp::GlyphRun { glyphs, text, .. } = op {
            assert!(glyphs.iter().all(|g| g.gid != 0), "no .notdef");
            assert!(glyphs.iter().all(|g| (g.cluster as usize) < text.len()));
        }
    }
    // The cache pins 1.5.0 and recomputes byte-identically.
    let laid = vsd_layout::add_render_cache(&doc, &opts).unwrap();
    let cache = laid.render_cache().unwrap().unwrap();
    assert_eq!(cache.engine_version, "1.5.0");
    assert!(matches!(
        vsd_layout::verify_render_cache(&laid).unwrap(),
        RecomputeOutcome::Match { .. }
    ));
    // Raster + tagged-PDF export accept the new faces.
    let page = vsd_core::layout::Page::from_value(&laid.store.get_value(&cache.pages[0]).unwrap())
        .unwrap();
    assert!(!vsd_render::render_page_png(&laid, &page, 96.0)
        .unwrap()
        .is_empty());
    assert!(
        !vsd_pdf::export_pdf(&laid, None, &vsd_pdf::ExportOptions::default())
            .unwrap()
            .is_empty()
    );

    // Frozen contract: engine 1.4 refuses these scripts (it shaped only
    // Arabic + Devanagari).
    assert!(matches!(
        vsd_layout::layout_document(&doc, &opts.with_engine(EngineVersion::V1_4)),
        Err(vsd_layout::LayoutError::Unsupported(_))
    ));
}

/// Engine 1.5 mirrors `Bidi_Mirrored` characters in RTL runs (UAX #9
/// HL6): a `(` opening a clause in Hebrew is drawn with the `)` glyph,
/// while the logical text keeps the `(`. The Hebrew letters themselves
/// stay on the proven format-0.3 TextRun path; only the mirrored
/// bracket segment becomes a positioned GlyphRun. Engine 1.2 does not
/// mirror (frozen): it emits no GlyphRun at all.
#[test]
fn engine_1_5_mirrors_brackets_in_rtl() {
    use vsd_core::layout::DisplayOp;
    use vsd_layout::font::{Face, FontMetrics};
    use vsd_layout::{EngineVersion, LayoutOptions};

    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "he".into(),
        dir: Direction::Rtl,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![Node::Para(Para {
            children: vec![Inline::Text("שלום (עולם) ושלום".into())],
        })],
    }))
    .build()
    .unwrap();

    let opts = LayoutOptions::default().with_engine(EngineVersion::V1_5);
    let pages = vsd_layout::layout_document(&doc, &opts).unwrap();
    let reg = FontMetrics::face_metrics(Face::Regular);
    let open_gid = reg.glyph('(').0;
    let close_gid = reg.glyph(')').0;
    assert_ne!(open_gid, close_gid);

    let mut saw_mirrored_open = false;
    let mut saw_mirrored_close = false;
    let mut saw_hebrew_textrun = false;
    for op in pages.iter().flat_map(|p| &p.ops) {
        match op {
            // Bracket segments are Regular-face positioned glyph runs.
            DisplayOp::GlyphRun {
                font, glyphs, text, ..
            } if *font == 0 => {
                for g in glyphs {
                    let logical = text[g.cluster as usize..].chars().next().unwrap();
                    if logical == '(' {
                        // Logical '(' must be drawn with the ')' glyph.
                        assert_eq!(g.gid, close_gid, "'(' must mirror to ')' glyph");
                        saw_mirrored_open = true;
                    }
                    if logical == ')' {
                        assert_eq!(g.gid, open_gid, "')' must mirror to '(' glyph");
                        saw_mirrored_close = true;
                    }
                }
                // The logical text is preserved (un-mirrored).
                assert!(text.contains('(') || text.contains(')') || text.contains(' '));
            }
            // Hebrew stays on the format-0.3 rtl TextRun path (face 5).
            DisplayOp::TextRun { font, rtl, .. } if *font == 5 && *rtl => {
                saw_hebrew_textrun = true;
            }
            _ => {}
        }
    }
    assert!(saw_mirrored_open, "the opening paren must be mirrored");
    assert!(saw_mirrored_close, "the closing paren must be mirrored");
    assert!(
        saw_hebrew_textrun,
        "Hebrew letters must remain plain rtl text runs (only brackets convert)"
    );

    // Frozen contract: engine 1.2 does not mirror — it emits no GlyphRun.
    let pages_12 =
        vsd_layout::layout_document(&doc, &opts.with_engine(EngineVersion::V1_2)).unwrap();
    assert!(
        pages_12
            .iter()
            .flat_map(|p| &p.ops)
            .all(|op| !matches!(op, DisplayOp::GlyphRun { .. })),
        "engine 1.2 must not produce glyph runs (no mirroring)"
    );
}

/// Engine 1.6 shapes Thai/Lao and breaks their spaceless text into lines
/// at dictionary-word boundaries. On a narrow page a Thai paragraph must
/// wrap to multiple lines, each within the content width, with the
/// logical text preserved across the wrap; engine 1.5 still refuses Thai.
#[test]
fn engine_1_6_shapes_and_breaks_thai() {
    use vsd_core::layout::DisplayOp;
    use vsd_layout::{EngineVersion, LayoutOptions, RecomputeOutcome};

    // A long, spaceless Thai paragraph (no inter-word spaces at all).
    let thai = "ภาษาไทยเป็นภาษาที่สวยงามและมีเอกลักษณ์เฉพาะตัวซึ่งเขียนติดกันโดยไม่มีการเว้นวรรค";
    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "th".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![Node::Para(Para {
            children: vec![Inline::Text(thai.into())],
        })],
    }))
    .build()
    .unwrap();

    // Narrow page so the paragraph must wrap.
    let opts = LayoutOptions {
        page_width_um: 60_000,
        page_height_um: 200_000,
        engine: EngineVersion::V1_6,
    };
    let content_w_mm = 60.0 - 2.0 * 20.0; // page − margins
    let pages = vsd_layout::layout_document(&doc, &opts).unwrap();

    // Every Thai run is a GlyphRun in the Thai face (17); collect them in
    // page order and check width + logical-text preservation.
    let mut thai_runs = 0usize;
    let mut joined = String::new();
    for op in pages.iter().flat_map(|p| &p.ops) {
        if let DisplayOp::GlyphRun {
            font, glyphs, text, ..
        } = op
        {
            assert_eq!(*font, 17, "Thai must use face 17");
            assert!(glyphs.iter().all(|g| g.gid != 0), "no .notdef");
            let w: f64 = glyphs.iter().map(|g| g.x_advance).sum();
            assert!(
                w <= content_w_mm + 0.5,
                "line wider than content box: {w} mm"
            );
            joined.push_str(text);
            thai_runs += 1;
        }
    }
    // Dictionary breaking actually wrapped the paragraph.
    assert!(thai_runs >= 2, "Thai paragraph must wrap to >=2 lines");
    // Logical text is preserved across the wrap (no characters lost or
    // reordered; Thai is LTR so visual order == logical within a line).
    assert_eq!(joined, thai, "wrapped Thai must reconstruct the source");

    // Cache pins 1.6.0 and recomputes byte-identically.
    let laid = vsd_layout::add_render_cache(&doc, &opts).unwrap();
    let cache = laid.render_cache().unwrap().unwrap();
    assert_eq!(cache.engine_version, "1.6.0");
    assert!(matches!(
        vsd_layout::verify_render_cache(&laid).unwrap(),
        RecomputeOutcome::Match { .. }
    ));

    // Frozen contract: engine 1.5 refuses Thai (it had no dictionary).
    assert!(matches!(
        vsd_layout::layout_document(&doc, &opts.with_engine(EngineVersion::V1_5)),
        Err(vsd_layout::LayoutError::Unsupported(_))
    ));
}

/// Engine 1.7 lays out CJK (Han/kana/Hangul) in the pinned pan-CJK face,
/// per glyph, with inter-ideograph line breaking. On a narrow page a
/// spaceless Chinese paragraph must wrap, each line within the content
/// box, logical text preserved; raster and (CFF) PDF export both accept
/// it; engine 1.6 still refuses CJK.
#[test]
fn engine_1_7_lays_out_and_breaks_cjk() {
    use vsd_core::layout::DisplayOp;
    use vsd_layout::{EngineVersion, LayoutOptions, RecomputeOutcome};

    // Spaceless Chinese, plus Japanese and Korean to exercise the whole
    // pan-CJK face.
    let zh = "这是一个用于测试中日韩文字排版的段落它没有空格但可以在表意文字之间换行";
    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "zh".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![
            Node::Para(Para {
                children: vec![Inline::Text(zh.into())],
            }),
            Node::Para(Para {
                children: vec![Inline::Text("日本語と한국어".into())],
            }),
        ],
    }))
    .build()
    .unwrap();

    let opts = LayoutOptions {
        page_width_um: 60_000,
        page_height_um: 200_000,
        engine: EngineVersion::V1_7,
    };
    let content_w_mm = 60.0 - 2.0 * 20.0;
    let pages = vsd_layout::layout_document(&doc, &opts).unwrap();

    // CJK is emitted as plain text runs in the CJK face (19), not shaped.
    let mut cjk_runs = 0usize;
    let mut joined = String::new();
    let mut zh_lines = 0usize;
    for op in pages.iter().flat_map(|p| &p.ops) {
        match op {
            DisplayOp::TextRun {
                font,
                text,
                size_pt,
                ..
            } if *font == 19 => {
                let m = vsd_layout::font::FontMetrics::face_metrics(vsd_layout::font::Face::Cjk);
                let size_um = (size_pt * 25400.0 / 72.0) as i64;
                let w_mm: f64 = text
                    .chars()
                    .map(|c| m.char_advance_um(c, size_um) as f64)
                    .sum::<f64>()
                    / 1000.0;
                assert!(w_mm <= content_w_mm + 0.5, "CJK line too wide: {w_mm} mm");
                assert!(text.chars().all(|c| m.glyph(c).0 != 0), "no .notdef");
                joined.push_str(text);
                cjk_runs += 1;
                if zh.contains(text.as_str()) {
                    zh_lines += 1;
                }
            }
            // No GlyphRun: CJK is not shaped.
            DisplayOp::GlyphRun { .. } => panic!("CJK must not be shaped"),
            _ => {}
        }
    }
    assert!(cjk_runs >= 2, "CJK must produce multiple runs");
    assert!(
        zh_lines >= 2,
        "the Chinese paragraph must wrap to >=2 lines"
    );
    assert!(joined.contains('这') && joined.contains('日') && joined.contains('한'));

    // Cache pins 1.7.0 and recomputes byte-identically.
    let laid = vsd_layout::add_render_cache(&doc, &opts).unwrap();
    let cache = laid.render_cache().unwrap().unwrap();
    assert_eq!(cache.engine_version, "1.7.0");
    assert!(matches!(
        vsd_layout::verify_render_cache(&laid).unwrap(),
        RecomputeOutcome::Match { .. }
    ));

    // Raster + tagged-PDF export accept the CJK (CFF) face. The PDF must
    // embed the CFF as a CIDFontType0 / FontFile3.
    let page = vsd_core::layout::Page::from_value(&laid.store.get_value(&cache.pages[0]).unwrap())
        .unwrap();
    assert!(!vsd_render::render_page_png(&laid, &page, 96.0)
        .unwrap()
        .is_empty());
    let pdf = vsd_pdf::export_pdf(&laid, None, &vsd_pdf::ExportOptions::default()).unwrap();
    assert!(!pdf.is_empty());
    let pdf_str = String::from_utf8_lossy(&pdf);
    assert!(
        pdf_str.contains("/CIDFontType0") && pdf_str.contains("/FontFile3"),
        "CJK PDF must embed the CFF via CIDFontType0 / FontFile3"
    );

    // Frozen contract: engine 1.6 refuses CJK (it had no CJK font).
    assert!(matches!(
        vsd_layout::layout_document(&doc, &opts.with_engine(EngineVersion::V1_6)),
        Err(vsd_layout::LayoutError::Unsupported(_))
    ));
}

/// Engine 1.8 lays out a `vertical-rl` document: characters stack
/// top-to-bottom in a column and columns advance right-to-left. Each
/// character is its own positioned run; op order is reading order, so the
/// logical text is preserved. Engine 1.7 refuses vertical-rl.
#[test]
fn engine_1_8_vertical_writing_mode() {
    use vsd_core::layout::DisplayOp;
    use vsd_core::tree::WritingMode;
    use vsd_layout::{EngineVersion, LayoutOptions, RecomputeOutcome};

    let ja = "これは縦書きの文章です日本語の組版を確認します";
    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "ja".into(),
        dir: Direction::Ltr,
        writing_mode: WritingMode::VerticalRl,
        children: vec![Node::Para(Para {
            children: vec![Inline::Text(ja.into())],
        })],
    }))
    .build()
    .unwrap();

    // Small page so the paragraph fills several columns.
    let opts = LayoutOptions {
        page_width_um: 80_000,
        page_height_um: 120_000,
        engine: EngineVersion::V1_8,
    };
    let pages = vsd_layout::layout_document(&doc, &opts).unwrap();
    assert_eq!(pages.len(), 1);

    // Collect every positioned character run in op (reading) order.
    let runs: Vec<(f64, f64, String)> = pages[0]
        .ops
        .iter()
        .filter_map(|op| match op {
            DisplayOp::TextRun { x, y, text, .. } => Some((*x, *y, text.clone())),
            _ => None,
        })
        .collect();
    assert!(runs.len() >= ja.chars().count(), "one run per character");

    // Logical text is preserved in op order (top-to-bottom, R-to-L).
    let joined: String = runs.iter().map(|(_, _, t)| t.as_str()).collect();
    assert_eq!(joined, ja, "vertical op order must equal reading order");

    // Columns advance right-to-left: the run x positions are
    // non-increasing across the document, and there is more than one
    // distinct column (so wrapping actually happened).
    let xs: Vec<f64> = runs.iter().map(|(x, _, _)| *x).collect();
    assert!(
        xs.windows(2).all(|w| w[1] <= w[0] + 0.001),
        "columns must advance right-to-left (x non-increasing)"
    );
    let distinct_cols = {
        let mut v: Vec<i64> = xs.iter().map(|x| (x * 1000.0) as i64).collect();
        v.dedup();
        v.len()
    };
    assert!(distinct_cols >= 2, "text must wrap to >=2 columns");

    // Within the first column, y increases (top-to-bottom).
    let first_col_x = xs[0];
    let first_col_ys: Vec<f64> = runs
        .iter()
        .filter(|(x, _, _)| (*x - first_col_x).abs() < 0.001)
        .map(|(_, y, _)| *y)
        .collect();
    assert!(
        first_col_ys.windows(2).all(|w| w[1] > w[0]),
        "characters stack top-to-bottom within a column"
    );

    // Cache pins 1.8.0 and recomputes byte-identically; raster + PDF ok.
    let laid = vsd_layout::add_render_cache(&doc, &opts).unwrap();
    let cache = laid.render_cache().unwrap().unwrap();
    assert_eq!(cache.engine_version, "1.8.0");
    assert!(matches!(
        vsd_layout::verify_render_cache(&laid).unwrap(),
        RecomputeOutcome::Match { .. }
    ));
    let page = vsd_core::layout::Page::from_value(&laid.store.get_value(&cache.pages[0]).unwrap())
        .unwrap();
    assert!(!vsd_render::render_page_png(&laid, &page, 96.0)
        .unwrap()
        .is_empty());
    assert!(
        !vsd_pdf::export_pdf(&laid, None, &vsd_pdf::ExportOptions::default())
            .unwrap()
            .is_empty()
    );

    // Frozen contract: engine 1.7 refuses vertical-rl (no vertical mode).
    assert!(matches!(
        vsd_layout::layout_document(&doc, &opts.with_engine(EngineVersion::V1_7)),
        Err(vsd_layout::LayoutError::Unsupported(_))
    ));
}

/// Engine 1.9 lays out the remaining complex scripts (Tibetan with tsheg
/// breaking shown here) and routes CJK punctuation into the pan-CJK face.
/// Engine 1.8 leaves both on the Regular path (the version-gated routing
/// keeps frozen engines byte-identical), proven here.
#[test]
fn engine_1_9_extended_scripts_and_cjk_punctuation() {
    use vsd_core::layout::DisplayOp;
    use vsd_layout::{EngineVersion, LayoutOptions, RecomputeOutcome};

    let tibetan = "བོད་སྐད་ནི་བོད་ཀྱི་སྐད་ཡིག་ཡིན།";
    let cjkp = "日本語「引用」。";
    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "mul".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![
            Node::Para(Para {
                children: vec![Inline::Text(tibetan.into())],
            }),
            Node::Para(Para {
                children: vec![Inline::Text(cjkp.into())],
            }),
        ],
    }))
    .build()
    .unwrap();

    let opts = LayoutOptions {
        page_width_um: 70_000,
        page_height_um: 200_000,
        engine: EngineVersion::V1_9,
    };
    let pages = vsd_layout::layout_document(&doc, &opts).unwrap();

    let mut tibetan_runs = 0usize;
    let mut saw_cjk_punct = false;
    let mut joined = String::new();
    for op in pages.iter().flat_map(|p| &p.ops) {
        match op {
            DisplayOp::GlyphRun {
                font, glyphs, text, ..
            } => {
                if *font == 20 {
                    assert!(glyphs.iter().all(|g| g.gid != 0), "Tibetan .notdef");
                    tibetan_runs += 1;
                }
                joined.push_str(text);
            }
            DisplayOp::TextRun { font, text, .. } => {
                if *font == 19 && (text.contains('。') || text.contains('\u{300C}')) {
                    saw_cjk_punct = true;
                }
                joined.push_str(text);
            }
            _ => {}
        }
    }
    assert!(tibetan_runs >= 2, "Tibetan must wrap at tsheg to >=2 runs");
    assert!(
        saw_cjk_punct,
        "CJK punctuation must be set in the pan-CJK face under 1.9"
    );
    assert_eq!(joined, format!("{tibetan}{cjkp}"), "logical text preserved");

    // Cache pins 1.9.0 and recomputes byte-identically; raster + PDF ok.
    let laid = vsd_layout::add_render_cache(&doc, &opts).unwrap();
    let cache = laid.render_cache().unwrap().unwrap();
    assert_eq!(cache.engine_version, "1.9.0");
    assert!(matches!(
        vsd_layout::verify_render_cache(&laid).unwrap(),
        RecomputeOutcome::Match { .. }
    ));
    let page = vsd_core::layout::Page::from_value(&laid.store.get_value(&cache.pages[0]).unwrap())
        .unwrap();
    assert!(!vsd_render::render_page_png(&laid, &page, 96.0)
        .unwrap()
        .is_empty());
    assert!(
        !vsd_pdf::export_pdf(&laid, None, &vsd_pdf::ExportOptions::default())
            .unwrap()
            .is_empty()
    );

    // Frozen contract: engine 1.8 routes neither — Tibetan stays on the
    // Regular path (no face-20 run) and CJK punctuation stays Regular
    // (no face-19 run carrying it). So a frozen engine is byte-identical.
    let pages_18 =
        vsd_layout::layout_document(&doc, &opts.with_engine(EngineVersion::V1_8)).unwrap();
    let any_tibetan_18 = pages_18
        .iter()
        .flat_map(|p| &p.ops)
        .any(|op| matches!(op, DisplayOp::GlyphRun { font, .. } if *font == 20));
    let any_cjk_punct_18 = pages_18.iter().flat_map(|p| &p.ops).any(|op| {
        matches!(op, DisplayOp::TextRun { font, text, .. }
            if *font == 19 && (text.contains('。') || text.contains('\u{300C}')))
    });
    assert!(
        !any_tibetan_18,
        "engine 1.8 must not route Tibetan (frozen)"
    );
    assert!(
        !any_cjk_punct_18,
        "engine 1.8 must not route CJK punctuation (frozen)"
    );
}

/// Engine 1.10 flows a section carrying the format-0.6 `cols` attribute
/// into multiple columns: content fills column 0 top-to-bottom, then
/// column 1, then a new page. Earlier engines refuse a multi-column
/// section rather than collapsing it to a single column, and a document
/// with no multi-column section is byte-identical between 1.9 and 1.10.
#[test]
fn engine_1_10_multi_column_layout() {
    use vsd_core::layout::DisplayOp;
    use vsd_layout::{EngineVersion, LayoutOptions, RecomputeOutcome};

    // Many short paragraphs so column 0 fills and overflows into column 1
    // on a deliberately short page.
    let paras: Vec<Node> = (0..24)
        .map(|i| {
            Node::Para(Para {
                children: vec![Inline::Text(format!(
                    "Paragraph number {i} in the column flow."
                ))],
            })
        })
        .collect();
    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![Node::Section(Section {
            role: "body".into(),
            columns: 2,
            children: paras,
        })],
    }))
    .build()
    .unwrap();

    let opts = LayoutOptions {
        page_width_um: 210_000,
        page_height_um: 90_000, // short page → forces column + page breaks
        engine: EngineVersion::V1_10,
    };
    let pages = vsd_layout::layout_document(&doc, &opts).unwrap();

    // Two columns on an A4-wide page: col 0 left edge ≈ 20mm, col 1 left
    // edge ≈ 20 + 82.5 + 5 = 107.5mm. Both bands must carry text.
    let xs: Vec<f64> = pages
        .iter()
        .flat_map(|p| &p.ops)
        .filter_map(|op| match op {
            DisplayOp::TextRun { x, .. } => Some(*x),
            _ => None,
        })
        .collect();
    let in_col0 = xs.iter().any(|&x| (19.0..21.0).contains(&x));
    let in_col1 = xs.iter().any(|&x| (106.5..108.5).contains(&x));
    assert!(in_col0, "left column must carry text (x≈20mm)");
    assert!(in_col1, "right column must carry text (x≈107.5mm)");

    // Logical text is preserved in reading order across the columns.
    let joined: String = pages
        .iter()
        .flat_map(|p| &p.ops)
        .filter_map(|op| match op {
            DisplayOp::TextRun { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(joined.contains("Paragraph number 0 "));
    assert!(joined.contains("Paragraph number 23 "));

    // The cache pins 1.10.0 and recomputes byte-identically; raster + PDF
    // accept the multi-column pages.
    let laid = vsd_layout::add_render_cache(&doc, &opts).unwrap();
    let cache = laid.render_cache().unwrap().unwrap();
    assert_eq!(cache.engine_version, "1.10.0");
    assert!(matches!(
        vsd_layout::verify_render_cache(&laid).unwrap(),
        RecomputeOutcome::Match { .. }
    ));
    let page = vsd_core::layout::Page::from_value(&laid.store.get_value(&cache.pages[0]).unwrap())
        .unwrap();
    assert!(!vsd_render::render_page_png(&laid, &page, 96.0)
        .unwrap()
        .is_empty());
    assert!(
        !vsd_pdf::export_pdf(&laid, None, &vsd_pdf::ExportOptions::default())
            .unwrap()
            .is_empty()
    );

    // Frozen contract: engine 1.9 refuses a multi-column section rather
    // than mis-rendering it as one column.
    assert!(matches!(
        vsd_layout::layout_document(&doc, &opts.with_engine(EngineVersion::V1_9)),
        Err(vsd_layout::LayoutError::Unsupported(_))
    ));

    // A document with no multi-column section is byte-identical between
    // 1.9 and 1.10 (multi-column changes flow only when `cols > 1`).
    let plain = DocumentBuilder::new(Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![
            Node::Heading(Heading {
                level: 1,
                children: vec![Inline::Text("Plain".into())],
            }),
            Node::Para(Para {
                children: vec![Inline::Text(
                    "A single-column paragraph of body text.".into(),
                )],
            }),
        ],
    }))
    .build()
    .unwrap();
    let p_19 = vsd_layout::layout_document(&plain, &opts.with_engine(EngineVersion::V1_9)).unwrap();
    let p_10 =
        vsd_layout::layout_document(&plain, &opts.with_engine(EngineVersion::V1_10)).unwrap();
    assert_eq!(p_19, p_10, "single-column flow unchanged by engine 1.10");
}

/// Engine 1.11 lays out a `math` node's MathML with the pinned STIX Two
/// Math face (index 24): the quadratic formula yields positioned glyph
/// runs in that face plus rules (the fraction bar and the radical
/// overbar). Earlier engines do not — they render the fallback image or
/// the MathML source as a code block — and a document with no math node
/// is byte-identical between 1.10 and 1.11.
#[test]
fn engine_1_11_mathml_layout() {
    use vsd_core::layout::DisplayOp;
    use vsd_core::tree::Math;
    use vsd_layout::{EngineVersion, LayoutOptions, RecomputeOutcome};

    // x = (-b ± √(b²−4ac)) / 2a
    let mathml = "<math><mi>x</mi><mo>=</mo><mfrac>\
        <mrow><mo>-</mo><mi>b</mi><mo>±</mo><msqrt>\
        <mrow><msup><mi>b</mi><mn>2</mn></msup><mo>-</mo>\
        <mn>4</mn><mi>a</mi><mi>c</mi></mrow></msqrt></mrow>\
        <mrow><mn>2</mn><mi>a</mi></mrow></mfrac></math>";
    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![
            Node::Heading(Heading {
                level: 1,
                children: vec![Inline::Text("The quadratic formula".into())],
            }),
            Node::Math(Math {
                mathml: mathml.into(),
                fallback: None,
            }),
        ],
    }))
    .build()
    .unwrap();

    let opts = LayoutOptions::default().with_engine(EngineVersion::V1_11);
    assert_eq!(opts.engine.as_str(), "1.11.0");
    let pages = vsd_layout::layout_document(&doc, &opts).unwrap();

    let mut math_glyphs = 0usize;
    let mut rules = 0usize;
    for op in pages.iter().flat_map(|p| &p.ops) {
        match op {
            DisplayOp::GlyphRun { font, glyphs, .. } if *font == 24 => {
                assert!(glyphs.iter().all(|g| g.gid != 0), "no .notdef in math");
                math_glyphs += glyphs.len();
            }
            DisplayOp::Rect { .. } => rules += 1,
            _ => {}
        }
    }
    assert!(
        math_glyphs >= 10,
        "the formula has many glyphs in the math face"
    );
    assert!(rules >= 2, "fraction bar + radical overbar are rules");

    // Cache pins 1.11.0 and recomputes byte-identically; raster + PDF
    // accept the math face (a CFF font → FontFile3 path, subset-verified).
    let laid = vsd_layout::add_render_cache(&doc, &opts).unwrap();
    let cache = laid.render_cache().unwrap().unwrap();
    assert_eq!(cache.engine_version, "1.11.0");
    assert!(matches!(
        vsd_layout::verify_render_cache(&laid).unwrap(),
        RecomputeOutcome::Match { .. }
    ));
    let page = vsd_core::layout::Page::from_value(&laid.store.get_value(&cache.pages[0]).unwrap())
        .unwrap();
    assert!(!vsd_render::render_page_png(&laid, &page, 96.0)
        .unwrap()
        .is_empty());
    assert!(
        !vsd_pdf::export_pdf(&laid, None, &vsd_pdf::ExportOptions::default())
            .unwrap()
            .is_empty()
    );

    // Frozen contract: engine 1.10 does not lay out MathML — it has no
    // math face (index 24) glyph run; the formula renders as a code block.
    let pages_10 =
        vsd_layout::layout_document(&doc, &opts.with_engine(EngineVersion::V1_10)).unwrap();
    let any_math_10 = pages_10
        .iter()
        .flat_map(|p| &p.ops)
        .any(|op| matches!(op, DisplayOp::GlyphRun { font, .. } if *font == 24));
    assert!(!any_math_10, "engine 1.10 must not lay out MathML (frozen)");

    // A document with no math node is byte-identical between 1.10 and 1.11.
    let plain = DocumentBuilder::new(Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![Node::Para(Para {
            children: vec![Inline::Text("Plain body text, no mathematics here.".into())],
        })],
    }))
    .build()
    .unwrap();
    let q_10 =
        vsd_layout::layout_document(&plain, &opts.with_engine(EngineVersion::V1_10)).unwrap();
    let q_11 =
        vsd_layout::layout_document(&plain, &opts.with_engine(EngineVersion::V1_11)).unwrap();
    assert_eq!(q_10, q_11, "non-math flow unchanged by engine 1.11");

    // Unsupported MathML with no fallback is refused, not mis-rendered.
    let exotic = DocumentBuilder::new(Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![Node::Math(Math {
            mathml: "<math><mtable><mtr><mtd><mn>1</mn></mtd></mtr></mtable></math>".into(),
            fallback: None,
        })],
    }))
    .build()
    .unwrap();
    assert!(matches!(
        vsd_layout::layout_document(&exotic, &opts),
        Err(vsd_layout::LayoutError::Unsupported(_))
    ));
}
