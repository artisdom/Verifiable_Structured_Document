//! Block layout and pagination (LAYOUT-1.0.md §6–§9).
//!
//! Internally everything is integer micrometers; ops convert to the
//! display list's mm/pt floats only at emission (§1).

use std::collections::BTreeMap;

use vsd_core::document::Document;
use vsd_core::forms::FieldValue;
use vsd_core::layout::{Color, DisplayOp, Page};
use vsd_core::manifest::Blob;
use vsd_core::tree::{Node, Row, Table};

use crate::font::{muldiv, Face, FontMetrics};
use crate::hyphen::Hyphenator;
use crate::text::{break_lines_hyphenated, layout_text, measure_code_line, LayoutText, Line};
use crate::{LayoutError, Result};

// --- Normative constants (LAYOUT-1.0.md §6) --------------------------------

const MARGIN: i64 = 20_000;
const SIZE_BODY: i64 = 3881;
const SIZE_H: [i64; 6] = [8467, 6350, 4939, 4233, 3881, 3528];
const SIZE_CODE: i64 = 3528;
const SIZE_CAPTION: i64 = 3175;
const SPACE_AFTER: i64 = 2117;
const SPACE_BEFORE_H: i64 = 4233;
const SPACE_BEFORE_H1: i64 = 6350;
const LIST_INDENT: i64 = 7_000;
const LIST_ITEM_GAP: i64 = 1058;
const CELL_PAD: i64 = 1000;
const RULE: i64 = 100;
const FIELD_BLANK_W: i64 = 30_000;
const CAPTION_GAP: i64 = 1058;

const BLACK: Color = [0x00, 0x00, 0x00, 0xff];
const LINK_BLUE: Color = [0x1a, 0x0d, 0xab, 0xff];
const HEADER_BG: Color = [0xf0, 0xf0, 0xf0, 0xff];

/// Page geometry in µm. Default A4.
/// Which normative contract to lay out under. Documents pin the
/// version in their render cache; old caches stay verifiable forever.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EngineVersion {
    /// LAYOUT-1.0.md — single face; style spans affect nothing.
    V1_0,
    /// LAYOUT-1.1.md — bold/italic/bold-italic faces honored from the
    /// style table; everything else identical to 1.0.
    V1_1,
    /// LAYOUT-1.2.md — + monospace (code and `mono` spans), underline
    /// rendering, justified body paragraphs, and RTL/bidi for
    /// non-joining scripts (Hebrew). Joining scripts and CJK are
    /// refused, never mis-rendered.
    V1_2,
    /// LAYOUT-1.3.md — page furniture: Knuth–Liang hyphenation of
    /// English body text (pinned en-US patterns) and widow/orphan
    /// control in pagination. Everything else identical to 1.2.
    V1_3,
    /// LAYOUT-1.4.md — shaped complex scripts: Arabic and Devanagari are
    /// shaped by the pinned pure-Rust HarfBuzz port and emitted as
    /// positioned `GlyphRun`s (format 0.4). Everything else identical to
    /// 1.3; CJK / Thai / other Indic scripts are still refused.
    V1_4,
}

impl EngineVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            EngineVersion::V1_0 => "1.0.0",
            EngineVersion::V1_1 => "1.1.0",
            EngineVersion::V1_2 => "1.2.0",
            EngineVersion::V1_3 => "1.3.0",
            EngineVersion::V1_4 => "1.4.0",
        }
    }

    pub fn parse(s: &str) -> Option<EngineVersion> {
        match s {
            "1.0.0" => Some(EngineVersion::V1_0),
            "1.1.0" => Some(EngineVersion::V1_1),
            "1.2.0" => Some(EngineVersion::V1_2),
            "1.3.0" => Some(EngineVersion::V1_3),
            "1.4.0" => Some(EngineVersion::V1_4),
            _ => None,
        }
    }

    fn style_policy(self) -> crate::text::StylePolicy {
        match self {
            EngineVersion::V1_0 => crate::text::StylePolicy::V1_0,
            EngineVersion::V1_1 => crate::text::StylePolicy::V1_1,
            // 1.3 adds no new style flags; it reuses the 1.2 policy.
            EngineVersion::V1_2 | EngineVersion::V1_3 => crate::text::StylePolicy::V1_2,
            EngineVersion::V1_4 => crate::text::StylePolicy::V1_4,
        }
    }

    fn justify(self) -> bool {
        matches!(
            self,
            EngineVersion::V1_2 | EngineVersion::V1_3 | EngineVersion::V1_4
        )
    }

    fn bidi(self) -> bool {
        matches!(
            self,
            EngineVersion::V1_2 | EngineVersion::V1_3 | EngineVersion::V1_4
        )
    }

    fn mono_code(self) -> bool {
        matches!(
            self,
            EngineVersion::V1_2 | EngineVersion::V1_3 | EngineVersion::V1_4
        )
    }

    fn hyphenate(self) -> bool {
        matches!(self, EngineVersion::V1_3 | EngineVersion::V1_4)
    }

    fn widow_orphan(self) -> bool {
        matches!(self, EngineVersion::V1_3 | EngineVersion::V1_4)
    }

    /// Shape complex scripts (Arabic, Devanagari) into positioned
    /// glyphs instead of refusing them (engine 1.4).
    fn shaped(self) -> bool {
        self == EngineVersion::V1_4
    }
}

/// Scripts engine 1.2 cannot lay out faithfully. Earlier engine
/// versions keep their frozen `.notdef` behavior; 1.2 claims script
/// awareness, so it refuses instead of mis-rendering (joining scripts
/// need a real shaper; CJK needs CJK fonts — both future versions).
fn refused_script(c: char) -> Option<&'static str> {
    match c {
        '\u{0600}'..='\u{06FF}'
        | '\u{0750}'..='\u{077F}'
        | '\u{08A0}'..='\u{08FF}'
        | '\u{FB50}'..='\u{FDFF}'
        | '\u{FE70}'..='\u{FEFF}' => Some("Arabic (joining script: needs a shaper)"),
        '\u{0700}'..='\u{074F}' => Some("Syriac (joining script: needs a shaper)"),
        '\u{0900}'..='\u{0DFF}' => Some("Indic scripts (need a shaper)"),
        '\u{0E00}'..='\u{0EFF}' => Some("Thai/Lao (need dictionary line breaking)"),
        '\u{1100}'..='\u{11FF}'
        | '\u{3040}'..='\u{30FF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{AC00}'..='\u{D7AF}'
        | '\u{F900}'..='\u{FAFF}' => Some("CJK (needs CJK fonts)"),
        _ => None,
    }
}

