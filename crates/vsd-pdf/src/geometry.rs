//! Geometry-based recovery for **untagged** foreign PDFs (ROADMAP 3c).
//!
//! A foreign PDF with no logical structure tree still positions its text
//! precisely. We decode each page's content stream into positioned text
//! fragments (tracking the text matrix and font size, decoding glyph
//! bytes through each font's encoding like `lopdf`'s extractor), then
//! cluster them: aligned compact cells become tables, clear vertical gutters
//! separate columns read left-to-right, and spanning titles or text divide
//! column regions. Remaining baseline rows become lines and paragraphs;
//! larger-than-body fonts mark headings.
//!
//! Tables require at least three aligned rows with short cells (up to 24
//! characters); two-column tables also require repeated numeric values in
//! one column. Ambiguous layouts fall through to prose recovery. Header scope,
//! merged/empty cells, multiline cells, and ruling lines are not inferred.
//! Text advances remain approximate; rotated text and Form XObjects are not
//! handled by this extractor.
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

use vsd_core::tree::{Cell, ColSpec, Inline, Node, Para, Row, Table};

/// Short identifier recorded in the import provenance `tool` claim.
pub const TOOL: &str = "vsd-pdf/geometry-recovery";

/// A shown piece of text with its page position and effective size, all
/// in PDF user-space points (y grows up).
#[derive(Clone)]
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
                    let mut start_x = tm_x;
                    let mut text = String::new();
                    for el in arr {
                        match el {
                            Object::String(bytes, _) => {
                                if let Ok(t) = Pdf::decode_text(enc, bytes) {
                                    tm_x += est_width(&t, fs * scale);
                                    text.push_str(&t);
                                }
                            }
                            Object::Integer(_) | Object::Real(_) => {
                                let advance = -num(el) / 1000.0 * fs * scale;
                                tm_x += advance;
                                // Large positive adjustments separate cells/columns;
                                // ordinary kerning stays within the same text run.
                                if advance > fs.abs() * scale * 1.5 && !text.trim().is_empty() {
                                    out.push(Fragment {
                                        x: start_x,
                                        y: tm_y,
                                        size: fs * scale,
                                        text: std::mem::take(&mut text),
                                    });
                                    start_x = tm_x;
                                } else if text.is_empty() {
                                    start_x = tm_x;
                                }
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

/// Preserve baseline rows until tables and column regions have been identified.
/// Joining an entire baseline first would erase the boundaries between cells.
fn page_blocks(frags: Vec<Fragment>) -> Vec<Node> {
    let rows = group_rows(frags);
    if rows.is_empty() {
        return Vec::new();
    }
    let body = median_size(&rows.iter().cloned().map(row_line).collect::<Vec<_>>());
    let mut out = Vec::new();
    let mut pending = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let (end, table) = recover_table(&rows, i);
        if let Some(table) = table {
            out.extend(column_blocks(std::mem::take(&mut pending), body, 0));
            out.push(table);
        } else {
            pending.extend_from_slice(&rows[i..end]);
        }
        i = end;
    }
    out.extend(column_blocks(pending, body, 0));
    out
}

fn group_rows(mut frags: Vec<Fragment>) -> Vec<Vec<Fragment>> {
    frags.retain(|f| f.x.is_finite() && f.y.is_finite() && f.size.is_finite());
    frags.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));
    let mut rows: Vec<Vec<Fragment>> = Vec::new();
    for f in frags {
        if let Some(row) = rows.last_mut() {
            let first = &row[0];
            if (first.y - f.y).abs() <= first.size.max(f.size) * 0.35 {
                row.push(f);
                continue;
            }
        }
        rows.push(vec![f]);
    }
    for row in &mut rows {
        row.sort_by(|a, b| a.x.total_cmp(&b.x));
    }
    rows
}

/// Merge adjacent text runs, leaving wide horizontal gaps as cell boundaries.
fn row_cells(row: &[Fragment]) -> Vec<Fragment> {
    let mut cells: Vec<Fragment> = Vec::new();
    for f in row {
        if let Some(last) = cells.last_mut() {
            let gap = f.x - (last.x + est_width(&last.text, last.size));
            if gap <= last.size.max(f.size) * 1.5 {
                last.text.push(' ');
                last.text.push_str(&f.text);
                last.size = last.size.max(f.size);
                continue;
            }
        }
        cells.push(f.clone());
    }
    for cell in &mut cells {
        cell.text = collapse(&cell.text);
    }
    cells
}

/// Require at least three compact, consistently aligned rows. Two-column
/// tables additionally need repeated numeric values in a column: short parallel
/// prose is otherwise indistinguishable from a borderless table. Do not infer
/// header scope, spans, or empty cells from geometry alone.
fn recover_table(rows: &[Vec<Fragment>], start: usize) -> (usize, Option<Node>) {
    let first = row_cells(&rows[start]);
    let compact = |cells: &[Fragment]| {
        cells.len() >= 2
            && cells
                .iter()
                .all(|c| c.size > 0.0 && c.text.chars().count() <= 24)
    };
    if !compact(&first) {
        return (start + 1, None);
    }
    let mut cells = vec![first];
    let mut end = start + 1;
    while end < rows.len() {
        let next = row_cells(&rows[end]);
        let prev = &cells[cells.len() - 1];
        let gap = prev[0].y - next[0].y;
        if !compact(&next)
            || next.len() != cells[0].len()
            || gap <= 0.0
            || gap > prev[0].size.max(next[0].size) * 2.5
            || next.iter().zip(&cells[0]).any(|(a, b)| {
                (a.x - b.x).abs() > a.size.max(b.size) * 0.5
                    || (a.size - b.size).abs() > b.size * 0.25
            })
        {
            break;
        }
        cells.push(next);
        end += 1;
    }
    if cells.len() < 3 {
        return (end, None);
    }
    if cells[0].len() == 2
        && !(0..2).any(|col| {
            cells
                .iter()
                .filter(|row| numeric_cell(&row[col].text))
                .count()
                >= 2
        })
    {
        return (end, None);
    }
    let cols = (0..cells[0].len())
        .map(|_| ColSpec { width: None })
        .collect();
    let body = cells
        .into_iter()
        .map(|row| Row {
            cells: row
                .into_iter()
                .map(|c| Cell {
                    span: None,
                    scope: None,
                    children: vec![Node::Para(Para {
                        children: vec![Inline::Text(c.text)],
                    })],
                })
                .collect(),
        })
        .collect();
    (
        end,
        Some(Node::Table(Table {
            cols,
            head: vec![],
            body,
            foot: vec![],
        })),
    )
}

