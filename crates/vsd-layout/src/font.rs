//! The pinned font (LAYOUT-1.0.md §2) and the integer arithmetic
//! primitive (§1.1).
//!
//! Noto Sans Regular v2.015 (hinted), SIL OFL 1.1, embedded at build
//! time. The font binary is part of the engine version: its `cmap` and
//! `hmtx` tables are the sole source of glyph mapping and metrics.

use std::sync::OnceLock;

// Re-exported for vsd-render and vsd-pdf, which must use the *same*
// pinned parser the metrics came from.
pub use ttf_parser::{GlyphId, OutlineBuilder};

/// The embedded regular face (SHA-256
/// `478c558ea716033cd60c03438f628dfa75694dcf6b5f6d505a2f05fd2b4f3823`).
pub static FONT_BYTES: &[u8] = include_bytes!("../assets/NotoSans-Regular.ttf");
static FONT_BOLD: &[u8] = include_bytes!("../assets/NotoSans-Bold.ttf");
static FONT_ITALIC: &[u8] = include_bytes!("../assets/NotoSans-Italic.ttf");
static FONT_BOLD_ITALIC: &[u8] = include_bytes!("../assets/NotoSans-BoldItalic.ttf");
static FONT_MONO: &[u8] = include_bytes!("../assets/NotoSansMono-Regular.ttf");
static FONT_HEBREW: &[u8] = include_bytes!("../assets/NotoSansHebrew-Regular.ttf");

/// A face of the pinned family. Display lists carry the index in
/// `TextRun::font`. Engine 1.0 only ever emits `Regular`; 1.1 adds
/// indices 1–3 (LAYOUT-1.1.md); 1.2 adds Mono and Hebrew
/// (LAYOUT-1.2.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Face {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    BoldItalic = 3,
    Mono = 4,
    Hebrew = 5,
}

impl Face {
    pub const ALL: [Face; 6] = [
        Face::Regular,
        Face::Bold,
        Face::Italic,
        Face::BoldItalic,
        Face::Mono,
        Face::Hebrew,
    ];

    pub fn index(self) -> u64 {
        self as u64
    }

    /// Display lists from future engines may carry unknown indices;
    /// consumers fall back to Regular rather than failing to draw.
    pub fn from_index(i: u64) -> Face {
        match i {
            1 => Face::Bold,
            2 => Face::Italic,
            3 => Face::BoldItalic,
            4 => Face::Mono,
            5 => Face::Hebrew,
            _ => Face::Regular,
        }
    }

    pub fn pick(bold: bool, italic: bool) -> Face {
        match (bold, italic) {
            (false, false) => Face::Regular,
            (true, false) => Face::Bold,
            (false, true) => Face::Italic,
            (true, true) => Face::BoldItalic,
        }
    }

    /// Engine 1.2 style resolution: `mono` overrides weight/slant (the
    /// pinned mono family ships one face in this engine version).
    pub fn pick_with_mono(bold: bool, italic: bool, mono: bool) -> Face {
        if mono {
            Face::Mono
        } else {
            Face::pick(bold, italic)
        }
    }

    /// Per-character script fallback (engine 1.2): Hebrew-block
    /// codepoints come from the Hebrew face regardless of styling
    /// (it ships one face in this engine version).
    pub fn for_char(self, c: char) -> Face {
        if matches!(c, '\u{0590}'..='\u{05FF}' | '\u{FB1D}'..='\u{FB4F}') {
            Face::Hebrew
        } else {
            self
        }
    }

    /// The face's TTF binary (for PDF embedding / rasterization).
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Face::Regular => FONT_BYTES,
            Face::Bold => FONT_BOLD,
            Face::Italic => FONT_ITALIC,
            Face::BoldItalic => FONT_BOLD_ITALIC,
            Face::Mono => FONT_MONO,
            Face::Hebrew => FONT_HEBREW,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Face::Regular => "NotoSans-Regular",
            Face::Bold => "NotoSans-Bold",
            Face::Italic => "NotoSans-Italic",
            Face::BoldItalic => "NotoSans-BoldItalic",
            Face::Mono => "NotoSansMono-Regular",
            Face::Hebrew => "NotoSansHebrew-Regular",
        }
    }
}

/// The one rounding primitive of the contract (§1.1):
/// `floor((a × b + floor(d/2)) / d)`, evaluated in 128-bit precision.
#[inline]
pub fn muldiv(a: i64, b: i64, d: i64) -> i64 {
    debug_assert!(d > 0);
    let n = a as i128 * b as i128 + (d as i128) / 2;
    n.div_euclid(d as i128) as i64
}

pub struct FontMetrics {
    face: ttf_parser::Face<'static>,
    pub upem: i64,
    pub ascent_units: i64,
    pub descent_units: i64,
    pub line_gap_units: i64,
}

