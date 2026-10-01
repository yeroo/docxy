//! Print areas and print titles: the sheet-scoped `_xlnm.Print_Area` and
//! `_xlnm.Print_Titles` defined names, read and written in Excel's spelling.

use crate::sheet::{
    DefinedName, MAX_COLS, MAX_ROWS, Workbook, cell_name, col_name, parse_cell_name,
    quote_sheet_name,
};

pub const PRINT_AREA: &str = "_xlnm.Print_Area";
pub const PRINT_TITLES: &str = "_xlnm.Print_Titles";

/// A rectangle (r1, c1, r2, c2), 0-based and inclusive.
pub type Rect = (u32, u32, u32, u32);

/// One reference of a print-area or print-titles definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintRef {
    /// `$A$1:$C$10` (or one cell).
    Cells(Rect),
    /// `$1:$2`: whole rows r1..=r2.
    Rows(u32, u32),
    /// `$A:$B`: whole columns c1..=c2.
    Cols(u32, u32),
}

impl PrintRef {
    /// The cells it covers.
    pub fn rect(self) -> Rect {
        match self {
            PrintRef::Cells(r) => r,
            PrintRef::Rows(a, b) => (a, 0, b, MAX_COLS - 1),
            PrintRef::Cols(a, b) => (0, a, MAX_ROWS - 1, b),
        }
    }

    /// A rectangle as Excel spells it in a print definition: whole rows and
    /// columns as `$1:$2` / `$A:$B`.
    pub fn of_rect((r1, c1, r2, c2): Rect) -> PrintRef {
        if c1 == 0 && c2 >= MAX_COLS - 1 {
            PrintRef::Rows(r1, r2)
        } else if r1 == 0 && r2 >= MAX_ROWS - 1 {
            PrintRef::Cols(c1, c2)
        } else {
            PrintRef::Cells((r1, c1, r2, c2))
        }
    }

    fn spell(self) -> String {
        let abs = |r: u32, c: u32| format!("${}${}", col_name(c), r + 1);
        match self {
            PrintRef::Cells((r1, c1, r2, c2)) if (r1, c1) == (r2, c2) => abs(r1, c1),
            PrintRef::Cells((r1, c1, r2, c2)) => format!("{}:{}", abs(r1, c1), abs(r2, c2)),
            PrintRef::Rows(a, b) => format!("${}:${}", a + 1, b + 1),
            PrintRef::Cols(a, b) => format!("${}:${}", col_name(a), col_name(b)),
        }
    }
}

/// The references of a definition (`Sheet1!$A$1:$C$10,Sheet1!$1:$2`), sheet
/// prefixes dropped. A part that isn't a reference (`#REF!`) is skipped.
pub fn parse_refs(formula: &str) -> Vec<PrintRef> {
    split_top_level(formula)
        .into_iter()
        .filter_map(|part| parse_ref(strip_sheet(part.trim())))
        .collect()
}

