//! Conformance-vector runner: every file under `testdata/` must behave
//! exactly as `vectors.json` declares. A second implementation passing
//! this corpus interoperates with this one — that is the point.
//!
//! Regenerate the corpus with `cargo run -p xtask -- gen-vectors`.

use std::path::PathBuf;

use vsd_container::{read_file, ReadOptions};

fn testdata() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

fn vectors() -> serde_json::Value {
    let path = testdata().join("vectors.json");
    serde_json::from_str(&std::fs::read_to_string(&path).expect("read vectors.json"))
        .expect("parse vectors.json")
}

#[test]
fn valid_vectors_accepted_with_expected_identity() {
    let v = vectors();
    let entries = v["valid"].as_array().expect("valid array");
    assert!(!entries.is_empty());

    for entry in entries {
        let file = entry["file"].as_str().unwrap();
        let path = testdata().join(file);
        let vsd = read_file(&path, &ReadOptions::default())
            .unwrap_or_else(|e| panic!("{file}: conforming reader must accept: {e}"));

        let expected_id = entry["doc_id"].as_str().unwrap();
        assert_eq!(
            vsd.document_id.to_hex(),
            expected_id,
            "{file}: document identity mismatch"
        );

        let report = vsd_core::validate::validate(&vsd.document);
        assert!(
            report.is_valid(),
            "{file}: must validate, findings: {:?}",
            report.findings
        );

        if let Some(profile) = entry["profile"].as_str() {
            assert_eq!(vsd.document.manifest.profile.as_str(), profile, "{file}");
        }
        if let Some(n) = entry["objects"].as_u64() {
            assert_eq!(vsd.document.store.len() as u64, n, "{file}: object count");
        }
        if let Some(n) = entry["signatures"].as_u64() {
            assert_eq!(vsd.signatures.len() as u64, n, "{file}: signature count");
            for sig in &vsd.signatures {
                assert_eq!(
                    vsd_sign::verify(&vsd.document, sig).unwrap(),
                    vsd_sign::Verdict::Valid,
                    "{file}: signature must verify"
                );
            }
        }
        if let Some(pred) = entry["predecessor"].as_str() {
            assert_eq!(
                vsd.document.manifest.predecessor.map(|p| p.to_hex()),
                Some(pred.to_string()),
                "{file}: predecessor chain"
            );
        }
        if let Some(expected_hash) = entry["layout_hash"].as_str() {
            // The cross-platform layout determinism gate: re-run the
            // engine on THIS machine and require the recorded hash.
            let cache = vsd.document.render_cache().unwrap().expect("cache");
            assert_eq!(
                hex::encode(cache.layout_hash),
                expected_hash,
                "{file}: stored layout hash"
            );
            if let Some(n) = entry["layout_pages"].as_u64() {
                assert_eq!(cache.pages.len() as u64, n, "{file}: page count");
            }
            match vsd_layout::verify_render_cache(&vsd.document).unwrap() {
                vsd_layout::RecomputeOutcome::Match { .. } => {}
                other => panic!(
                    "{file}: recomputation must reproduce the cache byte-identically \
                     on every platform, got {other:?}"
                ),
            }
        }
        if let Some(secret) = entry["must_not_contain"].as_str() {
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
                "{file}: redacted content leaked"
            );
        }
    }
}

#[test]
fn invalid_vectors_rejected() {
    let v = vectors();
    let entries = v["invalid"].as_array().expect("invalid array");
    assert!(!entries.is_empty());

    for entry in entries {
        let file = entry["file"].as_str().unwrap();
        let path = testdata().join(file);
        let result = read_file(&path, &ReadOptions::default());
        assert!(
            result.is_err(),
            "{file}: a conforming reader MUST reject this file ({})",
            entry["reject_reason"].as_str().unwrap_or("")
        );
    }
}

#[test]
fn recompression_preserves_identity_across_vectors() {
    // The pair of vectors that *demonstrates* spec §2.4: different bytes,
    // same document.
    let a = read_file(
        testdata().join("valid/minimal.vsd"),
        &ReadOptions::default(),
    )
    .unwrap();
    let b = read_file(
        testdata().join("valid/minimal-compressed.vsd"),
        &ReadOptions::default(),
    )
    .unwrap();
    let raw = std::fs::read(testdata().join("valid/minimal.vsd")).unwrap();
    let compressed = std::fs::read(testdata().join("valid/minimal-compressed.vsd")).unwrap();
    assert_ne!(raw, compressed, "vectors should differ at the byte level");
    assert_eq!(a.document_id, b.document_id, "…but be the same document");
}
