use super::*;
use crate::print::paginate::{Job, What, paginate};
use crate::sheet::{Cell, Sheet, Xf};

fn sheet(name: &str) -> Sheet {
    Sheet {
        name: name.into(),
        ..Sheet::default()
    }
}

fn opts() -> PdfOptions {
    PdfOptions {
        date: "10/2/2026".into(),
        time: "9:30 AM".into(),
        path: "C:\\work\\".into(),
        file: "Book.xlsx".into(),
    }
}

fn pdf_of(wb: &Workbook, sheets: Vec<usize>) -> Result<Vec<u8>, PrintError> {
    let pages = paginate(wb, &Job::new(What::ActiveSheets(sheets)));
    to_pdf(wb, &pages, &opts())
}

/// The content streams of a PDF, as text.
fn streams(pdf: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(pdf);
    text.split(">>\nstream\n")
        .skip(1)
        .filter_map(|s| s.split("endstream").next())
        .map(str::to_string)
        .collect()
}

/// The strings each page shows, in drawing order.
fn page_texts(pdf: &[u8]) -> Vec<Vec<String>> {
    streams(pdf)
        .iter()
        .map(|s| {
            s.lines()
                .filter_map(|l| {
                    let a = l.find(") Tj")?;
                    let b = l[..a].rfind(" (")?;
                    Some(l[b + 2..a].replace("\\(", "(").replace("\\)", ")"))
                })
                .collect()
        })
        .collect()
}

/// The `re W n` clip box drawn before `text`: (x, y, w, h).
fn clip_of(pdf: &[u8], text: &str) -> (f64, f64, f64, f64) {
    let all = streams(pdf).join("\n");
    let lines: Vec<&str> = all.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains(&format!("({text}) Tj")))
        .unwrap_or_else(|| panic!("{text} not drawn"));
    let clip = lines[..at]
        .iter()
        .rev()
        .find(|l| l.ends_with("re W n"))
        .expect("a clip");
    let n: Vec<f64> = clip
        .split_whitespace()
        .take(4)
        .map(|v| v.parse().unwrap())
        .collect();
    (n[0], n[1], n[2], n[3])
}

#[test]
fn an_empty_sheet_is_nothing_to_print() {
    let mut wb = Workbook::default();
    wb.sheets.push(sheet("Empty"));
    assert_eq!(pdf_of(&wb, vec![0]), Err(PrintError::NothingToPrint));
    assert_eq!(
        PrintError::NothingToPrint.to_string(),
        "We didn't find anything to print."
    );
}

#[test]
fn every_page_prints_with_its_header_and_the_pdf_is_deterministic() {
    // FIL-CASE-043's shape: a header `&CPage &P of &N` over many pages.
    let mut s = sheet("Data");
    for r in 0..100 {
        for c in 0..13 {
            s.set_cell(r, c, Cell::number(f64::from(r * 100 + c)));
        }
    }
    s.page_setup.header_footer.odd_header = Some("&CPage &P of &N".into());
    s.page_setup.header_footer.odd_footer = Some("&L&A &F&R&D".into());
    let mut wb = Workbook::default();
    wb.sheets.push(s);
    let pages = paginate(&wb, &Job::new(What::ActiveSheets(vec![0])));
    let pdf = pdf_of(&wb, vec![0]).unwrap();
    assert_eq!(pdf, pdf_of(&wb, vec![0]).unwrap(), "same bytes twice");
    assert!(String::from_utf8_lossy(&pdf).contains(&format!("/Count {}", pages.total)));
    let texts = page_texts(&pdf);
    assert_eq!(texts.len(), pages.total as usize);
    for (i, page) in texts.iter().enumerate() {
        let want = format!("Page {} of {}", i + 1, pages.total);
        assert!(page.contains(&want), "page {i}: {page:?}");
        assert!(page.contains(&"Data Book.xlsx".to_string()), "{page:?}");
        assert!(page.contains(&"10/2/2026".to_string()), "{page:?}");
    }
    // The first page's first and last cells.
    assert!(texts[0].contains(&"0".to_string()));
    assert!(texts[0].contains(&"4409".to_string()), "{:?}", texts[0]);
}

