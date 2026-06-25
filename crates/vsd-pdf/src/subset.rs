//! Deterministic, glyph-id-stable font subsetting for PDF embedding.
//!
//! The PDF exporter embeds the *used* glyphs of each pinned face. Rather
//! than rewrite the cmap/hmtx and renumber glyphs (which would also force
//! a `CIDToGIDMap` and break the CJK font's CID == GID identity), this
//! subsetter keeps **every glyph id stable** and merely empties the
//! outline data of unused glyphs — which is the bulk of a font's size
//! (the 16 MB pan-CJK face especially). It handles both TrueType (`glyf`)
//! and CFF/OpenType faces.
//!
//! **Safety net.** Subsetting in the trusted, must-stay-deterministic PDF
//! path is risky, and PDF-glyph correctness can't be checked here. So
//! every subset is *self-verified*: it is re-parsed with the same pinned
//! `ttf-parser`, and every used glyph's outline and advance are compared
//! to the original. If subsetting is unsupported or verification fails,
//! [`subset_face`] returns the original font bytes unchanged. A bug can
//! therefore only ever yield a *larger* PDF, never a broken one — and the
//! decision is a pure function of (font, used set), so it is
//! deterministic across platforms.

use std::collections::BTreeSet;

use vsd_layout::font::{GlyphId, OutlineBuilder};

/// Subset `font_bytes` to `used` (glyph 0 is always kept), preserving
/// glyph ids. Returns the original bytes unchanged if the font shape is
/// unsupported or the subset fails self-verification.
pub fn subset_face(font_bytes: &[u8], used: &BTreeSet<u16>) -> Vec<u8> {
    let mut keep = used.clone();
    keep.insert(0);
    if let Some(sub) = try_subset(font_bytes, &keep) {
        if verify(&sub, font_bytes, &keep) {
            return sub;
        }
    }
    font_bytes.to_vec()
}

fn try_subset(font_bytes: &[u8], keep: &BTreeSet<u16>) -> Option<Vec<u8>> {
    let dir = TableDirectory::parse(font_bytes)?;
    if dir.find(b"glyf").is_some() && dir.find(b"loca").is_some() {
        subset_glyf(font_bytes, &dir, keep)
    } else if dir.find(b"CFF ").is_some() {
        subset_cff(font_bytes, &dir, keep)
    } else {
        None
    }
}

/// Re-parse `sub` and confirm every kept glyph outlines identically to
/// the original and has the same advance.
fn verify(sub: &[u8], orig: &[u8], keep: &BTreeSet<u16>) -> bool {
    let (Ok(fa), Ok(fb)) = (
        ttf_parser::Face::parse(sub, 0),
        ttf_parser::Face::parse(orig, 0),
    ) else {
        return false;
    };
    if fa.number_of_glyphs() != fb.number_of_glyphs() {
        return false;
    }
    for &gid in keep {
        let g = GlyphId(gid);
        if fa.glyph_hor_advance(g) != fb.glyph_hor_advance(g) {
            return false;
        }
        let mut pa = PathSink::default();
        let mut pb = PathSink::default();
        let ra = fa.outline_glyph(g, &mut pa);
        let rb = fb.outline_glyph(g, &mut pb);
        if ra.is_some() != rb.is_some() || pa.0 != pb.0 {
            return false;
        }
    }
    true
}

#[derive(Default)]
struct PathSink(String);

impl OutlineBuilder for PathSink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push_str(&format!("M{x} {y} "));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push_str(&format!("L{x} {y} "));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.push_str(&format!("Q{x1} {y1} {x} {y} "));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.push_str(&format!("C{x1} {y1} {x2} {y2} {x} {y} "));
    }
    fn close(&mut self) {
        self.0.push('Z');
    }
}

// --- sfnt table directory ----------------------------------------------------

struct TableRecord {
    tag: [u8; 4],
    offset: usize,
    length: usize,
}

struct TableDirectory {
    sfnt: u32,
    records: Vec<TableRecord>,
}

fn be16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

impl TableDirectory {
    fn parse(d: &[u8]) -> Option<TableDirectory> {
        let sfnt = be32(d, 0)?;
        let num = be16(d, 4)? as usize;
        let mut records = Vec::with_capacity(num);
        for i in 0..num {
            let b = 12 + i * 16;
            let tag: [u8; 4] = d.get(b..b + 4)?.try_into().ok()?;
            records.push(TableRecord {
                tag,
                offset: be32(d, b + 8)? as usize,
                length: be32(d, b + 12)? as usize,
            });
        }
        Some(TableDirectory { sfnt, records })
    }

