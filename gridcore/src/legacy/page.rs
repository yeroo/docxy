//! A worksheet's page setup, page breaks and outline settings in BIFF8
//! (#1440): the records the `.xls` reader reads into [`PageSetup`] and the
//! writer writes from it, in one place so the two agree bit for bit.
//!
//! MS-XLS 2.4: SETUP (0x00A1), LEFTMARGIN..BOTTOMMARGIN (0x0026-0x0029),
//! HEADER (0x0014), FOOTER (0x0015), HEADERFOOTER (0x089C), HCENTER
//! (0x0083), VCENTER (0x0084), PRINTHEADERS (0x002A), PRINTGRIDLINES
//! (0x002B), WSBOOL (0x0081), HORIZONTALPAGEBREAKS (0x001B) and
//! VERTICALPAGEBREAKS (0x001A).

use crate::print::setup::{
    CellComments, HeaderFooter, Margins, Orientation, PageOrder, PageSetup, PrintErrors,
};
use crate::sheet::{OutlineSettings, PageBreak};

pub(super) const HEADER: u16 = 0x0014;
pub(super) const FOOTER: u16 = 0x0015;
pub(super) const VPAGEBREAKS: u16 = 0x001A;
pub(super) const HPAGEBREAKS: u16 = 0x001B;
pub(super) const LEFTMARGIN: u16 = 0x0026;
pub(super) const RIGHTMARGIN: u16 = 0x0027;
pub(super) const TOPMARGIN: u16 = 0x0028;
pub(super) const BOTTOMMARGIN: u16 = 0x0029;
pub(super) const PRINTHEADERS: u16 = 0x002A;
pub(super) const PRINTGRIDLINES: u16 = 0x002B;
pub(super) const GUTS: u16 = 0x0080;
pub(super) const WSBOOL: u16 = 0x0081;
pub(super) const GRIDSET: u16 = 0x0082;
pub(super) const HCENTER: u16 = 0x0083;
pub(super) const VCENTER: u16 = 0x0084;
pub(super) const SETUP: u16 = 0x00A1;
pub(super) const HEADERFOOTER: u16 = 0x089C;

// SETUP's grbit.
const F_LEFT_TO_RIGHT: u16 = 0x0001;
const F_PORTRAIT: u16 = 0x0002;
const F_NO_PLS: u16 = 0x0004;
const F_NO_COLOR: u16 = 0x0008;
const F_DRAFT: u16 = 0x0010;
const F_NOTES: u16 = 0x0020;
const F_NO_ORIENT: u16 = 0x0040;
const F_USE_PAGE: u16 = 0x0080;
const F_END_NOTES: u16 = 0x0200;

// WSBOOL's.
const F_SHOW_AUTO_BREAKS: u16 = 0x0001;
const F_ROW_SUMS_BELOW: u16 = 0x0040;
const F_COL_SUMS_RIGHT: u16 = 0x0080;
const F_FIT_TO_PAGE: u16 = 0x0100;
const F_DSP_GUTS: u16 = 0x0400;

// HEADERFOOTER's flags.
const F_HF_DIFF_ODD_EVEN: u16 = 0x0001;
const F_HF_DIFF_FIRST: u16 = 0x0002;
const F_HF_SCALE_WITH_DOC: u16 = 0x0004;
const F_HF_ALIGN_MARGINS: u16 = 0x0008;

/// The most characters a BIFF8 header or footer holds.
pub(super) const MAX_HF: usize = 255;
/// The most breaks a BIFF8 page-break record holds (Excel's limit).
pub(super) const MAX_BREAKS: usize = 1026;
/// The deepest outline level BIFF8 holds (ROW and COLINFO's 3 bits), the
/// model's own limit.
pub(super) const MAX_LEVEL: u8 = crate::outline::MAX_LEVEL;

/// The page setup a sheet substream means before any of its records: the
/// margins Excel assumes for an absent LEFTMARGIN..BOTTOMMARGIN (0.75, 0.75,
/// 1, 1 inches) and header and footer margins for an absent SETUP (0.5),
/// not the model's Normal ones.
pub(super) fn absent() -> PageSetup {
    PageSetup {
        margins: Margins {
            left: 0.75,
            right: 0.75,
            top: 1.0,
            bottom: 1.0,
            header: 0.5,
            footer: 0.5,
        },
        ..PageSetup::default()
    }
}

