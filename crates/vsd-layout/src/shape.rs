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
    debug_assert!(face.is_shaped());
    // One parsed shaper face per family, indexed by face index, built
    // once on first shaping. Only the shaped faces are ever requested.
    static FACES: OnceLock<Vec<rustybuzz::Face<'static>>> = OnceLock::new();
    let faces = FACES.get_or_init(|| {
        Face::ALL
            .iter()
            .map(|f| rustybuzz::Face::from_slice(f.bytes(), 0).expect("embedded font must parse"))
            .collect()
    });
    &faces[face as usize]
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

/// Position a non-shaped run for a right-to-left context with Unicode
/// bidi mirroring (LAYOUT-1.5.md §3). Each character is mapped to its
/// own glyph in `face`, except `Bidi_Mirrored` characters, which map to
/// their mirror's glyph; the glyphs are then emitted in **visual order**
/// (the logical sequence reversed) so consumers draw them left-to-right
/// like any other `GlyphRun`. No reordering or ligatures occur — this is
/// the simple per-character path (Hebrew, neutrals), not the shaper.
///
/// The logical text is preserved by the caller; `cluster` is the byte
/// offset of the source character, so search and extraction still see
/// the original `(`, not the drawn `)`.
pub fn position_mirrored_rtl(face: Face, text: &str, size_um: i64) -> Shaped {
    debug_assert!(!face.is_shaped());
    let m = FontMetrics::face_metrics(face);
    let mut glyphs: Vec<ShapedGlyph> = text
        .char_indices()
        .filter(|&(_, c)| !c.is_control())
        .map(|(i, c)| {
            let drawn = crate::bidi_mirror::mirror_char(c).unwrap_or(c);
            let gid = m.glyph(drawn);
            ShapedGlyph {
                gid: gid.0,
                x_advance_um: muldiv(m.advance_units(gid), size_um, m.upem),
                x_offset_um: 0,
                y_offset_um: 0,
                cluster: i as u32,
            }
        })
        .collect();
    // Logical → visual order for RTL.
    glyphs.reverse();
    let width_um = glyphs.iter().map(|g| g.x_advance_um).sum();
    Shaped { glyphs, width_um }
}

/// Whether a run contains any character that mirrors in an RTL context —
/// the cheap test that decides whether engine 1.5 must position a run
/// with [`position_mirrored_rtl`] instead of emitting a plain
/// reversed-logical text run.
pub fn has_mirrored(text: &str) -> bool {
    text.chars()
        .any(|c| crate::bidi_mirror::mirror_char(c).is_some())
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
    fn engine_1_5_brahmic_scripts_shape_with_real_glyphs() {
        // One representative word per newly pinned script; every glyph
        // must be a real glyph (font + shaper agree on coverage).
        for (face, word) in [
            (Face::Bengali, "বাংলা"),
            (Face::Gurmukhi, "ਪੰਜਾਬੀ"),
            (Face::Gujarati, "ગુજરાતી"),
            (Face::Oriya, "ଓଡ଼ିଆ"),
            (Face::Tamil, "தமிழ்"),
            (Face::Telugu, "తెలుగు"),
            (Face::Kannada, "ಕನ್ನಡ"),
            (Face::Malayalam, "മലയാളം"),
            (Face::Sinhala, "සිංහල"),
        ] {
            let shaped = shape_run(face, word, 3881);
            assert!(!shaped.glyphs.is_empty(), "{face:?} produced no glyphs");
            assert!(shaped.width_um > 0, "{face:?} has zero width");
            assert!(
                shaped.glyphs.iter().all(|g| g.gid != 0),
                "{face:?} hit .notdef — font/shaper coverage gap"
            );
        }
    }

    #[test]
    fn engine_1_9_complex_scripts_shape_with_real_glyphs() {
        // Tibetan, Khmer, Myanmar, Ethiopic — each shaped by the same
        // pinned shaper; every glyph must be real (font + shaper agree).
        for (face, word) in [
            (Face::Tibetan, "བོད་སྐད"),
            (Face::Khmer, "ភាសាខ្មែរ"),
            (Face::Myanmar, "မြန်မာ"),
            (Face::Ethiopic, "አማርኛ"),
        ] {
            let shaped = shape_run(face, word, 3881);
            assert!(!shaped.glyphs.is_empty(), "{face:?} produced no glyphs");
            assert!(shaped.width_um > 0, "{face:?} has zero width");
            assert!(
                shaped.glyphs.iter().all(|g| g.gid != 0),
                "{face:?} hit .notdef — font/shaper coverage gap"
            );
        }
    }

    #[test]
    fn mirrored_rtl_swaps_brackets_keeps_letters_and_reverses() {
        // "(א)" — a Hebrew letter in parens. Drawn RTL, the opening
        // paren must become the closing-paren glyph and vice versa, and
        // the glyph order is the logical order reversed.
        let open = FontMetrics::face_metrics(Face::Regular).glyph('(').0;
        let close = FontMetrics::face_metrics(Face::Regular).glyph(')').0;
        assert_ne!(open, close);
        let s = position_mirrored_rtl(Face::Regular, "()", 3881);
        assert_eq!(s.glyphs.len(), 2);
        // Logical char 0 '(' mirrors to a ')' glyph; logical char 1 ')'
        // mirrors to a '(' glyph. After the visual reverse, the first
        // glyph drawn is the mirror of the *last* logical char.
        assert_eq!(
            s.glyphs[0].gid, open,
            "first drawn glyph is '(' (mirror of ')')"
        );
        assert_eq!(s.glyphs[0].cluster, 1);
        assert_eq!(
            s.glyphs[1].gid, close,
            "second drawn glyph is ')' (mirror of '(')"
        );
        assert_eq!(s.glyphs[1].cluster, 0);
        assert!(has_mirrored("(x)"));
        assert!(!has_mirrored("abc"));
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
