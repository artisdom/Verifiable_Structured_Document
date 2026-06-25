//! MathML Core (subset) layout — engine 1.11 (LAYOUT-1.11.md).
//!
//! Real box-and-glue math typesetting over the pinned **STIX Two Math**
//! face and its OpenType `MATH` table. The supported element subset is
//! `math`, `mrow`, `mi`, `mn`, `mo`, `mtext`, `mspace`, `msup`, `msub`,
//! `msubsup`, `mfrac`, `msqrt`, `mroot`, `munder`, `mover`, `munderover`,
//! and `mfenced`. Anything outside the subset — `mtable`/matrices,
//! `mmultiscripts`, `menclose`, `semantics`, unknown elements or unknown
//! named entities — is **refused** (`Unsupported`), never mis-rendered.
//!
//! Everything is integer micrometers with the engine's `muldiv` rounding;
//! the only inputs are the pinned font's `MATH` table, `cmap`, `hmtx`,
//! and glyph outlines (for ink extents), so layout is deterministic.

use crate::font::{muldiv, Face, FontMetrics, GlyphId, OutlineBuilder};
use crate::{LayoutError, Result};

// --- Public output ----------------------------------------------------------

/// One positioned primitive of a laid-out formula. Coordinates are
/// micrometers relative to the formula's left edge (`x`, growing right)
/// and **baseline** (`y` above the baseline, growing up).
#[derive(Clone, Debug, PartialEq)]
pub enum MathPrim {
    /// A glyph drawn from the math face at `size_um`, its baseline `y`
    /// above the formula baseline, left side bearing origin at `x`.
    Glyph {
        x: i64,
        y: i64,
        gid: u16,
        size_um: i64,
    },
    /// A filled rectangle (fraction bar, radical rule, underbar/overbar),
    /// `(x, y)` its bottom-left corner above the formula baseline.
    Rule { x: i64, y: i64, w: i64, h: i64 },
}

/// A laid-out formula: its advance width and its extents above/below the
/// baseline, with every primitive positioned in formula space.
#[derive(Clone, Debug, PartialEq)]
pub struct MathLayout {
    pub width_um: i64,
    pub ascent_um: i64,
    pub descent_um: i64,
    pub prims: Vec<MathPrim>,
}

/// Lay out a MathML Core (subset) string at `base_size_um`.
pub fn layout_math(mathml: &str, base_size_um: i64) -> Result<MathLayout> {
    let xml = parse_xml(mathml)?;
    let node = build_node(&xml)?;
    let fm = FontMetrics::face_metrics(Face::Math);
    let ctx = MathCtx::new(fm);
    let b = ctx.layout(&node, base_size_um)?;
    Ok(MathLayout {
        width_um: b.width,
        ascent_um: b.ascent,
        descent_um: b.depth,
        prims: b.items,
    })
}

// --- A minimal, strict XML reader -------------------------------------------

#[derive(Debug)]
enum Xml {
    Elem {
        name: String,
        attrs: Vec<(String, String)>,
        children: Vec<Xml>,
    },
    Text(String),
}

fn parse_xml(s: &str) -> Result<Xml> {
    let mut p = XmlParser {
        b: s.as_bytes(),
        i: 0,
    };
    p.skip_ws_and_decls()?;
    let root = p.parse_element()?;
    p.skip_ws_and_decls()?;
    if p.i != p.b.len() {
        return Err(unsupported(
            "MathML: trailing content after the root element",
        ));
    }
    Ok(root)
}

struct XmlParser<'a> {
    b: &'a [u8],
    i: usize,
}

