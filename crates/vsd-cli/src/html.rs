//! HTML → VSD import (the 3d HTML on-ramp).
//!
//! A pragmatic mapping of common HTML to the content tree, mirroring the
//! Markdown importer's discipline: headings keep their level, lists and
//! tables their structure, links and inline styling carry over, image
//! `alt` text is **required** (accessibility is a validity condition),
//! and `<script>`/`<style>`/foreign content is dropped — no executable
//! or non-content payload ever enters the tree.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tl::{Node as HNode, NodeHandle, Parser};

use vsd_core::document::DocumentBuilder;
use vsd_core::manifest::{Blob, Metadata, Profile, ResourceEntry, ResourceKind, Style};
use vsd_core::tree::{
    Cell, CellScope, Code, ColSpec, Direction, Doc, Figure, Heading, Inline, Link, List, Node,
    Para, Row, Section, Span, Table,
};
use vsd_core::{Document, ResourceTable};

pub fn document_from_html(html: &str, base_dir: &Path, profile: Profile) -> Result<Document> {
    let dom = tl::parse(html, tl::ParserOptions::default())
        .map_err(|e| anyhow::anyhow!("HTML parse error: {e}"))?;
    let parser = dom.parser();
    let mut b = Builder {
        base_dir: base_dir.to_path_buf(),
        blocks: vec![Vec::new()],
        styles: Vec::new(),
        style_idx: BTreeMap::new(),
        resources: Vec::new(),
        blobs: Vec::new(),
        title: None,
    };
    let top: Vec<NodeHandle> = dom.children().to_vec();
    b.block_children(&top, parser)?;
    b.finish(profile)
}

struct Builder {
    base_dir: PathBuf,
    /// Stack of block frames (for lists / quotes / cells, like the
    /// Markdown importer); the bottom frame is the document body.
    blocks: Vec<Vec<Node>>,
    styles: Vec<Style>,
    style_idx: BTreeMap<(bool, bool, bool, bool), u64>,
    resources: Vec<(String, ResourceEntry)>,
    blobs: Vec<Blob>,
    title: Option<String>,
}

impl Builder {
    fn push_block(&mut self, node: Node) {
        self.blocks.last_mut().expect("block frame").push(node);
    }

    fn style(&mut self, key: (bool, bool, bool, bool)) -> u64 {
        if let Some(&i) = self.style_idx.get(&key) {
            return i;
        }
        let i = self.styles.len() as u64;
        self.styles.push(Style {
            bold: key.0,
            italic: key.1,
            underline: key.2,
            mono: key.3,
        });
        self.style_idx.insert(key, i);
        i
    }

    /// Walk a sequence of sibling nodes as block content, gathering loose
    /// inline runs (`text <b>x</b>`) into implicit paragraphs.
    fn block_children(&mut self, children: &[NodeHandle], parser: &Parser) -> Result<()> {
        let mut pending: Vec<Inline> = Vec::new();
        for h in children {
            let Some(node) = h.get(parser) else { continue };
            match node {
                HNode::Raw(bytes) => {
                    let t = collapse(&decode_entities(&bytes.as_utf8_str()));
                    if !t.trim().is_empty() {
                        pending.push(Inline::Text(t));
                    }
                }
                HNode::Comment(_) => {}
                HNode::Tag(tag) => {
                    let name = tag.name().as_utf8_str().to_ascii_lowercase();
                    let kids = tag.children();
                    let kids = kids.top().as_slice();
                    if is_skipped(&name) {
                        if name == "title" && self.title.is_none() {
                            let t = collapse(&decode_entities(&node.inner_text(parser)));
                            if !t.trim().is_empty() {
                                self.title = Some(t.trim().to_owned());
                            }
                        }
                        continue;
                    }
                    if is_block(&name) {
                        flush_para(&mut pending, self);
                        self.block_tag(&name, h, kids, parser)?;
                    } else if is_transparent(&name) {
                        flush_para(&mut pending, self);
                        self.block_children(kids, parser)?;
                    } else {
                        // Inline element sitting directly in block flow.
                        self.collect_inline(&mut pending, node, parser)?;
                    }
                }
            }
        }
        flush_para(&mut pending, self);
        Ok(())
    }

