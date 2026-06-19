//! Knuth–Liang hyphenation (LAYOUT-1.3.md §2).
//!
//! Pinned en-US TeX patterns, embedded at build time and part of the
//! engine version. The algorithm is pure integer/string work — no
//! float, no locale, no allocation that depends on iteration order —
//! so the set of break points for a given word is identical on every
//! platform, exactly like the rest of the layout contract.
//!
//! Only ASCII-Latin words are hyphenated; anything else returns no
//! break points, so the engine never invents a wrong break.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The pinned pattern set: TeX `hyph-en-us` (Liang's original en-US
/// patterns), SHA-256
/// `0f57318b878b132547ae92db39a6e1d1cf2a05d9008874955d6ecb910007a463`.
/// Freely redistributable; part of engine version 1.3's identity.
static PATTERNS_SRC: &str = include_str!("../assets/hyph-en-us.pat.txt");

/// TeX defaults: at least two letters before, three after, a break.
const LEFT_MIN: usize = 2;
const RIGHT_MIN: usize = 3;

/// A compiled pattern table. Lookups only (never iterated), so the
/// `HashMap` introduces no order dependence.
pub struct Hyphenator {
    /// Dotted pattern letters → inter-letter values (length = letters+1).
    map: HashMap<String, Vec<u8>>,
    /// Longest pattern (in chars), to bound substring scanning.
    max_len: usize,
}

static EN_US: OnceLock<Hyphenator> = OnceLock::new();

impl Hyphenator {
    /// The pinned en-US hyphenator (parsed once).
    pub fn en_us() -> &'static Hyphenator {
        EN_US.get_or_init(|| Hyphenator::parse(PATTERNS_SRC))
    }

    fn parse(src: &str) -> Hyphenator {
        let mut map = HashMap::new();
        let mut max_len = 0usize;
        for raw in src.lines() {
            let line = raw.trim();
            // Skip blanks and any comment/control lines: a pattern is
            // letters, dots, and ASCII digits only.
            if line.is_empty()
                || !line
                    .chars()
                    .all(|c| c == '.' || c.is_ascii_digit() || c.is_alphabetic())
            {
                continue;
            }
            // Split into the bare letter string and the interleaved
            // values: `values[k]` is the value at the point *before*
            // letter `k` (and `values[len]` is after the last letter).
            let mut letters = String::new();
            let mut values = vec![0u8];
            for c in line.chars() {
                if let Some(d) = c.to_digit(10) {
                    *values.last_mut().unwrap() = d as u8;
                } else {
                    letters.push(c);
                    values.push(0);
                }
            }
            if letters.is_empty() {
                continue;
            }
            max_len = max_len.max(letters.chars().count());
            map.insert(letters, values);
        }
        Hyphenator { map, max_len }
    }

    /// Byte offsets within `word` at which a hyphen may be inserted
    /// (each offset is the boundary *after* the prefix that stays on the
    /// line). Empty unless `word` is an ASCII-Latin token long enough to
    /// satisfy the left/right minimums.
    pub fn breaks(&self, word: &str) -> Vec<usize> {
        if word.len() < LEFT_MIN + RIGHT_MIN || !word.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Vec::new();
        }
        let lower = word.to_ascii_lowercase();
        // Dotted word: '.' + letters + '.'.
        let dotted: Vec<char> = core::iter::once('.')
            .chain(lower.chars())
            .chain(core::iter::once('.'))
            .collect();
        let dlen = dotted.len();
        // Level at each inter-character point of the dotted word.
        let mut level = vec![0u8; dlen + 1];
        for start in 0..dlen {
            let mut frag = String::new();
            let max = self.max_len.min(dlen - start);
            for step in 0..max {
                frag.push(dotted[start + step]);
                if let Some(values) = self.map.get(&frag) {
                    for (k, &v) in values.iter().enumerate() {
                        let p = start + k;
                        if v > level[p] {
                            level[p] = v;
                        }
                    }
                }
            }
        }
        // Map dotted points back to word break offsets.
        //   dotted[0]='.'  dotted[1]=word[0] ... dotted[w]=word[w-1]  dotted[w+1]='.'
        // A break after word char `a` (0-based) is the point before
        // dotted[a+2]; allowed when level[a+2] is odd. Left letters =
        // a+1, right letters = w-1-a.
        let wchars: Vec<usize> = word
            .char_indices()
            .map(|(i, _)| i)
            .chain(core::iter::once(word.len()))
            .collect();
        let w = lower.chars().count();
        let mut out = Vec::new();
        for a in 0..w {
            let left = a + 1;
            let right = w - 1 - a;
            if left < LEFT_MIN || right < RIGHT_MIN {
                continue;
            }
            if level[a + 2] % 2 == 1 {
                out.push(wchars[a + 1]);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_words_break_where_tex_does() {
        let h = Hyphenator::en_us();
        // The canonical Liang example: hy-phen-ation.
        let mark = |word: &str| {
            let bp = h.breaks(word);
            let mut s = String::new();
            let mut last = 0;
            for &b in &bp {
                s.push_str(&word[last..b]);
                s.push('-');
                last = b;
            }
            s.push_str(&word[last..]);
            s
        };
        assert_eq!(mark("hyphenation"), "hy-phen-ation");
        // righthyphenmin=3 suppresses the "put-er" break (only 2 right).
        assert_eq!(mark("computer"), "com-puter");
        assert_eq!(mark("algorithm"), "al-go-rithm");
        assert_eq!(mark("determine"), "de-ter-mine");
    }

    #[test]
    fn respects_left_and_right_minimums() {
        let h = Hyphenator::en_us();
        // No break in the first 2 or last 3 letters.
        for &b in &h.breaks("hyphenation") {
            assert!(b >= LEFT_MIN, "break too early at {b}");
            assert!(
                "hyphenation".len() - b >= RIGHT_MIN,
                "break too late at {b}"
            );
        }
        // Short words never hyphenate.
        assert!(h.breaks("the").is_empty());
        assert!(h.breaks("cat").is_empty());
    }

    #[test]
    fn non_latin_and_mixed_are_left_whole() {
        let h = Hyphenator::en_us();
        assert!(h.breaks("שלום").is_empty());
        assert!(h.breaks("on-line").is_empty()); // contains a hyphen already
        assert!(h.breaks("v1sion").is_empty()); // contains a digit
    }

    #[test]
    fn deterministic_across_calls() {
        let h = Hyphenator::en_us();
        assert_eq!(h.breaks("hyphenation"), h.breaks("hyphenation"));
    }
}
