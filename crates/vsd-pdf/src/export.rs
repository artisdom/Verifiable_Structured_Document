//! VSD → PDF export (spec §11).
//!
//! Lossless for visuals by construction: VSD display lists are a strict
//! subset of PDF's imaging model, so emission is mechanical. The export
//! is **tagged** — the structure tree is rebuilt from the display
//! list's `(node_path)` back-references into the content tree, so the
//! resulting PDF carries real H1–H6/P/Code/Caption/Formula semantics
//! and figure alt text, making it better-tagged than most native PDFs.
//!
//! For *content* losslessness the export embeds the canonical `.vsd`
//! source as a PDF attachment (a "hybrid PDF"): importing recovers the
//! exact original, verifiable by document id. Organizations can adopt
//! VSD internally with zero external-compatibility risk — the only
//! adoption posture that has ever worked.
//!
//! The writer is deterministic: no timestamps, no randomness, fixed
//! compression — same document, same bytes.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use vsd_core::document::Document;
use vsd_core::layout::{DisplayOp, Page};
use vsd_core::manifest::Blob;
use vsd_core::tree::Node;
use vsd_core::ObjectId as VsdId;
use vsd_layout::font::{FontMetrics, FONT_BYTES};

use crate::write::{flate, pdf_string, ObjId, PdfWriter};
use crate::{PdfError, Result};

const PT_PER_MM: f64 = 72.0 / 25.4;

#[derive(Clone, Debug)]
pub struct ExportOptions {
    /// Embed the canonical `.vsd` bytes as a PDF attachment, enabling
    /// verifiable lossless round-trips. On by default.
    pub embed_source: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        ExportOptions { embed_source: true }
    }
}

