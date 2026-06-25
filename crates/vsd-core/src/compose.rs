//! Fluent authoring API (ROADMAP 4c): the high-level way to build
//! documents from Rust — invoice generators and report pipelines are
//! the highest-volume document producers on earth, and they should not
//! have to hand-assemble tree nodes.
//!
//! ```
//! use vsd_core::compose::Compose;
//!
//! let doc = Compose::new("en")
//!     .title("Quarterly report")
//!     .author("A. Person")
//!     .h1("Q3 results")
//!     .para("Revenue grew in every region.")
//!     .bullets(["EMEA up 12%", "APAC up 9%"])
//!     .table(["Region", "Revenue"], [["EMEA", "1.2M"], ["APAC", "0.9M"]])
//!     .code(Some("rust"), "fn growth() -> f64 { 0.12 }")
//!     .finish()
//!     .unwrap();
//!
//! assert!(vsd_core::validate::validate(&doc).is_valid());
//! ```

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::document::{Document, DocumentBuilder};
use crate::error::Result;
use crate::manifest::{Blob, Metadata, Profile, ResourceEntry, ResourceKind};
use crate::tree::{
    Cell, CellScope, Code, ColSpec, Direction, Doc, Figure, Heading, Inline, List, Node, Para, Row,
    Section, Table,
};
use crate::ResourceTable;

/// A fluent document composer. Every method returns `self`; call
/// [`Compose::finish`] to produce a validated-buildable [`Document`].
pub struct Compose {
    lang: String,
    children: Vec<Node>,
    metadata: Metadata,
    profile: Profile,
    resources: Vec<(String, ResourceEntry)>,
    blobs: Vec<Blob>,
}

