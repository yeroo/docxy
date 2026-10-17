//! Font embedding for `to_pdf`: the provider traits a host implements, and the
//! std-only pieces the writer needs (a TrueType subsetter, the ToUnicode CMap
//! and `/W` array writers, the subset tag). Parsing fonts for their metrics is
//! the host's job (`PdfFace`); this module only rewrites the sfnt bytes it is
//! handed, so `docxcore` stays dependency-free.

use std::collections::BTreeSet;
use std::fmt::Debug;
use std::rc::Rc;

/// One font face, as the PDF writer needs it.
pub trait PdfFace: Debug {
    /// A stable identity (equal for the same face of the same file).
    fn id(&self) -> u64;
    /// The PostScript name, without a subset tag.
    fn ps_name(&self) -> String;
    fn units_per_em(&self) -> u16;
    /// The glyph for `ch`, `None` when the face does not cover it.
    fn glyph(&self, ch: char) -> Option<u16>;
    /// The advance of `gid` in font units.
    fn advance(&self, gid: u16) -> u16;
    fn ascent(&self) -> i16;
    fn descent(&self) -> i16;
    /// The font file (an sfnt with `glyf` outlines, or a collection).
    fn sfnt(&self) -> Rc<[u8]>;
    /// Which face of a collection `sfnt` is; 0 for a plain sfnt.
    fn face_index(&self) -> u32 {
        0
    }
}

/// Where the writer gets faces from.
pub trait PdfFontProvider: Debug {
    /// The face for a document font family and style, `None` when the host
    /// has none.
    fn face(&self, family: &str, bold: bool, italic: bool) -> Option<Rc<dyn PdfFace>>;
    /// Faces tried, in order, for a character the run's own face lacks.
    fn fallbacks(&self) -> &[Rc<dyn PdfFace>];
}

/// What an export had to leave out or approximate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdfReport {
    /// Characters no face covered: printed as `.notdef` of the run's face, or
    /// as a Courier `?` when the run has no usable face.
    pub missing_chars: usize,
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// The table directory of face `index` of an sfnt or collection.
pub(crate) fn sfnt_tables(data: &[u8], index: u32) -> Option<Vec<([u8; 4], &[u8])>> {
    let dir = if data.get(..4)? == b"ttcf" {
        if index >= u32_at(data, 8)? {
            return None;
        }
        u32_at(data, 12 + 4 * index as usize)? as usize
    } else if index == 0 {
        0
    } else {
        return None;
    };
    let n = u16_at(data, dir + 4)? as usize;
    let mut tables = Vec::with_capacity(n);
    for i in 0..n {
        let at = dir + 12 + 16 * i;
        let tag: [u8; 4] = data.get(at..at + 4)?.try_into().ok()?;
        let off = u32_at(data, at + 8)? as usize;
        let len = u32_at(data, at + 12)? as usize;
        tables.push((tag, data.get(off..off.checked_add(len)?)?));
    }
    Some(tables)
}

fn table<'a>(tables: &[([u8; 4], &'a [u8])], tag: &[u8; 4]) -> Option<&'a [u8]> {
    tables.iter().find(|(t, _)| t == tag).map(|(_, d)| *d)
}

fn checksum(d: &[u8]) -> u32 {
    let mut sum = 0u32;
    for c in d.chunks(4) {
        let mut w = [0u8; 4];
        w[..c.len()].copy_from_slice(c);
        sum = sum.wrapping_add(u32::from_be_bytes(w));
    }
    sum
}

/// Add the components of a composite glyph to `set`.
fn add_components(glyph: &[u8], count: usize, set: &mut Vec<u16>) {
    if glyph.len() < 10 || (u16_at(glyph, 0).unwrap_or(0) as i16) >= 0 {
        return;
    }
    let mut at = 10;
    loop {
        let (Some(flags), Some(gid)) = (u16_at(glyph, at), u16_at(glyph, at + 2)) else {
            return;
        };
        if (gid as usize) < count {
            set.push(gid);
        }
        at += 4 + if flags & 0x0001 != 0 { 4 } else { 2 };
        if flags & 0x0008 != 0 {
            at += 2;
        } else if flags & 0x0040 != 0 {
            at += 4;
        } else if flags & 0x0080 != 0 {
            at += 8;
        }
        if flags & 0x0020 == 0 {
            return;
        }
    }
}

