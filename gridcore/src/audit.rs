//! Background error checking (Excel's green-triangle rules) and the sets
//! behind formula auditing.
//!
//! [`check_cell`] answers, for one cell, which of the [`Rule`]s flag it.
//! Which rules are on is the workbook's [`Rules`]; Ignore Error (which hides
//! one rule on some cells) is [`crate::sheet::Sheet::ignored_errors`], applied by
//! [`flags`]. Everything here reads the workbook as it stands: values are the
//! cells' own, so the caller recalculates first.

use crate::entry::text_as_number;
use crate::formula::{direct_precedents, translate_formula};
use crate::sheet::{CellValue, Workbook};

/// One kind of problem Error Checking reports on a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rule {
    /// The cell holds an error value (`#DIV/0!`, `#N/A`, ...).
    ErrorValue,
    /// A number stored as text (a leading apostrophe or a Text format).
    NumberAsText,
    /// A formula that differs from the formulas on both sides of it, which
    /// agree with each other.
    Inconsistent,
    /// A formula whose range leaves out a number next to it.
    OmitsAdjacent,
    /// A formula that refers to nothing but empty cells.
    RefsEmpty,
    /// A formula in an unlocked cell.
    UnlockedFormula,
}

impl Rule {
    /// Every rule, in the order the smart tag and the Error Checking dialog
    /// look for them.
    pub const ALL: [Rule; 6] = [
        Rule::ErrorValue,
        Rule::NumberAsText,
        Rule::Inconsistent,
        Rule::OmitsAdjacent,
        Rule::RefsEmpty,
        Rule::UnlockedFormula,
    ];

    /// The `<ignoredError>` attribute that names the rule in a worksheet.
    pub fn attr(self) -> &'static str {
        match self {
            Rule::ErrorValue => "evalError",
            Rule::NumberAsText => "numberStoredAsText",
            Rule::Inconsistent => "formula",
            Rule::OmitsAdjacent => "formulaRange",
            Rule::RefsEmpty => "emptyCellReference",
            Rule::UnlockedFormula => "unlockedFormula",
        }
    }

    /// The rule an `<ignoredError>` attribute names, `None` for one this
    /// model does not carry (`twoDigitTextYear`, `calculatedColumn`, ...).
    pub fn from_attr(name: &str) -> Option<Rule> {
        Rule::ALL.into_iter().find(|r| r.attr() == name)
    }

    /// The bold line of Error Checking's dialog and the smart tag's first row.
    pub fn title(self) -> &'static str {
        match self {
            Rule::ErrorValue => "Error in Value",
            Rule::NumberAsText => "Number Stored as Text",
            Rule::Inconsistent => "Inconsistent Formula",
            Rule::OmitsAdjacent => "Formula Omits Adjacent Cells",
            Rule::RefsEmpty => "Formula Refers to Empty Cells",
            Rule::UnlockedFormula => "Unlocked Cell Containing Formula",
        }
    }

    /// The explanation under the title.
    pub fn explanation(self) -> &'static str {
        match self {
            Rule::ErrorValue => "The formula in this cell results in an error.",
            Rule::NumberAsText => {
                "The number in this cell is formatted as text or preceded by an apostrophe."
            }
            Rule::Inconsistent => {
                "The formula in this cell differs from the formulas in this area of the spreadsheet."
            }
            Rule::OmitsAdjacent => {
                "The formula in this cell refers to a range that has additional numbers adjacent to it."
            }
            Rule::RefsEmpty => "The formula in this cell refers to empty cells.",
            Rule::UnlockedFormula => {
                "This formula is not protected, which can cause problems if the sheet is protected."
            }
        }
    }
}

/// What the smart tag and the Error Checking dialog call an error value, as
/// Excel does (`Divide by Zero Error` for `#DIV/0!`).
pub fn error_title(code: &str) -> &'static str {
    match code {
        "#DIV/0!" => "Divide by Zero Error",
        "#N/A" => "Value Not Available Error",
        "#NAME?" => "Invalid Name Error",
        "#NULL!" => "Null Error",
        "#NUM!" => "Number Error",
        "#REF!" => "Invalid Cell Reference Error",
        "#SPILL!" => "Spill Error",
        "#CALC!" => "Calculation Error",
        _ => "Error in Value",
    }
}

