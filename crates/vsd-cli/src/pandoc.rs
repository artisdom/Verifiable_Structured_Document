//! Pandoc JSON AST → VSD import (ROADMAP 4d).
//!
//! Pandoc reads dozens of formats (docx, rst, LaTeX, Org, EPUB, …); its
//! `-t json` AST is a stable, documented tree. Consuming it here turns
//! all of those into a VSD on-ramp in one importer:
//!
//! ```text
//! pandoc paper.docx -t json | vsd pack-pandoc -o paper.vsd
//! ```
//!
//! The mapping follows the same discipline as the Markdown and HTML
//! importers: structure and text carry over, image `alt` is **required**,
//! and raw/foreign content (`RawBlock`/`RawInline`, notes) is dropped —
//! nothing executable or non-content enters the tree. TeX math has no
//! faithful MathML conversion here, so it is preserved as its source text
//! rather than mis-converted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value as J;

use vsd_core::document::DocumentBuilder;
use vsd_core::manifest::{Blob, Metadata, Profile, ResourceEntry, ResourceKind, Style};
use vsd_core::tree::{
    Cell, CellScope, Code, ColSpec, Direction, Doc, Figure, Heading, Inline, Link, List, Node,
    Para, Row, Section, Span, Table,
};
use vsd_core::{Document, ResourceTable};

pub fn document_from_pandoc(json: &str, base_dir: &Path, profile: Profile) -> Result<Document> {
    let root: J = serde_json::from_str(json).context("parsing Pandoc JSON AST")?;
    anyhow::ensure!(
        root.get("blocks").is_some() && root.get("pandoc-api-version").is_some(),
        "not a Pandoc JSON AST (expected `pandoc-api-version` and `blocks`); \
         produce one with `pandoc -t json`"
    );
    let mut b = Builder {
        base_dir: base_dir.to_path_buf(),
        blocks: vec![Vec::new()],
        styles: Vec::new(),
        style_idx: BTreeMap::new(),
        resources: Vec::new(),
        blobs: Vec::new(),
        title: meta_title(&root),
    };
    if let Some(arr) = root.get("blocks").and_then(J::as_array) {
        b.blocks_into_current(arr)?;
    }
    b.finish(profile)
}

struct Builder {
    base_dir: PathBuf,
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

    /// Walk a fresh `[Block]` list into a new frame, returning its nodes.
    fn blocks_frame(&mut self, arr: &[J]) -> Result<Vec<Node>> {
        self.blocks.push(Vec::new());
        self.blocks_into_current(arr)?;
        Ok(self.blocks.pop().expect("frame"))
    }

    fn blocks_into_current(&mut self, arr: &[J]) -> Result<()> {
        for blk in arr {
            self.block(blk)?;
        }
        Ok(())
    }