fn is_rtl_char(c: char) -> bool {
    matches!(c,
        '\u{0590}'..='\u{05FF}' | '\u{FB1D}'..='\u{FB4F}'   // Hebrew
        | '\u{0600}'..='\u{06FF}' | '\u{0750}'..='\u{077F}' // Arabic
        | '\u{08A0}'..='\u{08FF}' | '\u{FB50}'..='\u{FDFF}'
        | '\u{FE70}'..='\u{FEFF}')
}

#[derive(Clone, Copy, Debug)]
pub struct LayoutOptions {
    pub page_width_um: i64,
    pub page_height_um: i64,
    pub engine: EngineVersion,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        LayoutOptions {
            page_width_um: 210_000,
            page_height_um: 297_000,
            engine: EngineVersion::V1_4,
        }
    }
}

impl LayoutOptions {
    pub fn letter() -> Self {
        LayoutOptions {
            page_width_um: 215_900,
            page_height_um: 279_400,
            ..Default::default()
        }
    }

    pub fn with_engine(mut self, engine: EngineVersion) -> Self {
        self.engine = engine;
        self
    }
}

// --- Internal µm op representation ------------------------------------------

#[derive(Clone)]
enum Op {
    Text {
        x: i64,
        baseline: i64, // relative to atom top
        size_um: i64,
        face: Face,
        color: Color,
        rtl: bool,
        text: String,
        path: Vec<u64>,
        range: (u64, u64),
    },
    Glyphs {
        x: i64,
        baseline: i64, // relative to atom top
        size_um: i64,
        face: Face,
        color: Color,
        glyphs: Vec<crate::shape::ShapedGlyph>,
        text: String,
        path: Vec<u64>,
        range: (u64, u64),
    },
    Rect {
        x: i64,
        y: i64,
        w: i64,
        h: i64,
        color: Color,
    },
    Image {
        x: i64,
        y: i64,
        w: i64,
        h: i64,
        res: vsd_core::ObjectId,
    },
}

impl Op {
    fn finalize(self, y_off: i64) -> DisplayOp {
        let mm = |um: i64| um as f64 / 1000.0;
        let pt = |um: i64| um as f64 * 72.0 / 25400.0;
        match self {
            Op::Text {
                x,
                baseline,
                size_um,
                face,
                color,
                rtl,
                text,
                path,
                range,
            } => DisplayOp::TextRun {
                x: mm(x),
                y: mm(y_off + baseline),
                font: face.index(),
                size_pt: pt(size_um),
                color,
                rtl,
                text,
                node_path: path,
                char_range: range,
            },
            Op::Glyphs {
                x,
                baseline,
                size_um,
                face,
                color,
                glyphs,
                text,
                path,
                range,
            } => DisplayOp::GlyphRun {
                x: mm(x),
                y: mm(y_off + baseline),
                font: face.index(),
                size_pt: pt(size_um),
                color,
                glyphs: glyphs
                    .into_iter()
                    .map(|g| vsd_core::layout::Glyph {
                        gid: g.gid,
                        x_advance: mm(g.x_advance_um),
                        x_offset: mm(g.x_offset_um),
                        y_offset: mm(g.y_offset_um),
                        cluster: g.cluster,
                    })
                    .collect(),
                text,
                node_path: path,
                char_range: range,
            },
            Op::Rect { x, y, w, h, color } => DisplayOp::Rect {
                x: mm(x),
                y: mm(y_off + y),
                w: mm(w),
                h: mm(h),
                fill: color,
            },
            Op::Image { x, y, w, h, res } => DisplayOp::Image {
                x: mm(x),
                y: mm(y_off + y),
                w: mm(w),
                h: mm(h),
                res,
            },
        }
    }
}

/// An indivisible vertical slice of a block (a text line, a table row,
/// a figure, a spacer).
#[derive(Clone)]
struct Atom {
    ops: Vec<Op>,
    height: i64,
}

impl Atom {
    fn spacer(height: i64) -> Atom {
        Atom {
            ops: Vec::new(),
            height,
        }
    }
}

/// One block, fragmented into atoms plus spacing semantics.
#[derive(Clone)]
struct Frag {
    space_before: i64,
    space_after: i64,
    atoms: Vec<Atom>,
    /// Heading rule: keep the whole block plus one body line together.
    keep_with_next: bool,
    /// Page-break hint.
    force_break: bool,
    /// The atoms are consecutive text lines of one paragraph, so
    /// widow/orphan control (engine 1.3) applies when paginating them.
    text_lines: bool,
}

impl Frag {
    fn empty() -> Frag {
        Frag {
            space_before: 0,
            space_after: 0,
            atoms: Vec::new(),
            keep_with_next: false,
            force_break: false,
            text_lines: false,
        }
    }

    fn block(atoms: Vec<Atom>) -> Frag {
        Frag {
            space_before: 0,
            space_after: SPACE_AFTER,
            atoms,
            keep_with_next: false,
            force_break: false,
            text_lines: false,
        }
    }

    fn total_height(&self) -> i64 {
        self.atoms.iter().map(|a| a.height).sum()
    }
}

// --- Incremental relayout (ROADMAP 2g) ---------------------------------------

/// A fragment cache reusable across layouts — the "per-section layout
/// fence": block fragmentation (text shaping, line breaking — the
/// expensive part) is a pure function of the block's canonical bytes
/// plus the layout inputs, so unchanged blocks are reused and a
/// one-paragraph edit re-shapes one paragraph, not the world.
/// Pagination (cheap) always re-runs, so the output is **byte-identical
/// to a from-scratch layout** — guaranteed by test, required by the
/// determinism contract.
#[derive(Default)]
pub struct LayoutSession {
    cache: std::collections::HashMap<CacheKey, Frag>,
    /// Reuse statistics for the most recent layout.
    pub hits: usize,
    pub misses: usize,
}

impl LayoutSession {
    pub fn new() -> LayoutSession {
        LayoutSession::default()
    }
}

type CacheKey = ([u8; 32], i64, i64, EngineVersion);

// --- Engine ------------------------------------------------------------------

