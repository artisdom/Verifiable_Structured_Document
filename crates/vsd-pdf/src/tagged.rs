//! Foreign **tagged-PDF** structure recovery (ROADMAP 3b).
//!
//! A tagged PDF carries a logical structure tree (`StructTreeRoot` →
//! `StructElem` nodes with standard structure types: `Document`, `Sect`,
//! `H1`–`H6`, `P`, `L`/`LI`/`LBody`, `Table`/`TR`/`TH`/`TD`, `Figure`,
//! `Code`, …). Walking it recovers a genuine semantic content tree —
//! headings with levels, paragraphs, lists, and tables — far better than
//! the naive text-recovery fallback.
//!
//! The text of each leaf element lives in **marked content**: the element
//! references one or more MCIDs, and the page content stream marks the
//! glyphs of MCID *n* with `/MCID n BDC … EMC`. We build an MCID → text
//! map per page (decoding glyph bytes through each font's encoding, like
//! `lopdf`'s own `extract_text`) and resolve every element's text from it.
//!
//! This path is **lossy and honest about it**: it recovers structure and
//! text, not exact layout, and the caller still marks the result
//! `format-migrated` with the original PDF attached. Anything it cannot
//! recover faithfully (a `Figure`'s pixels, a `Formula`'s MathML) becomes
//! its alt/text rather than a mis-reconstruction; an untagged PDF yields
//! `None` so the caller falls back to text recovery.

use std::collections::{BTreeMap, HashMap};

use lopdf::{Dictionary, Document as Pdf, Encoding, Object, ObjectId};

use vsd_core::tree::{
    Cell, CellScope, ColSpec, Heading, Inline, List, Node, Para, Row, Section, Table,
};

/// Walk a foreign PDF's structure tree into VSD block nodes, or `None`
/// if the PDF is not tagged (no usable `StructTreeRoot`) — in which case
/// the caller falls back to the naive text recoverer.
pub fn recover_tagged(pdf: &Pdf) -> Option<Vec<Node>> {
    let root = struct_tree_root(pdf)?;
    let mcid_text = build_mcid_text(pdf);
    let walker = Walker { pdf, mcid_text };

    let mut blocks = Vec::new();
    for kid in kids_of(pdf, &root) {
        walker.walk_block(&kid, None, &mut blocks);
    }
    (!blocks.is_empty()).then_some(blocks)
}

/// Short identifier recorded in the import provenance `tool` claim.
pub const TOOL: &str = "vsd-pdf/tagged-recovery";

// --- The structure-tree walk ------------------------------------------------

struct Walker<'a> {
    pdf: &'a Pdf,
    /// page object id → (MCID → concatenated text).
    mcid_text: HashMap<ObjectId, HashMap<i64, String>>,
}

