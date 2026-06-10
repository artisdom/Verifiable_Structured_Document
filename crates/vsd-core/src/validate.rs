//! Document validation (spec §3, §5, §10).
//!
//! Validation is layered:
//! 1. every object's bytes are canonical CBOR and hash to their id
//!    (checked at store load — see `ObjectStore::put_verified`)
//! 2. the manifest's closure decodes into well-formed typed objects
//! 3. tree validity: mandatory alt text, heading levels, table shape,
//!    resolvable references, unique field ids, well-formed expressions
//! 4. profile conformance (§10)
//! 5. store hygiene: orphan objects are reported (a redaction-leak smell)

use std::collections::BTreeSet;

use crate::document::Document;
use crate::error::Result;
use crate::manifest::{Profile, RenderCache, ResourceTable, VSD_VERSION};
use crate::tree::{Inline, Node};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Severity {
    /// The document is malformed; conforming readers MUST reject it.
    Error,
    /// Permitted but suspicious; tools SHOULD surface it.
    Warning,
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn is_valid(&self) -> bool {
        !self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    pub fn errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.severity == Severity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
    }

    fn error(&mut self, code: &'static str, message: impl Into<String>) {
        self.findings.push(Finding {
            severity: Severity::Error,
            code,
            message: message.into(),
        });
    }

    fn warn(&mut self, code: &'static str, message: impl Into<String>) {
        self.findings.push(Finding {
            severity: Severity::Warning,
            code,
            message: message.into(),
        });
    }
}

pub fn validate(doc: &Document) -> Report {
    let mut report = Report::default();
    if let Err(e) = validate_inner(doc, &mut report) {
        // A hard failure while walking (missing object, undecodable node)
        // is itself a validity error, not a tool crash.
        report.error("E_STRUCTURE", e.to_string());
    }
    report
}

fn validate_inner(doc: &Document, r: &mut Report) -> Result<()> {
    // Version gate.
    if doc.manifest.version.0 != VSD_VERSION.0 {
        r.error(
            "E_VERSION",
            format!(
                "major version {} not supported (reader supports {})",
                doc.manifest.version.0, VSD_VERSION.0
            ),
        );
        return Ok(());
    }

    // Decode the root-level objects; each failure is a finding.
    let root = match doc.root_node() {
        Ok(n) => Some(n),
        Err(e) => {
            r.error("E_ROOT", format!("content tree root: {e}"));
            None
        }
    };
    let resources = match doc.resources() {
        Ok(t) => Some(t),
        Err(e) => {
            r.error("E_RESOURCES", format!("resource table: {e}"));
            None
        }
    };
    if let Err(e) = doc.metadata() {
        r.error("E_METADATA", format!("metadata: {e}"));
    }
    if let Err(e) = doc.provenance() {
        r.error("E_PROVENANCE", format!("provenance: {e}"));
    }
    let render_cache = match doc.render_cache() {
        Ok(c) => c,
        Err(e) => {
            r.error("E_RENDER_CACHE", format!("render cache: {e}"));
            None
        }
    };

    // Tree validity.
    if let (Some(root), Some(resources)) = (&root, &resources) {
        if !matches!(root, Node::Doc(_)) {
            r.error("E_ROOT_KIND", "content tree root must be a doc node");
        }
        let mut ctx = TreeCtx {
            doc,
            resources,
            field_ids: BTreeSet::new(),
            report: r,
        };
        ctx.walk(root, &mut Vec::new())?;
    }

    // Render cache structural checks (spec §5.2): the layout-hash must
    // commit to the page list, and every page object must exist and
    // decode as a display list. Full recomputation requires the named
    // layout engine and is performed by `verify_layout` when available.
    if let Some(rc) = &render_cache {
        let expect = RenderCache::compute_layout_hash(&rc.pages)?;
        if expect != rc.layout_hash {
            r.error(
                "E_LAYOUT_HASH",
                "render cache layout-hash does not match its page list — the cache lies about the content",
            );
        }
        for page_id in &rc.pages {
            match doc.store.get_value(page_id) {
                Err(e) => r.error("E_PAGE_MISSING", format!("page object {page_id}: {e}")),
                Ok(v) => {
                    if let Err(e) = crate::layout::Page::from_value(&v) {
                        r.error("E_PAGE_DECODE", format!("page object {page_id}: {e}"));
                    }
                }
            }
        }
        if rc.width_mm <= 0.0 || rc.height_mm <= 0.0 {
            r.error("E_GEOMETRY", "render cache page geometry must be positive");
        }
    }

    // Profile conformance (§10).
    match doc.manifest.profile {
        Profile::Core => {}
        Profile::Archive => {
            if doc.manifest.render_cache.is_none() {
                r.error("E_ARCHIVE_CACHE", "VSD/Archive requires a render cache");
            }
            if doc.manifest.provenance.is_none() {
                r.error("E_ARCHIVE_PROV", "VSD/Archive requires a provenance chain");
            }
            if let Ok(fields) = doc.fields() {
                if !fields.is_empty() {
                    r.error("E_ARCHIVE_FIELDS", "VSD/Archive forbids the field layer");
                }
            }
        }
        Profile::Form => {}
        Profile::Stream => {
            if doc.manifest.page_index.is_none() {
                r.error("E_STREAM_INDEX", "VSD/Stream requires a page-index object");
            }
        }
    }

    // Store hygiene: orphans.
    let closure = doc.closure()?;
    let orphans: Vec<_> = doc
        .store
        .ids()
        .filter(|id| !closure.contains(id))
        .collect();
    if !orphans.is_empty() {
        r.warn(
            "W_ORPHANS",
            format!(
                "{} object(s) in the store are unreachable from the manifest (first: {}); \
                 redacted documents MUST NOT contain orphans",
                orphans.len(),
                orphans[0]
            ),
        );
    }
    for id in &closure {
        if !doc.store.contains(id) {
            r.error("E_MISSING_OBJECT", format!("referenced object {id} is absent"));
        }
    }
    Ok(())
}

