//! Formula auditing (#675): the green indicator on a flagged cell, its smart
//! tag menu (Convert to Number, Copy Formula from Above, Ignore Error, Edit in
//! Formula Bar), and what they do to the sheet. The rules are
//! `gridcore::audit`'s; this holds the view's side: which cells are flagged
//! now, and the edits.
//!
//! A flagged cell is looked up on every paint, so the flags a view has found
//! are kept until an edit moves [`SheetView::edit_gen`] (every undo step and
//! every restore does), and are found a cell at a time, only for the cells
//! drawn.

use crate::dialog::catalog;
use crate::dialog::{ButtonRole, Dialog, DialogOwner};
use crate::sheet_goto::SHEET_PROTECTED;
use crate::sheet_step::SheetStep;
use crate::{DocTab, SheetView, Surface};
use gridcore::audit::{self, Along, Rule};
use gridcore::sheet::cell_name;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Bound;

/// A command of the smart tag's menu, on the active cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuditAct {
    /// Convert to Number: the text of a number stored as text becomes the number.
    ConvertToNumber,
    /// Copy Formula from Above (or Left): the formula the cell's neighbours agree on.
    CopyFormula,
    /// Ignore Error: the first rule that flags the cell stops flagging it.
    IgnoreError,
    /// Edit in Formula Bar: opens the cell's editor.
    EditInFormulaBar,
    /// Help on this error.
    Help,
    /// Formulas > Error Checking: walks the flagged cells.
    ErrorChecking,
}

/// What the view remembers between paints.
#[derive(Default)]
pub(crate) struct AuditState {
    cache: RefCell<FlagCache>,
}

/// The flags found so far, valid for one (`edit_gen`, sheet).
#[derive(Default)]
struct FlagCache {
    key: Option<(u64, usize)>,
    flags: HashMap<(u32, u32), Vec<Rule>>,
}

impl SheetView {
    /// The rules that flag the cell on the active sheet and that Ignore Error
    /// has not hidden, first the one the smart tag names.
    pub(crate) fn cell_flags(&self, row: u32, col: u32) -> Vec<Rule> {
        let key = (self.edit_gen, self.active);
        let mut cache = self.audit.cache.borrow_mut();
        if cache.key != Some(key) {
            cache.key = Some(key);
            cache.flags.clear();
        }
        cache
            .flags
            .entry((row, col))
            .or_insert_with(|| audit::flags(&self.pkg.workbook, self.active, row, col))
            .clone()
    }

    /// The rule the smart tag shows for the active cell, if it is flagged.
    pub(crate) fn flagged_rule(&self) -> Option<Rule> {
        let (r, c) = self.sel;
        self.cell_flags(r, c).first().copied()
    }

    /// The smart tag's heading for the active cell: its error's name, else
    /// the rule's.
    pub(crate) fn smart_tag_title(&self, rule: Rule) -> String {
        self.smart_tag_title_at(rule, self.sel)
    }

    /// [`Self::smart_tag_title`] for the cell at `(r, c)`.
    pub(crate) fn smart_tag_title_at(&self, rule: Rule, (r, c): (u32, u32)) -> String {
        match (rule, self.sheet().cell(r, c).map(|x| &x.value)) {
            (Rule::ErrorValue, Some(gridcore::sheet::CellValue::Error(code))) => {
                audit::error_title(code).to_string()
            }
            _ => rule.title().to_string(),
        }
    }

    /// Which neighbour Copy Formula takes the active cell's formula from,
    /// and the formula as it reads here.
    pub(crate) fn copy_formula_source(&self) -> Option<(Along, String)> {
        let (r, c) = self.sel;
        audit::consistent_formula(&self.pkg.workbook, self.active, r, c)
    }