#[test]
fn the_header_hangs_from_its_distance_and_the_footer_stands_on_its() {
    let mut s = sheet("S");
    s.set_cell(0, 0, Cell::number(1.0));
    s.page_setup.header_footer.odd_header = Some("&LTop".into());
    s.page_setup.header_footer.odd_footer = Some("&RBottom".into());
    let mut wb = Workbook::default();
    wb.sheets.push(s);
    let pdf = pdf_of(&wb, vec![0]).unwrap();
    let all = streams(&pdf).join("\n");
    // Letter: 792 − 0.3 in (21.6) − 0.8 × 11 = 761.6; left margin 50.4.
    assert!(
        all.contains("BT /F1 11 Tf 50.4 761.6 Td (Top) Tj ET"),
        "{all}"
    );
    // Footer at 0.3 in + 0.2 × 11 = 23.8, right-aligned to 612 − 50.4.
    let w = text_width("Bottom", false, 11.0);
    assert!(
        all.contains("Td (Bottom) Tj")
            && all.contains(&format!(" {} 23.8 Td (Bottom)", num(561.6 - w))),
        "{all}"
    );
}

#[test]
fn hidden_rows_and_semicolon_formats_print_nothing() {
    // FIL-CASE-057.
    let mut wb = Workbook::default();
    wb.styles.xfs.push(Xf::default());
    let hidden = wb.styles.intern(Xf {
        code: Some(";;;".into()),
        ..Xf::default()
    });
    let mut s = sheet("S");
    for r in 0..10 {
        s.set_cell(r, 0, Cell::text(&format!("row{}", r + 1)));
    }
    s.set_row_hidden(4, true);
    s.set_cell(
        2,
        1,
        Cell {
            style: hidden,
            ..Cell::text("secret")
        },
    );
    wb.sheets.push(s);
    let mut other = sheet("Hidden");
    other.set_cell(0, 0, Cell::text("classified"));
    other.hidden = true;
    wb.sheets.push(other);
    let pages = paginate(&wb, &Job::new(What::EntireWorkbook));
    let pdf = to_pdf(&wb, &pages, &opts()).unwrap();
    let texts = page_texts(&pdf);
    assert_eq!(texts.len(), 1);
    let t = &texts[0];
    assert!(t.contains(&"row4".to_string()) && t.contains(&"row6".to_string()));
    assert!(!t.contains(&"row5".to_string()), "{t:?}");
    assert!(
        !t.iter().any(|s| s == "secret" || s == "classified"),
        "{t:?}"
    );
}

#[test]
fn number_formats_apply_and_a_number_too_wide_shows_hashes() {
    let mut wb = Workbook::default();
    wb.styles.xfs.push(Xf::default());
    let pct = wb.styles.intern(Xf {
        code: Some("0%".into()),
        ..Xf::default()
    });
    let mut s = sheet("S");
    s.set_cell(
        0,
        0,
        Cell {
            style: pct,
            ..Cell::number(0.5)
        },
    );
    s.set_cell(1, 0, Cell::number(123_456_789_012.0));
    s.col_defs.push(crate::sheet::ColDef {
        min: 0,
        max: 0,
        width: Some(5.0),
        attrs: String::new(),
    });
    wb.sheets.push(s);
    let t = &page_texts(&pdf_of(&wb, vec![0]).unwrap())[0];
    assert!(t.contains(&"50%".to_string()), "{t:?}");
    assert!(
        t.iter()
            .any(|s| !s.is_empty() && s.chars().all(|c| c == '#')),
        "{t:?}"
    );
}

#[test]
fn text_overflows_into_empty_cells_and_stops_at_a_filled_one() {
    let long = "A heading much wider than one column";
    let mut wb = Workbook::default();
    let mut s = sheet("S");
    s.set_cell(0, 0, Cell::text(long));
    s.set_cell(1, 0, Cell::text(&format!("{long}!")));
    s.set_cell(1, 1, Cell::text("x"));
    s.set_cell(1, 4, Cell::text("end"));
    wb.sheets.push(s);
    let pdf = pdf_of(&wb, vec![0]).unwrap();
    // Row 1: spills over B, C, … as far as it needs (48 pt a column).
    let (_, _, w, _) = clip_of(&pdf, long);
    let need = text_width(long, false, 11.0) + 4.0;
    assert!(w >= need && w < need + 48.0, "{w} for {need}");
    // Row 2: B2 is filled, so A2's text is cut at A's edge.
    let (_, _, w, _) = clip_of(&pdf, &format!("{long}!"));
    assert_eq!(w, 48.0);
}