/// Which rules are on: Excel's File > Options > Formulas > Error checking
/// rules. All but [`Rule::RefsEmpty`] by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rules {
    pub error_value: bool,
    pub number_as_text: bool,
    pub inconsistent: bool,
    pub omits_adjacent: bool,
    pub refs_empty: bool,
    pub unlocked_formula: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            error_value: true,
            number_as_text: true,
            inconsistent: true,
            omits_adjacent: true,
            refs_empty: false,
            unlocked_formula: true,
        }
    }
}

impl Rules {
    /// Whether `rule` is on.
    pub fn on(&self, rule: Rule) -> bool {
        match rule {
            Rule::ErrorValue => self.error_value,
            Rule::NumberAsText => self.number_as_text,
            Rule::Inconsistent => self.inconsistent,
            Rule::OmitsAdjacent => self.omits_adjacent,
            Rule::RefsEmpty => self.refs_empty,
            Rule::UnlockedFormula => self.unlocked_formula,
        }
    }
}

/// One `<ignoredError>`: the rules Ignore Error turned off over some ranges.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IgnoredError {
    /// The `sqref` ranges, as (r1, c1, r2, c2), 0-based inclusive.
    pub sqref: Vec<(u32, u32, u32, u32)>,
    /// The rules hidden over them.
    pub rules: Vec<Rule>,
    /// The attributes this model does not carry (`twoDigitTextYear="1"`), as
    /// they stand in the file, so they are written back.
    pub extra: String,
}

impl IgnoredError {
    /// Whether this entry hides `rule` on the cell.
    pub fn hides(&self, rule: Rule, row: u32, col: u32) -> bool {
        self.rules.contains(&rule)
            && self
                .sqref
                .iter()
                .any(|&(r1, c1, r2, c2)| (r1..=r2).contains(&row) && (c1..=c2).contains(&col))
    }
}

/// The rules that flag the cell, whether or not they are on or ignored.
/// Empty for a cell the sheet does not have.
pub fn check_cell(wb: &Workbook, sheet: usize, row: u32, col: u32) -> Vec<Rule> {
    let rules = wb.audit_rules;
    let Some(s) = wb.sheets.get(sheet) else {
        return Vec::new();
    };
    let Some(cell) = s.cell(row, col) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if rules.error_value && matches!(cell.value, CellValue::Error(_)) {
        out.push(Rule::ErrorValue);
    }
    if rules.number_as_text && number_as_text(cell) {
        out.push(Rule::NumberAsText);
    }
    if cell.formula.is_some() {
        if rules.inconsistent && inconsistent(wb, sheet, row, col) {
            out.push(Rule::Inconsistent);
        }
        if rules.omits_adjacent && omits_adjacent(wb, sheet, row, col) {
            out.push(Rule::OmitsAdjacent);
        }
        if rules.refs_empty && refs_empty(wb, sheet, row, col) {
            out.push(Rule::RefsEmpty);
        }
        if rules.unlocked_formula
            && wb
                .styles
                .xfs
                .get(cell.style as usize)
                .is_some_and(|x| x.unlocked)
        {
            out.push(Rule::UnlockedFormula);
        }
    }
    out
}

/// The rules that flag the cell and that Ignore Error has not hidden: what
/// the indicator, the smart tag and Error Checking show.
pub fn flags(wb: &Workbook, sheet: usize, row: u32, col: u32) -> Vec<Rule> {
    let mut out = check_cell(wb, sheet, row, col);
    if let Some(s) = wb.sheets.get(sheet) {
        out.retain(|&rule| !s.ignored_errors.iter().any(|e| e.hides(rule, row, col)));
    }
    out
}