    /// Convert to Number on the active cell: one undo step. `Ok(false)` when
    /// the cell is not a number stored as text.
    pub(crate) fn audit_convert(&mut self) -> Result<bool, String> {
        if self.sheet().is_protected() {
            return Err(SHEET_PROTECTED.into());
        }
        let (s, (r, c)) = (self.active, self.sel);
        let Some(cell) = audit::convert_to_number(&mut self.pkg.workbook, s, r, c) else {
            return Ok(false);
        };
        self.push_undo_at(s, (r, c, r, c), SheetStep::Named("Convert to Number"));
        self.engine
            .set_cell(&mut self.pkg.workbook, (s, r, c), cell);
        Ok(true)
    }

    /// Copy Formula from Above (Left) on the active cell: one undo step.
    /// `Ok(false)` when the cell is not an inconsistent formula.
    pub(crate) fn audit_copy_formula(&mut self) -> Result<bool, String> {
        if self.sheet().is_protected() {
            return Err(SHEET_PROTECTED.into());
        }
        let Some((_, formula)) = self.copy_formula_source() else {
            return Ok(false);
        };
        let (s, (r, c)) = (self.active, self.sel);
        let style = self.sheet().cell(r, c).map_or(0, |x| x.style);
        let mut cell = gridcore::sheet::Cell::formula(&formula);
        cell.style = style;
        if self.refuses(s, &[(r, c, cell.clone())]) {
            return Err(gridcore::engine::PART_OF_ARRAY.into());
        }
        self.push_undo_at(s, (r, c, r, c), SheetStep::Named("Copy Formula"));
        self.engine
            .set_cell(&mut self.pkg.workbook, (s, r, c), cell);
        Ok(true)
    }

    /// Ignore Error on the active cell: one undo step. `Ok(false)` when
    /// nothing flags it.
    pub(crate) fn audit_ignore(&mut self) -> Result<bool, String> {
        let Some(rule) = self.flagged_rule() else {
            return Ok(false);
        };
        let (s, (r, c)) = (self.active, self.sel);
        self.push_undo_at(s, (r, c, r, c), SheetStep::Named("Ignore Error"));
        audit::ignore(&mut self.pkg.workbook.sheets[s], rule, r, c);
        Ok(true)
    }
}

impl crate::Docxy {
    /// Run a smart tag command on the active sheet's active cell; a refusal
    /// or a word about the result goes to the status line.
    pub(crate) fn sheet_audit_act(&mut self, act: AuditAct, cx: &mut gpui::Context<Self>) {
        if act == AuditAct::EditInFormulaBar {
            self.sheet_begin_edit(None, cx);
            return;
        }
        if act == AuditAct::ErrorChecking {
            if let Some(tab) = self.tabs.get_mut(self.active) {
                if let Err(e) = open_error_checking(tab) {
                    tab.status = e.into();
                }
            }
            cx.notify();
            return;
        }
        let Some(v) = self.active_sheet_mut() else {
            return;
        };
        let rule = v.flagged_rule();
        let done = match act {
            AuditAct::ConvertToNumber => v.audit_convert().map(|d| (d, "Converted to a number")),
            AuditAct::CopyFormula => v.audit_copy_formula().map(|d| (d, "Formula copied")),
            AuditAct::IgnoreError => v.audit_ignore().map(|d| (d, "Error ignored")),
            AuditAct::Help => Ok((false, "")),
            AuditAct::EditInFormulaBar | AuditAct::ErrorChecking => unreachable!("handled above"),
        };
        match (act, done) {
            (_, Err(e)) => self.set_status(e),
            (AuditAct::Help, _) => {
                let what = rule.map_or("There is no error here", |r| r.explanation());
                self.set_status(what);
            }
            (_, Ok((true, said))) => {
                self.mark_sheet_dirty();
                self.set_status(said);
            }
            (_, Ok((false, _))) => self.set_status("Nothing to fix here"),
        }
        cx.notify();
    }
}

/// Error Checking's last word, when the walk has nothing more to show.
pub(crate) const COMPLETE: &str = "The error check is complete for the entire sheet.";

const HELP: &str = "Help on this error";
const IGNORE: &str = "Ignore Error";
const EDIT: &str = "Edit in Formula Bar";
const PREVIOUS: &str = "Previous";
const NEXT: &str = "Next";

