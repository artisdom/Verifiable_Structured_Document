//! Render layer types (spec §5) — display lists and the layout-engine
//! interface.
//!
//! A page is a *flat* array of positioned operations: deliberately dumber
//! than PDF content streams. No inline state machine, no transform
//! nesting, no procedural functions. Every text run carries a
//! back-reference `(node_path, char_range)` into the content tree, so
//! selection, search, and screen-reader sync are exact.
//!
//! The reference layout engine ("vsd-layout") is versioned, normative,
//! and *not yet implemented* — layout determinism across text-shaping
//! stacks is the format's hardest open problem (spec §13.1). The types
//! and the verification hook ship now so caches produced by any engine
//! are representable and structurally checkable.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::cbor::{MapBuilder, Value};
use crate::document::Document;
use crate::error::{Error, Result};
use crate::object::ObjectId;

/// One page of the render cache: a display list.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub width_mm: f64,
    pub height_mm: f64,
    pub ops: Vec<DisplayOp>,
}

/// RGBA color, 8 bits per channel (ICC-managed color uses resources).
pub type Color = [u8; 4];

/// One positioned glyph of a shaped run (format 0.4). Glyphs are stored
/// in **visual order**; `x_advance`/`x_offset`/`y_offset` are in
/// millimetres (exact µm/1000 conversions); `cluster` is the logical
/// UTF-8 byte offset within the run's `text`, so glyphs map back to
/// source characters for extraction and accessibility.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    pub gid: u16,
    pub x_advance: f64,
    pub x_offset: f64,
    pub y_offset: f64,
    pub cluster: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DisplayOp {
    /// A run of shaped text. Positions are in millimetres from the page
    /// top-left; `node_path` indexes child positions from the tree root;
    /// `char_range` is a UTF-8 byte range within the source node's text.
    TextRun {
        x: f64,
        y: f64,
        font: u64,
        size_pt: f64,
        color: Color,
        text: String,
        node_path: Vec<u64>,
        char_range: (u64, u64),
        /// Right-to-left run (format 0.3): `text` is stored in logical
        /// order; consumers draw its glyphs right-to-left starting at
        /// `x` (the run's left edge). Omitted from the encoding when
        /// false, so pre-0.3 pages decode unchanged.
        rtl: bool,
    },
    /// A run of pre-shaped, positioned glyphs (format 0.4): the engine
    /// shaped a complex script (Arabic, Devanagari) once, so consumers
    /// draw glyph ids at offsets and need no shaper. `x`/`y` are the
    /// run's origin (left edge, baseline) in mm; glyphs are in visual
    /// order; `text` is the logical source substring with `char_range`
    /// the byte range into the source node — selection/search/extraction
    /// use these, not the glyphs.
    GlyphRun {
        x: f64,
        y: f64,
        font: u64,
        size_pt: f64,
        color: Color,
        glyphs: Vec<Glyph>,
        text: String,
        node_path: Vec<u64>,
        char_range: (u64, u64),
    },
    /// Raster or vector resource placement.
    Image {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        res: ObjectId,
    },
    /// Filled rectangle (rules, table borders, backgrounds).
    Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        fill: Color,
    },
}