/// Read a SETUP body into `ps`. False when it holds something the model
/// can't (a negative first page number), which is then left out.
///
/// fNoPls makes only iPaperSize, iScale, iRes, iVRes, iCopies, fNoOrient and
/// fPortrait undefined (MS-XLS 2.4.257); the rest counts either way.
pub(super) fn read_setup(ps: &mut PageSetup, b: &[u8]) -> Option<bool> {
    let u16_at = |i: usize| b.get(i..i + 2).map(|x| u16::from_le_bytes([x[0], x[1]]));
    let f64_at = |i: usize| {
        b.get(i..i + 8)
            .map(|x| f64::from_le_bytes(x.try_into().unwrap_or([0; 8])))
    };
    let (paper, scale, start) = (u16_at(0)?, u16_at(2)?, u16_at(4)? as i16);
    let (fit_w, fit_h, grbit) = (u16_at(6)?, u16_at(8)?, u16_at(10)?);
    let (header, footer) = (f64_at(16)?, f64_at(24)?);
    let mut kept = true;
    if grbit & F_NO_PLS == 0 {
        if paper != 0 {
            ps.paper_size = paper as u32;
        }
        if crate::print::setup::SCALE_RANGE.contains(&(scale as u32)) {
            ps.scale = scale as u32;
        }
        ps.orientation = if grbit & F_NO_ORIENT != 0 {
            Orientation::Default
        } else if grbit & F_PORTRAIT != 0 {
            Orientation::Portrait
        } else {
            Orientation::Landscape
        };
    }
    ps.fit_width = fit_w as u32;
    ps.fit_height = fit_h as u32;
    if grbit & F_USE_PAGE != 0 {
        if start >= 0 {
            ps.first_page_number = Some(start as u32);
        } else {
            kept = false;
        }
    }
    ps.page_order = if grbit & F_LEFT_TO_RIGHT != 0 {
        PageOrder::OverThenDown
    } else {
        PageOrder::DownThenOver
    };
    ps.black_and_white = grbit & F_NO_COLOR != 0;
    ps.draft = grbit & F_DRAFT != 0;
    ps.cell_comments = match (grbit & F_NOTES != 0, grbit & F_END_NOTES != 0) {
        (false, _) => CellComments::None,
        (true, false) => CellComments::AsDisplayed,
        (true, true) => CellComments::AtEnd,
    };
    ps.errors = match (grbit >> 10) & 3 {
        0 => PrintErrors::Displayed,
        1 => PrintErrors::Blank,
        2 => PrintErrors::Dash,
        _ => PrintErrors::NA,
    };
    if header.is_finite() && header >= 0.0 {
        ps.margins.header = header;
    }
    if footer.is_finite() && footer >= 0.0 {
        ps.margins.footer = footer;
    }
    Some(kept)
}

/// Read a margin record (0x0026-0x0029) into `ps`.
pub(super) fn read_margin(ps: &mut PageSetup, ty: u16, v: f64) {
    if !v.is_finite() || v < 0.0 {
        return;
    }
    let m = &mut ps.margins;
    match ty {
        LEFTMARGIN => m.left = v,
        RIGHTMARGIN => m.right = v,
        TOPMARGIN => m.top = v,
        BOTTOMMARGIN => m.bottom = v,
        _ => {}
    }
}

/// WSBOOL into the page setup's fit-to-page and the outline settings.
pub(super) fn read_wsbool(ps: &mut PageSetup, outline: &mut OutlineSettings, grbit: u16) {
    ps.fit_to_page = grbit & F_FIT_TO_PAGE != 0;
    outline.summary_below = grbit & F_ROW_SUMS_BELOW != 0;
    outline.summary_right = grbit & F_COL_SUMS_RIGHT != 0;
}

/// HEADERFOOTER's flags (reserved bits masked: Excel sets some) into `hf`.
pub(super) fn read_hf_flags(hf: &mut HeaderFooter, flags: u16) {
    hf.different_odd_even = flags & F_HF_DIFF_ODD_EVEN != 0;
    hf.different_first = flags & F_HF_DIFF_FIRST != 0;
    hf.scale_with_doc = flags & F_HF_SCALE_WITH_DOC != 0;
    hf.align_with_margins = flags & F_HF_ALIGN_MARGINS != 0;
}