impl Walker<'_> {
    /// Append the block node(s) recovered from one structure element to
    /// `out`. `pg` is the inherited page object id (an element's own `/Pg`
    /// overrides it) used to resolve its MCIDs.
    fn walk_block(&self, elem: &Dictionary, pg: Option<ObjectId>, out: &mut Vec<Node>) {
        let pg = self.page_of(elem).or(pg);
        let s = struct_type(elem);
        match s.as_deref() {
            // Grouping containers that introduce a semantic section.
            Some("Sect" | "Art" | "Part") => {
                let mut children = Vec::new();
                for kid in self.child_elems(elem) {
                    self.walk_block(&kid, pg, &mut children);
                }
                if !children.is_empty() {
                    out.push(Node::Section(Section {
                        role: s.unwrap().to_ascii_lowercase(),
                        columns: 1,
                        children,
                    }));
                }
            }
            // Transparent grouping: flow children straight into the parent.
            Some("Document" | "Div" | "NonStruct" | "TOC" | "TOCI" | "Index" | "Aside") => {
                for kid in self.child_elems(elem) {
                    self.walk_block(&kid, pg, out);
                }
            }
            Some(h) if is_heading(h) => {
                let text = self.text_of(elem, pg);
                if !text.is_empty() {
                    out.push(Node::Heading(Heading {
                        level: heading_level(h),
                        children: vec![Inline::Text(text)],
                    }));
                }
            }
            Some("P" | "Caption" | "Quote" | "BlockQuote" | "Note" | "Formula") => {
                let text = self.text_of(elem, pg);
                if !text.is_empty() {
                    out.push(Node::Para(Para {
                        children: vec![Inline::Text(text)],
                    }));
                }
            }
            Some("Code") => {
                let text = self.text_of(elem, pg);
                if !text.is_empty() {
                    out.push(Node::Code(vsd_core::tree::Code { lang: None, text }));
                }
            }
            Some("L") => {
                if let Some(list) = self.recover_list(elem, pg) {
                    out.push(list);
                }
            }
            Some("Table") => {
                if let Some(table) = self.recover_table(elem, pg) {
                    out.push(table);
                }
            }
            Some("Figure" | "Formula2") => {
                // The pixels are page-drawn and not recoverable as a VSD
                // figure resource; preserve the alt text (the accessible
                // content) as a paragraph rather than inventing an image.
                let alt = elem_alt(elem).unwrap_or_else(|| self.text_of(elem, pg));
                let alt = alt.trim();
                if !alt.is_empty() {
                    out.push(Node::Para(Para {
                        children: vec![Inline::Text(alt.to_owned())],
                    }));
                }
            }
            // Unknown / inline-ish element with block children: recurse;
            // if it is a leaf with text, treat it as a paragraph.
            _ => {
                let children = self.child_elems(elem);
                if children.is_empty() {
                    let text = self.text_of(elem, pg);
                    if !text.is_empty() {
                        out.push(Node::Para(Para {
                            children: vec![Inline::Text(text)],
                        }));
                    }
                } else {
                    for kid in children {
                        self.walk_block(&kid, pg, out);
                    }
                }
            }
        }
    }

    fn recover_list(&self, elem: &Dictionary, pg: Option<ObjectId>) -> Option<Node> {
        let ordered = list_is_ordered(self.pdf, elem);
        let mut items = Vec::new();
        for li in self.child_elems(elem) {
            if struct_type(&li).as_deref() != Some("LI") {
                continue;
            }
            let li_pg = self.page_of(&li).or(pg);
            let mut blocks = Vec::new();
            for part in self.child_elems(&li) {
                match struct_type(&part).as_deref() {
                    // The label (bullet/number) is regenerated by VSD's
                    // list rendering, so drop the source `Lbl`.
                    Some("Lbl") => {}
                    Some("LBody") => {
                        for b in self.child_elems(&part) {
                            self.walk_block(&b, li_pg, &mut blocks);
                        }
                        // An LBody with only marked content (no child
                        // elements) carries the item text directly.
                        if blocks.is_empty() {
                            let text = self.text_of(&part, li_pg);
                            if !text.is_empty() {
                                blocks.push(Node::Para(Para {
                                    children: vec![Inline::Text(text)],
                                }));
                            }
                        }
                    }
                    _ => self.walk_block(&part, li_pg, &mut blocks),
                }
            }
            if !blocks.is_empty() {
                items.push(blocks);
            }
        }
        (!items.is_empty()).then_some(Node::List(List { ordered, items }))
    }

    fn recover_table(&self, elem: &Dictionary, pg: Option<ObjectId>) -> Option<Node> {
        // Rows may sit directly under Table or under THead/TBody/TFoot.
        let mut head = Vec::new();
        let mut body = Vec::new();
        let mut foot = Vec::new();
        let mut max_cols = 0usize;
        for group in self.child_elems(elem) {
            let gty = struct_type(&group);
            let target = match gty.as_deref() {
                Some("THead") => &mut head,
                Some("TFoot") => &mut foot,
                Some("TBody") => &mut body,
                Some("TR") => &mut body, // a bare row → body
                _ => continue,
            };
            let group_pg = self.page_of(&group).or(pg);
            if gty.as_deref() == Some("TR") {
                if let Some(row) = self.recover_row(&group, group_pg, &mut max_cols) {
                    target.push(row);
                }
            } else {
                for tr in self.child_elems(&group) {
                    if struct_type(&tr).as_deref() == Some("TR") {
                        let tr_pg = self.page_of(&tr).or(group_pg);
                        if let Some(row) = self.recover_row(&tr, tr_pg, &mut max_cols) {
                            target.push(row);
                        }
                    }
                }
            }
        }
        if head.is_empty() && body.is_empty() && foot.is_empty() {
            return None;
        }
        let cols = (0..max_cols).map(|_| ColSpec { width: None }).collect();
        Some(Node::Table(Table {
            cols,
            head,
            body,
            foot,
        }))
    }

    fn recover_row(
        &self,
        tr: &Dictionary,
        pg: Option<ObjectId>,
        max_cols: &mut usize,
    ) -> Option<Row> {
        let mut cells = Vec::new();
        for c in self.child_elems(tr) {
            let cty = struct_type(&c);
            let scope = match cty.as_deref() {
                Some("TH") => Some(cell_scope(self.pdf, &c)),
                Some("TD") => None,
                _ => continue,
            };
            let cell_pg = self.page_of(&c).or(pg);
            let mut blocks = Vec::new();
            // A cell may hold block children, or marked content directly.
            for b in self.child_elems(&c) {
                self.walk_block(&b, cell_pg, &mut blocks);
            }
            if blocks.is_empty() {
                let text = self.text_of(&c, cell_pg);
                blocks.push(Node::Para(Para {
                    children: vec![Inline::Text(text)],
                }));
            }
            cells.push(Cell {
                span: None,
                scope,
                children: blocks,
            });
        }
        if cells.is_empty() {
            return None;
        }
        *max_cols = (*max_cols).max(cells.len());
        Some(Row { cells })
    }

    /// All `StructElem` children of an element (skipping marked-content
    /// references and object references).
    fn child_elems(&self, elem: &Dictionary) -> Vec<Dictionary> {
        let mut out = Vec::new();
        for k in self.kids(elem) {
            if let Object::Dictionary(d) = &k {
                if d.get(b"S").is_ok() {
                    out.push(d.clone());
                }
            }
        }
        out
    }

    /// The resolved `/K` entries of an element (a single object or array).
    fn kids(&self, elem: &Dictionary) -> Vec<Object> {
        match elem.get(b"K") {
            Ok(k) => resolve_kids(self.pdf, k),
            Err(_) => Vec::new(),
        }
    }

    /// The page object id this element's marked content lives on.
    fn page_of(&self, elem: &Dictionary) -> Option<ObjectId> {
        match elem.get(b"Pg").ok()? {
            Object::Reference(id) => Some(*id),
            _ => None,
        }
    }

    /// Concatenate the text of an element: its own MCIDs plus the text of
    /// any inline child elements, in order. Whitespace is collapsed.
    fn text_of(&self, elem: &Dictionary, pg: Option<ObjectId>) -> String {
        let mut buf = String::new();
        self.collect_text(elem, pg, &mut buf);
        collapse_ws(&buf)
    }

    fn collect_text(&self, elem: &Dictionary, pg: Option<ObjectId>, buf: &mut String) {
        let pg = self.page_of(elem).or(pg);
        for k in self.kids(elem) {
            match k {
                Object::Integer(mcid) => self.push_mcid(pg, mcid, buf),
                Object::Dictionary(d) => {
                    // A marked-content reference (/MCR) or a nested element.
                    if let Ok(Object::Name(t)) = d.get(b"Type") {
                        if t.as_slice() == b"MCR" {
                            let mpg = match d.get(b"Pg") {
                                Ok(Object::Reference(id)) => Some(*id),
                                _ => pg,
                            };
                            if let Ok(Object::Integer(mcid)) = d.get(b"MCID") {
                                self.push_mcid(mpg, *mcid, buf);
                            }
                            continue;
                        }
                        if t.as_slice() == b"OBJR" {
                            continue; // object reference (annotation) — no text
                        }
                    }
                    if d.get(b"S").is_ok() {
                        self.collect_text(&d, pg, buf);
                    }
                }
                _ => {}
            }
        }
    }

    fn push_mcid(&self, pg: Option<ObjectId>, mcid: i64, buf: &mut String) {
        if let Some(pg) = pg {
            if let Some(t) = self.mcid_text.get(&pg).and_then(|m| m.get(&mcid)) {
                buf.push_str(t);
            }
        }
    }
}

