//! # vsd-render
//!
//! Rasterizes VSD display-list pages to pixels via tiny-skia.
//!
//! Division of trust: the **display list is the normative artifact** —
//! deterministic, hashed, recomputable (see `vsd-layout`). The raster
//! is an *informative view* of it: pixel output is golden-tested but
//! not part of the document identity. Glyph outlines come from the same
//! pinned font the layout engine measured with, so what is drawn is
//! what was measured.

#![forbid(unsafe_code)]

use thiserror::Error;
use tiny_skia::{
    Color, FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Rect as SkRect, Transform,
};

use vsd_core::document::Document;
use vsd_core::layout::{DisplayOp, Page};
use vsd_core::manifest::Blob;
use vsd_layout::font::{Face, FontMetrics};

pub type Result<T> = std::result::Result<T, RenderError>;

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("page too large to rasterize at this dpi")]
    TooLarge,

    #[error("png encode failed: {0}")]
    Encode(String),

    #[error(transparent)]
    Core(#[from] vsd_core::Error),
}

/// Render one page at the given resolution. White background, sRGB.
pub fn render_page(doc: &Document, page: &Page, dpi: f64) -> Result<Pixmap> {
    let ppm = dpi / 25.4; // pixels per millimetre
    let w_px = (page.width_mm * ppm).ceil() as u32;
    let h_px = (page.height_mm * ppm).ceil() as u32;
    let mut pixmap = Pixmap::new(w_px.max(1), h_px.max(1)).ok_or(RenderError::TooLarge)?;
    pixmap.fill(Color::WHITE);

    for op in &page.ops {
        match op {
            DisplayOp::Rect { x, y, w, h, fill } => {
                draw_rect(&mut pixmap, *x * ppm, *y * ppm, *w * ppm, *h * ppm, *fill);
            }
            DisplayOp::TextRun {
                x,
                y,
                font,
                size_pt,
                color,
                rtl,
                text,
                ..
            } => {
                draw_text_run(
                    &mut pixmap,
                    *x * ppm,
                    *y * ppm,
                    size_pt * dpi / 72.0,
                    Face::from_index(*font),
                    *color,
                    text,
                    *rtl,
                );
            }
            DisplayOp::Image { x, y, w, h, res } => {
                draw_image(
                    doc,
                    &mut pixmap,
                    *x * ppm,
                    *y * ppm,
                    *w * ppm,
                    *h * ppm,
                    res,
                )?;
            }
        }
    }
    Ok(pixmap)
}

/// Render a page to PNG bytes.
pub fn render_page_png(doc: &Document, page: &Page, dpi: f64) -> Result<Vec<u8>> {
    render_page(doc, page, dpi)?
        .encode_png()
        .map_err(|e| RenderError::Encode(e.to_string()))
}

/// Draw a UI label with the engine font (used by viewers for chrome
/// like verification banners — same pinned face as document text).
pub fn draw_label(
    pixmap: &mut Pixmap,
    x_px: f64,
    baseline_px: f64,
    size_px: f64,
    color: [u8; 4],
    text: &str,
) {
    draw_text(pixmap, x_px, baseline_px, size_px, color, text);
}

/// Fill an axis-aligned rectangle (viewer chrome / highlights).
pub fn fill_rect_px(pixmap: &mut Pixmap, x: f64, y: f64, w: f64, h: f64, color: [u8; 4]) {
    draw_rect(pixmap, x, y, w, h, color);
}

fn rgba(c: [u8; 4]) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], c[3])
}

fn draw_rect(pixmap: &mut Pixmap, x: f64, y: f64, w: f64, h: f64, fill: [u8; 4]) {
    // Hairlines must survive rasterization: never thinner than 1 px.
    let Some(rect) =
        SkRect::from_xywh(x as f32, y as f32, (w as f32).max(1.0), (h as f32).max(1.0))
    else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color(rgba(fill));
    paint.anti_alias = false;
    pixmap.fill_rect(rect, &paint, Transform::identity(), None);
}