/// `formula` split at commas outside quoted sheet names.
fn split_top_level(formula: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut quoted = false;
    let mut start = 0;
    for (i, c) in formula.char_indices() {
        match c {
            '\'' => quoted = !quoted,
            ',' if !quoted => {
                out.push(&formula[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&formula[start..]);
    out
}

/// The reference after a `Sheet!` / `'My sheet'!` prefix.
fn strip_sheet(part: &str) -> &str {
    let mut quoted = false;
    let mut bang = None;
    for (i, c) in part.char_indices() {
        match c {
            '\'' => quoted = !quoted,
            '!' if !quoted => bang = Some(i),
            _ => {}
        }
    }
    match bang {
        Some(i) => &part[i + 1..],
        None => part,
    }
}

fn parse_ref(r: &str) -> Option<PrintRef> {
    let (a, b) = r.split_once(':').unwrap_or((r, r));
    if let (Some((r1, c1)), Some((r2, c2))) = (parse_cell_name(a), parse_cell_name(b)) {
        return Some(PrintRef::Cells((
            r1.min(r2),
            c1.min(c2),
            r1.max(r2),
            c1.max(c2),
        )));
    }
    let row = |s: &str| {
        let n: u32 = s.trim().trim_start_matches('$').parse().ok()?;
        (1..=MAX_ROWS).contains(&n).then_some(n - 1)
    };
    if let (Some(x), Some(y)) = (row(a), row(b)) {
        return Some(PrintRef::Rows(x.min(y), x.max(y)));
    }
    let col = |s: &str| {
        let s = s.trim().trim_start_matches('$');
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphabetic()) {
            return None;
        }
        // Reuse the cell parser: column `C` is cell `C1`'s column.
        parse_cell_name(&format!("{s}1")).map(|(_, c)| c)
    };
    match (col(a), col(b)) {
        (Some(x), Some(y)) => Some(PrintRef::Cols(x.min(y), x.max(y))),
        _ => None,
    }
}

/// `refs` as a definition on `sheet`: each reference prefixed with the sheet
/// name, quoted when it must be.
pub fn spell_refs(sheet: &str, refs: &[PrintRef]) -> String {
    let prefix = quote_sheet_name(sheet);
    refs.iter()
        .map(|r| format!("{prefix}!{}", r.spell()))
        .collect::<Vec<_>>()
        .join(",")
}

fn find<'a>(wb: &'a Workbook, sheet: usize, name: &str) -> Option<&'a DefinedName> {
    wb.defined_names
        .iter()
        .find(|d| d.scope == Some(sheet) && d.name.eq_ignore_ascii_case(name))
}

/// Set (`Some`) or remove (`None`) a sheet-scoped built-in name.
fn put(wb: &mut Workbook, sheet: usize, name: &str, formula: Option<String>) {
    let at = wb
        .defined_names
        .iter()
        .position(|d| d.scope == Some(sheet) && d.name.eq_ignore_ascii_case(name));
    match (at, formula) {
        (Some(i), Some(f)) => wb.defined_names[i].formula = f,
        (Some(i), None) => {
            wb.defined_names.remove(i);
        }
        (None, Some(f)) => wb.defined_names.push(DefinedName {
            name: name.to_string(),
            scope: Some(sheet),
            formula: f,
        }),
        (None, None) => {}
    }
}

/// The sheet's print area, in stored order. Empty when it has none.
pub fn print_area(wb: &Workbook, sheet: usize) -> Vec<Rect> {
    find(wb, sheet, PRINT_AREA)
        .map(|d| {
            parse_refs(&d.formula)
                .into_iter()
                .map(PrintRef::rect)
                .collect()
        })
        .unwrap_or_default()
}

/// Make `ranges` the sheet's print area (Set Print Area). No ranges clears it.
pub fn set_print_area(wb: &mut Workbook, sheet: usize, ranges: &[Rect]) {
    let Some(name) = wb.sheets.get(sheet).map(|s| s.name.clone()) else {
        return;
    };
    let refs: Vec<PrintRef> = ranges.iter().map(|&r| PrintRef::of_rect(r)).collect();
    let formula = (!refs.is_empty()).then(|| spell_refs(&name, &refs));
    put(wb, sheet, PRINT_AREA, formula);
}

/// Add `range` to the sheet's print area (Add to Print Area); with no print
/// area yet, it becomes one.
pub fn add_print_area(wb: &mut Workbook, sheet: usize, range: Rect) {
    let mut ranges = print_area(wb, sheet);
    ranges.push(range);
    set_print_area(wb, sheet, &ranges);
}

/// Clear Print Area. `false` when there was none.
pub fn clear_print_area(wb: &mut Workbook, sheet: usize) -> bool {
    let had = find(wb, sheet, PRINT_AREA).is_some();
    put(wb, sheet, PRINT_AREA, None);
    had
}

/// The rows and columns repeated on every page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrintTitles {
    /// Rows to repeat at top (r1, r2).
    pub rows: Option<(u32, u32)>,
    /// Columns to repeat at left (c1, c2).
    pub cols: Option<(u32, u32)>,
}

pub fn print_titles(wb: &Workbook, sheet: usize) -> PrintTitles {
    let mut t = PrintTitles::default();
    if let Some(d) = find(wb, sheet, PRINT_TITLES) {
        for r in parse_refs(&d.formula) {
            match r {
                PrintRef::Rows(a, b) => t.rows = Some((a, b)),
                PrintRef::Cols(a, b) => t.cols = Some((a, b)),
                PrintRef::Cells(_) => {}
            }
        }
    }
    t
}

