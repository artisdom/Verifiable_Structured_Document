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
static FONT_ARABIC: &[u8] = include_bytes!("../assets/NotoSansArabic-Regular.ttf");
static FONT_DEVANAGARI: &[u8] = include_bytes!("../assets/NotoSansDevanagari-Regular.ttf");
// Engine 1.5 — the remaining major Brahmic scripts (LAYOUT-1.5.md §2),
// all shaped by the same pinned `rustybuzz` as Devanagari.
static FONT_BENGALI: &[u8] = include_bytes!("../assets/NotoSansBengali-Regular.ttf");
static FONT_GURMUKHI: &[u8] = include_bytes!("../assets/NotoSansGurmukhi-Regular.ttf");
static FONT_GUJARATI: &[u8] = include_bytes!("../assets/NotoSansGujarati-Regular.ttf");
static FONT_ORIYA: &[u8] = include_bytes!("../assets/NotoSansOriya-Regular.ttf");
static FONT_TAMIL: &[u8] = include_bytes!("../assets/NotoSansTamil-Regular.ttf");
static FONT_TELUGU: &[u8] = include_bytes!("../assets/NotoSansTelugu-Regular.ttf");
static FONT_KANNADA: &[u8] = include_bytes!("../assets/NotoSansKannada-Regular.ttf");
static FONT_MALAYALAM: &[u8] = include_bytes!("../assets/NotoSansMalayalam-Regular.ttf");
static FONT_SINHALA: &[u8] = include_bytes!("../assets/NotoSansSinhala-Regular.ttf");
// Engine 1.6 — Thai and Lao (LAYOUT-1.6.md §2), shaped by the same
// pinned `rustybuzz`; their line breaking is dictionary-based (§3).
static FONT_THAI: &[u8] = include_bytes!("../assets/NotoSansThai-Regular.ttf");
static FONT_LAO: &[u8] = include_bytes!("../assets/NotoSansLao-Regular.ttf");
// Engine 1.7 — full Noto Sans CJK SC (LAYOUT-1.7.md §2). A CFF/OpenType
// font (CID-keyed, Adobe-Identity-0 ROS → CID == GID). Not shaped: Han,
// kana, and hangul render per glyph; line breaking is inter-ideograph.
static FONT_CJK: &[u8] = include_bytes!("../assets/NotoSansCJKsc-Regular.otf");
// Engine 1.9 — the remaining major complex scripts (LAYOUT-1.9.md §2),
// shaped by the same pinned `rustybuzz`. Khmer/Myanmar are spaceless
// (dictionary line breaking); Tibetan breaks at the tsheg; Ethiopic uses
// spaces.
static FONT_TIBETAN: &[u8] = include_bytes!("../assets/NotoSerifTibetan-Regular.ttf");
static FONT_KHMER: &[u8] = include_bytes!("../assets/NotoSansKhmer-Regular.ttf");
static FONT_MYANMAR: &[u8] = include_bytes!("../assets/NotoSansMyanmar-Regular.ttf");
static FONT_ETHIOPIC: &[u8] = include_bytes!("../assets/NotoSansEthiopic-Regular.ttf");
// Engine 1.11 — STIX Two Math (LAYOUT-1.11.md §2), a CFF/OpenType math
// font carrying a real OpenType `MATH` table. Used *only* by the MathML
// layout path (`crate::mathml`); it is never in any script-routing map,
// so it cannot change any frozen engine's output. SIL OFL 1.1.
static FONT_MATH: &[u8] = include_bytes!("../assets/STIXTwoMath-Regular.otf");

/// A face of the pinned family. Display lists carry the index in
/// `TextRun::font` / `GlyphRun::font`. Engine 1.0 only ever emits
/// `Regular`; 1.1 adds indices 1–3 (LAYOUT-1.1.md); 1.2 adds Mono and
/// Hebrew (LAYOUT-1.2.md); 1.4 adds the shaped scripts Arabic and
/// Devanagari (LAYOUT-1.4.md), emitted only via `GlyphRun`; 1.5 adds the
/// remaining major Brahmic scripts (LAYOUT-1.5.md), also `GlyphRun`-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Face {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    BoldItalic = 3,
    Mono = 4,
    Hebrew = 5,
    Arabic = 6,
    Devanagari = 7,
    Bengali = 8,
    Gurmukhi = 9,
    Gujarati = 10,
    Oriya = 11,
    Tamil = 12,
    Telugu = 13,
    Kannada = 14,
    Malayalam = 15,
    Sinhala = 16,
    Thai = 17,
    Lao = 18,
    Cjk = 19,
    Tibetan = 20,
    Khmer = 21,
    Myanmar = 22,
    Ethiopic = 23,
    /// STIX Two Math — used only by the MathML layout path (engine 1.11);
    /// never routed by `for_char`/`shaped_for`, so it is freeze-neutral.
    Math = 24,
}