    fn block_tag(
        &mut self,
        name: &str,
        handle: &NodeHandle,
        kids: &[NodeHandle],
        parser: &Parser,
    ) -> Result<()> {
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = name.as_bytes()[1] - b'0';
                let children = self.inlines(kids, parser)?;
                if !children.is_empty() {
                    if self.title.is_none() && level == 1 {
                        self.title = Some(inline_text(&children));
                    }
                    self.push_block(Node::Heading(Heading { level, children }));
                }
            }
            "p" => {
                let children = self.inlines(kids, parser)?;
                if !children.is_empty() {
                    self.push_block(Node::Para(Para { children }));
                }
            }
            "ul" | "ol" => {
                let ordered = name == "ol";
                let mut items = Vec::new();
                for li in kids {
                    let Some(HNode::Tag(t)) = li.get(parser) else {
                        continue;
                    };
                    if t.name().as_utf8_str().to_ascii_lowercase() != "li" {
                        continue;
                    }
                    self.blocks.push(Vec::new());
                    let li_kids = t.children();
                    self.block_children(li_kids.top().as_slice(), parser)?;
                    let item = self.blocks.pop().expect("li frame");
                    if !item.is_empty() {
                        items.push(item);
                    }
                }
                if !items.is_empty() {
                    self.push_block(Node::List(List { ordered, items }));
                }
            }
            "pre" => {
                // Preserve verbatim text; unwrap a single <code> child.
                let text = decode_entities(&node_inner_text(handle, parser));
                let text = text.strip_suffix('\n').unwrap_or(&text).to_owned();
                if !text.is_empty() {
                    self.push_block(Node::Code(Code { lang: None, text }));
                }
            }
            "blockquote" => {
                self.blocks.push(Vec::new());
                self.block_children(kids, parser)?;
                let children = self.blocks.pop().expect("quote frame");
                if !children.is_empty() {
                    self.push_block(Node::Section(Section {
                        role: "quote".into(),
                        columns: 1,
                        children,
                    }));
                }
            }
            "section" | "article" | "aside" => {
                self.blocks.push(Vec::new());
                self.block_children(kids, parser)?;
                let children = self.blocks.pop().expect("section frame");
                if !children.is_empty() {
                    self.push_block(Node::Section(Section {
                        role: name.to_owned(),
                        columns: 1,
                        children,
                    }));
                }
            }
            "table" => {
                if let Some(table) = self.build_table(kids, parser)? {
                    self.push_block(table);
                }
            }
            "figure" => {
                // A <figure> usually wraps an <img> (+ optional caption).
                self.block_children(kids, parser)?;
            }
            "img" => {
                if let Some(HNode::Tag(t)) = handle.get(parser) {
                    let src = attr(t, "src").unwrap_or_default();
                    let alt = attr(t, "alt").unwrap_or_default();
                    self.add_figure(&src, &alt)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn build_table(&mut self, kids: &[NodeHandle], parser: &Parser) -> Result<Option<Node>> {
        let mut head = Vec::new();
        let mut body = Vec::new();
        let mut foot = Vec::new();
        let mut max_cols = 0usize;
        // Rows may be direct children or grouped in thead/tbody/tfoot.
        for grp in kids {
            let Some(HNode::Tag(t)) = grp.get(parser) else {
                continue;
            };
            let gname = t.name().as_utf8_str().to_ascii_lowercase();
            let gkids = t.children();
            match gname.as_str() {
                "thead" => {
                    self.collect_rows(gkids.top().as_slice(), parser, &mut head, &mut max_cols)?
                }
                "tfoot" => {
                    self.collect_rows(gkids.top().as_slice(), parser, &mut foot, &mut max_cols)?
                }
                "tbody" => {
                    self.collect_rows(gkids.top().as_slice(), parser, &mut body, &mut max_cols)?
                }
                "tr" => {
                    if let Some(row) =
                        self.build_row(gkids.top().as_slice(), parser, &mut max_cols)?
                    {
                        body.push(row);
                    }
                }
                _ => {}
            }
        }
        if head.is_empty() && body.is_empty() && foot.is_empty() {
            return Ok(None);
        }
        let cols = (0..max_cols).map(|_| ColSpec { width: None }).collect();
        Ok(Some(Node::Table(Table {
            cols,
            head,
            body,
            foot,
        })))
    }

    fn collect_rows(
        &mut self,
        kids: &[NodeHandle],
        parser: &Parser,
        out: &mut Vec<Row>,
        max_cols: &mut usize,
    ) -> Result<()> {
        for tr in kids {
            let Some(HNode::Tag(t)) = tr.get(parser) else {
                continue;
            };
            if t.name().as_utf8_str().to_ascii_lowercase() != "tr" {
                continue;
            }
            let trk = t.children();
            if let Some(row) = self.build_row(trk.top().as_slice(), parser, max_cols)? {
                out.push(row);
            }
        }
        Ok(())
    }

    fn build_row(
        &mut self,
        kids: &[NodeHandle],
        parser: &Parser,
        max_cols: &mut usize,
    ) -> Result<Option<Row>> {
        let mut cells = Vec::new();
        for c in kids {
            let Some(HNode::Tag(t)) = c.get(parser) else {
                continue;
            };
            let cname = t.name().as_utf8_str().to_ascii_lowercase();
            let scope = match cname.as_str() {
                "th" => Some(CellScope::Col),
                "td" => None,
                _ => continue,
            };
            let ck = t.children();
            let children = self.inlines(ck.top().as_slice(), parser)?;
            cells.push(Cell {
                span: None,
                scope,
                children: vec![Node::Para(Para { children })],
            });
        }
        if cells.is_empty() {
            return Ok(None);
        }
        *max_cols = (*max_cols).max(cells.len());
        Ok(Some(Row { cells }))
    }

    /// Collect the inline content of a sequence of nodes.
    fn inlines(&mut self, children: &[NodeHandle], parser: &Parser) -> Result<Vec<Inline>> {
        let mut out = Vec::new();
        for h in children {
            if let Some(node) = h.get(parser) {
                self.collect_inline(&mut out, node, parser)?;
            }
        }
        trim_edges(&mut out);
        Ok(out)
    }

    fn collect_inline(
        &mut self,
        out: &mut Vec<Inline>,
        node: &HNode,
        parser: &Parser,
    ) -> Result<()> {
        match node {
            HNode::Raw(bytes) => {
                let t = collapse(&decode_entities(&bytes.as_utf8_str()));
                if !t.is_empty() {
                    out.push(Inline::Text(t));
                }
            }
            HNode::Comment(_) => {}
            HNode::Tag(tag) => {
                let name = tag.name().as_utf8_str().to_ascii_lowercase();
                let kids = tag.children();
                let kids = kids.top().as_slice();
                match name.as_str() {
                    "br" => out.push(Inline::Text(" ".into())),
                    "a" => {
                        let href = attr(tag, "href").unwrap_or_default();
                        let children = self.inlines(kids, parser)?;
                        if href.is_empty() {
                            out.extend(children);
                        } else {
                            out.push(Inline::Link(Link { href, children }));
                        }
                    }
                    "strong" | "b" => {
                        self.styled(out, (true, false, false, false), kids, parser)?
                    }
                    "em" | "i" => self.styled(out, (false, true, false, false), kids, parser)?,
                    "u" | "ins" => self.styled(out, (false, false, true, false), kids, parser)?,
                    "code" | "kbd" | "samp" | "tt" => {
                        self.styled(out, (false, false, false, true), kids, parser)?
                    }
                    // Skip embedded scripts/styles entirely.
                    n if is_skipped(n) => {}
                    // Any other inline/unknown element: take its text.
                    _ => {
                        let inner = self.inlines(kids, parser)?;
                        out.extend(inner);
                    }
                }
            }
        }
        Ok(())
    }

    fn styled(
        &mut self,
        out: &mut Vec<Inline>,
        key: (bool, bool, bool, bool),
        kids: &[NodeHandle],
        parser: &Parser,
    ) -> Result<()> {
        let children = self.inlines(kids, parser)?;
        if children.is_empty() {
            return Ok(());
        }
        let style = self.style(key);
        out.push(Inline::Span(Span {
            style: Some(style),
            children,
        }));
        Ok(())
    }

    fn add_figure(&mut self, src: &str, alt: &str) -> Result<()> {
        anyhow::ensure!(
            !alt.trim().is_empty(),
            "HTML <img src={src:?}> needs alt text — accessibility is a validity condition in VSD"
        );
        let path = self.base_dir.join(src);
        let data = std::fs::read(&path)
            .with_context(|| format!("reading image {} (from HTML)", path.display()))?;
        let mime = match src.rsplit('.').next().map(str::to_lowercase).as_deref() {
            Some("png") => "image/png",
            Some("jpg") | Some("jpeg") => "image/jpeg",
            Some("svg") => "image/svg+xml",
            _ => "application/octet-stream",
        }
        .to_owned();
        let blob = Blob {
            mime: mime.clone(),
            data,
        };
        let id = vsd_core::ObjectId::of_value(&blob.to_value())?;
        let name = format!("img{}", self.resources.len());
        self.resources.push((
            name,
            ResourceEntry {
                kind: if mime == "image/svg+xml" {
                    ResourceKind::Vector
                } else {
                    ResourceKind::Image
                },
                mime,
                data: id,
            },
        ));
        self.blobs.push(blob);
        self.push_block(Node::Figure(Figure {
            res: id,
            alt: alt.trim().to_owned(),
            decorative: false,
            caption: vec![],
        }));
        Ok(())
    }

    fn finish(mut self, profile: Profile) -> Result<Document> {
        anyhow::ensure!(self.blocks.len() == 1, "unbalanced HTML structure");
        let root = Node::Doc(Doc {
            lang: "en".into(),
            dir: Direction::Ltr,
            writing_mode: vsd_core::tree::WritingMode::Horizontal,
            children: self.blocks.pop().expect("body"),
        });
        let mut builder = DocumentBuilder::new(root)
            .metadata(Metadata {
                title: self.title,
                ..Default::default()
            })
            .profile(profile);
        for blob in &self.blobs {
            builder.add_object(blob.to_value())?;
        }
        builder = builder.resources(ResourceTable {
            entries: self.resources,
            styles: self.styles,
        });
        Ok(builder.build()?)
    }
}

/// Block-level elements that start their own VSD block.
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "h1" | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "p"
            | "ul"
            | "ol"
            | "pre"
            | "blockquote"
            | "section"
            | "article"
            | "aside"
            | "table"
            | "figure"
            | "img"
    )
}

