//! Geometry-based recovery for **untagged** foreign PDFs (ROADMAP 3c).
//!
//! A foreign PDF with no logical structure tree still positions its text
//! precisely. We decode each page's content stream into positioned text
//! fragments (tracking the text matrix and font size, decoding glyph
//! bytes through each font's encoding like `lopdf`'s extractor), then
//! cluster them: fragments on the same baseline become a line, lines
//! separated by a normal leading become a paragraph, a larger-than-body
//! font marks a heading, and a clear vertical gutter splits a page into
//! columns read left-to-right.
//!
//! This is **heuristic** — there is no ground truth in an untagged PDF —
//! so the result is still marked `format-migrated { lossy: true }`. It is
//! strictly better than the naive line-grouping fallback (headings and
//! paragraph boundaries are recovered from layout, not guessed from blank
//! lines), and it never invents content: every character comes from the
//! page's own text. Returns `None` if no text is found, so the caller
//! falls back to the trait recoverer.

use lopdf::{Document as Pdf, Encoding, Object, ObjectId};
use std::collections::BTreeMap;

use vsd_core::tree::{Inline, Node, Para};

/// Short identifier recorded in the import provenance `tool` claim.
pub const TOOL: &str = "vsd-pdf/geometry-recovery";

/// A shown piece of text with its page position and effective size, all
/// in PDF user-space points (y grows up).
struct Fragment {
    x: f64,
    y: f64,
    size: f64,
    text: String,
}

/// Recover block nodes from an untagged PDF by text geometry, or `None`
/// if the pages carry no extractable text.
pub fn recover_geometry(pdf: &Pdf) -> Option<Vec<Node>> {
    let mut blocks = Vec::new();
    let mut any_text = false;
    for (i, (_, page_id)) in pdf.get_pages().into_iter().enumerate() {
        if i > 0 {
            blocks.push(Node::PageBreakHint);
        }
        let frags = page_fragments(pdf, page_id);
        if !frags.is_empty() {
            any_text = true;
        }
        blocks.extend(page_blocks(frags));
    }
    // Drop a trailing page-break with nothing after it.
    while matches!(blocks.last(), Some(Node::PageBreakHint)) {
        blocks.pop();
    }
    (any_text && !blocks.is_empty()).then_some(blocks)
}

// --- Fragment extraction from a page content stream -------------------------