impl Face {
    pub const ALL: [Face; 25] = [
        Face::Regular,
        Face::Bold,
        Face::Italic,
        Face::BoldItalic,
        Face::Mono,
        Face::Hebrew,
        Face::Arabic,
        Face::Devanagari,
        Face::Bengali,
        Face::Gurmukhi,
        Face::Gujarati,
        Face::Oriya,
        Face::Tamil,
        Face::Telugu,
        Face::Kannada,
        Face::Malayalam,
        Face::Sinhala,
        Face::Thai,
        Face::Lao,
        Face::Cjk,
        Face::Tibetan,
        Face::Khmer,
        Face::Myanmar,
        Face::Ethiopic,
        Face::Math,
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
            6 => Face::Arabic,
            7 => Face::Devanagari,
            8 => Face::Bengali,
            9 => Face::Gurmukhi,
            10 => Face::Gujarati,
            11 => Face::Oriya,
            12 => Face::Tamil,
            13 => Face::Telugu,
            14 => Face::Kannada,
            15 => Face::Malayalam,
            16 => Face::Sinhala,
            17 => Face::Thai,
            18 => Face::Lao,
            19 => Face::Cjk,
            20 => Face::Tibetan,
            21 => Face::Khmer,
            22 => Face::Myanmar,
            23 => Face::Ethiopic,
            24 => Face::Math,
            _ => Face::Regular,
        }
    }

    /// The shaped-script face a codepoint requires, or `None` for
    /// scripts handled by the simple per-glyph path. Used to route runs
    /// to the shaper and to pick the embedded font. Engine 1.4 shapes
    /// only Arabic + Devanagari; engine 1.5 adds the remaining Brahmic
    /// scripts — *which* of these an engine version actually shapes (vs.
    /// refuses) is gated per version by `EngineVersion::shaped_face`, so
    /// this map can grow without changing a frozen engine's behavior.
    pub fn shaped_for(c: char) -> Option<Face> {
        match c {
            // Arabic, Arabic Supplement, Extended-A, presentation forms.
            '\u{0600}'..='\u{06FF}'
            | '\u{0750}'..='\u{077F}'
            | '\u{08A0}'..='\u{08FF}'
            | '\u{FB50}'..='\u{FDFF}'
            | '\u{FE70}'..='\u{FEFF}' => Some(Face::Arabic),
            // Devanagari (+ extended).
            '\u{0900}'..='\u{097F}' | '\u{A8E0}'..='\u{A8FF}' => Some(Face::Devanagari),
            // The other major Brahmic blocks (engine 1.5).
            '\u{0980}'..='\u{09FF}' => Some(Face::Bengali),
            '\u{0A00}'..='\u{0A7F}' => Some(Face::Gurmukhi),
            '\u{0A80}'..='\u{0AFF}' => Some(Face::Gujarati),
            '\u{0B00}'..='\u{0B7F}' => Some(Face::Oriya),
            '\u{0B80}'..='\u{0BFF}' => Some(Face::Tamil),
            '\u{0C00}'..='\u{0C7F}' => Some(Face::Telugu),
            '\u{0C80}'..='\u{0CFF}' => Some(Face::Kannada),
            '\u{0D00}'..='\u{0D7F}' => Some(Face::Malayalam),
            '\u{0D80}'..='\u{0DFF}' => Some(Face::Sinhala),
            // Thai and Lao (engine 1.6) — shaped here, but their *line
            // breaking* is dictionary-based (no inter-word spaces).
            '\u{0E00}'..='\u{0E7F}' => Some(Face::Thai),
            '\u{0E80}'..='\u{0EFF}' => Some(Face::Lao),
            _ => None,
        }
    }

    /// Whether this face is shaped via the pinned shaper (engine 1.4+).
    pub fn is_shaped(self) -> bool {
        matches!(
            self,
            Face::Arabic
                | Face::Devanagari
                | Face::Bengali
                | Face::Gurmukhi
                | Face::Gujarati
                | Face::Oriya
                | Face::Tamil
                | Face::Telugu
                | Face::Kannada
                | Face::Malayalam
                | Face::Sinhala
                | Face::Thai
                | Face::Lao
                | Face::Tibetan
                | Face::Khmer
                | Face::Myanmar
                | Face::Ethiopic
        )
    }

    /// The face for a complex script added in engine 1.9 (Tibetan, Khmer,
    /// Myanmar, Ethiopic), or `None`. Kept **separate** from
    /// [`Face::shaped_for`] (and routed only under the engine-1.9 style
    /// policy) because these blocks were never in the historical refusal
    /// set: routing them unconditionally would change a frozen engine's
    /// output for a document that contains them.
    pub fn extended_for(c: char) -> Option<Face> {
        match c {
            '\u{0F00}'..='\u{0FFF}' => Some(Face::Tibetan),
            '\u{1000}'..='\u{109F}' | '\u{A9E0}'..='\u{A9FF}' | '\u{AA60}'..='\u{AA7F}' => {
                Some(Face::Myanmar)
            }
            '\u{1200}'..='\u{139F}' | '\u{2D80}'..='\u{2DDF}' | '\u{AB00}'..='\u{AB2F}' => {
                Some(Face::Ethiopic)
            }
            '\u{1780}'..='\u{17FF}' | '\u{19E0}'..='\u{19FF}' => Some(Face::Khmer),
            _ => None,
        }
    }

    /// CJK punctuation and fullwidth forms (U+3000–303F, U+FF00–FFEF) —
    /// set in the pan-CJK face under the engine-1.9 style policy. These
    /// ranges sit just outside the historical CJK refusal set (so they
    /// are gated, like [`Face::extended_for`], not routed by default).
    pub fn is_cjk_punct(c: char) -> bool {
        matches!(c, '\u{3000}'..='\u{303F}' | '\u{FF00}'..='\u{FFEF}')
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

    /// Per-character script fallback: Hebrew-block codepoints come from
    /// the Hebrew face (engine 1.2), and shaped-script codepoints from
    /// their face (Arabic / Devanagari, engine 1.4), regardless of
    /// styling — each ships one face in this engine version. Latin and
    /// the other simple scripts keep the styled face `self`.
    pub fn for_char(self, c: char) -> Face {
        if let Some(shaped) = Face::shaped_for(c) {
            shaped
        } else if matches!(c, '\u{0590}'..='\u{05FF}' | '\u{FB1D}'..='\u{FB4F}') {
            Face::Hebrew
        } else if is_cjk(c) {
            // CJK (engine 1.7): one pinned pan-CJK face; not shaped.
            Face::Cjk
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
            Face::Arabic => FONT_ARABIC,
            Face::Devanagari => FONT_DEVANAGARI,
            Face::Bengali => FONT_BENGALI,
            Face::Gurmukhi => FONT_GURMUKHI,
            Face::Gujarati => FONT_GUJARATI,
            Face::Oriya => FONT_ORIYA,
            Face::Tamil => FONT_TAMIL,
            Face::Telugu => FONT_TELUGU,
            Face::Kannada => FONT_KANNADA,
            Face::Malayalam => FONT_MALAYALAM,
            Face::Sinhala => FONT_SINHALA,
            Face::Thai => FONT_THAI,
            Face::Lao => FONT_LAO,
            Face::Cjk => FONT_CJK,
            Face::Tibetan => FONT_TIBETAN,
            Face::Khmer => FONT_KHMER,
            Face::Myanmar => FONT_MYANMAR,
            Face::Ethiopic => FONT_ETHIOPIC,
            Face::Math => FONT_MATH,
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
            Face::Arabic => "NotoSansArabic-Regular",
            Face::Devanagari => "NotoSansDevanagari-Regular",
            Face::Bengali => "NotoSansBengali-Regular",
            Face::Gurmukhi => "NotoSansGurmukhi-Regular",
            Face::Gujarati => "NotoSansGujarati-Regular",
            Face::Oriya => "NotoSansOriya-Regular",
            Face::Tamil => "NotoSansTamil-Regular",
            Face::Telugu => "NotoSansTelugu-Regular",
            Face::Kannada => "NotoSansKannada-Regular",
            Face::Malayalam => "NotoSansMalayalam-Regular",
            Face::Sinhala => "NotoSansSinhala-Regular",
            Face::Thai => "NotoSansThai-Regular",
            Face::Lao => "NotoSansLao-Regular",
            Face::Cjk => "NotoSansCJKsc-Regular",
            Face::Tibetan => "NotoSerifTibetan-Regular",
            Face::Khmer => "NotoSansKhmer-Regular",
            Face::Myanmar => "NotoSansMyanmar-Regular",
            Face::Ethiopic => "NotoSansEthiopic-Regular",
            Face::Math => "STIXTwoMath-Regular",
        }
    }
}

