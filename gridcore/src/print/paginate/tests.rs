use super::*;
use crate::print::area::{PrintTitles, set_print_area, set_print_titles};
use crate::print::setup::PageOrder;
use crate::sheet::{Cell, ColDef, PageBreak, SheetFormat};

/// A workbook of sheets named S0, S1, … each filled with numbers in
/// rows × cols from A1.
fn book(sizes: &[(u32, u32)]) -> Workbook {
    let mut wb = Workbook::default();
    for (i, &(rows, cols)) in sizes.iter().enumerate() {
        let mut s = Sheet {
            name: format!("S{i}"),
            ..Sheet::default()
        };
        for r in 0..rows {
            for c in 0..cols {
                s.set_cell(r, c, Cell::number(f64::from(r * 100 + c)));
            }
        }
        wb.sheets.push(s);
    }
    wb
}

fn active(wb: &Workbook) -> Pages {
    paginate(wb, &Job::new(What::ActiveSheets(vec![0])))
}

/// Each page as (first row, first col, last row, last col).
fn ranges(p: &Pages) -> Vec<Rect> {
    p.pages.iter().map(Page::range).collect()
}

// Letter portrait, Normal margins: a body of 511.2 × 684 pt. Default
// columns are 64 px = 48 pt (10 to a page), rows 15 pt (45 to a page).

#[test]
fn column_widths_follow_ecma_pixels() {
    assert_eq!(width_px(9.140625), 64.0);
    assert_eq!(width_px(8.43), 64.0 - 5.0);
    assert_eq!(width_px(20.0), 140.0);
    let mut s = Sheet::default();
    assert_eq!(col_points(&s, 0), 48.0, "baseColWidth 8 → 61 px → 64 px");
    s.format = SheetFormat {
        base_col_width: 10,
        ..SheetFormat::default()
    };
    assert_eq!(col_points(&s, 0), 80.0 * 0.75, "10·7+5 = 75 → 80 px");
    s.format.default_col_width = Some(9.140625);
    assert_eq!(col_points(&s, 0), 48.0);
    s.col_defs.push(ColDef {
        min: 1,
        max: 1,
        width: Some(20.0),
        attrs: String::new(),
    });
    assert_eq!(col_points(&s, 1), 105.0);
    s.col_defs.push(ColDef {
        min: 2,
        max: 2,
        width: None,
        attrs: " hidden=\"1\"".into(),
    });
    assert_eq!(col_points(&s, 2), 0.0);
}

#[test]
fn row_heights_follow_ht_then_the_sheet_default() {
    let mut s = Sheet::default();
    assert_eq!(row_points(&s, 0), 15.0);
    s.format.default_row_height = Some(20.0);
    assert_eq!(row_points(&s, 0), 20.0);
    s.set_row_height(1, Some(30.0));
    assert_eq!(row_points(&s, 1), 30.0);
    s.set_row_hidden(1, true);
    assert_eq!(row_points(&s, 1), 0.0);
}

#[test]
fn an_empty_sheet_has_nothing_to_print() {
    let wb = book(&[(0, 0)]);
    let p = active(&wb);
    assert!(p.pages.is_empty());
    assert_eq!(p.total, 0);
}

#[test]
fn the_used_area_pages_down_then_over() {
    let wb = book(&[(200, 13)]);
    let p = active(&wb);
    assert_eq!(p.total, 10);
    assert_eq!(
        ranges(&p),
        vec![
            (0, 0, 44, 9),
            (45, 0, 89, 9),
            (90, 0, 134, 9),
            (135, 0, 179, 9),
            (180, 0, 199, 9),
            (0, 10, 44, 12),
            (45, 10, 89, 12),
            (90, 10, 134, 12),
            (135, 10, 179, 12),
            (180, 10, 199, 12),
        ]
    );
    assert_eq!(
        p.pages.iter().map(|p| p.number).collect::<Vec<_>>(),
        (1..=10).collect::<Vec<_>>()
    );
}

