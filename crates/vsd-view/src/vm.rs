//! The viewer's model — all the behavior that matters, with no GUI
//! attached, so it is unit-testable: page navigation, zoom, search
//! (exact, via display-list text runs — not OCR), highlight geometry,
//! and the verification banner.

use vsd_core::layout::{DisplayOp, Page};
use vsd_core::Document;
use vsd_layout::font::FontMetrics;

const PT_TO_MM: f64 = 25.4 / 72.0;

/// Verification summary shown as the banner — first-class UI, not a
/// property dialog three menus deep.
pub struct Banner {
    pub ok: bool,
    pub text: String,
}

pub fn make_banner(doc: &Document, signatures: &[vsd_container::Signature]) -> Banner {
    // Banner strings stick to glyphs the pinned font actually has
    // (Noto Sans LGC has no dingbats — a tofu box in the trust banner
    // would be a terrible look).
    let report = vsd_core::validate::validate(doc);
    if !report.is_valid() {
        return Banner {
            ok: false,
            text: "INVALID — document fails validation".into(),
        };
    }
    let mut sig_note = String::new();
    if !signatures.is_empty() {
        let valid = signatures
            .iter()
            .filter(|s| matches!(vsd_sign::verify(doc, s), Ok(vsd_sign::Verdict::Valid)))
            .count();
        if valid < signatures.len() {
            return Banner {
                ok: false,
                text: format!(
                    "SIGNATURE PROBLEM — {valid}/{} signatures verify for this revision",
                    signatures.len()
                ),
            };
        }
        sig_note = format!(" · {valid} signature(s) verified");
    }
    match vsd_layout::verify_render_cache(doc) {
        Ok(vsd_layout::RecomputeOutcome::Match { pages }) => Banner {
            ok: true,
            text: format!("VERIFIED — {pages} page(s) recomputed, pixels match content{sig_note}"),
        },
        Ok(vsd_layout::RecomputeOutcome::NoCache) => Banner {
            ok: true,
            text: format!("VERIFIED — laid out directly from content{sig_note}"),
        },
        Ok(vsd_layout::RecomputeOutcome::Mismatch { .. }) => Banner {
            ok: false,
            text: "RENDER CACHE LIES — displayed pixels do not match the content tree".into(),
        },
        Ok(vsd_layout::RecomputeOutcome::UnknownEngine { name, version }) => Banner {
            ok: false,
            text: format!("UNVERIFIED — cache from unknown engine {name}/{version}"),
        },
        Err(e) => Banner {
            ok: false,
            text: format!("VERIFICATION ERROR: {e}"),
        },
    }
}

/// A search hit's highlight box, in page millimetres.
#[derive(Debug, PartialEq)]
pub struct Highlight {
    pub x_mm: f64,
    pub y_mm: f64,
    pub w_mm: f64,
    pub h_mm: f64,
}

/// Width of `text` at `size_pt` in millimetres, using the engine font
/// (informative geometry for UI chrome; the normative layout is µm).
pub fn text_width_mm(text: &str, size_pt: f64) -> f64 {
    let m = FontMetrics::get();
    let units: i64 = text
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| m.advance_units(m.glyph(c)))
        .sum();
    units as f64 * size_pt / m.upem as f64 * PT_TO_MM
}

/// Find case-insensitive matches of `query` in a page's text runs.
/// Exact, because runs carry their own text — no raster heuristics.
pub fn page_highlights(page: &Page, query: &str) -> Vec<Highlight> {
    if query.is_empty() {
        return Vec::new();
    }
    let m = FontMetrics::get();
    let needle = query.to_lowercase();
    let mut out = Vec::new();
    for op in &page.ops {
        let DisplayOp::TextRun {
            x,
            y,
            size_pt,
            text,
            ..
        } = op
        else {
            continue;
        };
        let hay = text.to_lowercase();
        let mut from = 0usize;
        while let Some(pos) = hay[from..].find(&needle) {
            let start = from + pos;
            let end = start + needle.len();
            // Byte offsets from the lowercased haystack are only safe to
            // slice the original if boundaries align; guard for that.
            if text.is_char_boundary(start) && text.is_char_boundary(end) {
                let ascent_mm = m.ascent_units as f64 * size_pt / m.upem as f64 * PT_TO_MM;
                let descent_mm = -m.descent_units as f64 * size_pt / m.upem as f64 * PT_TO_MM;
                out.push(Highlight {
                    x_mm: x + text_width_mm(&text[..start], *size_pt),
                    y_mm: y - ascent_mm,
                    w_mm: text_width_mm(&text[start..end], *size_pt),
                    h_mm: ascent_mm + descent_mm,
                });
            }
            from = end.max(from + 1);
        }
    }
    out
}

/// Pages (0-based) containing at least one match.
pub fn matching_pages(pages: &[Page], query: &str) -> Vec<usize> {
    pages
        .iter()
        .enumerate()
        .filter(|(_, p)| !page_highlights(p, query).is_empty())
        .map(|(i, _)| i)
        .collect()
}