pub(crate) struct Engine<'a> {
    doc: &'a Document,
    font: &'static FontMetrics,
    opts: LayoutOptions,
    env: BTreeMap<String, FieldValue>,
    styles: Vec<vsd_core::manifest::Style>,
    /// Fingerprint of layout inputs beyond the block itself (field
    /// environment + style table) — part of every cache key.
    inputs_fp: [u8; 32],
    /// Document base direction is right-to-left (engine 1.2+).
    base_rtl: bool,
    /// Document language is English — gates hyphenation (engine 1.3),
    /// whose pinned patterns are en-US.
    lang_en: bool,
    session: Option<&'a mut LayoutSession>,
    pages: Vec<Page>,
    cur: Vec<DisplayOp>,
    y: i64,
    page_top: bool,
    prev_after: i64,
}

pub fn layout_document(doc: &Document, opts: &LayoutOptions) -> Result<Vec<Page>> {
    layout_document_with_session(doc, opts, None)
}

/// Layout with a reusable [`LayoutSession`] fragment cache (2g). The
/// result is identical to [`layout_document`]; only the work differs.
pub fn layout_document_with_session(
    doc: &Document,
    opts: &LayoutOptions,
    mut session: Option<&mut LayoutSession>,
) -> Result<Vec<Page>> {
    let root = doc.root_node()?;
    let Node::Doc(d) = &root else {
        return Err(LayoutError::Unsupported("root must be a doc node".into()));
    };
    let base_rtl = match d.dir {
        vsd_core::tree::Direction::Ltr => false,
        vsd_core::tree::Direction::Rtl => {
            if !opts.engine.bidi() {
                return Err(LayoutError::Unsupported(
                    "dir=rtl requires engine 1.2 or later".into(),
                ));
            }
            true
        }
    };

    // Field environment: filled values + computed fields (§7 Field).
    let fields = doc.fields()?;
    let inputs = match doc.manifest.field_layer {
        Some(id) => vsd_core::forms::FilledLayer::from_value(&doc.store.get_value(&id)?)?.env(),
        None => BTreeMap::new(),
    };
    let env = vsd_core::forms::evaluate_computed(&fields, &inputs)?;
    let styles = doc.resources()?.styles;
    // Hyphenation is language-gated; the pinned patterns are en-US.
    let lang_en = d.lang.to_ascii_lowercase().starts_with("en");

    // Inputs fingerprint: anything besides the block bytes that can
    // change fragmentation must invalidate cache entries.
    let mut fp = blake3::Hasher::new();
    for (k, v) in &env {
        fp.update(k.as_bytes());
        fp.update(&[0]);
        fp.update(v.to_text().as_bytes());
        fp.update(&[1]);
    }
    for s in &styles {
        fp.update(&[
            s.bold as u8,
            s.italic as u8,
            s.underline as u8,
            s.mono as u8,
        ]);
    }
    fp.update(&[base_rtl as u8, lang_en as u8]);
    let inputs_fp = *fp.finalize().as_bytes();

    if let Some(s) = session.as_deref_mut() {
        s.hits = 0;
        s.misses = 0;
    }
    let mut eng = Engine {
        doc,
        font: FontMetrics::get(),
        opts: *opts,
        env,
        styles,
        inputs_fp,
        base_rtl,
        lang_en,
        session: session.map(|s| &mut *s),
        pages: Vec::new(),
        cur: Vec::new(),
        y: MARGIN,
        page_top: true,
        prev_after: 0,
    };
    let mut path = Vec::new();
    eng.flow_blocks(&d.children, &mut path)?;
    eng.finish();
    Ok(eng.pages)
}