fn page_fragments(pdf: &Pdf, page_id: ObjectId) -> Vec<Fragment> {
    let mut out = Vec::new();
    let Ok(fonts) = pdf.get_page_fonts(page_id) else {
        return out;
    };
    let encodings: BTreeMap<Vec<u8>, Encoding> = fonts
        .into_iter()
        .filter_map(|(name, font)| font.get_font_encoding(pdf).ok().map(|e| (name, e)))
        .collect();
    let Ok(content) = pdf.get_and_decode_page_content(page_id) else {
        return out;
    };

    // Text state: line matrix translation (tlm), text matrix translation
    // (tm), leading, font size, vertical scale (from Tm), encoding.
    let (mut tlm_x, mut tlm_y) = (0.0f64, 0.0f64);
    let (mut tm_x, mut tm_y) = (0.0f64, 0.0f64);
    let mut leading = 0.0f64;
    let mut fs = 0.0f64;
    let mut scale = 1.0f64;
    let mut enc: Option<&Encoding> = None;

    let num = |o: &Object| -> f64 {
        match o {
            Object::Integer(i) => *i as f64,
            Object::Real(r) => *r as f64,
            _ => 0.0,
        }
    };

    for op in &content.operations {
        let a = &op.operands;
        match op.operator.as_str() {
            "BT" => {
                tlm_x = 0.0;
                tlm_y = 0.0;
                tm_x = 0.0;
                tm_y = 0.0;
            }
            "Tf" => {
                enc = a
                    .first()
                    .and_then(|o| o.as_name().ok())
                    .and_then(|n| encodings.get(n));
                fs = a.get(1).map(num).unwrap_or(0.0);
            }
            "Td" => {
                tlm_x += a.first().map(num).unwrap_or(0.0);
                tlm_y += a.get(1).map(num).unwrap_or(0.0);
                tm_x = tlm_x;
                tm_y = tlm_y;
            }
            "TD" => {
                let ty = a.get(1).map(num).unwrap_or(0.0);
                leading = -ty;
                tlm_x += a.first().map(num).unwrap_or(0.0);
                tlm_y += ty;
                tm_x = tlm_x;
                tm_y = tlm_y;
            }
            "TL" => leading = a.first().map(num).unwrap_or(0.0),
            "T*" => {
                tlm_y -= leading;
                tm_x = tlm_x;
                tm_y = tlm_y;
            }
            "Tm" => {
                // [a b c d e f]: translation (e,f), vertical scale ~ d.
                scale = a.get(3).map(num).unwrap_or(1.0).abs().max(0.01);
                tlm_x = a.get(4).map(num).unwrap_or(0.0);
                tlm_y = a.get(5).map(num).unwrap_or(0.0);
                tm_x = tlm_x;
                tm_y = tlm_y;
            }
            "Tj" => {
                if let Some(enc) = enc {
                    if let Some(Object::String(bytes, _)) = a.first() {
                        push_fragment(&mut out, pdf, enc, bytes, tm_x, tm_y, fs * scale, &mut tm_x);
                    }
                }
            }
            "'" | "\"" => {
                // Move to next line, then show (the apostrophe operators).
                tlm_y -= leading;
                tm_x = tlm_x;
                tm_y = tlm_y;
                let s = if op.operator == "\"" {
                    a.get(2)
                } else {
                    a.first()
                };
                if let (Some(enc), Some(Object::String(bytes, _))) = (enc, s) {
                    push_fragment(&mut out, pdf, enc, bytes, tm_x, tm_y, fs * scale, &mut tm_x);
                }
            }
            "TJ" => {
                if let (Some(enc), Some(Object::Array(arr))) = (enc, a.first()) {
                    let start_x = tm_x;
                    let mut text = String::new();
                    for el in arr {
                        match el {
                            Object::String(bytes, _) => {
                                if let Ok(t) = Pdf::decode_text(enc, bytes) {
                                    text.push_str(&t);
                                }
                                tm_x += est_width(
                                    &Pdf::decode_text(enc, bytes).unwrap_or_default(),
                                    fs * scale,
                                );
                            }
                            Object::Integer(_) | Object::Real(_) => {
                                tm_x += -num(el) / 1000.0 * fs * scale;
                            }
                            _ => {}
                        }
                    }
                    if !text.trim().is_empty() {
                        out.push(Fragment {
                            x: start_x,
                            y: tm_y,
                            size: fs * scale,
                            text,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn push_fragment(
    out: &mut Vec<Fragment>,
    _pdf: &Pdf,
    enc: &Encoding,
    bytes: &[u8],
    x: f64,
    y: f64,
    size: f64,
    tm_x: &mut f64,
) {
    if let Ok(text) = Pdf::decode_text(enc, bytes) {
        *tm_x += est_width(&text, size);
        if !text.trim().is_empty() {
            out.push(Fragment { x, y, size, text });
        }
    }
}

/// A rough advance estimate (we don't carry per-glyph widths): enough to
/// keep same-line fragments in monotonic x order for sorting.
fn est_width(text: &str, size: f64) -> f64 {
    text.chars().count() as f64 * size * 0.5
}

// --- Clustering: fragments → lines → blocks ---------------------------------

struct Line {
    text: String,
    x: f64,
    y: f64,
    size: f64,
}

fn page_blocks(frags: Vec<Fragment>) -> Vec<Node> {
    if frags.is_empty() {
        return Vec::new();
    }
    // Conservative column split: if a clear vertical gutter separates the
    // fragments into left/right bands, recover each column in order.
    if let Some((left, right)) = split_columns(&frags) {
        let mut out = page_blocks(left);
        out.extend(page_blocks(right));
        return out;
    }

    let lines = group_lines(frags);
    lines_to_blocks(lines)
}

/// Group fragments into text lines by baseline proximity.
fn group_lines(mut frags: Vec<Fragment>) -> Vec<Line> {
    // Top-to-bottom (y descending), then left-to-right.
    frags.sort_by(|a, b| {
        b.y.partial_cmp(&a.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut lines: Vec<Line> = Vec::new();
    for f in frags {
        if let Some(last) = lines.last_mut() {
            // Same line if the baseline is within half the line's size.
            if (last.y - f.y).abs() <= last.size.max(f.size) * 0.5 {
                // Insert a space if there's a horizontal gap.
                if !last.text.ends_with(' ') && !f.text.starts_with(' ') {
                    last.text.push(' ');
                }
                last.text.push_str(&f.text);
                last.size = last.size.max(f.size);
                continue;
            }
        }
        lines.push(Line {
            text: f.text.clone(),
            x: f.x,
            y: f.y,
            size: f.size,
        });
    }
    for l in &mut lines {
        l.text = collapse(&l.text);
    }
    lines.retain(|l| !l.text.trim().is_empty());
    lines
}

/// Turn ordered lines into headings and paragraphs. The body size is the
/// median line size; a line ≥ 1.25× that (and not too long) is a heading;
/// consecutive body lines join into a paragraph until the vertical gap or
/// the left indent jumps.
fn lines_to_blocks(lines: Vec<Line>) -> Vec<Node> {
    if lines.is_empty() {
        return Vec::new();
    }
    let body = median_size(&lines);
    let mut out = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let mut para_x = 0.0f64;
    let mut prev_y = 0.0f64;
    let mut prev_size = body;

    let flush = |para: &mut Vec<String>, out: &mut Vec<Node>| {
        if !para.is_empty() {
            let text = para.join(" ");
            para.clear();
            if !text.trim().is_empty() {
                out.push(Node::Para(Para {
                    children: vec![Inline::Text(text)],
                }));
            }
        }
    };

    for (i, l) in lines.iter().enumerate() {
        let is_heading = l.size >= body * 1.25 && l.text.chars().count() <= 120;
        if is_heading {
            flush(&mut para, &mut out);
            out.push(Node::Heading(vsd_core::tree::Heading {
                level: heading_level(l.size, body),
                children: vec![Inline::Text(l.text.clone())],
            }));
        } else {
            let gap = prev_y - l.y;
            let new_para = para.is_empty()
                || i == 0
                || gap > prev_size * 1.8
                || (l.x - para_x).abs() > prev_size * 1.5;
            if new_para {
                flush(&mut para, &mut out);
                para_x = l.x;
            }
            para.push(l.text.clone());
        }
        prev_y = l.y;
        prev_size = l.size;
    }
    flush(&mut para, &mut out);
    out
}

fn heading_level(size: f64, body: f64) -> u8 {
    let r = size / body;
    if r >= 2.0 {
        1
    } else if r >= 1.6 {
        2
    } else {
        3
    }
}

fn median_size(lines: &[Line]) -> f64 {
    let mut sizes: Vec<f64> = lines.iter().map(|l| l.size).collect();
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let m = sizes[sizes.len() / 2];
    if m > 0.0 {
        m
    } else {
        12.0
    }
}

/// Detect a clean two-column split: a vertical gutter near the middle
/// that almost no fragment straddles, with substantial text on both
/// sides. Conservative on purpose — a wrong split is worse than none.
fn split_columns(frags: &[Fragment]) -> Option<(Vec<Fragment>, Vec<Fragment>)> {
    if frags.len() < 12 {
        return None;
    }
    let min_x = frags.iter().map(|f| f.x).fold(f64::INFINITY, f64::min);
    let max_x = frags
        .iter()
        .map(|f| f.x + est_width(&f.text, f.size))
        .fold(f64::NEG_INFINITY, f64::max);
    if !(min_x.is_finite() && max_x.is_finite()) || max_x - min_x < 100.0 {
        return None;
    }
    let mid = (min_x + max_x) / 2.0;
    // A fragment straddles the gutter if it starts left of mid and extends
    // well past it.
    let mut straddle = 0usize;
    let mut left = 0usize;
    let mut right = 0usize;
    for f in frags {
        let end = f.x + est_width(&f.text, f.size);
        if f.x < mid && end > mid + (max_x - min_x) * 0.05 {
            straddle += 1;
        } else if end <= mid {
            left += 1;
        } else if f.x >= mid {
            right += 1;
        }
    }
    let total = frags.len();
    // Require a near-empty gutter and real content on both sides.
    if straddle * 20 <= total && left * 5 >= total && right * 5 >= total {
        let l: Vec<Fragment> = frags
            .iter()
            .filter(|f| f.x + est_width(&f.text, f.size) <= mid)
            .map(clone_frag)
            .collect();
        let r: Vec<Fragment> = frags
            .iter()
            .filter(|f| f.x + est_width(&f.text, f.size) > mid)
            .map(clone_frag)
            .collect();
        return Some((l, r));
    }
    None
}

fn clone_frag(f: &Fragment) -> Fragment {
    Fragment {
        x: f.x,
        y: f.y,
        size: f.size,
        text: f.text.clone(),
    }
}

/// Collapse runs of whitespace to single spaces.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
