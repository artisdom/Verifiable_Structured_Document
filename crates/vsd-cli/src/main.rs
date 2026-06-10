//! `vsd` — the VSD reference command-line tool.

#![forbid(unsafe_code)]

mod author;
mod markdown;

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
    /// Create a .vsd from a JSON authoring file or a Markdown file.
    Pack {
        /// Input document: .json (authoring dialect) or .md (CommonMark
        /// + tables). Format is inferred from the extension.
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
    Verify {
        file: PathBuf,
        /// Also re-run the layout engine and require the render cache to
        /// match the content tree exactly (spec §5.2) — the check that
        /// makes "visible pixels ≠ extracted text" detectable.
        #[arg(long)]
        recompute: bool,
    },
    /// Lay the document out with vsd-layout/1.0, attaching a verifiable
    /// render cache and page index (produces a successor document).
    Layout {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Page size: a4 | letter.
        #[arg(long, default_value = "a4")]
        page_size: String,
    },
    /// Rasterize a page of the render cache to PNG.
    Render {
        file: PathBuf,
        /// 1-based page number.
        #[arg(long, default_value_t = 1)]
        page: usize,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long, default_value_t = 144.0)]
        dpi: f64,
    },
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
    /// Fill form fields, producing a new document layered over the base.
    Fill {
        file: PathBuf,
        /// Field assignments, e.g. --set qty=3 --set name="Alice".
        /// Values are parsed according to the field's kind.
        #[arg(long = "set", value_name = "ID=VALUE")]
        sets: Vec<String>,
        #[arg(short, long)]
        output: PathBuf,
        /// Fail (instead of warn) when constraints are violated.
        #[arg(long)]
        strict: bool,
    },
    /// Replace all fields with their final values (defined merge, §6).
    Flatten {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Export to tagged PDF. By default the canonical .vsd travels
    /// inside the PDF as an attachment (hybrid PDF), making the round
    /// trip back to VSD lossless and verifiable.
    Export {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Do not embed the source .vsd in the PDF.
        #[arg(long)]
        no_embed_source: bool,
    },
    /// Import a PDF: lossless if it is a hybrid PDF carrying its VSD
    /// source; otherwise heuristic structure recovery (marked lossy in
    /// provenance, original PDF embedded as an attachment).
    Import {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Batch-convert a directory of .json/.md/.pdf files to .vsd and
    /// report cross-document object deduplication.
    Migrate {
        input_dir: PathBuf,
        #[arg(short, long)]
        output_dir: PathBuf,
        #[arg(long, default_value = "core")]
        profile: String,
    },
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
        Command::Verify { file, recompute } => verify(&file, recompute),
        Command::Layout {
            file,
            output,
            page_size,
        } => layout(&file, &output, &page_size),
        Command::Render {
            file,
            page,
            output,
            dpi,
        } => render(&file, page, &output, dpi),
        Command::Redact {
            file,
            path,
            reason,
            output,
        } => redact(&file, &path, reason, &output),
        Command::Diff { old, new } => diff(&old, &new),
        Command::Fill {
            file,
            sets,
            output,
            strict,
        } => fill(&file, &sets, &output, strict),
        Command::Flatten { file, output } => flatten(&file, &output),
        Command::Export {
            file,
            output,
            no_embed_source,
        } => export(&file, &output, no_embed_source),
        Command::Import { file, output } => import(&file, &output),
        Command::Migrate {
            input_dir,
            output_dir,
            profile,
        } => migrate(&input_dir, &output_dir, &profile),
    }
}

fn load(path: &Path) -> Result<vsd_container::VsdFile> {
    read_file(path, &ReadOptions::default()).with_context(|| format!("reading {}", path.display()))
}