impl XmlParser<'_> {
    fn err(&self, msg: &str) -> LayoutError {
        unsupported(&format!("MathML parse: {msg}"))
    }

    /// Skip whitespace, XML declarations (`<?…?>`), comments (`<!--…-->`),
    /// and doctype/`<!…>` markup between elements.
    fn skip_ws_and_decls(&mut self) -> Result<()> {
        loop {
            while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.b[self.i..].starts_with(b"<?") {
                let end = find(self.b, self.i, b"?>").ok_or_else(|| self.err("unterminated <?"))?;
                self.i = end + 2;
            } else if self.b[self.i..].starts_with(b"<!--") {
                let end =
                    find(self.b, self.i, b"-->").ok_or_else(|| self.err("unterminated comment"))?;
                self.i = end + 3;
            } else if self.b[self.i..].starts_with(b"<!") {
                let end = find(self.b, self.i, b">").ok_or_else(|| self.err("unterminated <!"))?;
                self.i = end + 1;
            } else {
                return Ok(());
            }
        }
    }

    fn parse_element(&mut self) -> Result<Xml> {
        if self.i >= self.b.len() || self.b[self.i] != b'<' {
            return Err(self.err("expected '<'"));
        }
        self.i += 1;
        let name = self.read_name()?;
        let mut attrs = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(b'/') {
                self.i += 1;
                self.expect(b'>')?;
                return Ok(Xml::Elem {
                    name,
                    attrs,
                    children: Vec::new(),
                });
            }
            if self.peek() == Some(b'>') {
                self.i += 1;
                break;
            }
            let aname = self.read_name()?;
            self.skip_ws();
            self.expect(b'=')?;
            self.skip_ws();
            let aval = self.read_quoted()?;
            attrs.push((aname, aval));
        }
        // Children until the matching close tag.
        let mut children = Vec::new();
        loop {
            if self.b[self.i..].starts_with(b"</") {
                self.i += 2;
                let close = self.read_name()?;
                self.skip_ws();
                self.expect(b'>')?;
                if close != name {
                    return Err(self.err(&format!("</{close}> closes <{name}>")));
                }
                return Ok(Xml::Elem {
                    name,
                    attrs,
                    children,
                });
            } else if self.b[self.i..].starts_with(b"<?") || self.b[self.i..].starts_with(b"<!--") {
                self.skip_ws_and_decls()?;
            } else if self.peek() == Some(b'<') {
                children.push(self.parse_element()?);
            } else if self.i >= self.b.len() {
                return Err(self.err(&format!("unclosed <{name}>")));
            } else {
                children.push(Xml::Text(self.read_text()?));
            }
        }
    }

    fn read_text(&mut self) -> Result<String> {
        let start = self.i;
        while self.i < self.b.len() && self.b[self.i] != b'<' {
            self.i += 1;
        }
        decode_entities(core::str::from_utf8(&self.b[start..self.i]).map_err(|_| self.err("utf8"))?)
    }

    fn read_name(&mut self) -> Result<String> {
        let start = self.i;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b':' {
                self.i += 1;
            } else {
                break;
            }
        }
        if self.i == start {
            return Err(self.err("expected a name"));
        }
        Ok(String::from_utf8_lossy(&self.b[start..self.i]).into_owned())
    }

    fn read_quoted(&mut self) -> Result<String> {
        let q = self.peek().filter(|&c| c == b'"' || c == b'\'');
        let q = q.ok_or_else(|| self.err("expected a quoted value"))?;
        self.i += 1;
        let start = self.i;
        while self.i < self.b.len() && self.b[self.i] != q {
            self.i += 1;
        }
        if self.i >= self.b.len() {
            return Err(self.err("unterminated attribute value"));
        }
        let raw = core::str::from_utf8(&self.b[start..self.i]).map_err(|_| self.err("utf8"))?;
        self.i += 1;
        decode_entities(raw)
    }

    fn skip_ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn expect(&mut self, c: u8) -> Result<()> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected '{}'", c as char)))
        }
    }
}

fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    (from..=b.len().saturating_sub(needle.len())).find(|&j| b[j..].starts_with(needle))
}

