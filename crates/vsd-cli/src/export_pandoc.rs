//! VSD → Pandoc JSON AST writer (ROADMAP 4d, the reverse direction).
//!
//! The on-ramp [`crate::pandoc`] reads Pandoc's AST *into* VSD; this
//! emits it *out*, so VSD reaches every format Pandoc writes:
//!
//! ```text
//! vsd export-pandoc report.vsd | pandoc -f json -o report.docx
//! ```
//!
//! The AST is `pandoc-api-version` 1.23. Figures reference their image by
//! a generated filename and the bytes are returned as assets for the CLI
//! to write alongside, so Pandoc can pick them up. MathML has no faithful
//! Pandoc-math representation, so it travels as a `RawInline "html"`
//! (passed through to HTML-family outputs, dropped elsewhere) rather than
//! mis-converted to TeX.

use anyhow::Result;
use serde_json::{json, Value as J};

use vsd_core::manifest::Style;
use vsd_core::tree::{Inline, Node, Span};
use vsd_core::Document;

/// A rendered export: the text artifact plus any binary assets it
/// references (image files), to be written alongside it.
pub struct Export {
    pub content: String,
    pub assets: Vec<(String, Vec<u8>)>,
}

pub fn document_to_pandoc(doc: &Document) -> Result<Export> {
    let styles = doc.resources()?.styles;
    let mut w = Writer {
        doc,
        styles,
        assets: Vec::new(),
    };
    let root = doc.root_node()?;
    let (lang, title, blocks) = match &root {
        Node::Doc(d) => {
            let blocks = w.blocks(&d.children)?;
            (d.lang.clone(), doc.metadata()?.title, blocks)
        }
        _ => (String::from("und"), None, Vec::new()),
    };
    let mut meta = serde_json::Map::new();
    if let Some(t) = title {
        meta.insert(
            "title".into(),
            json!({"t":"MetaInlines","c": str_inlines(&t)}),
        );
    }
    if !lang.is_empty() {
        meta.insert("lang".into(), json!({"t":"MetaString","c": lang}));
    }
    let ast = json!({
        "pandoc-api-version": [1, 23],
        "meta": J::Object(meta),
        "blocks": blocks,
    });
    Ok(Export {
        content: serde_json::to_string(&ast)?,
        assets: w.assets,
    })
}

struct Writer<'a> {
    doc: &'a Document,
    styles: Vec<Style>,
    assets: Vec<(String, Vec<u8>)>,
}

