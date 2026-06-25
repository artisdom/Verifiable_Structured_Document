//! JSON authoring format → VSD document.
//!
//! A human-writable JSON dialect for creating documents. This is an
//! *authoring convenience*, not part of the format: the canonical
//! representation is always the deterministic CBOR content tree.
//!
//! ```json
//! {
//!   "lang": "en",
//!   "title": "Quarterly report",
//!   "authors": ["A. Person"],
//!   "content": [
//!     { "type": "heading", "level": 1, "text": "Q3 results" },
//!     { "type": "para", "children": ["Revenue ", {"text": "doubled", "bold": true}, "."] },
//!     { "type": "figure", "src": "chart.png", "alt": "Revenue by month, rising" },
//!     { "type": "table", "columns": 2,
//!       "head": [["Item", "Price"]],
//!       "body": [["Widget", "4.20"]] }
//!   ]
//! }
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value as Json;

use vsd_core::document::DocumentBuilder;
use vsd_core::forms::Expr;
use vsd_core::manifest::{Blob, Metadata, Profile, ResourceEntry, ResourceKind, Style};
use vsd_core::tree::{
    Cell, CellScope, Code, ColSpec, Direction, Doc, Field, FieldKind, Figure, Heading, Inline,
    Link, List, Math, Node, Para, Row, Section, Span, Table,
};
use vsd_core::{Document, ResourceTable};

pub fn document_from_json(json: &Json, base_dir: &Path, profile: Profile) -> Result<Document> {
    let obj = json.as_object().context("document must be a JSON object")?;

    let lang = obj
        .get("lang")
        .and_then(Json::as_str)
        .unwrap_or("en")
        .to_owned();
    let dir = match obj.get("dir").and_then(Json::as_str) {
        None | Some("ltr") => Direction::Ltr,
        Some("rtl") => Direction::Rtl,
        Some(other) => bail!("dir must be \"ltr\" or \"rtl\", got {other:?}"),
    };

    let mut ctx = AuthorCtx {
        base_dir,
        styles: Vec::new(),
        style_index: BTreeMap::new(),
        resources: Vec::new(),
        blobs: Vec::new(),
        next_res: 0,
    };

    let content = obj
        .get("content")
        .and_then(Json::as_array)
        .context("document needs a \"content\" array")?;
    let children = content
        .iter()
        .map(|n| ctx.block(n))
        .collect::<Result<Vec<_>>>()?;

    let root = Node::Doc(Doc {
        lang,
        dir,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children,
    });

    let metadata = Metadata {
        title: obj.get("title").and_then(Json::as_str).map(str::to_owned),
        authors: obj
            .get("authors")
            .and_then(Json::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Json::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        created: obj.get("created").and_then(Json::as_str).map(str::to_owned),
        modified: obj
            .get("modified")
            .and_then(Json::as_str)
            .map(str::to_owned),
        keywords: obj
            .get("keywords")
            .and_then(Json::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Json::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        custom: obj
            .get("custom")
            .and_then(Json::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                    .collect()
            })
            .unwrap_or_default(),
    };

    let mut builder = DocumentBuilder::new(root)
        .metadata(metadata)
        .profile(profile);
    let mut entries = Vec::new();
    for (name, entry, blob) in ctx
        .resources
        .iter()
        .zip(ctx.blobs.iter())
        .map(|((n, e), b)| (n.clone(), e.clone(), b.clone()))
    {
        let id = builder.add_object(blob.to_value())?;
        debug_assert_eq!(id, entry.data);
        entries.push((name, entry));
    }
    builder = builder.resources(ResourceTable {
        entries,
        styles: ctx.styles,
    });
    Ok(builder.build()?)
}

struct AuthorCtx<'a> {
    base_dir: &'a Path,
    styles: Vec<Style>,
    style_index: BTreeMap<(bool, bool, bool, bool), u64>,
    resources: Vec<(String, ResourceEntry)>,
    blobs: Vec<Blob>,
    next_res: usize,
}

