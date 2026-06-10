//! Property-based tests over whole documents: container roundtrip and
//! the redaction-never-leaks guarantee, on *generated* documents rather
//! than hand-picked ones.

use proptest::collection::vec as pvec;
use proptest::prelude::*;

use vsd_container::{read_document, write_document, ReadOptions, WriteOptions};
use vsd_core::document::DocumentBuilder;
use vsd_core::tree::{Direction, Doc, Heading, Inline, List, Node, Para, Section};
use vsd_core::Document;

const MARKER: &str = "\u{1F512}REDACT-ME-7f3a9\u{1F512}";

/// A paragraph of benign text (never contains the marker).
fn arb_para() -> impl Strategy<Value = Node> {
    "[a-zA-Z0-9 ,.]{1,60}".prop_map(|text| {
        Node::Para(Para {
            children: vec![Inline::Text(text)],
        })
    })
}

/// A block: paragraph, heading, list, or section of paragraphs.
fn arb_block() -> impl Strategy<Value = Node> {
    prop_oneof![
        arb_para(),
        ("[a-zA-Z ]{1,30}", 1u8..=6).prop_map(|(t, l)| Node::Heading(Heading {
            level: l,
            children: vec![Inline::Text(t)],
        })),
        (any::<bool>(), pvec(arb_para(), 1..4)).prop_map(|(ordered, items)| Node::List(List {
            ordered,
            items: items.into_iter().map(|p| vec![p]).collect(),
        })),
        ("[a-z]{1,12}", pvec(arb_para(), 1..4))
            .prop_map(|(role, children)| { Node::Section(Section { role, children }) }),
    ]
}

fn build_doc(mut blocks: Vec<Node>, secret_at: usize) -> (Document, Vec<usize>) {
    let idx = secret_at % (blocks.len() + 1);
    blocks.insert(
        idx,
        Node::Para(Para {
            children: vec![Inline::Text(format!("classified: {MARKER} end"))],
        }),
    );
    let doc = DocumentBuilder::new(Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        children: blocks,
    }))
    .build()
    .unwrap();
    (doc, vec![idx])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn container_roundtrip_any_document(
        blocks in pvec(arb_block(), 1..8),
        compress in any::<bool>(),
    ) {
        let doc = DocumentBuilder::new(Node::Doc(Doc {
            lang: "en".into(),
            dir: Direction::Ltr,
            children: blocks,
        }))
        .build()
        .unwrap();
        let id = doc.document_id().unwrap();
        let bytes = write_document(&doc, &[], &WriteOptions { compress }).unwrap();
        let back = read_document(&bytes, &ReadOptions::default()).unwrap();
        prop_assert_eq!(back.document_id, id);
        prop_assert!(vsd_core::validate::validate(&back.document).is_valid());
        // Determinism: same inputs, same bytes.
        prop_assert_eq!(write_document(&doc, &[], &WriteOptions { compress }).unwrap(), bytes);
    }

    #[test]
    fn redaction_never_leaks(
        blocks in pvec(arb_block(), 0..7),
        secret_at in any::<usize>(),
        compress in any::<bool>(),
    ) {
        let (doc, path) = build_doc(blocks, secret_at);

        // Sanity: the secret is present before redaction.
        let before = write_document(&doc, &[], &WriteOptions { compress: false }).unwrap();
        let needle = MARKER.as_bytes();
        prop_assert!(before.windows(needle.len()).any(|w| w == needle));

        let redacted = vsd_core::redact::redact(&doc, &path, None).unwrap().document;

        // The marker is gone from every byte, compressed or not...
        let after = write_document(&redacted, &[], &WriteOptions { compress: false }).unwrap();
        prop_assert!(
            !after.windows(needle.len()).any(|w| w == needle),
            "redacted secret survived in container bytes"
        );
        // ...and from every object in the store.
        for (_, bytes) in redacted.store.iter() {
            prop_assert!(!bytes.windows(needle.len()).any(|w| w == needle));
        }
        // The result is still a valid document with no orphans.
        let report = vsd_core::validate::validate(&redacted);
        prop_assert!(report.is_valid());
        prop_assert_eq!(report.warnings().count(), 0);

        // And it still roundtrips through the chosen container options.
        let bytes = write_document(&redacted, &[], &WriteOptions { compress }).unwrap();
        let back = read_document(&bytes, &ReadOptions::default()).unwrap();
        prop_assert_eq!(back.document_id, redacted.document_id().unwrap());
    }
}