/// Export a document to PDF bytes. `source_bytes`, when provided (and
/// `embed_source` is on), is attached verbatim — pass the original
/// container so signatures travel inside the PDF.
pub fn export_pdf(
    doc: &Document,
    source_bytes: Option<&[u8]>,
    opts: &ExportOptions,
) -> Result<Vec<u8>> {
    // Display lists: from the render cache when present, else a fresh
    // deterministic layout at A4.
    let pages: Vec<Page> = match doc.render_cache()? {
        Some(cache) => cache
            .pages
            .iter()
            .map(|id| Page::from_value(&doc.store.get_value(id)?))
            .collect::<vsd_core::Result<_>>()?,
        None => vsd_layout::layout_document(doc, &vsd_layout::LayoutOptions::default())
            .map_err(|e| PdfError::Layout(e.to_string()))?,
    };

    let root = doc.root_node()?;
    let alts = collect_figure_alts(doc, &root)?;
    let metrics = FontMetrics::get();

    // --- Glyph usage across the whole document ---------------------------
    let mut gid_to_char: BTreeMap<u16, char> = BTreeMap::new();
    for page in &pages {
        for op in &page.ops {
            if let DisplayOp::TextRun { text, .. } = op {
                for c in text.chars().filter(|c| !c.is_control()) {
                    gid_to_char.entry(metrics.glyph(c).0).or_insert(c);
                }
            }
        }
    }

    let mut w = PdfWriter::new();

    // --- Font objects ------------------------------------------------------
    let font_ref = embed_font(&mut w, metrics, &gid_to_char);

    // --- Image XObjects (deduplicated by content address) ------------------
    let mut images: Vec<(VsdId, ObjId)> = Vec::new();
    for page in &pages {
        for op in &page.ops {
            if let DisplayOp::Image { res, .. } = op {
                if !images.iter().any(|(id, _)| id == res) {
                    if let Some(obj) = embed_image(&mut w, doc, res) {
                        images.push((*res, obj));
                    }
                }
            }
        }
    }

    // --- Pages: content streams + marked-content records --------------------
    let pages_obj = w.alloc();
    let mut page_refs = Vec::new();
    let mut page_records: Vec<Vec<McRecord>> = Vec::new();
    for (page_i, page) in pages.iter().enumerate() {
        let (content, records) = page_content(page, metrics, &images, &alts, doc, &root);
        let compressed = flate(content.as_bytes());
        let content_obj = w.stream("/Filter /FlateDecode", &compressed);

        let mut xobjects = String::new();
        for (i, (_, obj)) in images.iter().enumerate() {
            let _ = write!(xobjects, "/Im{} {} ", i, obj.r());
        }
        let page_obj = w.add(format!(
            "<< /Type /Page /Parent {} /MediaBox [0 0 {:.3} {:.3}] /Contents {} \
             /StructParents {} /Resources << /Font << /F1 {} >> /XObject << {} >> >> >>",
            pages_obj.r(),
            page.width_mm * PT_PER_MM,
            page.height_mm * PT_PER_MM,
            content_obj.r(),
            page_i,
            font_ref.r(),
            xobjects,
        ));
        page_refs.push(page_obj);
        page_records.push(records);
    }
    let kids: Vec<String> = page_refs.iter().map(|p| p.r()).collect();
    w.set(
        pages_obj,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            page_refs.len()
        ),
    );

    // --- Structure tree ------------------------------------------------------
    let struct_root = build_struct_tree(&mut w, &page_refs, &page_records);

    // --- Embedded canonical source (the hybrid trick) ------------------------
    let mut names_entry = String::new();
    let mut af_entry = String::new();
    if opts.embed_source {
        if let Some(bytes) = source_bytes {
            let ef_stream = w.stream(
                &format!("/Type /EmbeddedFile /Params << /Size {} >>", bytes.len()),
                bytes,
            );
            let filespec = w.add(format!(
                "<< /Type /Filespec /F (source.vsd) /UF (source.vsd) \
                 /Desc (Canonical VSD source of this document; import with vsd-pdf to \
                 recover it losslessly and verify its identity) \
                 /AFRelationship /Source /EF << /F {} >> >>",
                ef_stream.r()
            ));
            names_entry = format!(
                "/Names << /EmbeddedFiles << /Names [(source.vsd) {}] >> >> ",
                filespec.r()
            );
            af_entry = format!("/AF [{}] ", filespec.r());
        }
    }

    // --- Catalog & info ------------------------------------------------------
    let catalog = w.add(format!(
        "<< /Type /Catalog /Pages {} /MarkInfo << /Marked true >> /StructTreeRoot {} \
         /ViewerPreferences << /DisplayDocTitle true >> /Lang (en) {}{}>>",
        pages_obj.r(),
        struct_root.r(),
        names_entry,
        af_entry,
    ));

    let meta = doc.metadata()?;
    let doc_id = doc.document_id()?;
    let mut info = String::from("<< ");
    if let Some(title) = &meta.title {
        let _ = write!(info, "/Title {} ", pdf_string(title));
    }
    if !meta.authors.is_empty() {
        let _ = write!(info, "/Author {} ", pdf_string(&meta.authors.join("; ")));
    }
    let _ = write!(
        info,
        "/Producer (vsd-pdf {}) /Keywords (vsd-doc-id:{}) >>",
        env!("CARGO_PKG_VERSION"),
        doc_id.to_hex()
    );
    let info_obj = w.add(info);

    Ok(w.finish(catalog, Some(info_obj)))
}

// --- Content stream -----------------------------------------------------------

/// One marked-content sequence on a page.
struct McRecord {
    mcid: u32,
    tag: &'static str,
    node_path: Vec<u64>,
    alt: Option<String>,
}

