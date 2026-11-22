//! An `.xlsb`'s `xl/styles.bin` as the import reads it (#884): the number
//! formats, fonts, fills and borders its cell XFs name, made into model
//! [`Xf`]s the first time a cell uses one, and the differential formats
//! (BrtDXF) conditional formatting names.
//!
//! What the model holds of a cell format: bold, italic, the font colour,
//! size and name, a solid fill and a thin box border. Everything else
//! (underline, patterned fills, other borders, alignment) is left out.

use std::collections::HashMap;

use super::{BookIn, Le, format_code};
use crate::sheet::{Dxf, Xf};

// The record ids, [MS-XLSB] 2.3.2; each record's layout is its entry in
// [MS-XLSB] 2.4 (Records), under its name.
/// BrtFont.
pub(super) const BRT_FONT: u32 = 43;
/// BrtFmt.
pub(super) const BRT_FMT: u32 = 44;
/// BrtFill.
pub(super) const BRT_FILL: u32 = 45;
/// BrtBorder.
pub(super) const BRT_BORDER: u32 = 46;
/// BrtXF.
pub(super) const BRT_XF: u32 = 47;
/// BrtBeginDXFs and BrtEndDXFs.
pub(super) const BRT_BEGIN_DXFS: u32 = 505;
pub(super) const BRT_END_DXFS: u32 = 506;
/// BrtDXF.
pub(super) const BRT_DXF: u32 = 507;
/// BrtBeginFills, BrtBeginFonts, BrtBeginBorders and BrtBeginCellXFs, each
/// ended by its BrtEnd record (the next id).
pub(super) const BRT_BEGIN_FILLS: u32 = 603;
pub(super) const BRT_END_FILLS: u32 = 604;
pub(super) const BRT_BEGIN_FONTS: u32 = 611;
pub(super) const BRT_END_FONTS: u32 = 612;
pub(super) const BRT_BEGIN_BORDERS: u32 = 613;
pub(super) const BRT_END_BORDERS: u32 = 614;
pub(super) const BRT_BEGIN_CELL_XFS: u32 = 617;
pub(super) const BRT_END_CELL_XFS: u32 = 618;

/// A BrtColor ([MS-XLSB] 2.4, BrtColor) as the model sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Color {
    /// Automatic (xColorType 0), an unset colour (4), or a system colour.
    Auto,
    Rgb(u8, u8, u8),
    /// A theme colour (xColorType 3): its index and tint.
    Theme(u8, i16),
}

/// A BrtColor: `fValidRGB` and `xColorType` (7 bits), `index`,
/// `nTintAndShade`, then red, green, blue and alpha.
pub(super) fn color(c: &mut Le) -> Option<Color> {
    let kind = c.u8()? >> 1;
    let index = c.u8()?;
    let tint = c.u16()? as i16;
    let (r, g, b) = (c.u8()?, c.u8()?, c.u8()?);
    c.u8()?;
    Some(match kind {
        // Indexed: the default palette (a BrtIndexedColor palette of the
        // file's own is not read).
        1 => match super::default_icv_rgb(index as u32) {
            Some((r, g, b)) => Color::Rgb(r, g, b),
            None => Color::Auto,
        },
        2 => Color::Rgb(r, g, b),
        3 => Color::Theme(index, tint),
        _ => Color::Auto,
    })
}

/// A font colour as the model holds it: (colour, unresolved). Black and
/// the automatic colour are none, as is theme 1 without a tint, Excel's
/// default text colour (as the `.xlsx` loader reads `<color theme="1"/>`).
/// Another theme colour is some colour the model doesn't resolve.
pub(super) fn font_color(c: Color) -> (Option<(u8, u8, u8)>, bool) {
    match c {
        Color::Rgb(0, 0, 0) | Color::Auto | Color::Theme(1, 0) => (None, false),
        Color::Rgb(r, g, b) => (Some((r, g, b)), false),
        Color::Theme(..) => (None, true),
    }
}

/// A fill colour as the model holds it: (colour, unresolved).
pub(super) fn fill_color(c: Color) -> (Option<(u8, u8, u8)>, bool) {
    match c {
        Color::Auto => (None, false),
        Color::Rgb(r, g, b) => (Some((r, g, b)), false),
        Color::Theme(..) => (None, true),
    }
}