impl Writer<'_> {
    fn blocks(&mut self, nodes: &[Node]) -> Result<Vec<J>> {
        let mut out = Vec::new();
        for n in nodes {
            self.block(n, &mut out)?;
        }
        Ok(out)
    }

    fn block(&mut self, node: &Node, out: &mut Vec<J>) -> Result<()> {
        match node {
            Node::Heading(h) => out.push(json!({
                "t":"Header","c":[h.level as i64, attr_empty(), self.inlines(&h.children)]
            })),
            Node::Para(p) => out.push(json!({"t":"Para","c": self.inlines(&p.children)})),
            Node::Code(c) => {
                let classes = c.lang.iter().cloned().collect::<Vec<_>>();
                out.push(json!({
                    "t":"CodeBlock","c":[["", classes, []], c.text]
                }));
            }
            Node::List(l) => {
                let items = l
                    .items
                    .iter()
                    .map(|blocks| self.blocks(blocks))
                    .collect::<Result<Vec<_>>>()?;
                if l.ordered {
                    out.push(json!({
                        "t":"OrderedList",
                        "c":[[1, {"t":"Decimal"}, {"t":"Period"}], items]
                    }));
                } else {
                    out.push(json!({"t":"BulletList","c": items}));
                }
            }
            Node::Table(t) => out.push(self.table(t)?),
            Node::Figure(f) => {
                let (name, alt) = self.figure_asset(f)?;
                let img = json!({
                    "t":"Image","c":[attr_empty(), str_inlines(&alt), [name, ""]]
                });
                // A standalone Figure block (pandoc-types 1.23).
                out.push(json!({
                    "t":"Figure",
                    "c":[attr_empty(), [null, []], [{"t":"Plain","c":[img]}]]
                }));
            }
            Node::Section(s) => {
                let children = s.children.clone();
                if s.role == "quote" {
                    out.push(json!({"t":"BlockQuote","c": self.blocks(&children)?}));
                } else {
                    // Generic sections flow transparently.
                    let inner = self.blocks(&children)?;
                    out.extend(inner);
                }
            }
            Node::Math(m) => {
                // Block math with no faithful Pandoc form → raw HTML MathML.
                out.push(json!({"t":"RawBlock","c":["html", m.mathml]}));
            }
            Node::Field(f) => {
                // No Pandoc form-field type; surface the field's label.
                let label = f.label.clone().unwrap_or_else(|| f.id.clone());
                out.push(json!({"t":"Para","c": str_inlines(&format!("[{label}]"))}));
            }
            Node::Redacted(_) => out.push(json!({
                "t":"Para","c":[{"t":"Strikeout","c":[str_str("[redacted]")]}]
            })),
            Node::PageBreakHint => {} // no Pandoc equivalent
            Node::SubtreeRef(id) => {
                let sub = Node::from_value(&self.doc.store.get_value(id)?)?;
                self.block(&sub, out)?;
            }
            Node::Salted(s) => self.block(&s.child, out)?,
            Node::Doc(_) => {} // nested docs are not expressible
        }
        Ok(())
    }

    fn table(&mut self, t: &vsd_core::tree::Table) -> Result<J> {
        let ncols = t.cols.len().max(1);
        let colspecs: Vec<J> = (0..ncols)
            .map(|_| json!([{"t":"AlignDefault"}, {"t":"ColWidthDefault"}]))
            .collect();
        let rows = |w: &mut Self, rs: &[vsd_core::tree::Row]| -> Result<Vec<J>> {
            rs.iter()
                .map(|r| {
                    let cells = r
                        .cells
                        .iter()
                        .map(|c| {
                            let (rs, cs) = c.span.unwrap_or((1, 1));
                            Ok(json!([
                                attr_empty(),
                                {"t":"AlignDefault"},
                                rs as i64,
                                cs as i64,
                                w.blocks(&c.children)?
                            ]))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Ok(json!([attr_empty(), cells]))
                })
                .collect()
        };
        let head = rows(self, &t.head)?;
        let body = rows(self, &t.body)?;
        let foot = rows(self, &t.foot)?;
        Ok(json!({
            "t":"Table","c":[
                attr_empty(),
                [null, []],
                colspecs,
                [attr_empty(), head],
                [[attr_empty(), 0, [], body]],
                [attr_empty(), foot]
            ]
        }))
    }

    fn inlines(&self, inls: &[Inline]) -> Vec<J> {
        let mut out = Vec::new();
        for i in inls {
            self.inline(i, &mut out);
        }
        out
    }

    fn inline(&self, inl: &Inline, out: &mut Vec<J>) {
        match inl {
            Inline::Text(s) => out.extend(str_inlines(s)),
            Inline::Link(l) => out.push(json!({
                "t":"Link","c":[attr_empty(), self.inlines(&l.children), [l.href, ""]]
            })),
            Inline::Span(sp) => out.extend(self.span(sp)),
            Inline::Math(m) => out.push(json!({"t":"RawInline","c":["html", m.mathml]})),
            Inline::FootnoteRef(id) => out.extend(str_inlines(id)),
        }
    }

    fn span(&self, sp: &Span) -> Vec<J> {
        let style = sp.style.and_then(|i| self.styles.get(i as usize)).cloned();
        match style {
            Some(s) if s.mono => {
                // Pandoc Code is plain text; flatten the children's text.
                vec![json!({"t":"Code","c":[attr_empty(), plain_text(&sp.children)]})]
            }
            Some(s) => {
                let mut v = self.inlines(&sp.children);
                if s.underline {
                    v = vec![json!({"t":"Underline","c": v})];
                }
                if s.italic {
                    v = vec![json!({"t":"Emph","c": v})];
                }
                if s.bold {
                    v = vec![json!({"t":"Strong","c": v})];
                }
                v
            }
            None => self.inlines(&sp.children),
        }
    }

    /// Register a figure's image bytes as an asset and return its
    /// (filename, alt-text). The image is referenced by filename in the
    /// AST; the CLI writes the bytes next to the output.
    fn figure_asset(&mut self, f: &vsd_core::tree::Figure) -> Result<(String, String)> {
        let blob = vsd_core::manifest::Blob::from_value(&self.doc.store.get_value(&f.res)?)?;
        let ext = match blob.mime.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/svg+xml" => "svg",
            _ => "bin",
        };
        let name = format!("img{}.{ext}", self.assets.len());
        self.assets.push((name.clone(), blob.data));
        Ok((name, f.alt.clone()))
    }
}