    fn find(&self, tag: &[u8; 4]) -> Option<&TableRecord> {
        self.records.iter().find(|r| &r.tag == tag)
    }

    fn table<'a>(&self, d: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
        let r = self.find(tag)?;
        d.get(r.offset..r.offset + r.length)
    }
}

/// Reassemble an sfnt from `(tag, data)` tables: directory sorted by tag,
/// 4-byte-aligned table data, per-table checksums, and the `head`
/// checkSumAdjustment fixed up. Deterministic.
fn assemble(sfnt: u32, mut tables: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    tables.sort_by_key(|t| t.0);
    let n = tables.len();
    let mut out = Vec::new();
    out.extend_from_slice(&sfnt.to_be_bytes());
    out.extend_from_slice(&(n as u16).to_be_bytes());
    // searchRange / entrySelector / rangeShift
    let mut max_pow2 = 1usize;
    let mut sel = 0u16;
    while max_pow2 * 2 <= n.max(1) {
        max_pow2 *= 2;
        sel += 1;
    }
    let search_range = (max_pow2 * 16) as u16;
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&sel.to_be_bytes());
    out.extend_from_slice(&(((n * 16) as u16).wrapping_sub(search_range)).to_be_bytes());

    let dir_size = 12 + n * 16;
    // Lay out table data after the directory, 4-byte aligned.
    let mut offsets = Vec::with_capacity(n);
    let mut cur = dir_size;
    for (_, data) in &tables {
        offsets.push(cur);
        cur += (data.len() + 3) & !3;
    }
    // Directory records.
    let mut head_record_pos = None;
    for (i, (tag, data)) in tables.iter().enumerate() {
        if tag == b"head" {
            head_record_pos = Some(out.len());
        }
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(data).to_be_bytes());
        out.extend_from_slice(&(offsets[i] as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    }
    // Table data, aligned.
    for (i, (_, data)) in tables.iter().enumerate() {
        debug_assert_eq!(out.len(), offsets[i]);
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    // head.checkSumAdjustment = 0xB1B0AFBA - checksum(whole file), with
    // the field itself treated as zero during the whole-file checksum.
    if let Some(rec) = head_record_pos {
        // Zero the stored head checksum's adjustment field location:
        // the adjustment lives at head_data_offset + 8.
        let head_tag_idx = tables.iter().position(|(t, _)| t == b"head").unwrap();
        let head_off = offsets[head_tag_idx];
        // Ensure the field is zero before computing the whole-file sum.
        for b in &mut out[head_off + 8..head_off + 12] {
            *b = 0;
        }
        let adj = 0xB1B0AFBAu32.wrapping_sub(checksum(&out));
        out[head_off + 8..head_off + 12].copy_from_slice(&adj.to_be_bytes());
        // The directory's stored checksum for `head` is computed over the
        // table with its adjustment field zero, which is what we wrote.
        let _ = rec;
    }
    out
}

/// sfnt table checksum: sum of big-endian u32 words, zero-padded.
fn checksum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    let mut i = 0;
    while i < data.len() {
        let mut word = [0u8; 4];
        for (k, b) in word.iter_mut().enumerate() {
            if let Some(&v) = data.get(i + k) {
                *b = v;
            }
        }
        sum = sum.wrapping_add(u32::from_be_bytes(word));
        i += 4;
    }
    sum
}

// --- glyf subsetting ----------------------------------------------------------