impl Engine<'_> {
    fn content_width(&self) -> i64 {
        self.opts.page_width_um - 2 * MARGIN
    }

    fn limit(&self) -> i64 {
        self.opts.page_height_um - MARGIN
    }

    fn new_page(&mut self) {
        let ops = std::mem::take(&mut self.cur);
        self.pages.push(self.make_page(ops));
        self.y = MARGIN;
        self.page_top = true;
        self.prev_after = 0;
    }

    fn make_page(&self, ops: Vec<DisplayOp>) -> Page {
        Page {
            width_mm: self.opts.page_width_um as f64 / 1000.0,
            height_mm: self.opts.page_height_um as f64 / 1000.0,
            ops,
        }
    }

    fn finish(&mut self) {
        // Never emit an empty trailing page; an empty document still
        // produces exactly one (empty) page.
        if !self.cur.is_empty() || self.pages.is_empty() {
            let ops = std::mem::take(&mut self.cur);
            let page = self.make_page(ops);
            self.pages.push(page);
        }
    }

    fn flow_blocks(&mut self, blocks: &[Node], path: &mut Vec<u64>) -> Result<()> {
        for (i, block) in blocks.iter().enumerate() {
            path.push(i as u64);
            // Sections flow transparently in the page stream (their
            // children are top-level blocks); everything else fragments.
            match block {
                Node::Section(s) => {
                    let children = s.children.clone();
                    self.flow_blocks(&children, path)?
                }
                Node::SubtreeRef(id) => {
                    let sub = Node::from_value(&self.doc.store.get_value(id)?)?;
                    if let Node::Section(s) = &sub {
                        self.flow_blocks(&s.children, path)?;
                    } else {
                        let frag = self.fragment_cached(&sub, path)?;
                        self.place(frag);
                    }
                }
                other => {
                    let frag = self.fragment_cached(other, path)?;
                    self.place(frag);
                }
            }
            path.pop();
        }
        Ok(())
    }

    /// Fragment a top-level block, consulting the session's fragment
    /// cache (ROADMAP 2g). The key covers everything fragmentation
    /// depends on: the block's canonical bytes, position/width, engine
    /// version, and the inputs fingerprint (field env + style table) —
    /// so a hit is *provably* the identical result. node paths inside
    /// ops are part of the block's output, so the key also covers the
    /// path.
    fn fragment_cached(&mut self, node: &Node, path: &[u64]) -> Result<Frag> {
        let Some(_) = self.session else {
            return self.fragment(node, path);
        };
        let mut hasher = blake3::Hasher::new();
        hasher.update(&node.to_value()?.encode()?);
        hasher.update(&self.inputs_fp);
        for p in path {
            hasher.update(&p.to_le_bytes());
        }
        let key: CacheKey = (
            *hasher.finalize().as_bytes(),
            MARGIN,
            self.content_width(),
            self.opts.engine,
        );
        if let Some(session) = self.session.as_deref_mut() {
            if let Some(frag) = session.cache.get(&key) {
                session.hits += 1;
                return Ok(frag.clone());
            }
        }
        let frag = self.fragment(node, path)?;
        if let Some(session) = self.session.as_deref_mut() {
            session.misses += 1;
            session.cache.insert(key, frag.clone());
        }
        Ok(frag)
    }

    /// Place a fragmented block into the page flow (LAYOUT-1.0.md §8).
    fn place(&mut self, frag: Frag) {
        if frag.force_break {
            if !self.page_top {
                self.new_page();
            }
            return;
        }
        if frag.atoms.is_empty() {
            return;
        }
        let mut gap = if self.page_top {
            0
        } else {
            self.prev_after.max(frag.space_before)
        };

        // Keep-with-next: the block plus one body line must fit.
        if frag.keep_with_next && !self.page_top {
            let need = gap + frag.total_height() + FontMetrics::line_height_um(SIZE_BODY);
            if self.y + need > self.limit() {
                self.new_page();
                gap = 0;
            }
        }

        // Widow/orphan control (engine 1.3): paginate paragraph lines so
        // a page break never strands a single line.
        if self.opts.engine.widow_orphan() && frag.text_lines && frag.atoms.len() >= 2 {
            self.place_text_lines(frag, gap);
            return;
        }

        for atom in frag.atoms {
            if !self.page_top && self.y + gap + atom.height > self.limit() {
                self.new_page();
                gap = 0;
            }
            self.y += gap;
            gap = 0;
            let y = self.y;
            self.cur
                .extend(atom.ops.into_iter().map(|op| op.finalize(y)));
            self.y += atom.height;
            self.page_top = false;
        }
        self.prev_after = frag.space_after;
    }

    /// Place the lines of one paragraph with widow/orphan control
    /// (LAYOUT-1.3.md §3): every page break keeps **at least two** lines
    /// on each side. When that is impossible without splitting (a 2- or
    /// 3-line paragraph, or one taller than a page after the break), the
    /// paragraph is moved whole to a fresh page; a paragraph taller than
    /// a full page is split as evenly as the rule allows, never looping.
    fn place_text_lines(&mut self, frag: Frag, mut gap: i64) {
        let mut atoms = frag.atoms;
        let heights: Vec<i64> = atoms.iter().map(|a| a.height).collect();
        let n = atoms.len();
        let mut i = 0usize;
        while i < n {
            let lead_gap = if self.page_top { 0 } else { gap };
            // How many of the remaining lines fit on the current page.
            let mut acc = 0i64;
            let mut fit = 0usize;
            for (off, &h) in heights[i..].iter().enumerate() {
                acc += h;
                if self.y + lead_gap + acc <= self.limit() {
                    fit = off + 1;
                } else {
                    break;
                }
            }
            let remaining = n - i;
            let take = if fit >= remaining {
                remaining
            } else {
                let mut t = fit;
                if remaining - t == 1 {
                    t = t.saturating_sub(1); // widow: keep ≥2 lines for the next page
                }
                if t < 2 {
                    // Orphan: <2 lines would stay here. On a fresh page,
                    // place what fits (the paragraph is taller than a
                    // page — unavoidable). Otherwise move it whole down.
                    if self.page_top {
                        fit.max(1)
                    } else {
                        self.new_page();
                        gap = 0;
                        continue;
                    }
                } else {
                    t
                }
            };
            let mut g = lead_gap;
            for (atom, &h) in atoms[i..i + take].iter_mut().zip(&heights[i..i + take]) {
                self.y += g;
                g = 0;
                let y = self.y;
                let ops = core::mem::take(&mut atom.ops);
                self.cur.extend(ops.into_iter().map(|op| op.finalize(y)));
                self.y += h;
                self.page_top = false;
            }
            i += take;
            if i < n {
                self.new_page();
                gap = 0;
            }
        }
        self.prev_after = frag.space_after;
    }

    /// Fragment a block into atoms at the given left edge and width.
    fn fragment(&self, node: &Node, path: &[u64]) -> Result<Frag> {
        self.fragment_at(node, path, MARGIN, self.content_width())
    }

    /// Layout text with this engine version's style policy. Engine 1.2
    /// refuses scripts it cannot lay out faithfully; earlier engines
    /// keep their frozen `.notdef` behavior.
    fn styled_text(&self, inlines: &[vsd_core::tree::Inline]) -> Result<crate::text::LayoutText> {
        let lt = layout_text(inlines, &self.styles, self.opts.engine.style_policy());
        self.check_scripts(&lt.text)?;
        Ok(lt)
    }

    fn check_scripts(&self, text: &str) -> Result<()> {
        if !self.opts.engine.bidi() {
            return Ok(());
        }
        for c in text.chars() {
            // Engine 1.4 shapes the scripts it has a pinned font + shaper
            // for (Arabic, Devanagari); everything else it cannot set
            // faithfully is still refused, never mis-rendered.
            if self.opts.engine.shaped() && Face::shaped_for(c).is_some() {
                continue;
            }
            if let Some(what) = refused_script(c) {
                return Err(LayoutError::Unsupported(format!(
                    "engine {} cannot faithfully lay out {what}; \
                     refusing rather than mis-rendering (U+{:04X})",
                    self.opts.engine.as_str(),
                    c as u32
                )));
            }
        }
        Ok(())
    }

    fn fragment_at(&self, node: &Node, path: &[u64], x: i64, width: i64) -> Result<Frag> {
        Ok(match node {
            Node::Para(p) => {
                let lt = self.styled_text(&p.children)?;
                let mut frag = Frag::block(self.text_atoms(&lt, SIZE_BODY, x, width, path, true));
                // Body paragraphs are eligible for widow/orphan control.
                frag.text_lines = true;
                frag
            }
            Node::Heading(h) => {
                let lt = self.styled_text(&h.children)?;
                let size = SIZE_H[(h.level - 1) as usize];
                let mut frag = Frag::block(self.text_atoms(&lt, size, x, width, path, false));
                frag.space_before = if h.level == 1 {
                    SPACE_BEFORE_H1
                } else {
                    SPACE_BEFORE_H
                };
                frag.keep_with_next = true;
                frag
            }
            Node::Code(c) => Frag::block(self.code_atoms(&c.text, x, path)?),
            Node::Math(m) => match m.fallback {
                Some(res) => Frag::block(vec![self.image_atom(res, x, width)?]),
                None => Frag::block(self.code_atoms(&m.mathml, x, path)?),
            },
            Node::List(l) => Frag::block(self.list_atoms(l, path, x, width)?),
            Node::Table(t) => Frag::block(self.table_atoms(t, path, x, width)?),
            Node::Figure(f) => {
                let mut atoms = vec![self.image_atom(f.res, x, width)?];
                if !f.caption.is_empty() {
                    let lt = self.styled_text(&f.caption)?;
                    let mut caption = self.text_atoms(&lt, SIZE_CAPTION, x, width, path, false);
                    if let Some(first) = caption.first_mut() {
                        for op in &mut first.ops {
                            if let Op::Text { baseline, .. } = op {
                                *baseline += CAPTION_GAP;
                            }
                        }
                        first.height += CAPTION_GAP;
                    }
                    atoms.extend(caption);
                }
                Frag::block(atoms)
            }
            Node::Field(f) => Frag::block(vec![self.field_atom(f, x, path)?]),
            Node::Redacted(_) => {
                let h = FontMetrics::line_height_um(SIZE_BODY);
                Frag::block(vec![Atom {
                    ops: vec![Op::Rect {
                        x,
                        y: 0,
                        w: width,
                        h,
                        color: BLACK,
                    }],
                    height: h,
                }])
            }
            Node::PageBreakHint => Frag {
                force_break: true,
                ..Frag::empty()
            },
            Node::Section(s) => {
                // Sections at non-top level (inside cells/items) stack.
                Frag::block(self.stack_blocks(&s.children, path, x, width)?)
            }
            Node::SubtreeRef(id) => {
                let sub = Node::from_value(&self.doc.store.get_value(id)?)?;
                self.fragment_at(&sub, path, x, width)?
            }
            // Salt wrappers are invisible to layout and back-references.
            Node::Salted(s) => self.fragment_at(&s.child, path, x, width)?,
            Node::Doc(_) => {
                return Err(LayoutError::Unsupported(
                    "nested doc nodes are not layoutable".into(),
                ))
            }
        })
    }

    /// Text block → one atom per line, runs split at link, face,
    /// underline, and script boundaries. Baseline metrics always come
    /// from the Regular face (LAYOUT-1.1.md: faces change advances,
    /// never vertical rhythm).
    ///
    /// Engine 1.2 (LAYOUT-1.2.md): `justify` requests full
    /// justification (body paragraphs only); lines containing RTL
    /// characters — or any line of a `dir=rtl` document — are ordered
    /// by UAX #9 and never justified.
    fn text_atoms(
        &self,
        lt: &LayoutText,
        size_um: i64,
        x_left: i64,
        width: i64,
        path: &[u64],
        justify: bool,
    ) -> Vec<Atom> {
        let line_h = FontMetrics::line_height_um(size_um);
        let ascent = self.font.ascent_um(size_um);
        let bidi = if self.opts.engine.bidi() && (self.base_rtl || lt.text.chars().any(is_rtl_char))
        {
            Some(unicode_bidi::BidiInfo::new(
                &lt.text,
                Some(if self.base_rtl {
                    unicode_bidi::Level::rtl()
                } else {
                    unicode_bidi::Level::ltr()
                }),
            ))
        } else {
            None
        };
        // Hyphenation (engine 1.3): English body text only, and never on
        // bidi-reordered lines (the hyphen would land on the wrong
        // visual edge — deferred with the rest of complex-script work).
        let hyph = if justify && self.opts.engine.hyphenate() && self.lang_en && bidi.is_none() {
            Some(Hyphenator::en_us())
        } else {
            None
        };
        let lines = break_lines_hyphenated(lt, size_um, width.max(1), hyph);
        let justify = justify && self.opts.engine.justify();
        lines
            .iter()
            .enumerate()
            .map(|(li, line)| {
                let mut ops = Vec::new();
                if let Some(info) = &bidi {
                    self.emit_bidi_line(
                        lt, line, info, size_um, x_left, width, ascent, &mut ops, path,
                    );
                } else {
                    // The last line of a justified block stays ragged.
                    let j = justify && li + 1 < lines.len();
                    self.emit_ltr_line(lt, line, size_um, x_left, width, ascent, j, &mut ops, path);
                }
                Atom {
                    ops,
                    height: line_h,
                }
            })
            .collect()
    }

    /// Emit one left-to-right line. With `justify`, the slack
    /// `width − line_width` is distributed over the line's word gaps
    /// (integer division, remainder to the leftmost gaps).
    #[allow(clippy::too_many_arguments)]
    fn emit_ltr_line(
        &self,
        lt: &LayoutText,
        line: &Line,
        size_um: i64,
        x_left: i64,
        width: i64,
        ascent: i64,
        justify: bool,
        ops: &mut Vec<Op>,
        path: &[u64],
    ) {
        // Word-gap bonuses: byte position of each space → extra µm.
        let mut spaces: Vec<(usize, i64)> = Vec::new();
        let mut boundaries: Vec<usize> = Vec::new();
        if justify {
            let gaps: Vec<usize> = lt.text[line.start..line.end]
                .char_indices()
                .filter(|&(_, c)| c == ' ')
                .map(|(i, _)| line.start + i)
                .collect();
            let extra = width - line.width_um;
            if !gaps.is_empty() && extra > 0 {
                let n = gaps.len() as i64;
                let (per, rem) = (extra / n, extra % n);
                for (i, &b) in gaps.iter().enumerate() {
                    spaces.push((b, per + i64::from((i as i64) < rem)));
                    // Runs split after each gap so the bonus shifts the
                    // rest of the line.
                    boundaries.push(b + 1);
                }
            }
        }
        let bonus_before = |b: usize| -> i64 {
            spaces
                .iter()
                .filter(|&&(sb, _)| sb < b)
                .map(|&(_, x)| x)
                .sum()
        };
        for (s, e, is_link, face, underline) in
            segment_line_with(line.start, line.end, lt, &boundaries)
        {
            if s >= e {
                continue;
            }
            let x = x_left + lt.width_um(line.start, s, size_um) + bonus_before(s);
            let color = if is_link { LINK_BLUE } else { BLACK };
            push_segment(ops, lt, s, e, color, face, false, x, ascent, size_um, path);
            if underline {
                // The rect runs to the segment's visual end, so a
                // justified gap inside an underlined range stays solid.
                let x_end = x_left + lt.width_um(line.start, e, size_um) + bonus_before(e);
                ops.push(Op::Rect {
                    x,
                    y: ascent,
                    w: x_end - x,
                    h: RULE,
                    color,
                });
            }
        }
        // Inserted hyphen at a mid-word break (engine 1.3). It is layout
        // decoration, not source content, so it carries an empty
        // char_range at the break point (like list bullets / field
        // labels) — consumers can drop it from copy/extraction. Its
        // width was already counted in `line.width_um`, so on a
        // justified line it lands flush at the right edge.
        if line.hyphen {
            let at = line.end.saturating_sub(1);
            let face = lt.face_at(at);
            let is_link = lt.links.iter().any(|&(ls, le)| at >= ls && at < le);
            let x = x_left + lt.width_um(line.start, line.end, size_um) + bonus_before(line.end);
            ops.push(Op::Text {
                x,
                baseline: ascent,
                size_um,
                face,
                color: if is_link { LINK_BLUE } else { BLACK },
                rtl: false,
                text: "-".to_owned(),
                path: path.to_vec(),
                range: (line.end as u64, line.end as u64),
            });
        }
    }

    /// Emit one line in UAX #9 visual order. The line box is
    /// right-aligned when the document base direction is RTL; within
    /// the line, runs are placed left-to-right in visual order and RTL
    /// runs carry the display list's `rtl` flag (logical-order text,
    /// drawn right-to-left from the run's left edge).
    #[allow(clippy::too_many_arguments)]
    fn emit_bidi_line(
        &self,
        lt: &LayoutText,
        line: &Line,
        info: &unicode_bidi::BidiInfo,
        size_um: i64,
        x_left: i64,
        width: i64,
        ascent: i64,
        ops: &mut Vec<Op>,
        path: &[u64],
    ) {
        // The layout text is whitespace-collapsed (no newlines), so
        // there is exactly one bidi paragraph.
        let para = &info.paragraphs[0];
        let (levels, runs) = info.visual_runs(para, line.start..line.end);
        let mut cursor = if self.base_rtl {
            x_left + (width - line.width_um).max(0)
        } else {
            x_left
        };
        for run in runs {
            let run_rtl = levels[run.start].is_rtl();
            let mut segs = segment_line(run.start, run.end, lt);
            if run_rtl {
                // Visual order within an RTL run is reversed.
                segs.reverse();
            }
            for (s, e, is_link, face, underline) in segs {
                if s >= e {
                    continue;
                }
                let color = if is_link { LINK_BLUE } else { BLACK };
                let w = push_segment(
                    ops, lt, s, e, color, face, run_rtl, cursor, ascent, size_um, path,
                );
                if underline {
                    ops.push(Op::Rect {
                        x: cursor,
                        y: ascent,
                        w,
                        h: RULE,
                        color,
                    });
                }
                cursor += w;
            }
        }
    }

    /// Code block → verbatim lines; tabs advance to 4-space stops.
    /// Engine 1.2 sets code in the pinned mono face (advances measured
    /// in mono; baselines stay on the Regular rhythm).
    fn code_atoms(&self, text: &str, x_left: i64, path: &[u64]) -> Result<Vec<Atom>> {
        self.check_scripts(text)?;
        let size = SIZE_CODE;
        let face = if self.opts.engine.mono_code() {
            Face::Mono
        } else {
            Face::Regular
        };
        let metrics = FontMetrics::face_metrics(face);
        let line_h = FontMetrics::line_height_um(size);
        let ascent = self.font.ascent_um(size);
        let mut atoms = Vec::new();
        let mut offset = 0usize;
        for line in text.split('\n') {
            let (segments, _) = measure_code_line(metrics, line, size);
            let ops = segments
                .into_iter()
                .filter(|&(s, e, _)| s < e)
                .map(|(s, e, seg_x)| Op::Text {
                    x: x_left + seg_x,
                    baseline: ascent,
                    size_um: size,
                    face,
                    color: BLACK,
                    rtl: false,
                    text: line[s..e].to_owned(),
                    path: path.to_vec(),
                    range: ((offset + s) as u64, (offset + e) as u64),
                })
                .collect();
            atoms.push(Atom {
                ops,
                height: line_h,
            });
            offset += line.len() + 1;
        }
        Ok(atoms)
    }

    /// Stack child blocks without pagination (cells, list items, nested
    /// sections); inter-block gaps become spacer atoms.
    fn stack_blocks(
        &self,
        blocks: &[Node],
        base_path: &[u64],
        x: i64,
        width: i64,
    ) -> Result<Vec<Atom>> {
        let mut atoms = Vec::new();
        let mut prev_after: Option<i64> = None;
        for (i, b) in blocks.iter().enumerate() {
            let mut path = base_path.to_vec();
            path.push(i as u64);
            let frag = self.fragment_at(b, &path, x, width)?;
            if frag.atoms.is_empty() {
                continue;
            }
            if let Some(after) = prev_after {
                atoms.push(Atom::spacer(after.max(frag.space_before)));
            }
            prev_after = Some(frag.space_after);
            atoms.extend(frag.atoms);
        }
        Ok(atoms)
    }

    fn list_atoms(
        &self,
        l: &vsd_core::tree::List,
        path: &[u64],
        x: i64,
        width: i64,
    ) -> Result<Vec<Atom>> {
        let mut atoms = Vec::new();
        let inner_x = x + LIST_INDENT;
        let inner_w = (width - LIST_INDENT).max(1);
        for (i, item) in l.items.iter().enumerate() {
            if i > 0 {
                atoms.push(Atom::spacer(LIST_ITEM_GAP));
            }
            let mut item_path = path.to_vec();
            item_path.push(i as u64);
            let mut item_atoms = self.stack_blocks(item, &item_path, inner_x, inner_w)?;
            // The item label: attributed to the list node, range (0,0).
            let label = if l.ordered {
                format!("{}. ", i + 1)
            } else {
                "\u{2022} ".to_owned()
            };
            let label_op = Op::Text {
                x,
                baseline: self.font.ascent_um(SIZE_BODY),
                size_um: SIZE_BODY,
                face: Face::Regular,
                color: BLACK,
                rtl: false,
                text: label,
                path: path.to_vec(),
                range: (0, 0),
            };
            match item_atoms.first_mut() {
                Some(first) => first.ops.insert(0, label_op),
                None => item_atoms.push(Atom {
                    ops: vec![label_op],
                    height: FontMetrics::line_height_um(SIZE_BODY),
                }),
            }
            atoms.extend(item_atoms);
        }
        Ok(atoms)
    }

    fn table_atoms(&self, t: &Table, path: &[u64], x: i64, width: i64) -> Result<Vec<Atom>> {
        // Column widths (LAYOUT-1.0.md §7 Table).
        let weights: Vec<i64> = t
            .cols
            .iter()
            .map(|c| {
                let w = c.width.unwrap_or(1.0);
                let milli = (w * 1000.0).round() as i64;
                milli.max(1)
            })
            .collect();
        let total: i64 = weights.iter().sum();
        let n = weights.len().max(1);
        let mut col_w = Vec::with_capacity(n);
        let mut used = 0i64;
        for (i, w) in weights.iter().enumerate() {
            let cw = if i + 1 == n {
                width - used
            } else {
                muldiv(width, *w, total)
            };
            col_w.push(cw);
            used += cw;
        }
        if col_w.is_empty() {
            col_w.push(width);
        }

        let mut atoms = Vec::new();
        let mut cell_idx = 0u64;
        let head_count = t.head.len();
        for (row_i, row) in t.head.iter().chain(&t.body).chain(&t.foot).enumerate() {
            let header = row_i < head_count;
            let atom = self.row_atom(row, &mut cell_idx, path, x, &col_w, header, row_i == 0)?;
            atoms.push(atom);
        }
        Ok(atoms)
    }

    #[allow(clippy::too_many_arguments)]
    fn row_atom(
        &self,
        row: &Row,
        cell_idx: &mut u64,
        table_path: &[u64],
        x: i64,
        col_w: &[i64],
        header: bool,
        first_row: bool,
    ) -> Result<Atom> {
        let top_rule = if first_row { RULE } else { 0 };
        let total_w: i64 = col_w.iter().sum();

        // Lay out each cell's content at its absolute x.
        let mut cells: Vec<(Vec<Atom>, i64)> = Vec::new(); // (atoms, height)
        let mut cx = x;
        for (ci, cell) in row.cells.iter().enumerate() {
            let cw = col_w
                .get(ci)
                .copied()
                .unwrap_or_else(|| *col_w.last().unwrap_or(&1000));
            let mut cell_path = table_path.to_vec();
            cell_path.push(*cell_idx);
            *cell_idx += 1;
            let inner = self.stack_blocks(
                &cell.children,
                &cell_path,
                cx + CELL_PAD,
                (cw - 2 * CELL_PAD).max(1),
            )?;
            let h: i64 = inner.iter().map(|a| a.height).sum();
            cells.push((inner, h));
            cx += cw;
        }
        let row_h = cells.iter().map(|(_, h)| *h).max().unwrap_or(0) + 2 * CELL_PAD;

        let mut ops = Vec::new();
        // Header background spans the full row.
        if header {
            ops.push(Op::Rect {
                x,
                y: top_rule,
                w: total_w,
                h: row_h,
                color: HEADER_BG,
            });
        }
        // Cell content, offset below the top rule plus padding.
        for (atoms, _) in cells {
            let mut cy = top_rule + CELL_PAD;
            for atom in atoms {
                for op in atom.ops {
                    ops.push(offset_op(op, cy));
                }
                cy += atom.height;
            }
        }
        // Grid rules: horizontal above the first row and below every
        // row; verticals at every column boundary.
        if first_row {
            ops.push(Op::Rect {
                x,
                y: 0,
                w: total_w,
                h: RULE,
                color: BLACK,
            });
        }
        ops.push(Op::Rect {
            x,
            y: top_rule + row_h,
            w: total_w,
            h: RULE,
            color: BLACK,
        });
        let mut bx = x;
        for i in 0..=col_w.len() {
            ops.push(Op::Rect {
                x: if i == col_w.len() { bx - RULE } else { bx },
                y: top_rule,
                w: RULE,
                h: row_h,
                color: BLACK,
            });
            if i < col_w.len() {
                bx += col_w[i];
            }
        }

        Ok(Atom {
            ops,
            height: top_rule + row_h + RULE,
        })
    }
}