/// A range of cells in row-major order, as the sheet's cell map takes one.
type CellRange = (Bound<(u32, u32)>, Bound<(u32, u32)>);

/// The nearest flagged cell in `range` of the sheet's cells (row by row),
/// from the near end, or the far end when `last`.
fn scan(v: &SheetView, range: CellRange, last: bool) -> Option<(u32, u32)> {
    let flagged = |&&(r, c): &&(u32, u32)| !v.cell_flags(r, c).is_empty();
    let mut keys = v.sheet().cells.range(range).map(|(k, _)| k);
    if last {
        keys.rev().find(flagged).copied()
    } else {
        keys.find(flagged).copied()
    }
}

/// The flagged cell the walk from `start` shows after `cur` (the first when
/// `cur` is `None`): no cell of the sheet is looked at but the ones that
/// exist, in order, until one is flagged.
fn walk_next(v: &SheetView, start: (u32, u32), cur: Option<(u32, u32)>) -> Option<(u32, u32)> {
    use Bound::{Excluded, Included, Unbounded};
    match cur {
        Some(cur) if cur <= start => scan(v, (Excluded(cur), Included(start)), false),
        Some(cur) => scan(v, (Excluded(cur), Unbounded), false)
            .or_else(|| scan(v, (Unbounded, Included(start)), false)),
        None => scan(v, (Excluded(start), Unbounded), false)
            .or_else(|| scan(v, (Unbounded, Included(start)), false)),
    }
}

/// The flagged cell before `cur` in the walk from `start`, `None` at its first.
fn walk_previous(v: &SheetView, start: (u32, u32), cur: (u32, u32)) -> Option<(u32, u32)> {
    use Bound::{Excluded, Unbounded};
    if cur <= start {
        scan(v, (Unbounded, Excluded(cur)), true)
            .or_else(|| scan(v, (Excluded(start), Unbounded), true))
    } else {
        scan(v, (Excluded(start), Excluded(cur)), true)
    }
}

fn sheet_view_of(tab: &mut DocTab) -> Result<&mut SheetView, String> {
    match &mut tab.surface {
        Surface::Sheet(v) => Ok(v),
        _ => Err("Error Checking needs a spreadsheet".into()),
    }
}

/// The Error Checking dialog on the flagged cell `cur` of a walk from `start`.
fn error_dialog(v: &SheetView, start: (u32, u32), cur: (u32, u32)) -> Dialog {
    let rule = v.cell_flags(cur.0, cur.1).first().copied();
    let formula = v
        .sheet()
        .cell(cur.0, cur.1)
        .and_then(|c| c.formula.as_deref())
        .map(|f| format!("={f}"));
    let mut lines = vec![format!("Error in cell {}", cell_name(cur.0, cur.1))];
    lines.extend(formula);
    if let Some(rule) = rule {
        lines.push(v.smart_tag_title_at(rule, cur));
        lines.push(rule.explanation().to_string());
    }
    let mut d = Dialog::message(
        catalog::ERROR_CHECKING,
        "Error Checking",
        lines.join("\n"),
        &[
            (HELP, ButtonRole::Accept),
            (IGNORE, ButtonRole::Accept),
            (EDIT, ButtonRole::Accept),
            (PREVIOUS, ButtonRole::Accept),
            (NEXT, ButtonRole::Accept),
            ("Close", ButtonRole::Cancel),
        ],
        DialogOwner::ErrorChecking { start, cur },
    );
    for b in &mut d.buttons {
        b.default = b.label == NEXT;
        if b.label == PREVIOUS {
            b.enabled = walk_previous(v, start, cur).is_some();
        }
    }
    d
}

fn done_dialog() -> Dialog {
    Dialog::message(
        catalog::ERROR_CHECK_DONE,
        "Error Checking",
        COMPLETE.to_string(),
        &[("OK", ButtonRole::Cancel)],
        DialogOwner::ErrorCheckDone,
    )
}