    fn block(&mut self, blk: &J) -> Result<()> {
        let Some(t) = blk.get("t").and_then(J::as_str) else {
            return Ok(());
        };
        let c = blk.get("c");
        match t {
            "Para" | "Plain" => {
                let children =
                    self.inlines(c.and_then(J::as_array).map(Vec::as_slice).unwrap_or(&[]))?;
                if !children.is_empty() {
                    self.push_block(Node::Para(Para { children }));
                }
            }
            "LineBlock" => {
                // [[Inline]] — join the lines into one paragraph.
                let mut children = Vec::new();
                if let Some(lines) = c.and_then(J::as_array) {
                    for (i, line) in lines.iter().enumerate() {
                        if i > 0 {
                            children.push(Inline::Text(" ".into()));
                        }
                        children.extend(
                            self.inlines(line.as_array().map(Vec::as_slice).unwrap_or(&[]))?,
                        );
                    }
                }
                if !children.is_empty() {
                    self.push_block(Node::Para(Para { children }));
                }
            }
            "Header" => {
                // [Int, Attr, [Inline]]
                let level = c
                    .and_then(|c| c.get(0))
                    .and_then(J::as_u64)
                    .unwrap_or(1)
                    .clamp(1, 6) as u8;
                let children = self.inlines(arr_at(c, 2))?;
                if !children.is_empty() {
                    if self.title.is_none() && level == 1 {
                        self.title = Some(inline_text(&children));
                    }
                    self.push_block(Node::Heading(Heading { level, children }));
                }
            }
            "CodeBlock" => {
                // [Attr, Text] — Attr = [id, [classes], [[k,v]]]
                let lang = c
                    .and_then(|c| c.get(0))
                    .and_then(|a| a.get(1))
                    .and_then(J::as_array)
                    .and_then(|cl| cl.first())
                    .and_then(J::as_str)
                    .map(str::to_owned);
                let text = c
                    .and_then(|c| c.get(1))
                    .and_then(J::as_str)
                    .unwrap_or("")
                    .to_owned();
                if !text.is_empty() {
                    self.push_block(Node::Code(Code { lang, text }));
                }
            }
            "BlockQuote" => {
                let children =
                    self.blocks_frame(c.and_then(J::as_array).map(Vec::as_slice).unwrap_or(&[]))?;
                if !children.is_empty() {
                    self.push_block(Node::Section(Section {
                        role: "quote".into(),
                        columns: 1,
                        children,
                    }));
                }
            }
            "BulletList" => self.list(
                c.and_then(J::as_array).map(Vec::as_slice).unwrap_or(&[]),
                false,
            )?,
            "OrderedList" => {
                // [ListAttributes, [[Block]]]
                let items = c.and_then(|c| c.get(1)).and_then(J::as_array);
                self.list(items.map(Vec::as_slice).unwrap_or(&[]), true)?;
            }
            "DefinitionList" => {
                // [[ [Inline], [[Block]] ]] — term paragraph, then defs.
                if let Some(entries) = c.and_then(J::as_array) {
                    for entry in entries {
                        let term = self.inlines(arr_at(Some(entry), 0))?;
                        if !term.is_empty() {
                            self.push_block(Node::Para(Para { children: term }));
                        }
                        if let Some(defs) = entry.get(1).and_then(J::as_array) {
                            for def in defs {
                                let blocks = self.blocks_frame(
                                    def.as_array().map(Vec::as_slice).unwrap_or(&[]),
                                )?;
                                for b in blocks {
                                    self.push_block(b);
                                }
                            }
                        }
                    }
                }
            }
            "Table" => {
                if let Some(table) = self.table(c)? {
                    self.push_block(table);
                }
            }
            "Figure" => {
                // [Attr, Caption, [Block]] — find the image within.
                if let Some(blocks) = c.and_then(|c| c.get(2)).and_then(J::as_array) {
                    self.figure_from_blocks(blocks)?;
                }
            }
            "Div" => {
                // Transparent: flow children into the current frame.
                self.blocks_into_current(
                    c.and_then(|c| c.get(1))
                        .and_then(J::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                )?;
            }
            // HorizontalRule, RawBlock, Null → dropped (no foreign content).
            _ => {}
        }
        Ok(())
    }

    fn list(&mut self, items: &[J], ordered: bool) -> Result<()> {
        let mut out = Vec::new();
        for item in items {
            let blocks = self.blocks_frame(item.as_array().map(Vec::as_slice).unwrap_or(&[]))?;
            if !blocks.is_empty() {
                out.push(blocks);
            }
        }
        if !out.is_empty() {
            self.push_block(Node::List(List {
                ordered,
                items: out,
            }));
        }
        Ok(())
    }

    fn table(&mut self, c: Option<&J>) -> Result<Option<Node>> {
        // [Attr, Caption, [ColSpec], TableHead, [TableBody], TableFoot]
        let Some(c) = c.and_then(J::as_array) else {
            return Ok(None);
        };
        let ncols = c.get(2).and_then(J::as_array).map(|v| v.len()).unwrap_or(0);
        let mut head = Vec::new();
        let mut body = Vec::new();
        let mut foot = Vec::new();

        // TableHead = [Attr, [Row]]
        if let Some(rows) = c.get(3).and_then(|h| h.get(1)).and_then(J::as_array) {
            for r in rows {
                if let Some(row) = self.table_row(r, true)? {
                    head.push(row);
                }
            }
        }
        // [TableBody]; each body = [Attr, RowHeadColumns, [Row] (head), [Row] (body)]
        if let Some(bodies) = c.get(4).and_then(J::as_array) {
            for bd in bodies {
                if let Some(rows) = bd.get(2).and_then(J::as_array) {
                    for r in rows {
                        if let Some(row) = self.table_row(r, true)? {
                            head.push(row);
                        }
                    }
                }
                if let Some(rows) = bd.get(3).and_then(J::as_array) {
                    for r in rows {
                        if let Some(row) = self.table_row(r, false)? {
                            body.push(row);
                        }
                    }
                }
            }
        }
        // TableFoot = [Attr, [Row]]
        if let Some(rows) = c.get(5).and_then(|f| f.get(1)).and_then(J::as_array) {
            for r in rows {
                if let Some(row) = self.table_row(r, false)? {
                    foot.push(row);
                }
            }
        }
        if head.is_empty() && body.is_empty() && foot.is_empty() {
            return Ok(None);
        }
        let cols = (0..ncols.max(1)).map(|_| ColSpec { width: None }).collect();
        Ok(Some(Node::Table(Table {
            cols,
            head,
            body,
            foot,
        })))
    }

    fn table_row(&mut self, row: &J, header: bool) -> Result<Option<Row>> {
        // Row = [Attr, [Cell]]
        let Some(cells_json) = row.get(1).and_then(J::as_array) else {
            return Ok(None);
        };
        let mut cells = Vec::new();
        for cell in cells_json {
            // Cell = [Attr, Alignment, RowSpan, ColSpan, [Block]]
            let ca = cell.as_array();
            let rowspan = ca.and_then(|a| a.get(2)).and_then(J::as_u64).unwrap_or(1) as u32;
            let colspan = ca.and_then(|a| a.get(3)).and_then(J::as_u64).unwrap_or(1) as u32;
            let blocks_json = ca
                .and_then(|a| a.get(4))
                .and_then(J::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let mut blocks = self.blocks_frame(blocks_json)?;
            if blocks.is_empty() {
                blocks.push(Node::Para(Para { children: vec![] }));
            }
            cells.push(Cell {
                span: (rowspan > 1 || colspan > 1).then_some((rowspan, colspan)),
                scope: header.then_some(CellScope::Col),
                children: blocks,
            });
        }
        if cells.is_empty() {
            return Ok(None);
        }
        Ok(Some(Row { cells }))
    }

    /// Find the first `Image` inside a figure's blocks and emit a Figure.
    fn figure_from_blocks(&mut self, blocks: &[J]) -> Result<()> {
        for blk in blocks {
            if let Some(inls) = blk.get("c").and_then(J::as_array) {
                for inl in inls {
                    if inl.get("t").and_then(J::as_str) == Some("Image") {
                        let (src, alt) = image_src_alt(inl);
                        self.add_figure(&src, &alt)?;
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    fn inlines(&mut self, arr: &[J]) -> Result<Vec<Inline>> {
        let mut out = Vec::new();
        for inl in arr {
            self.inline(&mut out, inl)?;
        }
        Ok(out)
    }

    fn inline(&mut self, out: &mut Vec<Inline>, inl: &J) -> Result<()> {
        let Some(t) = inl.get("t").and_then(J::as_str) else {
            return Ok(());
        };
        let c = inl.get("c");
        match t {
            "Str" => {
                if let Some(s) = c.and_then(J::as_str) {
                    out.push(Inline::Text(s.to_owned()));
                }
            }
            "Space" | "SoftBreak" | "LineBreak" => out.push(Inline::Text(" ".into())),
            "Emph" => self.styled(out, (false, true, false, false), c)?,
            "Strong" => self.styled(out, (true, false, false, false), c)?,
            "Underline" => self.styled(out, (false, false, true, false), c)?,
            "Code" => {
                // [Attr, Text]
                if let Some(text) = c.and_then(|c| c.get(1)).and_then(J::as_str) {
                    let style = self.style((false, false, false, true));
                    out.push(Inline::Span(Span {
                        style: Some(style),
                        children: vec![Inline::Text(text.to_owned())],
                    }));
                }
            }
            "Math" => {
                // [MathType, Text] — TeX has no faithful MathML here; keep
                // the source text rather than mis-convert.
                if let Some(text) = c.and_then(|c| c.get(1)).and_then(J::as_str) {
                    out.push(Inline::Text(text.to_owned()));
                }
            }
            "Quoted" => {
                // [QuoteType, [Inline]]
                let double = c
                    .and_then(|c| c.get(0))
                    .and_then(|q| q.get("t"))
                    .and_then(J::as_str)
                    == Some("DoubleQuote");
                let (open, close) = if double {
                    ('“', '”')
                } else {
                    ('‘', '’')
                };
                out.push(Inline::Text(open.to_string()));
                let inner = self.inlines(arr_at(c, 1))?;
                out.extend(inner);
                out.push(Inline::Text(close.to_string()));
            }
            "Link" => {
                // [Attr, [Inline], [url, title]]
                let href = c
                    .and_then(|c| c.get(2))
                    .and_then(|t| t.get(0))
                    .and_then(J::as_str)
                    .unwrap_or("")
                    .to_owned();
                let children = self.inlines(arr_at(c, 1))?;
                if href.is_empty() {
                    out.extend(children);
                } else {
                    out.push(Inline::Link(Link { href, children }));
                }
            }
            "Image" => {
                // Inline image: VSD figures are block-level, so keep the
                // alt text inline (a standalone image arrives as a Figure
                // block and is handled there).
                let (_src, alt) = image_src_alt(inl);
                if !alt.trim().is_empty() {
                    out.push(Inline::Text(alt));
                }
            }
            "Note" => {}      // footnote content dropped
            "RawInline" => {} // foreign content dropped
            // SmallCaps / Strikeout / Superscript / Subscript / Span /
            // Cite / SoftBreak-likes: keep their text, drop the styling we
            // don't model.
            _ => {
                if let Some(arr) = c.and_then(J::as_array) {
                    // Span/Cite carry [Attr, [Inline]] or [.., [Inline]];
                    // the last array element is the inline children.
                    let inner_arr = arr
                        .iter()
                        .rev()
                        .find_map(J::as_array)
                        .map(|v| v.to_vec())
                        .unwrap_or_else(|| arr.to_vec());
                    let inner = self.inlines(&inner_arr)?;
                    out.extend(inner);
                } else if let Some(s) = c.and_then(J::as_str) {
                    out.push(Inline::Text(s.to_owned()));
                }
            }
        }
        Ok(())
    }

    fn styled(
        &mut self,
        out: &mut Vec<Inline>,
        key: (bool, bool, bool, bool),
        c: Option<&J>,
    ) -> Result<()> {
        let children = self.inlines(c.and_then(J::as_array).map(Vec::as_slice).unwrap_or(&[]))?;
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
            "Pandoc image {src:?} needs alt text — accessibility is a validity condition in VSD"
        );
        let path = self.base_dir.join(src);
        let data = std::fs::read(&path)
            .with_context(|| format!("reading image {} (from Pandoc AST)", path.display()))?;
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
        anyhow::ensure!(self.blocks.len() == 1, "unbalanced Pandoc structure");
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

/// `c[i]` as an inline array, or empty.
fn arr_at(c: Option<&J>, i: usize) -> &[J] {
    c.and_then(|c| c.get(i))
        .and_then(J::as_array)
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

/// Extract (src, alt) from an `Image` inline: `[Attr, [Inline], [url, title]]`.
fn image_src_alt(inl: &J) -> (String, String) {
    let c = inl.get("c");
    let src = c
        .and_then(|c| c.get(2))
        .and_then(|t| t.get(0))
        .and_then(J::as_str)
        .unwrap_or("")
        .to_owned();
    let alt = c
        .and_then(|c| c.get(1))
        .and_then(J::as_array)
        .map(|inls| inline_json_text(inls))
        .unwrap_or_default();
    (src, alt)
}

/// Plain-text of a raw inline JSON array (for alt text / titles).
fn inline_json_text(arr: &[J]) -> String {
    let mut out = String::new();
    for inl in arr {
        match inl.get("t").and_then(J::as_str) {
            Some("Str") => {
                if let Some(s) = inl.get("c").and_then(J::as_str) {
                    out.push_str(s);
                }
            }
            Some("Space") | Some("SoftBreak") | Some("LineBreak") => out.push(' '),
            _ => {
                if let Some(a) = inl.get("c").and_then(J::as_array) {
                    if let Some(inner) = a.iter().rev().find_map(J::as_array) {
                        out.push_str(&inline_json_text(inner));
                    }
                }
            }
        }
    }
    out
}

/// The document title from Pandoc `meta`, if present.
fn meta_title(root: &J) -> Option<String> {
    let title = root.get("meta")?.get("title")?;
    match title.get("t").and_then(J::as_str) {
        Some("MetaString") => title.get("c").and_then(J::as_str).map(str::to_owned),
        Some("MetaInlines") => title
            .get("c")
            .and_then(J::as_array)
            .map(|a| inline_json_text(a)),
        _ => None,
    }
    .filter(|s| !s.trim().is_empty())
    .map(|s| s.trim().to_owned())
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

    fn doc(json: &str) -> Document {
        document_from_pandoc(json, Path::new("."), Profile::Core).unwrap()
    }
    fn body(d: &Document) -> Vec<Node> {
        match d.root_node().unwrap() {
            Node::Doc(doc) => doc.children,
            _ => panic!("doc root"),
        }
    }

    #[test]
    fn maps_meta_headings_paragraphs_lists() {
        let json = r#"{
          "pandoc-api-version":[1,23],
          "meta":{"title":{"t":"MetaInlines","c":[{"t":"Str","c":"My"},{"t":"Space"},{"t":"Str","c":"Doc"}]}},
          "blocks":[
            {"t":"Header","c":[2,["",[],[]],[{"t":"Str","c":"Section"}]]},
            {"t":"Para","c":[{"t":"Str","c":"a"},{"t":"Space"},{"t":"Strong","c":[{"t":"Str","c":"bold"}]},{"t":"Space"},{"t":"Link","c":[["",[],[]],[{"t":"Str","c":"link"}],["http://x",""]]}]},
            {"t":"BulletList","c":[[{"t":"Plain","c":[{"t":"Str","c":"one"}]}],[{"t":"Plain","c":[{"t":"Str","c":"two"}]}]]},
            {"t":"CodeBlock","c":[["",["rust"],[]],"fn main(){}"]}
          ]
        }"#;
        let d = doc(json);
        assert!(vsd_core::validate::validate(&d).is_valid());
        assert_eq!(d.metadata().unwrap().title.as_deref(), Some("My Doc"));
        let b = body(&d);
        assert!(matches!(&b[0], Node::Heading(h) if h.level == 2));
        assert!(matches!(&b[1], Node::Para(_)));
        match &b[2] {
            Node::List(l) => {
                assert!(!l.ordered);
                assert_eq!(l.items.len(), 2);
            }
            _ => panic!("bullet list"),
        }
        assert!(
            matches!(&b[3], Node::Code(c) if c.lang.as_deref()==Some("rust") && c.text=="fn main(){}")
        );
        let text = vsd_core::extract::extract_text(&d).unwrap();
        assert!(text.contains("a bold link"), "text: {text}");
    }

    #[test]
    fn maps_table_with_header_scope() {
        // pandoc-types 1.23 Table: [Attr, Caption, [ColSpec], Head, [Body], Foot]
        let cell = |s: &str| {
            format!(
                r#"[["",[],[]],{{"t":"AlignDefault"}},1,1,[{{"t":"Plain","c":[{{"t":"Str","c":"{s}"}}]}}]]"#
            )
        };
        let row = |a: &str, b: &str| format!(r#"[["",[],[]],[{},{}]]"#, cell(a), cell(b));
        let json = format!(
            r#"{{"pandoc-api-version":[1,23],"meta":{{}},"blocks":[
              {{"t":"Table","c":[
                ["",[],[]],
                [null,[]],
                [[{{"t":"AlignDefault"}},{{"t":"ColWidthDefault"}}],[{{"t":"AlignDefault"}},{{"t":"ColWidthDefault"}}]],
                [["",[],[]],[{head}]],
                [[["",[],[]],0,[],[{bodyrow}]]],
                [["",[],[]],[]]
              ]}}
            ]}}"#,
            head = row("Name", "Qty"),
            bodyrow = row("Widget", "7")
        );
        let d = doc(&json);
        let b = body(&d);
        let Node::Table(t) = &b[0] else {
            panic!("table");
        };
        assert_eq!(t.head.len(), 1);
        assert_eq!(t.body.len(), 1);
        assert_eq!(t.head[0].cells.len(), 2);
        assert_eq!(t.head[0].cells[0].scope, Some(CellScope::Col));
        assert_eq!(t.body[0].cells[0].scope, None);
        let text = vsd_core::extract::extract_text(&d).unwrap();
        assert!(text.contains("Name") && text.contains("Widget"));
    }

    #[test]
    fn quotes_and_math_and_raw() {
        let json = r#"{"pandoc-api-version":[1,23],"meta":{},"blocks":[
          {"t":"Para","c":[
            {"t":"Quoted","c":[{"t":"DoubleQuote"},[{"t":"Str","c":"hi"}]]},
            {"t":"Space"},
            {"t":"Math","c":[{"t":"InlineMath"},"x^2"]},
            {"t":"RawInline","c":["html","<b>x</b>"]}
          ]}
        ]}"#;
        let d = doc(json);
        let text = vsd_core::extract::extract_text(&d).unwrap();
        assert!(
            text.contains('\u{201c}') && text.contains('\u{201d}'),
            "smart quotes"
        );
        assert!(text.contains("x^2"), "math kept as source text");
        assert!(!text.contains("<b>"), "raw inline dropped");
    }

    #[test]
    fn rejects_non_pandoc_json() {
        let r = document_from_pandoc(r#"{"t":"doc","content":[]}"#, Path::new("."), Profile::Core);
        match r {
            Ok(_) => panic!("plain JSON must be rejected as not a Pandoc AST"),
            Err(e) => assert!(format!("{e:#}").contains("Pandoc")),
        }
    }
}