fn offset_op(op: Op, dy: i64) -> Op {
    match op {
        Op::Text {
            x,
            baseline,
            size_um,
            face,
            color,
            rtl,
            text,
            path,
            range,
        } => Op::Text {
            x,
            baseline: baseline + dy,
            size_um,
            face,
            color,
            rtl,
            text,
            path,
            range,
        },
        Op::Glyphs {
            x,
            baseline,
            size_um,
            face,
            color,
            glyphs,
            text,
            path,
            range,
        } => Op::Glyphs {
            x,
            baseline: baseline + dy,
            size_um,
            face,
            color,
            glyphs,
            text,
            path,
            range,
        },
        Op::Rect { x, y, w, h, color } => Op::Rect {
            x,
            y: y + dy,
            w,
            h,
            color,
        },
        Op::Image { x, y, w, h, res } => Op::Image {
            x,
            y: y + dy,
            w,
            h,
            res,
        },
    }
}

/// Emit one homogeneous segment `[s, e)` at absolute x `x`, as either a
/// simple text op or — for a shaped-script face (engine 1.4) — a
/// positioned `Op::Glyphs` run produced by the pinned shaper. Returns
/// the segment's advance width in µm (shaped width for glyph runs).
#[allow(clippy::too_many_arguments)]
fn push_segment(
    ops: &mut Vec<Op>,
    lt: &LayoutText,
    s: usize,
    e: usize,
    color: Color,
    face: Face,
    rtl: bool,
    x: i64,
    ascent: i64,
    size_um: i64,
    path: &[u64],
) -> i64 {
    if face.is_shaped() {
        let shaped = crate::shape::shape_run(face, &lt.text[s..e], size_um);
        let w = shaped.width_um;
        ops.push(Op::Glyphs {
            x,
            baseline: ascent,
            size_um,
            face,
            color,
            glyphs: shaped.glyphs,
            text: lt.text[s..e].to_owned(),
            path: path.to_vec(),
            range: (s as u64, e as u64),
        });
        w
    } else {
        ops.push(Op::Text {
            x,
            baseline: ascent,
            size_um,
            face,
            color,
            rtl,
            text: lt.text[s..e].to_owned(),
            path: path.to_vec(),
            range: (s as u64, e as u64),
        });
        lt.width_um(s, e, size_um)
    }
}