impl Page {
    pub fn to_value(&self) -> Value {
        let ops = self
            .ops
            .iter()
            .map(|op| match op {
                DisplayOp::TextRun {
                    x,
                    y,
                    font,
                    size_pt,
                    color,
                    text,
                    node_path,
                    char_range,
                    rtl,
                } => MapBuilder::new()
                    .put("op", Value::text("text"))
                    .put("x", Value::Float(*x))
                    .put("y", Value::Float(*y))
                    .put("font", Value::Unsigned(*font))
                    .put("size", Value::Float(*size_pt))
                    .put("color", Value::Bytes(color.to_vec()))
                    .put("text", Value::text(text))
                    .put(
                        "src",
                        Value::Array(node_path.iter().map(|&i| Value::Unsigned(i)).collect()),
                    )
                    .put(
                        "range",
                        Value::Array(vec![
                            Value::Unsigned(char_range.0),
                            Value::Unsigned(char_range.1),
                        ]),
                    )
                    .put_opt("rtl", if *rtl { Some(Value::Bool(true)) } else { None })
                    .build(),
                DisplayOp::GlyphRun {
                    x,
                    y,
                    font,
                    size_pt,
                    color,
                    glyphs,
                    text,
                    node_path,
                    char_range,
                } => MapBuilder::new()
                    .put("op", Value::text("glyphs"))
                    .put("x", Value::Float(*x))
                    .put("y", Value::Float(*y))
                    .put("font", Value::Unsigned(*font))
                    .put("size", Value::Float(*size_pt))
                    .put("color", Value::Bytes(color.to_vec()))
                    .put(
                        "g",
                        Value::Array(
                            glyphs
                                .iter()
                                .map(|gl| {
                                    Value::Array(vec![
                                        Value::Unsigned(gl.gid as u64),
                                        Value::Float(gl.x_advance),
                                        Value::Float(gl.x_offset),
                                        Value::Float(gl.y_offset),
                                        Value::Unsigned(gl.cluster as u64),
                                    ])
                                })
                                .collect(),
                        ),
                    )
                    .put("text", Value::text(text))
                    .put(
                        "src",
                        Value::Array(node_path.iter().map(|&i| Value::Unsigned(i)).collect()),
                    )
                    .put(
                        "range",
                        Value::Array(vec![
                            Value::Unsigned(char_range.0),
                            Value::Unsigned(char_range.1),
                        ]),
                    )
                    .build(),
                DisplayOp::Image { x, y, w, h, res } => MapBuilder::new()
                    .put("op", Value::text("image"))
                    .put("x", Value::Float(*x))
                    .put("y", Value::Float(*y))
                    .put("w", Value::Float(*w))
                    .put("h", Value::Float(*h))
                    .put("res", res.to_value())
                    .build(),
                DisplayOp::Rect { x, y, w, h, fill } => MapBuilder::new()
                    .put("op", Value::text("rect"))
                    .put("x", Value::Float(*x))
                    .put("y", Value::Float(*y))
                    .put("w", Value::Float(*w))
                    .put("h", Value::Float(*h))
                    .put("fill", Value::Bytes(fill.to_vec()))
                    .build(),
            })
            .collect();
        MapBuilder::new()
            .put("t", Value::text("page"))
            .put("w", Value::Float(self.width_mm))
            .put("h", Value::Float(self.height_mm))
            .put("ops", Value::Array(ops))
            .build()
    }