/// Formulas > Error Checking: select the first flagged cell after the active
/// one and show it, or say the check is complete when none is.
pub(crate) fn open_error_checking(tab: &mut DocTab) -> Result<(), String> {
    let v = sheet_view_of(tab)?;
    let start = v.sel;
    match walk_next(v, start, None) {
        Some(cur) => {
            crate::sheet_goto::go_to(v, v.active, (cur.0, cur.1, cur.0, cur.1));
            let d = error_dialog(v, start, cur);
            tab.dialogs.push(d);
        }
        None => tab.dialogs.push(done_dialog()),
    }
    Ok(())
}

/// Show `cur` (or the end of the walk) in place of the open dialog.
fn show(tab: &mut DocTab, start: (u32, u32), cur: Option<(u32, u32)>) -> Result<(), String> {
    let v = sheet_view_of(tab)?;
    let d = match cur {
        Some(cur) => {
            crate::sheet_goto::go_to(v, v.active, (cur.0, cur.1, cur.0, cur.1));
            error_dialog(v, start, cur)
        }
        None => done_dialog(),
    };
    tab.dialogs.pop();
    tab.dialogs.push(d);
    Ok(())
}

/// A press Error Checking handles: Help, Ignore Error, Edit in Formula Bar,
/// Previous and Next. `None` for any other (Close closes through the stack).
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    let DialogOwner::ErrorChecking { start, cur } = top.owner else {
        return None;
    };
    let pressed = |label: &str| {
        top.buttons
            .iter()
            .any(|b| b.label == label && b.enabled && b.label.eq_ignore_ascii_case(button.trim()))
    };
    let (help, ignore, edit, previous, next) = (
        pressed(HELP),
        pressed(IGNORE),
        pressed(EDIT),
        pressed(PREVIOUS),
        pressed(NEXT),
    );
    Some(if help {
        let v = match sheet_view_of(tab) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let what = v
            .cell_flags(cur.0, cur.1)
            .first()
            .map_or("There is no error here", |r| r.explanation());
        tab.status = what.into();
        Ok(())
    } else if ignore {
        ignore_here(tab, start, cur)
    } else if edit {
        let v = match sheet_view_of(tab) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        if v.sheet().is_protected() {
            return Some(Err(SHEET_PROTECTED.into()));
        }
        v.begin_cell_edit(None);
        tab.dialogs.pop();
        Ok(())
    } else if previous {
        match sheet_view_of(tab).map(|v| walk_previous(v, start, cur)) {
            Ok(Some(p)) => show(tab, start, Some(p)),
            Ok(None) => Ok(()),
            Err(e) => Err(e),
        }
    } else if next {
        match sheet_view_of(tab).map(|v| walk_next(v, start, Some(cur))) {
            Ok(n) => show(tab, start, n),
            Err(e) => Err(e),
        }
    } else {
        return None;
    })
}