#[test]
fn right_aligned_numbers_and_error_substitution() {
    let mut wb = Workbook::default();
    let mut s = sheet("S");
    s.set_cell(0, 0, Cell::number(7.0));
    s.set_cell(
        1,
        0,
        Cell {
            value: CellValue::Error("#DIV/0!".into()),
            ..Cell::default()
        },
    );
    s.page_setup.errors = PrintErrors::Dash;
    wb.sheets.push(s);
    let pdf = pdf_of(&wb, vec![0]).unwrap();
    let all = streams(&pdf).join("\n");
    // 7 ends 2 pt short of column A's right edge (50.4 + 48).
    let x = 50.4 + 48.0 - 2.0 - text_width("7", false, 11.0);
    assert!(all.contains(&format!("{} ", num(x))), "{all}");
    let t = &page_texts(&pdf)[0];
    assert!(
        t.contains(&"--".to_string()) && !t.iter().any(|s| s.contains("DIV")),
        "{t:?}"
    );
}

#[test]
fn gridlines_and_headings_print_when_asked() {
    let mut wb = Workbook::default();
    let mut s = sheet("S");
    s.set_cell(1, 1, Cell::number(1.0));
    s.page_setup.grid_lines = true;
    s.page_setup.headings = true;
    wb.sheets.push(s);
    let pdf = pdf_of(&wb, vec![0]).unwrap();
    let t = &page_texts(&pdf)[0];
    for label in ["A", "B", "1", "2"] {
        assert!(t.contains(&label.to_string()), "{label} in {t:?}");
    }
    let all = streams(&pdf).join("\n");
    // Four cells' gridlines.
    assert_eq!(all.matches("0.75 G 0.25 w").count(), 4, "{all}");
}

#[test]
fn landscape_pages_are_turned_and_text_outside_cp1252_is_a_question_mark() {
    let mut wb = Workbook::default();
    let mut s = sheet("S");
    s.set_cell(0, 0, Cell::text("Grüße (ok) 日本"));
    s.page_setup.orientation = crate::print::setup::Orientation::Landscape;
    wb.sheets.push(s);
    let pdf = pdf_of(&wb, vec![0]).unwrap();
    let raw = String::from_utf8_lossy(&pdf);
    assert!(raw.contains("/MediaBox [0 0 792 612]"), "{raw}");
    assert!(pdf.windows(4).any(|w| w == b"Gr\xFC\xDF"), "Latin-1 kept");
    assert!(raw.contains("\\(ok\\) ??) Tj"), "{raw}");
}

#[test]
fn cp1252s_high_row_is_kept_and_anything_else_is_a_question_mark() {
    let mut wb = Workbook::default();
    let mut s = sheet("S");
    s.set_cell(0, 0, Cell::text("Brand™ Œuvre €5 — “ok”"));
    wb.sheets.push(s);
    let pdf = pdf_of(&wb, vec![0]).unwrap();
    let want: &[u8] = b"(Brand\x99 \x8Cuvre \x805 \x97 \x93ok\x94) Tj";
    assert!(pdf.windows(want.len()).any(|w| w == want));
    assert_eq!(winansi('Ā'), b'?');
    assert_eq!(winansi('\u{81}'), b'?', "unused in cp1252");
}

#[test]
fn a_selection_with_nothing_in_it_is_nothing_to_print() {
    let mut wb = Workbook::default();
    let mut s = sheet("S");
    s.set_cell(0, 0, Cell::number(1.0));
    wb.sheets.push(s);
    let pages = paginate(
        &wb,
        &Job::new(What::Selection {
            sheet: 0,
            ranges: vec![(99, 25, 100, 25)],
        }),
    );
    assert_eq!(
        to_pdf(&wb, &pages, &opts()),
        Err(PrintError::NothingToPrint)
    );
}

#[test]
fn a_job_cut_short_is_refused() {
    let mut wb = Workbook::default();
    let mut s = sheet("S");
    s.set_cell(0, 0, Cell::number(1.0));
    wb.sheets.push(s);
    let mut pages = paginate(&wb, &Job::new(What::ActiveSheets(vec![0])));
    pages.truncated = true;
    let e = to_pdf(&wb, &pages, &opts()).unwrap_err();
    assert_eq!(e, PrintError::TooManyPages);
    assert!(e.to_string().contains("100000 pages"), "{e}");
}