fn page_content(
    page: &Page,
    metrics: &FontMetrics,
    images: &[(VsdId, ObjId)],
    alts: &BTreeMap<VsdId, String>,
    doc: &Document,
    root: &Node,
) -> (String, Vec<McRecord>) {
    let h_pt = page.height_mm * PT_PER_MM;
    let mut s = String::new();
    let mut records = Vec::new();
    let mut mcid = 0u32;

    for op in &page.ops {
        match op {
            DisplayOp::Rect { x, y, w, h, fill } => {
                // Decoration: rules, backgrounds, redaction bars are
                // artifacts, invisible to assistive tech by design.
                let _ = writeln!(
                    s,
                    "/Artifact BMC q {} {:.3} {:.3} {:.3} {:.3} re f Q EMC",
                    rg(*fill),
                    x * PT_PER_MM,
                    h_pt - (y + h) * PT_PER_MM,
                    w * PT_PER_MM,
                    h * PT_PER_MM,
                );
            }
            DisplayOp::TextRun {
                x,
                y,
                size_pt,
                color,
                text,
                node_path,
                ..
            } => {
                let tag = struct_tag(doc, root, node_path);
                let mut hexes = String::with_capacity(text.len() * 4);
                for c in text.chars().filter(|c| !c.is_control()) {
                    let _ = write!(hexes, "{:04x}", metrics.glyph(c).0);
                }
                let _ = writeln!(
                    s,
                    "/{tag} << /MCID {mcid} >> BDC {} BT /F1 {:.3} Tf {:.3} {:.3} Td <{hexes}> Tj ET EMC",
                    rg(*color),
                    size_pt,
                    x * PT_PER_MM,
                    h_pt - y * PT_PER_MM,
                );
                records.push(McRecord {
                    mcid,
                    tag,
                    node_path: node_path.clone(),
                    alt: None,
                });
                mcid += 1;
            }
            DisplayOp::Image { x, y, w, h, res } => {
                let Some(idx) = images.iter().position(|(id, _)| id == res) else {
                    continue;
                };
                let _ = writeln!(
                    s,
                    "/Figure << /MCID {mcid} >> BDC q {:.3} 0 0 {:.3} {:.3} {:.3} cm /Im{idx} Do Q EMC",
                    w * PT_PER_MM,
                    h * PT_PER_MM,
                    x * PT_PER_MM,
                    h_pt - (y + h) * PT_PER_MM,
                );
                records.push(McRecord {
                    mcid,
                    tag: "Figure",
                    node_path: Vec::new(),
                    alt: alts.get(res).cloned(),
                });
                mcid += 1;
            }
        }
    }
    (s, records)
}

fn rg(c: [u8; 4]) -> String {
    format!(
        "{:.4} {:.4} {:.4} rg",
        c[0] as f64 / 255.0,
        c[1] as f64 / 255.0,
        c[2] as f64 / 255.0
    )
}

// --- Structure tree -------------------------------------------------------------

/// Map a display-list node path to a standard PDF structure type by
/// resolving it against the content tree.
fn struct_tag(doc: &Document, root: &Node, path: &[u64]) -> &'static str {
    match resolve_node(doc, root, path) {
        Some(Node::Heading(h)) => match h.level {
            1 => "H1",
            2 => "H2",
            3 => "H3",
            4 => "H4",
            5 => "H5",
            _ => "H6",
        },
        Some(Node::Code(_)) => "Code",
        Some(Node::Math(_)) => "Formula",
        Some(Node::Figure(_)) => "Caption", // caption runs carry the figure's path
        Some(Node::List(_)) => "Lbl",       // list label runs carry the list's path
        _ => "P",
    }
}

/// Resolve a node path per the LAYOUT-1.0.md §9 semantics.
fn resolve_node(doc: &Document, node: &Node, path: &[u64]) -> Option<Node> {
    let node = match node {
        Node::SubtreeRef(id) => Node::from_value(&doc.store.get_value(id).ok()?).ok()?,
        other => other.clone(),
    };
    let Some((&idx, rest)) = path.split_first() else {
        return Some(node);
    };
    let idx = idx as usize;
    match &node {
        Node::Doc(d) => resolve_node(doc, d.children.get(idx)?, rest),
        Node::Section(s) => resolve_node(doc, s.children.get(idx)?, rest),
        Node::List(l) => {
            // [item, block, …]
            let item = l.items.get(idx)?;
            let (&block, rest2) = rest.split_first()?;
            resolve_node(doc, item.get(block as usize)?, rest2)
        }
        Node::Table(t) => {
            // [cell (reading order), block, …]
            let cell = t
                .head
                .iter()
                .chain(&t.body)
                .chain(&t.foot)
                .flat_map(|r| &r.cells)
                .nth(idx)?;
            let (&block, rest2) = rest.split_first()?;
            resolve_node(doc, cell.children.get(block as usize)?, rest2)
        }
        _ => None,
    }
}

fn collect_figure_alts(doc: &Document, root: &Node) -> Result<BTreeMap<VsdId, String>> {
    let mut out = BTreeMap::new();
    walk_alts(doc, root, &mut out)?;
    Ok(out)
}

