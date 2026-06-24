# vsd-layout/1.9 — Layout Engine Contract (delta over 1.8)

**Status:** Normative for engine id `vsd-layout` version `1.9.0`.
This document specifies only the differences from
[LAYOUT-1.8.md](LAYOUT-1.8.md) (and transitively 1.7 … 1.0); everything
not mentioned here is **identical to 1.8**.

1.0–1.8 caches remain verifiable forever. 1.9 introduces **no format
change** (it reuses the format-0.4 `glyphs` and `text` ops), so every
prior `.vsd` vector is byte-identical and the 1.0–1.8 golden layout
hashes are **unchanged**.

Engine 1.9 closes the remaining "complex scripts" gap: **Tibetan, Khmer,
Myanmar, Ethiopic**, plus routing of **CJK punctuation / fullwidth
forms** to the pan-CJK face.

## 1. New pinned dependencies

| Kind | Item | SHA-256 |
|---|---|---|
| Font (index 20) | NotoSerifTibetan-Regular | `ee97bf3dc56e813651db734c9f35f8f1d41e7e31acf5f7d893e64ad22b292446` |
| Font (index 21) | NotoSansKhmer-Regular    | `e66675f2082788f0511a714bef5a1748928294b38c8e286a96ea73a864b5e605` |
| Font (index 22) | NotoSansMyanmar-Regular  | `fafce4db400bc0b214907ccdbfb0ad2f18a57bfefd08c8a571830b84088cf2fc` |
| Font (index 23) | NotoSansEthiopic-Regular | `f6f7fc379db9438959a2b0527e7a2cf36ea9c84626d56ec444fff37fc24c3c10` |
| Dictionary | ICU `khmerdict.txt`   | `87bee2d17cd5148aa36957eb05409eefc124de8ad519b81b789298ef3e60b5d9` |
| Dictionary | ICU `burmesedict.txt` | `61d8abc3d9102b2f9bf0c9f44db0d7ab89b18172d8cd26832e4c83174bd8673b` |

(Tibetan ships only as Noto **Serif** Tibetan; the other three are Noto
Sans.) All four shape via the **same** pinned `rustybuzz`; Khmer and
Myanmar reuse the engine-1.6 dictionary segmenter over their pinned ICU
word lists. No new shaper or tool.

## 2. Scripts and routing — the freeze gate

Unlike every prior complex script, these blocks were **never in the
historical refusal set** (`refused_script`): pre-1.9 engines render them
in the Regular face (`.notdef`), not as an error. Adding them to the
shared `Face::for_char` map would therefore change a frozen engine's
output for any document that contains them.

So 1.9 routes them through a **version-gated style policy**
(`extended_scripts`, set only for engine 1.9), kept separate from
`Face::shaped_for`/`for_char`:

- `Face::extended_for` maps Tibetan (U+0F00–0FFF) → face 20, Myanmar
  (U+1000–109F, U+A9E0–A9FF, U+AA60–AA7F) → 22, Ethiopic (U+1200–139F,
  U+2D80–2DDF, U+AB00–AB2F) → 23, Khmer (U+1780–17FF, U+19E0–19FF) → 21.
- `Face::is_cjk_punct` (U+3000–303F, U+FF00–FFEF) routes to the pan-CJK
  face (19).

Under the 1.9 policy these route to their faces and are shaped (or, for
CJK punctuation, set per glyph in the pan-CJK face); under every earlier
policy the flag is off and they stay on the Regular path **exactly as
before**. Frozen engines are byte-identical — proven by the corpus.

## 3. Line breaking

- **Tibetan** has no spaces; a break is permitted after an intersyllabic
  **tsheg** (U+0F0B) or a **shad** (U+0F0D). The non-breaking delimiter
  tsheg (U+0F0C) yields no break. These zero-width opportunities feed the
  same greedy breaker as everything else.
- **Khmer** and **Myanmar** are spaceless: break opportunities come from
  the engine-1.6 forward longest-match dictionary segmenter over the
  pinned ICU Khmer / Burmese word lists (`Dictionary::for_face`).
- **Ethiopic** is word-spaced and breaks at spaces, like Latin.

Each shaped run is measured by the same shaper used at emission, so a
wrapped prefix measures exactly as the run actually emitted.

## 4. Determinism

Unchanged in kind: integer micrometers throughout; fonts, shaper, and
dictionaries pinned by hash; the tsheg rule and the dictionary segmenter
are pure functions of the text. The conformance corpus carries a golden
hash for 1.9 (`valid/laid-out-1.9.vsd`: Tibetan, Khmer, Myanmar,
Ethiopic, and a CJK-punctuation paragraph) alongside the unchanged
1.0–1.8 hashes; a conforming implementation MUST reproduce all ten.

## 5. Still deliberately absent (future versions)

Scripts genuinely beyond this set — Mongolian (which is itself vertical),
N'Ko and Adlam (RTL joining), Syriac, and historic / rarely-used scripts
— remain refused or unrouted until a version pins their fonts and
breaking rules. Also still open: vertical tables / tate-chu-yoko, PDF
font subsetting, multi-column layout, and MathML layout. The engine
refuses or leaves on the Regular path what it cannot set faithfully,
rather than mis-rendering it.
