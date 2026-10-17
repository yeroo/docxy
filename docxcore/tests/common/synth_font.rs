//! A synthetic TrueType builder for the PDF font tests: no binary fixture and
//! no licence. Glyph 0 is `.notdef`; every other glyph is a triangle (or a
//! composite of earlier glyphs) with a chosen advance, mapped from one char.
#![allow(dead_code)]

pub enum Shape {
    Triangle,
    /// A composite of these (earlier) glyph ids.
    Composite(Vec<u16>),
}

pub struct SynthGlyph {
    pub ch: char,
    pub advance: u16,
    pub shape: Shape,
}

pub fn glyph(ch: char, advance: u16) -> SynthGlyph {
    SynthGlyph {
        ch,
        advance,
        shape: Shape::Triangle,
    }
}

fn be16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn be32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn triangle(w: u16) -> Vec<u8> {
    let mut g = Vec::new();
    be16(&mut g, 1);
    for v in [0i16, 0, w as i16, 700] {
        be16(&mut g, v as u16);
    }
    be16(&mut g, 2); // endPtsOfContours
    be16(&mut g, 0); // instructions
    g.extend_from_slice(&[1, 1, 1]);
    for dx in [0i16, w as i16, -(w as i16)] {
        be16(&mut g, dx as u16);
    }
    for dy in [0i16, 0, 700] {
        be16(&mut g, dy as u16);
    }
    g
}

