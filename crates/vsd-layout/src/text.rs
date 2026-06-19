//! Text preparation (LAYOUT-1.0.md §4) and line breaking (§5), face
//! attribution added by engine 1.1 (LAYOUT-1.1.md).

use vsd_core::manifest::Style;
use vsd_core::tree::Inline;

use crate::font::{Face, FontMetrics};
use crate::hyphen::Hyphenator;

/// Which style-table flags an engine version honors (each contract
/// freezes its own policy forever).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StylePolicy {
    /// Honor `b`/`i` flags (LAYOUT-1.1.md).
    pub bold_italic: bool,
    /// Honor `mono` (LAYOUT-1.2.md): overrides weight/slant.
    pub mono: bool,
    /// Collect `u` ranges for underline rects (LAYOUT-1.2.md).
    pub underline: bool,
    /// Per-character script fallback to the Hebrew face (LAYOUT-1.2.md).
    pub script_fallback: bool,
}

impl StylePolicy {
    /// Engine 1.0: styles affect nothing.
    pub const V1_0: StylePolicy = StylePolicy {
        bold_italic: false,
        mono: false,
        underline: false,
        script_fallback: false,
    };
    /// Engine 1.1: real bold/italic faces.
    pub const V1_1: StylePolicy = StylePolicy {
        bold_italic: true,
        mono: false,
        underline: false,
        script_fallback: false,
    };
    /// Engine 1.2: + mono, underline, script fallback.
    pub const V1_2: StylePolicy = StylePolicy {
        bold_italic: true,
        mono: true,
        underline: true,
        script_fallback: true,
    };
}

/// A block's layout text plus attribution ranges (byte ranges into the
/// layout text): links carry link color; face ranges carry non-regular
/// faces; underline ranges carry the `u` flag (1.2+).
pub struct LayoutText {
    pub text: String,
    pub links: Vec<(usize, usize)>,
    pub faces: Vec<(usize, usize, Face)>,
    pub underlines: Vec<(usize, usize)>,
    /// Whether width/face queries apply per-character script fallback.
    pub script_fallback: bool,
}

impl LayoutText {
    /// Style-attributed face at a byte position (Regular outside any
    /// range). Script fallback is applied per character on top of this
    /// — use [`LayoutText::face_for`] for the final face.
    pub fn face_at(&self, byte: usize) -> Face {
        self.faces
            .iter()
            .find(|(s, e, _)| (*s..*e).contains(&byte))
            .map(|(_, _, f)| *f)
            .unwrap_or(Face::Regular)
    }

    /// The face a specific character is measured and drawn with.
    pub fn face_for(&self, byte: usize, c: char) -> Face {
        let styled = self.face_at(byte);
        if self.script_fallback {
            styled.for_char(c)
        } else {
            styled
        }
    }

    /// Advance of an inserted hyphen `-` at a break ending at
    /// `before_byte`, measured in the style face of the last content
    /// character (script fallback never applies — the hyphen is ASCII).
    /// The same value is used by line breaking (to honor the width
    /// limit) and by emission (to place the glyph), so they agree.
    pub fn hyphen_um(&self, before_byte: usize, size_um: i64) -> i64 {
        let face = self.face_at(before_byte.saturating_sub(1));
        FontMetrics::face_metrics(face).char_advance_um('-', size_um)
    }

    /// Exact width of `text[start..end]` in µm: integer sum of scaled
    /// advances, each char measured in its attributed face.
    pub fn width_um(&self, start: usize, end: usize, size_um: i64) -> i64 {
        self.text[start..end]
            .char_indices()
            .filter(|(_, c)| !c.is_control())
            .map(|(off, c)| {
                FontMetrics::face_metrics(self.face_for(start + off, c)).char_advance_um(c, size_um)
            })
            .sum()
    }
}