fn walk_alts(doc: &Document, node: &Node, out: &mut BTreeMap<VsdId, String>) -> Result<()> {
    match node {
        Node::Figure(f) => {
            out.entry(f.res).or_insert_with(|| f.alt.clone());
        }
        Node::Doc(d) => {
            for c in &d.children {
                walk_alts(doc, c, out)?;
            }
        }
        Node::Section(s) => {
            for c in &s.children {
                walk_alts(doc, c, out)?;
            }
        }
        Node::List(l) => {
            for item in &l.items {
                for c in item {
                    walk_alts(doc, c, out)?;
                }
            }
        }
        Node::Table(t) => {
            for row in t.head.iter().chain(&t.body).chain(&t.foot) {
                for cell in &row.cells {
                    for c in &cell.children {
                        walk_alts(doc, c, out)?;
                    }
                }
            }
        }
        Node::SubtreeRef(id) => {
            let sub = Node::from_value(&doc.store.get_value(id)?)?;
            walk_alts(doc, &sub, out)?;
        }
        _ => {}
    }
    Ok(())
}

/// Build StructTreeRoot, the Document element, per-block elements
/// (grouping consecutive same-path runs), and the parent tree.
fn build_struct_tree(
    w: &mut PdfWriter,
    page_refs: &[ObjId],
    page_records: &[Vec<McRecord>],
) -> ObjId {
    let struct_root = w.alloc();
    let doc_elem = w.alloc();

    let mut elem_refs: Vec<ObjId> = Vec::new();
    let mut parent_tree_pages: Vec<ObjId> = Vec::new();

    for (page_i, records) in page_records.iter().enumerate() {
        // mcid → element ref for this page's parent-tree entry.
        let mut by_mcid: Vec<ObjId> = Vec::new();
        let mut i = 0usize;
        while i < records.len() {
            // Group consecutive records with the same tag + node path
            // (one logical block split across lines).
            let mut j = i + 1;
            while j < records.len()
                && records[j].tag == records[i].tag
                && records[j].node_path == records[i].node_path
                && records[i].alt.is_none()
                && records[j].alt.is_none()
            {
                j += 1;
            }
            let mcids: Vec<String> = records[i..j].iter().map(|r| r.mcid.to_string()).collect();
            let alt = records[i]
                .alt
                .as_ref()
                .map(|a| format!("/Alt {} ", pdf_string(a)))
                .unwrap_or_default();
            let elem = w.add(format!(
                "<< /Type /StructElem /S /{} /P {} /Pg {} {}/K [{}] >>",
                records[i].tag,
                doc_elem.r(),
                page_refs[page_i].r(),
                alt,
                mcids.join(" ")
            ));
            elem_refs.push(elem);
            for _ in i..j {
                by_mcid.push(elem);
            }
            i = j;
        }
        let arr = w.add(format!(
            "[{}]",
            by_mcid.iter().map(|e| e.r()).collect::<Vec<_>>().join(" ")
        ));
        parent_tree_pages.push(arr);
    }

    let nums: Vec<String> = parent_tree_pages
        .iter()
        .enumerate()
        .map(|(i, arr)| format!("{} {}", i, arr.r()))
        .collect();
    let parent_tree = w.add(format!("<< /Nums [{}] >>", nums.join(" ")));

    w.set(
        doc_elem,
        format!(
            "<< /Type /StructElem /S /Document /P {} /K [{}] >>",
            struct_root.r(),
            elem_refs
                .iter()
                .map(|e| e.r())
                .collect::<Vec<_>>()
                .join(" ")
        ),
    );
    w.set(
        struct_root,
        format!(
            "<< /Type /StructTreeRoot /K {} /ParentTree {} /ParentTreeNextKey {} >>",
            doc_elem.r(),
            parent_tree.r(),
            page_records.len()
        ),
    );
    struct_root
}

// --- Font embedding ---------------------------------------------------------------