/// Split a line's byte range into (start, end, is_link, face,
/// underline) segments at every link, face, underline, and script
/// boundary, so each segment is drawable as one homogeneous run.
fn segment_line(
    start: usize,
    end: usize,
    lt: &LayoutText,
) -> Vec<(usize, usize, bool, Face, bool)> {
    segment_line_with(start, end, lt, &[])
}

fn segment_line_with(
    start: usize,
    end: usize,
    lt: &LayoutText,
    extra: &[usize],
) -> Vec<(usize, usize, bool, Face, bool)> {
    let mut bounds = vec![start, end];
    let push_range = |s: usize, e: usize, bounds: &mut Vec<usize>| {
        if s > start && s < end {
            bounds.push(s);
        }
        if e > start && e < end {
            bounds.push(e);
        }
    };
    for &(s, e) in &lt.links {
        push_range(s, e, &mut bounds);
    }
    for &(s, e, _) in &lt.faces {
        push_range(s, e, &mut bounds);
    }
    for &(s, e) in &lt.underlines {
        push_range(s, e, &mut bounds);
    }
    for &b in extra {
        if b > start && b < end {
            bounds.push(b);
        }
    }
    // Script fallback (1.2): a run carries one face index, so split at
    // every transition in or out of a fallback script.
    if lt.script_fallback {
        let mut prev: Option<Face> = None;
        for (off, c) in lt.text[start..end].char_indices() {
            let f = lt.face_for(start + off, c);
            if let Some(p) = prev {
                if f != p {
                    bounds.push(start + off);
                }
            }
            prev = Some(f);
        }
    }
    bounds.sort_unstable();
    bounds.dedup();
    bounds
        .windows(2)
        .map(|w| {
            let (s, e) = (w[0], w[1]);
            let is_link = lt.links.iter().any(|&(ls, le)| s >= ls && e <= le);
            let underline = lt.underlines.iter().any(|&(us, ue)| s >= us && e <= ue);
            let face = match lt.text[s..e].chars().next() {
                Some(c) => lt.face_for(s, c),
                None => lt.face_at(s),
            };
            (s, e, is_link, face, underline)
        })
        .collect()
}