impl Compose {
    pub fn new(lang: impl Into<String>) -> Self {
        Compose {
            lang: lang.into(),
            children: Vec::new(),
            metadata: Metadata::default(),
            profile: Profile::Core,
            resources: Vec::new(),
            blobs: Vec::new(),
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.metadata.title = Some(title.into());
        self
    }

    pub fn author(mut self, author: impl Into<String>) -> Self {
        self.metadata.authors.push(author.into());
        self
    }

    /// RFC 3339 creation timestamp (author-supplied; the format has no clock).
    pub fn created(mut self, rfc3339: impl Into<String>) -> Self {
        self.metadata.created = Some(rfc3339.into());
        self
    }

    pub fn profile(mut self, profile: Profile) -> Self {
        self.profile = profile;
        self
    }

    pub fn heading(mut self, level: u8, text: impl Into<String>) -> Self {
        self.children.push(Node::Heading(Heading {
            level: level.clamp(1, 6),
            children: vec![Inline::Text(text.into())],
        }));
        self
    }

    pub fn h1(self, text: impl Into<String>) -> Self {
        self.heading(1, text)
    }

    pub fn h2(self, text: impl Into<String>) -> Self {
        self.heading(2, text)
    }

    pub fn h3(self, text: impl Into<String>) -> Self {
        self.heading(3, text)
    }

    pub fn para(mut self, text: impl Into<String>) -> Self {
        self.children.push(Node::Para(Para {
            children: vec![Inline::Text(text.into())],
        }));
        self
    }

    /// A paragraph from pre-built inline content (links, styled spans).
    pub fn para_rich(mut self, children: Vec<Inline>) -> Self {
        self.children.push(Node::Para(Para { children }));
        self
    }

    pub fn bullets<I, S>(mut self, items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.children.push(Node::List(List {
            ordered: false,
            items: items.into_iter().map(|s| vec![para(s)]).collect(),
        }));
        self
    }

    pub fn numbered<I, S>(mut self, items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.children.push(Node::List(List {
            ordered: true,
            items: items.into_iter().map(|s| vec![para(s)]).collect(),
        }));
        self
    }

    pub fn code(mut self, lang: Option<&str>, text: impl Into<String>) -> Self {
        self.children.push(Node::Code(Code {
            lang: lang.map(str::to_string),
            text: text.into(),
        }));
        self
    }

    /// A simple table: header texts + body rows of texts. Header cells
    /// get real column scope (a structural fact, per spec §3).
    pub fn table<H, HS, R, RR, RS>(mut self, headers: H, rows: R) -> Self
    where
        H: IntoIterator<Item = HS>,
        HS: Into<String>,
        R: IntoIterator<Item = RR>,
        RR: IntoIterator<Item = RS>,
        RS: Into<String>,
    {
        let head_cells: Vec<Cell> = headers
            .into_iter()
            .map(|t| cell(t, Some(CellScope::Col)))
            .collect();
        let ncols = head_cells.len().max(1);
        let body: Vec<Row> = rows
            .into_iter()
            .map(|r| Row {
                cells: r.into_iter().map(|t| cell(t, None)).collect(),
            })
            .collect();
        self.children.push(Node::Table(Table {
            cols: vec![ColSpec { width: None }; ncols],
            head: vec![Row { cells: head_cells }],
            body,
            foot: vec![],
        }));
        self
    }

    /// Embed an image (PNG/JPEG bytes) as a figure. Alt text is a
    /// required argument because it is a validity condition.
    pub fn figure(
        mut self,
        mime: impl Into<String>,
        data: Vec<u8>,
        alt: impl Into<String>,
    ) -> Result<Self> {
        let mime = mime.into();
        let blob = Blob {
            mime: mime.clone(),
            data,
        };
        let id = crate::ObjectId::of_value(&blob.to_value())?;
        let name = alloc::format!("res{}", self.resources.len());
        self.resources.push((
            name,
            ResourceEntry {
                kind: ResourceKind::Image,
                mime,
                data: id,
            },
        ));
        self.blobs.push(blob);
        self.children.push(Node::Figure(Figure {
            res: id,
            alt: alt.into(),
            decorative: false,
            caption: vec![],
        }));
        Ok(self)
    }

    /// A semantic section composed via a nested closure.
    pub fn section(self, role: impl Into<String>, f: impl FnOnce(Compose) -> Compose) -> Self {
        self.section_columns(role, 1, f)
    }

    /// A section whose content flows into `columns` layout columns
    /// (format 0.6; honored by engine 1.10+). `columns == 1` is a plain
    /// single-column section.
    pub fn section_columns(
        mut self,
        role: impl Into<String>,
        columns: u32,
        f: impl FnOnce(Compose) -> Compose,
    ) -> Self {
        let inner = f(Compose::new(self.lang.clone()));
        self.resources.extend(inner.resources);
        self.blobs.extend(inner.blobs);
        self.children.push(Node::Section(Section {
            role: role.into(),
            columns: columns.max(1),
            children: inner.children,
        }));
        self
    }

    pub fn page_break(mut self) -> Self {
        self.children.push(Node::PageBreakHint);
        self
    }

    /// Append any pre-built node (escape hatch to the full tree model).
    pub fn node(mut self, node: Node) -> Self {
        self.children.push(node);
        self
    }

    pub fn finish(self) -> Result<Document> {
        let root = Node::Doc(Doc {
            lang: self.lang,
            dir: Direction::Ltr,
            writing_mode: crate::tree::WritingMode::Horizontal,
            children: self.children,
        });
        let mut builder = DocumentBuilder::new(root)
            .metadata(self.metadata)
            .profile(self.profile);
        for blob in &self.blobs {
            builder.add_object(blob.to_value())?;
        }
        builder
            .resources(ResourceTable {
                entries: self.resources,
                styles: Vec::new(),
            })
            .build()
    }
}

fn para(text: impl Into<String>) -> Node {
    Node::Para(Para {
        children: vec![Inline::Text(text.into())],
    })
}

fn cell(text: impl Into<String>, scope: Option<CellScope>) -> Cell {
    Cell {
        span: None,
        scope,
        children: vec![para(text)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_builds_a_valid_document() {
        let doc = Compose::new("en")
            .title("Invoice 42")
            .author("Acme")
            .h1("Invoice")
            .para("Thank you for your business.")
            .table(["Item", "Price"], [["Widget", "4.20"], ["Gadget", "7.00"]])
            .numbered(["Pay within 20 days", "Reference invoice 42"])
            .section("terms", |s| s.h2("Terms").para("Net 20."))
            .finish()
            .unwrap();

        let report = crate::validate::validate(&doc);
        assert!(report.is_valid(), "findings: {:?}", report.findings);
        let text = crate::extract::extract_text(&doc).unwrap();
        assert!(text.contains("Invoice"));
        assert!(text.contains("Widget\t4.20"));
        assert!(text.contains("1. Pay within 20 days"));
        assert!(text.contains("Net 20."));
    }
}
