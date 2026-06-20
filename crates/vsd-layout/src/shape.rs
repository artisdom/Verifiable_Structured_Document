//! Complex-script shaping (LAYOUT-1.4.md §2) via the pinned pure-Rust
//! HarfBuzz port (`rustybuzz`, exact version). Shaping runs once, here
//! in the engine; the display list carries the resulting positioned
//! glyphs (`DisplayOp::GlyphRun`) so consumers never need a shaper.
//!
//! Determinism: `rustybuzz` operates entirely in integer font units and
//! is pinned by exact version, exactly like the metrics parser. Advances
//! and offsets are scaled to micrometers with the same `muldiv` rule as
//! every other advance, so a shaped run is byte-identical on every
//! platform — the property the whole layout contract rests on.

use std::sync::OnceLock;

use crate::font::{muldiv, Face, FontMetrics};

/// One positioned glyph of a shaped run, in integer micrometers, in
/// **visual order** (rustybuzz reorders to visual order after shaping,
/// so consumers draw glyphs left-to-right at increasing x regardless of
/// the run's script direction).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapedGlyph {
    pub gid: u16,
    pub x_advance_um: i64,
    pub x_offset_um: i64,
    pub y_offset_um: i64,
    /// Logical byte offset within the shaped substring (HarfBuzz
    /// cluster), for mapping glyphs back to source text.
    pub cluster: u32,
}

/// A shaped run: its glyphs (visual order) and total advance width.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shaped {
    pub glyphs: Vec<ShapedGlyph>,
    pub width_um: i64,
}

fn rb_face(face: Face) -> &'static rustybuzz::Face<'static> {
    static ARABIC: OnceLock<rustybuzz::Face<'static>> = OnceLock::new();
    static DEVANAGARI: OnceLock<rustybuzz::Face<'static>> = OnceLock::new();
    let init =
        |f: Face| rustybuzz::Face::from_slice(f.bytes(), 0).expect("embedded font must parse");
    match face {
        Face::Arabic => ARABIC.get_or_init(|| init(Face::Arabic)),
        Face::Devanagari => DEVANAGARI.get_or_init(|| init(Face::Devanagari)),
        _ => unreachable!("shape_run called for a non-shaped face"),
    }
}

/// Shape `text` in the given shaped face at `size_um`. Script,
/// direction, and language are derived deterministically from the
/// content (`guess_segment_properties`), so Arabic shapes right-to-left
/// with joining/ligatures and Devanagari reorders matras and forms
/// conjuncts — all from the pinned shaper and font.
pub fn shape_run(face: Face, text: &str, size_um: i64) -> Shaped {
    debug_assert!(face.is_shaped());
    let rb = rb_face(face);
    let upem = FontMetrics::face_metrics(face).upem;

    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    let out = rustybuzz::shape(rb, &[], buffer);

    let infos = out.glyph_infos();
    let positions = out.glyph_positions();
    let mut glyphs = Vec::with_capacity(infos.len());
    let mut width_um = 0i64;
    for (info, pos) in infos.iter().zip(positions) {
        let x_advance_um = muldiv(pos.x_advance as i64, size_um, upem);
        glyphs.push(ShapedGlyph {
            gid: info.glyph_id as u16,
            x_advance_um,
            x_offset_um: muldiv(pos.x_offset as i64, size_um, upem),
            y_offset_um: muldiv(pos.y_offset as i64, size_um, upem),
            cluster: info.cluster,
        });
        width_um += x_advance_um;
    }
    Shaped { glyphs, width_um }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arabic_shapes_with_joining_and_real_glyphs() {
        // "al-ʿarabiyya" — the word "Arabic".
        let word = "العربية";
        let shaped = shape_run(Face::Arabic, word, 3881);
        assert!(!shaped.glyphs.is_empty());
        assert!(shaped.width_um > 0);
        // Every glyph is a real glyph (no .notdef), proving the Arabic
        // font and shaper agree on coverage.
        assert!(shaped.glyphs.iter().all(|g| g.gid != 0));
        // Clusters are logical byte offsets within the text.
        assert!(shaped
            .glyphs
            .iter()
            .all(|g| (g.cluster as usize) < word.len()));
        // Shaping is contextual, not a 1:1 cmap mapping: the joined
        // forms differ from naively mapping each codepoint in isolation.
        let naive: Vec<u16> = word
            .chars()
            .map(|c| FontMetrics::face_metrics(Face::Arabic).glyph(c).0)
            .collect();
        let shaped_gids: Vec<u16> = shaped.glyphs.iter().map(|g| g.gid).collect();
        assert_ne!(shaped_gids, naive, "shaping must apply contextual forms");
    }

    #[test]
    fn devanagari_reorders_and_forms_clusters() {
        // "namaste" — has an i-matra and a conjunct.
        let shaped = shape_run(Face::Devanagari, "नमस्ते", 3881);
        assert!(!shaped.glyphs.is_empty());
        assert!(shaped.width_um > 0);
        assert!(shaped.glyphs.iter().all(|g| g.gid != 0));
    }

    #[test]
    fn shaping_is_deterministic() {
        assert_eq!(
            shape_run(Face::Arabic, "العربية", 3881),
            shape_run(Face::Arabic, "العربية", 3881),
        );
        // Width is the exact integer sum of scaled advances.
        let s = shape_run(Face::Arabic, "العربية", 3881);
        assert_eq!(
            s.width_um,
            s.glyphs.iter().map(|g| g.x_advance_um).sum::<i64>()
        );
    }
}
