//! Text preparation (LAYOUT-1.0.md §4) and line breaking (§5).

use vsd_core::tree::Inline;

use crate::font::FontMetrics;

/// A block's layout text plus link attribution ranges (byte ranges into
/// the layout text that carry link color).
pub struct LayoutText {
    pub text: String,
    pub links: Vec<(usize, usize)>,
}

/// Derive the layout text of inline content: concatenate in tree order,
/// collapse whitespace runs to a single space, strip leading/trailing.
pub fn layout_text(inlines: &[Inline]) -> LayoutText {
    let mut raw = String::new();
    let mut links = Vec::new();
    collect(inlines, false, &mut raw, &mut links);

    // Collapse whitespace, remapping link ranges as we go.
    let mut text = String::with_capacity(raw.len());
    let mut map = vec![0usize; raw.len() + 1]; // raw byte pos → collapsed byte pos
    let mut pending_space = false;
    let mut started = false;
    for (i, c) in raw.char_indices() {
        map[i] = text.len() + usize::from(pending_space);
        if matches!(c, ' ' | '\t' | '\n' | '\r') {
            if started {
                pending_space = true;
            }
        } else {
            if pending_space {
                text.push(' ');
                pending_space = false;
            }
            started = true;
            text.push(c);
        }
    }
    map[raw.len()] = text.len();

    let links = links
        .into_iter()
        .map(|(s, e)| (map[s].min(text.len()), map[e].min(text.len())))
        .filter(|(s, e)| s < e)
        .collect();
    LayoutText { text, links }
}

fn collect(inlines: &[Inline], in_link: bool, out: &mut String, links: &mut Vec<(usize, usize)>) {
    for inline in inlines {
        match inline {
            Inline::Text(s) => out.push_str(s),
            Inline::Span(sp) => collect(&sp.children, in_link, out, links),
            Inline::Link(l) => {
                let start = out.len();
                collect(&l.children, true, out, links);
                if !in_link {
                    links.push((start, out.len()));
                }
            }
            Inline::Math(m) => out.push_str(&m.mathml),
            Inline::FootnoteRef(id) => {
                out.push('[');
                out.push_str(id);
                out.push(']');
            }
        }
    }
}

/// One laid-out line: a byte range into the layout text (trailing break
/// space excluded) and its measured width.
#[derive(Debug, PartialEq, Eq)]
pub struct Line {
    pub start: usize,
    pub end: usize,
    pub width_um: i64,
}

/// Greedy first-fit line breaking (§5). Break opportunities exist only
/// after a collapsed space; oversized segments force-break before the
/// first overflowing glyph with a minimum of one glyph per line.
pub fn break_lines(font: &FontMetrics, text: &str, size_um: i64, max_width_um: i64) -> Vec<Line> {
    let mut lines = Vec::new();
    if text.is_empty() {
        return lines;
    }
    let space_w = font.space_advance_um(size_um);

    let mut line_start = 0usize;
    let mut line_width = 0i64;
    let mut pos = 0usize;

    // Words are the maximal space-free segments (text is collapsed, so
    // separators are single U+0020s).
    for word in text.split(' ') {
        let word_start = pos;
        let word_end = pos + word.len();
        pos = word_end + 1; // step over the separating space

        let sep_w = if line_width > 0 { space_w } else { 0 };
        let word_w = font.text_width_um(word, size_um);

        if line_width + sep_w + word_w <= max_width_um {
            line_width += sep_w + word_w;
            continue;
        }

        // The word does not fit after the current content.
        if line_width > 0 {
            lines.push(Line {
                start: line_start,
                end: word_start.saturating_sub(1),
                width_um: line_width,
            });
            line_start = word_start;
        }
        if word_w <= max_width_um {
            line_width = word_w;
            continue;
        }

        // Force-break the oversized word, ≥ 1 glyph per line.
        let mut seg_start = word_start;
        let mut seg_width = 0i64;
        for (off, c) in word.char_indices() {
            let cw = if c.is_control() {
                0
            } else {
                font.char_advance_um(c, size_um)
            };
            if seg_width > 0 && seg_width + cw > max_width_um {
                lines.push(Line {
                    start: seg_start,
                    end: word_start + off,
                    width_um: seg_width,
                });
                seg_start = word_start + off;
                seg_width = 0;
            }
            seg_width += cw;
        }
        line_start = seg_start;
        line_width = seg_width;
    }
    if line_width > 0 || line_start < text.len() {
        lines.push(Line {
            start: line_start,
            end: text.len(),
            width_um: line_width,
        });
    }
    lines
}