#[test]
fn over_then_down_runs_across_first() {
    let mut wb = book(&[(60, 13)]);
    wb.sheets[0].page_setup.page_order = PageOrder::OverThenDown;
    assert_eq!(
        ranges(&active(&wb)),
        vec![
            (0, 0, 44, 9),
            (0, 10, 44, 12),
            (45, 0, 59, 9),
            (45, 10, 59, 12)
        ]
    );
}

#[test]
fn the_used_area_ignores_formatting_that_does_not_print() {
    let mut wb = book(&[(3, 2)]);
    // A styled but empty cell far away: style 0 is plain, so it adds nothing.
    wb.sheets[0].set_cell(
        99,
        20,
        Cell {
            style: 0,
            ..Cell::default()
        },
    );
    assert_eq!(used_area(&wb, 0), Some((0, 0, 2, 1)));
}

#[test]
fn each_print_area_range_starts_its_own_pages_and_ignore_prints_the_used_area() {
    let mut wb = book(&[(200, 13)]);
    set_print_area(&mut wb, 0, &[(0, 0, 9, 2), (0, 4, 4, 5)]);
    let p = active(&wb);
    assert_eq!(ranges(&p), vec![(0, 0, 9, 2), (0, 4, 4, 5)]);
    let mut job = Job::new(What::ActiveSheets(vec![0]));
    job.ignore_print_areas = true;
    assert_eq!(paginate(&wb, &job).total, 10);
}

#[test]
fn a_whole_column_print_area_stops_at_the_used_rows() {
    let mut wb = book(&[(20, 5)]);
    set_print_area(&mut wb, 0, &[(0, 1, MAX_ROWS - 1, 2)]);
    assert_eq!(ranges(&active(&wb)), vec![(0, 1, 19, 2)]);
}

#[test]
fn title_rows_and_columns_repeat_on_pages_that_do_not_show_them() {
    let mut wb = book(&[(100, 13)]);
    set_print_titles(
        &mut wb,
        0,
        PrintTitles {
            rows: Some((0, 0)),
            cols: Some((0, 0)),
        },
    );
    let p = active(&wb);
    // Page 1 shows row 1 and column A itself. Later pages down repeat row 1,
    // which costs each a row: 44 rows after the first page.
    let first = &p.pages[0];
    assert_eq!(
        (first.title_rows.clone(), first.title_cols.clone()),
        (vec![], vec![])
    );
    assert_eq!(first.range(), (0, 0, 44, 9));
    let second = &p.pages[1];
    assert_eq!(second.title_rows, vec![0]);
    assert_eq!(second.title_cols, vec![]);
    assert_eq!(second.range(), (45, 0, 88, 9));
    // Pages over repeat column A, so they hold 9 columns.
    let over = p.pages.iter().find(|pg| pg.cols[0] >= 10).unwrap();
    assert_eq!(over.title_cols, vec![0]);
    assert_eq!(over.range().1, 10);
}

#[test]
fn hidden_rows_and_columns_are_skipped() {
    let mut wb = book(&[(10, 3)]);
    wb.sheets[0].set_row_hidden(4, true);
    wb.sheets[0].col_defs.push(ColDef {
        min: 1,
        max: 1,
        width: None,
        attrs: " hidden=\"1\"".into(),
    });
    let p = active(&wb);
    assert_eq!(p.pages.len(), 1);
    assert_eq!(p.pages[0].rows, vec![0, 1, 2, 3, 5, 6, 7, 8, 9]);
    assert_eq!(p.pages[0].cols, vec![0, 2]);
}

#[test]
fn hidden_sheets_do_not_print_with_the_entire_workbook() {
    // FIL-CASE-057.
    let mut wb = book(&[(10, 1), (5, 5)]);
    wb.sheets[0].set_row_hidden(4, true);
    wb.sheets[1].hidden = true;
    let p = paginate(&wb, &Job::new(What::EntireWorkbook));
    assert_eq!(p.pages.len(), 1);
    assert_eq!(p.pages[0].sheet, 0);
    assert!(!p.pages[0].rows.contains(&4));
}