impl Engine<'_> {
    fn image_atom(&self, res: vsd_core::ObjectId, x: i64, width: i64) -> Result<Atom> {
        // Intrinsic size: PNG IHDR at 96 dpi, else a 40×30 mm box (§7).
        let (mut w, mut h) = (40_000i64, 30_000i64);
        if let Ok(v) = self.doc.store.get_value(&res) {
            if let Ok(blob) = Blob::from_value(&v) {
                if blob.mime == "image/png" {
                    if let Some((pw, ph)) = png_dims(&blob.data) {
                        w = muldiv(pw as i64, 25400, 96);
                        h = muldiv(ph as i64, 25400, 96);
                    }
                }
            }
        }
        if w > width {
            h = muldiv(h, width, w);
            w = width;
        }
        Ok(Atom {
            ops: vec![Op::Image { x, y: 0, w, h, res }],
            height: h,
        })
    }

    fn field_atom(&self, f: &vsd_core::tree::Field, x: i64, path: &[u64]) -> Result<Atom> {
        let size = SIZE_BODY;
        let ascent = self.font.ascent_um(size);
        let line_h = FontMetrics::line_height_um(size);
        let label = format!("{}: ", f.label.as_deref().unwrap_or(&f.id));
        let label_w = self.font.text_width_um(&label, size);
        let mut ops = vec![Op::Text {
            x,
            baseline: ascent,
            size_um: size,
            face: Face::Regular,
            color: BLACK,
            rtl: false,
            text: label,
            path: path.to_vec(),
            range: (0, 0),
        }];
        let value = self.env.get(&f.id).cloned().unwrap_or(FieldValue::Empty);
        if value == FieldValue::Empty {
            ops.push(Op::Rect {
                x: x + label_w,
                y: ascent,
                w: FIELD_BLANK_W,
                h: RULE,
                color: BLACK,
            });
        } else {
            let text = value.to_text();
            self.check_scripts(&text)?;
            ops.push(Op::Text {
                x: x + label_w,
                baseline: ascent,
                size_um: size,
                face: Face::Regular,
                color: BLACK,
                rtl: false,
                text,
                path: path.to_vec(),
                range: (0, 0),
            });
        }
        Ok(Atom {
            ops,
            height: line_h,
        })
    }
}

