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
        children: vec![],
    }));
    let blob_id = builder.add_object(blob.to_value()).unwrap();

    let root = Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        children: vec![
            Node::Heading(Heading {
                level: 1,
                children: vec![Inline::Text("Service Agreement".into())],
            }),
            Node::Section(Section {
                role: "terms".into(),
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
        children: vec![],
    }));
    let blob_id = builder.add_object(blob.to_value()).unwrap();

    let root = Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
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
        lang: "ar".into(),
        dir: Direction::Rtl,
        children: vec![],
    }))
    .build()
    .unwrap();
    assert!(matches!(
        vsd_layout::layout_document(&doc, &vsd_layout::LayoutOptions::default()),
        Err(vsd_layout::LayoutError::Unsupported(_))
    ));
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