fn subset_glyf(d: &[u8], dir: &TableDirectory, keep: &BTreeSet<u16>) -> Option<Vec<u8>> {
    let head = dir.table(d, b"head")?;
    let maxp = dir.table(d, b"maxp")?;
    let loca_tbl = dir.table(d, b"loca")?;
    let glyf_tbl = dir.table(d, b"glyf")?;
    let num_glyphs = be16(maxp, 4)? as usize;
    let long_loca = be16(head, 50)? == 1;

    // Original glyph offsets.
    let read_loca = |i: usize| -> Option<usize> {
        if long_loca {
            be32(loca_tbl, i * 4).map(|v| v as usize)
        } else {
            be16(loca_tbl, i * 2).map(|v| v as usize * 2)
        }
    };
    let mut starts = Vec::with_capacity(num_glyphs + 1);
    for i in 0..=num_glyphs {
        starts.push(read_loca(i)?);
    }

    // Transitive closure over composite-glyph components.
    let mut closure = keep.clone();
    let mut work: Vec<u16> = keep.iter().copied().collect();
    while let Some(gid) = work.pop() {
        let gi = gid as usize;
        if gi + 1 >= starts.len() {
            continue;
        }
        let (s, e) = (starts[gi], starts[gi + 1]);
        if e <= s {
            continue;
        }
        let g = glyf_tbl.get(s..e)?;
        // numberOfContours < 0 ⇒ composite.
        if (be16(g, 0)? as i16) >= 0 {
            continue;
        }
        for comp in CompositeComponents::new(g) {
            if closure.insert(comp) {
                work.push(comp);
            }
        }
    }

    // Build new glyf (kept glyphs verbatim, others empty) + long loca.
    let mut new_glyf = Vec::new();
    let mut new_loca = Vec::with_capacity((num_glyphs + 1) * 4);
    for gid in 0..num_glyphs {
        new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
        if closure.contains(&(gid as u16)) {
            let (s, e) = (starts[gid], starts[gid + 1]);
            new_glyf.extend_from_slice(glyf_tbl.get(s..e)?);
            while new_glyf.len() % 2 != 0 {
                new_glyf.push(0);
            }
        }
    }
    new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());

    // head with indexToLocFormat = 1 (long).
    let mut new_head = head.to_vec();
    new_head[50..52].copy_from_slice(&1u16.to_be_bytes());

    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for r in &dir.records {
        let data = match &r.tag {
            b"glyf" => new_glyf.clone(),
            b"loca" => new_loca.clone(),
            b"head" => new_head.clone(),
            _ => d.get(r.offset..r.offset + r.length)?.to_vec(),
        };
        tables.push((r.tag, data));
    }
    Some(assemble(dir.sfnt, tables))
}

/// Iterates the component glyph ids of a composite `glyf` glyph.
struct CompositeComponents<'a> {
    g: &'a [u8],
    pos: usize,
    done: bool,
}

impl<'a> CompositeComponents<'a> {
    fn new(g: &'a [u8]) -> CompositeComponents<'a> {
        CompositeComponents {
            g,
            pos: 10,
            done: false,
        }
    }
}

impl Iterator for CompositeComponents<'_> {
    type Item = u16;
    fn next(&mut self) -> Option<u16> {
        if self.done {
            return None;
        }
        const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
        const WE_HAVE_A_SCALE: u16 = 0x0008;
        const MORE_COMPONENTS: u16 = 0x0020;
        const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
        const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
        let flags = be16(self.g, self.pos)?;
        let gid = be16(self.g, self.pos + 2)?;
        let mut p = self.pos + 4;
        p += if flags & ARG_1_AND_2_ARE_WORDS != 0 {
            4
        } else {
            2
        };
        if flags & WE_HAVE_A_SCALE != 0 {
            p += 2;
        } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
            p += 4;
        } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
            p += 8;
        }
        if flags & MORE_COMPONENTS == 0 {
            self.done = true;
        } else {
            self.pos = p;
        }
        Some(gid)
    }
}

// --- CFF subsetting -----------------------------------------------------------

/// Read a CFF INDEX at `pos`; returns the object byte ranges and the
/// position just past the INDEX.
fn read_index(d: &[u8], pos: usize) -> Option<(Vec<(usize, usize)>, usize)> {
    let count = be16(d, pos)? as usize;
    if count == 0 {
        return Some((Vec::new(), pos + 2));
    }
    let off_size = *d.get(pos + 2)? as usize;
    if !(1..=4).contains(&off_size) {
        return None;
    }
    let off_at = |i: usize| -> Option<usize> {
        let base = pos + 3 + i * off_size;
        let bytes = d.get(base..base + off_size)?;
        let mut v = 0usize;
        for &b in bytes {
            v = (v << 8) | b as usize;
        }
        Some(v)
    };
    let data_base = pos + 3 + (count + 1) * off_size - 1;
    let mut objs = Vec::with_capacity(count);
    for i in 0..count {
        objs.push((data_base + off_at(i)?, data_base + off_at(i + 1)?));
    }
    let end = data_base + off_at(count)?;
    Some((objs, end))
}