/// PNG IHDR dimensions (width, height), if the bytes are a PNG.
fn png_dims(data: &[u8]) -> Option<(u32, u32)> {
    const MAGIC: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    if data.len() < 24 || data[..8] != MAGIC || &data[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(data[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(data[20..24].try_into().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_line_splits_on_links() {
        let lt = LayoutText {
            text: "0123456789".into(),
            links: vec![(2, 5)],
            faces: vec![(7, 9, Face::Bold)],
            underlines: vec![],
            script_fallback: false,
        };
        let segs = segment_line(0, 10, &lt);
        assert_eq!(
            segs,
            vec![
                (0, 2, false, Face::Regular, false),
                (2, 5, true, Face::Regular, false),
                (5, 7, false, Face::Regular, false),
                (7, 9, false, Face::Bold, false),
                (9, 10, false, Face::Regular, false),
            ]
        );
    }

    #[test]
    fn segment_line_splits_on_underlines_and_scripts() {
        let lt = LayoutText {
            text: "ab שלום cd".into(),
            links: vec![],
            faces: vec![],
            underlines: vec![(0, 2)],
            script_fallback: true,
        };
        let segs = segment_line(0, lt.text.len(), &lt);
        // "ab" underlined; " " regular; "שלום" Hebrew face; " cd" regular.
        assert_eq!(segs[0], (0, 2, false, Face::Regular, true));
        assert_eq!(segs[1], (2, 3, false, Face::Regular, false));
        assert_eq!(segs[2].3, Face::Hebrew);
        assert_eq!(&lt.text[segs[2].0..segs[2].1], "שלום");
        assert_eq!(segs[3].3, Face::Regular);
    }

    #[test]
    fn png_dims_parses_ihdr() {
        let mut data = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        data.extend_from_slice(&13u32.to_be_bytes());
        data.extend_from_slice(b"IHDR");
        data.extend_from_slice(&320u32.to_be_bytes());
        data.extend_from_slice(&240u32.to_be_bytes());
        assert_eq!(png_dims(&data), Some((320, 240)));
        assert_eq!(png_dims(b"not a png"), None);
    }
}