// --- MCID → text extraction from page content streams -----------------------

fn build_mcid_text(pdf: &Pdf) -> HashMap<ObjectId, HashMap<i64, String>> {
    let mut out = HashMap::new();
    for (_num, page_id) in pdf.get_pages() {
        let mut map = HashMap::new();
        extract_page_mcid_text(pdf, page_id, &mut map);
        out.insert(page_id, map);
    }
    out
}

fn extract_page_mcid_text(pdf: &Pdf, page_id: ObjectId, map: &mut HashMap<i64, String>) {
    let Ok(fonts) = pdf.get_page_fonts(page_id) else {
        return;
    };
    let encodings: BTreeMap<Vec<u8>, Encoding> = fonts
        .into_iter()
        .filter_map(|(name, font)| font.get_font_encoding(pdf).ok().map(|e| (name, e)))
        .collect();
    let properties = page_properties(pdf, page_id);
    let Ok(content) = pdf.get_and_decode_page_content(page_id) else {
        return;
    };

    let mut cur_enc: Option<&Encoding> = None;
    // Stack of marked-content nesting; each entry is the MCID it declared
    // (or None for a BMC / BDC without an MCID).
    let mut mc_stack: Vec<Option<i64>> = Vec::new();

    for op in &content.operations {
        match op.operator.as_str() {
            "Tf" => {
                cur_enc = op
                    .operands
                    .first()
                    .and_then(|o| o.as_name().ok())
                    .and_then(|n| encodings.get(n));
            }
            "BDC" => mc_stack.push(bdc_mcid(&op.operands, &properties)),
            "BMC" => mc_stack.push(None),
            "EMC" => {
                mc_stack.pop();
            }
            "Tj" | "TJ" => {
                let Some(enc) = cur_enc else { continue };
                // Innermost enclosing MCID, if any.
                let Some(mcid) = mc_stack.iter().rev().find_map(|m| *m) else {
                    continue;
                };
                let mut s = String::new();
                collect_show(&op.operands, enc, &mut s);
                if !s.is_empty() {
                    map.entry(mcid).or_default().push_str(&s);
                }
            }
            _ => {}
        }
    }
}

