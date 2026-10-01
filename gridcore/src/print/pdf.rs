//! Printed pages as a PDF: a small dependency-free writer, deterministic for
//! a given clock, so its bytes are testable.
//!
//! Each [`Page`] is drawn as printing lays it out: margins, the scale, the
//! sheet centred when `printOptions` asks, print titles, cell text as the grid
//! displays it (number formats applied, `;;;` blank, numbers right-aligned),
//! text overflowing into empty neighbours as Excel prints it and a number too
//! wide shown as `###`, fills and borders, gridlines and headings when asked,
//! and the header and footer with their fields filled in.
//!
//! Text is set in the standard-14 Helvetica family with WinAnsiEncoding, so
//! nothing is embedded; characters outside cp1252 print as `?`. Widths come
//! from Adobe's Helvetica and Helvetica-Bold AFM metrics.

use super::hf::{Fields, Sections, render};
use super::paginate::{Page, Pages, body_points, col_points, heading_points, row_points};
use super::setup::PrintErrors;
use crate::sheet::{Align, CellValue, Workbook, col_name, format_with};

/// Why a job writes no PDF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintError {
    /// "We didn't find anything to print." (FIL-103): the job has no pages.
    NothingToPrint,
    /// The job has more than [`super::paginate::MAX_PAGES`] pages.
    TooManyPages,
}

impl std::fmt::Display for PrintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrintError::NothingToPrint => f.write_str("We didn't find anything to print."),
            PrintError::TooManyPages => write!(
                f,
                "This would print more than {} pages; set a print area or select less.",
                super::paginate::MAX_PAGES
            ),
        }
    }
}

impl std::error::Error for PrintError {}

/// What the header and footer fields print that the workbook doesn't know.
#[derive(Clone, Debug, Default)]
pub struct PdfOptions {
    /// `&D`, as the caller's clock and locale render it.
    pub date: String,
    /// `&T`.
    pub time: String,
    /// `&Z`: the workbook's folder, with a trailing separator.
    pub path: String,
    /// `&F`: the workbook's file name.
    pub file: String,
}

/// Helvetica glyph widths (1/1000 em) for ASCII 32–126, from Adobe's AFM.
const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278,
    278, // ' '..'/'
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584,
    556, // '0'..'?'
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722,
    778, // '@'..'O'
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469,
    556, // 'P'..'_'
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556,
    556, // '`'..'o'
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584, // 'p'..'~'
];

/// Helvetica-Bold glyph widths for ASCII 32–126, from Adobe's AFM.
const HELVETICA_BOLD: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278,
    278, // ' '..'/'
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584,
    611, // '0'..'?'
    975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722,
    778, // '@'..'O'
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584,
    556, // 'P'..'_'
    333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611,
    611, // '`'..'o'
    611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584, // 'p'..'~'
];

/// The width of `s` in points, at `size`.
pub fn text_width(s: &str, bold: bool, size: f64) -> f64 {
    let table = if bold { &HELVETICA_BOLD } else { &HELVETICA };
    let units: u32 = s
        .chars()
        .map(|c| match c as u32 {
            u @ 32..=126 => u32::from(table[(u - 32) as usize]),
            // Latin-1 letters are mostly as wide as a digit.
            _ => 556,
        })
        .sum();
    f64::from(units) / 1000.0 * size
}

/// A char as one WinAnsi (cp1252) byte; anything else is `?`.
fn winansi(ch: char) -> u8 {
    // cp1252's 0x80–0x9F row; 0x81, 0x8D, 0x8F, 0x90 and 0x9D are unused.
    const HIGH: [(char, u8); 27] = [
        ('€', 0x80),
        ('‚', 0x82),
        ('ƒ', 0x83),
        ('„', 0x84),
        ('…', 0x85),
        ('†', 0x86),
        ('‡', 0x87),
        ('ˆ', 0x88),
        ('‰', 0x89),
        ('Š', 0x8A),
        ('‹', 0x8B),
        ('Œ', 0x8C),
        ('Ž', 0x8E),
        ('‘', 0x91),
        ('’', 0x92),
        ('“', 0x93),
        ('”', 0x94),
        ('•', 0x95),
        ('–', 0x96),
        ('—', 0x97),
        ('˜', 0x98),
        ('™', 0x99),
        ('š', 0x9A),
        ('›', 0x9B),
        ('œ', 0x9C),
        ('ž', 0x9E),
        ('Ÿ', 0x9F),
    ];
    match ch as u32 {
        u @ 0x20..=0x7E => u as u8,
        u @ 0xA0..=0xFF => u as u8,
        _ => HIGH
            .iter()
            .find(|(c, _)| *c == ch)
            .map_or(b'?', |&(_, b)| b),
    }
}

