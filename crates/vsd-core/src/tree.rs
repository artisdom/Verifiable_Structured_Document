//! The content tree — the canonical layer (spec §3).
//!
//! A typed tree of semantic nodes. This is what gets signed, extracted,
//! diffed, indexed, and read by assistive technology and machines.
//!
//! Encoding discipline: every node maps to a CBOR map with a `t`
//! discriminator. Optional fields are *omitted* when absent (never
//! encoded as null), so each logical node has exactly one encoding.
//! Decoding is strict: unknown keys and missing required keys are errors
//! in this major version.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::cbor::{MapBuilder, Value};
use crate::error::{Error, Result};
use crate::object::ObjectId;

/// Block-level node.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Doc(Doc),
    Section(Section),
    Heading(Heading),
    Para(Para),
    Table(Table),
    Figure(Figure),
    List(List),
    Code(Code),
    Math(Math),
    Field(Field),
    PageBreakHint,
    Redacted(Redacted),
    /// A subtree stored as a separate object, referenced by hash.
    /// This is what makes the tree a Merkle structure (spec §3).
    SubtreeRef(ObjectId),
    /// A salt wrapper for selective disclosure (spec §7.3, added in
    /// minor 0.2): random bytes mixed into the subtree's content
    /// address so that a *hidden* sibling's hash cannot be confirmed by
    /// hashing a guess of its content. Transparent everywhere else —
    /// layout, extraction, and validation see only the child.
    Salted(Salted),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Salted {
    /// 16–32 random bytes, generated at sealing time.
    pub salt: Vec<u8>,
    pub child: Box<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Doc {
    pub lang: String,
    pub dir: Direction,
    /// Block flow / inline direction of the document (format 0.5).
    pub writing_mode: WritingMode,
    pub children: Vec<Node>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Ltr,
    Rtl,
}

/// Document writing mode (format 0.5). `Horizontal` is `horizontal-tb`
/// (lines run left/right per `dir`, stacked top-to-bottom — the default
/// and the only mode before engine 1.8). `VerticalRl` is `vertical-rl`:
/// characters stack top-to-bottom in a column and columns advance
/// right-to-left (CJK vertical typesetting).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WritingMode {
    #[default]
    Horizontal,
    VerticalRl,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// Semantic role, e.g. "chapter", "appendix", "abstract".
    pub role: String,
    /// Number of layout columns this section's content flows into
    /// (format 0.6). `1` is the default single column and is *omitted*
    /// from the encoding, so every pre-0.6 section is byte-identical.
    /// Honored by engine 1.10+; earlier engines refuse `columns > 1`
    /// rather than mis-render. Encoded as the `cols` key.
    pub columns: u32,
    pub children: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Heading {
    pub level: u8, // 1..=6, enforced at decode and validation
    pub children: Vec<Inline>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Para {
    pub children: Vec<Inline>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub cols: Vec<ColSpec>,
    pub head: Vec<Row>,
    pub body: Vec<Row>,
    pub foot: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColSpec {
    /// Relative width weight; layout engines normalize.
    pub width: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub cells: Vec<Cell>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    /// (row span, col span); None means (1, 1).
    pub span: Option<(u32, u32)>,
    /// Real header semantics — `scope` makes "this is a header for its
    /// row/column" a structural fact, not a visual inference.
    pub scope: Option<CellScope>,
    pub children: Vec<Node>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellScope {
    Row,
    Col,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Figure {
    /// Resource object (image or vector graphic).
    pub res: ObjectId,
    /// REQUIRED. Empty only when `decorative` is true (spec §3):
    /// accessibility is a validity condition, not an afterthought.
    pub alt: String,
    pub decorative: bool,
    pub caption: Vec<Inline>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct List {
    pub ordered: bool,
    /// Each item is a sequence of block nodes.
    pub items: Vec<Vec<Node>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Code {
    pub lang: Option<String>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Math {
    /// MathML Core.
    pub mathml: String,
    /// Optional pre-rendered vector fallback.
    pub fallback: Option<ObjectId>,
}

/// Spec §6 — declarative form field. No scripts: constraints and computed
/// values are total expressions (see [`crate::forms`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub id: String,
    pub kind: FieldKind,
    pub label: Option<String>,
    pub required: bool,
    /// Validation expression; must evaluate to a boolean.
    pub constraint: Option<crate::forms::Expr>,
    /// Derived-value expression.
    pub computed: Option<crate::forms::Expr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Number,
    Date,
    Choice,
    Checkbox,
    Signature,
    Attachment,
}

impl FieldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldKind::Text => "text",
            FieldKind::Number => "number",
            FieldKind::Date => "date",
            FieldKind::Choice => "choice",
            FieldKind::Checkbox => "checkbox",
            FieldKind::Signature => "signature",
            FieldKind::Attachment => "attachment",
        }
    }

    pub fn parse(s: &str) -> Result<FieldKind> {
        Ok(match s {
            "text" => FieldKind::Text,
            "number" => FieldKind::Number,
            "date" => FieldKind::Date,
            "choice" => FieldKind::Choice,
            "checkbox" => FieldKind::Checkbox,
            "signature" => FieldKind::Signature,
            "attachment" => FieldKind::Attachment,
            other => return Err(Error::Schema(format!("unknown field kind {other:?}"))),
        })
    }
}

/// Spec §7.2 — the residue of a destructive redaction. The removed
/// subtree is gone from the store; `proof` is BLAKE3 of its canonical
/// encoding so an escrowed original can later be matched against it.
#[derive(Clone, Debug, PartialEq)]
pub struct Redacted {
    pub reason: Option<String>,
    pub proof: Option<[u8; 32]>,
}

/// Inline content.
#[derive(Clone, Debug, PartialEq)]
pub enum Inline {
    Text(String),
    Span(Span),
    Link(Link),
    Math(Math),
    FootnoteRef(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    /// Index into the style table in the resource table object.
    pub style: Option<u64>,
    pub children: Vec<Inline>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub href: String,
    pub children: Vec<Inline>,
}

// ---------------------------------------------------------------------------
// CBOR encoding
// ---------------------------------------------------------------------------

fn req<'a>(map: &'a Value, key: &str, ctx: &str) -> Result<&'a Value> {
    map.get(key)
        .ok_or_else(|| Error::Schema(format!("{ctx}: missing required key {key:?}")))
}

fn req_text(map: &Value, key: &str, ctx: &str) -> Result<String> {
    req(map, key, ctx)?
        .as_text()
        .map(str::to_owned)
        .ok_or_else(|| Error::Schema(format!("{ctx}: {key:?} must be a text string")))
}

fn opt_text(map: &Value, key: &str, ctx: &str) -> Result<Option<String>> {
    match map.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_text()
            .map(|s| Some(s.to_owned()))
            .ok_or_else(|| Error::Schema(format!("{ctx}: {key:?} must be a text string"))),
    }
}

/// Verify the decoder consumed every key (strict schema: unknown keys are
/// rejected in this major version, preserving one-encoding-per-document).
fn check_keys(map: &Value, allowed: &[&str], ctx: &str) -> Result<()> {
    for (k, _) in map.as_map().unwrap_or(&[]) {
        let key = k
            .as_text()
            .ok_or_else(|| Error::Schema(format!("{ctx}: non-text map key")))?;
        if !allowed.contains(&key) {
            return Err(Error::Schema(format!("{ctx}: unknown key {key:?}")));
        }
    }
    Ok(())
}

fn nodes_to_value(nodes: &[Node]) -> Result<Value> {
    nodes
        .iter()
        .map(Node::to_value)
        .collect::<Result<Vec<_>>>()
        .map(Value::Array)
}

fn nodes_from_value(v: &Value, ctx: &str) -> Result<Vec<Node>> {
    v.as_array()
        .ok_or_else(|| Error::Schema(format!("{ctx}: children must be an array")))?
        .iter()
        .map(Node::from_value)
        .collect()
}

fn inlines_to_value(inlines: &[Inline]) -> Result<Value> {
    inlines
        .iter()
        .map(Inline::to_value)
        .collect::<Result<Vec<_>>>()
        .map(Value::Array)
}

fn inlines_from_value(v: &Value, ctx: &str) -> Result<Vec<Inline>> {
    v.as_array()
        .ok_or_else(|| Error::Schema(format!("{ctx}: inline children must be an array")))?
        .iter()
        .map(Inline::from_value)
        .collect()
}

impl Node {
    pub fn to_value(&self) -> Result<Value> {
        Ok(match self {
            Node::Doc(d) => MapBuilder::new()
                .put("t", Value::text("doc"))
                .put("lang", Value::text(&d.lang))
                .put(
                    "dir",
                    Value::text(match d.dir {
                        Direction::Ltr => "ltr",
                        Direction::Rtl => "rtl",
                    }),
                )
                .put(
                    "wm",
                    Value::text(match d.writing_mode {
                        WritingMode::Horizontal => "htb",
                        WritingMode::VerticalRl => "vrl",
                    }),
                )
                .put("children", nodes_to_value(&d.children)?)
                .build(),
            Node::Section(s) => MapBuilder::new()
                .put("t", Value::text("sec"))
                .put("role", Value::text(&s.role))
                .put_opt(
                    "cols",
                    (s.columns > 1).then_some(Value::Unsigned(s.columns as u64)),
                )
                .put("children", nodes_to_value(&s.children)?)
                .build(),
            Node::Heading(h) => MapBuilder::new()
                .put("t", Value::text("h"))
                .put("level", Value::Unsigned(h.level as u64))
                .put("children", inlines_to_value(&h.children)?)
                .build(),
            Node::Para(p) => MapBuilder::new()
                .put("t", Value::text("p"))
                .put("children", inlines_to_value(&p.children)?)
                .build(),
            Node::Table(t) => {
                let cols = t
                    .cols
                    .iter()
                    .map(|c| {
                        MapBuilder::new()
                            .put_opt("width", c.width.map(Value::Float))
                            .build()
                    })
                    .collect();
                MapBuilder::new()
                    .put("t", Value::text("table"))
                    .put("cols", Value::Array(cols))
                    .put("head", rows_to_value(&t.head)?)
                    .put("body", rows_to_value(&t.body)?)
                    .put("foot", rows_to_value(&t.foot)?)
                    .build()
            }
            Node::Figure(fig) => MapBuilder::new()
                .put("t", Value::text("fig"))
                .put("res", fig.res.to_value())
                .put("alt", Value::text(&fig.alt))
                .put_opt(
                    "decorative",
                    if fig.decorative {
                        Some(Value::Bool(true))
                    } else {
                        None
                    },
                )
                .put("caption", inlines_to_value(&fig.caption)?)
                .build(),
            Node::List(l) => {
                let items = l
                    .items
                    .iter()
                    .map(|blocks| nodes_to_value(blocks))
                    .collect::<Result<Vec<_>>>()?;
                MapBuilder::new()
                    .put("t", Value::text("list"))
                    .put("ordered", Value::Bool(l.ordered))
                    .put("items", Value::Array(items))
                    .build()
            }
            Node::Code(c) => MapBuilder::new()
                .put("t", Value::text("code"))
                .put_opt("lang", c.lang.as_deref().map(Value::text))
                .put("text", Value::text(&c.text))
                .build(),
            Node::Math(m) => math_to_value(m),
            Node::Field(f) => MapBuilder::new()
                .put("t", Value::text("field"))
                .put("id", Value::text(&f.id))
                .put("kind", Value::text(f.kind.as_str()))
                .put_opt("label", f.label.as_deref().map(Value::text))
                .put("required", Value::Bool(f.required))
                .put_opt("constraint", f.constraint.as_ref().map(|e| e.to_value()))
                .put_opt("computed", f.computed.as_ref().map(|e| e.to_value()))
                .build(),
            Node::PageBreakHint => MapBuilder::new().put("t", Value::text("pagebreak")).build(),
            Node::Redacted(r) => MapBuilder::new()
                .put("t", Value::text("redacted"))
                .put_opt("reason", r.reason.as_deref().map(Value::text))
                .put_opt("proof", r.proof.map(|p| Value::Bytes(p.to_vec())))
                .build(),
            Node::SubtreeRef(id) => MapBuilder::new()
                .put("t", Value::text("ref"))
                .put("ref", id.to_value())
                .build(),
            Node::Salted(s) => MapBuilder::new()
                .put("t", Value::text("salted"))
                .put("salt", Value::Bytes(s.salt.clone()))
                .put("child", s.child.to_value()?)
                .build(),
        })
    }

    pub fn from_value(v: &Value) -> Result<Node> {
        let t = req_text(v, "t", "node")?;
        match t.as_str() {
            "doc" => {
                check_keys(v, &["t", "lang", "dir", "wm", "children"], "doc")?;
                let dir = match req_text(v, "dir", "doc")?.as_str() {
                    "ltr" => Direction::Ltr,
                    "rtl" => Direction::Rtl,
                    other => return Err(Error::Schema(format!("doc: bad dir {other:?}"))),
                };
                let writing_mode = match req_text(v, "wm", "doc")?.as_str() {
                    "htb" => WritingMode::Horizontal,
                    "vrl" => WritingMode::VerticalRl,
                    other => return Err(Error::Schema(format!("doc: bad wm {other:?}"))),
                };
                Ok(Node::Doc(Doc {
                    lang: req_text(v, "lang", "doc")?,
                    dir,
                    writing_mode,
                    children: nodes_from_value(req(v, "children", "doc")?, "doc")?,
                }))
            }
            "sec" => {
                check_keys(v, &["t", "role", "cols", "children"], "sec")?;
                let columns = match v.get("cols") {
                    None => 1,
                    Some(c) => {
                        let n = c.as_u64().filter(|n| (2..=64).contains(n)).ok_or_else(|| {
                            Error::Schema("sec: cols must be an integer 2..=64".into())
                        })?;
                        n as u32
                    }
                };
                Ok(Node::Section(Section {
                    role: req_text(v, "role", "sec")?,
                    columns,
                    children: nodes_from_value(req(v, "children", "sec")?, "sec")?,
                }))
            }
            "h" => {
                check_keys(v, &["t", "level", "children"], "h")?;
                let level = req(v, "level", "h")?
                    .as_u64()
                    .filter(|l| (1..=6).contains(l))
                    .ok_or_else(|| Error::Schema("h: level must be 1..=6".into()))?;
                Ok(Node::Heading(Heading {
                    level: level as u8,
                    children: inlines_from_value(req(v, "children", "h")?, "h")?,
                }))
            }
            "p" => {
                check_keys(v, &["t", "children"], "p")?;
                Ok(Node::Para(Para {
                    children: inlines_from_value(req(v, "children", "p")?, "p")?,
                }))
            }
            "table" => {
                check_keys(v, &["t", "cols", "head", "body", "foot"], "table")?;
                let cols = req(v, "cols", "table")?
                    .as_array()
                    .ok_or_else(|| Error::Schema("table: cols must be an array".into()))?
                    .iter()
                    .map(|c| {
                        check_keys(c, &["width"], "colspec")?;
                        let width = match c.get("width") {
                            None => None,
                            Some(w) => Some(w.as_f64().ok_or_else(|| {
                                Error::Schema("colspec: width must be a float".into())
                            })?),
                        };
                        Ok(ColSpec { width })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(Node::Table(Table {
                    cols,
                    head: rows_from_value(req(v, "head", "table")?)?,
                    body: rows_from_value(req(v, "body", "table")?)?,
                    foot: rows_from_value(req(v, "foot", "table")?)?,
                }))
            }
            "fig" => {
                check_keys(v, &["t", "res", "alt", "decorative", "caption"], "fig")?;
                let decorative = match v.get("decorative") {
                    None => false,
                    Some(d) => d
                        .as_bool()
                        .filter(|&b| b)
                        .ok_or_else(|| Error::Schema("fig: decorative may only be true".into()))?,
                };
                Ok(Node::Figure(Figure {
                    res: ObjectId::from_value(req(v, "res", "fig")?)?,
                    alt: req_text(v, "alt", "fig")?,
                    decorative,
                    caption: inlines_from_value(req(v, "caption", "fig")?, "fig")?,
                }))
            }
            "list" => {
                check_keys(v, &["t", "ordered", "items"], "list")?;
                let ordered = req(v, "ordered", "list")?
                    .as_bool()
                    .ok_or_else(|| Error::Schema("list: ordered must be a bool".into()))?;
                let items = req(v, "items", "list")?
                    .as_array()
                    .ok_or_else(|| Error::Schema("list: items must be an array".into()))?
                    .iter()
                    .map(|i| nodes_from_value(i, "list item"))
                    .collect::<Result<Vec<_>>>()?;
                Ok(Node::List(List { ordered, items }))
            }
            "code" => {
                check_keys(v, &["t", "lang", "text"], "code")?;
                Ok(Node::Code(Code {
                    lang: opt_text(v, "lang", "code")?,
                    text: req_text(v, "text", "code")?,
                }))
            }
            "math" => Ok(Node::Math(math_from_value(v)?)),
            "field" => {
                check_keys(
                    v,
                    &[
                        "t",
                        "id",
                        "kind",
                        "label",
                        "required",
                        "constraint",
                        "computed",
                    ],
                    "field",
                )?;
                Ok(Node::Field(Field {
                    id: req_text(v, "id", "field")?,
                    kind: FieldKind::parse(&req_text(v, "kind", "field")?)?,
                    label: opt_text(v, "label", "field")?,
                    required: req(v, "required", "field")?
                        .as_bool()
                        .ok_or_else(|| Error::Schema("field: required must be a bool".into()))?,
                    constraint: v
                        .get("constraint")
                        .map(crate::forms::Expr::from_value)
                        .transpose()?,
                    computed: v
                        .get("computed")
                        .map(crate::forms::Expr::from_value)
                        .transpose()?,
                }))
            }
            "pagebreak" => {
                check_keys(v, &["t"], "pagebreak")?;
                Ok(Node::PageBreakHint)
            }
            "redacted" => {
                check_keys(v, &["t", "reason", "proof"], "redacted")?;
                let proof = match v.get("proof") {
                    None => None,
                    Some(p) => {
                        let b = p
                            .as_bytes()
                            .ok_or_else(|| Error::Schema("redacted: proof must be bytes".into()))?;
                        Some(<[u8; 32]>::try_from(b).map_err(|_| {
                            Error::Schema("redacted: proof must be 32 bytes".into())
                        })?)
                    }
                };
                Ok(Node::Redacted(Redacted {
                    reason: opt_text(v, "reason", "redacted")?,
                    proof,
                }))
            }
            "ref" => {
                check_keys(v, &["t", "ref"], "ref")?;
                Ok(Node::SubtreeRef(ObjectId::from_value(req(
                    v, "ref", "ref",
                )?)?))
            }
            "salted" => {
                check_keys(v, &["t", "salt", "child"], "salted")?;
                let salt = req(v, "salt", "salted")?
                    .as_bytes()
                    .filter(|b| (16..=32).contains(&b.len()))
                    .ok_or_else(|| Error::Schema("salted: salt must be 16..=32 bytes".into()))?
                    .to_vec();
                Ok(Node::Salted(Salted {
                    salt,
                    child: alloc::boxed::Box::new(Node::from_value(req(v, "child", "salted")?)?),
                }))
            }
            other => Err(Error::Schema(format!("unknown node type {other:?}"))),
        }
    }
}

fn rows_to_value(rows: &[Row]) -> Result<Value> {
    rows.iter()
        .map(|r| {
            let cells = r
                .cells
                .iter()
                .map(|c| {
                    Ok(MapBuilder::new()
                        .put_opt(
                            "span",
                            c.span.map(|(r, c)| {
                                Value::Array(vec![
                                    Value::Unsigned(r as u64),
                                    Value::Unsigned(c as u64),
                                ])
                            }),
                        )
                        .put_opt(
                            "scope",
                            c.scope.map(|s| {
                                Value::text(match s {
                                    CellScope::Row => "row",
                                    CellScope::Col => "col",
                                })
                            }),
                        )
                        .put("children", nodes_to_value(&c.children)?)
                        .build())
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(MapBuilder::new().put("cells", Value::Array(cells)).build())
        })
        .collect::<Result<Vec<_>>>()
        .map(Value::Array)
}

fn rows_from_value(v: &Value) -> Result<Vec<Row>> {
    v.as_array()
        .ok_or_else(|| Error::Schema("table rows must be an array".into()))?
        .iter()
        .map(|r| {
            check_keys(r, &["cells"], "row")?;
            let cells = req(r, "cells", "row")?
                .as_array()
                .ok_or_else(|| Error::Schema("row: cells must be an array".into()))?
                .iter()
                .map(|c| {
                    check_keys(c, &["span", "scope", "children"], "cell")?;
                    let span = match c.get("span") {
                        None => None,
                        Some(s) => {
                            let a = s.as_array().filter(|a| a.len() == 2).ok_or_else(|| {
                                Error::Schema("cell: span must be [rows, cols]".into())
                            })?;
                            let rs = a[0]
                                .as_u64()
                                .filter(|&n| n >= 1 && n <= u32::MAX as u64)
                                .ok_or_else(|| Error::Schema("cell: bad row span".into()))?;
                            let cs = a[1]
                                .as_u64()
                                .filter(|&n| n >= 1 && n <= u32::MAX as u64)
                                .ok_or_else(|| Error::Schema("cell: bad col span".into()))?;
                            Some((rs as u32, cs as u32))
                        }
                    };
                    let scope = match c.get("scope") {
                        None => None,
                        Some(s) => Some(match s.as_text() {
                            Some("row") => CellScope::Row,
                            Some("col") => CellScope::Col,
                            _ => return Err(Error::Schema("cell: bad scope".into())),
                        }),
                    };
                    Ok(Cell {
                        span,
                        scope,
                        children: nodes_from_value(req(c, "children", "cell")?, "cell")?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Row { cells })
        })
        .collect()
}

fn math_to_value(m: &Math) -> Value {
    MapBuilder::new()
        .put("t", Value::text("math"))
        .put("mathml", Value::text(&m.mathml))
        .put_opt("fallback", m.fallback.map(ObjectId::to_value))
        .build()
}

fn math_from_value(v: &Value) -> Result<Math> {
    check_keys(v, &["t", "mathml", "fallback"], "math")?;
    Ok(Math {
        mathml: req_text(v, "mathml", "math")?,
        fallback: v.get("fallback").map(ObjectId::from_value).transpose()?,
    })
}

impl Inline {
    pub fn to_value(&self) -> Result<Value> {
        Ok(match self {
            Inline::Text(s) => Value::text(s),
            Inline::Span(sp) => MapBuilder::new()
                .put("t", Value::text("span"))
                .put_opt("style", sp.style.map(Value::Unsigned))
                .put("children", inlines_to_value(&sp.children)?)
                .build(),
            Inline::Link(l) => MapBuilder::new()
                .put("t", Value::text("link"))
                .put("href", Value::text(&l.href))
                .put("children", inlines_to_value(&l.children)?)
                .build(),
            Inline::Math(m) => math_to_value(m),
            Inline::FootnoteRef(id) => MapBuilder::new()
                .put("t", Value::text("fnref"))
                .put("id", Value::text(id))
                .build(),
        })
    }

    pub fn from_value(v: &Value) -> Result<Inline> {
        if let Value::Text(s) = v {
            return Ok(Inline::Text(s.clone()));
        }
        let t = req_text(v, "t", "inline")?;
        match t.as_str() {
            "span" => {
                check_keys(v, &["t", "style", "children"], "span")?;
                let style = match v.get("style") {
                    None => None,
                    Some(s) => Some(
                        s.as_u64()
                            .ok_or_else(|| Error::Schema("span: style must be a uint".into()))?,
                    ),
                };
                Ok(Inline::Span(Span {
                    style,
                    children: inlines_from_value(req(v, "children", "span")?, "span")?,
                }))
            }
            "link" => {
                check_keys(v, &["t", "href", "children"], "link")?;
                Ok(Inline::Link(Link {
                    href: req_text(v, "href", "link")?,
                    children: inlines_from_value(req(v, "children", "link")?, "link")?,
                }))
            }
            "math" => Ok(Inline::Math(math_from_value(v)?)),
            "fnref" => {
                check_keys(v, &["t", "id"], "fnref")?;
                Ok(Inline::FootnoteRef(req_text(v, "id", "fnref")?))
            }
            other => Err(Error::Schema(format!("unknown inline type {other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample_doc() -> Node {
        Node::Doc(Doc {
            lang: "en".into(),
            dir: Direction::Ltr,
            writing_mode: WritingMode::Horizontal,
            children: vec![
                Node::Heading(Heading {
                    level: 1,
                    children: vec![Inline::Text("Title".into())],
                }),
                Node::Para(Para {
                    children: vec![
                        Inline::Text("Hello ".into()),
                        Inline::Span(Span {
                            style: Some(0),
                            children: vec![Inline::Text("world".into())],
                        }),
                    ],
                }),
                Node::Table(Table {
                    cols: vec![ColSpec { width: None }, ColSpec { width: Some(2.0) }],
                    head: vec![Row {
                        cells: vec![
                            Cell {
                                span: None,
                                scope: Some(CellScope::Col),
                                children: vec![Node::Para(Para {
                                    children: vec![Inline::Text("Item".into())],
                                })],
                            },
                            Cell {
                                span: None,
                                scope: Some(CellScope::Col),
                                children: vec![Node::Para(Para {
                                    children: vec![Inline::Text("Price".into())],
                                })],
                            },
                        ],
                    }],
                    body: vec![Row {
                        cells: vec![
                            Cell {
                                span: None,
                                scope: None,
                                children: vec![Node::Para(Para {
                                    children: vec![Inline::Text("Widget".into())],
                                })],
                            },
                            Cell {
                                span: None,
                                scope: None,
                                children: vec![Node::Para(Para {
                                    children: vec![Inline::Text("4.20".into())],
                                })],
                            },
                        ],
                    }],
                    foot: vec![],
                }),
            ],
        })
    }

    #[test]
    fn node_roundtrip() {
        let doc = sample_doc();
        let v = doc.to_value().unwrap();
        let bytes = v.encode().unwrap();
        let back = Node::from_value(&crate::cbor::Value::decode(&bytes).unwrap()).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn unknown_key_rejected() {
        let v = crate::cbor::MapBuilder::new()
            .put("t", Value::text("p"))
            .put("children", Value::Array(vec![]))
            .put("evil", Value::Bool(true))
            .build();
        assert!(Node::from_value(&v).is_err());
    }

    #[test]
    fn heading_level_bounds() {
        let v = crate::cbor::MapBuilder::new()
            .put("t", Value::text("h"))
            .put("level", Value::Unsigned(7))
            .put("children", Value::Array(vec![]))
            .build();
        assert!(Node::from_value(&v).is_err());
    }
}