static METRICS: OnceLock<[FontMetrics; 6]> = OnceLock::new();

impl FontMetrics {
    fn parse_face(bytes: &'static [u8]) -> FontMetrics {
        let face = ttf_parser::Face::parse(bytes, 0).expect("embedded font must parse");
        FontMetrics {
            upem: face.units_per_em() as i64,
            ascent_units: face.ascender() as i64,
            descent_units: face.descender() as i64,
            line_gap_units: face.line_gap() as i64,
            face,
        }
    }

    fn all() -> &'static [FontMetrics; 6] {
        METRICS.get_or_init(|| {
            [
                Self::parse_face(Face::Regular.bytes()),
                Self::parse_face(Face::Bold.bytes()),
                Self::parse_face(Face::Italic.bytes()),
                Self::parse_face(Face::BoldItalic.bytes()),
                Self::parse_face(Face::Mono.bytes()),
                Self::parse_face(Face::Hebrew.bytes()),
            ]
        })
    }

    /// The regular face — baseline metrics and engine-1.0 behavior.
    pub fn get() -> &'static FontMetrics {
        &Self::all()[0]
    }

    /// Metrics for a specific face (engine 1.1+).
    pub fn face_metrics(face: Face) -> &'static FontMetrics {
        &Self::all()[face as usize]
    }

    pub fn face(&self) -> &ttf_parser::Face<'static> {
        &self.face
    }

    /// Codepoint → glyph id; absent codepoints map to `.notdef` (0).
    pub fn glyph(&self, c: char) -> GlyphId {
        self.face.glyph_index(c).unwrap_or(GlyphId(0))
    }

    /// Advance width in font units for a glyph.
    pub fn advance_units(&self, gid: GlyphId) -> i64 {
        self.face.glyph_hor_advance(gid).unwrap_or(0) as i64
    }

    /// Advance in µm for one codepoint at `size_um`.
    pub fn char_advance_um(&self, c: char, size_um: i64) -> i64 {
        muldiv(self.advance_units(self.glyph(c)), size_um, self.upem)
    }

    pub fn space_advance_um(&self, size_um: i64) -> i64 {
        self.char_advance_um(' ', size_um)
    }

    pub fn ascent_um(&self, size_um: i64) -> i64 {
        muldiv(self.ascent_units, size_um, self.upem)
    }

    pub fn descent_um(&self, size_um: i64) -> i64 {
        muldiv(self.descent_units, size_um, self.upem)
    }

    /// Line height: `muldiv(size, 7, 5)` (1.4 ×), per the constants table.
    pub fn line_height_um(size_um: i64) -> i64 {
        muldiv(size_um, 7, 5)
    }

    /// Width of a string in µm: exact integer sum of scaled advances.
    /// C0 controls contribute nothing (§3.5).
    pub fn text_width_um(&self, text: &str, size_um: i64) -> i64 {
        text.chars()
            .filter(|c| !c.is_control())
            .map(|c| self.char_advance_um(c, size_um))
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_parses_and_has_expected_shape() {
        let m = FontMetrics::get();
        assert_eq!(m.upem, 1000, "Noto Sans upem");
        assert!(m.ascent_units > 0);
        assert!(m.descent_units < 0);
        // Latin coverage present.
        assert_ne!(m.glyph('A').0, 0);
        assert_ne!(m.glyph('ж').0, 0); // Cyrillic
        assert_ne!(m.glyph('λ').0, 0); // Greek
        assert!(m.char_advance_um('M', 3881) > m.char_advance_um('i', 3881));
    }

    #[test]
    fn muldiv_is_floor_of_half_up() {
        assert_eq!(muldiv(11, 25400, 72), 3881); // 11 pt → µm (contract table)
        assert_eq!(muldiv(24, 25400, 72), 8467);
        assert_eq!(muldiv(18, 25400, 72), 6350);
        assert_eq!(muldiv(14, 25400, 72), 4939);
        assert_eq!(muldiv(12, 25400, 72), 4233);
        assert_eq!(muldiv(10, 25400, 72), 3528);
        assert_eq!(muldiv(9, 25400, 72), 3175);
        assert_eq!(muldiv(-100, 3881, 1000), -388); // floor(-388.06) = -389? see below
    }

    #[test]
    fn muldiv_negative_floor_semantics() {
        // floor((-100×3881 + 500)/1000) = floor(-387.6) = -388
        assert_eq!(muldiv(-100, 3881, 1000), -388);
        // and a clean negative case: floor((-1000×1 + 500)/1000) = floor(-0.5) = -1
        assert_eq!(muldiv(-1000, 1, 1000), -1);
    }
}
