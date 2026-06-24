//! Dictionary-based line breaking for scripts with no inter-word spaces
//! (LAYOUT-1.6.md §3): Thai and Lao.
//!
//! These scripts write words without spaces, so line-break opportunities
//! must be discovered by segmenting the text against a word list. The
//! engine uses a deterministic **forward longest-match** segmenter over a
//! pinned dictionary: at each position it consumes the longest dictionary
//! word that starts there (an out-of-dictionary character is skipped
//! without creating a break), and the boundary after each matched word is
//! a permitted break point.
//!
//! The dictionaries are the ICU `brkitr` word lists, pinned by hash and
//! embedded by value, frozen with the engine version exactly like the
//! fonts and the mirroring table. The segmenter does only integer/string
//! work with hashed lookups, so break points — and therefore line breaks
//! — are byte-identical on every platform.

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::font::Face;

static THAI_TXT: &str = include_str!("../assets/thaidict.txt");
static LAO_TXT: &str = include_str!("../assets/laodict.txt");
static KHMER_TXT: &str = include_str!("../assets/khmerdict.txt");
static BURMESE_TXT: &str = include_str!("../assets/burmesedict.txt");

/// A pinned word list and the longest-match segmenter over it.
pub struct Dictionary {
    words: HashSet<&'static str>,
    /// Longest entry, in characters — bounds the match search.
    max_chars: usize,
}

impl Dictionary {
    /// The pinned Thai dictionary (ICU `thaidict.txt`, SHA-256
    /// `3166abde40c0f44ab91c28f5ce96d7d1472cb7882e1c0bda0a72f8f69dba4274`).
    pub fn thai() -> &'static Dictionary {
        static D: OnceLock<Dictionary> = OnceLock::new();
        D.get_or_init(|| Dictionary::parse(THAI_TXT))
    }

    /// The pinned Lao dictionary (ICU `laodict.txt`, SHA-256
    /// `3c876934a3fa81031d2333525eafaca6a7c9f842e3b98f18c38880420afb5d36`).
    pub fn lao() -> &'static Dictionary {
        static D: OnceLock<Dictionary> = OnceLock::new();
        D.get_or_init(|| Dictionary::parse(LAO_TXT))
    }

    /// The pinned Khmer dictionary (ICU `khmerdict.txt`, SHA-256
    /// `87bee2d17cd5148aa36957eb05409eefc124de8ad519b81b789298ef3e60b5d9`).
    pub fn khmer() -> &'static Dictionary {
        static D: OnceLock<Dictionary> = OnceLock::new();
        D.get_or_init(|| Dictionary::parse(KHMER_TXT))
    }

    /// The pinned Burmese (Myanmar) dictionary (ICU `burmesedict.txt`,
    /// SHA-256 `61d8abc3d9102b2f9bf0c9f44db0d7ab89b18172d8cd26832e4c83174bd8673b`).
    pub fn burmese() -> &'static Dictionary {
        static D: OnceLock<Dictionary> = OnceLock::new();
        D.get_or_init(|| Dictionary::parse(BURMESE_TXT))
    }

    /// The dictionary that segments a shaped face's script, if any.
    pub fn for_face(face: Face) -> Option<&'static Dictionary> {
        match face {
            Face::Thai => Some(Dictionary::thai()),
            Face::Lao => Some(Dictionary::lao()),
            Face::Khmer => Some(Dictionary::khmer()),
            Face::Myanmar => Some(Dictionary::burmese()),
            _ => None,
        }
    }

    fn parse(text: &'static str) -> Dictionary {
        let mut words = HashSet::new();
        let mut max_chars = 1;
        for line in text.lines() {
            let w = line.trim_matches(|c: char| c == '\u{feff}' || c.is_whitespace());
            if w.is_empty() || w.starts_with('#') {
                continue;
            }
            max_chars = max_chars.max(w.chars().count());
            words.insert(w);
        }
        Dictionary { words, max_chars }
    }

    /// Break opportunities within `run` as **relative byte offsets**,
    /// strictly inside `(0, run.len())`, ascending. Each is the boundary
    /// immediately after a matched dictionary word (so it is a whole-word,
    /// cluster-safe place to wrap). Out-of-dictionary characters advance
    /// one scalar at a time and introduce no break, so unknown spans are
    /// never split.
    pub fn segment(&self, run: &str) -> Vec<usize> {
        let chars: Vec<(usize, char)> = run.char_indices().collect();
        let n = chars.len();
        let mut breaks = Vec::new();
        let mut i = 0;
        while i < n {
            let start = chars[i].0;
            let limit = (i + self.max_chars).min(n);
            let mut matched_chars = 0;
            let mut matched_end = start;
            // Longest dictionary word starting at character i. `j` indexes
            // the *end* character, so the range loop is the clearest form.
            #[allow(clippy::needless_range_loop)]
            for j in (i + 1)..=limit {
                let end = if j < n { chars[j].0 } else { run.len() };
                if self.words.contains(&run[start..end]) {
                    matched_chars = j - i;
                    matched_end = end;
                }
            }
            if matched_chars > 0 {
                if matched_end < run.len() {
                    breaks.push(matched_end);
                }
                i += matched_chars;
            } else {
                i += 1;
            }
        }
        breaks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionaries_load_with_plausible_size() {
        // The pinned word lists parse to tens of thousands of entries.
        assert!(Dictionary::thai().words.len() > 20_000);
        assert!(Dictionary::lao().words.len() > 20_000);
        assert!(Dictionary::khmer().words.len() > 20_000);
        assert!(Dictionary::burmese().words.len() > 20_000);
        assert!(Dictionary::thai().max_chars >= 2);
    }

    #[test]
    fn khmer_and_burmese_segment_to_valid_boundaries() {
        // Boundaries (if any) must be in range, ascending, and on char
        // boundaries — for both spaceless scripts. (Functional wrapping
        // is covered by the integration test over real paragraphs.)
        for (d, phrase) in [
            (Dictionary::khmer(), "ភាសាខ្មែរជាភាសាមួយ"),
            (Dictionary::burmese(), "မြန်မာဘာသာစကား"),
        ] {
            let breaks = d.segment(phrase);
            assert!(breaks
                .iter()
                .all(|&o| o > 0 && o < phrase.len() && phrase.is_char_boundary(o)));
            assert!(breaks.windows(2).all(|w| w[0] < w[1]));
        }
    }

    #[test]
    fn segments_a_known_thai_phrase_at_word_boundaries() {
        // "สวัสดีครับ" = "สวัสดี" (hello) + "ครับ" (polite particle).
        let d = Dictionary::thai();
        let phrase = "สวัสดีครับ";
        let breaks = d.segment(phrase);
        // Exactly one internal break, at the boundary between the two
        // words; it is a valid char boundary inside the phrase.
        assert_eq!(breaks.len(), 1, "one word boundary expected");
        let b = breaks[0];
        assert!(b > 0 && b < phrase.len());
        assert!(phrase.is_char_boundary(b));
        assert_eq!(&phrase[..b], "สวัสดี");
        assert_eq!(&phrase[b..], "ครับ");
    }

    #[test]
    fn segmentation_is_deterministic_and_in_range() {
        let d = Dictionary::thai();
        let phrase = "ภาษาไทยสวยงาม";
        let a = d.segment(phrase);
        let b = d.segment(phrase);
        assert_eq!(a, b);
        assert!(a
            .iter()
            .all(|&o| o > 0 && o < phrase.len() && phrase.is_char_boundary(o)));
        assert!(a.windows(2).all(|w| w[0] < w[1]), "ascending, unique");
    }
}