/// Ignore Error: hide `rule` on one cell. The rule joins an entry that hides
/// the same rule alone and carries nothing unmodelled, else gets an entry of
/// its own, so what a loaded file says stays as it is.
pub fn ignore(sheet: &mut crate::sheet::Sheet, rule: Rule, row: u32, col: u32) {
    if sheet.ignored_errors.iter().any(|e| e.hides(rule, row, col)) {
        return;
    }
    let rect = (row, col, row, col);
    match sheet
        .ignored_errors
        .iter_mut()
        .find(|e| e.extra.is_empty() && e.rules == [rule])
    {
        Some(e) => e.sqref.push(rect),
        None => sheet.ignored_errors.push(IgnoredError {
            sqref: vec![rect],
            rules: vec![rule],
            extra: String::new(),
        }),
    }
}

/// Convert to Number: the cell a number stored as text becomes: its value as
/// a number in a General cell (the Text format and the apostrophe go), and a
/// `12%` or `1,234` its format as typing it would give. `None` when the cell
/// is not a number stored as text. The style is resolved into `wb`.
pub fn convert_to_number(
    wb: &mut Workbook,
    sheet: usize,
    row: u32,
    col: u32,
) -> Option<crate::sheet::Cell> {
    let cell = wb.sheets.get(sheet)?.cell(row, col)?;
    let CellValue::Text(text) = &cell.value else {
        return None;
    };
    if cell.formula.is_some() {
        return None;
    }
    let (n, format) = text_as_number(text)?;
    let mut xf = wb.styles.xf(cell.style);
    if crate::entry::is_text(&xf) {
        xf.numfmt = crate::sheet::NumFmt::General;
        xf.set_code(None);
    }
    xf.quote_prefix = false;
    if let Some(code) = format.filter(|_| crate::entry::is_general(&xf)) {
        xf.set_code(Some(code.to_string()));
    }
    let style = wb.styles.intern(xf);
    let mut out = crate::sheet::Cell::number(n);
    out.style = style;
    Some(out)
}

fn number_as_text(cell: &crate::sheet::Cell) -> bool {
    cell.formula.is_none()
        && matches!(&cell.value, CellValue::Text(t) if text_as_number(t).is_some())
}

/// The formula of a cell as it would read moved by (`dr`, `dc`), normalised
/// by a parse and a print so the spelling does not matter; `None` for a cell
/// without a formula or one that does not parse.
fn shifted(wb: &Workbook, sheet: usize, row: u32, col: u32, dr: i64, dc: i64) -> Option<String> {
    let src = wb.sheets.get(sheet)?.cell(row, col)?.formula.as_deref()?;
    translate_formula(src, dr, dc)
}

/// Where an inconsistent formula's neighbours that agree with each other
/// sit: above and below it, or to its left and right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Along {
    Column,
    Row,
}

/// Inconsistent: the neighbours on both sides, in a row or in a column, are
/// formulas that read the same relative to their own cell, and this one
/// does not. The direction they sit in and the formula they agree on, as it
/// reads in this cell.
fn inconsistent_with(wb: &Workbook, sheet: usize, row: u32, col: u32) -> Option<(Along, String)> {
    let (r, c) = (i64::from(row), i64::from(col));
    let mine = shifted(wb, sheet, row, col, 0, 0)?;
    // (before, after) neighbours as (dr, dc) steps.
    [
        (Along::Row, (0, -1), (0, 1)),
        (Along::Column, (-1, 0), (1, 0)),
    ]
    .into_iter()
    .find_map(|(along, (br, bc), (ar, ac))| {
        let at = |dr: i64, dc: i64| -> Option<String> {
            let (nr, nc) = (r + dr, c + dc);
            if nr < 0 || nc < 0 {
                return None;
            }
            // The neighbour's formula as it would read in this cell.
            shifted(wb, sheet, nr as u32, nc as u32, -dr, -dc)
        };
        match (at(br, bc), at(ar, ac)) {
            (Some(a), Some(b)) if a == b && a != mine => Some((along, a)),
            _ => None,
        }
    })
}