/// Plain text of one page, in paint order (for Ctrl+C).
pub fn page_text(page: &Page) -> String {
    let mut out = String::new();
    let mut last_y = f64::NEG_INFINITY;
    for op in &page.ops {
        if let DisplayOp::TextRun { y, text, .. } = op {
            if *y > last_y && !out.is_empty() {
                out.push('\n');
            } else if !out.is_empty() && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push_str(text);
            last_y = *y;
        }
    }
    out
}

/// Navigation + zoom state.
pub struct ViewState {
    pub page: usize,
    pub page_count: usize,
    /// 1.0 = fit width.
    pub zoom: f64,
    pub query: String,
    /// Pages with matches for `query`.
    pub matches: Vec<usize>,
    /// Search input mode buffer (Some while typing after '/').
    pub input: Option<String>,
}

impl ViewState {
    pub fn new(page_count: usize) -> Self {
        ViewState {
            page: 0,
            page_count,
            zoom: 1.0,
            query: String::new(),
            matches: Vec::new(),
            input: None,
        }
    }

    pub fn next_page(&mut self) {
        if self.page + 1 < self.page_count {
            self.page += 1;
        }
    }

    pub fn prev_page(&mut self) {
        self.page = self.page.saturating_sub(1);
    }

    pub fn zoom_in(&mut self) {
        self.zoom = (self.zoom * 1.2).min(8.0);
    }

    pub fn zoom_out(&mut self) {
        self.zoom = (self.zoom / 1.2).max(0.2);
    }

    pub fn zoom_reset(&mut self) {
        self.zoom = 1.0;
    }

    /// Jump to the next page with a match, wrapping.
    pub fn next_match(&mut self) {
        if let Some(&p) = self
            .matches
            .iter()
            .find(|&&p| p > self.page)
            .or_else(|| self.matches.first())
        {
            self.page = p;
        }
    }

    pub fn commit_search(&mut self, pages: &[Page]) {
        if let Some(q) = self.input.take() {
            self.query = q;
            self.matches = matching_pages(pages, &self.query);
            if !self.matches.contains(&self.page) {
                self.next_match();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vsd_core::compose::Compose;

    fn pages() -> (Document, Vec<Page>) {
        let doc = Compose::new("en")
            .h1("Findings")
            .para("The unique marker XYZZY lives here.")
            .page_break()
            .para("Second page, nothing to see.")
            .finish()
            .unwrap();
        let pages =
            vsd_layout::layout_document(&doc, &vsd_layout::LayoutOptions::default()).unwrap();
        (doc, pages)
    }

    #[test]
    fn search_finds_exact_highlights() {
        let (_, pages) = pages();
        assert_eq!(pages.len(), 2);
        let hits = page_highlights(&pages[0], "xyzzy");
        assert_eq!(hits.len(), 1, "case-insensitive exact hit");
        let h = &hits[0];
        assert!(h.w_mm > 0.0 && h.h_mm > 0.0);
        // The highlight starts after the preceding text, inside margins.
        assert!(h.x_mm > 20.0 && h.x_mm < 190.0);
        assert!(page_highlights(&pages[1], "xyzzy").is_empty());
        assert_eq!(matching_pages(&pages, "xyzzy"), vec![0]);
    }

    #[test]
    fn navigation_and_search_state() {
        let (_, pgs) = pages();
        let mut vs = ViewState::new(pgs.len());
        vs.next_page();
        assert_eq!(vs.page, 1);
        vs.next_page();
        assert_eq!(vs.page, 1, "clamped at end");

        vs.input = Some("xyzzy".into());
        vs.commit_search(&pgs);
        assert_eq!(vs.page, 0, "jumped to the matching page");
        assert_eq!(vs.matches, vec![0]);

        vs.zoom_in();
        assert!(vs.zoom > 1.0);
        vs.zoom_reset();
        assert_eq!(vs.zoom, 1.0);
    }

    #[test]
    fn banner_states() {
        let (doc, _) = pages();
        let banner = make_banner(&doc, &[]);
        assert!(banner.ok);
        assert!(banner.text.contains("laid out directly"), "{}", banner.text);

        let laid =
            vsd_layout::add_render_cache(&doc, &vsd_layout::LayoutOptions::default()).unwrap();
        let banner = make_banner(&laid, &[]);
        assert!(banner.ok);
        assert!(
            banner.text.contains("pixels match content"),
            "{}",
            banner.text
        );

        let key = vsd_sign::SigningKey::from_seed(&[9u8; 32]).unwrap();
        let sig = key.sign_document(&laid).unwrap();
        let banner = make_banner(&laid, &[sig]);
        assert!(banner.ok);
        assert!(
            banner.text.contains("1 signature(s) verified"),
            "{}",
            banner.text
        );
    }

    #[test]
    fn page_text_copies_run_content() {
        let (_, pgs) = pages();
        let text = page_text(&pgs[0]);
        assert!(text.contains("Findings"));
        assert!(text.contains("XYZZY"));
    }
}
