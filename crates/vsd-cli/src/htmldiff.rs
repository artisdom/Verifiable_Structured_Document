//! `vsd diff --html` (ROADMAP 4f): a self-contained redline view from
//! the structural diff — contract negotiation without "compare in
//! Word". No JavaScript in the output, by policy.

use std::fmt::Write as _;

use anyhow::Result;
use vsd_core::tree::{Inline, Node};
use vsd_core::Document;

/// One divergence between the two trees.
struct Change {
    path: String,
    old: Option<String>,
    new: Option<String>,
}

pub fn render_diff_html(old: &Document, new: &Document) -> Result<String> {
    let mut changes = Vec::new();
    let old_root = old.root_node()?;
    let new_root = new.root_node()?;
    walk(
        Some(&old_root),
        Some(&new_root),
        old,
        new,
        &mut Vec::new(),
        &mut changes,
    )?;

    let old_id = old.document_id()?;
    let new_id = new.document_id()?;
    let chained = new.manifest.predecessor == Some(old_id);

    let mut html = String::new();
    html.push_str(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n\
         <title>VSD diff</title>\n<style>\n\
         body{font:15px/1.5 system-ui,sans-serif;max-width:60rem;margin:2rem auto;padding:0 1rem;color:#111}\n\
         h1{font-size:1.3rem} code{font-size:.85em;background:#f4f4f4;padding:.1em .3em;border-radius:3px}\n\
         .meta{color:#555;font-size:.9rem}\n\
         .change{border:1px solid #ddd;border-radius:6px;margin:1rem 0;overflow:hidden}\n\
         .path{background:#f7f7f7;padding:.4rem .8rem;font-family:monospace;font-size:.85rem;color:#444}\n\
         .old,.new{padding:.6rem .8rem;white-space:pre-wrap}\n\
         .old{background:#fff5f5;color:#7d2424;text-decoration:line-through}\n\
         .new{background:#f2fbf4;color:#1e5631}\n\
         .badge{display:inline-block;border-radius:4px;padding:.1em .5em;font-size:.8rem;margin-left:.5em}\n\
         .ok{background:#d9f2e0;color:#1e5631}.warn{background:#fde9d9;color:#8a4b16}\n\
         </style></head><body>\n",
    );
    let _ = writeln!(
        html,
        "<h1>Document diff <span class=\"badge {}\">{}</span></h1>",
        if chained { "ok" } else { "warn" },
        if chained {
            "amendment chain verified"
        } else {
            "no predecessor link"
        }
    );
    let _ = writeln!(
        html,
        "<p class=\"meta\">old: <code>{}</code><br>new: <code>{}</code></p>",
        old_id.to_hex(),
        new_id.to_hex()
    );

    if changes.is_empty() {
        html.push_str("<p>No structural differences.</p>\n");
    }
    for c in &changes {
        let _ = writeln!(
            html,
            "<div class=\"change\"><div class=\"path\">{}</div>",
            esc(&c.path)
        );
        if let Some(old_text) = &c.old {
            let _ = writeln!(
                html,
                "<div class=\"old\"><del>{}</del></div>",
                esc(old_text)
            );
        }
        if let Some(new_text) = &c.new {
            let _ = writeln!(
                html,
                "<div class=\"new\"><ins>{}</ins></div>",
                esc(new_text)
            );
        }
        html.push_str("</div>\n");
    }
    html.push_str("</body></html>\n");
    Ok(html)
}

fn walk(
    a: Option<&Node>,
    b: Option<&Node>,
    da: &Document,
    db: &Document,
    path: &mut Vec<usize>,
    out: &mut Vec<Change>,
) -> Result<()> {
    if out.len() >= 500 {
        return Ok(());
    }
    let resolve = |n: Option<&Node>, d: &Document| -> Result<Option<Node>> {
        Ok(match n {
            Some(Node::SubtreeRef(id)) => Some(Node::from_value(&d.store.get_value(id)?)?),
            Some(other) => Some(other.clone()),
            None => None,
        })
    };
    let a = resolve(a, da)?;
    let b = resolve(b, db)?;
    if a == b {
        return Ok(());
    }
    // Containers of the same kind: recurse pairwise over children.
    if let (Some(ac), Some(bc)) = (a.as_ref().and_then(children), b.as_ref().and_then(children)) {
        let n = ac.len().max(bc.len());
        for i in 0..n {
            path.push(i);
            walk(ac.get(i), bc.get(i), da, db, path, out)?;
            path.pop();
        }
        return Ok(());
    }
    out.push(Change {
        path: fmt_path(path),
        old: a.as_ref().map(|n| node_text(n, da)),
        new: b.as_ref().map(|n| node_text(n, db)),
    });
    Ok(())
}

/// Same-kind containers expose comparable child lists.
fn children(n: &Node) -> Option<Vec<Node>> {
    match n {
        Node::Doc(d) => Some(d.children.clone()),
        Node::Section(s) => Some(s.children.clone()),
        Node::Salted(s) => Some(vec![(*s.child).clone()]),
        _ => None,
    }
}

fn fmt_path(path: &[usize]) -> String {
    if path.is_empty() {
        "(root)".into()
    } else {
        path.iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }
}

/// Human-readable text of a standalone subtree (no store available —
/// unresolved refs print as placeholders). Used by `verify-disclosure`.
pub fn node_plain_text(node: &Node) -> String {
    // An empty document provides ref-resolution that always misses,
    // which renders as "[unresolvable subtree]" — correct for a
    // disclosure bundle, where siblings are deliberately absent.
    let empty = vsd_core::document::DocumentBuilder::new(Node::Doc(vsd_core::tree::Doc {
        lang: "und".into(),
        dir: vsd_core::tree::Direction::Ltr,
        writing_mode: vsd_core::tree::WritingMode::Horizontal,
        children: vec![],
    }))
    .build()
    .expect("empty doc");
    node_text(node, &empty)
}

/// Human-readable text of a subtree (kind-prefixed for non-text nodes).
fn node_text(node: &Node, doc: &Document) -> String {
    match node {
        Node::Para(p) => inline_text(&p.children),
        Node::Heading(h) => format!("[h{}] {}", h.level, inline_text(&h.children)),
        Node::Code(c) => format!("[code] {}", c.text),
        Node::List(l) => l
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let marker = if l.ordered {
                    format!("{}. ", i + 1)
                } else {
                    "• ".into()
                };
                format!(
                    "{marker}{}",
                    item.iter()
                        .map(|n| node_text(n, doc))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Node::Table(t) => t
            .head
            .iter()
            .chain(&t.body)
            .chain(&t.foot)
            .map(|row| {
                row.cells
                    .iter()
                    .map(|c| {
                        c.children
                            .iter()
                            .map(|n| node_text(n, doc))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .collect::<Vec<_>>()
                    .join(" | ")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Node::Figure(f) => format!("[figure: {}]", f.alt),
        Node::Field(f) => format!("[field {}]", f.id),
        Node::Math(m) => format!("[math] {}", m.mathml),
        Node::Redacted(r) => format!(
            "[REDACTED{}]",
            r.reason
                .as_deref()
                .map(|s| format!(": {s}"))
                .unwrap_or_default()
        ),
        Node::Section(s) => s
            .children
            .iter()
            .map(|n| node_text(n, doc))
            .collect::<Vec<_>>()
            .join("\n"),
        Node::Doc(d) => d
            .children
            .iter()
            .map(|n| node_text(n, doc))
            .collect::<Vec<_>>()
            .join("\n"),
        Node::SubtreeRef(id) => match doc.store.get_value(id).and_then(|v| Node::from_value(&v)) {
            Ok(n) => node_text(&n, doc),
            Err(_) => "[unresolvable subtree]".into(),
        },
        Node::PageBreakHint => "[page break]".into(),
        Node::Salted(s) => node_text(&s.child, doc),
    }
}

fn inline_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for i in inlines {
        match i {
            Inline::Text(s) => out.push_str(s),
            Inline::Span(sp) => out.push_str(&inline_text(&sp.children)),
            Inline::Link(l) => out.push_str(&inline_text(&l.children)),
            Inline::Math(m) => out.push_str(&m.mathml),
            Inline::FootnoteRef(id) => {
                let _ = write!(out, "[{id}]");
            }
        }
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vsd_core::compose::Compose;

    #[test]
    fn redline_shows_old_and_new() {
        let old = Compose::new("en")
            .h1("Agreement")
            .para("The fee is $400 per month.")
            .finish()
            .unwrap();
        let redacted = vsd_core::redact::redact(&old, &[1], Some("fee".into())).unwrap();

        let html = render_diff_html(&old, &redacted.document).unwrap();
        assert!(html.contains("<del>The fee is $400 per month.</del>"));
        assert!(html.contains("<ins>[REDACTED: fee]</ins>"));
        assert!(html.contains("amendment chain verified"));
        assert!(!html.to_lowercase().contains("<script"), "no JS, by policy");
    }
}