/// Containers we flow through transparently (their children are blocks).
fn is_transparent(name: &str) -> bool {
    matches!(
        name,
        "html" | "body" | "main" | "div" | "header" | "footer" | "nav" | "hgroup" | "fieldset"
    )
}

/// Elements dropped wholesale — no executable or non-content payload
/// enters the tree (the same policy as the Markdown importer's HTML drop).
fn is_skipped(name: &str) -> bool {
    matches!(
        name,
        "head" | "script" | "style" | "template" | "noscript" | "title" | "meta" | "link" | "svg"
    )
}

fn attr(tag: &tl::HTMLTag, key: &str) -> Option<String> {
    tag.attributes()
        .get(key)
        .flatten()
        .map(|b| b.as_utf8_str().into_owned())
}

fn node_inner_text(handle: &NodeHandle, parser: &Parser) -> String {
    handle
        .get(parser)
        .map(|n| n.inner_text(parser).into_owned())
        .unwrap_or_default()
}

fn flush_para(pending: &mut Vec<Inline>, b: &mut Builder) {
    if pending.is_empty() {
        return;
    }
    let mut children = std::mem::take(pending);
    trim_edges(&mut children);
    if !children.is_empty() && inline_text(&children).trim() != "" {
        b.push_block(Node::Para(Para { children }));
    }
}