fn attr_empty() -> J {
    json!(["", [], []])
}

fn str_str(s: &str) -> J {
    json!({"t":"Str","c": s})
}

/// Split text into Pandoc `Str` words and `Space`/`SoftBreak` tokens.
fn str_inlines(s: &str) -> Vec<J> {
    let mut out = Vec::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut Vec<J>| {
        if !word.is_empty() {
            out.push(str_str(word));
            word.clear();
        }
    };
    for c in s.chars() {
        if c == '\n' {
            flush(&mut word, &mut out);
            out.push(json!({"t":"SoftBreak"}));
        } else if c.is_whitespace() {
            flush(&mut word, &mut out);
            // Collapse a run of spaces into a single Space token.
            if !matches!(out.last(), Some(v) if v.get("t").and_then(J::as_str)==Some("Space")) {
                out.push(json!({"t":"Space"}));
            }
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// Flatten inline content to plain text (for `Code`, which is textual).
fn plain_text(inls: &[Inline]) -> String {
    let mut out = String::new();
    for i in inls {
        match i {
            Inline::Text(s) => out.push_str(s),
            Inline::Span(sp) => out.push_str(&plain_text(&sp.children)),
            Inline::Link(l) => out.push_str(&plain_text(&l.children)),
            Inline::Math(m) => out.push_str(&m.mathml),
            Inline::FootnoteRef(id) => out.push_str(id),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vsd_core::compose::Compose;

    fn ast(doc: &Document) -> J {
        serde_json::from_str(&document_to_pandoc(doc).unwrap().content).unwrap()
    }

    #[test]
    fn emits_pandoc_ast_round_trippable_by_the_on_ramp() {
        let doc = Compose::new("en")
            .title("Round Trip")
            .h1("Title")
            .para("Body text here.")
            .finish()
            .unwrap();
        let a = ast(&doc);
        assert_eq!(a["pandoc-api-version"][0], 1);
        assert_eq!(a["meta"]["title"]["t"], "MetaInlines");
        let blocks = a["blocks"].as_array().unwrap();
        assert_eq!(blocks[0]["t"], "Header");
        assert_eq!(blocks[0]["c"][0], 1);
        assert_eq!(blocks[1]["t"], "Para");

        // The on-ramp consumes our own output and recovers the tree.
        let json = document_to_pandoc(&doc).unwrap().content;
        let back = crate::pandoc::document_from_pandoc(
            &json,
            std::path::Path::new("."),
            vsd_core::manifest::Profile::Core,
        )
        .unwrap();
        let text = vsd_core::extract::extract_text(&back).unwrap();
        assert!(text.contains("Title"));
        assert!(text.contains("Body text here."));
    }

    #[test]
    fn maps_lists_code_and_styling() {
        let doc = Compose::new("en")
            .h2("Sub")
            .bullets(["one", "two"])
            .code(Some("rust"), "fn main() {}")
            .finish()
            .unwrap();
        let a = ast(&doc);
        let blocks = a["blocks"].as_array().unwrap();
        assert!(blocks.iter().any(|b| b["t"] == "BulletList"));
        let cb = blocks.iter().find(|b| b["t"] == "CodeBlock").unwrap();
        assert_eq!(cb["c"][0][1][0], "rust");
        assert_eq!(cb["c"][1], "fn main() {}");
    }
}