/// Derive the layout text of inline content: concatenate in tree order,
/// collapse whitespace runs to a single space, strip leading/trailing.
/// `styles` is the document's style table, honored per `policy` (with
/// `StylePolicy::V1_0` output is byte-identical to the 1.0 contract).
pub fn layout_text(inlines: &[Inline], styles: &[Style], policy: StylePolicy) -> LayoutText {
    let mut raw = String::new();
    let mut links = Vec::new();
    let mut faces = Vec::new();
    let mut underlines = Vec::new();
    collect(
        inlines,
        false,
        (false, false, false, false),
        styles,
        policy,
        &mut raw,
        &mut links,
        &mut faces,
        &mut underlines,
    );

    // Collapse whitespace, remapping attribution ranges as we go.
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

    let remap = |s: usize, e: usize| (map[s].min(text.len()), map[e].min(text.len()));
    let links = links
        .into_iter()
        .map(|(s, e)| remap(s, e))
        .filter(|(s, e)| s < e)
        .collect();
    let faces = faces
        .into_iter()
        .map(|(s, e, f)| {
            let (s, e) = remap(s, e);
            (s, e, f)
        })
        .filter(|(s, e, _)| s < e)
        .collect();
    let underlines = underlines
        .into_iter()
        .map(|(s, e)| remap(s, e))
        .filter(|(s, e)| s < e)
        .collect();
    LayoutText {
        text,
        links,
        faces,
        underlines,
        script_fallback: policy.script_fallback,
    }
}