fn pack(input: &Path, output: &Path, profile: &str, no_compress: bool) -> Result<()> {
    let profile = Profile::parse(profile)?;
    let base = input.parent().unwrap_or(Path::new("."));
    let text =
        std::fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?;
    let doc = match input.extension().and_then(|e| e.to_str()) {
        Some("md") | Some("markdown") => markdown::document_from_markdown(&text, base, profile)?,
        _ => {
            let json: serde_json::Value =
                serde_json::from_str(&text).context("parsing authoring JSON")?;
            author::document_from_json(&json, base, profile)?
        }
    };

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
        if doc.manifest.render_cache.is_some() {
            "present"
        } else {
            "none (structure-only)"
        }
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
    let hex_str =
        std::fs::read_to_string(path).with_context(|| format!("reading key {}", path.display()))?;
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

fn layout(file: &Path, output: &Path, page_size: &str) -> Result<()> {
    let vsd = load(file)?;
    let opts = match page_size {
        "a4" => vsd_layout::LayoutOptions::default(),
        "letter" => vsd_layout::LayoutOptions::letter(),
        other => bail!("unknown page size {other:?} (use a4 or letter)"),
    };
    let laid = vsd_layout::add_render_cache(&vsd.document, &opts)?;
    let cache = laid.render_cache()?.expect("cache just added");
    // The manifest now commits to the cache → new identity; prior
    // signatures belong to the predecessor and are not carried over.
    write_file(output, &laid, &[], &WriteOptions::default())?;
    if !vsd.signatures.is_empty() {
        eprintln!(
            "note: {} signature(s) on the input signed the pre-layout document; sign the new file",
            vsd.signatures.len()
        );
    }
    println!(
        "laid out {} page(s) with {}/{} → {}",
        cache.pages.len(),
        vsd_layout::ENGINE_NAME,
        vsd_layout::ENGINE_VERSION,
        output.display()
    );
    println!("layout-hash    : {}", hex::encode(cache.layout_hash));
    println!(
        "new document id: {} (predecessor: {})",
        laid.document_id()?,
        vsd.document_id
    );
    Ok(())
}

fn render(file: &Path, page: usize, output: &Path, dpi: f64) -> Result<()> {
    let vsd = load(file)?;
    let cache = vsd
        .document
        .render_cache()?
        .context("document has no render cache (run `vsd layout` first)")?;
    let page_id = cache
        .pages
        .get(page.checked_sub(1).context("pages are 1-based")?)
        .with_context(|| format!("page {page} of {}", cache.pages.len()))?;
    let page_obj = vsd_core::layout::Page::from_value(&vsd.document.store.get_value(page_id)?)?;
    let png = vsd_render::render_page_png(&vsd.document, &page_obj, dpi)?;
    std::fs::write(output, &png)?;
    println!(
        "rendered page {page}/{} at {dpi} dpi → {} ({} bytes)",
        cache.pages.len(),
        output.display(),
        png.len()
    );
    Ok(())
}

fn verify(file: &Path, recompute: bool) -> Result<()> {
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

    if recompute {
        use vsd_layout::RecomputeOutcome;
        match vsd_layout::verify_render_cache(&vsd.document)? {
            RecomputeOutcome::Match { pages } => {
                println!(
                    "recompute   : OK — {pages} page(s) re-laid out, byte-identical to the cache; \
                     pixels and meaning agree"
                );
            }
            RecomputeOutcome::NoCache => {
                println!("recompute   : no render cache present (structure-only document)");
            }
            RecomputeOutcome::UnknownEngine { name, version } => {
                all_ok = false;
                println!(
                    "recompute   : FAILED — cache claims engine {name}/{version}, which this \
                     build cannot reproduce"
                );
            }
            RecomputeOutcome::Mismatch {
                expected_pages,
                cached_pages,
            } => {
                all_ok = false;
                println!(
                    "recompute   : FAILED — the render cache LIES about the content tree \
                     (expected {} page(s), cache has {}). What this document displays is not \
                     what it says.",
                    expected_pages.len(),
                    cached_pages.len()
                );
            }
        }
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
        .map(|p| {
            p.parse::<usize>()
                .context("path must be dot-separated indices")
        })
        .collect::<Result<Vec<_>>>()?;
    let result = vsd_core::redact::redact(&vsd.document, &path, reason)?;

    // Spec §7.2: prior signatures cover the pre-redaction manifest; they
    // belong to the predecessor, not to this revision. Drop them.
    write_file(output, &result.document, &[], &WriteOptions::default())?;
    println!("redacted node at path {path_str} → {}", output.display());
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

fn fill(file: &Path, sets: &[String], output: &Path, strict: bool) -> Result<()> {
    use std::collections::BTreeMap;
    use vsd_core::forms::FieldValue;
    use vsd_core::tree::FieldKind;

    let vsd = load(file)?;
    let fields = vsd.document.fields()?;
    let kinds: BTreeMap<&str, FieldKind> = fields.iter().map(|f| (f.id.as_str(), f.kind)).collect();

    let mut inputs = BTreeMap::new();
    for s in sets {
        let (id, raw) = s
            .split_once('=')
            .with_context(|| format!("--set {s:?}: expected ID=VALUE"))?;
        let kind = kinds
            .get(id)
            .with_context(|| format!("no field with id {id:?}"))?;
        let value = match kind {
            FieldKind::Number => FieldValue::Num(
                raw.parse::<f64>()
                    .with_context(|| format!("field {id:?} is a number, got {raw:?}"))?,
            ),
            FieldKind::Checkbox => FieldValue::Bool(
                raw.parse::<bool>()
                    .with_context(|| format!("field {id:?} is a checkbox, use true/false"))?,
            ),
            _ => FieldValue::Str(raw.to_owned()),
        };
        inputs.insert(id.to_owned(), value);
    }

    let result = vsd_core::fill::fill(&vsd.document, &inputs)?;
    for v in &result.violations {
        eprintln!(
            "[{}] {}: {}",
            if strict { "ERROR" } else { "warn " },
            v.field,
            v.message
        );
    }
    if strict && !result.violations.is_empty() {
        bail!("constraints violated; not writing output");
    }
    write_file(output, &result.document, &[], &WriteOptions::default())?;
    println!(
        "filled {} field(s) → {}\nnew document id: {} (predecessor: {})",
        inputs.len(),
        output.display(),
        result.document.document_id()?,
        vsd.document_id
    );
    Ok(())
}

fn flatten(file: &Path, output: &Path) -> Result<()> {
    let vsd = load(file)?;
    let flat = vsd_core::fill::flatten(&vsd.document)?;
    write_file(output, &flat, &[], &WriteOptions::default())?;
    println!(
        "flattened → {}\nnew document id: {} (predecessor: {})",
        output.display(),
        flat.document_id()?,
        vsd.document_id
    );
    Ok(())
}

fn export(file: &Path, output: &Path, no_embed_source: bool) -> Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let vsd = vsd_container::read_document(&bytes, &ReadOptions::default())?;
    let opts = vsd_pdf::ExportOptions {
        embed_source: !no_embed_source,
    };
    let pdf = vsd_pdf::export_pdf(&vsd.document, Some(&bytes), &opts)?;
    std::fs::write(output, &pdf)?;
    println!(
        "exported {} → {} ({} bytes, tagged PDF{})",
        file.display(),
        output.display(),
        pdf.len(),
        if opts.embed_source {
            ", canonical .vsd embedded — round trip is lossless"
        } else {
            ""
        }
    );
    if !vsd.signatures.is_empty() && opts.embed_source {
        println!(
            "{} signature(s) travel inside the embedded source and survive the round trip",
            vsd.signatures.len()
        );
    }
    Ok(())
}

fn import(file: &Path, output: &Path) -> Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    match vsd_pdf::import_pdf(&bytes, &vsd_pdf::TextRecovery)? {
        vsd_pdf::ImportOutcome::Lossless {
            document,
            signatures,
            document_id,
        } => {
            write_file(output, &document, &signatures, &WriteOptions::default())?;
            println!(
                "hybrid PDF: recovered the canonical VSD losslessly → {}",
                output.display()
            );
            println!(
                "document id: {document_id} (verified), {} signature(s) intact",
                signatures.len()
            );
        }
        vsd_pdf::ImportOutcome::Recovered {
            document,
            pages_read,
        } => {
            let report = vsd_core::validate::validate(&document);
            print_findings(&report);
            write_file(output, &document, &[], &WriteOptions::default())?;
            println!(
                "foreign PDF: heuristic structure recovery over {pages_read} page(s) → {}",
                output.display()
            );
            println!(
                "marked format-migrated (lossy) in provenance; original PDF embedded as attachment"
            );
            println!("document id: {}", document.document_id()?);
        }
    }
    Ok(())
}

fn migrate(input_dir: &Path, output_dir: &Path, profile: &str) -> Result<()> {
    use std::collections::BTreeMap;

    let profile = Profile::parse(profile)?;
    let mut inputs = Vec::new();
    collect_migratable(input_dir, &mut inputs)?;
    if inputs.is_empty() {
        bail!("no .json/.md/.pdf files under {}", input_dir.display());
    }
    std::fs::create_dir_all(output_dir)?;

    let mut converted = 0usize;
    let mut failures = 0usize;
    let mut sum_objects = 0usize;
    let mut sum_bytes = 0u64;
    let mut unique: BTreeMap<vsd_core::ObjectId, u64> = BTreeMap::new();

    for path in &inputs {
        let rel = path.strip_prefix(input_dir).unwrap_or(path);
        let out = output_dir.join(rel).with_extension("vsd");
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let result = (|| -> Result<vsd_core::Document> {
            match path.extension().and_then(|e| e.to_str()) {
                Some("md") | Some("markdown") => {
                    let text = std::fs::read_to_string(path)?;
                    markdown::document_from_markdown(
                        &text,
                        path.parent().unwrap_or(Path::new(".")),
                        profile,
                    )
                }
                Some("json") => {
                    let json: serde_json::Value =
                        serde_json::from_str(&std::fs::read_to_string(path)?)?;
                    author::document_from_json(
                        &json,
                        path.parent().unwrap_or(Path::new(".")),
                        profile,
                    )
                }
                Some("pdf") => {
                    let bytes = std::fs::read(path)?;
                    match vsd_pdf::import_pdf(&bytes, &vsd_pdf::TextRecovery)? {
                        vsd_pdf::ImportOutcome::Lossless { document, .. } => Ok(document),
                        vsd_pdf::ImportOutcome::Recovered { document, .. } => Ok(document),
                    }
                }
                _ => unreachable!("filtered by collect_migratable"),
            }
        })();
        match result {
            Ok(doc) => {
                write_file(&out, &doc, &[], &WriteOptions::default())?;
                sum_objects += doc.store.len();
                sum_bytes += doc.store.total_bytes();
                for (id, bytes) in doc.store.iter() {
                    unique.insert(*id, bytes.len() as u64);
                }
                converted += 1;
                println!("  {} → {}", rel.display(), out.display());
            }
            Err(e) => {
                failures += 1;
                eprintln!("  {} FAILED: {e:#}", rel.display());
            }
        }
    }

    // The dedup report: the content-addressing payoff, made visible.
    let unique_bytes: u64 = unique.values().sum();
    println!("\nmigrated {converted} document(s), {failures} failure(s)");
    println!(
        "objects: {} total across documents, {} unique ({:.1}% shared)",
        sum_objects,
        unique.len(),
        if sum_objects > 0 {
            100.0 * (sum_objects - unique.len()) as f64 / sum_objects as f64
        } else {
            0.0
        }
    );
    println!(
        "canonical bytes: {} summed, {} deduplicated — a shared object store would save {:.1}%",
        sum_bytes,
        unique_bytes,
        if sum_bytes > 0 {
            100.0 * (sum_bytes - unique_bytes) as f64 / sum_bytes as f64
        } else {
            0.0
        }
    );
    Ok(())
}

fn collect_migratable(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_migratable(&path, out)?;
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("json") | Some("md") | Some("markdown") | Some("pdf")
        ) {
            out.push(path);
        }
    }
    out.sort();
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
        C::Float(x) => serde_json::Number::from_f64(*x)
            .map(J::Number)
            .unwrap_or(J::Null),
    }
}