/// A text as a header or footer: `None` when empty.
pub(super) fn hf_text(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

/// A break the reader read: BIFF8's are all manual. `start`/`end` are the
/// columns (rows) it spans; `min` is written only when it isn't 0.
pub(super) fn read_break(id: u16, start: u16, end: u16) -> PageBreak {
    let min = if start == 0 {
        String::new()
    } else {
        format!(" min=\"{start}\"")
    };
    PageBreak {
        id: id as u32,
        attrs: format!("{min} max=\"{end}\" man=\"1\""),
    }
}

// ---- writing ----------------------------------------------------------------

/// What a sheet's page setup records hold, ready to write, and whether
/// anything didn't fit.
pub(super) struct Written {
    /// (record type, body), in Excel's order, for the block between
    /// WSBOOL and DEFCOLWIDTH: the breaks, HEADER, FOOTER, HCENTER,
    /// VCENTER, the margins, SETUP and HEADERFOOTER.
    pub records: Vec<(u16, Vec<u8>)>,
    /// Some value BIFF8 can't hold was left out.
    pub setup_lost: bool,
    /// Some manual break past the grid or the record's limit was left out.
    pub breaks_lost: bool,
}

/// PRINTHEADERS and PRINTGRIDLINES, which go before GRIDSET and GUTS.
pub(super) fn print_records(ps: &PageSetup) -> [(u16, Vec<u8>); 2] {
    [
        (PRINTHEADERS, u16::from(ps.headings).to_le_bytes().to_vec()),
        (
            PRINTGRIDLINES,
            u16::from(ps.grid_lines).to_le_bytes().to_vec(),
        ),
    ]
}

/// WSBOOL from the page setup and outline settings.
pub(super) fn wsbool(ps: &PageSetup, outline: OutlineSettings) -> u16 {
    let mut b = F_SHOW_AUTO_BREAKS | F_DSP_GUTS;
    if outline.summary_below {
        b |= F_ROW_SUMS_BELOW;
    }
    if outline.summary_right {
        b |= F_COL_SUMS_RIGHT;
    }
    if ps.fit_to_page {
        b |= F_FIT_TO_PAGE;
    }
    b
}

/// GUTS: the row and column gutters for outlines `row_levels` and
/// `col_levels` deep (0 when flat), as Excel sizes them.
pub(super) fn guts(row_levels: u8, col_levels: u8) -> Vec<u8> {
    let mac = |l: u8| if l == 0 { 0 } else { u16::from(l) + 1 };
    let gutter = |l: u8| if l == 0 { 0 } else { 12 * u16::from(l) + 17 };
    [
        gutter(row_levels),
        gutter(col_levels),
        mac(row_levels),
        mac(col_levels),
    ]
    .iter()
    .flat_map(|v| v.to_le_bytes())
    .collect()
}

/// The records from the breaks to HEADERFOOTER for a sheet's page setup
/// and breaks. `max_row`/`max_col` are the grid's last row and column.
pub(super) fn records(
    ps: &PageSetup,
    row_breaks: &[PageBreak],
    col_breaks: &[PageBreak],
    max_row: u32,
    max_col: u32,
) -> Written {
    let mut out = Written {
        records: Vec::new(),
        setup_lost: false,
        breaks_lost: false,
    };
    // Breaks: manual ones only (an automatic one is Excel's to work out
    // again), sorted by id, on the grid, over the span `min`..`max` says
    // (the whole sheet when it says none), cut to the grid.
    let mut breaks = |ty: u16, list: &[PageBreak], max_id: u32, span: u32| {
        let mut kept: Vec<[u16; 3]> = Vec::new();
        for b in list.iter().filter(|b| b.is_manual()) {
            // A break at 0 is before the first row: nothing to break.
            if b.id == 0 {
                continue;
            }
            if b.id > max_id {
                out.breaks_lost = true;
                continue;
            }
            let attr = |name: &str| {
                crate::sheet::xml_attr(&b.attrs, name).and_then(|v| v.trim().parse::<u32>().ok())
            };
            let end = attr("max").unwrap_or(span).min(span);
            let start = attr("min").unwrap_or(0).min(end);
            kept.push([b.id as u16, start as u16, end as u16]);
        }
        kept.sort_unstable_by_key(|k| k[0]);
        kept.dedup_by_key(|k| k[0]);
        if kept.len() > MAX_BREAKS {
            kept.truncate(MAX_BREAKS);
            out.breaks_lost = true;
        }
        if kept.is_empty() {
            return;
        }
        let mut body = (kept.len() as u16).to_le_bytes().to_vec();
        for k in kept {
            for v in k {
                body.extend_from_slice(&v.to_le_bytes());
            }
        }
        out.records.push((ty, body));
    };
    breaks(HPAGEBREAKS, row_breaks, max_row, max_col);
    breaks(VPAGEBREAKS, col_breaks, max_col, max_row);

    let hf = &ps.header_footer;
    let mut text = |t: &Option<String>| -> Vec<u16> {
        let units: Vec<u16> = t.as_deref().unwrap_or("").encode_utf16().collect();
        if units.len() > MAX_HF {
            out.setup_lost = true;
            return Vec::new();
        }
        units
    };
    let (odd_h, odd_f) = (text(&hf.odd_header), text(&hf.odd_footer));
    let (even_h, even_f) = (text(&hf.even_header), text(&hf.even_footer));
    let (first_h, first_f) = (text(&hf.first_header), text(&hf.first_footer));
    // HEADER and FOOTER: an empty body is none.
    for (ty, units) in [(HEADER, &odd_h), (FOOTER, &odd_f)] {
        let body = if units.is_empty() {
            Vec::new()
        } else {
            let mut b = (units.len() as u16).to_le_bytes().to_vec();
            super::ptg_write::push_chars(&mut b, units);
            b
        };
        out.records.push((ty, body));
    }
    out.records
        .push((HCENTER, u16::from(ps.h_centered).to_le_bytes().to_vec()));
    out.records
        .push((VCENTER, u16::from(ps.v_centered).to_le_bytes().to_vec()));
    let m = &ps.margins;
    for (ty, v) in [
        (LEFTMARGIN, m.left),
        (RIGHTMARGIN, m.right),
        (TOPMARGIN, m.top),
        (BOTTOMMARGIN, m.bottom),
    ] {
        out.records.push((ty, v.to_le_bytes().to_vec()));
    }

    // SETUP: printer settings always given (fNoPls clear), with a 600 dpi
    // resolution and one copy, which the model doesn't keep; the model's
    // Default orientation is fNoOrient.
    let mut grbit = match ps.orientation {
        Orientation::Default => F_NO_ORIENT | F_PORTRAIT,
        Orientation::Portrait => F_PORTRAIT,
        Orientation::Landscape => 0,
    };
    if ps.page_order == PageOrder::OverThenDown {
        grbit |= F_LEFT_TO_RIGHT;
    }
    if ps.black_and_white {
        grbit |= F_NO_COLOR;
    }
    if ps.draft {
        grbit |= F_DRAFT;
    }
    match ps.cell_comments {
        CellComments::None => {}
        CellComments::AsDisplayed => grbit |= F_NOTES,
        CellComments::AtEnd => grbit |= F_NOTES | F_END_NOTES,
    }
    grbit |= match ps.errors {
        PrintErrors::Displayed => 0,
        PrintErrors::Blank => 1,
        PrintErrors::Dash => 2,
        PrintErrors::NA => 3,
    } << 10;
    let start = match ps.first_page_number {
        Some(n) if n <= i16::MAX as u32 => {
            grbit |= F_USE_PAGE;
            n as u16
        }
        Some(_) => {
            out.setup_lost = true;
            1
        }
        None => 1,
    };
    let paper = match u16::try_from(ps.paper_size) {
        Ok(p) => p,
        Err(_) => {
            out.setup_lost = true;
            1
        }
    };
    let clamp16 = |v: u32| v.min(u16::MAX as u32) as u16;
    let mut setup: Vec<u8> = [
        paper,
        clamp16(ps.scale),
        start,
        clamp16(ps.fit_width),
        clamp16(ps.fit_height),
        grbit,
        600,
        600,
    ]
    .iter()
    .flat_map(|v| v.to_le_bytes())
    .collect();
    setup.extend_from_slice(&m.header.to_le_bytes());
    setup.extend_from_slice(&m.footer.to_le_bytes());
    setup.extend_from_slice(&1u16.to_le_bytes());
    out.records.push((SETUP, setup));

    // HEADERFOOTER: FrtHeader, the sheet's view (none), flags, the even
    // and first page texts.
    let mut flags = 0;
    if hf.different_odd_even {
        flags |= F_HF_DIFF_ODD_EVEN;
    }
    if hf.different_first {
        flags |= F_HF_DIFF_FIRST;
    }
    if hf.scale_with_doc {
        flags |= F_HF_SCALE_WITH_DOC;
    }
    if hf.align_with_margins {
        flags |= F_HF_ALIGN_MARGINS;
    }
    let mut body = HEADERFOOTER.to_le_bytes().to_vec();
    body.extend_from_slice(&[0; 10 + 16]);
    body.extend_from_slice(&flags.to_le_bytes());
    let texts = [&even_h, &even_f, &first_h, &first_f];
    for t in texts {
        body.extend_from_slice(&(t.len() as u16).to_le_bytes());
    }
    for t in texts {
        if !t.is_empty() {
            // XLUnicodeStringNoCch: the flags byte, then the characters.
            let mut s = Vec::new();
            super::ptg_write::push_chars(&mut s, t);
            body.extend_from_slice(&s);
        }
    }
    out.records.push((HEADERFOOTER, body));
    out
}
