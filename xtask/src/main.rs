//! Workspace developer tasks.
//!
//! `cargo run -p xtask -- gen-vectors` regenerates the public
//! conformance corpus under `testdata/`: valid and invalid `.vsd` files
//! plus `vectors.json` recording expected outcomes. A second
//! implementation should pass this corpus without reading our source —
//! it is the seed of design invariant I4 (one reference renderer,
//! conformance-tested).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::json;

use vsd_container::{write_document, WriteOptions};
use vsd_core::document::DocumentBuilder;
use vsd_core::forms::{ArithOp, CmpOp, Expr, FieldValue};
use vsd_core::manifest::{Blob, Metadata, Profile, ResourceEntry, ResourceKind};
use vsd_core::tree::{
    Cell, CellScope, ColSpec, Direction, Doc, Field, FieldKind, Figure, Heading, Inline, List,
    Node, Para, Row, Section, Table,
};
use vsd_core::{Document, ResourceTable};

fn main() -> Result<()> {
    let task = std::env::args().nth(1).unwrap_or_default();
    match task.as_str() {
        "gen-vectors" => gen_vectors(),
        other => bail!("unknown task {other:?}; available: gen-vectors"),
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Fixed signing seed: conformance vectors must be reproducible, and
/// Ed25519 signing is deterministic (RFC 8032), so the same seed always
/// yields byte-identical signatures. This key is obviously public —
/// vectors prove *verification* behavior, not custody.
const VECTOR_KEY_SEED: [u8; 32] = [42u8; 32];

fn gen_vectors() -> Result<()> {
    let testdata = root().join("testdata");
    let valid = testdata.join("valid");
    let invalid = testdata.join("invalid");
    std::fs::create_dir_all(&valid)?;
    std::fs::create_dir_all(&invalid)?;

    let mut valid_entries = Vec::new();
    let mut invalid_entries = Vec::new();

    // --- Valid vectors ------------------------------------------------------

    let minimal = minimal_doc()?;
    let raw = write_document(&minimal, &[], &WriteOptions { compress: false })?;
    write(&valid.join("minimal.vsd"), &raw)?;
    valid_entries.push(json!({
        "file": "valid/minimal.vsd",
        "doc_id": minimal.document_id()?.to_hex(),
        "objects": minimal.store.len(),
        "profile": "core",
        "note": "smallest interesting document: heading, paragraph, table, figure",
    }));

    let compressed = write_document(&minimal, &[], &WriteOptions { compress: true })?;
    write(&valid.join("minimal-compressed.vsd"), &compressed)?;
    valid_entries.push(json!({
        "file": "valid/minimal-compressed.vsd",
        "doc_id": minimal.document_id()?.to_hex(),
        "objects": minimal.store.len(),
        "profile": "core",
        "note": "same document id as minimal.vsd: identity survives recompression (spec 2.4)",
    }));

    let key = vsd_sign::SigningKey::from_seed(&VECTOR_KEY_SEED)?;
    let sig = key.sign_document(&minimal)?;
    let signed = write_document(&minimal, &[sig], &WriteOptions { compress: false })?;
    write(&valid.join("signed.vsd"), &signed)?;
    valid_entries.push(json!({
        "file": "valid/signed.vsd",
        "doc_id": minimal.document_id()?.to_hex(),
        "signatures": 1,
        "signer_pubkey": hex::encode(key.verifying_key().to_bytes()),
        "profile": "core",
        "note": "one valid ed25519 document-scope signature (deterministic test key)",
    }));

    let form = form_doc()?;
    let form_bytes = write_document(&form, &[], &WriteOptions { compress: false })?;
    write(&valid.join("form-filled.vsd"), &form_bytes)?;
    valid_entries.push(json!({
        "file": "valid/form-filled.vsd",
        "doc_id": form.document_id()?.to_hex(),
        "profile": "form",
        "note": "field layer with filled values, computed field, and constraints",
    }));

    // Laid-out vector: the cross-platform layout determinism proof.
    // vectors.json is generated on one OS and CI regenerates + diffs it
    // on another — if integer-µm layout were not platform-identical,
    // this hash would not survive the trip.
    let laid = vsd_layout::add_render_cache(&minimal, &vsd_layout::LayoutOptions::default())?;
    let laid_bytes = write_document(&laid, &[], &WriteOptions { compress: false })?;
    write(&valid.join("laid-out.vsd"), &laid_bytes)?;
    let cache = laid.render_cache()?.expect("cache present");
    valid_entries.push(json!({
        "file": "valid/laid-out.vsd",
        "doc_id": laid.document_id()?.to_hex(),
        "predecessor": minimal.document_id()?.to_hex(),
        "profile": "core",
        "layout_engine": format!("{}/{}", vsd_layout::ENGINE_NAME, vsd_layout::ENGINE_VERSION),
        "layout_hash": hex::encode(cache.layout_hash),
        "layout_pages": cache.pages.len(),
        "note": "render cache from vsd-layout/1.0; recomputing layout MUST reproduce these exact page objects (LAYOUT-1.0.md)",
    }));

    let (redacted, predecessor_id, proof) = redacted_doc()?;
    let red_bytes = write_document(&redacted, &[], &WriteOptions { compress: false })?;
    write(&valid.join("redacted.vsd"), &red_bytes)?;
    valid_entries.push(json!({
        "file": "valid/redacted.vsd",
        "doc_id": redacted.document_id()?.to_hex(),
        "predecessor": predecessor_id.to_hex(),
        "redaction_proof": hex::encode(proof),
        "profile": "core",
        "must_not_contain": "TOP-SECRET-ACCOUNT-9912",
        "note": "destructive redaction: the secret exists in no byte of this file (spec 7.2)",
    }));

    // Sealed + disclosure vectors (spec §7.3): fixed salts keep the
    // corpus deterministic — test vectors prove *mechanics*, privacy
    // comes from random salts in production.
    let mut salt_counter = 0u8;
    let mut fixed_salts = move || {
        salt_counter += 1;
        [salt_counter; 16]
    };
    let sealed = vsd_core::disclose::seal_salted(&minimal, &mut fixed_salts)?;
    let sealed_bytes = write_document(&sealed, &[], &WriteOptions { compress: false })?;
    write(&valid.join("sealed-salted.vsd"), &sealed_bytes)?;
    valid_entries.push(json!({
        "file": "valid/sealed-salted.vsd",
        "doc_id": sealed.document_id()?.to_hex(),
        "predecessor": minimal.document_id()?.to_hex(),
        "profile": "core",
        "note": "every top-level block hoisted behind a salted SubtreeRef (deterministic salts for the corpus)",
    }));

    let bundle = vsd_core::disclose::disclose(&sealed, 0)?;
    write(&testdata.join("valid/disclosure.vsdp"), &bundle.encode()?)?;
    valid_entries.push(json!({
        "file": "valid/disclosure.vsdp",
        "kind": "disclosure-bundle",
        "doc_id": sealed.document_id()?.to_hex(),
        "disclosed_index": 0,
        "disclosed_contains": "Service Agreement",
        "hidden_siblings": 3,
        "salted": true,
        "note": "selective disclosure of block 0; a conforming verifier MUST accept it against doc_id and MUST reject any byte modification",
    }));

    // --- Invalid vectors ----------------------------------------------------

    let mut bad_magic = raw.clone();
    bad_magic[4] = b'\n'; // the classic text-mode transfer corruption
    write(&invalid.join("bad-magic.vsd"), &bad_magic)?;
    invalid_entries.push(json!({
        "file": "invalid/bad-magic.vsd",
        "reject_reason": "magic bytes corrupted (text-mode transfer)",
    }));

    write(&invalid.join("truncated.vsd"), &raw[..raw.len() - 7])?;
    invalid_entries.push(json!({
        "file": "invalid/truncated.vsd",
        "reject_reason": "file shorter than the size declared in the header",
    }));

    let mut corrupt = raw.clone();
    let needle = b"Service Agreement";
    let pos = corrupt
        .windows(needle.len())
        .position(|w| w == needle)
        .context("payload text not found")?;
    corrupt[pos] ^= 0x01;
    write(&invalid.join("corrupt-objs.vsd"), &corrupt)?;
    invalid_entries.push(json!({
        "file": "invalid/corrupt-objs.vsd",
        "reject_reason": "single flipped bit in the object store (checksum + object hash)",
    }));

    let mut evil = raw.clone();
    let pos = evil
        .windows(4)
        .position(|w| w == b"OBJS")
        .context("OBJS fourcc not found")?;
    evil[pos..pos + 4].copy_from_slice(b"EVIL");
    write(&invalid.join("unknown-critical.vsd"), &evil)?;
    invalid_entries.push(json!({
        "file": "invalid/unknown-critical.vsd",
        "reject_reason": "unknown chunk type with the critical flag set",
    }));

    write(
        &invalid.join("trailer-docid-mismatch.vsd"),
        &tamper_trailer_docid(&raw)?,
    )?;
    invalid_entries.push(json!({
        "file": "invalid/trailer-docid-mismatch.vsd",
        "reject_reason": "trailer doc-id does not equal BLAKE3(MNFST payload); chunk checksum deliberately fixed up so only the identity check can catch it",
    }));

    write(
        &invalid.join("noncanonical-mnfst.vsd"),
        &noncanonical_manifest_file(&minimal)?,
    )?;
    invalid_entries.push(json!({
        "file": "invalid/noncanonical-mnfst.vsd",
        "reject_reason": "MNFST payload carries a trailing byte: not the canonical encoding, so it cannot be the document identity preimage",
    }));

    // --- Expectation manifest ------------------------------------------------

    let manifest = json!({
        "format": "VSD conformance vectors",
        "format_version": [0, 1],
        "generator": "cargo run -p xtask -- gen-vectors",
        "rules": {
            "valid": "a conforming reader MUST accept the file, derive exactly doc_id, and report it valid",
            "invalid": "a conforming reader MUST reject the file at read or validation time",
        },
        "valid": valid_entries,
        "invalid": invalid_entries,
    });
    write(
        &testdata.join("vectors.json"),
        (serde_json::to_string_pretty(&manifest)? + "\n").as_bytes(),
    )?;

    println!(
        "conformance vectors regenerated under {}",
        testdata.display()
    );
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    println!("  {}", path.display());
    Ok(())
}

// --- documents --------------------------------------------------------------

fn minimal_doc() -> Result<Document> {
    let blob = Blob {
        mime: "image/png".into(),
        data: vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3],
    };
    let mut builder = DocumentBuilder::new(Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        children: vec![],
    }));
    let blob_id = builder.add_object(blob.to_value())?;

    let root = Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        children: vec![
            Node::Heading(Heading {
                level: 1,
                children: vec![Inline::Text("Service Agreement".into())],
            }),
            Node::Para(Para {
                children: vec![Inline::Text(
                    "This is the canonical minimal conformance document.".into(),
                )],
            }),
            Node::Table(Table {
                cols: vec![ColSpec { width: None }, ColSpec { width: None }],
                head: vec![Row {
                    cells: vec![header_cell("Item"), header_cell("Price")],
                }],
                body: vec![Row {
                    cells: vec![body_cell("Widget"), body_cell("4.20")],
                }],
                foot: vec![],
            }),
            Node::Figure(Figure {
                res: blob_id,
                alt: "A small test image".into(),
                decorative: false,
                caption: vec![],
            }),
        ],
    });

    let mut builder = DocumentBuilder::new(root).metadata(Metadata {
        title: Some("VSD conformance: minimal".into()),
        authors: vec!["VSD test vectors".into()],
        created: Some("2026-06-10T00:00:00Z".into()),
        ..Default::default()
    });
    builder.add_object(blob.to_value())?;
    Ok(builder
        .resources(ResourceTable {
            entries: vec![(
                "img0".into(),
                ResourceEntry {
                    kind: ResourceKind::Image,
                    mime: "image/png".into(),
                    data: blob_id,
                },
            )],
            styles: vec![],
        })
        .profile(Profile::Core)
        .build()?)
}