/// Ignore Error on the cell the dialog shows: it stays while another rule
/// still flags the cell, else the walk goes on.
fn ignore_here(tab: &mut DocTab, start: (u32, u32), cur: (u32, u32)) -> Result<(), String> {
    let v = sheet_view_of(tab)?;
    v.sel = cur;
    v.anchor = cur;
    if !v.audit_ignore()? {
        return Ok(());
    }
    tab.sheet_edited();
    let v = sheet_view_of(tab)?;
    if v.cell_flags(cur.0, cur.1).is_empty() {
        let n = walk_next(v, start, Some(cur));
        show(tab, start, n)
    } else {
        show(tab, start, Some(cur))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Surface, new_sheet_surface};
    use core::prelude::v1::test;
    use gridcore::sheet::{Cell, CellValue};

    /// A new sheet holding `cells`, calculated as typing them would.
    fn view(cells: &[(&str, Cell)]) -> SheetView {
        let Surface::Sheet(mut v) = new_sheet_surface() else {
            panic!("a new sheet surface")
        };
        for (name, cell) in cells {
            let (r, c) = gridcore::sheet::parse_cell_name(name).unwrap();
            v.engine
                .set_cell(&mut v.pkg.workbook, (0, r, c), cell.clone());
        }
        v
    }

    fn at(v: &mut SheetView, a1: &str) {
        let (r, c) = gridcore::sheet::parse_cell_name(a1).unwrap();
        v.sel = (r, c);
        v.anchor = (r, c);
    }

    #[test]
    fn an_error_and_a_number_stored_as_text_are_flagged() {
        let v = view(&[
            ("A1", Cell::formula("1/0")),
            ("B1", Cell::text("123")),
            ("C1", Cell::number(1.0)),
        ]);
        assert_eq!(v.cell_flags(0, 0), vec![Rule::ErrorValue]);
        assert_eq!(v.cell_flags(0, 1), vec![Rule::NumberAsText]);
        assert!(v.cell_flags(0, 2).is_empty());
    }

    #[test]
    fn convert_to_number_writes_the_number_and_undo_brings_the_text_back() {
        let mut v = view(&[("B1", Cell::text("123"))]);
        at(&mut v, "B1");
        assert_eq!(v.audit_convert(), Ok(true));
        assert_eq!(
            v.sheet().cell(0, 1).unwrap().value,
            CellValue::Number(123.0)
        );
        assert!(v.cell_flags(0, 1).is_empty(), "no longer flagged");
        assert!(v.undo_step());
        assert_eq!(
            v.sheet().cell(0, 1).unwrap().value,
            CellValue::Text("123".into())
        );
        assert_eq!(
            v.cell_flags(0, 1),
            vec![Rule::NumberAsText],
            "flagged again"
        );
        assert!(v.redo_step());
        assert_eq!(
            v.sheet().cell(0, 1).unwrap().value,
            CellValue::Number(123.0)
        );
    }

    #[test]
    fn convert_to_number_on_anything_else_does_nothing() {
        let mut v = view(&[("A1", Cell::text("abc")), ("A2", Cell::number(1.0))]);
        for a1 in ["A1", "A2", "Z9"] {
            at(&mut v, a1);
            assert_eq!(v.audit_convert(), Ok(false), "{a1}");
        }
        assert!(v.undo.is_empty(), "no step for nothing");
    }

    #[test]
    fn convert_to_number_is_refused_on_a_protected_sheet() {
        let mut v = view(&[("B1", Cell::text("123"))]);
        at(&mut v, "B1");
        v.pkg.workbook.sheets[0].set_protected(true);
        assert_eq!(v.audit_convert(), Err(SHEET_PROTECTED.to_string()));
        assert_eq!(
            v.sheet().cell(0, 1).unwrap().value,
            CellValue::Text("123".into())
        );
    }

    #[test]
    fn ignore_error_stops_the_flag_and_undo_restores_it() {
        let mut v = view(&[("A1", Cell::formula("1/0")), ("A2", Cell::formula("1/0"))]);
        at(&mut v, "A1");
        assert_eq!(v.audit_ignore(), Ok(true));
        assert!(v.cell_flags(0, 0).is_empty());
        assert_eq!(
            v.cell_flags(1, 0),
            vec![Rule::ErrorValue],
            "A2 still flagged"
        );
        assert_eq!(v.sheet().ignored_errors.len(), 1);
        assert!(v.undo_step());
        assert!(v.sheet().ignored_errors.is_empty());
        assert_eq!(v.cell_flags(0, 0), vec![Rule::ErrorValue]);
        assert!(v.redo_step());
        assert!(v.cell_flags(0, 0).is_empty());
        assert_eq!(v.sheet().ignored_errors.len(), 1);
    }

    #[test]
    fn ignore_error_on_an_unflagged_cell_makes_no_step() {
        let mut v = view(&[("A1", Cell::number(1.0))]);
        at(&mut v, "A1");
        assert_eq!(v.audit_ignore(), Ok(false));
        assert!(v.undo.is_empty());
    }

    #[test]
    fn copy_formula_takes_what_the_neighbours_agree_on() {
        let mut v = view(&[
            ("B1", Cell::formula("A1+1")),
            ("B2", Cell::formula("A2*2")),
            ("B3", Cell::formula("A3+1")),
        ]);
        at(&mut v, "B2");
        assert_eq!(v.cell_flags(1, 1), vec![Rule::Inconsistent]);
        assert_eq!(v.copy_formula_source().map(|(a, _)| a), Some(Along::Column));
        assert_eq!(v.audit_copy_formula(), Ok(true));
        assert_eq!(
            v.sheet().cell(1, 1).unwrap().formula.as_deref(),
            Some("A2+1")
        );
        assert!(v.cell_flags(1, 1).is_empty());
        assert!(v.undo_step());
        assert_eq!(
            v.sheet().cell(1, 1).unwrap().formula.as_deref(),
            Some("A2*2")
        );
    }

    #[test]
    fn the_flags_follow_an_edit_and_the_active_sheet() {
        let mut v = view(&[("B1", Cell::text("5"))]);
        assert_eq!(v.cell_flags(0, 1), vec![Rule::NumberAsText]);
        // An edit that goes through an undo step drops what was remembered.
        at(&mut v, "B1");
        v.push_undo(SheetStep::Named("Test"));
        v.pkg.workbook.sheets[0].set_cell(0, 1, Cell::text("x"));
        assert!(v.cell_flags(0, 1).is_empty());
    }

    #[test]
    fn the_smart_tag_names_an_error_value_as_excel_does() {
        let mut v = view(&[
            ("A1", Cell::formula("1/0")),
            ("B1", Cell::formula("NA()")),
            ("C1", Cell::text("1")),
        ]);
        at(&mut v, "A1");
        assert_eq!(v.smart_tag_title(Rule::ErrorValue), "Divide by Zero Error");
        at(&mut v, "B1");
        assert_eq!(
            v.smart_tag_title(Rule::ErrorValue),
            "Value Not Available Error"
        );
        at(&mut v, "C1");
        assert_eq!(
            v.smart_tag_title(Rule::NumberAsText),
            "Number Stored as Text"
        );
    }

    fn ids(v: &SheetView) -> Vec<String> {
        crate::sheet_menus::smart_tag_menu(v)
            .unwrap()
            .into_iter()
            .map(|m| match m {
                crate::menu::MenuItem::Item(e) => e.id,
                crate::menu::MenuItem::Heading(h) => format!("# {h}"),
                _ => "-".to_string(),
            })
            .collect()
    }

    #[test]
    fn the_smart_tag_menu_offers_what_the_rule_does() {
        let mut v = view(&[
            ("A1", Cell::formula("1/0")),
            ("B1", Cell::text("123")),
            ("C1", Cell::number(1.0)),
        ]);
        at(&mut v, "A1");
        assert_eq!(
            ids(&v),
            [
                "# Divide by Zero Error",
                "error-help",
                "-",
                "ignore-error",
                "edit-in-formula-bar"
            ]
        );
        at(&mut v, "B1");
        assert_eq!(
            ids(&v),
            [
                "# Number Stored as Text",
                "convert-to-number",
                "error-help",
                "-",
                "ignore-error",
                "edit-in-formula-bar"
            ]
        );
        at(&mut v, "C1");
        assert!(
            crate::sheet_menus::smart_tag_menu(&v).is_none(),
            "not flagged"
        );
    }

    #[test]
    fn an_ignored_error_has_no_smart_tag_menu() {
        let mut v = view(&[("B1", Cell::text("123"))]);
        at(&mut v, "B1");
        assert_eq!(v.audit_ignore(), Ok(true));
        assert!(crate::sheet_menus::smart_tag_menu(&v).is_none());
    }

    #[test]
    fn an_inconsistent_formula_offers_to_copy_the_neighbours_formula() {
        let mut v = view(&[
            ("B1", Cell::formula("A1+1")),
            ("B2", Cell::formula("A2*2")),
            ("B3", Cell::formula("A3+1")),
        ]);
        at(&mut v, "B2");
        assert!(ids(&v).contains(&"copy-formula".to_string()));
    }
}