impl AuthorCtx<'_> {
    fn block(&mut self, j: &Json) -> Result<Node> {
        // A bare string is shorthand for a paragraph.
        if let Some(s) = j.as_str() {
            return Ok(Node::Para(Para {
                children: vec![Inline::Text(s.to_owned())],
            }));
        }
        let obj = j.as_object().context("block must be an object or string")?;
        let t = obj
            .get("type")
            .and_then(Json::as_str)
            .context("block needs a \"type\"")?;
        Ok(match t {
            "heading" => Node::Heading(Heading {
                level: obj
                    .get("level")
                    .and_then(Json::as_u64)
                    .filter(|l| (1..=6).contains(l))
                    .context("heading needs level 1-6")? as u8,
                children: self.inlines(obj)?,
            }),
            "para" => Node::Para(Para {
                children: self.inlines(obj)?,
            }),
            "section" => Node::Section(Section {
                role: obj
                    .get("role")
                    .and_then(Json::as_str)
                    .unwrap_or("section")
                    .to_owned(),
                columns: obj
                    .get("columns")
                    .and_then(Json::as_u64)
                    .map(|n| n as u32)
                    .unwrap_or(1),
                children: obj
                    .get("content")
                    .and_then(Json::as_array)
                    .context("section needs \"content\"")?
                    .iter()
                    .map(|n| self.block(n))
                    .collect::<Result<Vec<_>>>()?,
            }),
            "table" => self.table(obj)?,
            "figure" => self.figure(obj)?,
            "list" => Node::List(List {
                ordered: obj.get("ordered").and_then(Json::as_bool).unwrap_or(false),
                items: obj
                    .get("items")
                    .and_then(Json::as_array)
                    .context("list needs \"items\"")?
                    .iter()
                    .map(|item| match item {
                        Json::Array(blocks) => blocks
                            .iter()
                            .map(|b| self.block(b))
                            .collect::<Result<Vec<_>>>(),
                        other => Ok(vec![self.block(other)?]),
                    })
                    .collect::<Result<Vec<_>>>()?,
            }),
            "code" => Node::Code(Code {
                lang: obj.get("lang").and_then(Json::as_str).map(str::to_owned),
                text: obj
                    .get("text")
                    .and_then(Json::as_str)
                    .context("code needs \"text\"")?
                    .to_owned(),
            }),
            "math" => Node::Math(Math {
                mathml: obj
                    .get("mathml")
                    .and_then(Json::as_str)
                    .context("math needs \"mathml\"")?
                    .to_owned(),
                fallback: None,
            }),
            "field" => Node::Field(self.field(obj)?),
            "pagebreak" => Node::PageBreakHint,
            other => bail!("unknown block type {other:?}"),
        })
    }

    fn field(&mut self, obj: &serde_json::Map<String, Json>) -> Result<Field> {
        let expr = |key: &str| -> Result<Option<Expr>> {
            match obj.get(key) {
                None => Ok(None),
                Some(j) => {
                    // Expressions are authored in the same s-expression
                    // JSON shape as the wire format.
                    let cbor = json_to_cbor(j)?;
                    Ok(Some(
                        Expr::from_value(&cbor).map_err(|e| anyhow!("{key}: {e}"))?,
                    ))
                }
            }
        };
        Ok(Field {
            id: obj
                .get("id")
                .and_then(Json::as_str)
                .context("field needs \"id\"")?
                .to_owned(),
            kind: FieldKind::parse(
                obj.get("kind")
                    .and_then(Json::as_str)
                    .context("field needs \"kind\"")?,
            )?,
            label: obj.get("label").and_then(Json::as_str).map(str::to_owned),
            required: obj.get("required").and_then(Json::as_bool).unwrap_or(false),
            constraint: expr("constraint")?,
            computed: expr("computed")?,
        })
    }

    fn figure(&mut self, obj: &serde_json::Map<String, Json>) -> Result<Node> {
        let src = obj
            .get("src")
            .and_then(Json::as_str)
            .context("figure needs \"src\"")?;
        let path = self.base_dir.join(src);
        let data = std::fs::read(&path)
            .with_context(|| format!("reading figure resource {}", path.display()))?;
        let mime = obj
            .get("mime")
            .and_then(Json::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| guess_mime(src));
        let kind = if mime == "image/svg+xml" {
            ResourceKind::Vector
        } else {
            ResourceKind::Image
        };
        let blob = Blob {
            mime: mime.clone(),
            data,
        };
        let data_id = vsd_core::ObjectId::of_value(&blob.to_value())?;

        let name = format!("res{}", self.next_res);
        self.next_res += 1;
        self.resources.push((
            name,
            ResourceEntry {
                kind,
                mime,
                data: data_id,
            },
        ));
        self.blobs.push(blob);

        let decorative = obj
            .get("decorative")
            .and_then(Json::as_bool)
            .unwrap_or(false);
        let alt = obj
            .get("alt")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_owned();
        if alt.trim().is_empty() && !decorative {
            bail!(
                "figure {src:?}: \"alt\" is required (or set \"decorative\": true) — \
                 accessibility is a validity condition in VSD"
            );
        }
        Ok(Node::Figure(Figure {
            res: data_id,
            alt,
            decorative,
            caption: match obj.get("caption") {
                None => vec![],
                Some(c) => self.inline_list(c)?,
            },
        }))
    }

    fn table(&mut self, obj: &serde_json::Map<String, Json>) -> Result<Node> {
        let mut rows = |key: &str, header: bool| -> Result<Vec<Row>> {
            Ok(match obj.get(key) {
                None => vec![],
                Some(j) => j
                    .as_array()
                    .with_context(|| format!("table {key:?} must be an array of rows"))?
                    .iter()
                    .map(|row| {
                        let cells = row
                            .as_array()
                            .context("table row must be an array of cells")?
                            .iter()
                            .map(|cell| {
                                let children = match cell {
                                    Json::String(s) => vec![Node::Para(Para {
                                        children: vec![Inline::Text(s.clone())],
                                    })],
                                    other => vec![self.block(other)?],
                                };
                                Ok(Cell {
                                    span: None,
                                    scope: if header { Some(CellScope::Col) } else { None },
                                    children,
                                })
                            })
                            .collect::<Result<Vec<_>>>()?;
                        Ok(Row { cells })
                    })
                    .collect::<Result<Vec<_>>>()?,
            })
        };
        let head = rows("head", true)?;
        let body = rows("body", false)?;
        let foot = rows("foot", false)?;
        let ncols = obj
            .get("columns")
            .and_then(Json::as_u64)
            .map(|n| n as usize)
            .or_else(|| head.first().or(body.first()).map(|r| r.cells.len()))
            .context("table needs \"columns\" or at least one row")?;
        Ok(Node::Table(Table {
            cols: vec![ColSpec { width: None }; ncols],
            head,
            body,
            foot,
        }))
    }

    fn inlines(&mut self, obj: &serde_json::Map<String, Json>) -> Result<Vec<Inline>> {
        if let Some(text) = obj.get("text").and_then(Json::as_str) {
            return Ok(vec![Inline::Text(text.to_owned())]);
        }
        match obj.get("children") {
            Some(c) => self.inline_list(c),
            None => Ok(vec![]),
        }
    }

    fn inline_list(&mut self, j: &Json) -> Result<Vec<Inline>> {
        match j {
            Json::String(s) => Ok(vec![Inline::Text(s.clone())]),
            Json::Array(items) => items.iter().map(|i| self.inline(i)).collect(),
            _ => bail!("inline content must be a string or array"),
        }
    }

    fn inline(&mut self, j: &Json) -> Result<Inline> {
        if let Some(s) = j.as_str() {
            return Ok(Inline::Text(s.to_owned()));
        }
        let obj = j.as_object().context("inline must be a string or object")?;
        if let Some(href) = obj.get("href").and_then(Json::as_str) {
            return Ok(Inline::Link(Link {
                href: href.to_owned(),
                children: match obj.get("text").and_then(Json::as_str) {
                    Some(t) => vec![Inline::Text(t.to_owned())],
                    None => self.inline_list(obj.get("children").unwrap_or(&Json::Null))?,
                },
            }));
        }
        let text = obj
            .get("text")
            .and_then(Json::as_str)
            .context("styled inline needs \"text\"")?
            .to_owned();
        let key = (
            obj.get("bold").and_then(Json::as_bool).unwrap_or(false),
            obj.get("italic").and_then(Json::as_bool).unwrap_or(false),
            obj.get("underline")
                .and_then(Json::as_bool)
                .unwrap_or(false),
            obj.get("mono").and_then(Json::as_bool).unwrap_or(false),
        );
        if key == (false, false, false, false) {
            return Ok(Inline::Text(text));
        }
        let style = match self.style_index.get(&key) {
            Some(&i) => i,
            None => {
                let i = self.styles.len() as u64;
                self.styles.push(Style {
                    bold: key.0,
                    italic: key.1,
                    underline: key.2,
                    mono: key.3,
                });
                self.style_index.insert(key, i);
                i
            }
        };
        Ok(Inline::Span(Span {
            style: Some(style),
            children: vec![Inline::Text(text)],
        }))
    }
}

/// Convert authoring JSON to a CBOR value (for form expressions).
fn json_to_cbor(j: &Json) -> Result<vsd_core::cbor::Value> {
    use vsd_core::cbor::Value;
    Ok(match j {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => {
            if let Some(u) = n.as_u64() {
                Value::Unsigned(u)
            } else if let Some(i) = n.as_i64() {
                Value::int(i)
            } else {
                Value::Float(n.as_f64().context("bad number")?)
            }
        }
        Json::String(s) => Value::text(s),
        Json::Array(a) => Value::Array(a.iter().map(json_to_cbor).collect::<Result<Vec<_>>>()?),
        Json::Object(o) => Value::Map(
            o.iter()
                .map(|(k, v)| Ok((Value::text(k), json_to_cbor(v)?)))
                .collect::<Result<Vec<_>>>()?,
        ),
    })
}

fn guess_mime(path: &str) -> String {
    let lower = path.to_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "jxl" => "image/jxl",
        "avif" => "image/avif",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
    .to_owned()
}
