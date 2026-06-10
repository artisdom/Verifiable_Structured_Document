//! `vsd` — the VSD reference command-line tool.

#![forbid(unsafe_code)]

mod author;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use vsd_container::{read_file, write_file, ReadOptions, SigScope, WriteOptions};
use vsd_core::manifest::Profile;
use vsd_core::validate::Severity;

#[derive(Parser)]
#[command(
    name = "vsd",
    version,
    about = "Verifiable Structured Document — layout fidelity of PDF, parseability of HTML,\nintegrity model of Git, attack surface of a JPEG."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a .vsd from a JSON authoring file.
    Pack {
        /// Input JSON document (see `vsd pack --help` for the dialect).
        input: PathBuf,
        /// Output .vsd path.
        #[arg(short, long)]
        output: PathBuf,
        /// Conformance profile: core, archive, form, stream.
        #[arg(long, default_value = "core")]
        profile: String,
        /// Disable zstd compression of the object store.
        #[arg(long)]
        no_compress: bool,
    },
    /// Show document identity, manifest, and store statistics.
    Info { file: PathBuf },
    /// Validate a document against the spec and its profile.
    Validate { file: PathBuf },
    /// Extract content: plain text (default) or the full tree as JSON.
    Extract {
        file: PathBuf,
        /// Output format: text | json
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// List every object in the store with sizes.
    Objects { file: PathBuf },
    /// Generate an Ed25519 keypair.
    Keygen {
        /// Output path for the secret key (public key gets .pub appended).
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Sign a document (whole-document scope) and write a new file.
    Sign {
        file: PathBuf,
        /// Secret key file from `vsd keygen`.
        #[arg(short, long)]
        key: PathBuf,
        /// Output path (defaults to overwriting the input).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Verify container integrity, document validity, and all signatures.
    Verify { file: PathBuf },
    /// Destructively redact the subtree at PATH (e.g. "2" or "1.3").
    Redact {
        file: PathBuf,
        /// Dot-separated child indices from the document root.
        #[arg(long)]
        path: String,
        /// Reason recorded in the redaction node.
        #[arg(long)]
        reason: Option<String>,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Compare two documents as object-set + structural diffs.
    Diff { old: PathBuf, new: PathBuf },
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Pack {
            input,
            output,
            profile,
            no_compress,
        } => pack(&input, &output, &profile, no_compress),
        Command::Info { file } => info(&file),
        Command::Validate { file } => validate(&file),
        Command::Extract { file, format } => extract(&file, &format),
        Command::Objects { file } => objects(&file),
        Command::Keygen { output } => keygen(&output),
        Command::Sign { file, key, output } => sign(&file, &key, output.as_deref()),
        Command::Verify { file } => verify(&file),
        Command::Redact {
            file,
            path,
            reason,
            output,
        } => redact(&file, &path, reason, &output),
        Command::Diff { old, new } => diff(&old, &new),
    }
}

fn load(path: &Path) -> Result<vsd_container::VsdFile> {
    read_file(path, &ReadOptions::default())
        .with_context(|| format!("reading {}", path.display()))
}

fn pack(input: &Path, output: &Path, profile: &str, no_compress: bool) -> Result<()> {
    let profile = Profile::parse(profile)?;
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?,
    )
    .context("parsing authoring JSON")?;
    let base = input.parent().unwrap_or(Path::new("."));
    let doc = author::document_from_json(&json, base, profile)?;

    // Refuse to write an invalid document.
    let report = vsd_core::validate::validate(&doc);
    print_findings(&report);
    if !report.is_valid() {
        bail!("document failed validation; not writing output");
    }

    let opts = WriteOptions {
        compress: !no_compress,
    };
    write_file(output, &doc, &[], &opts)?;
    let size = std::fs::metadata(output)?.len();
    println!(
        "wrote {} ({} bytes, {} objects)\ndocument id: {}",
        output.display(),
        size,
        doc.store.len(),
        doc.document_id()?
    );
    Ok(())
}

fn info(file: &Path) -> Result<()> {
    let vsd = load(file)?;
    let doc = &vsd.document;
    let meta = doc.metadata()?;
    println!("document id : {}", vsd.document_id);
    println!(
        "format      : VSD {}.{} (profile: {})",
        vsd.format_version.0,
        vsd.format_version.1,
        doc.manifest.profile.as_str()
    );
    if let Some(title) = &meta.title {
        println!("title       : {title}");
    }
    if !meta.authors.is_empty() {
        println!("authors     : {}", meta.authors.join(", "));
    }
    if let Some(created) = &meta.created {
        println!("created     : {created}");
    }
    println!(
        "objects     : {} ({} bytes canonical)",
        doc.store.len(),
        doc.store.total_bytes()
    );
    println!(
        "render cache: {}",
        if doc.manifest.render_cache.is_some() { "present" } else { "none (structure-only)" }
    );
    if let Some(pred) = doc.manifest.predecessor {
        println!("predecessor : {pred} (amendment chain)");
    }
    if let Some(prov) = doc.provenance()? {
        println!("provenance  : {} assertion(s)", prov.assertions.len());
        for a in &prov.assertions {
            println!("  - {}", a.kind);
        }
    }
    println!(
        "signatures  : {}",
        if vsd.signatures.is_empty() {
            "none".to_owned()
        } else {
            format!("{} (run `vsd verify` to check)", vsd.signatures.len())
        }
    );
    Ok(())
}

fn validate(file: &Path) -> Result<()> {
    let vsd = load(file)?;
    let report = vsd_core::validate::validate(&vsd.document);
    print_findings(&report);
    if report.is_valid() {
        println!(
            "VALID — {} conforms to VSD/{}",
            file.display(),
            vsd.document.manifest.profile.as_str()
        );
        Ok(())
    } else {
        bail!("{} is not a valid VSD document", file.display());
    }
}

fn print_findings(report: &vsd_core::validate::Report) {
    for f in &report.findings {
        let tag = match f.severity {
            Severity::Error => "ERROR",
            Severity::Warning => "warn ",
        };
        eprintln!("[{tag}] {}: {}", f.code, f.message);
    }
}

fn extract(file: &Path, format: &str) -> Result<()> {
    let vsd = load(file)?;
    match format {
        "text" => print!("{}", vsd_core::extract::extract_text(&vsd.document)?),
        "json" => {
            let root = vsd.document.root_node()?;
            let v = root.to_value()?;
            println!("{}", serde_json::to_string_pretty(&cbor_to_json(&v))?);
        }
        other => bail!("unknown format {other:?} (use text or json)"),
    }
    Ok(())
}

fn objects(file: &Path) -> Result<()> {
    let vsd = load(file)?;
    let closure = vsd.document.closure()?;
    for (id, bytes) in vsd.document.store.iter() {
        let mark = if *id == vsd.document.manifest.root {
            " (root)"
        } else if !closure.contains(id) {
            " (ORPHAN)"
        } else {
            ""
        };
        println!("{id}  {:>10} bytes{mark}", bytes.len());
    }
    Ok(())
}

fn keygen(output: &Path) -> Result<()> {
    let key = vsd_sign::SigningKey::generate();
    let pub_path = output.with_extension(format!(
        "{}pub",
        output
            .extension()
            .map(|e| format!("{}.", e.to_string_lossy()))
            .unwrap_or_default()
    ));
    std::fs::write(output, hex::encode(key.seed()))?;
    std::fs::write(&pub_path, hex::encode(key.verifying_key().to_bytes()))?;
    println!(
        "secret key: {}\npublic key: {} ({})",
        output.display(),
        pub_path.display(),
        hex::encode(key.verifying_key().to_bytes())
    );
    println!("keep the secret key offline; only the .pub needs distribution");
    Ok(())
}

fn read_key(path: &Path) -> Result<vsd_sign::SigningKey> {
    let hex_str = std::fs::read_to_string(path)
        .with_context(|| format!("reading key {}", path.display()))?;
    let seed = hex::decode(hex_str.trim()).context("key file must be hex")?;
    Ok(vsd_sign::SigningKey::from_seed(&seed)?)
}

fn sign(file: &Path, key: &Path, output: Option<&Path>) -> Result<()> {
    let vsd = load(file)?;
    let key = read_key(key)?;
    let sig = key.sign_document(&vsd.document)?;
    let mut sigs = vsd.signatures.clone();
    sigs.push(sig);
    let out = output.unwrap_or(file);
    write_file(out, &vsd.document, &sigs, &WriteOptions::default())?;
    println!(
        "signed {} (document id {})\nsignatures now: {}",
        out.display(),
        vsd.document_id,
        sigs.len()
    );
    Ok(())
}

fn verify(file: &Path) -> Result<()> {
    // Container integrity (checksums, hashes, canonical form) is enforced
    // during load — reaching this line means the bytes are intact.
    let vsd = load(file)?;
    println!("container   : OK (all chunk checksums and object hashes verified)");
    println!("document id : {}", vsd.document_id);

    let report = vsd_core::validate::validate(&vsd.document);
    print_findings(&report);
    println!(
        "validation  : {}",
        if report.is_valid() { "OK" } else { "FAILED" }
    );

    let mut all_ok = report.is_valid();
    if vsd.signatures.is_empty() {
        println!("signatures  : none present");
    }
    for (i, sig) in vsd.signatures.iter().enumerate() {
        let verdict = vsd_sign::verify(&vsd.document, sig)?;
        let desc = match verdict {
            vsd_sign::Verdict::Valid => "VALID".to_owned(),
            vsd_sign::Verdict::ValidForOtherTarget => {
                "valid cryptography, but target is not this document (predecessor signature?)"
                    .to_owned()
            }
            vsd_sign::Verdict::Invalid(reason) => {
                all_ok = false;
                format!("INVALID — {reason}")
            }
        };
        println!(
            "signature {i} : [{}] key {}… → {desc}",
            match sig.scope {
                SigScope::Document => "document",
                SigScope::Subtree => "subtree",
                SigScope::FieldLayer => "field-layer",
            },
            &hex::encode(&sig.pubkey)[..16]
        );
    }
    if all_ok {
        println!("VERIFIED");
        Ok(())
    } else {
        bail!("verification failed");
    }
}

fn redact(file: &Path, path_str: &str, reason: Option<String>, output: &Path) -> Result<()> {
    let vsd = load(file)?;
    let path: Vec<usize> = path_str
        .split('.')
        .map(|p| p.parse::<usize>().context("path must be dot-separated indices"))
        .collect::<Result<Vec<_>>>()?;
    let result = vsd_core::redact::redact(&vsd.document, &path, reason)?;

    // Spec §7.2: prior signatures cover the pre-redaction manifest; they
    // belong to the predecessor, not to this revision. Drop them.
    write_file(output, &result.document, &[], &WriteOptions::default())?;
    println!(
        "redacted node at path {path_str} → {}",
        output.display()
    );
    println!("removed-subtree proof: {}", hex::encode(result.proof));
    println!(
        "purged {} unreferenced object(s) from the store",
        result.purged.len()
    );
    if result.cache_invalidated {
        println!("render cache invalidated (recompute required by spec §7.2)");
    }
    println!(
        "new document id: {} (predecessor: {})",
        result.document.document_id()?,
        vsd.document_id
    );
    Ok(())
}

fn diff(old: &Path, new: &Path) -> Result<()> {
    let a = load(old)?;
    let b = load(new)?;
    let d = vsd_core::diff::diff(&a.document, &b.document)?;
    if d.same_document {
        println!(
            "identical documents (id {}) — possibly different containers/compression",
            a.document_id
        );
        return Ok(());
    }
    println!("old: {}", a.document_id);
    println!("new: {}", b.document_id);
    if b.document.manifest.predecessor == Some(a.document_id) {
        println!("new declares old as its predecessor (amendment chain)");
    }
    println!(
        "objects: {} shared, {} added (+{} B), {} removed (-{} B)",
        d.shared,
        d.added.len(),
        d.added_bytes,
        d.removed.len(),
        d.removed_bytes
    );
    for p in &d.changed_paths {
        println!("changed: {p}");
    }
    Ok(())
}

/// Render a CBOR value as JSON for `extract --format json`.
/// Byte strings become hex; this is a debug/interop view, not a
/// canonical representation.
fn cbor_to_json(v: &vsd_core::cbor::Value) -> serde_json::Value {
    use serde_json::Value as J;
    use vsd_core::cbor::Value as C;
    match v {
        C::Unsigned(n) => J::from(*n),
        C::Negative(n) => J::from(-1i128.saturating_sub(*n as i128) as i64),
        C::Bytes(b) => J::String(format!("hex:{}", hex::encode(b))),
        C::Text(s) => J::String(s.clone()),
        C::Array(a) => J::Array(a.iter().map(cbor_to_json).collect()),
        C::Map(m) => J::Object(
            m.iter()
                .map(|(k, val)| {
                    let key = match k {
                        C::Text(s) => s.clone(),
                        other => format!("{other:?}"),
                    };
                    (key, cbor_to_json(val))
                })
                .collect(),
        ),
        C::Bool(b) => J::Bool(*b),
        C::Null => J::Null,
        C::Float(x) => serde_json::Number::from_f64(*x).map(J::Number).unwrap_or(J::Null),
    }
}