fn composite(parts: &[u16]) -> Vec<u8> {
    let mut g = Vec::new();
    be16(&mut g, 0xFFFF);
    for v in [0i16, 0, 500, 700] {
        be16(&mut g, v as u16);
    }
    for (i, &p) in parts.iter().enumerate() {
        let more = if i + 1 < parts.len() { 0x20 } else { 0 };
        be16(&mut g, 0x0003 | more);
        be16(&mut g, p);
        be16(&mut g, 0);
        be16(&mut g, 0);
    }
    g
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

pub fn assemble(tables: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    assemble_as(0x0001_0000, tables)
}

pub fn assemble_as(version: u32, tables: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let n = tables.len();
    let mut out = Vec::new();
    be32(&mut out, version);
    be16(&mut out, n as u16);
    be16(&mut out, 0);
    be16(&mut out, 0);
    be16(&mut out, 0);
    let offset = 12 + 16 * n;
    let mut body = Vec::new();
    for (tag, data) in tables {
        out.extend_from_slice(*tag);
        be32(&mut out, checksum(data));
        be32(&mut out, (offset + body.len()) as u32);
        be32(&mut out, data.len() as u32);
        body.extend_from_slice(data);
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
    }
    out.extend(body);
    out
}

/// What else a synthetic font carries.
#[derive(Default)]
pub struct Style {
    /// The family name (a `name` table is written when set).
    pub family: Option<String>,
    pub bold: bool,
    pub italic: bool,
    /// Outlines in a `CFF ` table instead of `glyf`/`loca`.
    pub cff: bool,
}

/// `fs_type` is the OS/2 embedding bits; `units` the units per em.
pub fn build(glyphs: &[SynthGlyph], units: u16, fs_type: u16) -> Vec<u8> {
    build_with(glyphs, units, fs_type, &Style::default())
}

fn name_table(family: &str, bold: bool, italic: bool) -> Vec<u8> {
    let sub = match (bold, italic) {
        (true, true) => "Bold Italic",
        (true, false) => "Bold",
        (false, true) => "Italic",
        (false, false) => "Regular",
    };
    let ps = format!("{}-{}", family.replace(' ', ""), sub.replace(' ', ""));
    let strings = [(1u16, family.to_string()), (2, sub.to_string()), (6, ps)];
    let mut records = Vec::new();
    let mut data = Vec::new();
    for (id, text) in &strings {
        let utf16: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        be16(&mut records, 3);
        be16(&mut records, 1);
        be16(&mut records, 0x409);
        be16(&mut records, *id);
        be16(&mut records, utf16.len() as u16);
        be16(&mut records, data.len() as u16);
        data.extend(utf16);
    }
    let mut t = Vec::new();
    be16(&mut t, 0);
    be16(&mut t, strings.len() as u16);
    be16(&mut t, 6 + records.len() as u16);
    t.extend(records);
    t.extend(data);
    t
}

pub fn build_with(glyphs: &[SynthGlyph], units: u16, fs_type: u16, style: &Style) -> Vec<u8> {
    let n = glyphs.len() + 1;
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    let mut hmtx = Vec::new();
    let mut push = |g: Vec<u8>, adv: u16, glyf: &mut Vec<u8>| {
        be32(&mut loca, glyf.len() as u32);
        glyf.extend(g);
        while !glyf.len().is_multiple_of(4) {
            glyf.push(0);
        }
        be16(&mut hmtx, adv);
        be16(&mut hmtx, 0);
    };
    push(triangle(400), 500, &mut glyf);
    for g in glyphs {
        let bytes = match &g.shape {
            Shape::Triangle => triangle(g.advance.min(2000)),
            Shape::Composite(p) => composite(p),
        };
        push(bytes, g.advance, &mut glyf);
    }
    be32(&mut loca, glyf.len() as u32);

    let mut head = Vec::new();
    be32(&mut head, 0x0001_0000);
    be32(&mut head, 0x0001_0000);
    be32(&mut head, 0); // checkSumAdjustment
    be32(&mut head, 0x5F0F_3CF5);
    be16(&mut head, 0);
    be16(&mut head, units);
    head.extend_from_slice(&[0; 16]); // created, modified
    for v in [0i16, 0, 1000, 1000] {
        be16(&mut head, v as u16);
    }
    be16(
        &mut head,
        u16::from(style.bold) | u16::from(style.italic) << 1,
    );
    be16(&mut head, 8);
    be16(&mut head, 2);
    be16(&mut head, 1); // long loca
    be16(&mut head, 0);

    let mut hhea = Vec::new();
    be32(&mut hhea, 0x0001_0000);
    be16(&mut hhea, (units as u32 * 8 / 10) as u16);
    be16(&mut hhea, (-(units as i32) * 2 / 10) as u16);
    be16(&mut hhea, 0);
    hhea.extend_from_slice(&[0; 24]);
    be16(&mut hhea, n as u16);

    let mut maxp = Vec::new();
    be32(&mut maxp, 0x0001_0000);
    be16(&mut maxp, n as u16);
    maxp.extend_from_slice(&[0; 26]);

    // cmap, one format 12 subtable.
    let mut sorted: Vec<(u32, u16)> = glyphs
        .iter()
        .enumerate()
        .map(|(i, g)| (g.ch as u32, i as u16 + 1))
        .collect();
    sorted.sort();
    let mut cmap = Vec::new();
    be16(&mut cmap, 0);
    be16(&mut cmap, 1);
    be16(&mut cmap, 3);
    be16(&mut cmap, 10);
    be32(&mut cmap, 12);
    be16(&mut cmap, 12);
    be16(&mut cmap, 0);
    be32(&mut cmap, 16 + 12 * sorted.len() as u32);
    be32(&mut cmap, 0);
    be32(&mut cmap, sorted.len() as u32);
    for (c, g) in &sorted {
        be32(&mut cmap, *c);
        be32(&mut cmap, *c);
        be32(&mut cmap, *g as u32);
    }

    let mut os2 = Vec::new();
    be16(&mut os2, 3);
    os2.extend_from_slice(&[0; 6]);
    be16(&mut os2, fs_type);
    os2.resize(96, 0);

    let mut tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"OS/2", os2),
        (b"cmap", cmap),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"maxp", maxp),
    ];
    if style.cff {
        tables.push((b"CFF ", vec![0; 16]));
    } else {
        tables.push((b"glyf", glyf));
        tables.push((b"loca", loca));
    }
    if let Some(family) = &style.family {
        tables.push((b"name", name_table(family, style.bold, style.italic)));
    }
    tables.sort_by_key(|(t, _)| **t);
    assemble_as(if style.cff { 0x4F54_544F } else { 0x0001_0000 }, &tables)
}

/// A TrueType collection holding `fonts` (each a standalone sfnt).
pub fn collection(fonts: &[Vec<u8>]) -> Vec<u8> {
    let header = 12 + 4 * fonts.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"ttcf");
    be32(&mut out, 0x0001_0000);
    be32(&mut out, fonts.len() as u32);
    let mut pos = header;
    let mut offsets = Vec::new();
    for f in fonts {
        offsets.push(pos as u32);
        pos += f.len();
    }
    for o in &offsets {
        be32(&mut out, *o);
    }
    for (f, base) in fonts.iter().zip(&offsets) {
        // Table offsets in a collection are file-absolute.
        let mut f = f.clone();
        let tables = u16::from_be_bytes([f[4], f[5]]) as usize;
        for t in 0..tables {
            let at = 12 + 16 * t + 8;
            let off = u32::from_be_bytes([f[at], f[at + 1], f[at + 2], f[at + 3]]) + base;
            f[at..at + 4].copy_from_slice(&off.to_be_bytes());
        }
        out.extend(f);
    }
    out
}