/// Serialize a CFF INDEX from object byte-slices.
fn write_index(objs: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(objs.len() as u16).to_be_bytes());
    if objs.is_empty() {
        return out;
    }
    let total: usize = objs.iter().map(|o| o.len()).sum();
    // off_size must hold the final offset, `total + 1`.
    let off_size: usize = if total < 0xff {
        1
    } else if total < 0xffff {
        2
    } else if total < 0xff_ffff {
        3
    } else {
        4
    };
    out.push(off_size as u8);
    let mut off = 1usize;
    let write_off = |out: &mut Vec<u8>, v: usize| {
        let b = v.to_be_bytes();
        out.extend_from_slice(&b[8 - off_size..]);
    };
    write_off(&mut out, off);
    for o in objs {
        off += o.len();
        write_off(&mut out, off);
    }
    for o in objs {
        out.extend_from_slice(o);
    }
    out
}

/// A parsed CFF DICT as `(operands_raw_bytes, operator)` entries, where
/// `operator` is the 1- or 2-byte operator encoded as `op` for 0..=21 and
/// `1200 + b` for the `12 b` two-byte operators.
fn parse_dict(d: &[u8]) -> Option<Vec<(Vec<u8>, u16)>> {
    let mut out = Vec::new();
    let mut operands_start = 0usize;
    let mut i = 0usize;
    while i < d.len() {
        let b = d[i];
        if b <= 21 {
            let op = if b == 12 {
                let op2 = *d.get(i + 1)?;
                i += 2;
                1200 + op2 as u16
            } else {
                i += 1;
                b as u16
            };
            out.push((
                d[operands_start..i - if op >= 1200 { 2 } else { 1 }].to_vec(),
                op,
            ));
            operands_start = i;
        } else if b == 28 {
            i += 3;
        } else if b == 29 {
            i += 5;
        } else if b == 30 {
            // real number: nibbles until 0xf.
            i += 1;
            while i < d.len() {
                let byte = d[i];
                i += 1;
                if byte & 0x0f == 0x0f || byte >> 4 == 0x0f {
                    break;
                }
            }
        } else if (32..=246).contains(&b) {
            i += 1;
        } else if (247..=254).contains(&b) {
            i += 2;
        } else {
            return None;
        }
    }
    Some(out)
}

/// Encode a CFF integer operand as a fixed 5-byte form (`29` + i32 BE), so
/// re-encoded DICT offsets have a size independent of their value.
fn enc_offset(v: i32) -> [u8; 5] {
    let b = v.to_be_bytes();
    [29, b[0], b[1], b[2], b[3]]
}

// Top DICT / Font DICT operators that carry absolute CFF offsets.
const OP_CHARSET: u16 = 15;
const OP_CHARSTRINGS: u16 = 17;
const OP_PRIVATE: u16 = 18;
const OP_FDARRAY: u16 = 1236;
const OP_FDSELECT: u16 = 1237;

