//! Markdown → VSD authoring (ROADMAP 3d): the cheap on-ramp. Every
//! README, invoice template, and static-site pipeline becomes a VSD
//! producer. CommonMark + tables via pulldown-cmark.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use vsd_core::document::DocumentBuilder;
use vsd_core::manifest::{Blob, Metadata, Profile, ResourceEntry, ResourceKind, Style};
use vsd_core::tree::{
    Cell, CellScope, Code, ColSpec, Direction, Doc, Figure, Heading, Inline, Link, List, Node,
    Para, Row, Section, Span, Table,
};
use vsd_core::{Document, ResourceTable};

pub fn document_from_markdown(md: &str, base_dir: &Path, profile: Profile) -> Result<Document> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    let mut b = Builder::new(base_dir);
    for event in Parser::new_ext(md, opts) {
        b.event(event)?;
    }
    b.finish(profile)
}

struct Builder<'a> {
    base_dir: &'a Path,
    /// Stack of block containers: [0] is the document body; items,
    /// blockquotes, and table cells push frames.
    blocks: Vec<Vec<Node>>,
    /// Stack of inline containers with the wrapper to apply at end.
    inlines: Vec<(InlineWrap, Vec<Inline>)>,
    /// (ordered, collected items) for nested lists.
    lists: Vec<(bool, Vec<Vec<Node>>)>,
    /// In-progress table: (header rows done?, head, body, columns).
    table: Option<TableState>,
    code: Option<(Option<String>, String)>,
    image: Option<(String, String)>, // (src, alt-in-progress)
    heading: Option<u8>,
    styles: Vec<Style>,
    style_idx: BTreeMap<(bool, bool, bool, bool), u64>,
    resources: Vec<(String, ResourceEntry)>,
    blobs: Vec<Blob>,
    title: Option<String>,
}

enum InlineWrap {
    Plain,
    Strong,
    Emph,
    Strike,
    Link(String),
}

struct TableState {
    in_head: bool,
    head: Vec<Row>,
    body: Vec<Row>,
    row: Vec<Cell>,
    cols: usize,
}

impl<'a> Builder<'a> {
    fn new(base_dir: &'a Path) -> Self {
        Builder {
            base_dir,
            blocks: vec![Vec::new()],
            inlines: Vec::new(),
            lists: Vec::new(),
            table: None,
            code: None,
            image: None,
            heading: None,
            styles: Vec::new(),
            style_idx: BTreeMap::new(),
            resources: Vec::new(),
            blobs: Vec::new(),
            title: None,
        }
    }

    fn push_block(&mut self, node: Node) {
        self.blocks.last_mut().expect("block frame").push(node);
    }