fn embed_font(w: &mut PdfWriter, metrics: &FontMetrics, gids: &BTreeMap<u16, char>) -> ObjId {
    let face = metrics.face();

    // FontFile2: the pinned TTF, flate-compressed.
    let compressed = flate(FONT_BYTES);
    let font_file = w.stream(
        &format!("/Filter /FlateDecode /Length1 {}", FONT_BYTES.len()),
        &compressed,
    );

    let bbox = face.global_bounding_box();
    let descriptor = w.add(format!(
        "<< /Type /FontDescriptor /FontName /NotoSans-Regular /Flags 32 \
         /FontBBox [{} {} {} {}] /ItalicAngle 0 /Ascent {} /Descent {} \
         /CapHeight {} /StemV 80 /FontFile2 {} >>",
        bbox.x_min,
        bbox.y_min,
        bbox.x_max,
        bbox.y_max,
        metrics.ascent_units,
        metrics.descent_units,
        face.capital_height().unwrap_or(714),
        font_file.r()
    ));

    // Widths for used glyphs (Noto Sans upem = 1000 = PDF glyph space).
    let mut w_array = String::new();
    for (&gid, _) in gids.iter() {
        let adv = metrics.advance_units(ttf_gid(gid));
        let _ = write!(w_array, "{gid} [{adv}] ");
    }
    let cid_font = w.add(format!(
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /NotoSans-Regular \
         /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
         /FontDescriptor {} /DW 600 /W [{}] /CIDToGIDMap /Identity >>",
        descriptor.r(),
        w_array.trim_end()
    ));

    // ToUnicode CMap so text extraction and copy/paste work everywhere.
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(u16, char)> = gids.iter().map(|(&g, &c)| (g, c)).collect();
    for chunk in entries.chunks(100) {
        let _ = writeln!(cmap, "{} beginbfchar", chunk.len());
        for (gid, c) in chunk {
            let mut buf = [0u16; 2];
            let units = c.encode_utf16(&mut buf);
            let hex: String = units.iter().map(|u| format!("{u:04X}")).collect();
            let _ = writeln!(cmap, "<{gid:04X}> <{hex}>");
        }
        let _ = writeln!(cmap, "endbfchar");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    let to_unicode = w.stream("/Filter /FlateDecode", &flate(cmap.as_bytes()));

    w.add(format!(
        "<< /Type /Font /Subtype /Type0 /BaseFont /NotoSans-Regular /Encoding /Identity-H \
         /DescendantFonts [{}] /ToUnicode {} >>",
        cid_font.r(),
        to_unicode.r()
    ))
}

fn ttf_gid(gid: u16) -> vsd_layout::font::GlyphId {
    vsd_layout::font::GlyphId(gid)
}

// --- Images ------------------------------------------------------------------------

/// Embed an image resource as a PDF XObject. PNG decodes to RGB +
/// Flate; JPEG passes through as DCTDecode. Returns None for media the
/// exporter cannot represent (the display list draws nothing for them).
fn embed_image(w: &mut PdfWriter, doc: &Document, res: &VsdId) -> Option<ObjId> {
    let blob = Blob::from_value(&doc.store.get_value(res).ok()?).ok()?;
    match blob.mime.as_str() {
        "image/png" => {
            let pixmap = tiny_skia::Pixmap::decode_png(&blob.data).ok()?;
            // Composite on white and drop alpha (1.0 limitation).
            let mut rgb =
                Vec::with_capacity(pixmap.width() as usize * pixmap.height() as usize * 3);
            for px in pixmap.pixels() {
                let p = px.demultiply();
                let a = p.alpha() as u32;
                let blend = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
                rgb.extend_from_slice(&[blend(p.red()), blend(p.green()), blend(p.blue())]);
            }
            let data = flate(&rgb);
            Some(w.stream(
                &format!(
                    "/Type /XObject /Subtype /Image /Width {} /Height {} \
                     /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode",
                    pixmap.width(),
                    pixmap.height()
                ),
                &data,
            ))
        }
        "image/jpeg" => {
            let (width, height) = jpeg_dims(&blob.data)?;
            Some(w.stream(
                &format!(
                    "/Type /XObject /Subtype /Image /Width {width} /Height {height} \
                     /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode"
                ),
                &blob.data,
            ))
        }
        _ => None,
    }
}

/// Width/height from a JPEG SOF marker.
fn jpeg_dims(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 4 || data[0] != 0xff || data[1] != 0xd8 {
        return None;
    }
    let mut i = 2usize;
    while i + 9 < data.len() {
        if data[i] != 0xff {
            return None;
        }
        let marker = data[i + 1];
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if matches!(marker, 0xc0..=0xc3) {
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}