/// The parts of a BrtFont the model holds.
#[derive(Clone, Debug)]
struct Font {
    bold: bool,
    italic: bool,
    color: Color,
    /// dyHeight, in twentieths of a point.
    height: u16,
    name: String,
}

/// BrtFont: dyHeight, grbit (fItalic is bit 1), bls, sss, uls, bFamily,
/// bCharSet, unused, brtColor, bFontScheme, name.
fn read_font(c: &mut Le) -> Option<Font> {
    let height = c.u16()?;
    let grbit = c.u16()?;
    let bls = c.u16()?;
    c.take(6)?;
    let color = color(c)?;
    c.u8()?;
    let name = c.wide()?;
    Some(Font {
        bold: bls >= 600,
        italic: grbit & 0x0002 != 0,
        color,
        height,
        name,
    })
}

/// BrtFill: fls (the pattern: 0 none, 1 solid), brtColorFore, then the
/// background colour and gradient the model doesn't hold. Only a solid
/// fill is kept, in its foreground colour.
fn read_fill(c: &mut Le) -> Option<Color> {
    let fls = c.u32()?;
    let fore = color(c)?;
    Some(if fls == 1 { fore } else { Color::Auto })
}

/// BrtBorder: flags, then a Blxf (dg, reserved, brtColor) for the top,
/// bottom, left, right and diagonal lines. A box is the four sides thin
/// (dg 1) with no diagonal; in another colour than black it is still a
/// box, and its colour is lost (as [`super::plain_border_color`] says).
fn read_border(c: &mut Le) -> Option<bool> {
    let flags = c.u8()?;
    let mut sides = [0u8; 5];
    for s in &mut sides {
        *s = c.u8()?;
        c.u8()?;
        color(c)?;
    }
    let diagonal = flags & 0x03 != 0 && sides[4] != 0;
    Some(sides[..4] == [1; 4] && !diagonal)
}

/// BrtXF: ixfeParent, iFmt, iFont, iFill, ixBorder, then alignment and
/// flags the model doesn't read here.
#[derive(Clone, Copy, Debug, Default)]
struct RawXf {
    fmt: u16,
    font: u16,
    fill: u16,
    border: u16,
}

/// The styles of an `.xlsb`, read from its `styles.bin`.
#[derive(Default)]
pub(super) struct Styles {
    codes: HashMap<u16, String>,
    fonts: Vec<Font>,
    fills: Vec<Color>,
    borders: Vec<bool>,
    xfs: Vec<RawXf>,
    /// The differential formats, in BrtDXF order (a CF rule's dxfId).
    pub(super) dxfs: Vec<Dxf>,
    /// Each cell XF's `book.xfs` index, made the first time a cell uses it.
    made: HashMap<u32, u32>,
}

impl Styles {
    /// Read `styles.bin`. A record that doesn't read is skipped, but still
    /// takes its place in its list, so the indices after it hold.
    pub(super) fn read(bytes: &[u8]) -> Styles {
        let mut s = Styles::default();
        let mut list = 0u32;
        for (ty, body) in super::xlsb::records(bytes) {
            let mut c = Le::new(body);
            match ty {
                BRT_BEGIN_FONTS | BRT_BEGIN_FILLS | BRT_BEGIN_BORDERS | BRT_BEGIN_CELL_XFS
                | BRT_BEGIN_DXFS => list = ty,
                BRT_END_FONTS | BRT_END_FILLS | BRT_END_BORDERS | BRT_END_CELL_XFS
                | BRT_END_DXFS => list = 0,
                BRT_FMT => {
                    if let (Some(id), Some(code)) = (c.u16(), c.wide()) {
                        s.codes.insert(id, code);
                    }
                }
                BRT_FONT if list == BRT_BEGIN_FONTS => {
                    s.fonts.push(read_font(&mut c).unwrap_or(Font {
                        bold: false,
                        italic: false,
                        color: Color::Auto,
                        height: 220,
                        name: String::new(),
                    }));
                }
                BRT_FILL if list == BRT_BEGIN_FILLS => {
                    s.fills.push(read_fill(&mut c).unwrap_or(Color::Auto));
                }
                BRT_BORDER if list == BRT_BEGIN_BORDERS => {
                    s.borders.push(read_border(&mut c).unwrap_or(false));
                }
                BRT_XF if list == BRT_BEGIN_CELL_XFS => {
                    let mut f = || -> Option<RawXf> {
                        c.u16()?;
                        Some(RawXf {
                            fmt: c.u16()?,
                            font: c.u16()?,
                            fill: c.u16()?,
                            border: c.u16()?,
                        })
                    };
                    s.xfs.push(f().unwrap_or_default());
                }
                BRT_DXF if list == BRT_BEGIN_DXFS => {
                    s.dxfs.push(read_dxf(&mut c).unwrap_or_default());
                }
                _ => {}
            }
        }
        s
    }