struct TreeCtx<'a> {
    doc: &'a Document,
    resources: &'a ResourceTable,
    field_ids: BTreeSet<String>,
    report: &'a mut Report,
}

impl TreeCtx<'_> {
    fn walk(&mut self, node: &Node, path: &mut Vec<usize>) -> Result<()> {
        match node {
            Node::Doc(d) => {
                if path.is_empty() {
                    if d.lang.is_empty() {
                        self.report.error("E_LANG", "doc: lang must be non-empty");
                    }
                } else {
                    self.report
                        .error("E_NESTED_DOC", "doc nodes may only appear at the root");
                }
                self.walk_children(&d.children, path)?;
            }
            Node::Section(s) => self.walk_children(&s.children, path)?,
            Node::Figure(f) => {
                // Spec §3: alt text is mandatory; empty only when decorative.
                if f.alt.trim().is_empty() && !f.decorative {
                    self.report.error(
                        "E_ALT_TEXT",
                        format!(
                            "figure at {path:?}: alt text is required (or mark decorative: true). \
                             Accessibility is a validity condition, not an afterthought."
                        ),
                    );
                }
                if !self.doc.store.contains(&f.res) {
                    self.report.error(
                        "E_FIG_RES",
                        format!("figure at {path:?} references missing resource {}", f.res),
                    );
                }
                self.walk_inlines(&f.caption, path);
            }
            Node::Table(t) => {
                if t.body.is_empty() {
                    self.report
                        .error("E_TABLE_EMPTY", format!("table at {path:?}: body must be non-empty"));
                }
                let ncols = t.cols.len();
                for row in t.head.iter().chain(&t.body).chain(&t.foot) {
                    // Sum of col spans must equal the declared column count.
                    let width: u64 = row
                        .cells
                        .iter()
                        .map(|c| c.span.map(|(_, cs)| cs as u64).unwrap_or(1))
                        .sum();
                    if ncols != 0 && width != ncols as u64 {
                        self.report.error(
                            "E_TABLE_SHAPE",
                            format!(
                                "table at {path:?}: row covers {width} column(s), expected {ncols}"
                            ),
                        );
                    }
                    for cell in &row.cells {
                        self.walk_children(&cell.children, path)?;
                    }
                }
            }
            Node::List(l) => {
                if l.items.is_empty() {
                    self.report
                        .warn("W_LIST_EMPTY", format!("list at {path:?} has no items"));
                }
                for item in &l.items {
                    self.walk_children(item, path)?;
                }
            }
            Node::Heading(h) => self.walk_inlines(&h.children, path),
            Node::Para(p) => self.walk_inlines(&p.children, path),
            Node::Math(m) => {
                if m.mathml.trim().is_empty() {
                    self.report
                        .error("E_MATH_EMPTY", format!("math at {path:?}: mathml is empty"));
                }
            }
            Node::Field(f) => {
                if !self.field_ids.insert(f.id.clone()) {
                    self.report.error(
                        "E_FIELD_DUP",
                        format!("duplicate field id {:?} at {path:?}", f.id),
                    );
                }
                // Expressions were depth/size/regex-checked at decode; here we
                // check reference hygiene once all ids are known (deferred —
                // see validate_field_refs called by the document walk's caller).
            }
            Node::SubtreeRef(id) => {
                let sub = Node::from_value(&self.doc.store.get_value(id)?)?;
                self.walk(&sub, path)?;
            }
            Node::Code(_) | Node::PageBreakHint | Node::Redacted(_) => {}
        }

        // After a full walk from the root, verify field references.
        if path.is_empty() {
            let mut fields = Vec::new();
            self.doc.walk_fields_into(node, &mut fields)?;
            let ids: BTreeSet<&str> = fields.iter().map(|f| f.id.as_str()).collect();
            for f in &fields {
                let mut refs = Vec::new();
                if let Some(e) = &f.constraint {
                    e.field_refs(&mut refs);
                }
                if let Some(e) = &f.computed {
                    e.field_refs(&mut refs);
                }
                for r in refs {
                    if !ids.contains(r.as_str()) {
                        self.report.error(
                            "E_FIELD_REF",
                            format!("field {:?} references unknown field {r:?}", f.id),
                        );
                    }
                }
            }
        }
        Ok(())
    }

    fn walk_children(&mut self, children: &[Node], path: &mut Vec<usize>) -> Result<()> {
        for (i, c) in children.iter().enumerate() {
            path.push(i);
            self.walk(c, path)?;
            path.pop();
        }
        Ok(())
    }

    fn walk_inlines(&mut self, inlines: &[Inline], path: &Vec<usize>) {
        for inline in inlines {
            match inline {
                Inline::Span(s) => {
                    if let Some(style) = s.style {
                        if style as usize >= self.resources.styles.len() {
                            self.report.error(
                                "E_STYLE_REF",
                                format!(
                                    "span at {path:?} references style {style}, but only {} defined",
                                    self.resources.styles.len()
                                ),
                            );
                        }
                    }
                    self.walk_inlines(&s.children, path);
                }
                Inline::Link(l) => {
                    if l.href.is_empty() {
                        self.report
                            .error("E_LINK_EMPTY", format!("link at {path:?} has empty href"));
                    }
                    self.walk_inlines(&l.children, path);
                }
                _ => {}
            }
        }
    }
}

impl Document {
    pub(crate) fn walk_fields_into(
        &self,
        node: &Node,
        out: &mut Vec<crate::tree::Field>,
    ) -> Result<()> {
        // Reuse the private traversal from document.rs via fields() on a
        // synthetic walk: simplest correct implementation is to recurse here.
        match node {
            Node::Field(f) => out.push(f.clone()),
            Node::Doc(d) => {
                for c in &d.children {
                    self.walk_fields_into(c, out)?;
                }
            }
            Node::Section(s) => {
                for c in &s.children {
                    self.walk_fields_into(c, out)?;
                }
            }
            Node::Table(t) => {
                for row in t.head.iter().chain(&t.body).chain(&t.foot) {
                    for cell in &row.cells {
                        for c in &cell.children {
                            self.walk_fields_into(c, out)?;
                        }
                    }
                }
            }
            Node::List(l) => {
                for item in &l.items {
                    for c in item {
                        self.walk_fields_into(c, out)?;
                    }
                }
            }
            Node::SubtreeRef(id) => {
                let sub = Node::from_value(&self.store.get_value(id)?)?;
                self.walk_fields_into(&sub, out)?;
            }
            _ => {}
        }
        Ok(())
    }
}