fn header_cell(text: &str) -> Cell {
    Cell {
        span: None,
        scope: Some(CellScope::Col),
        children: vec![Node::Para(Para {
            children: vec![Inline::Text(text.into())],
        })],
    }
}

fn body_cell(text: &str) -> Cell {
    Cell {
        span: None,
        scope: None,
        children: vec![Node::Para(Para {
            children: vec![Inline::Text(text.into())],
        })],
    }
}

fn form_doc() -> Result<Document> {
    let root = Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        children: vec![
            Node::Heading(Heading {
                level: 1,
                children: vec![Inline::Text("Order form".into())],
            }),
            Node::Field(Field {
                id: "qty".into(),
                kind: FieldKind::Number,
                label: Some("Quantity".into()),
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
                label: Some("Total".into()),
                required: false,
                constraint: None,
                computed: Some(Expr::Arith(
                    ArithOp::Mul,
                    vec![Expr::FieldRef("qty".into()), Expr::Num(9.5)],
                )),
            }),
        ],
    });
    let blank = DocumentBuilder::new(root).profile(Profile::Form).build()?;
    let mut inputs = BTreeMap::new();
    inputs.insert("qty".to_string(), FieldValue::Num(4.0));
    let filled = vsd_core::fill::fill(&blank, &inputs)?;
    anyhow::ensure!(filled.violations.is_empty());
    Ok(filled.document)
}