/// A PDF literal string, parentheses included.
fn pdf_string(s: &str, out: &mut Vec<u8>) {
    out.push(b'(');
    for ch in s.chars() {
        let b = winansi(ch);
        if matches!(b, b'(' | b')' | b'\\') {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b')');
}

fn num(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

/// Default font size of a cell with none of its own.
const FONT_PT: f64 = 11.0;
/// The gap Excel leaves between a cell edge and its text, in points.
const PAD: f64 = 2.0;

/// One page's content stream.
struct Canvas {
    out: Vec<u8>,
}

impl Canvas {
    fn op(&mut self, s: &str) {
        self.out.extend_from_slice(s.as_bytes());
        self.out.push(b'\n');
    }

    fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64, rgb: (u8, u8, u8)) {
        let (r, g, b) = rgb;
        self.op(&format!(
            "q {} {} {} rg {} {} {} {} re f Q",
            num(f64::from(r) / 255.0),
            num(f64::from(g) / 255.0),
            num(f64::from(b) / 255.0),
            num(x),
            num(y),
            num(w),
            num(h)
        ));
    }

    fn stroke_rect(&mut self, x: f64, y: f64, w: f64, h: f64, gray: f64, width: f64) {
        self.op(&format!(
            "q {} G {} w {} {} {} {} re S Q",
            num(gray),
            num(width),
            num(x),
            num(y),
            num(w),
            num(h)
        ));
    }

    /// Text at baseline (x, y), clipped to [clip_l, clip_r] when given.
    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        s: &str,
        x: f64,
        y: f64,
        size: f64,
        font: u8,
        rgb: Option<(u8, u8, u8)>,
        clip: Option<(f64, f64, f64, f64)>,
    ) {
        if s.is_empty() {
            return;
        }
        self.op("q");
        if let Some((cx, cy, cw, ch)) = clip {
            self.op(&format!(
                "{} {} {} {} re W n",
                num(cx),
                num(cy),
                num(cw),
                num(ch)
            ));
        }
        if let Some((r, g, b)) = rgb {
            self.op(&format!(
                "{} {} {} rg",
                num(f64::from(r) / 255.0),
                num(f64::from(g) / 255.0),
                num(f64::from(b) / 255.0)
            ));
        }
        let mut line =
            format!("BT /F{font} {} Tf {} {} Td ", num(size), num(x), num(y)).into_bytes();
        pdf_string(s, &mut line);
        line.extend_from_slice(b" Tj ET");
        self.out.extend_from_slice(&line);
        self.out.push(b'\n');
        self.op("Q");
    }
}

/// The font resource for a style: F1 Helvetica, F2 Bold, F3 Oblique,
/// F4 BoldOblique.
fn font_id(bold: bool, italic: bool) -> u8 {
    match (bold, italic) {
        (false, false) => 1,
        (true, false) => 2,
        (false, true) => 3,
        (true, true) => 4,
    }
}

/// What a cell prints: its display text, honouring `errors`.
fn cell_text(value: &CellValue, shown: String, errors: PrintErrors) -> String {
    match (value, errors) {
        (CellValue::Error(_), PrintErrors::Blank) => String::new(),
        (CellValue::Error(_), PrintErrors::Dash) => "--".into(),
        (CellValue::Error(_), PrintErrors::NA) => "#N/A".into(),
        _ => shown,
    }
}

/// One cell slot on the page: its sheet coordinates and box.
#[derive(Clone, Copy)]
struct Slot {
    line: u32,
    start: f64,
    size: f64,
}

/// Lay a list of lines out from `origin`, growing in `dir` (+1 right, -1
/// down).
fn slots(lines: &[u32], size: impl Fn(u32) -> f64, origin: f64, dir: f64) -> Vec<Slot> {
    let mut at = origin;
    lines
        .iter()
        .map(|&line| {
            let s = size(line);
            let slot = Slot {
                line,
                start: if dir > 0.0 { at } else { at - s },
                size: s,
            };
            at += dir * s;
            slot
        })
        .collect()
}