#[test]
fn a_manual_break_starts_a_page() {
    let mut wb = book(&[(30, 12)]);
    wb.sheets[0].row_breaks.push(PageBreak {
        id: 13,
        attrs: " max=\"16383\" man=\"1\"".into(),
    });
    wb.sheets[0].col_breaks.push(PageBreak {
        id: 3,
        attrs: " max=\"1048575\" man=\"1\"".into(),
    });
    // An automatic break the file stored is recomputed, not obeyed.
    wb.sheets[0].row_breaks.push(PageBreak {
        id: 20,
        attrs: " max=\"16383\"".into(),
    });
    assert_eq!(
        ranges(&active(&wb)),
        vec![
            (0, 0, 12, 2),
            (13, 0, 29, 2),
            (0, 3, 12, 11),
            (13, 3, 29, 11)
        ]
    );
}

#[test]
fn fit_one_page_wide_takes_the_largest_whole_percentage() {
    let mut wb = book(&[(200, 13)]);
    let ps = &mut wb.sheets[0].page_setup;
    ps.fit_to_page = true;
    ps.fit_width = 1;
    ps.fit_height = 0;
    let p = active(&wb);
    // 13 × 48 pt = 624 pt into 511.2: 81 % (82 % would be 511.68).
    assert_eq!(p.pages[0].scale, 0.81);
    assert!(
        p.pages
            .iter()
            .all(|pg| pg.cols == (0..13).collect::<Vec<_>>())
    );
    // 684 / (15 × 0.81) = 56.3 rows a page.
    assert_eq!(p.pages[0].rows.len(), 56);
    assert_eq!(p.total, 4);
}

#[test]
fn fit_to_page_with_the_schema_defaults_is_one_page() {
    let mut wb = book(&[(200, 13)]);
    wb.sheets[0].page_setup.fit_to_page = true;
    let p = active(&wb);
    assert_eq!(p.total, 1);
    assert_eq!(p.pages[0].scale, 0.22);
    // Without fitToPage the fit counts are ignored and scale applies.
    wb.sheets[0].page_setup.fit_to_page = false;
    wb.sheets[0].page_setup.scale = 50;
    let p = active(&wb);
    assert_eq!(p.pages[0].scale, 0.5);
    assert_eq!(p.pages[0].rows.len(), 91);
}

#[test]
fn landscape_and_margins_change_the_room() {
    let mut wb = book(&[(100, 20)]);
    wb.sheets[0].page_setup.orientation = super::super::setup::Orientation::Landscape;
    // 792 - 100.8 = 691.2 pt wide: 14 columns. 612 - 108 = 504 tall: 33 rows.
    let p = active(&wb);
    assert_eq!(p.pages[0].range(), (0, 0, 32, 13));
}

#[test]
fn numbering_continues_across_sheets_and_honours_a_first_page_number() {
    let mut wb = book(&[(50, 1), (10, 1), (10, 1)]);
    wb.sheets[1].page_setup.first_page_number = Some(10);
    let p = paginate(&wb, &Job::new(What::ActiveSheets(vec![0, 1, 2])));
    assert_eq!(
        p.pages
            .iter()
            .map(|p| (p.sheet, p.number, p.sheet_page))
            .collect::<Vec<_>>(),
        vec![(0, 1, 1), (0, 2, 2), (1, 10, 1), (2, 11, 1)]
    );
    assert_eq!(p.total, 4);
}