fn draw_text(
    pixmap: &mut Pixmap,
    x: f64,
    baseline_y: f64,
    size_px: f64,
    color: [u8; 4],
    text: &str,
) {
    draw_text_face(pixmap, x, baseline_y, size_px, Face::Regular, color, text)
}

fn draw_text_face(
    pixmap: &mut Pixmap,
    x: f64,
    baseline_y: f64,
    size_px: f64,
    typeface: Face,
    color: [u8; 4],
    text: &str,
) {
    draw_text_run(pixmap, x, baseline_y, size_px, typeface, color, text, false)
}

/// Draw a run with a specific face — glyph outlines and advances both
/// come from the face the engine measured with. An RTL run stores its
/// text in logical order; the glyphs are placed right-to-left from the
/// run's right edge, i.e. drawn in reversed logical order from `x`
/// (the run's left edge, format 0.3).
#[allow(clippy::too_many_arguments)]
fn draw_text_run(
    pixmap: &mut Pixmap,
    x: f64,
    baseline_y: f64,
    size_px: f64,
    typeface: Face,
    color: [u8; 4],
    text: &str,
    rtl: bool,
) {
    let metrics = FontMetrics::face_metrics(typeface);
    let face = metrics.face();
    let scale = size_px as f32 / metrics.upem as f32;

    let mut paint = Paint::default();
    paint.set_color(rgba(color));
    paint.anti_alias = true;

    let chars: Vec<char> = if rtl {
        text.chars().rev().collect()
    } else {
        text.chars().collect()
    };
    let mut pen_x = x as f32;
    let y0 = baseline_y as f32;
    for c in chars {
        if c.is_control() {
            continue;
        }
        let gid = metrics.glyph(c);
        let mut sink = GlyphSink {
            pb: PathBuilder::new(),
            scale,
            x0: pen_x,
            y0,
        };
        if face.outline_glyph(gid, &mut sink).is_some() {
            if let Some(path) = sink.pb.finish() {
                pixmap.fill_path(
                    &path,
                    &paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
        pen_x += metrics.advance_units(gid) as f32 * scale;
    }
}

/// Adapts ttf-parser outlines into a tiny-skia path, scaling font units
/// to pixels and flipping the y axis (TTF is y-up, raster is y-down).
struct GlyphSink {
    pb: PathBuilder,
    scale: f32,
    x0: f32,
    y0: f32,
}

impl GlyphSink {
    fn tx(&self, x: f32) -> f32 {
        self.x0 + x * self.scale
    }

    fn ty(&self, y: f32) -> f32 {
        self.y0 - y * self.scale
    }
}

impl vsd_layout::font::OutlineBuilder for GlyphSink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb.move_to(self.tx(x), self.ty(y));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.pb.line_to(self.tx(x), self.ty(y));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.pb
            .quad_to(self.tx(x1), self.ty(y1), self.tx(x), self.ty(y));
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.pb.cubic_to(
            self.tx(x1),
            self.ty(y1),
            self.tx(x2),
            self.ty(y2),
            self.tx(x),
            self.ty(y),
        );
    }

    fn close(&mut self) {
        self.pb.close();
    }
}

fn draw_image(
    doc: &Document,
    pixmap: &mut Pixmap,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    res: &vsd_core::ObjectId,
) -> Result<()> {
    let placeholder = |pixmap: &mut Pixmap| {
        draw_rect(pixmap, x, y, w, h, [0xd0, 0xd0, 0xd0, 0xff]);
    };
    let Ok(value) = doc.store.get_value(res) else {
        placeholder(pixmap);
        return Ok(());
    };
    let Ok(blob) = Blob::from_value(&value) else {
        placeholder(pixmap);
        return Ok(());
    };
    if blob.mime == "image/png" {
        if let Ok(src) = Pixmap::decode_png(&blob.data) {
            let sx = (w as f32) / src.width() as f32;
            let sy = (h as f32) / src.height() as f32;
            let transform = Transform::from_row(sx, 0.0, 0.0, sy, x as f32, y as f32);
            pixmap.draw_pixmap(0, 0, src.as_ref(), &PixmapPaint::default(), transform, None);
            return Ok(());
        }
    }
    placeholder(pixmap);
    Ok(())
}
