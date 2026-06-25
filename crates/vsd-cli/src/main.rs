//! `vsd` — the VSD reference command-line tool.

#![forbid(unsafe_code)]

mod author;
mod export_pandoc;
mod export_typst;
mod html;
mod htmldiff;
mod markdown;
mod pandoc;
mod serve;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use vsd_container::{read_file, write_file, ReadOptions, SigScope, WriteOptions};
use vsd_core::manifest::Profile;
use vsd_core::validate::Severity;
use vsd_core::Node;

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
        /// Input document: .json (authoring dialect), .md (CommonMark +
        /// tables), or .html. Format is inferred from the extension.
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
    /// Convert a Pandoc JSON AST (`pandoc -t json`) to a .vsd, unlocking
    /// every format Pandoc reads (docx, rst, LaTeX, Org, EPUB, …). Reads
    /// the AST from a file, or from stdin when no input is given.
    PackPandoc {
        /// Pandoc JSON AST file; omit (or `-`) to read stdin.
        input: Option<PathBuf>,
        /// Output .vsd path.
        #[arg(short, long)]
        output: PathBuf,
        /// Conformance profile: core, archive, form, stream.
        #[arg(long, default_value = "core")]
        profile: String,
        /// Disable zstd compression of the object store.
        #[arg(long)]
        no_compress: bool,
        /// Base directory for resolving relative image paths
        /// (defaults to the input file's directory, or the cwd for stdin).
        #[arg(long)]
        resource_dir: Option<PathBuf>,
    },
    /// Write a VSD out as a Pandoc JSON AST (the reverse of pack-pandoc),
    /// so `vsd export-pandoc x.vsd | pandoc -f json -o x.docx` reaches
    /// every format Pandoc writes. Prints to stdout unless `-o` is given.
    ExportPandoc {
        file: PathBuf,
        /// Output path for the JSON AST; omit to write stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Directory to write referenced images into (defaults to the
        /// output's directory, or the cwd for stdout).
        #[arg(long)]
        asset_dir: Option<PathBuf>,
    },
    /// Write a VSD out as Typst source (`.typ`) for the Typst typesetting
    /// ecosystem. Referenced images are written alongside the output.
    ExportTypst {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
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
    /// Generate a signing keypair.
    Keygen {
        /// Output path for the secret key (public key gets .pub appended).
        #[arg(short, long)]
        output: PathBuf,
        /// ed25519, or hybrid (Ed25519 + ML-DSA-65 post-quantum: both
        /// components must verify — the posture for documents that must
        /// outlive the quantum transition).
        #[arg(long, default_value = "ed25519")]
        algorithm: String,
    },
    /// Sign a document (whole-document scope) and write a new file.
    Sign {
        file: PathBuf,
        /// Secret key file from `vsd keygen` (ed25519 or hybrid,
        /// detected automatically).
        #[arg(short, long)]
        key: PathBuf,
        /// Output path (defaults to overwriting the input).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// X.509 certificate (PEM or DER) to attach; it must certify
        /// the signing key (binding is checked on verify).
        #[arg(long)]
        cert: Option<PathBuf>,
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
    /// Lay the document out with the reference engine, attaching a
    /// verifiable render cache and page index (successor document).
    Layout {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Page size: a4 | letter.
        #[arg(long, default_value = "a4")]
        page_size: String,
        /// Engine contract version: 1.1 (bold/italic faces) or 1.0.
        /// Old caches stay verifiable forever either way.
        #[arg(long, default_value = "1.1.0")]
        engine: String,
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
    Diff {
        old: PathBuf,
        new: PathBuf,
        /// Write a self-contained HTML redline view to this path.
        #[arg(long)]
        html: Option<PathBuf>,
    },
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
        /// Prompt for each field on the terminal with live constraint
        /// feedback (re-prompts on violation; empty input skips).
        #[arg(short, long)]
        interactive: bool,
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
        /// Emit an archival PDF/A file (PDF/A-3b with the embedded
        /// source, PDF/A-2b without): XMP + sRGB OutputIntent, /ID,
        /// subset-tagged fonts with /CIDSet.
        #[arg(long)]
        pdfa: bool,
    },
    /// Import a PDF: lossless if it is a hybrid PDF carrying its VSD
    /// source; otherwise heuristic structure recovery (marked lossy in
    /// provenance, original PDF embedded as an attachment).
    Import {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Batch-convert a directory of .json/.md/.html/.pdf files to .vsd
    /// and report cross-document object deduplication.
    Migrate {
        input_dir: PathBuf,
        #[arg(short, long)]
        output_dir: PathBuf,
        #[arg(long, default_value = "core")]
        profile: String,
    },
    /// Merkle-ize the document: hoist top-level blocks into subtree
    /// objects so individual blocks can be selectively disclosed.
    Seal {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Wrap each block with random salt so hidden siblings cannot
        /// be confirmed by hashing a guess of their content.
        #[arg(long)]
        salted: bool,
    },
    /// Produce a selective-disclosure bundle for one top-level block:
    /// proves the block belongs to the document id while siblings stay
    /// hidden (as hashes). Requires a sealed document.
    Disclose {
        file: PathBuf,
        /// Top-level block index (see `vsd extract`).
        #[arg(long)]
        index: u64,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Verify a disclosure bundle and print the disclosed content.
    VerifyDisclosure {
        bundle: PathBuf,
        /// Document id (hex) the bundle must prove membership of.
        #[arg(long)]
        expect: Option<String>,
    },
    /// Serve a directory of .vsd files as a content-addressed object
    /// store (docs/OBJECT-STORE-HTTP.md). The server is untrusted by
    /// design — clients verify every object by hash.
    Serve {
        /// Directory to index (recursively) for .vsd files.
        root: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8077")]
        addr: String,
    },
    /// Transparency log operations (RFC 6962-style, see vsd-tlog).
    #[command(subcommand)]
    Tlog(TlogCommand),
    /// Show or extend the provenance chain (spec §8).
    #[command(subcommand)]
    Provenance(ProvenanceCommand),
}

#[derive(Subcommand)]
enum TlogCommand {
    /// Append a document's id to the log (created if missing).
    Append {
        /// Log file path.
        #[arg(long)]
        log: PathBuf,
        file: PathBuf,
    },
    /// Print the tree head; sign it when a key is supplied.
    Head {
        #[arg(long)]
        log: PathBuf,
        /// Ed25519 key file (from `vsd keygen`) to sign the head.
        #[arg(long)]
        key: Option<PathBuf>,
    },
    /// Prove (and verify) a document's inclusion in the log.
    Prove {
        #[arg(long)]
        log: PathBuf,
        file: PathBuf,
    },
}

#[derive(Subcommand)]
enum ProvenanceCommand {
    /// List the assertions in the provenance chain.
    Show { file: PathBuf },
    /// Append an assertion (e.g. --kind ai-generated --claim
    /// model=claude-fable-5), producing a successor document.
    Add {
        file: PathBuf,
        #[arg(long)]
        kind: String,
        /// KEY=VALUE claims; repeatable.
        #[arg(long = "claim", value_name = "KEY=VALUE")]
        claims: Vec<String>,
        #[arg(short, long)]
        output: PathBuf,
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
        Command::PackPandoc {
            input,
            output,
            profile,
            no_compress,
            resource_dir,
        } => pack_pandoc(
            input.as_deref(),
            &output,
            &profile,
            no_compress,
            resource_dir.as_deref(),
        ),
        Command::ExportPandoc {
            file,
            output,
            asset_dir,
        } => export_pandoc_cmd(&file, output.as_deref(), asset_dir.as_deref()),
        Command::ExportTypst { file, output } => export_typst_cmd(&file, &output),
        Command::Info { file } => info(&file),
        Command::Validate { file } => validate(&file),
        Command::Extract { file, format } => extract(&file, &format),
        Command::Objects { file } => objects(&file),
        Command::Keygen { output, algorithm } => keygen(&output, &algorithm),
        Command::Sign {
            file,
            key,
            output,
            cert,
        } => sign(&file, &key, output.as_deref(), cert.as_deref()),
        Command::Verify { file, recompute } => verify(&file, recompute),
        Command::Layout {
            file,
            output,
            page_size,
            engine,
        } => layout(&file, &output, &page_size, &engine),
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
        Command::Diff { old, new, html } => diff(&old, &new, html.as_deref()),
        Command::Fill {
            file,
            sets,
            output,
            strict,
            interactive,
        } => fill(&file, &sets, &output, strict, interactive),
        Command::Flatten { file, output } => flatten(&file, &output),
        Command::Export {
            file,
            output,
            no_embed_source,
            pdfa,
        } => export(&file, &output, no_embed_source, pdfa),
        Command::Import { file, output } => import(&file, &output),
        Command::Migrate {
            input_dir,
            output_dir,
            profile,
        } => migrate(&input_dir, &output_dir, &profile),
        Command::Seal {
            file,
            output,
            salted,
        } => seal(&file, &output, salted),
        Command::Disclose {
            file,
            index,
            output,
        } => disclose_cmd(&file, index, &output),
        Command::VerifyDisclosure { bundle, expect } => {
            verify_disclosure_cmd(&bundle, expect.as_deref())
        }
        Command::Serve { root, addr } => serve::run(&root, &addr),
        Command::Tlog(cmd) => tlog(cmd),
        Command::Provenance(cmd) => provenance(cmd),
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
        Some("html") | Some("htm") => html::document_from_html(&text, base, profile)?,
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

fn pack_pandoc(
    input: Option<&Path>,
    output: &Path,
    profile: &str,
    no_compress: bool,
    resource_dir: Option<&Path>,
) -> Result<()> {
    use std::io::Read as _;
    let profile = Profile::parse(profile)?;
    let stdin = input.is_none() || input == Some(Path::new("-"));
    let text = if stdin {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("reading Pandoc JSON from stdin")?;
        s
    } else {
        let p = input.unwrap();
        std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?
    };
    // Resolve relative image paths against --resource-dir, else the input
    // file's directory, else the cwd.
    let base = resource_dir
        .or_else(|| input.filter(|_| !stdin).and_then(|p| p.parent()))
        .unwrap_or(Path::new("."));
    let doc = pandoc::document_from_pandoc(&text, base, profile)?;

    let report = vsd_core::validate::validate(&doc);
    print_findings(&report);
    if !report.is_valid() {
        bail!("document failed validation; not writing output");
    }
    let opts = WriteOptions {
        compress: !no_compress,
    };
    write_file(output, &doc, &[], &opts)?;
    println!(
        "wrote {} ({} objects) from Pandoc AST\ndocument id: {}",
        output.display(),
        doc.store.len(),
        doc.document_id()?
    );
    Ok(())
}

/// Write the binary assets a writer produced (image files) next to the
/// output, into `dir`.
fn write_assets(dir: &Path, assets: &[(String, Vec<u8>)]) -> Result<()> {
    for (name, bytes) in assets {
        std::fs::write(dir.join(name), bytes).with_context(|| format!("writing asset {name}"))?;
    }
    Ok(())
}

fn export_pandoc_cmd(file: &Path, output: Option<&Path>, asset_dir: Option<&Path>) -> Result<()> {
    let vsd = load(file)?;
    let out = export_pandoc::document_to_pandoc(&vsd.document)?;
    let asset_base = asset_dir
        .or_else(|| output.and_then(|p| p.parent()))
        .unwrap_or(Path::new("."));
    write_assets(asset_base, &out.assets)?;
    match output {
        Some(p) => {
            std::fs::write(p, &out.content)?;
            eprintln!(
                "wrote Pandoc JSON AST → {} ({} image asset(s)); convert with `pandoc -f json …`",
                p.display(),
                out.assets.len()
            );
        }
        None => print!("{}", out.content),
    }
    Ok(())
}

fn export_typst_cmd(file: &Path, output: &Path) -> Result<()> {
    let vsd = load(file)?;
    let out = export_typst::document_to_typst(&vsd.document)?;
    std::fs::write(output, &out.content)?;
    let dir = output.parent().unwrap_or(Path::new("."));
    write_assets(dir, &out.assets)?;
    println!(
        "wrote Typst source → {} ({} image asset(s)); typeset with `typst compile`",
        output.display(),
        out.assets.len()
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

fn pub_path_for(output: &Path) -> PathBuf {
    output.with_extension(format!(
        "{}pub",
        output
            .extension()
            .map(|e| format!("{}.", e.to_string_lossy()))
            .unwrap_or_default()
    ))
}

fn keygen(output: &Path, algorithm: &str) -> Result<()> {
    let pub_path = pub_path_for(output);
    match algorithm {
        "ed25519" => {
            let key = vsd_sign::SigningKey::generate();
            std::fs::write(output, hex::encode(key.seed()))?;
            std::fs::write(&pub_path, hex::encode(key.verifying_key().to_bytes()))?;
            println!("ed25519 keypair");
        }
        "hybrid" => {
            let key = vsd_sign::HybridSigningKey::generate()?;
            std::fs::write(output, hex::encode(key.to_bytes()))?;
            std::fs::write(&pub_path, hex::encode(key.public_key_bytes()))?;
            println!(
                "hybrid Ed25519 + ML-DSA-65 keypair — both components must verify; \
                 signatures are ~{} KB",
                vsd_sign::hybrid::SIG_LEN / 1024 + 1
            );
        }
        other => bail!("unknown algorithm {other:?} (use ed25519 or hybrid)"),
    }
    println!(
        "secret key: {}\npublic key: {}",
        output.display(),
        pub_path.display()
    );
    println!("keep the secret key offline; only the .pub needs distribution");
    Ok(())
}

/// A signing key of either supported algorithm, detected by length.
enum AnyKey {
    Ed25519(Box<vsd_sign::SigningKey>),
    Hybrid(Box<vsd_sign::HybridSigningKey>),
}

fn read_key(path: &Path) -> Result<AnyKey> {
    let hex_str =
        std::fs::read_to_string(path).with_context(|| format!("reading key {}", path.display()))?;
    let bytes = hex::decode(hex_str.trim()).context("key file must be hex")?;
    match bytes.len() {
        32 => Ok(AnyKey::Ed25519(Box::new(vsd_sign::SigningKey::from_seed(
            &bytes,
        )?))),
        n if n == 32 + vsd_sign::hybrid::ML_SK_LEN + vsd_sign::hybrid::ML_PK_LEN => Ok(
            AnyKey::Hybrid(Box::new(vsd_sign::HybridSigningKey::from_bytes(&bytes)?)),
        ),
        n => bail!("unrecognized key length {n} bytes"),
    }
}

fn sign(file: &Path, key: &Path, output: Option<&Path>, cert: Option<&Path>) -> Result<()> {
    let vsd = load(file)?;
    let mut sig = match read_key(key)? {
        AnyKey::Ed25519(k) => k.sign_document(&vsd.document)?,
        AnyKey::Hybrid(k) => k.sign_document(&vsd.document)?,
    };
    if let Some(cert_path) = cert {
        sig.cert = Some(
            std::fs::read(cert_path)
                .with_context(|| format!("reading certificate {}", cert_path.display()))?,
        );
        // Fail fast on a cert that does not certify this key.
        let binding = vsd_sign::check_cert_binding(&sig, None)?;
        println!(
            "certificate bound: {} (issuer {}, valid {}..{})",
            binding.subject, binding.issuer, binding.not_before, binding.not_after
        );
    }
    let alg = sig.alg;
    let mut sigs = vsd.signatures.clone();
    sigs.push(sig);
    let out = output.unwrap_or(file);
    write_file(out, &vsd.document, &sigs, &WriteOptions::default())?;
    println!(
        "signed {} with {} (document id {})\nsignatures now: {}",
        out.display(),
        alg.as_str(),
        vsd.document_id,
        sigs.len()
    );
    Ok(())
}

fn layout(file: &Path, output: &Path, page_size: &str, engine: &str) -> Result<()> {
    let vsd = load(file)?;
    let mut opts = match page_size {
        "a4" => vsd_layout::LayoutOptions::default(),
        "letter" => vsd_layout::LayoutOptions::letter(),
        other => bail!("unknown page size {other:?} (use a4 or letter)"),
    };
    let version = vsd_layout::EngineVersion::parse(engine)
        .or_else(|| vsd_layout::EngineVersion::parse(&format!("{engine}.0")))
        .with_context(|| format!("unknown engine version {engine:?} (use 1.0 or 1.1)"))?;
    opts = opts.with_engine(version);
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
        version.as_str(),
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
            "signature {i} : [{} {}] key {}… → {desc}",
            match sig.scope {
                SigScope::Document => "document",
                SigScope::Subtree => "subtree",
                SigScope::FieldLayer => "field-layer",
            },
            sig.alg.as_str(),
            &hex::encode(&sig.pubkey)[..16]
        );
        if sig.cert.is_some() {
            match vsd_sign::check_cert_binding(sig, None) {
                Ok(b) => println!(
                    "              cert: {} (issuer {}{}, valid {}..{})",
                    b.subject,
                    b.issuer,
                    if b.self_signed { ", self-signed" } else { "" },
                    b.not_before,
                    b.not_after
                ),
                Err(e) => {
                    all_ok = false;
                    println!("              cert: INVALID — {e}");
                }
            }
        }
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

fn diff(old: &Path, new: &Path, html: Option<&Path>) -> Result<()> {
    let a = load(old)?;
    let b = load(new)?;
    let d = vsd_core::diff::diff(&a.document, &b.document)?;
    if let Some(html_path) = html {
        let page = htmldiff::render_diff_html(&a.document, &b.document)?;
        std::fs::write(html_path, &page)?;
        println!("redline view → {}", html_path.display());
    }
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

fn fill(
    file: &Path,
    sets: &[String],
    output: &Path,
    strict: bool,
    interactive: bool,
) -> Result<()> {
    use std::collections::BTreeMap;
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
        let value = parse_field_value(*kind, raw)
            .with_context(|| format!("field {id:?} ({})", kind.as_str()))?;
        inputs.insert(id.to_owned(), value);
    }

    if interactive {
        let stdin = std::io::stdin();
        interactive_fill(
            &vsd.document,
            &fields,
            &mut inputs,
            &mut stdin.lock(),
            &mut std::io::stderr(),
        )?;
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

fn parse_field_value(
    kind: vsd_core::tree::FieldKind,
    raw: &str,
) -> Result<vsd_core::forms::FieldValue> {
    use vsd_core::forms::FieldValue;
    use vsd_core::tree::FieldKind;
    Ok(match kind {
        FieldKind::Number => FieldValue::Num(
            raw.parse::<f64>()
                .with_context(|| format!("expected a number, got {raw:?}"))?,
        ),
        FieldKind::Checkbox => FieldValue::Bool(
            raw.parse::<bool>()
                .with_context(|| format!("expected true/false, got {raw:?}"))?,
        ),
        _ => FieldValue::Str(raw.to_owned()),
    })
}

/// Terminal form-filling with live constraint evaluation (ROADMAP 4e):
/// each field is prompted, parsed by kind, and checked against its
/// constraint immediately — violations re-prompt with the reason.
/// Empty input keeps the current value. Generic over reader/writer so
/// the loop is testable without a terminal.
fn interactive_fill(
    doc: &vsd_core::Document,
    fields: &[vsd_core::tree::Field],
    inputs: &mut std::collections::BTreeMap<String, vsd_core::forms::FieldValue>,
    reader: &mut impl std::io::BufRead,
    out: &mut impl std::io::Write,
) -> Result<()> {
    use vsd_core::forms::{check_constraints, evaluate_computed, FieldValue};

    // Existing layer values pre-populate the session.
    if let Some(id) = doc.manifest.field_layer {
        let layer = vsd_core::forms::FilledLayer::from_value(&doc.store.get_value(&id)?)?;
        for (k, v) in layer.env() {
            inputs.entry(k).or_insert(v);
        }
    }

    writeln!(
        out,
        "interactive fill — empty input keeps the current value"
    )?;
    for field in fields {
        if field.computed.is_some() {
            continue; // computed fields derive; they are not asked
        }
        loop {
            let current = inputs
                .get(&field.id)
                .map(FieldValue::to_text)
                .unwrap_or_default();
            write!(
                out,
                "{} [{}]{}{}: ",
                field.label.as_deref().unwrap_or(&field.id),
                field.kind.as_str(),
                if field.required { " (required)" } else { "" },
                if current.is_empty() {
                    String::new()
                } else {
                    format!(" (current: {current})")
                },
            )?;
            out.flush()?;
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                writeln!(out)?;
                return Ok(()); // EOF ends the session, keeping entries so far
            }
            let line = line.trim();
            if line.is_empty() {
                break; // keep current value (possibly empty)
            }
            let value = match parse_field_value(field.kind, line) {
                Ok(v) => v,
                Err(e) => {
                    writeln!(out, "  ✗ {e:#}")?;
                    continue;
                }
            };
            // Live constraint check over the *whole* environment, so
            // cross-field constraints react immediately.
            let mut trial = inputs.clone();
            trial.insert(field.id.clone(), value.clone());
            let env = evaluate_computed(fields, &trial)?;
            let violation = check_constraints(fields, &env)
                .into_iter()
                .find(|v| v.field == field.id);
            match violation {
                Some(v) => {
                    writeln!(out, "  ✗ {}", v.message)?;
                    continue;
                }
                None => {
                    inputs.insert(field.id.clone(), value);
                    break;
                }
            }
        }
    }
    // Show derived results so the user sees what they signed up for.
    let env = evaluate_computed(fields, inputs)?;
    for field in fields.iter().filter(|f| f.computed.is_some()) {
        if let Some(v) = env.get(&field.id) {
            writeln!(
                out,
                "{} (computed): {}",
                field.label.as_deref().unwrap_or(&field.id),
                v.to_text()
            )?;
        }
    }
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

fn export(file: &Path, output: &Path, no_embed_source: bool, pdfa: bool) -> Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let vsd = vsd_container::read_document(&bytes, &ReadOptions::default())?;
    let opts = vsd_pdf::ExportOptions {
        embed_source: !no_embed_source,
        pdfa,
    };
    let pdf = vsd_pdf::export_pdf(&vsd.document, Some(&bytes), &opts)?;
    std::fs::write(output, &pdf)?;
    let kind = if pdfa {
        if opts.embed_source {
            "PDF/A-3b"
        } else {
            "PDF/A-2b"
        }
    } else {
        "tagged PDF"
    };
    println!(
        "exported {} → {} ({} bytes, {}{})",
        file.display(),
        output.display(),
        pdf.len(),
        kind,
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
            via,
        } => {
            let report = vsd_core::validate::validate(&document);
            print_findings(&report);
            write_file(output, &document, &[], &WriteOptions::default())?;
            let how = if via.contains("tagged") {
                "tagged-structure recovery (StructTreeRoot → content tree)"
            } else {
                "heuristic text recovery"
            };
            println!(
                "foreign PDF: {how} over {pages_read} page(s) → {}",
                output.display()
            );
            println!(
                "marked format-migrated (lossy, tool={via}) in provenance; \
                 original PDF embedded as attachment"
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
                Some("html") | Some("htm") => {
                    let text = std::fs::read_to_string(path)?;
                    html::document_from_html(
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
            Some("json") | Some("md") | Some("markdown") | Some("html") | Some("htm") | Some("pdf")
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

fn seal(file: &Path, output: &Path, salted: bool) -> Result<()> {
    let vsd = load(file)?;
    let sealed = if salted {
        let mut fresh_salt = || {
            let mut salt = [0u8; 16];
            getrandom::getrandom(&mut salt).expect("OS entropy");
            salt
        };
        vsd_core::disclose::seal_salted(&vsd.document, &mut fresh_salt)?
    } else {
        vsd_core::disclose::seal(&vsd.document)?
    };
    write_file(output, &sealed, &[], &WriteOptions::default())?;
    let Node::Doc(d) = sealed.root_node()? else {
        unreachable!()
    };
    println!(
        "sealed {} top-level block(s) into subtree objects{} → {}",
        d.children.len(),
        if salted {
            " (salted: hidden siblings cannot be guess-confirmed)"
        } else {
            " (UNSALTED: guessable siblings can be confirmed by hash)"
        },
        output.display()
    );
    println!(
        "new document id: {} (predecessor: {})\nsign the sealed file; disclosures verify against its id",
        sealed.document_id()?,
        vsd.document_id
    );
    Ok(())
}

fn disclose_cmd(file: &Path, index: u64, output: &Path) -> Result<()> {
    let vsd = load(file)?;
    let bundle = vsd_core::disclose::disclose(&vsd.document, index)?;
    std::fs::write(output, bundle.encode()?)?;
    println!(
        "disclosure of block {index} of document {} → {}",
        bundle.doc_id,
        output.display()
    );
    println!("siblings travel as hashes only; verify with `vsd verify-disclosure`");
    println!(
        "note: hashes are unsalted in this version — do not use where confirming a \
         guessed sibling is itself a leak"
    );
    Ok(())
}

fn verify_disclosure_cmd(bundle_path: &Path, expect: Option<&str>) -> Result<()> {
    let bytes = std::fs::read(bundle_path)?;
    let bundle = vsd_core::disclose::Disclosure::decode(&bytes)?;
    let expect_id = expect
        .map(|s| s.parse::<vsd_core::ObjectId>())
        .transpose()?;
    let verified = vsd_core::disclose::verify_disclosure(&bundle, expect_id)?;
    println!(
        "PROVEN: block {} belongs to document {}",
        verified.index, verified.doc_id
    );
    println!(
        "hidden siblings: {} ({})",
        verified.hidden_siblings,
        if verified.salted {
            "salted — guesses cannot be confirmed"
        } else {
            "unsalted — guessable content can be confirmed"
        }
    );
    println!("--- disclosed content ---");
    println!("{}", htmldiff::node_plain_text(&verified.subtree));
    Ok(())
}

fn tlog(cmd: TlogCommand) -> Result<()> {
    match cmd {
        TlogCommand::Append { log, file } => {
            let vsd = load(&file)?;
            let mut l = if log.exists() {
                vsd_tlog::Log::load(&log)?
            } else {
                vsd_tlog::Log::new()
            };
            let index = l.append(vsd.document_id.0);
            l.save(&log)?;
            println!(
                "appended {} at index {index}\nlog size {} · tree head {}",
                vsd.document_id,
                l.size(),
                hex::encode(l.root())
            );
        }
        TlogCommand::Head { log, key } => {
            let l = vsd_tlog::Log::load(&log)?;
            println!("size      : {}", l.size());
            println!("tree head : {}", hex::encode(l.root()));
            if let Some(key_path) = key {
                let AnyKey::Ed25519(k) = read_key(&key_path)? else {
                    bail!("tree heads are signed with ed25519 keys");
                };
                let sth = vsd_tlog::SignedTreeHead::sign_with_seed(&l, &k.seed());
                let sth_path = log.with_extension("sth");
                std::fs::write(&sth_path, sth.to_bytes())?;
                println!("signed head → {}", sth_path.display());
            }
        }
        TlogCommand::Prove { log, file } => {
            let vsd = load(&file)?;
            let l = vsd_tlog::Log::load(&log)?;
            let index = l
                .entries()
                .iter()
                .position(|e| *e == vsd.document_id.0)
                .with_context(|| format!("{} is not in the log", vsd.document_id))?
                as u64;
            let n = l.size();
            let root = l.root();
            let proof = l.inclusion_proof(index, n)?;
            vsd_tlog::verify_inclusion(&vsd.document_id.0, index, n, &proof, &root)?;
            println!(
                "INCLUSION PROVEN: {} is entry {index} of {} (tree head {})",
                vsd.document_id,
                n,
                hex::encode(root)
            );
            for (i, h) in proof.iter().enumerate() {
                println!("  path[{i}] {}", hex::encode(h));
            }
        }
    }
    Ok(())
}

fn provenance(cmd: ProvenanceCommand) -> Result<()> {
    match cmd {
        ProvenanceCommand::Show { file } => {
            let vsd = load(&file)?;
            match vsd.document.provenance()? {
                None => println!("no provenance chain"),
                Some(p) => {
                    for (i, a) in p.assertions.iter().enumerate() {
                        println!("assertion {i}: {} (anchors {})", a.kind, a.manifest_hash);
                        for (k, v) in &a.claims {
                            println!("    {k} = {v}");
                        }
                    }
                }
            }
        }
        ProvenanceCommand::Add {
            file,
            kind,
            claims,
            output,
        } => {
            let vsd = load(&file)?;
            let claims = claims
                .iter()
                .map(|c| {
                    c.split_once('=')
                        .map(|(k, v)| (k.to_owned(), v.to_owned()))
                        .with_context(|| format!("--claim {c:?}: expected KEY=VALUE"))
                })
                .collect::<Result<Vec<_>>>()?;

            let mut prov = vsd.document.provenance()?.unwrap_or_default();
            prov.assertions.push(vsd_core::manifest::Assertion {
                kind: kind.clone(),
                claims,
                // Each assertion anchors the manifest hash at this point
                // in history (spec §8).
                manifest_hash: vsd.document_id,
            });
            let mut store = vsd.document.store.clone();
            let prov_id = store.put_value(&prov.to_value())?;
            let manifest = vsd_core::Manifest {
                provenance: Some(prov_id),
                predecessor: Some(vsd.document_id),
                ..vsd.document.manifest.clone()
            };
            let mut doc = vsd_core::Document { manifest, store };
            let keep = doc.closure()?;
            doc.store.retain_only(&keep);
            write_file(&output, &doc, &[], &WriteOptions::default())?;
            println!(
                "appended {kind:?} assertion → {}\nnew document id: {} (predecessor: {})",
                output.display(),
                doc.document_id()?,
                vsd.document_id
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use vsd_core::compose::Compose;
    use vsd_core::forms::{ArithOp, CmpOp, Expr};
    use vsd_core::tree::{Field, FieldKind, Node};

    fn form_doc() -> vsd_core::Document {
        Compose::new("en")
            .h1("Order")
            .node(Node::Field(Field {
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
            }))
            .node(Node::Field(Field {
                id: "total".into(),
                kind: FieldKind::Number,
                label: Some("Total".into()),
                required: false,
                constraint: None,
                computed: Some(Expr::Arith(
                    ArithOp::Mul,
                    vec![Expr::FieldRef("qty".into()), Expr::Num(9.5)],
                )),
            }))
            .profile(Profile::Form)
            .finish()
            .unwrap()
    }

    #[test]
    fn interactive_fill_reprompts_on_violation_and_shows_computed() {
        let doc = form_doc();
        let fields = doc.fields().unwrap();
        let mut inputs = std::collections::BTreeMap::new();
        // First answer violates qty >= 1 (re-prompt), second passes.
        let mut reader = Cursor::new(b"0\n4\n".to_vec());
        let mut out = Vec::new();
        interactive_fill(&doc, &fields, &mut inputs, &mut reader, &mut out).unwrap();

        let transcript = String::from_utf8(out).unwrap();
        assert!(transcript.contains("constraint violated"), "{transcript}");
        assert!(transcript.contains("Total (computed): 38"), "{transcript}");
        assert_eq!(
            inputs.get("qty"),
            Some(&vsd_core::forms::FieldValue::Num(4.0))
        );

        // The session result fills cleanly.
        let r = vsd_core::fill::fill(&doc, &inputs).unwrap();
        assert!(r.violations.is_empty());
    }

    #[test]
    fn interactive_fill_handles_eof_and_bad_numbers() {
        let doc = form_doc();
        let fields = doc.fields().unwrap();
        let mut inputs = std::collections::BTreeMap::new();
        // Non-numeric answer re-prompts; EOF ends the session gracefully.
        let mut reader = Cursor::new(b"abc\n".to_vec());
        let mut out = Vec::new();
        interactive_fill(&doc, &fields, &mut inputs, &mut reader, &mut out).unwrap();
        let transcript = String::from_utf8(out).unwrap();
        assert!(transcript.contains("expected a number"), "{transcript}");
        assert!(inputs.is_empty());
    }
}