#[test]
fn pages_from_and_to_filter_by_position() {
    let wb = book(&[(200, 1)]);
    let mut job = Job::new(What::ActiveSheets(vec![0]));
    job.from = Some(2);
    job.to = Some(3);
    let p = paginate(&wb, &job);
    assert_eq!(
        p.pages.iter().map(|p| p.number).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(p.total, 5);
    job.from = Some(9);
    job.to = None;
    assert!(paginate(&wb, &job).pages.is_empty());
}

#[test]
fn a_selection_prints_each_range_on_its_own_pages() {
    let wb = book(&[(100, 10)]);
    let job = Job::new(What::Selection {
        sheet: 0,
        ranges: vec![(0, 0, 1, 1), (5, 5, 6, 6)],
    });
    assert_eq!(
        ranges(&paginate(&wb, &job)),
        vec![(0, 0, 1, 1), (5, 5, 6, 6)]
    );
}

#[test]
fn whole_column_and_whole_sheet_selections_print_only_the_used_cells() {
    // FIX r1 C1: `A:A` and `A:XFD` over a small sheet.
    let wb = book(&[(100, 13)]);
    let used = active(&wb).total;
    let sel = |ranges| paginate(&wb, &Job::new(What::Selection { sheet: 0, ranges }));
    let a = sel(vec![(0, 0, MAX_ROWS - 1, 0)]);
    assert_eq!(
        ranges(&a),
        vec![(0, 0, 44, 0), (45, 0, 89, 0), (90, 0, 99, 0)]
    );
    let all = sel(vec![(0, 0, MAX_ROWS - 1, MAX_COLS - 1)]);
    assert_eq!(all.total, used);
    assert!(!all.truncated);
    // A range with nothing in it prints nothing.
    assert_eq!(sel(vec![(99, 25, 100, 25)]).total, 0);
    assert_eq!(sel(vec![(200, 0, 300, 3), (0, 0, 1, 1)]).total, 1);
}

#[test]
fn a_job_past_the_page_limit_is_cut_short() {
    let mut wb = book(&[(1, 1)]);
    // A cell in the sheet's last row and column: about 2,300 pages down by
    // 154 across at 10 %.
    wb.sheets[0].set_cell(MAX_ROWS - 1, MAX_COLS - 1, Cell::number(1.0));
    wb.sheets[0].page_setup.scale = 10;
    let p = active(&wb);
    assert!(p.truncated);
    assert!(p.pages.len() <= MAX_PAGES);
    // FIX r2 m1: no sheet after the one cut short is laid out.
    let mut wb = book(&[(1, 1), (5, 5)]);
    wb.sheets[0].set_cell(MAX_ROWS - 1, MAX_COLS - 1, Cell::number(1.0));
    wb.sheets[0].page_setup.scale = 10;
    let p = paginate(&wb, &Job::new(What::ActiveSheets(vec![0, 1])));
    assert!(p.truncated);
    assert!(p.pages.iter().all(|pg| pg.sheet == 0), "sheet 1 laid out");
}

#[test]
fn a_page_number_at_the_top_of_its_range_does_not_overflow() {
    // FIX r2 m2.
    let mut wb = book(&[(100, 1)]);
    wb.sheets[0].page_setup.first_page_number = Some(u32::MAX);
    let p = active(&wb);
    assert_eq!(
        p.pages.iter().map(|p| p.number).collect::<Vec<_>>(),
        vec![u32::MAX; 3]
    );
}

#[test]
fn fit_to_ignores_manual_breaks() {
    // FIX r1 M1: Excel prints a fitted sheet on its pages whatever breaks
    // it carries.
    let mut wb = book(&[(30, 3)]);
    wb.sheets[0].row_breaks.push(PageBreak {
        id: 13,
        attrs: " max=\"16383\" man=\"1\"".into(),
    });
    let ps = &mut wb.sheets[0].page_setup;
    ps.fit_to_page = true;
    ps.fit_width = 1;
    ps.fit_height = 1;
    let p = active(&wb);
    assert_eq!(p.total, 1);
    assert_eq!(p.pages[0].scale, 1.0);
    // Without Fit to, the break holds.
    wb.sheets[0].page_setup.fit_to_page = false;
    assert_eq!(active(&wb).total, 2);
}
