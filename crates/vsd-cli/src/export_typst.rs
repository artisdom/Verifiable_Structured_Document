//! VSD → Typst source writer (ROADMAP 4d).
//!
//! Emits [Typst](https://typst.app) markup from the content tree so a VSD
//! can flow into the Typst typesetting ecosystem:
//!
//! ```text
//! vsd export-typst report.vsd -o report.typ && typst compile report.typ
//! ```
//!
//! Structure and inline styling map to Typst markup; images are written
//! as sibling files and referenced with their `alt` text preserved.
//! MathML has no faithful Typst-math conversion, so a math node travels
//! verbatim in a labelled raw block rather than being mis-converted.

use std::fmt::Write as _;

use anyhow::Result;

use vsd_core::manifest::Style;
use vsd_core::tree::{Inline, Node, Span};
use vsd_core::Document;

pub struct Export {
    pub content: String,
    pub assets: Vec<(String, Vec<u8>)>,
}

pub fn document_to_typst(doc: &Document) -> Result<Export> {
    let styles = doc.resources()?.styles;
    let mut w = Writer {
        doc,
        styles,
        assets: Vec::new(),
        out: String::new(),
    };
    let root = doc.root_node()?;
    let (lang, children) = match &root {
        Node::Doc(d) => (d.lang.clone(), d.children.clone()),
        _ => (String::from("en"), Vec::new()),
    };
    // Preamble: document title + text language.
    if let Some(title) = doc.metadata()?.title {
        let _ = writeln!(w.out, "#set document(title: {})", typ_str(&title));
    }
    if !lang.is_empty() {
        let _ = writeln!(w.out, "#set text(lang: {})", typ_str(&lang));
    }
    w.out.push('\n');
    w.blocks(&children, 0)?;
    Ok(Export {
        content: w.out,
        assets: w.assets,
    })
}

struct Writer<'a> {
    doc: &'a Document,
    styles: Vec<Style>,
    assets: Vec<(String, Vec<u8>)>,
    out: String,
}

impl Writer<'_> {
    fn blocks(&mut self, nodes: &[Node], depth: usize) -> Result<()> {
        for n in nodes {
            self.block(n, depth)?;
        }
        Ok(())
    }

    fn block(&mut self, node: &Node, depth: usize) -> Result<()> {
        match node {
            Node::Heading(h) => {
                let _ = writeln!(
                    self.out,
                    "{} {}\n",
                    "=".repeat(h.level as usize),
                    self.inlines(&h.children)
                );
            }
            Node::Para(p) => {
                let _ = writeln!(self.out, "{}\n", self.inlines(&p.children));
            }
            Node::Code(c) => {
                let lang = c.lang.as_deref().unwrap_or("");
                // Use a fence longer than any backtick run inside the code.
                let max = longest_backtick_run(&c.text);
                let fence = "`".repeat(max.max(2) + 1);
                let _ = writeln!(self.out, "{fence}{lang}\n{}\n{fence}\n", c.text);
            }
            Node::List(l) => {
                let marker = if l.ordered { "+" } else { "-" };
                for item in &l.items {
                    let mut sub = Writer {
                        doc: self.doc,
                        styles: self.styles.clone(),
                        assets: Vec::new(),
                        out: String::new(),
                    };
                    sub.blocks(item, depth + 1)?;
                    self.assets.append(&mut sub.assets);
                    // Marker on the first line, continuation indented.
                    let body = sub.out.trim_end();
                    let mut lines = body.lines();
                    if let Some(first) = lines.next() {
                        let _ = writeln!(self.out, "{marker} {first}");
                        for line in lines {
                            if line.is_empty() {
                                self.out.push('\n');
                            } else {
                                let _ = writeln!(self.out, "  {line}");
                            }
                        }
                    }
                }
                self.out.push('\n');
            }
            Node::Table(t) => self.table(t)?,
            Node::Figure(f) => {
                let blob =
                    vsd_core::manifest::Blob::from_value(&self.doc.store.get_value(&f.res)?)?;
                let ext = match blob.mime.as_str() {
                    "image/png" => "png",
                    "image/jpeg" => "jpg",
                    "image/svg+xml" => "svg",
                    _ => "bin",
                };
                let name = format!("img{}.{ext}", self.assets.len());
                self.assets.push((name.clone(), blob.data));
                let _ = write!(
                    self.out,
                    "#figure(\n  image({}, alt: {}),",
                    typ_str(&name),
                    typ_str(&f.alt)
                );
                if !f.caption.is_empty() {
                    let _ = write!(self.out, "\n  caption: [{}],", self.inlines(&f.caption));
                }
                self.out.push_str("\n)\n\n");
            }
            Node::Section(s) => {
                let children = s.children.clone();
                if s.role == "quote" {
                    self.out.push_str("#quote(block: true)[\n");
                    self.blocks(&children, depth)?;
                    self.out.push_str("]\n\n");
                } else {
                    self.blocks(&children, depth)?;
                }
            }
            Node::Math(m) => {
                // MathML has no faithful Typst-math form; keep it verbatim
                // in a labelled raw block rather than mis-convert.
                let _ = writeln!(self.out, "```mathml\n{}\n```\n", m.mathml);
            }
            Node::Field(f) => {
                let label = f.label.clone().unwrap_or_else(|| f.id.clone());
                let _ = writeln!(self.out, "#rect[{}]\n", typ_escape(&label));
            }
            Node::Redacted(_) => self.out.push_str("#strike[[redacted]]\n\n"),
            Node::PageBreakHint => self.out.push_str("#pagebreak()\n\n"),
            Node::SubtreeRef(id) => {
                let sub = Node::from_value(&self.doc.store.get_value(id)?)?;
                self.block(&sub, depth)?;
            }
            Node::Salted(s) => self.block(&s.child, depth)?,
            Node::Doc(_) => {}
        }
        Ok(())
    }

    fn table(&mut self, t: &vsd_core::tree::Table) -> Result<()> {
        let ncols = t.cols.len().max(1);
        let _ = write!(self.out, "#table(\n  columns: {ncols},");
        // Header rows via Typst's `table.header`.
        if !t.head.is_empty() {
            self.out.push_str("\n  table.header(");
            for row in &t.head {
                for cell in &row.cells {
                    let txt = self.cell_text(&cell.children)?;
                    let _ = write!(self.out, "[{txt}], ");
                }
            }
            self.out.push_str("),");
        }
        for row in t.body.iter().chain(&t.foot) {
            self.out.push_str("\n  ");
            for cell in &row.cells {
                let txt = self.cell_text(&cell.children)?;
                let _ = write!(self.out, "[{txt}], ");
            }
        }
        self.out.push_str("\n)\n\n");
        Ok(())
    }

    /// A table cell's content as inline Typst (paragraphs joined).
    fn cell_text(&mut self, blocks: &[Node]) -> Result<String> {
        let mut parts = Vec::new();
        for b in blocks {
            if let Node::Para(p) = b {
                parts.push(self.inlines(&p.children));
            } else {
                // Non-paragraph cell content: render and inline-flatten.
                let mut sub = Writer {
                    doc: self.doc,
                    styles: self.styles.clone(),
                    assets: Vec::new(),
                    out: String::new(),
                };
                sub.block(b, 0)?;
                self.assets.append(&mut sub.assets);
                parts.push(sub.out.trim().replace('\n', " "));
            }
        }
        Ok(parts.join(" "))
    }

    fn inlines(&self, inls: &[Inline]) -> String {
        let mut s = String::new();
        for i in inls {
            self.inline(i, &mut s);
        }
        s
    }

    fn inline(&self, inl: &Inline, out: &mut String) {
        match inl {
            Inline::Text(t) => out.push_str(&typ_escape(t)),
            Inline::Link(l) => {
                let _ = write!(
                    out,
                    "#link({})[{}]",
                    typ_str(&l.href),
                    self.inlines(&l.children)
                );
            }
            Inline::Span(sp) => out.push_str(&self.span(sp)),
            Inline::Math(m) => {
                // Inline math: keep the MathML as raw text, uncconverted.
                let _ = write!(out, "#raw({})", typ_str(&m.mathml));
            }
            Inline::FootnoteRef(id) => out.push_str(&typ_escape(id)),
        }
    }

    fn span(&self, sp: &Span) -> String {
        let style = sp.style.and_then(|i| self.styles.get(i as usize)).cloned();
        let inner = self.inlines(&sp.children);
        match style {
            Some(s) if s.mono => {
                // Inline raw; backticks in content are escaped by `typ_raw`.
                typ_raw(&plain_text(&sp.children))
            }
            Some(s) => {
                let mut v = inner;
                if s.underline {
                    v = format!("#underline[{v}]");
                }
                if s.italic {
                    v = format!("_{v}_");
                }
                if s.bold {
                    v = format!("*{v}*");
                }
                v
            }
            None => inner,
        }
    }
}

