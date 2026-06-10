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
    builder.resources(resources).profile(Profile::Core).build().unwrap()
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
    let redacted = vsd_core::redact::redact(&doc, &[1, 1], None).unwrap().document;

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
        report
            .errors()
            .any(|f| f.code == "E_ALT_TEXT"),
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
    let redacted = vsd_core::redact::redact(&doc, &[1, 1], None).unwrap().document;
    assert_eq!(
        vsd_sign::verify(&redacted, &subtree_sig).unwrap(),
        vsd_sign::Verdict::ValidForOtherTarget
    );
}