/// The MCID declared by a `BDC` operator: `tag properties BDC`. The
/// properties are either an inline dict `<< /MCID n >>` or a name keyed
/// into the page's `/Resources /Properties`.
fn bdc_mcid(operands: &[Object], properties: &Dictionary) -> Option<i64> {
    let props = operands.get(1)?;
    let dict = match props {
        Object::Dictionary(d) => d.clone(),
        Object::Name(n) => match properties.get(n).ok()? {
            Object::Dictionary(d) => d.clone(),
            _ => return None,
        },
        _ => return None,
    };
    match dict.get(b"MCID").ok()? {
        Object::Integer(n) => Some(*n),
        _ => None,
    }
}

/// Decode the text shown by a `Tj` (string) or `TJ` (array) operator.
fn collect_show(operands: &[Object], enc: &Encoding, out: &mut String) {
    for op in operands {
        match op {
            Object::String(bytes, _) => {
                if let Ok(t) = Pdf::decode_text(enc, bytes) {
                    out.push_str(&t);
                }
            }
            Object::Array(arr) => collect_show(arr, enc, out),
            _ => {}
        }
    }
}

/// The page's `/Resources /Properties` dict (named marked-content props).
fn page_properties(pdf: &Pdf, page_id: ObjectId) -> Dictionary {
    (|| {
        let page = pdf.get_dictionary(page_id).ok()?;
        let res = resolve_dict(pdf, page.get(b"Resources").ok()?)?;
        resolve_dict(pdf, res.get(b"Properties").ok()?)
    })()
    .unwrap_or_default()
}

// --- StructTreeRoot access + small PDF helpers ------------------------------