    /// The model format of cell XF `xf`.
    fn xf(&self, xf: u32) -> Xf {
        let mut out = Xf::default();
        let Some(raw) = self.xfs.get(xf as usize) else {
            return out;
        };
        if let Some(code) = format_code(&self.codes, raw.fmt) {
            out.set_code(Some(code));
        }
        if let Some(f) = self.fonts.get(raw.font as usize) {
            out.bold = f.bold;
            out.italic = f.italic;
            (out.color, out.color_unresolved) = font_color(f.color);
            let size = f.height as f64 / 20.0;
            out.font_size = (size != 11.0).then_some(size);
            out.font_name = (!f.name.is_empty() && !f.name.eq_ignore_ascii_case("Calibri"))
                .then(|| f.name.clone());
        }
        if let Some(&fill) = self.fills.get(raw.fill as usize) {
            (out.fill, out.fill_unresolved) = fill_color(fill);
        }
        out.border = self
            .borders
            .get(raw.border as usize)
            .copied()
            .unwrap_or(false);
        out
    }

    /// The `book.xfs` index of cell XF `xf`, made the first time a cell
    /// uses it.
    pub(super) fn of(&mut self, xf: u32, book: &mut BookIn) -> u32 {
        if let Some(&i) = self.made.get(&xf) {
            return i;
        }
        let i = book.add_xf(self.xf(xf));
        self.made.insert(xf, i);
        i
    }
}

/// BrtDXF: flags (fNewBorder), reserved, cprops, then cprops XFProps
/// ([MS-XLSB] 2.5, XFProp): xfPropType, cb (the whole XFProp's size) and the
/// value. The model's differential format holds the fill colour, the
/// font colour, bold and italic.
pub(super) fn read_dxf(c: &mut Le) -> Option<Dxf> {
    c.u16()?;
    c.u16()?;
    let n = c.u16()?;
    let mut d = Dxf::default();
    let (mut fore, mut back, mut pattern) = (None, None, None);
    for _ in 0..n {
        let ty = c.u16()?;
        let cb = c.u16()? as usize;
        let body = c.take(cb.checked_sub(4)?)?;
        let mut v = Le::new(body);
        match ty {
            // fill pattern
            0x00 => pattern = v.u8(),
            // foreground and background colours
            0x01 => fore = color(&mut v),
            0x02 => back = color(&mut v),
            // text colour
            0x05 => {
                if let Some(col) = color(&mut v) {
                    (d.color, d.color_unresolved) = match col {
                        // A dxf's explicit black is a colour it sets.
                        Color::Rgb(r, g, b) => (Some((r, g, b)), false),
                        other => font_color(other),
                    };
                }
            }
            // font weight (bls) and italic
            0x19 => d.bold = v.u16().map(|bls| bls >= 600),
            0x1C => d.italic = v.u8().map(|i| i != 0),
            _ => {}
        }
    }
    // A conditional format's solid fill is its background colour (as an
    // `.xlsx` dxf's `<bgColor>`); a fill pattern of none sets no fill.
    if pattern != Some(0) {
        if let Some(col) = back.or(fore) {
            (d.fill, d.fill_unresolved) = fill_color(col);
        }
    }
    Some(d)
}
