//! # vsd-layout
//!
//! The reference layout engine for VSD — **`vsd-layout/1.0`** — a
//! deterministic projection from content tree to display lists (spec
//! §5), governed by the normative contract in `docs/LAYOUT-1.0.md`.
//!
//! Two properties matter:
//!
//! 1. **Determinism**: all layout arithmetic is integer micrometers;
//!    the same document and engine version produce byte-identical page
//!    objects on every platform. The public conformance vectors carry
//!    golden layout hashes proving it across OSes in CI.
//! 2. **Verifiability**: [`verify_render_cache`] re-runs the engine and
//!    compares page object ids. A render cache that shows anything
//!    other than the content tree's text cannot survive recomputation —
//!    the "visible pixels ≠ extracted text" attack class dies here.

#![forbid(unsafe_code)]

mod engine;
pub mod font;
mod text;

use thiserror::Error;

use vsd_core::document::Document;
use vsd_core::manifest::{Manifest, PageIndex, RenderCache};
use vsd_core::ObjectId;

pub use engine::{
    layout_document, layout_document_with_session, EngineVersion, LayoutOptions, LayoutSession,
};

/// Engine identity — pinned in every render cache it produces.
pub const ENGINE_NAME: &str = "vsd-layout";
/// The newest contract this build implements (older versions remain
/// implemented forever: `EngineVersion::parse` lists them all).
pub const ENGINE_VERSION: &str = "1.1.0";

pub type Result<T> = std::result::Result<T, LayoutError>;

#[derive(Debug, Error)]
pub enum LayoutError {
    #[error("unsupported by vsd-layout/1.0: {0}")]
    Unsupported(String),

    #[error(transparent)]
    Core(#[from] vsd_core::Error),
}

/// Lay the document out and attach the render cache and page index,
/// producing a successor document (the manifest commits to the cache,
/// so the document id changes; the original is recorded as
/// `predecessor`, and prior signatures belong to it).
pub fn add_render_cache(doc: &Document, opts: &LayoutOptions) -> Result<Document> {
    let pages = layout_document(doc, opts)?;

    let mut store = doc.store.clone();
    let mut page_ids = Vec::with_capacity(pages.len());
    let mut closures = Vec::with_capacity(pages.len());
    for page in &pages {
        let value = page.to_value();
        let id = store.put_value(&value)?;
        // Page closure (spec §9): the page object plus every resource
        // it places, sorted and deduplicated.
        let mut closure = vec![id];
        for op in &page.ops {
            if let vsd_core::layout::DisplayOp::Image { res, .. } = op {
                closure.push(*res);
            }
        }
        closure.sort();
        closure.dedup();
        page_ids.push(id);
        closures.push(closure);
    }

    let layout_hash = RenderCache::compute_layout_hash(&page_ids)?;
    let cache = RenderCache {
        engine_name: ENGINE_NAME.into(),
        engine_version: opts.engine.as_str().into(),
        width_mm: opts.page_width_um as f64 / 1000.0,
        height_mm: opts.page_height_um as f64 / 1000.0,
        pages: page_ids,
        layout_hash,
    };
    let cache_id = store.put_value(&cache.to_value())?;
    let index_id = store.put_value(&PageIndex { pages: closures }.to_value())?;

    let manifest = Manifest {
        render_cache: Some(cache_id),
        page_index: Some(index_id),
        predecessor: Some(doc.manifest.document_id()?),
        ..doc.manifest.clone()
    };
    let mut out = Document { manifest, store };
    let keep = out.closure()?;
    out.store.retain_only(&keep); // drop any superseded cache objects
    Ok(out)
}

/// Outcome of recomputation-based cache verification (spec §5.2).
#[derive(Debug, PartialEq, Eq)]
pub enum RecomputeOutcome {
    /// The cache is exactly what this engine produces from the content
    /// tree: pixels and meaning agree.
    Match { pages: usize },
    /// The cache does not match the content tree — it lies.
    Mismatch {
        expected_pages: Vec<ObjectId>,
        cached_pages: Vec<ObjectId>,
    },
    /// No render cache present; nothing to verify.
    NoCache,
    /// The cache was produced by an engine this build cannot reproduce.
    UnknownEngine { name: String, version: String },
}

/// Re-run layout and compare page object ids against the cache. Every
/// engine version this build ever shipped remains implemented — a 1.0
/// cache recomputes under the 1.0 contract, byte-identically, forever.
pub fn verify_render_cache(doc: &Document) -> Result<RecomputeOutcome> {
    let Some(cache) = doc.render_cache()? else {
        return Ok(RecomputeOutcome::NoCache);
    };
    let version =
        EngineVersion::parse(&cache.engine_version).filter(|_| cache.engine_name == ENGINE_NAME);
    let Some(version) = version else {
        return Ok(RecomputeOutcome::UnknownEngine {
            name: cache.engine_name,
            version: cache.engine_version,
        });
    };
    // Geometry round-trips exactly: mm fields were produced as µm/1000.
    let opts = LayoutOptions {
        page_width_um: (cache.width_mm * 1000.0).round() as i64,
        page_height_um: (cache.height_mm * 1000.0).round() as i64,
        engine: version,
    };
    let pages = layout_document(doc, &opts)?;
    let mut expected = Vec::with_capacity(pages.len());
    for page in &pages {
        expected.push(ObjectId::of_value(&page.to_value())?);
    }
    if expected == cache.pages {
        Ok(RecomputeOutcome::Match {
            pages: expected.len(),
        })
    } else {
        Ok(RecomputeOutcome::Mismatch {
            expected_pages: expected,
            cached_pages: cache.pages,
        })
    }
}
