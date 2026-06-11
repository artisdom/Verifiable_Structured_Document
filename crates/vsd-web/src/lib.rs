//! # vsd-web
//!
//! The browser viewer core (ROADMAP 4b): vsd-core + vsd-layout +
//! vsd-render compiled to `wasm32-unknown-unknown`, exposed through a
//! deliberately tiny C ABI consumed by the hand-written `<vsd-doc>` web
//! component in `www/` — no JS bundler, no wasm-bindgen toolchain, no
//! npm. The viewer travels as a few hundred KB of WASM; that is the
//! distribution hack PDF never had.
//!
//! Safety posture: all parsing, verification, layout, and rasterization
//! happen in the safe, `forbid(unsafe_code)` crates this one wraps.
//! The single `ffi` module below contains the minimal pointer-crossing
//! unsafe that any WASM boundary requires, and nothing else.

#![deny(unsafe_code)]

use vsd_container::ReadOptions;
use vsd_core::layout::Page;
use vsd_core::Document;

/// A loaded document plus its display lists (from the render cache when
/// present and trusted-for-speed, else a fresh deterministic layout).
pub struct Session {
    pub document: Document,
    pub pages: Vec<Page>,
    pub signatures: Vec<vsd_container::Signature>,
    pub from_cache: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum VerifyState {
    /// Render cache recomputed byte-identically: pixels match meaning.
    RecomputeMatch,
    /// No cache present; pages were laid out fresh from the tree (which
    /// is trivially faithful to it).
    FreshLayout,
    /// The cache LIES about the content tree.
    RecomputeMismatch,
    /// Cache from an engine this build cannot reproduce.
    UnknownEngine,
}

impl Session {
    pub fn open(bytes: &[u8]) -> Result<Session, String> {
        let file = vsd_container::read_document(bytes, &ReadOptions::default())
            .map_err(|e| e.to_string())?;
        let document = file.document;
        let (pages, from_cache) = match document.render_cache().map_err(|e| e.to_string())? {
            Some(cache) => {
                let pages = cache
                    .pages
                    .iter()
                    .map(|id| {
                        Page::from_value(&document.store.get_value(id).map_err(|e| e.to_string())?)
                            .map_err(|e| e.to_string())
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                (pages, true)
            }
            None => (
                vsd_layout::layout_document(&document, &vsd_layout::LayoutOptions::default())
                    .map_err(|e| e.to_string())?,
                false,
            ),
        };
        Ok(Session {
            pages,
            signatures: file.signatures,
            from_cache,
            document,
        })
    }

    /// Verify every signature in the browser (Ed25519 and hybrid PQ —
    /// verification needs no RNG). Returns (valid, total).
    pub fn verify_signatures(&self) -> (usize, usize) {
        let valid = self
            .signatures
            .iter()
            .filter(|s| {
                matches!(
                    vsd_sign::verify(&self.document, s),
                    Ok(vsd_sign::Verdict::Valid)
                )
            })
            .count();
        (valid, self.signatures.len())
    }

    pub fn render_png(&self, page: usize, dpi: f64) -> Result<Vec<u8>, String> {
        let page = self.pages.get(page).ok_or("page out of range")?;
        vsd_render::render_page_png(&self.document, page, dpi).map_err(|e| e.to_string())
    }

    pub fn text(&self) -> Result<String, String> {
        vsd_core::extract::extract_text(&self.document).map_err(|e| e.to_string())
    }

    /// Re-run the layout engine against the content tree (spec §5.3).
    pub fn verify_recompute(&self) -> Result<VerifyState, String> {
        if !self.from_cache {
            return Ok(VerifyState::FreshLayout);
        }
        match vsd_layout::verify_render_cache(&self.document).map_err(|e| e.to_string())? {
            vsd_layout::RecomputeOutcome::Match { .. } => Ok(VerifyState::RecomputeMatch),
            vsd_layout::RecomputeOutcome::NoCache => Ok(VerifyState::FreshLayout),
            vsd_layout::RecomputeOutcome::Mismatch { .. } => Ok(VerifyState::RecomputeMismatch),
            vsd_layout::RecomputeOutcome::UnknownEngine { .. } => Ok(VerifyState::UnknownEngine),
        }
    }

    /// Viewer metadata as JSON (consumed by the web component's badge).
    pub fn info_json(&self) -> String {
        let meta = self.document.metadata().unwrap_or_default();
        let report = vsd_core::validate::validate(&self.document);
        let recompute = match self.verify_recompute() {
            Ok(VerifyState::RecomputeMatch) => "match",
            Ok(VerifyState::FreshLayout) => "fresh-layout",
            Ok(VerifyState::RecomputeMismatch) => "MISMATCH",
            Ok(VerifyState::UnknownEngine) => "unknown-engine",
            Err(_) => "error",
        };
        let (sigs_valid, sigs_total) = self.verify_signatures();
        serde_json::json!({
            "doc_id": self.document.document_id().map(|i| i.to_hex()).unwrap_or_default(),
            "title": meta.title,
            "pages": self.pages.len(),
            "valid": report.is_valid(),
            "warnings": report.warnings().count(),
            "signatures": sigs_total,
            "signatures_valid": sigs_valid,
            "recompute": recompute,
            "profile": self.document.manifest.profile.as_str(),
        })
        .to_string()
    }
}

/// The WASM boundary. Protocol (see `www/vsd-doc.js`):
///
/// 1. `vsd_alloc(len)` → write your bytes into linear memory
/// 2. `vsd_open(ptr, len)` → handle (> 0) or 0 on failure
/// 3. `vsd_page_count`, `vsd_render_page`, `vsd_text`, `vsd_info`,
///    `vsd_verify` — buffer-returning calls expose the result via
///    `vsd_buf_ptr()` + their returned length
/// 4. `vsd_close(handle)`, `vsd_free(ptr, len)`
#[allow(unsafe_code)]
#[cfg(target_arch = "wasm32")]
mod ffi {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::*;

    thread_local! {
        static SESSIONS: RefCell<BTreeMap<i32, Session>> = const { RefCell::new(BTreeMap::new()) };
        static NEXT: RefCell<i32> = const { RefCell::new(1) };
        static RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    }

    #[no_mangle]
    pub extern "C" fn vsd_alloc(len: u32) -> *mut u8 {
        let mut buf = vec![0u8; len as usize];
        let ptr = buf.as_mut_ptr();
        std::mem::forget(buf);
        ptr
    }

    /// # Safety
    /// `ptr` must come from `vsd_alloc(len)` and not be freed twice.
    #[no_mangle]
    pub unsafe extern "C" fn vsd_free(ptr: *mut u8, len: u32) {
        if !ptr.is_null() {
            drop(Vec::from_raw_parts(ptr, len as usize, len as usize));
        }
    }

    /// # Safety
    /// `ptr..ptr+len` must be a live allocation from `vsd_alloc`.
    #[no_mangle]
    pub unsafe extern "C" fn vsd_open(ptr: *const u8, len: u32) -> i32 {
        let bytes = std::slice::from_raw_parts(ptr, len as usize);
        match Session::open(bytes) {
            Ok(session) => {
                let handle = NEXT.with(|n| {
                    let mut n = n.borrow_mut();
                    let h = *n;
                    *n += 1;
                    h
                });
                SESSIONS.with(|s| s.borrow_mut().insert(handle, session));
                handle
            }
            Err(_) => 0,
        }
    }

    #[no_mangle]
    pub extern "C" fn vsd_close(handle: i32) {
        SESSIONS.with(|s| s.borrow_mut().remove(&handle));
    }

    #[no_mangle]
    pub extern "C" fn vsd_page_count(handle: i32) -> i32 {
        SESSIONS.with(|s| {
            s.borrow()
                .get(&handle)
                .map(|x| x.pages.len() as i32)
                .unwrap_or(-1)
        })
    }

    fn put_result(bytes: Vec<u8>) -> i32 {
        let len = bytes.len() as i32;
        RESULT.with(|r| *r.borrow_mut() = bytes);
        len
    }

    #[no_mangle]
    pub extern "C" fn vsd_buf_ptr() -> *const u8 {
        RESULT.with(|r| r.borrow().as_ptr())
    }

    #[no_mangle]
    pub extern "C" fn vsd_render_page(handle: i32, page: u32, dpi: f32) -> i32 {
        SESSIONS.with(|s| {
            s.borrow()
                .get(&handle)
                .and_then(|x| x.render_png(page as usize, dpi as f64).ok())
                .map(put_result)
                .unwrap_or(-1)
        })
    }

    #[no_mangle]
    pub extern "C" fn vsd_text(handle: i32) -> i32 {
        SESSIONS.with(|s| {
            s.borrow()
                .get(&handle)
                .and_then(|x| x.text().ok())
                .map(|t| put_result(t.into_bytes()))
                .unwrap_or(-1)
        })
    }

    #[no_mangle]
    pub extern "C" fn vsd_info(handle: i32) -> i32 {
        SESSIONS.with(|s| {
            s.borrow()
                .get(&handle)
                .map(|x| put_result(x.info_json().into_bytes()))
                .unwrap_or(-1)
        })
    }

    /// 0 = recompute match · 1 = fresh layout (no cache) ·
    /// 2 = MISMATCH (the cache lies) · 3 = unknown engine · -1 = error
    #[no_mangle]
    pub extern "C" fn vsd_verify(handle: i32) -> i32 {
        SESSIONS.with(|s| {
            s.borrow()
                .get(&handle)
                .and_then(|x| x.verify_recompute().ok())
                .map(|v| match v {
                    VerifyState::RecomputeMatch => 0,
                    VerifyState::FreshLayout => 1,
                    VerifyState::RecomputeMismatch => 2,
                    VerifyState::UnknownEngine => 3,
                })
                .unwrap_or(-1)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vsd_container::WriteOptions;
    use vsd_core::compose::Compose;

    fn doc_bytes(compress: bool) -> Vec<u8> {
        let doc = Compose::new("en")
            .title("Web view")
            .h1("Hello browser")
            .para("Rendered inside WASM, verified inside WASM.")
            .finish()
            .unwrap();
        let laid =
            vsd_layout::add_render_cache(&doc, &vsd_layout::LayoutOptions::default()).unwrap();
        vsd_container::write_document(&laid, &[], &WriteOptions { compress }).unwrap()
    }

    #[test]
    fn session_opens_renders_and_verifies() {
        let session = Session::open(&doc_bytes(false)).unwrap();
        assert_eq!(session.pages.len(), 1);
        assert!(session.from_cache);
        assert_eq!(
            session.verify_recompute().unwrap(),
            VerifyState::RecomputeMatch
        );

        let png = session.render_png(0, 96.0).unwrap();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));

        let info: serde_json::Value = serde_json::from_str(&session.info_json()).unwrap();
        assert_eq!(info["pages"], 1);
        assert_eq!(info["valid"], true);
        assert_eq!(info["recompute"], "match");
    }

    #[test]
    fn signatures_verify_without_rng_access() {
        // Sign on the host (keygen feature, dev-dep only); the verify
        // path used here is the same RNG-free code the wasm build ships.
        let doc = Compose::new("en").para("signed page").finish().unwrap();
        let ed = vsd_sign::SigningKey::from_seed(&[3u8; 32]).unwrap();
        let hybrid = vsd_sign::HybridSigningKey::generate().unwrap();
        let sigs = vec![
            ed.sign_document(&doc).unwrap(),
            hybrid.sign_document(&doc).unwrap(),
        ];
        let bytes =
            vsd_container::write_document(&doc, &sigs, &WriteOptions { compress: false }).unwrap();

        let session = Session::open(&bytes).unwrap();
        assert_eq!(session.verify_signatures(), (2, 2));
        let info: serde_json::Value = serde_json::from_str(&session.info_json()).unwrap();
        assert_eq!(info["signatures"], 2);
        assert_eq!(info["signatures_valid"], 2);
    }

    #[test]
    fn structure_only_documents_lay_out_fresh() {
        let doc = Compose::new("en").para("no cache here").finish().unwrap();
        let bytes =
            vsd_container::write_document(&doc, &[], &WriteOptions { compress: false }).unwrap();
        let session = Session::open(&bytes).unwrap();
        assert!(!session.from_cache);
        assert_eq!(
            session.verify_recompute().unwrap(),
            VerifyState::FreshLayout
        );
        assert_eq!(session.pages.len(), 1);
    }
}