fn inconsistent(wb: &Workbook, sheet: usize, row: u32, col: u32) -> bool {
    inconsistent_with(wb, sheet, row, col).is_some()
}

/// Copy Formula from Above / from Left: the formula the cell's neighbours
/// agree on, as it reads here, and which side to name (`Column`: above).
/// `None` when the cell is not an inconsistent formula.
pub fn consistent_formula(
    wb: &Workbook,
    sheet: usize,
    row: u32,
    col: u32,
) -> Option<(Along, String)> {
    inconsistent_with(wb, sheet, row, col)
}

/// Omits adjacent cells: a single-column (or single-row) range of the
/// formula that has a constant number right beyond either end, which is not
/// the formula's own cell.
fn omits_adjacent(wb: &Workbook, sheet: usize, row: u32, col: u32) -> bool {
    let Some(s) = wb.sheets.get(sheet) else {
        return false;
    };
    let number_at = |r: Option<u32>, c: Option<u32>| -> bool {
        let (Some(r), Some(c)) = (r, c) else {
            return false;
        };
        (r, c) != (row, col)
            && s.cell(r, c)
                .is_some_and(|x| x.formula.is_none() && matches!(x.value, CellValue::Number(_)))
    };
    direct_precedents(wb, sheet, row, col)
        .into_iter()
        .filter(|&(si, _)| si == sheet)
        .any(|(_, (r1, c1, r2, c2))| {
            if c1 == c2 && r1 != r2 {
                number_at(r1.checked_sub(1), Some(c1)) || number_at(r2.checked_add(1), Some(c1))
            } else if r1 == r2 && c1 != c2 {
                number_at(Some(r1), c1.checked_sub(1)) || number_at(Some(r1), c2.checked_add(1))
            } else {
                false
            }
        })
}