fn draw_page(wb: &Workbook, page: &Page, total: u32, opts: &PdfOptions) -> (f64, f64, Vec<u8>) {
    let sheet = &wb.sheets[page.sheet];
    let ps = &sheet.page_setup;
    let (pw, ph) = ps.page_points();
    let m = &ps.margins;
    let k = page.scale;
    let mut c = Canvas { out: Vec::new() };

    let (heading_w, heading_h) = if ps.headings {
        heading_points(sheet)
    } else {
        (0.0, 0.0)
    };
    let col_lines: Vec<u32> = page.title_cols.iter().chain(&page.cols).copied().collect();
    let row_lines: Vec<u32> = page.title_rows.iter().chain(&page.rows).copied().collect();
    let content_w: f64 = col_lines.iter().map(|&c| col_points(sheet, c) * k).sum();
    let content_h: f64 = row_lines.iter().map(|&r| row_points(sheet, r) * k).sum();
    let (body_w, body_h) = body_points(wb, page.sheet);
    let mut left = m.left * 72.0 + heading_w;
    let mut top = ph - m.top * 72.0 - heading_h;
    if ps.h_centered {
        left += ((body_w - content_w) / 2.0).max(0.0);
    }
    if ps.v_centered {
        top -= ((body_h - content_h) / 2.0).max(0.0);
    }
    let cols = slots(&col_lines, |c| col_points(sheet, c) * k, left, 1.0);
    let rows = slots(&row_lines, |r| row_points(sheet, r) * k, top, -1.0);

    // Fills, then gridlines, then borders, then text.
    for r in &rows {
        for col in &cols {
            if let Some(cell) = sheet.cell(r.line, col.line) {
                let xf = wb.styles.xf(cell.style);
                if let Some(rgb) = xf.fill {
                    c.fill_rect(col.start, r.start, col.size, r.size, rgb);
                }
            }
        }
    }
    if ps.grid_lines {
        for r in &rows {
            for col in &cols {
                c.stroke_rect(col.start, r.start, col.size, r.size, 0.75, 0.25);
            }
        }
    }
    for r in &rows {
        for col in &cols {
            if let Some(cell) = sheet.cell(r.line, col.line) {
                if wb.styles.xf(cell.style).border {
                    c.stroke_rect(col.start, r.start, col.size, r.size, 0.0, 0.5);
                }
            }
        }
    }
    if ps.headings {
        let size = FONT_PT * 0.9;
        for col in &cols {
            let label = col_name(col.line);
            let w = text_width(&label, false, size);
            c.stroke_rect(col.start, top, col.size, heading_h, 0.6, 0.25);
            c.text(
                &label,
                col.start + (col.size - w) / 2.0,
                top + 4.0,
                size,
                1,
                None,
                None,
            );
        }
        for r in &rows {
            let label = (r.line + 1).to_string();
            let w = text_width(&label, false, size);
            c.stroke_rect(left - heading_w, r.start, heading_w, r.size, 0.6, 0.25);
            c.text(
                &label,
                left - heading_w + (heading_w - w) / 2.0,
                r.start + r.size * 0.25,
                size,
                1,
                None,
                None,
            );
        }
    }

    for r in &rows {
        // What each slot of this row shows, for overflow.
        let texts: Vec<Option<(String, bool)>> = cols
            .iter()
            .map(|col| {
                let cell = sheet.cell(r.line, col.line)?;
                let xf = wb.styles.xf(cell.style);
                let shown = cell_text(
                    &cell.value,
                    format_with(&xf, &cell.value, wb.date1904),
                    ps.errors,
                );
                (!shown.is_empty()).then_some((shown, matches!(cell.value, CellValue::Number(_))))
            })
            .collect();
        for (i, col) in cols.iter().enumerate() {
            let Some((text, is_num)) = &texts[i] else {
                continue;
            };
            let cell = sheet
                .cell(r.line, col.line)
                .expect("a slot with text has a cell");
            let xf = wb.styles.xf(cell.style);
            let size = xf.font_size.unwrap_or(FONT_PT) * k;
            let pad = PAD * k;
            let align = match xf.align {
                Align::General => match cell.value {
                    CellValue::Number(_) => Align::Right,
                    CellValue::Bool(_) | CellValue::Error(_) => Align::Center,
                    _ => Align::Left,
                },
                a => a,
            };
            let mut text = text.clone();
            let mut tw = text_width(&text, xf.bold, size);
            let room = (col.size - 2.0 * pad).max(0.0);
            // A number never spills: too wide, it fills the cell with #s.
            if *is_num && tw > room {
                let hash = text_width("#", xf.bold, size).max(0.1);
                text = "#".repeat((room / hash).floor() as usize);
                tw = text_width(&text, xf.bold, size);
            }
            // Text spills over empty neighbours on the side it runs to.
            let (mut lo, mut hi) = (i, i);
            if !*is_num && tw > room {
                let span = |lo: usize, hi: usize| {
                    cols[hi].start + cols[hi].size - cols[lo].start - 2.0 * pad
                };
                let grow_right = matches!(align, Align::Left | Align::Center);
                let grow_left = matches!(align, Align::Right | Align::Center);
                loop {
                    let mut grew = false;
                    if span(lo, hi) >= tw {
                        break;
                    }
                    if grow_right && hi + 1 < cols.len() && texts[hi + 1].is_none() {
                        hi += 1;
                        grew = true;
                    }
                    if span(lo, hi) >= tw {
                        break;
                    }
                    if grow_left && lo > 0 && texts[lo - 1].is_none() {
                        lo -= 1;
                        grew = true;
                    }
                    if !grew {
                        break;
                    }
                }
            }
            let box_l = cols[lo].start;
            let box_r = cols[hi].start + cols[hi].size;
            let x = match align {
                Align::Right => box_r - pad - tw,
                Align::Center => col.start + (col.size - tw) / 2.0,
                _ => box_l + pad,
            };
            let y = r.start + (r.size * 0.2).max(1.0);
            let clip = Some((box_l, r.start, box_r - box_l, r.size));
            c.text(
                &text,
                x,
                y,
                size,
                font_id(xf.bold, xf.italic),
                xf.color,
                clip,
            );
        }
    }

    // Header and footer.
    let hf = &ps.header_footer;
    let fields = Fields {
        page: page.number,
        pages: total,
        date: opts.date.clone(),
        time: opts.time.clone(),
        path: opts.path.clone(),
        file: opts.file.clone(),
        tab: sheet.name.clone(),
    };
    let hf_size = if hf.scale_with_doc {
        FONT_PT * k
    } else {
        FONT_PT
    };
    let (hl, hr) = if hf.align_with_margins {
        (m.left * 72.0, pw - m.right * 72.0)
    } else {
        (0.5 * 72.0, pw - 0.5 * 72.0)
    };
    for header in [true, false] {
        let Some(stored) = hf.for_page(page.sheet_page, header) else {
            continue;
        };
        let s = Sections::parse(stored);
        for (section, which) in [(&s.left, 0), (&s.center, 1), (&s.right, 2)] {
            let text = render(section, &fields);
            let lines: Vec<&str> = text.split('\n').collect();
            let n = lines.len();
            for (j, line) in lines.iter().enumerate() {
                let tw = text_width(line, false, hf_size);
                let x = match which {
                    0 => hl,
                    1 => (pw - tw) / 2.0,
                    _ => hr - tw,
                };
                // A header hangs from its distance; a footer stands on its.
                let y = if header {
                    ph - m.header * 72.0 - hf_size * (j as f64 + 0.8)
                } else {
                    m.footer * 72.0 + hf_size * ((n - 1 - j) as f64 + 0.2)
                };
                c.text(line, x, y, hf_size, 1, None, None);
            }
        }
    }
    (pw, ph, c.out)
}