/// Trim a leading space on the first text run and a trailing space on the
/// last (HTML inter-element whitespace), without disturbing interior gaps.
fn trim_edges(inlines: &mut [Inline]) {
    if let Some(Inline::Text(t)) = inlines.first_mut() {
        let trimmed = t.trim_start().to_owned();
        *t = trimmed;
    }
    if let Some(Inline::Text(t)) = inlines.last_mut() {
        let trimmed = t.trim_end().to_owned();
        *t = trimmed;
    }
}

/// Collapse runs of ASCII/Unicode whitespace to single spaces, keeping a
/// single leading/trailing space when the source had one (so inline runs
/// stay separated). `<pre>` text bypasses this.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out
}

fn inline_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for i in inlines {
        match i {
            Inline::Text(s) => out.push_str(s),
            Inline::Span(sp) => out.push_str(&inline_text(&sp.children)),
            Inline::Link(l) => out.push_str(&inline_text(&l.children)),
            Inline::Math(m) => out.push_str(&m.mathml),
            Inline::FootnoteRef(id) => out.push_str(id),
        }
    }
    out
}

/// Decode the HTML character references we care about: all numeric
/// (`&#160;`, `&#xA0;`) plus a compact table of common named entities.
/// Unknown named entities are left verbatim (an importer should not
/// silently drop text it doesn't recognize).
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        // A reference ends at ';' within a short window; otherwise it's a
        // literal ampersand.
        match after.find(';').filter(|&i| i <= 31) {
            Some(semi) => {
                let ent = &after[..semi];
                let decoded =
                    if let Some(hex) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                        u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                    } else if let Some(dec) = ent.strip_prefix('#') {
                        dec.parse::<u32>().ok().and_then(char::from_u32)
                    } else {
                        named_entity(ent)
                    };
                match decoded {
                    Some(c) => {
                        out.push(c);
                        rest = &after[semi + 1..];
                    }
                    None => {
                        out.push('&');
                        rest = after;
                    }
                }
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{00A0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "times" => '×',
        "divide" => '÷',
        "deg" => '°',
        "plusmn" => '±',
        "euro" => '€',
        "pound" => '£',
        "cent" => '¢',
        "sect" => '§',
        "para" => '¶',
        "middot" => '·',
        "bull" => '•',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(html: &str) -> Document {
        document_from_html(html, Path::new("."), Profile::Core).unwrap()
    }
    fn body(d: &Document) -> Vec<Node> {
        match d.root_node().unwrap() {
            Node::Doc(doc) => doc.children,
            _ => panic!("doc root"),
        }
    }

    #[test]
    fn maps_headings_paragraphs_lists() {
        let d = doc("<html><head><title>T</title></head><body>\
             <h1>Title</h1><p>Hello <strong>world</strong> and <a href='/x'>link</a>.</p>\
             <ul><li>one</li><li>two</li></ul>\
             <ol><li>first</li></ol></body></html>");
        assert!(vsd_core::validate::validate(&d).is_valid());
        assert_eq!(d.metadata().unwrap().title.as_deref(), Some("Title"));
        let b = body(&d);
        assert!(matches!(&b[0], Node::Heading(h) if h.level == 1));
        assert!(matches!(&b[1], Node::Para(_)));
        match &b[2] {
            Node::List(l) => {
                assert!(!l.ordered);
                assert_eq!(l.items.len(), 2);
            }
            _ => panic!("expected ul"),
        }
        assert!(matches!(&b[3], Node::List(l) if l.ordered));
        // The link and the bold span survived into the paragraph.
        let text = vsd_core::extract::extract_text(&d).unwrap();
        assert!(text.contains("Hello world and link."), "text: {text}");
    }

    #[test]
    fn table_with_header_scope() {
        let d = doc("<table><thead><tr><th>Name</th><th>Qty</th></tr></thead>\
             <tbody><tr><td>Widget</td><td>7</td></tr></tbody></table>");
        let b = body(&d);
        let Node::Table(t) = &b[0] else {
            panic!("expected table");
        };
        assert_eq!(t.head.len(), 1);
        assert_eq!(t.body.len(), 1);
        assert_eq!(t.head[0].cells.len(), 2);
        assert_eq!(t.head[0].cells[0].scope, Some(CellScope::Col));
        assert_eq!(t.body[0].cells[0].scope, None);
    }

    #[test]
    fn decodes_entities_and_drops_scripts() {
        let d = doc("<body><p>A &amp; B &copy; &#169; &#x2014; end</p>\
             <script>alert('x')</script><style>p{color:red}</style></body>");
        let text = vsd_core::extract::extract_text(&d).unwrap();
        assert!(text.contains("A & B © © — end"), "text: {text}");
        assert!(!text.contains("alert"), "script content dropped");
        assert!(!text.contains("color"), "style content dropped");
    }

    #[test]
    fn preserves_pre_code() {
        let d = doc("<pre><code>fn main() {}\n</code></pre>");
        let b = body(&d);
        assert!(matches!(&b[0], Node::Code(c) if c.text == "fn main() {}"));
    }

    #[test]
    fn image_requires_alt_text() {
        match document_from_html("<img src='x.png'>", Path::new("."), Profile::Core) {
            Ok(_) => panic!("an <img> without alt must be refused"),
            Err(e) => assert!(format!("{e:#}").contains("alt text")),
        }
    }

    #[test]
    fn unknown_entity_kept_verbatim() {
        let d = doc("<p>a &notanentity; b</p>");
        let text = vsd_core::extract::extract_text(&d).unwrap();
        assert!(text.contains("&notanentity;"), "text: {text}");
    }
}