fn struct_tree_root(pdf: &Pdf) -> Option<Dictionary> {
    let catalog = pdf.catalog().ok()?;
    let root = resolve_dict(pdf, catalog.get(b"StructTreeRoot").ok()?)?;
    // A usable tree must have at least one kid.
    root.get(b"K").ok()?;
    Some(root)
}

/// Resolve the `/K` of a node into a flat list of objects (it may be a
/// single object or an array; references are followed).
fn resolve_kids(pdf: &Pdf, k: &Object) -> Vec<Object> {
    match deref(pdf, k) {
        Some(Object::Array(arr)) => arr.iter().filter_map(|o| deref(pdf, o)).collect::<Vec<_>>(),
        Some(other) => vec![other],
        None => Vec::new(),
    }
}

/// The standard structure type (`/S`), namespace prefix stripped.
fn struct_type(elem: &Dictionary) -> Option<String> {
    match elem.get(b"S").ok()? {
        Object::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
        _ => None,
    }
}

fn kids_of(pdf: &Pdf, elem: &Dictionary) -> Vec<Dictionary> {
    let mut out = Vec::new();
    if let Ok(k) = elem.get(b"K") {
        for o in resolve_kids(pdf, k) {
            if let Object::Dictionary(d) = o {
                if d.get(b"S").is_ok() {
                    out.push(d);
                }
            }
        }
    }
    out
}

fn elem_alt(elem: &Dictionary) -> Option<String> {
    match elem.get(b"Alt").ok()? {
        Object::String(s, _) => {
            let t = String::from_utf8_lossy(s).trim().to_owned();
            (!t.is_empty()).then_some(t)
        }
        _ => None,
    }
}

fn is_heading(s: &str) -> bool {
    s == "H" || (s.len() == 2 && s.starts_with('H') && s.as_bytes()[1].is_ascii_digit())
}

fn heading_level(s: &str) -> u8 {
    if let Some(d) = s.strip_prefix('H').and_then(|r| r.parse::<u8>().ok()) {
        d.clamp(1, 6)
    } else {
        1 // bare <H>
    }
}

/// Determine list ordering from the `L` element's `/A` ListNumbering
/// attribute (defaults to unordered).
fn list_is_ordered(pdf: &Pdf, elem: &Dictionary) -> bool {
    for attr in attribute_dicts(pdf, elem) {
        if let Ok(Object::Name(n)) = attr.get(b"ListNumbering") {
            return matches!(
                n.as_slice(),
                b"Decimal" | b"UpperRoman" | b"LowerRoman" | b"UpperAlpha" | b"LowerAlpha"
            );
        }
    }
    false
}

/// Header-cell scope from a `TH`'s `/A` Scope attribute (defaults to
/// column, the common case).
fn cell_scope(pdf: &Pdf, elem: &Dictionary) -> CellScope {
    for attr in attribute_dicts(pdf, elem) {
        if let Ok(Object::Name(n)) = attr.get(b"Scope") {
            return match n.as_slice() {
                b"Row" => CellScope::Row,
                _ => CellScope::Col,
            };
        }
    }
    CellScope::Col
}

/// The attribute dictionaries of an element's `/A` (a dict, or an array
/// that may interleave dicts with revision-number integers).
fn attribute_dicts(pdf: &Pdf, elem: &Dictionary) -> Vec<Dictionary> {
    let mut out = Vec::new();
    if let Ok(a) = elem.get(b"A") {
        match deref(pdf, a) {
            Some(Object::Dictionary(d)) => out.push(d),
            Some(Object::Array(arr)) => {
                for o in &arr {
                    if let Some(Object::Dictionary(d)) = deref(pdf, o) {
                        out.push(d);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn deref(pdf: &Pdf, obj: &Object) -> Option<Object> {
    match obj {
        Object::Reference(id) => pdf.get_object(*id).ok().cloned(),
        other => Some(other.clone()),
    }
}

fn resolve_dict(pdf: &Pdf, obj: &Object) -> Option<Dictionary> {
    match deref(pdf, obj)? {
        Object::Dictionary(d) => Some(d),
        _ => None,
    }
}

/// Collapse runs of whitespace to single spaces and trim — PDF text
/// runs carry irregular spacing.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