/// The pages as a PDF. No pages is [`PrintError::NothingToPrint`], a job
/// cut short at [`super::paginate::MAX_PAGES`] is
/// [`PrintError::TooManyPages`], and either writes nothing.
pub fn to_pdf(wb: &Workbook, pages: &Pages, opts: &PdfOptions) -> Result<Vec<u8>, PrintError> {
    if pages.truncated {
        return Err(PrintError::TooManyPages);
    }
    if pages.pages.is_empty() {
        return Err(PrintError::NothingToPrint);
    }
    // Objects: 1 Catalog, 2 Pages, 3–6 fonts, then per page content + page.
    let mut objs: Vec<Vec<u8>> = vec![Vec::new(), Vec::new()];
    for name in [
        "Helvetica",
        "Helvetica-Bold",
        "Helvetica-Oblique",
        "Helvetica-BoldOblique",
    ] {
        objs.push(
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /{name} /Encoding /WinAnsiEncoding >>"
            )
            .into_bytes(),
        );
    }
    let mut page_ids = Vec::new();
    for page in &pages.pages {
        let (w, h, content) = draw_page(wb, page, pages.total, opts);
        objs.push(
            [
                format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(),
                content,
                b"endstream".to_vec(),
            ]
            .concat(),
        );
        let content_id = objs.len();
        objs.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /Font << /F1 3 0 R /F2 4 0 R /F3 5 0 R /F4 6 0 R >> >> /Contents {content_id} 0 R >>",
                num(w),
                num(h)
            )
            .into_bytes(),
        );
        page_ids.push(objs.len());
    }
    objs[0] = b"<< /Type /Catalog /Pages 2 0 R >>".to_vec();
    let kids: String = page_ids.iter().map(|id| format!("{id} 0 R ")).collect();
    objs[1] = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.trim_end(),
        page_ids.len()
    )
    .into_bytes();

    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::with_capacity(objs.len());
    for (i, obj) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend(obj);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        out.extend(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    Ok(out)
}

#[cfg(test)]
mod tests;