/// Rewrite face `index` of `data` keeping only the outlines of `keep` (plus
/// glyph 0 and the components of composites). Glyph ids do not change: the
/// dropped glyphs are emptied, `loca` rebuilt in the long format, and only the
/// tables a PDF viewer needs (`head`, `hhea`, `maxp`, `hmtx`, `loca`, `glyf`,
/// `cvt `, `fpgm`, `prep`) are written.
pub fn subset_sfnt(data: &[u8], index: u32, keep: &BTreeSet<u16>) -> Option<Vec<u8>> {
    let tables = sfnt_tables(data, index)?;
    let head = table(&tables, b"head")?;
    let maxp = table(&tables, b"maxp")?;
    let loca = table(&tables, b"loca")?;
    let glyf = table(&tables, b"glyf")?;
    let count = u16_at(maxp, 4)? as usize;
    let long = u16_at(head, 50)? != 0;
    let offset = |i: usize| -> Option<usize> {
        if long {
            Some(u32_at(loca, 4 * i)? as usize)
        } else {
            Some(u16_at(loca, 2 * i)? as usize * 2)
        }
    };
    let glyph = |i: usize| -> Option<&[u8]> {
        let (a, b) = (offset(i)?, offset(i + 1)?);
        if a > b {
            return None;
        }
        glyf.get(a..b)
    };

    let mut stack: Vec<u16> = keep
        .iter()
        .copied()
        .filter(|&g| (g as usize) < count)
        .collect();
    stack.push(0);
    let mut used = BTreeSet::new();
    while let Some(g) = stack.pop() {
        if used.insert(g) {
            add_components(glyph(g as usize)?, count, &mut stack);
        }
    }

    let mut new_glyf = Vec::new();
    let mut new_loca = Vec::with_capacity(4 * (count + 1));
    for i in 0..count {
        new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
        if used.contains(&(i as u16)) {
            new_glyf.extend_from_slice(glyph(i)?);
            while !new_glyf.len().is_multiple_of(4) {
                new_glyf.push(0);
            }
        }
    }
    new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());

    let mut new_head = head.to_vec();
    new_head.get_mut(8..12)?.fill(0);
    new_head
        .get_mut(50..52)?
        .copy_from_slice(&1u16.to_be_bytes());

    let mut out_tables: Vec<([u8; 4], Vec<u8>)> = vec![
        (*b"glyf", new_glyf),
        (*b"head", new_head),
        (*b"hhea", table(&tables, b"hhea")?.to_vec()),
        (*b"hmtx", table(&tables, b"hmtx")?.to_vec()),
        (*b"loca", new_loca),
        (*b"maxp", maxp.to_vec()),
    ];
    for tag in [b"cvt ", b"fpgm", b"prep"] {
        if let Some(t) = table(&tables, tag) {
            out_tables.push((*tag, t.to_vec()));
        }
    }
    out_tables.sort_by_key(|(t, _)| *t);

    let n = out_tables.len();
    let entry_selector = (usize::BITS - 1 - n.leading_zeros()) as u16;
    let search_range = 16u16 << entry_selector;
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&((16 * n as u16) - search_range).to_be_bytes());
    let pos = 12 + 16 * n;
    let mut body = Vec::new();
    let mut head_at = 0;
    for (tag, d) in &out_tables {
        if tag == b"head" {
            head_at = pos + body.len();
        }
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(d).to_be_bytes());
        out.extend_from_slice(&((pos + body.len()) as u32).to_be_bytes());
        out.extend_from_slice(&(d.len() as u32).to_be_bytes());
        body.extend_from_slice(d);
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
    }
    out.extend(body);
    let adjust = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
    out[head_at + 8..head_at + 12].copy_from_slice(&adjust.to_be_bytes());
    Some(out)
}

/// The six-letter subset tag (`ABCDEF` in `ABCDEF+Name`), derived from the
/// face and the glyphs used so equal exports are equal bytes.
pub fn subset_tag(face_id: u64, gids: &BTreeSet<u16>) -> String {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    let mut eat = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01B3);
    };
    face_id.to_be_bytes().into_iter().for_each(&mut eat);
    for g in gids {
        g.to_be_bytes().into_iter().for_each(&mut eat);
    }
    (0..6)
        .map(|i| (b'A' + ((h >> (i * 8)) % 26) as u8) as char)
        .collect()
}