fn numeric_cell(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_digit())
        && text.chars().all(|c| {
            c.is_ascii_digit()
                || c.is_whitespace()
                || matches!(c, '.' | ',' | '-' | '+' | '%' | '$' | '€' | '£' | '(' | ')')
        })
}

fn column_blocks(rows: Vec<Vec<Fragment>>, body: f64, depth: usize) -> Vec<Node> {
    // Bound recursion for malicious pages with hundreds of apparent columns.
    if depth >= 16 {
        return lines_to_blocks(rows.into_iter().map(row_line).collect(), body);
    }
    let Some(cut) = column_cut(&rows) else {
        return lines_to_blocks(rows.into_iter().map(row_line).collect(), body);
    };
    let mut out = Vec::new();
    let mut band = Vec::new();
    let mut spanning = Vec::new();
    for row in rows {
        let crosses = row
            .iter()
            .any(|f| f.x < cut && f.x + est_width(&f.text, f.size) > cut);
        let heading = row.len() == 1 && row[0].size >= body * 1.25;
        if crosses || heading {
            out.extend(split_band(std::mem::take(&mut band), cut, body, depth));
            spanning.push(row_line(row));
        } else {
            out.extend(lines_to_blocks(std::mem::take(&mut spanning), body));
            band.push(row);
        }
    }
    out.extend(split_band(band, cut, body, depth));
    out.extend(lines_to_blocks(spanning, body));
    out
}

fn split_band(rows: Vec<Vec<Fragment>>, cut: f64, body: f64, depth: usize) -> Vec<Node> {
    let (left, right): (Vec<_>, Vec<_>) = rows.into_iter().flatten().partition(|f| f.x < cut);
    // Recurse to recover three or more columns, always partitioning every run.
    let mut out = column_blocks(group_rows(left), body, depth + 1);
    out.extend(column_blocks(group_rows(right), body, depth + 1));
    out
}

/// Search actual gaps rather than the page midpoint, allowing unequal widths.
/// Require at least three baselines in each column; baselines may be staggered. Spanning rows are handled as separators by `column_blocks`.
fn column_cut(rows: &[Vec<Fragment>]) -> Option<f64> {
    let mut candidates = Vec::new();
    for row in rows {
        for pair in row.windows(2) {
            let end = pair[0].x + est_width(&pair[0].text, pair[0].size);
            if pair[1].x - end > pair[0].size.max(pair[1].size) * 2.0 {
                candidates.push((end + pair[1].x) / 2.0);
            }
        }
    }
    let mut edges: Vec<f64> = rows
        .iter()
        .flatten()
        .flat_map(|f| [f.x, f.x + est_width(&f.text, f.size)])
        .collect();
    edges.sort_by(f64::total_cmp);
    edges.dedup_by(|a, b| (*a - *b).abs() < 1.0);
    for pair in edges.windows(2) {
        if pair[1] - pair[0] > 24.0 {
            candidates.push((pair[0] + pair[1]) / 2.0);
        }
    }
    candidates.sort_by(f64::total_cmp);
    candidates.dedup_by(|a, b| (*a - *b).abs() < 1.0);
    // Bound candidate evaluation on unusually fragmented foreign pages.
    let step = candidates.len().div_ceil(128).max(1);
    candidates
        .into_iter()
        .step_by(step)
        .filter_map(|cut| {
            let mut left_rows = 0;
            let mut right_rows = 0;
            let mut crossing = 0;
            for row in rows {
                let left = row.iter().any(|f| f.x + est_width(&f.text, f.size) <= cut);
                let right = row.iter().any(|f| f.x >= cut);
                if row
                    .iter()
                    .any(|f| f.x < cut && f.x + est_width(&f.text, f.size) > cut)
                {
                    crossing += 1;
                } else {
                    left_rows += usize::from(left);
                    right_rows += usize::from(right);
                }
            }
            let support = left_rows.min(right_rows);
            (support >= 3 && crossing <= support).then_some((support, cut))
        })
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.total_cmp(&a.1)))
        .map(|(_, cut)| cut)
}

fn row_line(row: Vec<Fragment>) -> Line {
    Line {
        text: collapse(
            &row.iter()
                .map(|f| f.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        ),
        x: row[0].x,
        y: row[0].y,
        size: row.iter().map(|f| f.size).fold(0.0, f64::max),
    }
}

/// Turn ordered lines into headings and paragraphs. The body size is the
/// page-wide median line size; a line ≥ 1.25× that (and not too long) is a heading;
/// consecutive body lines join into a paragraph until the vertical gap or
/// the left indent jumps.
fn lines_to_blocks(lines: Vec<Line>, body: f64) -> Vec<Node> {
    if lines.is_empty() {
        return Vec::new();
    }
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

/// Collapse runs of whitespace to single spaces.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