/// Refers to empty cells: some area the formula names holds no value at all.
fn refs_empty(wb: &Workbook, sheet: usize, row: u32, col: u32) -> bool {
    direct_precedents(wb, sheet, row, col)
        .into_iter()
        .any(|(si, (r1, c1, r2, c2))| {
            let Some(s) = wb.sheets.get(si) else {
                return false;
            };
            !s.cells
                .range((r1, 0)..=(r2, u32::MAX))
                .any(|(&(_, c), cell)| (c1..=c2).contains(&c) && !cell.is_blank())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::{Cell, Sheet, Styles, Xf};

    fn wb(cells: &[(&str, Cell)]) -> Workbook {
        let mut sheet = Sheet {
            name: "S".into(),
            ..Sheet::default()
        };
        for (name, cell) in cells {
            let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
            sheet.set_cell(r, c, cell.clone());
        }
        Workbook {
            sheets: vec![sheet],
            ..Workbook::default()
        }
    }

    fn rules(w: &Workbook, a1: &str) -> Vec<Rule> {
        let (r, c) = crate::sheet::parse_cell_name(a1).unwrap();
        check_cell(w, 0, r, c)
    }

    fn f(src: &str) -> Cell {
        Cell::formula(src)
    }

    fn err(code: &str) -> Cell {
        Cell {
            value: CellValue::Error(code.into()),
            formula: Some("1/0".into()),
            ..Cell::default()
        }
    }

    #[test]
    fn an_error_value_is_flagged_and_a_number_is_not() {
        let w = wb(&[("A1", err("#DIV/0!")), ("A2", Cell::number(1.0))]);
        assert_eq!(rules(&w, "A1"), vec![Rule::ErrorValue]);
        assert!(rules(&w, "A2").is_empty());
        assert!(rules(&w, "Z9").is_empty(), "a cell the sheet lacks");
    }

    #[test]
    fn a_number_stored_as_text_is_flagged_only_when_it_reads_as_a_number() {
        let w = wb(&[
            ("A1", Cell::text("123")),
            ("A2", Cell::text("12%")),
            ("A3", Cell::text(" 7 ")),
            ("A4", Cell::text("abc")),
            ("A5", Cell::text("")),
            ("A6", Cell::number(123.0)),
            ("A7", Cell::text("1/2/2020")),
        ]);
        assert_eq!(rules(&w, "A1"), vec![Rule::NumberAsText]);
        assert_eq!(rules(&w, "A2"), vec![Rule::NumberAsText]);
        assert_eq!(rules(&w, "A3"), vec![Rule::NumberAsText]);
        for a1 in ["A4", "A5", "A6", "A7"] {
            assert!(rules(&w, a1).is_empty(), "{a1}");
        }
    }

    #[test]
    fn a_formula_returning_text_is_not_a_number_stored_as_text() {
        let mut cell = f("\"123\"");
        cell.value = CellValue::Text("123".into());
        let w = wb(&[("A1", cell)]);
        assert!(rules(&w, "A1").is_empty());
    }

    #[test]
    fn a_formula_unlike_both_neighbours_that_agree_is_inconsistent() {
        // B1 and B3 read "the cell to the left, plus one"; B2 does not.
        let w = wb(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            ("B1", f("A1+1")),
            ("B2", f("A2*2")),
            ("B3", f("A3+1")),
        ]);
        assert_eq!(rules(&w, "B2"), vec![Rule::Inconsistent]);
        assert!(rules(&w, "B1").is_empty());
        assert!(rules(&w, "B3").is_empty());
    }

    #[test]
    fn inconsistent_looks_along_the_row_too() {
        let w = wb(&[
            ("A2", Cell::number(1.0)),
            ("B2", f("A2+1")),
            ("C2", f("A2*9")),
            ("D2", f("C2+1")),
        ]);
        // B2 and D2, C2's neighbours, both read "the cell to the left, plus
        // one"; C2 does not.
        assert_eq!(rules(&w, "C2"), vec![Rule::Inconsistent]);
    }

    #[test]
    fn inconsistent_needs_formulas_that_agree_on_both_sides() {
        // The neighbours differ from each other: nothing to be inconsistent with.
        let w = wb(&[("B1", f("A1+1")), ("B2", f("A2*2")), ("B3", f("A3-1"))]);
        assert!(rules(&w, "B2").is_empty());
        // A constant neighbour never counts.
        let w = wb(&[
            ("B1", Cell::number(1.0)),
            ("B2", f("A2*2")),
            ("B3", Cell::number(3.0)),
        ]);
        assert!(rules(&w, "B2").is_empty());
        // No neighbour on one side (the first row of a column run).
        let w = wb(&[("B1", f("A1*2")), ("B2", f("A2+1"))]);
        assert!(rules(&w, "B1").is_empty());
        // A constant is never inconsistent, only a formula is.
        let w = wb(&[
            ("B1", f("A1+1")),
            ("B2", Cell::number(9.0)),
            ("B3", f("A3+1")),
        ]);
        assert!(rules(&w, "B2").is_empty());
    }

    #[test]
    fn the_formula_the_neighbours_agree_on_reads_as_it_would_in_the_cell() {
        let w = wb(&[
            ("B1", f("A1+1")),
            ("B2", f("A2*2")),
            ("B3", f("A3+1")),
            ("C1", f("A1+1")),
        ]);
        assert_eq!(
            consistent_formula(&w, 0, 1, 1),
            Some((Along::Column, "A2+1".to_string()))
        );
        assert_eq!(consistent_formula(&w, 0, 0, 1), None, "B1 is consistent");
        assert_eq!(consistent_formula(&w, 0, 0, 2), None, "no neighbours agree");
    }

    #[test]
    fn inconsistent_ignores_how_the_formula_is_spelled() {
        let w = wb(&[("B1", f("A1+1")), ("B2", f("a2 + 1")), ("B3", f("A3+1"))]);
        assert!(rules(&w, "B2").is_empty());
    }

    #[test]
    fn a_range_next_to_a_number_omits_it() {
        // SUM(B2:B4) with a number in B1 above and B5 below.
        let w = wb(&[
            ("B1", Cell::number(5.0)),
            ("B2", Cell::number(1.0)),
            ("B3", Cell::number(2.0)),
            ("B4", Cell::number(3.0)),
            ("B5", Cell::number(4.0)),
            ("C1", f("SUM(B2:B4)")),
        ]);
        assert_eq!(rules(&w, "C1"), vec![Rule::OmitsAdjacent]);
        // Below only.
        let w = wb(&[
            ("B2", Cell::number(1.0)),
            ("B3", Cell::number(2.0)),
            ("B4", Cell::number(4.0)),
            ("C1", f("SUM(B2:B3)")),
        ]);
        assert_eq!(rules(&w, "C1"), vec![Rule::OmitsAdjacent]);
        // A row range, to its right.
        let w = wb(&[
            ("A1", Cell::number(1.0)),
            ("B1", Cell::number(2.0)),
            ("C1", Cell::number(3.0)),
            ("A2", f("SUM(A1:B1)")),
        ]);
        assert_eq!(rules(&w, "A2"), vec![Rule::OmitsAdjacent]);
    }

    #[test]
    fn a_range_with_nothing_numeric_beyond_it_omits_nothing() {
        let w = wb(&[
            ("B1", Cell::text("Total")),
            ("B2", Cell::number(1.0)),
            ("B3", Cell::number(2.0)),
            ("C1", f("SUM(B2:B3)")),
        ]);
        assert!(rules(&w, "C1").is_empty(), "a header is text");
        // The formula's own cell, right under the range, is not omitted.
        let w = wb(&[
            ("B1", Cell::number(1.0)),
            ("B2", Cell::number(2.0)),
            ("B3", f("SUM(B1:B2)")),
        ]);
        assert!(rules(&w, "B3").is_empty());
        // A formula result beyond the range is not a constant number.
        let w = wb(&[
            ("B1", Cell::number(1.0)),
            ("B2", Cell::number(2.0)),
            ("B3", f("B1+B2")),
            ("C1", f("SUM(B1:B2)")),
        ]);
        assert!(rules(&w, "C1").is_empty());
        // A single cell is no range.
        let w = wb(&[
            ("B1", Cell::number(1.0)),
            ("B2", Cell::number(2.0)),
            ("C1", f("B2")),
        ]);
        assert!(rules(&w, "C1").is_empty());
    }

    #[test]
    fn a_reference_to_empty_cells_is_flagged_only_with_the_rule_on() {
        let mut w = wb(&[
            ("A1", f("B1+1")),
            ("A2", f("SUM(C1:C3)")),
            ("B3", Cell::number(1.0)),
        ]);
        assert!(rules(&w, "A1").is_empty(), "off by default");
        w.audit_rules.refs_empty = true;
        assert_eq!(rules(&w, "A1"), vec![Rule::RefsEmpty]);
        assert_eq!(rules(&w, "A2"), vec![Rule::RefsEmpty]);
        let w2 = {
            let mut w = wb(&[
                ("A1", f("B3+1")),
                ("A2", f("SUM(B1:B3)")),
                ("B3", Cell::number(1.0)),
            ]);
            w.audit_rules.refs_empty = true;
            w
        };
        assert!(rules(&w2, "A1").is_empty());
        assert!(rules(&w2, "A2").is_empty(), "one number in the range");
    }

    #[test]
    fn a_formula_in_an_unlocked_cell_is_flagged() {
        let mut w = wb(&[
            ("A1", f("1+1")),
            ("A2", f("1+1")),
            ("A3", Cell::number(1.0)),
        ]);
        w.styles = Styles {
            xfs: vec![
                Xf::default(),
                Xf {
                    unlocked: true,
                    ..Xf::default()
                },
            ],
            ..Styles::default()
        };
        for a1 in ["A1", "A3"] {
            let (r, c) = crate::sheet::parse_cell_name(a1).unwrap();
            w.sheets[0].cells.get_mut(&(r, c)).unwrap().style = 1;
        }
        assert_eq!(rules(&w, "A1"), vec![Rule::UnlockedFormula]);
        assert!(rules(&w, "A2").is_empty(), "locked");
        assert!(rules(&w, "A3").is_empty(), "a constant is never flagged");
    }

    #[test]
    fn a_rule_that_is_off_flags_nothing() {
        let mut w = wb(&[("A1", err("#DIV/0!")), ("B1", Cell::text("1"))]);
        w.audit_rules.error_value = false;
        w.audit_rules.number_as_text = false;
        assert!(rules(&w, "A1").is_empty());
        assert!(rules(&w, "B1").is_empty());
    }

    #[test]
    fn ignore_error_hides_one_rule_on_its_cells_only() {
        let mut w = wb(&[
            ("A1", err("#DIV/0!")),
            ("A2", err("#N/A")),
            ("B1", Cell::text("1")),
        ]);
        w.sheets[0].ignored_errors.push(IgnoredError {
            sqref: vec![(0, 0, 0, 1)], // A1:B1
            rules: vec![Rule::ErrorValue],
            extra: String::new(),
        });
        assert!(flags(&w, 0, 0, 0).is_empty(), "A1 ignored");
        assert_eq!(flags(&w, 0, 1, 0), vec![Rule::ErrorValue], "A2 outside");
        assert_eq!(
            flags(&w, 0, 0, 1),
            vec![Rule::NumberAsText],
            "B1 is in the range, but another rule"
        );
        assert_eq!(
            check_cell(&w, 0, 0, 0),
            vec![Rule::ErrorValue],
            "check_cell ignores ignoring"
        );
    }

    fn with_ignored(sqref: Vec<(u32, u32, u32, u32)>) -> Workbook {
        let mut w = wb(&[("A1", Cell::number(1.0))]);
        let mut other = Sheet {
            name: "T".into(),
            ..Sheet::default()
        };
        other.ignored_errors.push(IgnoredError {
            sqref: vec![(5, 0, 5, 0)],
            rules: vec![Rule::ErrorValue],
            extra: String::new(),
        });
        w.sheets.push(other);
        w.sheets[0].ignored_errors.push(IgnoredError {
            sqref,
            rules: vec![Rule::ErrorValue],
            extra: String::new(),
        });
        w
    }

    #[test]
    fn inserting_rows_above_moves_ignore_error_with_its_cells() {
        let mut w = with_ignored(vec![(4, 1, 6, 1)]); // B5:B7
        crate::edit::insert_rows(&mut w, 0, 0, 2);
        assert_eq!(w.sheets[0].ignored_errors[0].sqref, vec![(6, 1, 8, 1)]);
        assert_eq!(
            w.sheets[1].ignored_errors[0].sqref,
            vec![(5, 0, 5, 0)],
            "another sheet is not moved"
        );
    }

    #[test]
    fn inserting_columns_moves_ignore_error_sideways() {
        let mut w = with_ignored(vec![(4, 1, 6, 1)]);
        crate::edit::insert_cols(&mut w, 0, 0, 1);
        assert_eq!(w.sheets[0].ignored_errors[0].sqref, vec![(4, 2, 6, 2)]);
    }

    #[test]
    fn deleting_rows_shrinks_or_drops_ignore_error() {
        // Rows 5-6 (0-based 4-5) deleted from B5:B7: one row left, moved up.
        let mut w = with_ignored(vec![(4, 1, 6, 1)]);
        crate::edit::delete_rows(&mut w, 0, 4, 2);
        assert_eq!(w.sheets[0].ignored_errors[0].sqref, vec![(4, 1, 4, 1)]);
        // Everything it covered deleted: the entry goes.
        let mut w = with_ignored(vec![(4, 1, 5, 1)]);
        crate::edit::delete_rows(&mut w, 0, 3, 4);
        assert!(w.sheets[0].ignored_errors.is_empty());
        // A delete below leaves it as it is.
        let mut w = with_ignored(vec![(4, 1, 6, 1)]);
        crate::edit::delete_rows(&mut w, 0, 10, 3);
        assert_eq!(w.sheets[0].ignored_errors[0].sqref, vec![(4, 1, 6, 1)]);
    }

    #[test]
    fn convert_to_number_makes_a_general_number_of_the_text() {
        let mut w = wb(&[
            ("A1", Cell::text("123")),
            ("A2", Cell::text("abc")),
            ("A3", f("1")),
        ]);
        w.styles.xfs = vec![Xf::default()];
        let cell = convert_to_number(&mut w, 0, 0, 0).unwrap();
        assert_eq!(cell.value, CellValue::Number(123.0));
        assert_eq!(cell.style, 0, "a General cell keeps its style");
        assert!(convert_to_number(&mut w, 0, 1, 0).is_none(), "not a number");
        assert!(convert_to_number(&mut w, 0, 2, 0).is_none(), "a formula");
        assert!(convert_to_number(&mut w, 0, 9, 9).is_none(), "no cell");
    }

    #[test]
    fn convert_to_number_drops_the_text_format_and_the_apostrophe_and_keeps_a_percent() {
        let mut w = wb(&[("A1", Cell::text("007")), ("A2", Cell::text("12%"))]);
        let mut text_xf = Xf {
            quote_prefix: true,
            bold: true,
            ..Xf::default()
        };
        text_xf.set_code(Some("@".into()));
        w.styles.xfs = vec![Xf::default(), text_xf];
        w.sheets[0].cells.get_mut(&(0, 0)).unwrap().style = 1;
        let a1 = convert_to_number(&mut w, 0, 0, 0).unwrap();
        let xf = w.styles.xf(a1.style);
        assert_eq!(a1.value, CellValue::Number(7.0));
        assert!(!crate::entry::is_text(&xf) && !xf.quote_prefix);
        assert!(xf.bold, "the rest of the format stays");
        let a2 = convert_to_number(&mut w, 0, 1, 0).unwrap();
        assert_eq!(
            a2.value,
            CellValue::Number(0.12),
            "a percent is its fraction, as typed"
        );
        assert_eq!(w.styles.xf(a2.style).code.as_deref(), Some("0%"));
    }

    #[test]
    fn ignore_adds_one_cell_and_merges_with_its_own_kind() {
        let mut w = wb(&[("A1", err("#N/A"))]);
        let s = &mut w.sheets[0];
        ignore(s, Rule::ErrorValue, 0, 0);
        ignore(s, Rule::ErrorValue, 0, 0); // twice: still one
        ignore(s, Rule::ErrorValue, 4, 1);
        ignore(s, Rule::NumberAsText, 4, 1);
        assert_eq!(s.ignored_errors.len(), 2);
        assert_eq!(s.ignored_errors[0].sqref, vec![(0, 0, 0, 0), (4, 1, 4, 1)]);
        assert_eq!(s.ignored_errors[1].rules, vec![Rule::NumberAsText]);
        // A loaded entry with an unmodelled attribute is left as it was.
        s.ignored_errors.clear();
        s.ignored_errors.push(IgnoredError {
            sqref: vec![(9, 9, 9, 9)],
            rules: vec![Rule::ErrorValue],
            extra: "twoDigitTextYear=\"1\"".into(),
        });
        ignore(s, Rule::ErrorValue, 0, 0);
        assert_eq!(s.ignored_errors.len(), 2);
        assert_eq!(s.ignored_errors[0].sqref, vec![(9, 9, 9, 9)]);
    }

    #[test]
    fn error_values_are_named_as_excel_names_them() {
        assert_eq!(error_title("#DIV/0!"), "Divide by Zero Error");
        assert_eq!(error_title("#REF!"), "Invalid Cell Reference Error");
        assert_eq!(error_title("#VALUE!"), "Error in Value");
        assert_eq!(error_title("#WHATEVER"), "Error in Value");
    }

    #[test]
    fn rule_names_round_trip_through_their_attribute() {
        for r in Rule::ALL {
            assert_eq!(Rule::from_attr(r.attr()), Some(r));
            assert!(!r.title().is_empty() && !r.explanation().is_empty());
        }
        assert_eq!(Rule::from_attr("twoDigitTextYear"), None);
    }
}