#[allow(clippy::too_many_arguments)]
fn collect(
    inlines: &[Inline],
    in_link: bool,
    // (bold, italic, mono, underline), OR-combined down the span stack.
    inherited: (bool, bool, bool, bool),
    styles: &[Style],
    policy: StylePolicy,
    out: &mut String,
    links: &mut Vec<(usize, usize)>,
    faces: &mut Vec<(usize, usize, Face)>,
    underlines: &mut Vec<(usize, usize)>,
) {
    for inline in inlines {
        match inline {
            Inline::Text(s) => {
                let start = out.len();
                out.push_str(s);
                if policy.bold_italic || policy.mono {
                    let face = if policy.mono {
                        Face::pick_with_mono(
                            inherited.0 && policy.bold_italic,
                            inherited.1 && policy.bold_italic,
                            inherited.2,
                        )
                    } else {
                        Face::pick(inherited.0, inherited.1)
                    };
                    if face != Face::Regular {
                        faces.push((start, out.len(), face));
                    }
                }
                if policy.underline && inherited.3 {
                    underlines.push((start, out.len()));
                }
            }
            Inline::Span(sp) => {
                let mut style_flags = inherited;
                if let Some(idx) = sp.style {
                    if let Some(style) = styles.get(idx as usize) {
                        style_flags.0 |= style.bold;
                        style_flags.1 |= style.italic;
                        style_flags.2 |= style.mono;
                        style_flags.3 |= style.underline;
                    }
                }
                collect(
                    &sp.children,
                    in_link,
                    style_flags,
                    styles,
                    policy,
                    out,
                    links,
                    faces,
                    underlines,
                );
            }
            Inline::Link(l) => {
                let start = out.len();
                collect(
                    &l.children,
                    true,
                    inherited,
                    styles,
                    policy,
                    out,
                    links,
                    faces,
                    underlines,
                );
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
    /// Measured width in µm, **including** a trailing hyphen when
    /// `hyphen` is set (so justification and right edges are correct).
    pub width_um: i64,
    /// The line ends at a mid-word hyphenation break (LAYOUT-1.3.md):
    /// emission appends a `-` glyph at the line's end.
    pub hyphen: bool,
}

/// Greedy first-fit line breaking with optional Knuth–Liang
/// hyphenation (LAYOUT-1.3.md §2). With `hyph = None` the output is
/// byte-identical to the 1.0 contract; with a hyphenator, a word that
/// overflows the current line is broken at the longest hyphenation
/// point whose prefix (plus a hyphen) still fits, before falling back
/// to moving the whole word down or — only for unhyphenatable
/// overflow — force-breaking by glyph.
pub fn break_lines_hyphenated(
    lt: &LayoutText,
    size_um: i64,
    max_width_um: i64,
    hyph: Option<&Hyphenator>,
) -> Vec<Line> {
    let mut lines = Vec::new();
    if lt.text.is_empty() {
        return lines;
    }
    let mut st = BreakState { start: 0, width: 0 };
    let mut pos = 0usize;
    for word in lt.text.split(' ') {
        let ws = pos;
        let we = pos + word.len();
        pos = we + 1; // step over the separating space
        lay_word(lt, size_um, max_width_um, hyph, ws, we, &mut lines, &mut st);
    }
    if st.width > 0 || st.start < lt.text.len() {
        lines.push(Line {
            start: st.start,
            end: lt.text.len(),
            width_um: st.width,
            hyphen: false,
        });
    }
    lines
}

/// Mutable line-building cursor: the byte offset the current (not yet
/// emitted) line starts at, and its accumulated width.
struct BreakState {
    start: usize,
    width: i64,
}

/// Place one space-free word `[ws, we)` after the current line content.
#[allow(clippy::too_many_arguments)]
fn lay_word(
    lt: &LayoutText,
    size_um: i64,
    max_width_um: i64,
    hyph: Option<&Hyphenator>,
    ws: usize,
    we: usize,
    lines: &mut Vec<Line>,
    st: &mut BreakState,
) {
    let sep_w = if st.width > 0 && ws > 0 {
        lt.width_um(ws - 1, ws, size_um)
    } else {
        0
    };
    let word_w = lt.width_um(ws, we, size_um);
    if st.width + sep_w + word_w <= max_width_um {
        st.width += sep_w + word_w;
        return;
    }
    // The word does not fit after the current content.
    if st.width > 0 {
        // Try to hyphenate a prefix onto the current line.
        if let Some(h) = hyph {
            let budget = max_width_um - st.width - sep_w;
            if let Some(rel) = best_hyphen_prefix(lt, h, ws, we, size_um, budget) {
                let bb = ws + rel;
                let content = st.width + sep_w + lt.width_um(ws, bb, size_um);
                lines.push(Line {
                    start: st.start,
                    end: bb,
                    width_um: content + lt.hyphen_um(bb, size_um),
                    hyphen: true,
                });
                st.start = bb;
                st.width = 0;
                lay_word_fresh(lt, size_um, max_width_um, hyph, bb, we, lines, st);
                return;
            }
        }
        // No hyphen prefix fit: flush the current line, place the word
        // on a fresh one.
        lines.push(Line {
            start: st.start,
            end: ws.saturating_sub(1),
            width_um: st.width,
            hyphen: false,
        });
        st.start = ws;
        st.width = 0;
    }
    lay_word_fresh(lt, size_um, max_width_um, hyph, ws, we, lines, st);
}

/// Place a word `[ws, we)` starting on an empty line, hyphenating or
/// force-breaking as needed. Leaves the trailing fragment as the new
/// current line in `st`.
#[allow(clippy::too_many_arguments)]
fn lay_word_fresh(
    lt: &LayoutText,
    size_um: i64,
    max_width_um: i64,
    hyph: Option<&Hyphenator>,
    mut ws: usize,
    we: usize,
    lines: &mut Vec<Line>,
    st: &mut BreakState,
) {
    loop {
        let word_w = lt.width_um(ws, we, size_um);
        if word_w <= max_width_um {
            st.start = ws;
            st.width = word_w;
            return;
        }
        if let Some(h) = hyph {
            if let Some(rel) = best_hyphen_prefix(lt, h, ws, we, size_um, max_width_um) {
                let bb = ws + rel;
                lines.push(Line {
                    start: ws,
                    end: bb,
                    width_um: lt.width_um(ws, bb, size_um) + lt.hyphen_um(bb, size_um),
                    hyphen: true,
                });
                ws = bb;
                continue;
            }
        }
        // Unhyphenatable overflow: force-break by glyph, ≥ 1 per line.
        let mut seg_start = ws;
        let mut seg_width = 0i64;
        for (off, c) in lt.text[ws..we].char_indices() {
            let at = ws + off;
            let cw = if c.is_control() {
                0
            } else {
                lt.width_um(at, at + c.len_utf8(), size_um)
            };
            if seg_width > 0 && seg_width + cw > max_width_um {
                lines.push(Line {
                    start: seg_start,
                    end: at,
                    width_um: seg_width,
                    hyphen: false,
                });
                seg_start = at;
                seg_width = 0;
            }
            seg_width += cw;
        }
        st.start = seg_start;
        st.width = seg_width;
        return;
    }
}

/// The largest hyphenation break (relative byte offset within
/// `[ws, we)`) whose prefix plus a hyphen fits within `budget` µm, or
/// `None` if no break point fits.
fn best_hyphen_prefix(
    lt: &LayoutText,
    hyph: &Hyphenator,
    ws: usize,
    we: usize,
    size_um: i64,
    budget: i64,
) -> Option<usize> {
    if budget <= 0 {
        return None;
    }
    let mut best = None;
    for rel in hyph.breaks(&lt.text[ws..we]) {
        let bb = ws + rel;
        if lt.width_um(ws, bb, size_um) + lt.hyphen_um(bb, size_um) <= budget {
            best = Some(rel); // breaks() ascends; keep the longest that fits
        }
    }
    best
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

    fn plain(text: &str) -> LayoutText {
        LayoutText {
            text: text.into(),
            links: vec![],
            faces: vec![],
            underlines: vec![],
            script_fallback: false,
        }
    }

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
        let lt = layout_text(&inlines, &[], StylePolicy::V1_0);
        assert_eq!(lt.text, "Hello world a link tail");
        assert_eq!(lt.links.len(), 1);
        let (s, e) = lt.links[0];
        assert_eq!(&lt.text[s..e], "a link");
        assert!(lt.faces.is_empty());
    }

    #[test]
    fn styles_attribute_faces_only_when_honored() {
        let styles = [Style {
            bold: true,
            italic: false,
            underline: false,
            mono: false,
        }];
        let inlines = vec![
            Inline::Text("plain ".into()),
            Inline::Span(Span {
                style: Some(0),
                children: vec![Inline::Text("bold".into())],
            }),
        ];
        // Engine 1.0: styles ignored, byte-identical behavior.
        let v10 = layout_text(&inlines, &styles, StylePolicy::V1_0);
        assert!(v10.faces.is_empty());
        assert_eq!(v10.face_at(7), Face::Regular);
        // Engine 1.1: the bold range is attributed and measured bolder.
        let v11 = layout_text(&inlines, &styles, StylePolicy::V1_1);
        assert_eq!(v11.text, "plain bold");
        assert_eq!(v11.face_at(7), Face::Bold);
        assert!(
            v11.width_um(6, 10, 3881) > v10.width_um(6, 10, 3881),
            "bold advances must be wider than regular"
        );
    }

    #[test]
    fn v1_2_policy_attributes_mono_underline_and_hebrew() {
        let styles = [
            Style {
                bold: false,
                italic: false,
                underline: false,
                mono: true,
            },
            Style {
                bold: true,
                italic: false,
                underline: true,
                mono: false,
            },
        ];
        let inlines = vec![
            Inline::Span(Span {
                style: Some(0),
                children: vec![Inline::Text("mono".into())],
            }),
            Inline::Text(" ".into()),
            Inline::Span(Span {
                style: Some(1),
                children: vec![Inline::Text("bu".into())],
            }),
            Inline::Text(" שלום".into()), // Hebrew via script fallback
        ];
        let lt = layout_text(&inlines, &styles, StylePolicy::V1_2);
        assert_eq!(lt.face_at(0), Face::Mono);
        assert_eq!(lt.face_at(5), Face::Bold);
        assert_eq!(lt.underlines, vec![(5, 7)]);
        // Hebrew characters resolve to the Hebrew face per char even
        // though no style range covers them.
        let heb_start = lt.text.find('ש').unwrap();
        assert_eq!(lt.face_for(heb_start, 'ש'), Face::Hebrew);
        assert_eq!(lt.face_at(heb_start), Face::Regular);
        // Mono advances are uniform; sans advances are not.
        let mono = FontMetrics::face_metrics(Face::Mono);
        assert_eq!(
            mono.char_advance_um('i', 3881),
            mono.char_advance_um('M', 3881),
            "monospace must be monospaced"
        );
        // Hebrew glyphs exist in the Hebrew face but not in the sans face.
        assert_ne!(FontMetrics::face_metrics(Face::Hebrew).glyph('ש').0, 0);
        assert_eq!(FontMetrics::get().glyph('ש').0, 0);
    }

    #[test]
    fn greedy_breaking_fills_lines() {
        let font = FontMetrics::get();
        let size = 3881;
        let lt = plain("aaa bbb ccc");
        let w_space = font.space_advance_um(size);
        // Width fits exactly the first two words.
        let max = font.text_width_um("aaa", size) + w_space + font.text_width_um("bbb", size);
        let lines = break_lines_hyphenated(&lt, size, max, None);
        assert_eq!(lines.len(), 2);
        assert_eq!(&lt.text[lines[0].start..lines[0].end], "aaa bbb");
        assert_eq!(&lt.text[lines[1].start..lines[1].end], "ccc");
    }

    #[test]
    fn oversized_word_force_breaks() {
        let font = FontMetrics::get();
        let size = 3881;
        let lt = plain("abcdefgh");
        let max = font.text_width_um("abc", size); // ~3 glyphs per line
        let lines = break_lines_hyphenated(&lt, size, max, None);
        assert!(lines.len() >= 2);
        // Every line has at least one glyph and no line exceeds max.
        for l in &lines {
            assert!(l.start < l.end);
            assert!(l.width_um <= max, "line overflows");
        }
        // Concatenation reproduces the word.
        let joined: String = lines.iter().map(|l| &lt.text[l.start..l.end]).collect();
        assert_eq!(joined, lt.text);
    }

    #[test]
    fn hyphenation_breaks_a_long_word_with_a_hyphen() {
        let font = FontMetrics::get();
        let h = Hyphenator::en_us();
        let size = 3881;
        // "establishment hyphenation" — force a width that cannot hold
        // "establishment" + the next word, so the second word hyphenates.
        let lt = plain("establishment hyphenation");
        let max = font.text_width_um("establishment hyphen", size);
        let plain_lines = break_lines_hyphenated(&lt, size, max, None);
        let hyph_lines = break_lines_hyphenated(&lt, size, max, Some(h));
        // Without hyphenation the second word moves down whole.
        assert!(plain_lines.iter().all(|l| !l.hyphen));
        // With hyphenation, a line ends at a mid-word break.
        let hy = hyph_lines
            .iter()
            .find(|l| l.hyphen)
            .expect("a hyphenated line");
        // The break is a real hyphenation point of "hyphenation".
        let word_off = lt.text.find("hyphenation").unwrap();
        let rel = hy.end - word_off;
        assert!(
            h.breaks("hyphenation").contains(&rel),
            "break at a K-L point"
        );
        // The hyphenated line (with its hyphen) never exceeds the width.
        for l in &hyph_lines {
            assert!(l.width_um <= max, "hyphenated line overflows");
        }
        // Dropping the inserted hyphen, the text is reproduced exactly.
        let joined: String = hyph_lines
            .iter()
            .map(|l| &lt.text[l.start..l.end])
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(joined.replace(' ', ""), lt.text.replace(' ', ""));
    }

    #[test]
    fn hyphenation_off_is_byte_identical_to_plain() {
        let font = FontMetrics::get();
        let h = Hyphenator::en_us();
        let size = 3881;
        // A paragraph with no hyphenatable overflow: short words that
        // never need breaking must lay out identically with or without
        // a hyphenator present.
        let lt = plain("the cat sat on a mat by the door");
        let max = font.text_width_um("the cat sat", size);
        assert_eq!(
            break_lines_hyphenated(&lt, size, max, None),
            break_lines_hyphenated(&lt, size, max, Some(h)),
        );
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