/// Code-line measurement (§4 exception): verbatim text where a tab
/// advances to the next multiple of four space-widths from line start.
/// Returns segments of (byte range, x offset µm) split at tabs, plus
/// the total width.
pub fn measure_code_line(
    font: &FontMetrics,
    line: &str,
    size_um: i64,
) -> (Vec<(usize, usize, i64)>, i64) {
    let tab_w = font.space_advance_um(size_um) * 4;
    let mut segments = Vec::new();
    let mut seg_start = 0usize;
    let mut seg_x = 0i64;
    for (i, c) in line.char_indices() {
        match c {
            '\t' => {
                if i > seg_start {
                    segments.push((seg_start, i, seg_x));
                }
                let x = seg_x + font.text_width_um(&line[seg_start..i], size_um);
                seg_x = if tab_w > 0 {
                    ((x / tab_w) + 1) * tab_w
                } else {
                    x
                };
                seg_start = i + 1;
            }
            '\r' => {
                if i > seg_start {
                    segments.push((seg_start, i, seg_x));
                }
                seg_start = i + 1;
                // carriage returns are dropped; x unchanged
            }
            _ => {}
        }
    }
    if line.len() > seg_start {
        segments.push((seg_start, line.len(), seg_x));
    }
    let total = segments
        .last()
        .map(|&(s, e, sx)| sx + font.text_width_um(&line[s..e], size_um))
        .unwrap_or(0);
    (segments, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vsd_core::tree::{Link, Span};

    #[test]
    fn whitespace_collapses_and_links_remap() {
        let inlines = vec![
            Inline::Text("  Hello \t\n world ".into()),
            Inline::Link(Link {
                href: "https://example.com".into(),
                children: vec![Inline::Text("a  link".into())],
            }),
            Inline::Span(Span {
                style: None,
                children: vec![Inline::Text("  tail".into())],
            }),
        ];
        let lt = layout_text(&inlines);
        assert_eq!(lt.text, "Hello world a link tail");
        assert_eq!(lt.links.len(), 1);
        let (s, e) = lt.links[0];
        assert_eq!(&lt.text[s..e], "a link");
    }

    #[test]
    fn greedy_breaking_fills_lines() {
        let font = FontMetrics::get();
        let size = 3881;
        let text = "aaa bbb ccc";
        let w_space = font.space_advance_um(size);
        // Width fits exactly the first two words.
        let max = font.text_width_um("aaa", size) + w_space + font.text_width_um("bbb", size);
        let lines = break_lines(font, text, size, max);
        assert_eq!(lines.len(), 2);
        assert_eq!(&text[lines[0].start..lines[0].end], "aaa bbb");
        assert_eq!(&text[lines[1].start..lines[1].end], "ccc");
    }

    #[test]
    fn oversized_word_force_breaks() {
        let font = FontMetrics::get();
        let size = 3881;
        let text = "abcdefgh";
        let max = font.text_width_um("abc", size); // ~3 glyphs per line
        let lines = break_lines(font, text, size, max);
        assert!(lines.len() >= 2);
        // Every line has at least one glyph and no line exceeds max.
        for l in &lines {
            assert!(l.start < l.end);
            assert!(l.width_um <= max, "line overflows");
        }
        // Concatenation reproduces the word.
        let joined: String = lines.iter().map(|l| &text[l.start..l.end]).collect();
        assert_eq!(joined, text);
    }

    #[test]
    fn code_tabs_advance_to_stops() {
        let font = FontMetrics::get();
        let size = 3528;
        let tab_w = font.space_advance_um(size) * 4;
        let (segs, _) = measure_code_line(font, "a\tb", size);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].2, 0);
        assert_eq!(segs[1].2, tab_w); // 'a' is narrower than one tab stop
    }
}