fn redacted_doc() -> Result<(Document, vsd_core::ObjectId, [u8; 32])> {
    let root = Node::Doc(Doc {
        lang: "en".into(),
        dir: Direction::Ltr,
        children: vec![
            Node::Para(Para {
                children: vec![Inline::Text("Public preamble.".into())],
            }),
            Node::Section(Section {
                role: "secrets".into(),
                children: vec![
                    Node::Para(Para {
                        children: vec![Inline::Text(
                            "Account number TOP-SECRET-ACCOUNT-9912 must not leak.".into(),
                        )],
                    }),
                    Node::List(List {
                        ordered: false,
                        items: vec![vec![Node::Para(Para {
                            children: vec![Inline::Text("A surviving sibling item.".into())],
                        })]],
                    }),
                ],
            }),
        ],
    });
    let original = DocumentBuilder::new(root).build()?;
    let predecessor = original.document_id()?;
    let r = vsd_core::redact::redact(&original, &[1, 0], Some("account number".into()))?;
    Ok((r.document, predecessor, r.proof))
}

// --- surgical corruption helpers ---------------------------------------------

/// Flip a byte inside the trailer's doc-id and *fix up the chunk
/// checksum*, so the only thing that can catch the tamper is the
/// doc-id ↔ BLAKE3(MNFST) cross-check itself.
fn tamper_trailer_docid(file: &[u8]) -> Result<Vec<u8>> {
    let mut out = file.to_vec();
    let trailer_off = u64::from_le_bytes(out[24..32].try_into().unwrap()) as usize;
    let payload_len =
        u64::from_le_bytes(out[trailer_off..trailer_off + 8].try_into().unwrap()) as usize;
    let payload_start = trailer_off + 16;

    // Locate the doc-id: the CBOR key "doc-id" is followed by 0x58 0x20
    // (bytes(32)); flip the first id byte.
    let payload = &out[payload_start..payload_start + payload_len];
    let key = b"doc-id";
    let kpos = payload
        .windows(key.len())
        .position(|w| w == key)
        .context("doc-id key not found in trailer")?;
    let id_pos = payload_start + kpos + key.len() + 2;
    out[id_pos] ^= 0xff;

    // Recompute the trailer chunk checksum over the tampered payload.
    let new_payload = &out[payload_start..payload_start + payload_len];
    let checksum = blake3::hash(new_payload);
    let check_pos = payload_start + payload_len;
    out[check_pos..check_pos + 8].copy_from_slice(&checksum.as_bytes()[..8]);
    Ok(out)
}