    pub fn from_value(v: &Value) -> Result<Page> {
        if v.get("t").and_then(Value::as_text) != Some("page") {
            return Err(Error::Schema("not a page object".into()));
        }
        let f = |m: &Value, key: &str| -> Result<f64> {
            m.get(key)
                .and_then(Value::as_f64)
                .ok_or_else(|| Error::Schema(format!("page op: missing float {key:?}")))
        };
        let color = |m: &Value, key: &str| -> Result<Color> {
            m.get(key)
                .and_then(Value::as_bytes)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .ok_or_else(|| Error::Schema(format!("page op: {key:?} must be 4 bytes RGBA")))
        };
        let ops = v
            .get("ops")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Schema("page: ops must be an array".into()))?
            .iter()
            .map(|op| {
                Ok(match op.get("op").and_then(Value::as_text) {
                    Some("text") => {
                        let src = op
                            .get("src")
                            .and_then(Value::as_array)
                            .ok_or_else(|| Error::Schema("text op: missing src".into()))?
                            .iter()
                            .map(|i| {
                                i.as_u64()
                                    .ok_or_else(|| Error::Schema("text op: bad src index".into()))
                            })
                            .collect::<Result<Vec<_>>>()?;
                        let range = op
                            .get("range")
                            .and_then(Value::as_array)
                            .filter(|a| a.len() == 2)
                            .ok_or_else(|| Error::Schema("text op: missing range".into()))?;
                        DisplayOp::TextRun {
                            x: f(op, "x")?,
                            y: f(op, "y")?,
                            font: op
                                .get("font")
                                .and_then(Value::as_u64)
                                .ok_or_else(|| Error::Schema("text op: missing font".into()))?,
                            size_pt: f(op, "size")?,
                            color: color(op, "color")?,
                            text: op
                                .get("text")
                                .and_then(Value::as_text)
                                .ok_or_else(|| Error::Schema("text op: missing text".into()))?
                                .to_owned(),
                            node_path: src,
                            char_range: (
                                range[0]
                                    .as_u64()
                                    .ok_or_else(|| Error::Schema("text op: bad range".into()))?,
                                range[1]
                                    .as_u64()
                                    .ok_or_else(|| Error::Schema("text op: bad range".into()))?,
                            ),
                            rtl: op.get("rtl").and_then(Value::as_bool).unwrap_or(false),
                        }
                    }
                    Some("glyphs") => {
                        let src = op
                            .get("src")
                            .and_then(Value::as_array)
                            .ok_or_else(|| Error::Schema("glyphs op: missing src".into()))?
                            .iter()
                            .map(|i| {
                                i.as_u64()
                                    .ok_or_else(|| Error::Schema("glyphs op: bad src index".into()))
                            })
                            .collect::<Result<Vec<_>>>()?;
                        let range = op
                            .get("range")
                            .and_then(Value::as_array)
                            .filter(|a| a.len() == 2)
                            .ok_or_else(|| Error::Schema("glyphs op: missing range".into()))?;
                        let glyphs = op
                            .get("g")
                            .and_then(Value::as_array)
                            .ok_or_else(|| Error::Schema("glyphs op: missing g".into()))?
                            .iter()
                            .map(|g| {
                                let a = g
                                    .as_array()
                                    .filter(|a| a.len() == 5)
                                    .ok_or_else(|| Error::Schema("glyphs op: bad glyph".into()))?;
                                let u = |i: usize| {
                                    a[i].as_u64().ok_or_else(|| {
                                        Error::Schema("glyphs op: bad glyph int".into())
                                    })
                                };
                                let fl = |i: usize| {
                                    a[i].as_f64().ok_or_else(|| {
                                        Error::Schema("glyphs op: bad glyph float".into())
                                    })
                                };
                                Ok(Glyph {
                                    gid: u(0)? as u16,
                                    x_advance: fl(1)?,
                                    x_offset: fl(2)?,
                                    y_offset: fl(3)?,
                                    cluster: u(4)? as u32,
                                })
                            })
                            .collect::<Result<Vec<_>>>()?;
                        DisplayOp::GlyphRun {
                            x: f(op, "x")?,
                            y: f(op, "y")?,
                            font: op
                                .get("font")
                                .and_then(Value::as_u64)
                                .ok_or_else(|| Error::Schema("glyphs op: missing font".into()))?,
                            size_pt: f(op, "size")?,
                            color: color(op, "color")?,
                            glyphs,
                            text: op
                                .get("text")
                                .and_then(Value::as_text)
                                .ok_or_else(|| Error::Schema("glyphs op: missing text".into()))?
                                .to_owned(),
                            node_path: src,
                            char_range: (
                                range[0]
                                    .as_u64()
                                    .ok_or_else(|| Error::Schema("glyphs op: bad range".into()))?,
                                range[1]
                                    .as_u64()
                                    .ok_or_else(|| Error::Schema("glyphs op: bad range".into()))?,
                            ),
                        }
                    }
                    Some("image") => DisplayOp::Image {
                        x: f(op, "x")?,
                        y: f(op, "y")?,
                        w: f(op, "w")?,
                        h: f(op, "h")?,
                        res: ObjectId::from_value(
                            op.get("res")
                                .ok_or_else(|| Error::Schema("image op: missing res".into()))?,
                        )?,
                    },
                    Some("rect") => DisplayOp::Rect {
                        x: f(op, "x")?,
                        y: f(op, "y")?,
                        w: f(op, "w")?,
                        h: f(op, "h")?,
                        fill: color(op, "fill")?,
                    },
                    other => return Err(Error::Schema(format!("unknown display op {other:?}"))),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Page {
            width_mm: f(v, "w")?,
            height_mm: f(v, "h")?,
            ops,
        })
    }
}

/// The contract a layout engine must satisfy (spec §5): a *deterministic*
/// projection from content tree to display lists. Determinism is the
/// normative requirement — same document and engine version must produce
/// byte-identical page objects on every platform.
pub trait LayoutEngine {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn layout(&self, doc: &Document, width_mm: f64, height_mm: f64) -> Result<Vec<Page>>;
}