fn subset_cff(d: &[u8], dir: &TableDirectory, keep: &BTreeSet<u16>) -> Option<Vec<u8>> {
    let cff_rec = dir.find(b"CFF ")?;
    let cff = d.get(cff_rec.offset..cff_rec.offset + cff_rec.length)?;

    let hdr_size = *cff.get(2)? as usize;
    let (_name, p) = read_index(cff, hdr_size)?;
    let (topdicts, p) = read_index(cff, p)?;
    let (strings, p) = read_index(cff, p)?;
    let (gsubrs, gsubrs_end) = read_index(cff, p)?;
    let top = topdicts.first()?;
    let top_bytes = cff.get(top.0..top.1)?;
    let top_entries = parse_dict(top_bytes)?;

    let get_off = |op: u16| -> Option<usize> {
        let (operands, _) = top_entries.iter().find(|(_, o)| *o == op)?;
        Some(parse_dict_int(operands)? as usize)
    };
    // CID-keyed CFF only (the path the CJK font uses); else bail to the
    // full-font fallback.
    let charstrings_off = get_off(OP_CHARSTRINGS)?;
    let charset_off = get_off(OP_CHARSET)?;
    let fdarray_off = get_off(OP_FDARRAY)?;
    let fdselect_off = get_off(OP_FDSELECT)?;
    if charset_off <= 2 {
        return None; // predefined charset: not the CID font we expect
    }

    // Subset the CharStrings INDEX (kept glyphs verbatim, others endchar).
    let (charstrings, _) = read_index(cff, charstrings_off)?;
    let n_glyphs = charstrings.len();
    let endchar = [14u8]; // CFF 'endchar'
    let cs_objs: Vec<&[u8]> = (0..n_glyphs)
        .map(|g| {
            if keep.contains(&(g as u16)) {
                let (s, e) = charstrings[g];
                cff.get(s..e).unwrap_or(&endchar)
            } else {
                &endchar
            }
        })
        .collect();
    let new_charstrings = write_index(&cs_objs);

    // FDArray + per-FD Private(+subrs) blocks. Read first: their start
    // offsets bound the charset / FDSelect sections below.
    let (fd_objs, _) = read_index(cff, fdarray_off)?;
    struct Fd {
        entries: Vec<(Vec<u8>, u16)>,
        priv_size: i32,      // Private DICT size (the operand) — excludes subrs
        priv_block: Vec<u8>, // Private DICT bytes + local subrs, verbatim
    }
    let mut fds = Vec::with_capacity(fd_objs.len());
    let mut priv_starts = Vec::with_capacity(fd_objs.len());
    for (s, e) in &fd_objs {
        let fd_bytes = cff.get(*s..*e)?;
        let entries = parse_dict(fd_bytes)?;
        let (operands, _) = entries.iter().find(|(_, o)| *o == OP_PRIVATE)?;
        let (psize, poff) = parse_dict_two(operands)?;
        let priv_start = poff as usize;
        let priv_end = priv_start + psize as usize;
        let priv_dict = cff.get(priv_start..priv_end)?;
        // The Private DICT's optional Subrs operator (op 19) gives a byte
        // offset *relative to the Private DICT start* of its local subrs
        // INDEX. Copy the contiguous span from the Private DICT through
        // the end of that INDEX verbatim, so the relative offset stays
        // valid wherever the subrs sit (not necessarily right after).
        let mut block_end = priv_end;
        if let Some(local_off) = parse_private_subrs(priv_dict) {
            let abs = priv_start + local_off;
            let (_, subrs_end) = read_index(cff, abs)?;
            block_end = block_end.max(subrs_end);
        }
        priv_starts.push(priv_start);
        fds.push(Fd {
            entries,
            priv_size: psize as i32,
            priv_block: cff.get(priv_start..block_end)?.to_vec(),
        });
    }

    // Verbatim blocks we keep: charset and FDSelect. Neither is trivially
    // length-self-describing, so bound each by the next known section
    // start (charstrings / fdarray / fdselect / charset / any private) —
    // robust against charset/FDSelect format quirks.
    let mut bounds = vec![charstrings_off, fdarray_off, fdselect_off, charset_off];
    bounds.extend(priv_starts.iter().copied());
    let next_after = |o: usize| bounds.iter().copied().filter(|&b| b > o).min();
    let charset_bytes = cff.get(charset_off..next_after(charset_off)?)?.to_vec();
    let fdselect_bytes = cff.get(fdselect_off..next_after(fdselect_off)?)?.to_vec();

    // --- Lay out the new CFF. Order: header, name, topdict, string,
    // gsubr, charset, fdselect, charstrings, fdarray, [private blocks].
    // The leading INDEX regions (name, string, global subrs) are copied
    // verbatim; recompute their exact byte ranges.
    let header_bytes = &cff[0..hdr_size];
    let name_end = after_index(cff, hdr_size)?;
    let topdict_end = after_index(cff, name_end)?;
    let string_end = after_index(cff, topdict_end)?;
    debug_assert_eq!(string_end, gsubrs_end.min(string_end));
    let gsubr_end = after_index(cff, string_end)?;
    let name_region = &cff[hdr_size..name_end];
    let string_region = &cff[topdict_end..string_end];
    let gsubr_region = &cff[string_end..gsubr_end];
    // Re-encode FDArray with placeholder Private offsets to size it.
    let make_fdarray = |priv_offsets: &[usize]| -> Vec<u8> {
        let objs: Vec<Vec<u8>> = fds
            .iter()
            .enumerate()
            .map(|(i, fd)| reencode_fd(&fd.entries, fd.priv_size, priv_offsets[i] as i32))
            .collect();
        let refs: Vec<&[u8]> = objs.iter().map(|v| v.as_slice()).collect();
        write_index(&refs)
    };
    let make_top =
        |charset: usize, fdselect: usize, charstrings: usize, fdarray: usize| -> Vec<u8> {
            let body = reencode_top(&top_entries, charset, fdselect, charstrings, fdarray);
            write_index(&[&body])
        };

    // Size with placeholders (max-size, since fixed 5-byte encoding makes
    // size constant regardless of the values).
    let top_index = make_top(0, 0, 0, 0);
    let fdarray_index = make_fdarray(&vec![0usize; fds.len()]);

    // Compute absolute offsets in layout order.
    let mut pos = 0usize;
    pos += header_bytes.len();
    pos += name_region.len();
    let top_pos = pos;
    pos += top_index.len();
    pos += string_region.len();
    pos += gsubr_region.len();
    let charset_pos = pos;
    pos += charset_bytes.len();
    let fdselect_pos = pos;
    pos += fdselect_bytes.len();
    let charstrings_pos = pos;
    pos += new_charstrings.len();
    let fdarray_pos = pos;
    pos += fdarray_index.len();
    let mut priv_offsets = Vec::with_capacity(fds.len());
    for fd in &fds {
        priv_offsets.push(pos);
        pos += fd.priv_block.len();
    }
    let _ = top_pos;

    // Re-emit with real offsets (sizes unchanged → positions still hold).
    let top_index = make_top(charset_pos, fdselect_pos, charstrings_pos, fdarray_pos);
    let fdarray_index = make_fdarray(&priv_offsets);

    let mut new_cff = Vec::with_capacity(pos);
    new_cff.extend_from_slice(header_bytes);
    new_cff.extend_from_slice(name_region);
    new_cff.extend_from_slice(&top_index);
    new_cff.extend_from_slice(string_region);
    new_cff.extend_from_slice(gsubr_region);
    new_cff.extend_from_slice(&charset_bytes);
    new_cff.extend_from_slice(&fdselect_bytes);
    new_cff.extend_from_slice(&new_charstrings);
    new_cff.extend_from_slice(&fdarray_index);
    for fd in &fds {
        new_cff.extend_from_slice(&fd.priv_block);
    }
    let _ = strings;
    let _ = gsubrs;

    // Reassemble the sfnt, replacing only the CFF table.
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for r in &dir.records {
        let data = if &r.tag == b"CFF " {
            new_cff.clone()
        } else {
            d.get(r.offset..r.offset + r.length)?.to_vec()
        };
        tables.push((r.tag, data));
    }
    Some(assemble(dir.sfnt, tables))
}