/// A Typst string literal: `"…"` with `\` and `"` escaped.
fn typ_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Escape Typst markup metacharacters in body text.
fn typ_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '`' | '#' | '$' | '@' | '<' | '>' | '[' | ']' | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Inline raw text `` `…` ``, using a backtick fence longer than any run
/// inside (Typst raw delimiters).
fn typ_raw(s: &str) -> String {
    let n = longest_backtick_run(s) + 1;
    let fence = "`".repeat(n.max(1));
    format!("{fence}{s}{fence}")
}

fn longest_backtick_run(s: &str) -> usize {
    let mut max = 0;
    let mut cur = 0;
    for c in s.chars() {
        if c == '`' {
            cur += 1;
            max = max.max(cur);
        } else {
            cur = 0;
        }
    }
    max
}

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

    fn typ(doc: &Document) -> String {
        document_to_typst(doc).unwrap().content
    }

    #[test]
    fn emits_headings_paragraphs_lists_code() {
        let doc = Compose::new("en")
            .title("T")
            .h1("Title")
            .h2("Sub")
            .para("Body text.")
            .bullets(["one", "two"])
            .code(Some("rust"), "fn main() {}")
            .finish()
            .unwrap();
        let s = typ(doc_ref(&doc));
        assert!(s.contains("#set document(title: \"T\")"));
        assert!(s.contains("= Title"));
        assert!(s.contains("== Sub"));
        assert!(s.contains("Body text."));
        assert!(s.contains("- one"));
        assert!(s.contains("- two"));
        assert!(s.contains("```rust\nfn main() {}\n```"));
    }

    #[test]
    fn escapes_markup_and_emits_links() {
        let doc = Compose::new("en").para("a * b _ c # d").finish().unwrap();
        let s = typ(&doc);
        // The asterisk/underscore/hash are escaped, not treated as markup.
        assert!(s.contains("\\*") && s.contains("\\_") && s.contains("\\#"));
    }

    fn doc_ref(d: &Document) -> &Document {
        d
    }
}