/// Rebuild a container whose MNFST payload has one trailing byte: every
/// checksum is valid, but the manifest bytes are not a canonical CBOR
/// encoding — so they cannot be the preimage of a document identity.
fn noncanonical_manifest_file(doc: &Document) -> Result<Vec<u8>> {
    const MAGIC: [u8; 8] = [0x89, b'V', b'S', b'D', 0x0d, 0x0a, 0x1a, 0x0a];

    fn chunk(fourcc: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 24);
        out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        out.extend_from_slice(fourcc);
        out.extend_from_slice(&1u32.to_le_bytes()); // critical
        out.extend_from_slice(payload);
        out.extend_from_slice(&blake3::hash(payload).as_bytes()[..8]);
        out
    }

    let mut mnfst_payload = doc.manifest.to_value().encode()?;
    mnfst_payload.push(0x00); // the poison byte

    // Object store + index, mirroring the writer's layout.
    let mut objs_payload = Vec::new();
    let mut entries: Vec<(vsd_core::cbor::Value, vsd_core::cbor::Value)> = Vec::new();
    let header_len = 32u64;
    let mnfst_chunk = chunk(b"MNFS", &mnfst_payload);

    // Two-pass fixpoint for the OBJS offset, like the real writer.
    let mut objs_offset_guess = header_len + mnfst_chunk.len() as u64;
    let index_payload = loop {
        entries.clear();
        objs_payload.clear();
        for (id, bytes) in doc.store.iter() {
            entries.push((
                vsd_core::cbor::Value::Bytes(id.as_slice().to_vec()),
                vsd_core::cbor::Value::Array(vec![
                    vsd_core::cbor::Value::Unsigned(objs_offset_guess),
                    vsd_core::cbor::Value::Unsigned(objs_payload.len() as u64),
                    vsd_core::cbor::Value::Unsigned(bytes.len() as u64),
                    vsd_core::cbor::Value::Unsigned(0),
                ]),
            ));
            objs_payload.extend_from_slice(bytes);
        }
        let payload = vsd_core::cbor::Value::Map(entries.clone()).encode()?;
        let resolved = header_len + mnfst_chunk.len() as u64 + payload.len() as u64 + 24;
        if resolved == objs_offset_guess {
            break payload;
        }
        objs_offset_guess = resolved;
    };

    let index_chunk = chunk(b"INDX", &index_payload);
    let objs_chunk = chunk(b"OBJS", &objs_payload);
    let trailer_offset =
        header_len + (mnfst_chunk.len() + index_chunk.len() + objs_chunk.len()) as u64;
    let trailer_payload = vsd_core::cbor::MapBuilder::new()
        .put(
            "index-offset",
            vsd_core::cbor::Value::Unsigned(header_len + mnfst_chunk.len() as u64),
        )
        .put("mnfst-offset", vsd_core::cbor::Value::Unsigned(header_len))
        .put(
            "doc-id",
            vsd_core::cbor::Value::Bytes(blake3::hash(&mnfst_payload).as_bytes().to_vec()),
        )
        .build()
        .encode()?;
    let trailer_chunk = chunk(b"TRLR", &trailer_payload);
    let total = trailer_offset + trailer_chunk.len() as u64;

    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&0u16.to_le_bytes()); // major
    out.extend_from_slice(&1u16.to_le_bytes()); // minor
    out.extend_from_slice(&1u32.to_le_bytes()); // core profile flag
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&trailer_offset.to_le_bytes());
    out.extend_from_slice(&mnfst_chunk);
    out.extend_from_slice(&index_chunk);
    out.extend_from_slice(&objs_chunk);
    out.extend_from_slice(&trailer_chunk);
    Ok(out)
}