fn after_index(d: &[u8], pos: usize) -> Option<usize> {
    Some(read_index(d, pos)?.1)
}

/// First integer operand of a DICT operand byte-string.
fn parse_dict_int(operands: &[u8]) -> Option<i64> {
    parse_operands(operands)?.first().copied()
}
fn parse_dict_two(operands: &[u8]) -> Option<(i64, i64)> {
    let v = parse_operands(operands)?;
    Some((*v.first()?, *v.get(1)?))
}

/// Parse the integer operands of a DICT entry (reals are skipped — they
/// never appear in the offset operators we read).
fn parse_operands(d: &[u8]) -> Option<Vec<i64>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < d.len() {
        let b = d[i];
        if b == 28 {
            out.push(i16::from_be_bytes([d[i + 1], d[i + 2]]) as i64);
            i += 3;
        } else if b == 29 {
            out.push(i32::from_be_bytes([d[i + 1], d[i + 2], d[i + 3], d[i + 4]]) as i64);
            i += 5;
        } else if b == 30 {
            i += 1;
            while i < d.len() {
                let byte = d[i];
                i += 1;
                if byte & 0x0f == 0x0f || byte >> 4 == 0x0f {
                    break;
                }
            }
        } else if (32..=246).contains(&b) {
            out.push(b as i64 - 139);
            i += 1;
        } else if (247..=250).contains(&b) {
            out.push((b as i64 - 247) * 256 + d[i + 1] as i64 + 108);
            i += 2;
        } else if (251..=254).contains(&b) {
            out.push(-(b as i64 - 251) * 256 - d[i + 1] as i64 - 108);
            i += 2;
        } else {
            return None;
        }
    }
    Some(out)
}

/// The local-Subrs offset (relative to the Private DICT) if present.
fn parse_private_subrs(private: &[u8]) -> Option<usize> {
    let entries = parse_dict(private)?;
    let (operands, _) = entries.iter().find(|(_, o)| *o == 19)?;
    Some(parse_dict_int(operands)? as usize)
}