/// Whether `c` is a CJK character the pinned pan-CJK face sets and the
/// inter-ideograph line breaker treats as breakable (engine 1.7): Han
/// (incl. Ext-A and compatibility), Hiragana, Katakana, and Hangul.
///
/// These are **exactly** the ranges every pre-1.7 engine already refuses
/// (`refused_script`), which is the freeze-safety invariant: routing a
/// codepoint to the CJK face here only affects engine versions that
/// would have refused it anyway, so no frozen engine's output changes.
/// (CJK symbols/punctuation U+3000–303F and the fullwidth forms are
/// outside the historical refusal set, so they are left on the Regular
/// path — a documented 1.7 gap, addressable only by a future engine.)
pub fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{1100}'..='\u{11FF}'   // Hangul Jamo
        | '\u{3040}'..='\u{30FF}' // Hiragana + Katakana
        | '\u{3400}'..='\u{4DBF}' // CJK Unified Ideographs Extension A
        | '\u{4E00}'..='\u{9FFF}' // CJK Unified Ideographs
        | '\u{AC00}'..='\u{D7AF}' // Hangul Syllables
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
    )
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

static METRICS: OnceLock<[FontMetrics; 25]> = OnceLock::new();

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

    fn all() -> &'static [FontMetrics; 25] {
        METRICS.get_or_init(|| Face::ALL.map(|f| Self::parse_face(f.bytes())))
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
