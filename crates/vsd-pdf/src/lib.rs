//! # vsd-pdf
//!
//! PDF interop for VSD — the adoption wedge (spec §11). A format
//! without a migration story is a hobby; this crate is the migration
//! story:
//!
//! - **Export** ([`export_pdf`]): VSD display lists are a strict subset
//!   of PDF's imaging model, so export is mechanical and visually
//!   lossless. The output is *tagged* (real H1–H6/P/Code/Caption
//!   structure rebuilt from content-tree back-references, figure alt
//!   text included) and — by default — **hybrid**: the canonical `.vsd`
//!   travels inside the PDF as an attachment, so the round trip back to
//!   VSD is the identity function, verifiable by document id.
//! - **Import** ([`import_pdf`]): hybrid PDFs recover losslessly;
//!   foreign PDFs go through pluggable [`StructureRecovery`] (the
//!   built-in recoverer is deliberately naive text extraction), marked
//!   `format-migrated { lossy: true }` in provenance with the original
//!   PDF embedded for legal continuity.

#![forbid(unsafe_code)]

mod export;
mod import;
mod subset;
mod write;

use thiserror::Error;

pub use export::{export_pdf, ExportOptions};
pub use import::{import_pdf, ImportOutcome, StructureRecovery, TextRecovery};

pub type Result<T> = std::result::Result<T, PdfError>;

#[derive(Debug, Error)]
pub enum PdfError {
    #[error("layout: {0}")]
    Layout(String),

    #[error("pdf parse: {0}")]
    Parse(String),

    #[error("embedded source.vsd is invalid: {0}")]
    EmbeddedSource(String),

    #[error(transparent)]
    Core(#[from] vsd_core::Error),
}