/// Re-encode the Top DICT, replacing the four offset operators with
/// fixed-size encodings of the given absolute offsets and copying every
/// other operator verbatim.
fn reencode_top(
    entries: &[(Vec<u8>, u16)],
    charset: usize,
    fdselect: usize,
    charstrings: usize,
    fdarray: usize,
) -> Vec<u8> {
    let mut out = Vec::new();
    for (operands, op) in entries {
        match *op {
            OP_CHARSET => emit_offset_op(&mut out, charset as i32, *op),
            OP_FDSELECT => emit_offset_op(&mut out, fdselect as i32, *op),
            OP_CHARSTRINGS => emit_offset_op(&mut out, charstrings as i32, *op),
            OP_FDARRAY => emit_offset_op(&mut out, fdarray as i32, *op),
            _ => {
                out.extend_from_slice(operands);
                emit_op(&mut out, *op);
            }
        }
    }
    out
}

/// Re-encode one Font DICT, replacing its Private (size, offset) with the
/// given values (size unchanged) at a fixed encoding.
fn reencode_fd(entries: &[(Vec<u8>, u16)], priv_size: i32, priv_off: i32) -> Vec<u8> {
    let mut out = Vec::new();
    for (operands, op) in entries {
        if *op == OP_PRIVATE {
            out.extend_from_slice(&enc_offset(priv_size));
            out.extend_from_slice(&enc_offset(priv_off));
            emit_op(&mut out, *op);
        } else {
            out.extend_from_slice(operands);
            emit_op(&mut out, *op);
        }
    }
    out
}

fn emit_offset_op(out: &mut Vec<u8>, v: i32, op: u16) {
    out.extend_from_slice(&enc_offset(v));
    emit_op(out, op);
}
fn emit_op(out: &mut Vec<u8>, op: u16) {
    if op >= 1200 {
        out.push(12);
        out.push((op - 1200) as u8);
    } else {
        out.push(op as u8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vsd_layout::font::Face;

    fn used(face: Face, chars: &str) -> BTreeSet<u16> {
        let m = vsd_layout::font::FontMetrics::face_metrics(face);
        chars.chars().map(|c| m.glyph(c).0).collect()
    }

    #[test]
    fn glyf_subset_keeps_used_glyphs_and_shrinks() {
        let face = Face::Regular;
        let bytes = face.bytes();
        let keep = used(face, "Hello, World!");
        let sub = subset_face(bytes, &keep);
        // Self-verification accepted it (else it would equal the original).
        assert!(sub.len() < bytes.len(), "subset should be smaller");
        // Re-parse and confirm used glyph outlines survive (verify() ran,
        // but assert here too for clarity).
        let fa = ttf_parser::Face::parse(&sub, 0).unwrap();
        let fb = ttf_parser::Face::parse(bytes, 0).unwrap();
        assert_eq!(fa.number_of_glyphs(), fb.number_of_glyphs());
        for &g in &keep {
            assert_eq!(
                fa.glyph_hor_advance(GlyphId(g)),
                fb.glyph_hor_advance(GlyphId(g))
            );
        }
    }

    #[test]
    fn cff_cjk_subset_is_valid_and_much_smaller() {
        let face = Face::Cjk;
        let bytes = face.bytes();
        let keep = used(face, "这是中文日本語한국어");
        let sub = subset_face(bytes, &keep);
        // The 16 MB CJK font shrinks substantially and still passes
        // self-verification (a real subset, not the fallback original).
        // It does not collapse to a few KB because all global/local subrs
        // are retained (Type2 subr subsetting — which would need a
        // charstring interpreter — is a future refinement); emptying the
        // unused glyph charstrings alone already roughly halves it.
        assert!(
            sub.len() < bytes.len() * 2 / 3,
            "CJK subset should be substantially smaller: {} vs {}",
            sub.len(),
            bytes.len()
        );
        let fa = ttf_parser::Face::parse(&sub, 0).unwrap();
        let fb = ttf_parser::Face::parse(bytes, 0).unwrap();
        for &g in &keep {
            let mut pa = PathSink::default();
            let mut pb = PathSink::default();
            fa.outline_glyph(GlyphId(g), &mut pa);
            fb.outline_glyph(GlyphId(g), &mut pb);
            assert_eq!(pa.0, pb.0, "glyph {g} outline must be preserved");
        }
    }

    #[test]
    fn subsetting_is_deterministic() {
        let face = Face::Cjk;
        let keep = used(face, "中文");
        assert_eq!(
            subset_face(face.bytes(), &keep),
            subset_face(face.bytes(), &keep)
        );
    }
}