    fn push_inline(&mut self, inline: Inline) {
        if let Some((_, frame)) = self.inlines.last_mut() {
            frame.push(inline);
        } else {
            // Stray inline outside any block: wrap in a paragraph.
            self.push_block(Node::Para(Para {
                children: vec![inline],
            }));
        }
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

    fn event(&mut self, event: Event<'_>) -> Result<()> {
        match event {
            Event::Start(tag) => self.start(tag)?,
            Event::End(tag) => self.end(tag)?,
            Event::Text(t) => {
                if let Some((_, alt)) = &mut self.image {
                    alt.push_str(&t);
                } else if let Some((_, buf)) = &mut self.code {
                    buf.push_str(&t);
                } else {
                    self.push_inline(Inline::Text(t.into_string()));
                }
            }
            Event::Code(t) => {
                let style = self.style((false, false, false, true));
                self.push_inline(Inline::Span(Span {
                    style: Some(style),
                    children: vec![Inline::Text(t.into_string())],
                }));
            }
            Event::SoftBreak | Event::HardBreak => self.push_inline(Inline::Text(" ".into())),
            Event::Rule => {} // thematic break: no VSD equivalent; dropped
            Event::Html(_) | Event::InlineHtml(_) => {} // no executable/foreign content
            _ => {}
        }
        Ok(())
    }

    fn start(&mut self, tag: Tag<'_>) -> Result<()> {
        match tag {
            Tag::Paragraph => self.inlines.push((InlineWrap::Plain, Vec::new())),
            Tag::Heading { level, .. } => {
                self.heading = Some(heading_level(level));
                self.inlines.push((InlineWrap::Plain, Vec::new()));
            }
            Tag::Strong => self.inlines.push((InlineWrap::Strong, Vec::new())),
            Tag::Emphasis => self.inlines.push((InlineWrap::Emph, Vec::new())),
            Tag::Strikethrough => self.inlines.push((InlineWrap::Strike, Vec::new())),
            Tag::Link { dest_url, .. } => self
                .inlines
                .push((InlineWrap::Link(dest_url.into_string()), Vec::new())),
            Tag::Image { dest_url, .. } => {
                self.image = Some((dest_url.into_string(), String::new()));
            }
            Tag::List(start) => self.lists.push((start.is_some(), Vec::new())),
            Tag::Item => self.blocks.push(Vec::new()),
            Tag::BlockQuote(_) => self.blocks.push(Vec::new()),
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(l) if !l.is_empty() => Some(l.into_string()),
                    _ => None,
                };
                self.code = Some((lang, String::new()));
            }
            Tag::Table(alignments) => {
                self.table = Some(TableState {
                    in_head: false,
                    head: Vec::new(),
                    body: Vec::new(),
                    row: Vec::new(),
                    cols: alignments.len(),
                });
            }
            Tag::TableHead => {
                if let Some(t) = &mut self.table {
                    t.in_head = true;
                }
            }
            Tag::TableRow => {}
            Tag::TableCell => self.inlines.push((InlineWrap::Plain, Vec::new())),
            _ => {}
        }
        Ok(())
    }

    fn end(&mut self, tag: TagEnd) -> Result<()> {
        match tag {
            TagEnd::Paragraph => {
                let (_, children) = self.inlines.pop().context("paragraph frame")?;
                if !children.is_empty() {
                    self.push_block(Node::Para(Para { children }));
                }
            }
            TagEnd::Heading(_) => {
                let (_, children) = self.inlines.pop().context("heading frame")?;
                let level = self.heading.take().unwrap_or(1);
                if self.title.is_none() && level == 1 {
                    self.title = Some(inline_text(&children));
                }
                self.push_block(Node::Heading(Heading { level, children }));
            }
            TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough => {
                let (wrap, children) = self.inlines.pop().context("style frame")?;
                let key = match wrap {
                    InlineWrap::Strong => (true, false, false, false),
                    InlineWrap::Emph => (false, true, false, false),
                    _ => (false, false, true, false), // strike ≈ underline-class styling
                };
                let style = self.style(key);
                self.push_inline(Inline::Span(Span {
                    style: Some(style),
                    children,
                }));
            }
            TagEnd::Link => {
                let (wrap, children) = self.inlines.pop().context("link frame")?;
                let InlineWrap::Link(href) = wrap else {
                    anyhow::bail!("mismatched link frame");
                };
                self.push_inline(Inline::Link(Link { href, children }));
            }
            TagEnd::Image => {
                let (src, alt) = self.image.take().context("image frame")?;
                self.add_figure(&src, &alt)?;
            }
            TagEnd::Item => {
                let item = self.blocks.pop().context("item frame")?;
                self.lists.last_mut().context("list frame")?.1.push(item);
            }
            TagEnd::List(_) => {
                let (ordered, items) = self.lists.pop().context("list frame")?;
                self.push_block(Node::List(List { ordered, items }));
            }
            TagEnd::BlockQuote(_) => {
                let children = self.blocks.pop().context("quote frame")?;
                self.push_block(Node::Section(Section {
                    role: "quote".into(),
                    children,
                }));
            }
            TagEnd::CodeBlock => {
                let (lang, mut text) = self.code.take().context("code frame")?;
                if text.ends_with('\n') {
                    text.pop();
                }
                self.push_block(Node::Code(Code { lang, text }));
            }
            TagEnd::TableCell => {
                let (_, children) = self.inlines.pop().context("cell frame")?;
                if let Some(t) = &mut self.table {
                    t.row.push(Cell {
                        span: None,
                        scope: t.in_head.then_some(CellScope::Col),
                        children: vec![Node::Para(Para { children })],
                    });
                }
            }
            TagEnd::TableHead => {
                if let Some(t) = &mut self.table {
                    let cells = std::mem::take(&mut t.row);
                    t.head.push(Row { cells });
                    t.in_head = false;
                }
            }
            TagEnd::TableRow => {
                if let Some(t) = &mut self.table {
                    let cells = std::mem::take(&mut t.row);
                    t.body.push(Row { cells });
                }
            }
            TagEnd::Table => {
                let t = self.table.take().context("table frame")?;
                self.push_block(Node::Table(Table {
                    cols: vec![ColSpec { width: None }; t.cols.max(1)],
                    head: t.head,
                    body: t.body,
                    foot: vec![],
                }));
            }
            _ => {}
        }
        Ok(())
    }

    fn add_figure(&mut self, src: &str, alt: &str) -> Result<()> {
        let path = self.base_dir.join(src);
        let data = std::fs::read(&path)
            .with_context(|| format!("reading image {} (from markdown)", path.display()))?;
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
        anyhow::ensure!(
            !alt.trim().is_empty(),
            "markdown image {src:?} needs alt text (![alt](src)) — accessibility is a \
             validity condition in VSD"
        );
        self.push_block(Node::Figure(Figure {
            res: id,
            alt: alt.to_owned(),
            decorative: false,
            caption: vec![],
        }));
        Ok(())
    }

    fn finish(mut self, profile: Profile) -> Result<Document> {
        anyhow::ensure!(self.blocks.len() == 1, "unbalanced markdown structure");
        let root = Node::Doc(Doc {
            lang: "en".into(),
            dir: Direction::Ltr,
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

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
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
            Inline::FootnoteRef(id) => out.push_str(id),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_maps_to_tree() {
        let md = "# Title\n\nHello **bold** and [link](https://example.com).\n\n\
                  - one\n- two\n\n```rust\nfn main() {}\n```\n\n\
                  | A | B |\n|---|---|\n| 1 | 2 |\n\n> quoted\n";
        let doc = document_from_markdown(md, Path::new("."), Profile::Core).unwrap();
        assert!(vsd_core::validate::validate(&doc).is_valid());
        assert_eq!(doc.metadata().unwrap().title.as_deref(), Some("Title"));

        let Node::Doc(d) = doc.root_node().unwrap() else {
            panic!()
        };
        assert!(matches!(&d.children[0], Node::Heading(h) if h.level == 1));
        assert!(matches!(&d.children[1], Node::Para(_)));
        assert!(matches!(&d.children[2], Node::List(l) if !l.ordered && l.items.len() == 2));
        assert!(
            matches!(&d.children[3], Node::Code(c) if c.lang.as_deref() == Some("rust") && c.text == "fn main() {}")
        );
        assert!(matches!(&d.children[4], Node::Table(t) if t.head.len() == 1 && t.body.len() == 1));
        assert!(matches!(&d.children[5], Node::Section(s) if s.role == "quote"));

        let text = vsd_core::extract::extract_text(&doc).unwrap();
        assert!(text.contains("Hello bold and link."));
    }

    #[test]
    fn image_without_alt_is_rejected() {
        // (No file read happens: alt check is after read, so point at a
        // real file — use Cargo.toml as a stand-in image.)
        let md = "![](Cargo.toml)\n";
        let err = document_from_markdown(md, Path::new("."), Profile::Core);
        assert!(err.is_err());
    }
}
