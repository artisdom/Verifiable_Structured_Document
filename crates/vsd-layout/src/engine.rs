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
use crate::text::{break_lines, layout_text, measure_code_line, LayoutText};
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
}

impl EngineVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            EngineVersion::V1_0 => "1.0.0",
            EngineVersion::V1_1 => "1.1.0",
        }
    }

    pub fn parse(s: &str) -> Option<EngineVersion> {
        match s {
            "1.0.0" => Some(EngineVersion::V1_0),
            "1.1.0" => Some(EngineVersion::V1_1),
            _ => None,
        }
    }

    fn honor_styles(self) -> bool {
        self != EngineVersion::V1_0
    }
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
            engine: EngineVersion::V1_1,
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
                text,
                path,
                range,
            } => DisplayOp::TextRun {
                x: mm(x),
                y: mm(y_off + baseline),
                font: face.index(),
                size_pt: pt(size_um),
                color,
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
}

impl Frag {
    fn empty() -> Frag {
        Frag {
            space_before: 0,
            space_after: 0,
            atoms: Vec::new(),
            keep_with_next: false,
            force_break: false,
        }
    }

    fn block(atoms: Vec<Atom>) -> Frag {
        Frag {
            space_before: 0,
            space_after: SPACE_AFTER,
            atoms,
            keep_with_next: false,
            force_break: false,
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
    if d.dir != vsd_core::tree::Direction::Ltr {
        return Err(LayoutError::Unsupported(
            "vsd-layout supports dir=ltr only (rtl arrives in a later engine version)".into(),
        ));
    }

    // Field environment: filled values + computed fields (§7 Field).
    let fields = doc.fields()?;
    let inputs = match doc.manifest.field_layer {
        Some(id) => vsd_core::forms::FilledLayer::from_value(&doc.store.get_value(&id)?)?.env(),
        None => BTreeMap::new(),
    };
    let env = vsd_core::forms::evaluate_computed(&fields, &inputs)?;
    let styles = doc.resources()?.styles;

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

    /// Fragment a block into atoms at the given left edge and width.
    fn fragment(&self, node: &Node, path: &[u64]) -> Result<Frag> {
        self.fragment_at(node, path, MARGIN, self.content_width())
    }

    /// Layout text with this engine version's style policy.
    fn styled_text(&self, inlines: &[vsd_core::tree::Inline]) -> crate::text::LayoutText {
        layout_text(inlines, &self.styles, self.opts.engine.honor_styles())
    }

    fn fragment_at(&self, node: &Node, path: &[u64], x: i64, width: i64) -> Result<Frag> {
        Ok(match node {
            Node::Para(p) => {
                let lt = self.styled_text(&p.children);
                Frag::block(self.text_atoms(&lt, SIZE_BODY, x, width, path))
            }
            Node::Heading(h) => {
                let lt = self.styled_text(&h.children);
                let size = SIZE_H[(h.level - 1) as usize];
                let mut frag = Frag::block(self.text_atoms(&lt, size, x, width, path));
                frag.space_before = if h.level == 1 {
                    SPACE_BEFORE_H1
                } else {
                    SPACE_BEFORE_H
                };
                frag.keep_with_next = true;
                frag
            }
            Node::Code(c) => Frag::block(self.code_atoms(&c.text, x, path)),
            Node::Math(m) => match m.fallback {
                Some(res) => Frag::block(vec![self.image_atom(res, x, width)?]),
                None => Frag::block(self.code_atoms(&m.mathml, x, path)),
            },
            Node::List(l) => Frag::block(self.list_atoms(l, path, x, width)?),
            Node::Table(t) => Frag::block(self.table_atoms(t, path, x, width)?),
            Node::Figure(f) => {
                let mut atoms = vec![self.image_atom(f.res, x, width)?];
                if !f.caption.is_empty() {
                    let lt = self.styled_text(&f.caption);
                    let mut caption = self.text_atoms(&lt, SIZE_CAPTION, x, width, path);
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
            Node::Field(f) => Frag::block(vec![self.field_atom(f, x, path)]),
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

    /// Text block → one atom per line, runs split at link and face
    /// boundaries. Baseline metrics always come from the Regular face
    /// (LAYOUT-1.1.md: faces change advances, never vertical rhythm).
    fn text_atoms(
        &self,
        lt: &LayoutText,
        size_um: i64,
        x_left: i64,
        width: i64,
        path: &[u64],
    ) -> Vec<Atom> {
        let line_h = FontMetrics::line_height_um(size_um);
        let ascent = self.font.ascent_um(size_um);
        let lines = break_lines(lt, size_um, width.max(1));
        lines
            .iter()
            .map(|line| {
                let mut ops = Vec::new();
                for (s, e, is_link, face) in segment_line(line.start, line.end, lt) {
                    let text = &lt.text[s..e];
                    if text.is_empty() {
                        continue;
                    }
                    let x = x_left + lt.width_um(line.start, s, size_um);
                    ops.push(Op::Text {
                        x,
                        baseline: ascent,
                        size_um,
                        face,
                        color: if is_link { LINK_BLUE } else { BLACK },
                        text: text.to_owned(),
                        path: path.to_vec(),
                        range: (s as u64, e as u64),
                    });
                }
                Atom {
                    ops,
                    height: line_h,
                }
            })
            .collect()
    }

    /// Code block → verbatim lines; tabs advance to 4-space stops.
    fn code_atoms(&self, text: &str, x_left: i64, path: &[u64]) -> Vec<Atom> {
        let size = SIZE_CODE;
        let line_h = FontMetrics::line_height_um(size);
        let ascent = self.font.ascent_um(size);
        let mut atoms = Vec::new();
        let mut offset = 0usize;
        for line in text.split('\n') {
            let (segments, _) = measure_code_line(self.font, line, size);
            let ops = segments
                .into_iter()
                .filter(|&(s, e, _)| s < e)
                .map(|(s, e, seg_x)| Op::Text {
                    x: x_left + seg_x,
                    baseline: ascent,
                    size_um: size,
                    face: Face::Regular,
                    color: BLACK,
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
        atoms
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
            text,
            path,
            range,
        } => Op::Text {
            x,
            baseline: baseline + dy,
            size_um,
            face,
            color,
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

/// Split a line's byte range into (start, end, is_link, face) segments
/// at every link and face boundary.
fn segment_line(start: usize, end: usize, lt: &LayoutText) -> Vec<(usize, usize, bool, Face)> {
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
    bounds.sort_unstable();
    bounds.dedup();
    bounds
        .windows(2)
        .map(|w| {
            let (s, e) = (w[0], w[1]);
            let is_link = lt.links.iter().any(|&(ls, le)| s >= ls && e <= le);
            (s, e, is_link, lt.face_at(s))
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

    fn field_atom(&self, f: &vsd_core::tree::Field, x: i64, path: &[u64]) -> Atom {
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
            ops.push(Op::Text {
                x: x + label_w,
                baseline: ascent,
                size_um: size,
                face: Face::Regular,
                color: BLACK,
                text: value.to_text(),
                path: path.to_vec(),
                range: (0, 0),
            });
        }
        Atom {
            ops,
            height: line_h,
        }
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
        };
        let segs = segment_line(0, 10, &lt);
        assert_eq!(
            segs,
            vec![
                (0, 2, false, Face::Regular),
                (2, 5, true, Face::Regular),
                (5, 7, false, Face::Regular),
                (7, 9, false, Face::Bold),
                (9, 10, false, Face::Regular),
            ]
        );
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