/// Decode XML/MathML character references. Numeric (`&#9617;`,
/// `&#x221A;`) are fully supported; a small set of named entities common
/// in MathML is recognized; any other named entity is an error (refuse,
/// never mis-render).
fn decode_entities(s: &str) -> Result<String> {
    if !s.contains('&') {
        return Ok(s.to_owned());
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let semi = after
            .find(';')
            .ok_or_else(|| unsupported("MathML: unterminated entity reference"))?;
        let ent = &after[..semi];
        let ch = if let Some(hex) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
            u32::from_str_radix(hex, 16).ok()
        } else if let Some(dec) = ent.strip_prefix('#') {
            dec.parse::<u32>().ok()
        } else {
            named_entity(ent)
        }
        .and_then(char::from_u32)
        .ok_or_else(|| unsupported(&format!("MathML: unsupported entity &{ent};")))?;
        out.push(ch);
        rest = &after[semi + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The named entities a MathML document is most likely to use. Kept small
/// and explicit; unknown names are refused.
fn named_entity(name: &str) -> Option<u32> {
    Some(match name {
        "amp" => 0x26,
        "lt" => 0x3C,
        "gt" => 0x3E,
        "quot" => 0x22,
        "apos" => 0x27,
        "nbsp" => 0x00A0,
        "times" => 0x00D7,
        "divide" => 0x00F7,
        "minus" => 0x2212,
        "plusmn" | "pm" => 0x00B1,
        "mp" => 0x2213,
        "middot" | "sdot" | "CenterDot" => 0x22C5,
        "lowast" => 0x2217,
        "deg" => 0x00B0,
        "prime" => 0x2032,
        "alpha" => 0x03B1,
        "beta" => 0x03B2,
        "gamma" => 0x03B3,
        "delta" => 0x03B4,
        "epsilon" | "epsi" => 0x03B5,
        "theta" => 0x03B8,
        "lambda" => 0x03BB,
        "mu" => 0x03BC,
        "pi" => 0x03C0,
        "rho" => 0x03C1,
        "sigma" => 0x03C3,
        "tau" => 0x03C4,
        "phi" => 0x03C6,
        "omega" => 0x03C9,
        "Gamma" => 0x0393,
        "Delta" => 0x0394,
        "Theta" => 0x0398,
        "Lambda" => 0x039B,
        "Pi" => 0x03A0,
        "Sigma" => 0x03A3,
        "Phi" => 0x03A6,
        "Omega" => 0x03A9,
        "infin" | "infty" => 0x221E,
        "sum" => 0x2211,
        "prod" => 0x220F,
        "int" => 0x222B,
        "radic" | "Sqrt" => 0x221A,
        "le" | "leq" => 0x2264,
        "ge" | "geq" => 0x2265,
        "ne" => 0x2260,
        "equiv" => 0x2261,
        "approx" => 0x2248,
        "rarr" | "rightarrow" => 0x2192,
        "larr" | "leftarrow" => 0x2190,
        "harr" => 0x2194,
        "forall" => 0x2200,
        "exist" => 0x2203,
        "isin" | "in" => 0x2208,
        "notin" => 0x2209,
        "sub" => 0x2282,
        "sup" => 0x2283,
        "cup" => 0x222A,
        "cap" => 0x2229,
        "InvisibleTimes" | "ImaginaryI" | "ApplyFunction" => 0x2061, // treated as zero-width
        _ => return None,
    })
}

// --- MathML element tree ----------------------------------------------------

#[derive(Debug)]
enum MNode {
    /// `mrow` / `math` — a horizontal sequence.
    Row(Vec<MNode>),
    /// `mi`, `mn`, `mo`, `mtext` — a token with a class.
    Token {
        text: String,
        cls: Cls,
    },
    /// `mspace width=…` — horizontal space, in 1/1000 em.
    Space(i64),
    Sup(Box<MNode>, Box<MNode>),
    Sub(Box<MNode>, Box<MNode>),
    SubSup(Box<MNode>, Box<MNode>, Box<MNode>),
    Frac(Box<MNode>, Box<MNode>),
    Sqrt(Box<MNode>),
    Root(Box<MNode>, Box<MNode>),
    Under(Box<MNode>, Box<MNode>),
    Over(Box<MNode>, Box<MNode>),
    UnderOver(Box<MNode>, Box<MNode>, Box<MNode>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cls {
    Ident,
    Number,
    Op,
    Text,
}

fn build_node(x: &Xml) -> Result<MNode> {
    let Xml::Elem {
        name,
        attrs,
        children,
    } = x
    else {
        return Err(unsupported("MathML: unexpected top-level text"));
    };
    // Strip an optional namespace prefix (e.g. `m:mi`).
    let tag = name.rsplit(':').next().unwrap_or(name);
    match tag {
        "math" | "mrow" | "mstyle" | "mpadded" => Ok(MNode::Row(build_children(children)?)),
        "mi" => Ok(token(children, Cls::Ident)?),
        "mn" => Ok(token(children, Cls::Number)?),
        "mo" => Ok(token(children, Cls::Op)?),
        "mtext" => Ok(token(children, Cls::Text)?),
        "mspace" => {
            let w = attr(attrs, "width").and_then(parse_em_1000).unwrap_or(0);
            Ok(MNode::Space(w))
        }
        "msup" => {
            let (b, s) = two(children, tag)?;
            Ok(MNode::Sup(Box::new(b), Box::new(s)))
        }
        "msub" => {
            let (b, s) = two(children, tag)?;
            Ok(MNode::Sub(Box::new(b), Box::new(s)))
        }
        "msubsup" => {
            let (b, sb, sp) = three(children, tag)?;
            Ok(MNode::SubSup(Box::new(b), Box::new(sb), Box::new(sp)))
        }
        "mfrac" => {
            let (n, d) = two(children, tag)?;
            Ok(MNode::Frac(Box::new(n), Box::new(d)))
        }
        "msqrt" => Ok(MNode::Sqrt(Box::new(MNode::Row(build_children(children)?)))),
        "mroot" => {
            let (b, idx) = two(children, tag)?;
            Ok(MNode::Root(Box::new(b), Box::new(idx)))
        }
        "munder" => {
            let (b, u) = two(children, tag)?;
            Ok(MNode::Under(Box::new(b), Box::new(u)))
        }
        "mover" => {
            let (b, o) = two(children, tag)?;
            Ok(MNode::Over(Box::new(b), Box::new(o)))
        }
        "munderover" => {
            let (b, u, o) = three(children, tag)?;
            Ok(MNode::UnderOver(Box::new(b), Box::new(u), Box::new(o)))
        }
        "mfenced" => build_mfenced(attrs, children),
        other => Err(unsupported(&format!(
            "MathML element <{other}> is not supported by engine 1.11; \
             refusing rather than mis-rendering"
        ))),
    }
}

fn build_children(children: &[Xml]) -> Result<Vec<MNode>> {
    children
        .iter()
        .filter(|c| !matches!(c, Xml::Text(t) if t.trim().is_empty()))
        .map(build_node)
        .collect()
}

/// `mfenced` is shorthand: open + children (separated) + close.
fn build_mfenced(attrs: &[(String, String)], children: &[Xml]) -> Result<MNode> {
    let open = attr(attrs, "open").unwrap_or("(").to_owned();
    let close = attr(attrs, "close").unwrap_or(")").to_owned();
    let sep = attr(attrs, "separators").unwrap_or(",").to_owned();
    let kids = build_children(children)?;
    let mut row = Vec::new();
    if !open.is_empty() {
        row.push(MNode::Token {
            text: open,
            cls: Cls::Op,
        });
    }
    let seps: Vec<char> = sep.chars().collect();
    for (i, k) in kids.into_iter().enumerate() {
        if i > 0 {
            let c = seps.get(i - 1).or_else(|| seps.last()).copied();
            if let Some(c) = c {
                row.push(MNode::Token {
                    text: c.to_string(),
                    cls: Cls::Op,
                });
            }
        }
        row.push(k);
    }
    if !close.is_empty() {
        row.push(MNode::Token {
            text: close,
            cls: Cls::Op,
        });
    }
    Ok(MNode::Row(row))
}

fn token(children: &[Xml], cls: Cls) -> Result<MNode> {
    let mut text = String::new();
    for c in children {
        match c {
            Xml::Text(t) => text.push_str(t),
            Xml::Elem { name, .. } => {
                return Err(unsupported(&format!(
                    "MathML: <{name}> inside a token element is not supported"
                )))
            }
        }
    }
    Ok(MNode::Token {
        text: text.trim().to_owned(),
        cls,
    })
}

fn two(children: &[Xml], tag: &str) -> Result<(MNode, MNode)> {
    let mut v = build_children(children)?;
    if v.len() != 2 {
        return Err(unsupported(&format!(
            "MathML <{tag}> requires exactly 2 children, found {}",
            v.len()
        )));
    }
    let b = v.remove(0);
    let a = v.remove(0);
    Ok((b, a))
}

fn three(children: &[Xml], tag: &str) -> Result<(MNode, MNode, MNode)> {
    let mut v = build_children(children)?;
    if v.len() != 3 {
        return Err(unsupported(&format!(
            "MathML <{tag}> requires exactly 3 children, found {}",
            v.len()
        )));
    }
    let c = v.remove(0);
    let b = v.remove(0);
    let a = v.remove(0);
    Ok((c, b, a))
}

fn attr<'a>(attrs: &'a [(String, String)], k: &str) -> Option<&'a str> {
    attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

/// Parse a MathML length given in `em` (the only unit we honor) into
/// 1/1000 em; bare numbers are treated as em. Other units → None.
fn parse_em_1000(s: &str) -> Option<i64> {
    let s = s.trim();
    let num = s.strip_suffix("em").unwrap_or(s).trim();
    let f: f64 = num.parse().ok()?;
    Some((f * 1000.0).round() as i64)
}

// --- Box layout -------------------------------------------------------------

/// A laid-out subexpression: advance `width`, `ascent`/`depth` extents,
/// and positioned items (baseline at y = 0, up positive).
struct MBox {
    width: i64,
    ascent: i64,
    depth: i64,
    items: Vec<MathPrim>,
}

impl MBox {
    fn empty() -> MBox {
        MBox {
            width: 0,
            ascent: 0,
            depth: 0,
            items: Vec::new(),
        }
    }

    /// Shift every item by (dx, dy) and grow extents accordingly.
    fn shifted(mut self, dx: i64, dy: i64) -> MBox {
        for it in &mut self.items {
            match it {
                MathPrim::Glyph { x, y, .. } => {
                    *x += dx;
                    *y += dy;
                }
                MathPrim::Rule { x, y, .. } => {
                    *x += dx;
                    *y += dy;
                }
            }
        }
        self.ascent += dy;
        self.depth -= dy;
        self
    }
}

struct MathCtx<'a> {
    fm: &'a FontMetrics,
    upem: i64,
}

impl<'a> MathCtx<'a> {
    fn new(fm: &'a FontMetrics) -> MathCtx<'a> {
        MathCtx { fm, upem: fm.upem }
    }

    /// Scale a font-design-unit value to µm at `size`.
    fn um(&self, units: i64, size: i64) -> i64 {
        muldiv(units, size, self.upem)
    }

    fn const_um(&self, value_units: i16, size: i64) -> i64 {
        self.um(value_units as i64, size)
    }

    fn constants(&self) -> Option<ttf_parser::math::Constants<'a>> {
        self.fm.face().tables().math.and_then(|m| m.constants)
    }

    fn axis_height(&self, size: i64) -> i64 {
        self.constants()
            .map(|c| self.const_um(c.axis_height().value, size))
            // Fallback: a quarter of the font size (≈ math axis).
            .unwrap_or(size / 4)
    }

    fn script_size(&self, size: i64) -> i64 {
        let pct = self
            .constants()
            .map(|c| c.script_percent_scale_down())
            .filter(|p| *p > 0)
            .unwrap_or(71);
        muldiv(size, pct as i64, 100)
    }

    fn layout(&self, node: &MNode, size: i64) -> Result<MBox> {
        match node {
            MNode::Row(items) => self.layout_row(items, size),
            MNode::Space(w_1000) => Ok(MBox {
                width: muldiv(size, *w_1000, 1000),
                ascent: 0,
                depth: 0,
                items: Vec::new(),
            }),
            MNode::Token { text, cls } => self.layout_token(text, *cls, size),
            MNode::Sup(b, s) => self.layout_scripts(b, None, Some(s), size),
            MNode::Sub(b, s) => self.layout_scripts(b, Some(s), None, size),
            MNode::SubSup(b, sb, sp) => self.layout_scripts(b, Some(sb), Some(sp), size),
            MNode::Frac(n, d) => self.layout_frac(n, d, size),
            MNode::Sqrt(r) => self.layout_radical(r, None, size),
            MNode::Root(r, idx) => self.layout_radical(r, Some(idx), size),
            MNode::Under(b, u) => self.layout_updown(b, Some(u), None, size),
            MNode::Over(b, o) => self.layout_updown(b, None, Some(o), size),
            MNode::UnderOver(b, u, o) => self.layout_updown(b, Some(u), Some(o), size),
        }
    }

    fn layout_row(&self, items: &[MNode], size: i64) -> Result<MBox> {
        let mut out = MBox::empty();
        let mut x = 0;
        for (i, it) in items.iter().enumerate() {
            // Inter-atom spacing around operators (a simplified operator
            // dictionary): a medium space on each side of a binary or
            // relational operator that sits between two operands.
            if i > 0 {
                x += self.op_space(items, i, size);
            }
            let b = self.layout(it, size)?;
            let placed = b.shifted(x, 0);
            x += self.advance_of(items, i, size, &placed);
            merge(&mut out, placed);
        }
        out.width = x.max(out.width);
        Ok(out)
    }

    /// Advance for item `i`: its box width plus any trailing operator
    /// space already accounted on the next iteration — so just the width.
    fn advance_of(&self, _items: &[MNode], _i: usize, _size: i64, placed: &MBox) -> i64 {
        placed.width
    }

    /// A medium math space (4.5/18 em ≈ `space_after_script`-scale) on
    /// each side of a binary/relational operator with operands on both
    /// sides. Returns the leading space to insert before item `i`.
    fn op_space(&self, items: &[MNode], i: usize, size: i64) -> i64 {
        let med = muldiv(size, 4, 18);
        let is_op = |n: &MNode| matches!(n, MNode::Token { cls: Cls::Op, text } if is_spaced_operator(text));
        // Space before a binary operator that has a following operand, or
        // before the operand that follows such an operator.
        let before_binop = is_op(&items[i]) && i + 1 < items.len();
        let after_binop = i >= 2 && is_op(&items[i - 1]);
        if before_binop || after_binop {
            med
        } else {
            0
        }
    }

    fn layout_token(&self, text: &str, cls: Cls, size: i64) -> Result<MBox> {
        let mut out = MBox::empty();
        let mut x = 0;
        let chars: Vec<char> = text.chars().collect();
        let italic_single = cls == Cls::Ident && chars.len() == 1 && chars[0].is_ascii_alphabetic();
        for &c in &chars {
            if c == '\u{2061}' || c == '\u{2062}' || c == '\u{2063}' {
                continue; // invisible operators: zero-width
            }
            let mapped = if italic_single { math_italic(c) } else { c };
            let gid = self.fm.glyph(mapped);
            if gid.0 == 0 && !c.is_whitespace() {
                return Err(unsupported(&format!(
                    "MathML: the math font has no glyph for U+{:04X} ({c:?}); \
                     refusing rather than mis-rendering",
                    c as u32
                )));
            }
            let adv = self.um(self.fm.advance_units(gid), size);
            let (asc, dep) = self.glyph_extents(gid, size);
            out.items.push(MathPrim::Glyph {
                x,
                y: 0,
                gid: gid.0,
                size_um: size,
            });
            out.ascent = out.ascent.max(asc);
            out.depth = out.depth.max(dep);
            x += adv;
        }
        if chars.is_empty() {
            // Empty token (e.g. <mo></mo>): zero-size, no glyph.
            return Ok(MBox::empty());
        }
        out.width = x;
        Ok(out)
    }

    /// Glyph ink extents (ascent above / depth below baseline) in µm,
    /// from the outline bounding box. Falls back to font ascent/descent.
    fn glyph_extents(&self, gid: GlyphId, size: i64) -> (i64, i64) {
        let mut bb = BBox::default();
        if self.fm.face().outline_glyph(gid, &mut bb).is_some() && bb.valid {
            let asc = self.um(bb.y_max as i64, size).max(0);
            let dep = self.um((-bb.y_min) as i64, size).max(0);
            (asc, dep)
        } else {
            (
                self.um(self.fm.ascent_units, size).max(0),
                self.um((-self.fm.descent_units).max(0), size),
            )
        }
    }

    fn layout_scripts(
        &self,
        base: &MNode,
        sub: Option<&MNode>,
        sup: Option<&MNode>,
        size: i64,
    ) -> Result<MBox> {
        let ssize = self.script_size(size);
        let base_b = self.layout(base, size)?;
        let mut out = MBox::empty();
        let mut x = 0;
        let base_w = base_b.width;
        merge(&mut out, base_b.shifted(0, 0));
        x += base_w;

        let c = self.constants();
        let gap_min = c
            .map(|c| self.const_um(c.sub_superscript_gap_min().value, size))
            .unwrap_or(self.um(self.upem / 5, size));
        let after = c
            .map(|c| self.const_um(c.space_after_script().value, size))
            .unwrap_or(size / 24);

        let mut sup_box = None;
        let mut sub_box = None;
        if let Some(s) = sup {
            let b = self.layout(s, ssize)?;
            let shift_up = c
                .map(|c| self.const_um(c.superscript_shift_up().value, size))
                .unwrap_or(muldiv(size, 7, 16));
            sup_box = Some((b, shift_up));
        }
        if let Some(s) = sub {
            let b = self.layout(s, ssize)?;
            let shift_dn = c
                .map(|c| self.const_um(c.subscript_shift_down().value, size))
                .unwrap_or(muldiv(size, 5, 16));
            sub_box = Some((b, shift_dn));
        }

        // Resolve shifts so the gap between sup bottom and sub top is at
        // least `gap_min` when both are present.
        let script_w = match (&sup_box, &sub_box) {
            (Some((sp, up)), Some((sb, dn))) => {
                let mut up = *up;
                let mut dn = *dn;
                let gap = (up - sp.depth) - (-(dn) + sb.ascent);
                if gap < gap_min {
                    let need = gap_min - gap;
                    up += need / 2;
                    dn += need - need / 2;
                }
                let w = sp.width.max(sb.width);
                merge(&mut out, sp.clone_box().shifted(x, up));
                merge(&mut out, sb.clone_box().shifted(x, -dn));
                w
            }
            (Some((sp, up)), None) => {
                merge(&mut out, sp.clone_box().shifted(x, *up));
                sp.width
            }
            (None, Some((sb, dn))) => {
                merge(&mut out, sb.clone_box().shifted(x, -*dn));
                sb.width
            }
            (None, None) => 0,
        };
        out.width = x + script_w + after;
        Ok(out)
    }

    fn layout_frac(&self, num: &MNode, den: &MNode, size: i64) -> Result<MBox> {
        let c = self.constants();
        let n = self.layout(num, size)?;
        let d = self.layout(den, size)?;
        let axis = self.axis_height(size);
        let rule = c
            .map(|c| self.const_um(c.fraction_rule_thickness().value, size))
            .filter(|t| *t > 0)
            .unwrap_or((size / 24).max(20));
        let num_gap = c
            .map(|c| self.const_um(c.fraction_numerator_gap_min().value, size))
            .unwrap_or(rule);
        let den_gap = c
            .map(|c| self.const_um(c.fraction_denominator_gap_min().value, size))
            .unwrap_or(rule);
        // The MATH baseline shifts (numerator up, denominator down).
        let table_num = c
            .map(|c| self.const_um(c.fraction_numerator_shift_up().value, size))
            .unwrap_or(0);
        let table_den = c
            .map(|c| self.const_um(c.fraction_denominator_shift_down().value, size))
            .unwrap_or(0);

        // Bar centered on the math axis.
        let rule_bottom = axis - rule / 2;
        let rule_top = rule_bottom + rule;
        // Numerator's ink bottom must clear the bar top by `num_gap`;
        // denominator's ink top must clear the bar bottom by `den_gap`.
        let num_up = table_num.max(rule_top + num_gap + n.depth);
        let den_dn = table_den.max(d.ascent + den_gap - rule_bottom);

        let width = n.width.max(d.width);
        let bar_pad = self.um(self.upem / 12, size).max(rule);
        let total_w = width + 2 * bar_pad;
        let mut out = MBox::empty();
        let n_dx = bar_pad + (width - n.width) / 2;
        let d_dx = bar_pad + (width - d.width) / 2;
        merge(&mut out, n.shifted(n_dx, num_up));
        merge(&mut out, d.shifted(d_dx, -den_dn));
        out.items.push(MathPrim::Rule {
            x: 0,
            y: rule_bottom,
            w: total_w,
            h: rule,
        });
        out.ascent = out.ascent.max(rule_top);
        out.depth = out.depth.max(-rule_bottom);
        out.width = total_w;
        Ok(out)
    }

    fn layout_radical(&self, radicand: &MNode, index: Option<&MNode>, size: i64) -> Result<MBox> {
        let c = self.constants();
        let rad = self.layout(radicand, size)?;
        let rule = c
            .map(|c| self.const_um(c.radical_rule_thickness().value, size))
            .filter(|t| *t > 0)
            .unwrap_or((size / 24).max(20));
        let gap = c
            .map(|c| self.const_um(c.radical_vertical_gap().value, size))
            .unwrap_or(rule);
        let extra = c
            .map(|c| self.const_um(c.radical_extra_ascender().value, size))
            .unwrap_or(rule);

        // The overbar rule sits `gap` above the radicand's ink top, with
        // the radicand resting on the formula baseline.
        let bar_bottom = rad.ascent + gap;
        let bar_top = bar_bottom + rule;

        // A √ sign tall enough to span from the radicand depth to the bar
        // top, selected from the MATH vertical glyph variants.
        let target_h = bar_top + rad.depth;
        let (sign_gid, sign_asc, sign_dep) = self.radical_sign(size, target_h)?;
        let sign_w = self.um(self.fm.advance_units(GlyphId(sign_gid)), size);
        // Place the sign so its ink top meets the bar top.
        let sign_baseline = bar_top - sign_asc;

        let mut out = MBox::empty();
        let mut x = 0;

        // Optional degree (mroot index): shrunk and raised into the kink.
        if let Some(idx) = index {
            let isize = self.script_size(self.script_size(size));
            let ib = self.layout(idx, isize)?;
            let kern_before = c
                .map(|c| self.const_um(c.radical_kern_before_degree().value, size))
                .unwrap_or(0);
            let kern_after = c
                .map(|c| self.const_um(c.radical_kern_after_degree().value, size))
                .unwrap_or(0);
            let raise_pct = c
                .map(|c| c.radical_degree_bottom_raise_percent() as i64)
                .unwrap_or(60);
            let sign_h = sign_asc + sign_dep;
            let idx_dy = sign_baseline - sign_dep + muldiv(sign_h, raise_pct, 100);
            let iw = ib.width;
            merge(&mut out, ib.shifted(x + kern_before, idx_dy));
            x += kern_before + iw + kern_after;
        }

        // The radical sign.
        out.items.push(MathPrim::Glyph {
            x,
            y: sign_baseline,
            gid: sign_gid,
            size_um: size,
        });
        out.depth = out
            .depth
            .max(rad.depth)
            .max(sign_dep - sign_baseline.max(0));
        x += sign_w;

        // The overbar rule across the radicand, then the radicand.
        let bar_pad = self.um(self.upem / 24, size);
        let rad_w = rad.width + bar_pad;
        out.items.push(MathPrim::Rule {
            x,
            y: bar_bottom,
            w: rad_w,
            h: rule,
        });
        merge(&mut out, rad.shifted(x + bar_pad, 0));
        out.ascent = out.ascent.max(bar_top + extra);
        out.width = x + rad_w;
        Ok(out)
    }

    /// Select a √ glyph at least `target_h` µm tall from the MATH vertical
    /// variants; returns (gid, ascent_um, depth_um).
    fn radical_sign(&self, size: i64, target_h: i64) -> Result<(u16, i64, i64)> {
        let base = self.fm.glyph('\u{221A}');
        if base.0 == 0 {
            return Err(unsupported("MathML: math font has no radical sign U+221A"));
        }
        let mut best = base;
        if let Some(variants) = self.fm.face().tables().math.and_then(|m| m.variants) {
            if let Some(vc) = variants.vertical_constructions.get(base) {
                for v in vc.variants {
                    best = v.variant_glyph;
                    let h = self.um(v.advance_measurement as i64, size);
                    if h >= target_h {
                        break;
                    }
                }
            }
        }
        let (asc, dep) = self.glyph_extents(best, size);
        Ok((best.0, asc, dep))
    }

    fn layout_updown(
        &self,
        base: &MNode,
        under: Option<&MNode>,
        over: Option<&MNode>,
        size: i64,
    ) -> Result<MBox> {
        let c = self.constants();
        let b = self.layout(base, size)?;
        let ssize = self.script_size(size);
        let mut width = b.width;
        let mut over_box = None;
        let mut under_box = None;
        if let Some(o) = over {
            let ob = self.layout(o, ssize)?;
            width = width.max(ob.width);
            over_box = Some(ob);
        }
        if let Some(u) = under {
            let ub = self.layout(u, ssize)?;
            width = width.max(ub.width);
            under_box = Some(ub);
        }
        let mut out = MBox::empty();
        let bx = (width - b.width) / 2;
        merge(&mut out, b.shifted(bx, 0));
        out.width = width;

        if let Some(ob) = over_box {
            let gap = c
                .map(|c| self.const_um(c.upper_limit_gap_min().value, size))
                .unwrap_or(size / 6);
            let dy = out.ascent + gap + ob.depth;
            let ox = (width - ob.width) / 2;
            let placed = ob.shifted(ox, dy);
            out.ascent = out.ascent.max(placed.ascent);
            merge_keep(&mut out, placed);
        }
        if let Some(ub) = under_box {
            let gap = c
                .map(|c| self.const_um(c.lower_limit_gap_min().value, size))
                .unwrap_or(size / 6);
            let dy = -(out.depth + gap + ub.ascent);
            let ux = (width - ub.width) / 2;
            let ub_depth = ub.depth;
            let placed = ub.shifted(ux, dy);
            out.depth = out.depth.max(-dy + ub_depth);
            merge_keep(&mut out, placed);
        }
        Ok(out)
    }
}

impl MBox {
    fn clone_box(&self) -> MBox {
        MBox {
            width: self.width,
            ascent: self.ascent,
            depth: self.depth,
            items: self.items.clone(),
        }
    }
}

/// Merge `b`'s items into `out`, growing `out`'s extents to cover it.
fn merge(out: &mut MBox, b: MBox) {
    out.ascent = out.ascent.max(b.ascent);
    out.depth = out.depth.max(b.depth);
    out.items.extend(b.items);
}

/// Merge without letting the child shrink the parent's extents (used when
/// the parent already set the controlling extent).
fn merge_keep(out: &mut MBox, b: MBox) {
    out.items.extend(b.items);
}

/// Operators that take medium space on each side (a tiny operator
/// dictionary — binary and relational symbols). Fences/punctuation do not.
fn is_spaced_operator(text: &str) -> bool {
    let mut it = text.chars();
    let (Some(c), None) = (it.next(), it.next()) else {
        return false;
    };
    matches!(
        c,
        '+' | '\u{2212}' // minus
            | '='
            | '<'
            | '>'
            | '\u{00D7}' // ×
            | '\u{00F7}' // ÷
            | '\u{00B1}' // ±
            | '\u{2213}' // ∓
            | '\u{22C5}' // ⋅
            | '\u{2217}' // ∗
            | '\u{2264}' // ≤
            | '\u{2265}' // ≥
            | '\u{2260}' // ≠
            | '\u{2261}' // ≡
            | '\u{2248}' // ≈
            | '\u{2192}' | '\u{2190}' | '\u{2194}'
            | '\u{2208}' | '\u{2209}'
            | '\u{222A}' | '\u{2229}'
    )
}

/// Map an ASCII letter to its Mathematical Italic codepoint (MathML's
/// default `mathvariant` for single-letter `mi`). `h` has a dedicated
/// Planck-constant slot in Unicode.
fn math_italic(c: char) -> char {
    match c {
        'h' => '\u{210E}', // PLANCK CONSTANT (ℎ) — italic h has no plane slot
        'a'..='z' => char::from_u32(0x1D44E + (c as u32 - 'a' as u32)).unwrap_or(c),
        'A'..='Z' => char::from_u32(0x1D434 + (c as u32 - 'A' as u32)).unwrap_or(c),
        _ => c,
    }
}

fn unsupported(msg: &str) -> LayoutError {
    LayoutError::Unsupported(msg.to_owned())
}

/// An [`OutlineBuilder`] that just accumulates the ink bounding box.
#[derive(Default)]
struct BBox {
    valid: bool,
    x_min: f32,
    y_min: f32,
    x_max: f32,
    y_max: f32,
}

impl BBox {
    fn pt(&mut self, x: f32, y: f32) {
        if !self.valid {
            self.valid = true;
            self.x_min = x;
            self.x_max = x;
            self.y_min = y;
            self.y_max = y;
        } else {
            self.x_min = self.x_min.min(x);
            self.x_max = self.x_max.max(x);
            self.y_min = self.y_min.min(y);
            self.y_max = self.y_max.max(y);
        }
    }
}

impl OutlineBuilder for BBox {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pt(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.pt(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.pt(x1, y1);
        self.pt(x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.pt(x1, y1);
        self.pt(x2, y2);
        self.pt(x, y);
    }
    fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: i64 = 3881;

    fn glyphs(l: &MathLayout) -> usize {
        l.prims
            .iter()
            .filter(|p| matches!(p, MathPrim::Glyph { .. }))
            .count()
    }
    fn rules(l: &MathLayout) -> usize {
        l.prims
            .iter()
            .filter(|p| matches!(p, MathPrim::Rule { .. }))
            .count()
    }

    #[test]
    fn lays_out_simple_expression() {
        let l = layout_math("<math><mi>x</mi><mo>+</mo><mn>1</mn></math>", SIZE).unwrap();
        assert!(l.width_um > 0 && l.ascent_um > 0);
        assert_eq!(glyphs(&l), 3, "x + 1 → three glyphs");
        // Operator spacing widens the row beyond the bare glyph advances.
        let bare = layout_math("<math><mi>x</mi><mn>1</mn></math>", SIZE).unwrap();
        assert!(
            l.width_um > bare.width_um,
            "binary + takes space on each side"
        );
    }

    #[test]
    fn fraction_has_a_bar_and_is_tall() {
        let frac = layout_math("<math><mfrac><mn>1</mn><mn>2</mn></mfrac></math>", SIZE).unwrap();
        assert_eq!(rules(&frac), 1, "fraction bar");
        assert_eq!(glyphs(&frac), 2);
        // A fraction stacks two rows, so it is taller than a lone digit.
        let one = layout_math("<math><mn>1</mn></math>", SIZE).unwrap();
        assert!(frac.ascent_um + frac.descent_um > one.ascent_um + one.descent_um);
    }

    #[test]
    fn superscript_raises_and_shrinks() {
        let sup = layout_math("<math><msup><mi>x</mi><mn>2</mn></msup></math>", SIZE).unwrap();
        assert_eq!(glyphs(&sup), 2);
        // The exponent sits above the base baseline.
        let max_y = sup
            .prims
            .iter()
            .filter_map(|p| match p {
                MathPrim::Glyph { y, .. } => Some(*y),
                _ => None,
            })
            .max()
            .unwrap();
        assert!(max_y > 0, "superscript is raised above the baseline");
    }

    #[test]
    fn sqrt_has_a_rule_and_sign() {
        let s = layout_math("<math><msqrt><mn>2</mn></msqrt></math>", SIZE).unwrap();
        assert_eq!(rules(&s), 1, "radical overbar");
        assert!(glyphs(&s) >= 2, "radical sign + radicand");
    }

    #[test]
    fn quadratic_formula_lays_out() {
        // x = (-b ± √(b²−4ac)) / 2a
        let m = "<math><mi>x</mi><mo>=</mo><mfrac>\
            <mrow><mo>-</mo><mi>b</mi><mo>±</mo><msqrt>\
            <mrow><msup><mi>b</mi><mn>2</mn></msup><mo>-</mo>\
            <mn>4</mn><mi>a</mi><mi>c</mi></mrow></msqrt></mrow>\
            <mrow><mn>2</mn><mi>a</mi></mrow></mfrac></math>";
        let l = layout_math(m, SIZE).unwrap();
        assert!(l.width_um > 0 && l.ascent_um > 0 && l.descent_um >= 0);
        assert!(glyphs(&l) >= 10);
        assert!(rules(&l) >= 2, "fraction bar + radical rule");
    }

    #[test]
    fn deterministic() {
        let m = "<math><mfrac><mi>a</mi><mi>b</mi></mfrac></math>";
        assert_eq!(layout_math(m, SIZE).unwrap(), layout_math(m, SIZE).unwrap());
    }

    #[test]
    fn entities_and_namespaces() {
        // Named + numeric entities, and a namespace prefix.
        let l = layout_math(
            "<m:math xmlns:m='x'><m:mn>2</m:mn><m:mo>&times;</m:mo><m:mn>3</m:mn></m:math>",
            SIZE,
        )
        .unwrap();
        assert_eq!(glyphs(&l), 3);
    }

    #[test]
    fn unsupported_element_is_refused() {
        let err = layout_math(
            "<math><mtable><mtr><mtd><mn>1</mn></mtd></mtr></mtable></math>",
            SIZE,
        );
        assert!(matches!(err, Err(LayoutError::Unsupported(_))));
    }

    #[test]
    fn unknown_entity_is_refused() {
        let err = layout_math("<math><mo>&notarealentity;</mo></math>", SIZE);
        assert!(matches!(err, Err(LayoutError::Unsupported(_))));
    }

    #[test]
    fn malformed_xml_is_refused() {
        assert!(layout_math("<math><mi>x</mo></math>", SIZE).is_err());
        assert!(layout_math("not xml at all", SIZE).is_err());
    }
}