fn utf16_hex(chars: &[char]) -> String {
    let mut buf = [0u16; 2];
    chars
        .iter()
        .flat_map(|c| c.encode_utf16(&mut buf).to_vec())
        .map(|u| format!("{u:04X}"))
        .collect()
}

/// A ToUnicode CMap: each `(cid, text)` maps a glyph back to the logical
/// characters it was shown for.
pub fn to_unicode_cmap(entries: &[(u16, Vec<char>)]) -> String {
    let mut sorted: Vec<&(u16, Vec<char>)> = entries.iter().collect();
    sorted.sort_by_key(|(cid, _)| *cid);
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for chunk in sorted.chunks(100) {
        s.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (cid, text) in chunk {
            s.push_str(&format!("<{cid:04X}> <{}>\n", utf16_hex(text)));
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s
}

/// A CIDFont `/W` array from `(cid, width in 1/1000 em)`, runs of consecutive
/// cids sharing one entry.
pub fn w_array(widths: &[(u16, u32)]) -> String {
    let mut sorted = widths.to_vec();
    sorted.sort();
    let mut s = String::from("[");
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i;
        while j + 1 < sorted.len() && sorted[j + 1].0 == sorted[j].0 + 1 {
            j += 1;
        }
        s.push_str(&format!(" {} [", sorted[i].0));
        let ws: Vec<String> = sorted[i..=j].iter().map(|(_, w)| w.to_string()).collect();
        s.push_str(&ws.join(" "));
        s.push(']');
        i = j + 1;
    }
    s.push_str(" ]");
    s
}

#[cfg(test)]
#[path = "../tests/common/synth_font.rs"]
mod synth_font;

#[cfg(test)]
mod tests {
    use super::*;

    use super::synth_font::{Shape, Style, SynthGlyph, build, build_with, collection, glyph};

    fn font() -> Vec<u8> {
        build(
            &[
                glyph('A', 600),
                glyph('B', 700),
                glyph('\u{0416}', 800),
                SynthGlyph {
                    ch: '\u{00C5}',
                    advance: 650,
                    shape: Shape::Composite(vec![1]),
                },
            ],
            1000,
            0,
        )
    }

    fn glyph_bytes(data: &[u8], gid: usize) -> usize {
        let t = sfnt_tables(data, 0).unwrap();
        let loca = table(&t, b"loca").unwrap();
        u32_at(loca, 4 * (gid + 1)).unwrap() as usize - u32_at(loca, 4 * gid).unwrap() as usize
    }

    #[test]
    fn subset_keeps_used_glyphs_and_empties_the_rest() {
        let data = font();
        let keep = BTreeSet::from([3u16]);
        let sub = subset_sfnt(&data, 0, &keep).unwrap();
        assert!(sub.len() < data.len() + 8);
        assert!(glyph_bytes(&sub, 0) > 0, "notdef kept");
        assert_eq!(glyph_bytes(&sub, 2), 0, "unused emptied");
        assert!(glyph_bytes(&sub, 3) > 0);
        let tags: Vec<[u8; 4]> = sfnt_tables(&sub, 0).unwrap().iter().map(|t| t.0).collect();
        assert_eq!(
            tags,
            [*b"glyf", *b"head", *b"hhea", *b"hmtx", *b"loca", *b"maxp"]
        );
        assert_eq!(checksum(&sub), 0xB1B0_AFBA, "head adjustment");
    }

    #[test]
    fn composite_components_are_kept() {
        let data = font();
        let sub = subset_sfnt(&data, 0, &BTreeSet::from([4u16])).unwrap();
        assert!(glyph_bytes(&sub, 4) > 0);
        assert!(glyph_bytes(&sub, 1) > 0, "component A kept");
        assert_eq!(glyph_bytes(&sub, 3), 0);
    }

    #[test]
    fn multi_component_composites_with_byte_arguments_and_scales_are_kept() {
        let mut glyphs: Vec<SynthGlyph> = "ABCD".chars().map(|c| glyph(c, 600)).collect();
        glyphs.push(SynthGlyph {
            ch: '\u{00C9}',
            advance: 650,
            // Byte arguments with a scale, an x/y scale, a 2x2 matrix, a scale.
            shape: Shape::Mixed(vec![1, 2, 3, 4]),
        });
        let data = build(&glyphs, 1000, 0);
        let sub = subset_sfnt(&data, 0, &BTreeSet::from([5u16])).unwrap();
        for gid in 1..=5 {
            assert!(glyph_bytes(&sub, gid) > 0, "glyph {gid} kept");
        }
        assert!(glyph_bytes(&sub, 0) > 0);
    }

    #[test]
    fn short_loca_is_read_and_the_subset_is_written_long() {
        let style = Style {
            short_loca: true,
            ..Style::default()
        };
        let glyphs = [glyph('A', 600), glyph('B', 700), glyph('C', 800)];
        let data = build_with(&glyphs, 1000, 0, &style);
        let t = sfnt_tables(&data, 0).unwrap();
        assert_eq!(u16_at(table(&t, b"head").unwrap(), 50), Some(0), "short");
        let loca = table(&t, b"loca").unwrap();
        let source_len = 2 * (u16_at(loca, 2 * 3).unwrap() - u16_at(loca, 2 * 2).unwrap()) as usize;
        let sub = subset_sfnt(&data, 0, &BTreeSet::from([2u16])).unwrap();
        let st = sfnt_tables(&sub, 0).unwrap();
        assert_eq!(u16_at(table(&st, b"head").unwrap(), 50), Some(1), "long");
        assert_eq!(glyph_bytes(&sub, 2), source_len);
        assert_eq!(glyph_bytes(&sub, 1), 0);
        assert_eq!(glyph_bytes(&sub, 3), 0);
    }

    #[test]
    fn subset_is_deterministic_and_smaller_with_few_glyphs() {
        let data = font();
        let a = subset_sfnt(&data, 0, &BTreeSet::from([2u16])).unwrap();
        assert_eq!(a, subset_sfnt(&data, 0, &BTreeSet::from([2u16])).unwrap());
        let all = subset_sfnt(&data, 0, &BTreeSet::from([1, 2, 3, 4])).unwrap();
        assert!(a.len() < all.len());
    }

    #[test]
    fn collection_member_is_subset() {
        let a = font();
        let b = build(&[glyph('Z', 300)], 1000, 0);
        let ttc = collection(&[a, b]);
        assert!(sfnt_tables(&ttc, 1).is_some());
        let sub = subset_sfnt(&ttc, 1, &BTreeSet::from([1u16])).unwrap();
        assert!(glyph_bytes(&sub, 1) > 0);
        assert!(subset_sfnt(&ttc, 2, &BTreeSet::new()).is_none());
        assert!(subset_sfnt(&font(), 1, &BTreeSet::new()).is_none());
    }

    #[test]
    fn garbage_is_refused_not_panicked() {
        assert!(subset_sfnt(b"", 0, &BTreeSet::new()).is_none());
        let data = font();
        assert!(subset_sfnt(&data[..40], 0, &BTreeSet::new()).is_none());
    }

    #[test]
    fn tag_is_stable_and_depends_on_glyphs() {
        let a = subset_tag(7, &BTreeSet::from([1, 2]));
        assert_eq!(a.len(), 6);
        assert!(a.chars().all(|c| c.is_ascii_uppercase()));
        assert_eq!(a, subset_tag(7, &BTreeSet::from([1, 2])));
        assert_ne!(a, subset_tag(7, &BTreeSet::from([1, 3])));
    }

    #[test]
    fn cmap_maps_back_to_logical_text() {
        let cmap = to_unicode_cmap(&[(5, vec!['\u{0416}']), (2, vec!['f', 'i']), (9, vec!['😀'])]);
        assert!(cmap.contains("<0002> <00660069>"));
        assert!(cmap.contains("<0005> <0416>"));
        assert!(cmap.contains("<0009> <D83DDE00>"));
        assert!(cmap.find("<0002>").unwrap() < cmap.find("<0005>").unwrap());
        assert!(cmap.contains("3 beginbfchar"));
    }

    #[test]
    fn w_array_groups_runs() {
        assert_eq!(
            w_array(&[(3, 500), (4, 600), (9, 700)]),
            "[ 3 [500 600] 9 [700] ]"
        );
        assert_eq!(w_array(&[]), "[ ]");
    }
}