/// Set the print titles: columns first, then rows, as Excel writes them.
/// Neither clears them.
pub fn set_print_titles(wb: &mut Workbook, sheet: usize, titles: PrintTitles) {
    let Some(name) = wb.sheets.get(sheet).map(|s| s.name.clone()) else {
        return;
    };
    let mut refs = Vec::new();
    if let Some((a, b)) = titles.cols {
        refs.push(PrintRef::Cols(a.min(b), a.max(b)));
    }
    if let Some((a, b)) = titles.rows {
        refs.push(PrintRef::Rows(a.min(b), a.max(b)));
    }
    let formula = (!refs.is_empty()).then(|| spell_refs(&name, &refs));
    put(wb, sheet, PRINT_TITLES, formula);
}

/// "A1:C10" for a rectangle, as the control verbs report ranges.
pub fn rect_name((r1, c1, r2, c2): Rect) -> String {
    if (r1, c1) == (r2, c2) {
        cell_name(r1, c1)
    } else {
        format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::Sheet;

    fn wb(names: &[&str]) -> Workbook {
        Workbook {
            sheets: names
                .iter()
                .map(|n| Sheet {
                    name: n.to_string(),
                    ..Sheet::default()
                })
                .collect(),
            ..Workbook::default()
        }
    }

    fn formula(wb: &Workbook, name: &str) -> Option<String> {
        find(wb, 0, name).map(|d| d.formula.clone())
    }

    #[test]
    fn set_add_and_clear_print_area_write_excels_spelling() {
        // FIL-CASE-045.
        let mut w = wb(&["Sheet1"]);
        set_print_area(&mut w, 0, &[(0, 0, 9, 2)]);
        assert_eq!(
            formula(&w, PRINT_AREA).as_deref(),
            Some("Sheet1!$A$1:$C$10")
        );
        add_print_area(&mut w, 0, (0, 4, 4, 5));
        assert_eq!(
            formula(&w, PRINT_AREA).as_deref(),
            Some("Sheet1!$A$1:$C$10,Sheet1!$E$1:$F$5")
        );
        assert_eq!(print_area(&w, 0), vec![(0, 0, 9, 2), (0, 4, 4, 5)]);
        assert!(clear_print_area(&mut w, 0));
        assert_eq!(formula(&w, PRINT_AREA), None);
        assert!(!clear_print_area(&mut w, 0));
    }

    #[test]
    fn a_sheet_name_that_needs_quotes_gets_them() {
        let mut w = wb(&["Q1 Sales"]);
        set_print_area(&mut w, 0, &[(1, 1, 1, 1)]);
        assert_eq!(formula(&w, PRINT_AREA).as_deref(), Some("'Q1 Sales'!$B$2"));
        assert_eq!(print_area(&w, 0), vec![(1, 1, 1, 1)]);
    }

    #[test]
    fn print_titles_are_columns_then_rows() {
        let mut w = wb(&["Sheet1"]);
        set_print_titles(
            &mut w,
            0,
            PrintTitles {
                rows: Some((0, 1)),
                cols: Some((0, 0)),
            },
        );
        assert_eq!(
            formula(&w, PRINT_TITLES).as_deref(),
            Some("Sheet1!$A:$A,Sheet1!$1:$2")
        );
        assert_eq!(
            print_titles(&w, 0),
            PrintTitles {
                rows: Some((0, 1)),
                cols: Some((0, 0))
            }
        );
        set_print_titles(&mut w, 0, PrintTitles::default());
        assert_eq!(formula(&w, PRINT_TITLES), None);
    }

    #[test]
    fn refs_parse_whole_rows_columns_quotes_and_skip_ref_errors() {
        assert_eq!(
            parse_refs("'It''s, here'!$A:$B,Report!$3:$1,Report!#REF!,Report!$C$5"),
            vec![
                PrintRef::Cols(0, 1),
                PrintRef::Rows(0, 2),
                PrintRef::Cells((4, 2, 4, 2))
            ]
        );
        // Whole rows set as a rectangle are spelled as rows.
        let mut w = wb(&["S"]);
        set_print_area(&mut w, 0, &[(0, 0, 4, MAX_COLS - 1)]);
        assert_eq!(formula(&w, PRINT_AREA).as_deref(), Some("S!$1:$5"));
    }
}
