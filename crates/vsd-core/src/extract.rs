//! Text extraction — a tree walk, not a research field (spec §3).
//!
//! Because the canonical layer *is* structure, extraction is exact:
//! reading order is tree order, table topology is structural fact, and
//! the output is the text the document semantically contains, byte for
//! byte what a screen reader or indexer sees.

use crate::document::Document;
use crate::error::Result;
use crate::tree::{Inline, Node};

/// Extract plain text in reading order. Blocks are separated by blank
/// lines; table cells by tabs, rows by newlines.
pub fn extract_text(doc: &Document) -> Result<String> {
    let mut out = String::new();
    walk(doc, &doc.root_node()?, &mut out)?;
    // Normalize trailing whitespace to exactly one final newline.
    let trimmed = out.trim_end();
    let mut s = trimmed.to_owned();
    if !s.is_empty() {
        s.push('\n');
    }
    Ok(s)
}

fn walk(doc: &Document, node: &Node, out: &mut String) -> Result<()> {
    match node {
        Node::Doc(d) => {
            for c in &d.children {
                walk(doc, c, out)?;
            }
        }
        Node::Section(s) => {
            for c in &s.children {
                walk(doc, c, out)?;
            }
        }
        Node::Heading(h) => {
            inlines(&h.children, out);
            out.push_str("\n\n");
        }
        Node::Para(p) => {
            inlines(&p.children, out);
            out.push_str("\n\n");
        }
        Node::Table(t) => {
            for row in t.head.iter().chain(&t.body).chain(&t.foot) {
                let mut first = true;
                for cell in &row.cells {
                    if !first {
                        out.push('\t');
                    }
                    first = false;
                    let mut cell_text = String::new();
                    for c in &cell.children {
                        walk(doc, c, &mut cell_text)?;
                    }
                    out.push_str(cell_text.trim().replace('\n', " ").as_str());
                }
                out.push('\n');
            }
            out.push('\n');
        }
        Node::Figure(f) => {
            // Alt text is the figure's textual content — that's the point.
            out.push('[');
            out.push_str(&f.alt);
            out.push(']');
            if !f.caption.is_empty() {
                out.push(' ');
                inlines(&f.caption, out);
            }
            out.push_str("\n\n");
        }
        Node::List(l) => {
            for (i, item) in l.items.iter().enumerate() {
                if l.ordered {
                    out.push_str(&format!("{}. ", i + 1));
                } else {
                    out.push_str("- ");
                }
                let mut item_text = String::new();
                for c in item {
                    walk(doc, c, &mut item_text)?;
                }
                out.push_str(item_text.trim());
                out.push('\n');
            }
            out.push('\n');
        }
        Node::Code(c) => {
            out.push_str(&c.text);
            out.push_str("\n\n");
        }
        Node::Math(m) => {
            out.push_str(&m.mathml);
            out.push_str("\n\n");
        }
        Node::Field(f) => {
            out.push('[');
            out.push_str(f.label.as_deref().unwrap_or(&f.id));
            out.push_str(": ____]\n\n");
        }
        Node::Redacted(r) => {
            out.push_str("[REDACTED");
            if let Some(reason) = &r.reason {
                out.push_str(": ");
                out.push_str(reason);
            }
            out.push_str("]\n\n");
        }
        Node::SubtreeRef(id) => {
            let sub = Node::from_value(&doc.store.get_value(id)?)?;
            walk(doc, &sub, out)?;
        }
        Node::PageBreakHint => {}
    }
    Ok(())
}

fn inlines(items: &[Inline], out: &mut String) {
    for i in items {
        match i {
            Inline::Text(s) => out.push_str(s),
            Inline::Span(sp) => inlines(&sp.children, out),
            Inline::Link(l) => inlines(&l.children, out),
            Inline::Math(m) => out.push_str(&m.mathml),
            Inline::FootnoteRef(id) => {
                out.push('[');
                out.push_str(id);
                out.push(']');
            }
        }
    }
}
