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

/// Excel's words when a trace has nothing to draw.
const NO_PRECEDENTS: &str = "The Trace Precedents command requires that the active cell contain a formula which refers to other cells.";
const NO_DEPENDENTS: &str = "The Trace Dependents command requires that the active cell be referred to by a formula in the workbook.";
const NO_ERROR: &str =
    "The Trace Error command requires that the active cell contain an error value.";
const NOTHING_MORE: &str = "There is nothing more to trace.";

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
    /// Formulas > Trace Precedents, Trace Dependents, Trace Error: one more
    /// level of arrows for the active cell.
    Trace(TraceKind),
    /// Formulas > Remove Arrows.
    RemoveArrows,
    /// Formulas > Evaluate Formula, and the smart tag's and Error Checking's
    /// Show Calculation Steps: the formula of the active cell, step by step.
    EvaluateFormula,
    /// Ctrl+[ and Ctrl+Shift+{ (`all`: every level): select what the cells
    /// read; Ctrl+] and Ctrl+Shift+}: the formulas that read them.
    Select { kind: Reach, all: bool },
}

/// Which way a selection or trace goes from the active cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    Precedents,
    Dependents,
}

/// What a trace draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TraceKind {
    Precedents,
    Dependents,
    Error,
}

impl AuditAct {
    /// Whether the command changes the sheet, as against reading it or only
    /// drawing over it: refused in Protected View and on a multi-area
    /// selection, which the smart tag does not act on.
    pub(crate) fn edits(self) -> bool {
        matches!(
            self,
            AuditAct::ConvertToNumber | AuditAct::CopyFormula | AuditAct::IgnoreError
        )
    }
}

/// One press of a trace command: the arrows it left for a cell, `level`
/// deep, as they were when it ran.
pub(crate) struct TraceSet {
    pub sheet: usize,
    pub cell: (u32, u32),
    pub kind: TraceKind,
    pub level: u32,
    pub arrows: Vec<audit::Arrow>,
}

/// What the view remembers between paints.
#[derive(Default)]
pub(crate) struct AuditState {
    cache: RefCell<FlagCache>,
    /// The traces drawn, valid while the sheet stands as it was when they
    /// were made (`traces_gen` is its `edit_gen`): arrows are view state,
    /// never saved, and an edit takes them away.
    traces: Vec<TraceSet>,
    traces_gen: u64,
    /// The formula Evaluate Formula is taking apart.
    eval: Option<gridcore::evalsteps::Evaluation>,
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

    /// The traces still drawn: none once an edit has moved the sheet on, and
    /// only the active sheet's.
    pub(crate) fn live_traces(&self) -> impl Iterator<Item = &TraceSet> {
        let live = self.audit.traces_gen == self.edit_gen;
        self.audit
            .traces
            .iter()
            .filter(move |t| live && t.sheet == self.active)
    }

    /// Every arrow of the live traces.
    pub(crate) fn trace_arrows(&self) -> Vec<audit::Arrow> {
        self.live_traces()
            .flat_map(|t| t.arrows.iter().copied())
            .collect()
    }

    /// Remove Arrows.
    pub(crate) fn remove_arrows(&mut self) {
        self.audit.traces.clear();
    }

    /// One press of a trace command on the active cell: a new trace at level
    /// one, or the next level of the one already drawn for the cell. Excel's
    /// refusal, as the status line shows it, when there is nothing to trace
    /// or nothing more of it; the arrows count when it drew.
    pub(crate) fn trace(&mut self, kind: TraceKind) -> Result<usize, String> {
        let (s, (r, c)) = (self.active, self.sel);
        // Arrows of an older state of the sheet, or of another sheet, go.
        if self.audit.traces_gen != self.edit_gen {
            self.audit.traces.clear();
            self.audit.traces_gen = self.edit_gen;
        }
        self.audit.traces.retain(|t| t.sheet == s);
        let found = self
            .audit
            .traces
            .iter()
            .position(|t| t.cell == (r, c) && t.kind == kind);
        let level = found.map_or(1, |i| self.audit.traces[i].level + 1);
        let arrows = match kind {
            TraceKind::Precedents => audit::precedent_arrows(&self.pkg.workbook, s, r, c, level),
            TraceKind::Dependents => audit::dependent_arrows(&mut self.engine, s, r, c, level),
            TraceKind::Error => audit::error_arrows(&self.pkg.workbook, s, r, c),
        };
        let before = found.map_or(0, |i| self.audit.traces[i].arrows.len());
        if arrows.is_empty() && found.is_none() {
            return Err(match kind {
                TraceKind::Precedents => NO_PRECEDENTS,
                TraceKind::Dependents => NO_DEPENDENTS,
                TraceKind::Error => NO_ERROR,
            }
            .into());
        }
        if arrows.len() == before {
            return Err(NOTHING_MORE.into());
        }
        let n = arrows.len();
        let set = TraceSet {
            sheet: s,
            cell: (r, c),
            kind,
            level,
            arrows,
        };
        match found {
            Some(i) => self.audit.traces[i] = set,
            None => self.audit.traces.push(set),
        }
        Ok(n)
    }

    /// Ctrl+[ and its kin: select the precedents or dependents of the
    /// selection, the active cell going to the first found. A precedent on
    /// another sheet is jumped to when the sheet has none of its own.
    pub(crate) fn select_reach(&mut self, kind: Reach, all: bool) -> Result<(), String> {
        use gridcore::edit::{GoSpecial, go_to_special};
        let s = self.active;
        let special = match kind {
            Reach::Precedents => GoSpecial::Precedents { all },
            Reach::Dependents => GoSpecial::Dependents { all },
        };
        let found = go_to_special(
            &self.pkg.workbook,
            s,
            &self.areas_all(),
            self.sel,
            special,
            &[],
        );
        match found {
            Ok(rects) => {
                self.set_areas(&rects);
                Ok(())
            }
            Err(why) => {
                let (r, c) = self.sel;
                let away = (kind == Reach::Precedents)
                    .then(|| {
                        gridcore::formula::direct_precedents(&self.pkg.workbook, s, r, c)
                            .into_iter()
                            .find(|&(si, _)| si != s)
                    })
                    .flatten();
                match away {
                    Some((si, rect)) => {
                        crate::sheet_goto::go_to(self, si, rect);
                        Ok(())
                    }
                    None => Err(why.into()),
                }
            }
        }
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
        match act {
            AuditAct::Trace(kind) => {
                let done = self.active_sheet_mut().map(|v| v.trace(kind));
                match done {
                    Some(Ok(n)) => {
                        self.set_status(format!("{n} arrow{} drawn", if n == 1 { "" } else { "s" }))
                    }
                    Some(Err(e)) => self.set_status(e),
                    None => {}
                }
                cx.notify();
                return;
            }
            AuditAct::RemoveArrows => {
                if let Some(v) = self.active_sheet_mut() {
                    v.remove_arrows();
                }
                cx.notify();
                return;
            }
            AuditAct::EvaluateFormula => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if let Err(e) = open_evaluate(tab) {
                        tab.status = e.into();
                    }
                }
                cx.notify();
                return;
            }
            AuditAct::Select { kind, all } => {
                if let Some(Err(e)) = self.active_sheet_mut().map(|v| v.select_reach(kind, all)) {
                    self.set_status(e);
                }
                cx.notify();
                return;
            }
            _ => {}
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
            AuditAct::EditInFormulaBar
            | AuditAct::ErrorChecking
            | AuditAct::Trace(_)
            | AuditAct::RemoveArrows
            | AuditAct::EvaluateFormula
            | AuditAct::Select { .. } => unreachable!("handled above"),
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
const STEPS: &str = "Show Calculation Steps\u{2026}";
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
            (STEPS, ButtonRole::Accept),
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
        // The steps are for a formula: a number stored as text has none.
        if b.label == STEPS {
            b.enabled = v
                .sheet()
                .cell(cur.0, cur.1)
                .is_some_and(|c| c.formula.is_some());
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
    if top.owner == DialogOwner::EvaluateFormula {
        return evaluate_click(tab, button);
    }
    let DialogOwner::ErrorChecking { start, cur } = top.owner else {
        return None;
    };
    let pressed = |label: &str| {
        top.buttons
            .iter()
            .any(|b| b.label == label && b.enabled && b.label.eq_ignore_ascii_case(button.trim()))
    };
    let (help, steps, ignore, edit, previous, next) = (
        pressed(HELP),
        pressed(STEPS),
        pressed(IGNORE),
        pressed(EDIT),
        pressed(PREVIOUS),
        pressed(NEXT),
    );
    Some(if steps {
        open_evaluate(tab)
    } else if help {
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

const EVALUATE: &str = "Evaluate";
const STEP_IN: &str = "Step In";
const STEP_OUT: &str = "Step Out";
const RESTART: &str = "Restart";

/// `text` with the bytes `span` underlined: each character of it followed by
/// a combining low line.
fn underlined(text: &str, span: &std::ops::Range<usize>) -> String {
    let mut out = String::new();
    for (i, ch) in text.char_indices() {
        out.push(ch);
        if span.contains(&i) {
            out.push('\u{332}');
        }
    }
    out
}

/// The Evaluate Formula dialog for the evaluation the view holds.
fn evaluate_dialog(v: &SheetView) -> Option<Dialog> {
    use crate::dialog::{Control, ControlKind, Value};
    let ev = v.audit.eval.as_ref()?;
    let view = ev.view(&v.pkg.workbook);
    let label = |name: &'static str, caption: &str, text: String| {
        Control::new(name, caption, ControlKind::Label, Value::Text(text))
    };
    let shown = match &view.underline {
        Some(span) => underlined(&view.text, span),
        None => view.text.clone(),
    };
    let mut controls = vec![
        label("reference", "Reference:", view.reference.clone()),
        label("evaluation", "Evaluation:", shown),
    ];
    if let Some(span) = &view.underline {
        controls.push(label(
            "next",
            "Next to evaluate:",
            view.text[span.clone()].to_string(),
        ));
    }
    if view.constant {
        controls.push(label(
            "note",
            "",
            "The cell currently being evaluated contains a constant.".into(),
        ));
    } else if view.volatile {
        controls.push(label(
            "note",
            "",
            "A volatile function may give a different result each time the formula is evaluated."
                .into(),
        ));
    }
    let mut d = Dialog::message(
        catalog::EVALUATE_FORMULA,
        "Evaluate Formula",
        String::new(),
        &[
            (EVALUATE, ButtonRole::Accept),
            (STEP_IN, ButtonRole::Accept),
            (STEP_OUT, ButtonRole::Accept),
            (RESTART, ButtonRole::Accept),
            ("Close", ButtonRole::Cancel),
        ],
        DialogOwner::EvaluateFormula,
    );
    d.text = None;
    d.controls = controls;
    for b in &mut d.buttons {
        b.enabled = match b.label.as_str() {
            EVALUATE => view.underline.is_some(),
            STEP_IN => view.step_in,
            STEP_OUT => view.step_out,
            _ => true,
        };
        b.default = b.label
            == if view.underline.is_some() {
                EVALUATE
            } else {
                RESTART
            };
    }
    Some(d)
}

/// Formulas > Evaluate Formula on the active cell: its formula as it stands,
/// with the part to calculate first underlined.
pub(crate) fn open_evaluate(tab: &mut DocTab) -> Result<(), String> {
    let v = sheet_view_of(tab)?;
    let (s, (r, c)) = (v.active, v.sel);
    v.audit.eval = Some(gridcore::evalsteps::Evaluation::new(
        &v.pkg.workbook,
        s,
        r,
        c,
        v.engine.clock,
    )?);
    let d = evaluate_dialog(v).ok_or("nothing to evaluate")?;
    tab.dialogs.push(d);
    Ok(())
}

/// A press the Evaluate Formula dialog handles; `None` for Close.
fn evaluate_click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    let pressed = |label: &str| {
        top.buttons
            .iter()
            .any(|b| b.label == label && b.enabled && b.label.eq_ignore_ascii_case(button.trim()))
    };
    let (evaluate, step_in, step_out, restart) = (
        pressed(EVALUATE),
        pressed(STEP_IN),
        pressed(STEP_OUT),
        pressed(RESTART),
    );
    if !(evaluate || step_in || step_out || restart) {
        return None;
    }
    let v = match sheet_view_of(tab) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let Some(mut ev) = v.audit.eval.take() else {
        return Some(Err("no evaluation is open".into()));
    };
    let wb = &v.pkg.workbook;
    if evaluate {
        ev.evaluate(wb);
    } else if step_in {
        ev.step_in(wb);
    } else if step_out {
        ev.step_out(wb);
    } else {
        ev.restart();
    }
    v.audit.eval = Some(ev);
    let d = evaluate_dialog(v);
    tab.dialogs.pop();
    tab.dialogs.push(d?);
    Some(Ok(()))
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

/// A trace arrow laid out in pixels, relative to the grid layer the cells
/// are drawn under.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Line {
    pub from: (f32, f32),
    pub to: (f32, f32),
    /// Red, along an error (Trace Error); else blue.
    pub red: bool,
    /// Dashed and black from a worksheet icon: the other end is on another sheet.
    pub off_sheet: bool,
    /// The box round a range the arrow starts from: (x0, y0, x1, y1).
    pub boxed: Option<(f32, f32, f32, f32)>,
    /// Where the worksheet icon sits (its top-left), when an end is off the sheet.
    pub icon: Option<(f32, f32)>,
}

/// The worksheet icon's side.
pub(crate) const ICON: f32 = 16.0;

/// Lay the arrows out for the sheet `active`: `col_x` is a column's left edge
/// (`None` when it is scrolled out of the layer), `row_y` a row's top,
/// `col_w` a column's width and `row_h` a cell's height. An arrow with an end
/// that is not drawn is left out; one with an end on another sheet runs to
/// the worksheet icon beside its cell.
pub(crate) fn layout(
    arrows: &[audit::Arrow],
    active: usize,
    col_x: impl Fn(u32) -> Option<f32>,
    row_y: impl Fn(u32) -> f32,
    col_w: impl Fn(u32) -> f32,
    row_h: f32,
) -> Vec<Line> {
    let mid_left = |r: u32, c: u32| col_x(c).map(|x| (x + 6.0, row_y(r) + row_h / 2.0));
    let mut out = Vec::new();
    for a in arrows {
        let red = a.kind == audit::ArrowKind::Error;
        let (fs, (r1, c1, r2, c2)) = a.from;
        let (ts, tr, tc) = a.to;
        if (fs, r1, c1) == (ts, tr, tc) && (r1, c1) == (r2, c2) {
            continue;
        }
        let line = match (fs == active, ts == active) {
            (true, true) => {
                let (Some(from), Some(to)) = (mid_left(r1, c1), mid_left(tr, tc)) else {
                    continue;
                };
                let boxed = ((r1, c1) != (r2, c2))
                    .then(|| {
                        let (x0, y0) = (col_x(c1)?, row_y(r1));
                        Some((x0, y0, col_x(c2)? + col_w(c2), row_y(r2) + row_h))
                    })
                    .flatten();
                Line {
                    from,
                    to,
                    red,
                    off_sheet: false,
                    boxed,
                    icon: None,
                }
            }
            (false, true) => {
                let Some(to) = mid_left(tr, tc) else { continue };
                let icon = ((to.0 - 6.0 - 30.0).max(0.0), to.1 - ICON / 2.0);
                Line {
                    from: (icon.0 + ICON, icon.1 + ICON / 2.0),
                    to,
                    red,
                    off_sheet: true,
                    boxed: None,
                    icon: Some(icon),
                }
            }
            (true, false) => {
                let (Some(from), Some(x)) = (mid_left(r1, c1), col_x(c1)) else {
                    continue;
                };
                let icon = (x + col_w(c1) + 14.0, from.1 - ICON / 2.0);
                Line {
                    from,
                    to: (icon.0, icon.1 + ICON / 2.0),
                    red,
                    off_sheet: true,
                    boxed: None,
                    icon: Some(icon),
                }
            }
            (false, false) => continue,
        };
        out.push(line);
    }
    out
}

/// The elements that draw the lines over the grid: one canvas for every
/// stroke, and a worksheet icon where an arrow comes from or goes to another
/// sheet. Not interactive.
pub(crate) fn overlay(lines: Vec<Line>) -> Vec<gpui::AnyElement> {
    use gpui::prelude::*;
    use gpui::{PathBuilder, Pixels, Point, canvas, div, point, px};
    if lines.is_empty() {
        return Vec::new();
    }
    let icons: Vec<(f32, f32)> = lines.iter().filter_map(|l| l.icon).collect();
    let strokes = canvas(
        |_, _, _| (),
        move |bounds, (), window, _cx| {
            let at = |x: f32, y: f32| -> Point<Pixels> {
                point(bounds.origin.x + px(x), bounds.origin.y + px(y))
            };
            for l in &lines {
                let color = crate::hsla_u(match (l.off_sheet, l.red) {
                    (true, _) => 0x303030,
                    (false, true) => 0xd00000,
                    (false, false) => 0x1f5fbf,
                });
                let mut stroke = |width: f32, dashed: bool, pts: &[(f32, f32)]| {
                    let mut b = PathBuilder::stroke(px(width));
                    if dashed {
                        b = b.dash_array(&[px(4.0), px(3.0)]);
                    }
                    for (i, &(x, y)) in pts.iter().enumerate() {
                        if i == 0 {
                            b.move_to(at(x, y));
                        } else {
                            b.line_to(at(x, y));
                        }
                    }
                    if let Ok(path) = b.build() {
                        window.paint_path(path, color);
                    }
                };
                stroke(1.5, l.off_sheet, &[l.from, l.to]);
                // The arrowhead, two strokes back from the point.
                let (dx, dy) = (l.to.0 - l.from.0, l.to.1 - l.from.1);
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                let (ux, uy) = (dx / len, dy / len);
                let back = (l.to.0 - ux * 8.0, l.to.1 - uy * 8.0);
                for side in [-1.0, 1.0] {
                    let wing = (back.0 - uy * 4.0 * side, back.1 + ux * 4.0 * side);
                    stroke(1.5, false, &[l.to, wing]);
                }
                // The dot the arrow starts from.
                stroke(5.0, false, &[l.from, (l.from.0 + 0.1, l.from.1)]);
                if let Some((x0, y0, x1, y1)) = l.boxed {
                    stroke(
                        1.0,
                        false,
                        &[(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)],
                    );
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    let mut out = vec![strokes.into_any_element()];
    for (x, y) in icons {
        out.push(
            div()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(ICON))
                .h(px(ICON))
                .flex()
                .items_center()
                .justify_center()
                .bg(crate::hsla_u(0xffffff))
                .border_1()
                .border_color(crate::hsla_u(0x303030))
                .text_size(px(10.))
                .text_color(crate::hsla_u(0x303030))
                .child("\u{25A6}")
                .into_any_element(),
        );
    }
    out
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
                "show-steps",
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

    fn chain() -> SheetView {
        // D1 = SUM(B1:B2) + C1; B1 = A1 * 2; E1 = D1 + 1.
        view(&[
            ("A1", Cell::number(3.0)),
            ("B1", Cell::formula("A1*2")),
            ("B2", Cell::number(5.0)),
            ("C1", Cell::number(1.0)),
            ("D1", Cell::formula("SUM(B1:B2)+C1")),
            ("E1", Cell::formula("D1+1")),
        ])
    }

    fn pairs(v: &SheetView) -> Vec<(String, String)> {
        v.trace_arrows()
            .iter()
            .map(|a| {
                let (r1, c1, r2, c2) = a.from.1;
                let from = if (r1, c1) == (r2, c2) {
                    gridcore::sheet::cell_name(r1, c1)
                } else {
                    format!(
                        "{}:{}",
                        gridcore::sheet::cell_name(r1, c1),
                        gridcore::sheet::cell_name(r2, c2)
                    )
                };
                (from, gridcore::sheet::cell_name(a.to.1, a.to.2))
            })
            .collect()
    }

    fn p(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    #[test]
    fn trace_precedents_draws_one_more_level_each_press() {
        let mut v = chain();
        at(&mut v, "D1");
        assert_eq!(v.trace(TraceKind::Precedents), Ok(2));
        assert_eq!(pairs(&v), vec![p("B1:B2", "D1"), p("C1", "D1")]);
        assert_eq!(v.trace(TraceKind::Precedents), Ok(3));
        assert_eq!(
            pairs(&v),
            vec![p("B1:B2", "D1"), p("C1", "D1"), p("A1", "B1")]
        );
        assert_eq!(
            v.trace(TraceKind::Precedents),
            Err(NOTHING_MORE.to_string()),
            "nothing past A1"
        );
        assert_eq!(v.trace_arrows().len(), 3, "the drawing stands");
    }

    #[test]
    fn trace_dependents_draws_from_the_cell_one_level_each_press() {
        let mut v = chain();
        at(&mut v, "B1");
        assert_eq!(v.trace(TraceKind::Dependents), Ok(1));
        assert_eq!(pairs(&v), vec![p("B1", "D1")]);
        assert_eq!(v.trace(TraceKind::Dependents), Ok(2));
        assert_eq!(pairs(&v), vec![p("B1", "D1"), p("D1", "E1")]);
    }

    #[test]
    fn traces_of_different_cells_and_kinds_stay_together_until_removed() {
        let mut v = chain();
        at(&mut v, "E1");
        v.trace(TraceKind::Precedents).unwrap();
        at(&mut v, "B1");
        v.trace(TraceKind::Dependents).unwrap();
        assert_eq!(pairs(&v), vec![p("D1", "E1"), p("B1", "D1")]);
        v.remove_arrows();
        assert!(v.trace_arrows().is_empty());
    }

    #[test]
    fn a_trace_with_nothing_to_draw_says_why() {
        let mut v = chain();
        at(&mut v, "A1");
        assert_eq!(v.trace(TraceKind::Precedents), Err(NO_PRECEDENTS.into()));
        at(&mut v, "E1");
        assert_eq!(v.trace(TraceKind::Dependents), Err(NO_DEPENDENTS.into()));
        assert_eq!(v.trace(TraceKind::Error), Err(NO_ERROR.into()));
        assert!(v.trace_arrows().is_empty());
    }

    #[test]
    fn trace_error_draws_red_from_where_the_error_starts() {
        let mut v = view(&[
            ("A1", Cell::number(5.0)),
            ("B1", Cell::formula("A1/0")),
            ("C1", Cell::formula("B1+1")),
        ]);
        at(&mut v, "C1");
        assert_eq!(v.trace(TraceKind::Error), Ok(2));
        let kinds: Vec<_> = v.trace_arrows().iter().map(|a| a.kind).collect();
        assert_eq!(
            kinds,
            vec![audit::ArrowKind::Error, audit::ArrowKind::Trace]
        );
    }

    #[test]
    fn an_edit_takes_the_arrows_away() {
        let mut v = chain();
        at(&mut v, "D1");
        v.trace(TraceKind::Precedents).unwrap();
        assert!(!v.trace_arrows().is_empty());
        v.push_undo(SheetStep::Named("Test"));
        v.pkg.workbook.sheets[0].set_cell(0, 2, Cell::number(2.0));
        assert!(v.trace_arrows().is_empty(), "the sheet moved on");
        // A new trace starts from level one again.
        assert_eq!(v.trace(TraceKind::Precedents), Ok(2));
    }

    #[test]
    fn arrows_belong_to_their_sheet() {
        let mut v = chain();
        at(&mut v, "D1");
        v.trace(TraceKind::Precedents).unwrap();
        v.active = 1;
        assert!(v.trace_arrows().is_empty());
        v.active = 0;
        assert_eq!(v.trace_arrows().len(), 2);
    }

    /// The cells the selection covers, whichever way the areas are cut.
    fn selected(v: &SheetView) -> Vec<(u32, u32)> {
        let mut cells: Vec<(u32, u32)> = v
            .areas_all()
            .into_iter()
            .flat_map(|(r1, c1, r2, c2)| {
                (r1..=r2).flat_map(move |r| (c1..=c2).map(move |c| (r, c)))
            })
            .collect();
        cells.sort_unstable();
        cells.dedup();
        cells
    }

    fn cells(names: &[&str]) -> Vec<(u32, u32)> {
        let mut out: Vec<_> = names
            .iter()
            .map(|n| gridcore::sheet::parse_cell_name(n).unwrap())
            .collect();
        out.sort_unstable();
        out
    }

    #[test]
    fn ctrl_bracket_selects_the_direct_precedents_and_shift_all_of_them() {
        let mut v = chain();
        at(&mut v, "D1");
        assert_eq!(v.select_reach(Reach::Precedents, false), Ok(()));
        assert_eq!(selected(&v), cells(&["B1", "B2", "C1"]));
        at(&mut v, "D1");
        v.clear_areas();
        assert_eq!(v.select_reach(Reach::Precedents, true), Ok(()));
        assert_eq!(selected(&v), cells(&["A1", "B1", "B2", "C1"]), "A1 too");
    }

    #[test]
    fn ctrl_bracket_selects_the_direct_dependents_and_shift_all_of_them() {
        let mut v = chain();
        at(&mut v, "B1");
        assert_eq!(v.select_reach(Reach::Dependents, false), Ok(()));
        assert_eq!(selected(&v), cells(&["D1"]));
        at(&mut v, "B1");
        v.clear_areas();
        assert_eq!(v.select_reach(Reach::Dependents, true), Ok(()));
        assert_eq!(selected(&v), cells(&["D1", "E1"]));
    }

    #[test]
    fn selecting_with_nothing_to_select_says_so_and_leaves_the_selection() {
        let mut v = chain();
        at(&mut v, "A1");
        assert!(v.select_reach(Reach::Precedents, false).is_err());
        assert_eq!(v.sel, gridcore::sheet::parse_cell_name("A1").unwrap());
        at(&mut v, "E1");
        assert!(v.select_reach(Reach::Dependents, false).is_err());
    }

    #[test]
    fn ctrl_bracket_goes_to_a_precedent_on_another_sheet() {
        let mut v = view(&[("A1", Cell::formula("Sheet2!B3+1"))]);
        let mut other = gridcore::sheet::Sheet {
            name: "Sheet2".into(),
            ..Default::default()
        };
        other.set_cell(2, 1, Cell::number(7.0));
        v.pkg.workbook.sheets.push(other);
        at(&mut v, "A1");
        assert_eq!(v.select_reach(Reach::Precedents, false), Ok(()));
        assert_eq!(v.active, 1);
        assert_eq!(v.sel, gridcore::sheet::parse_cell_name("B3").unwrap());
    }

    fn lay(arrows: &[audit::Arrow], active: usize) -> Vec<Line> {
        layout(
            arrows,
            active,
            |c| (c < 20).then_some(30.0 + c as f32 * 64.0),
            |r| r as f32 * 21.0,
            |_| 64.0,
            20.0,
        )
    }

    #[test]
    fn a_range_arrow_is_a_box_and_one_line_into_the_cell() {
        let mut v = chain();
        at(&mut v, "D1");
        v.trace(TraceKind::Precedents).unwrap();
        let lines = lay(&v.trace_arrows(), 0);
        assert_eq!(lines.len(), 2);
        // B1:B2 -> D1: the box is the two cells, the line goes to D1's left edge.
        let boxed = lines[0].boxed.unwrap();
        assert_eq!(boxed, (94.0, 0.0, 158.0, 41.0));
        assert_eq!(lines[0].from, (100.0, 10.0));
        assert_eq!(lines[0].to, (30.0 + 3.0 * 64.0 + 6.0, 10.0));
        assert!(lines[1].boxed.is_none(), "C1 is one cell");
        assert!(
            lines
                .iter()
                .all(|l| !l.off_sheet && l.icon.is_none() && !l.red)
        );
    }

    #[test]
    fn an_arrow_from_another_sheet_runs_from_the_worksheet_icon() {
        let a = audit::Arrow {
            from: (1, (4, 2, 4, 2)),
            to: (0, 0, 3),
            kind: audit::ArrowKind::Trace,
        };
        let lines = lay(&[a], 0);
        let l = &lines[0];
        assert!(l.off_sheet);
        let icon = l.icon.unwrap();
        assert_eq!(
            l.from,
            (icon.0 + ICON, icon.1 + ICON / 2.0),
            "from the icon"
        );
        assert_eq!(l.to, (30.0 + 3.0 * 64.0 + 6.0, 10.0));
        // And one to a dependent on another sheet ends at its icon.
        let b = audit::Arrow {
            from: (0, (0, 1, 0, 1)),
            to: (1, 7, 7),
            kind: audit::ArrowKind::Trace,
        };
        let lines = lay(&[b], 0);
        assert!(lines[0].off_sheet && lines[0].icon.is_some());
        assert_eq!(lines[0].from, (30.0 + 64.0 + 6.0, 10.0));
        // An arrow between two other sheets is not drawn here.
        let c = audit::Arrow {
            from: (1, (0, 0, 0, 0)),
            to: (2, 1, 1),
            kind: audit::ArrowKind::Trace,
        };
        assert!(lay(&[c], 0).is_empty());
    }

    #[test]
    fn an_arrow_to_a_scrolled_off_column_is_left_out_and_red_is_kept() {
        let a = audit::Arrow {
            from: (0, (0, 0, 0, 0)),
            to: (0, 2, 25),
            kind: audit::ArrowKind::Error,
        };
        assert!(lay(&[a], 0).is_empty(), "column 25 is not drawn");
        let b = audit::Arrow {
            from: (0, (0, 0, 0, 0)),
            to: (0, 2, 2),
            kind: audit::ArrowKind::Error,
        };
        assert!(lay(&[b], 0)[0].red);
    }

    #[test]
    fn the_trace_commands_are_not_todo_and_run_through_the_ribbon_table() {
        use crate::sheet_ribbon::{resolve_on, tab_def};
        let formulas = tab_def(crate::RibbonTab::Formulas);
        for (label, kind) in [
            ("Trace Precedents", Some(TraceKind::Precedents)),
            ("Trace Dependents", Some(TraceKind::Dependents)),
            ("Trace Error", Some(TraceKind::Error)),
            ("Remove Arrows", None),
        ] {
            let c = resolve_on(formulas, "Formulas", label, |_| false)
                .unwrap_or_else(|e| panic!("{label}: {e:?}"));
            let want = kind.map_or(AuditAct::RemoveArrows, AuditAct::Trace);
            assert_eq!(c.act, crate::SheetAct::Audit(want), "{label}");
        }
    }
}
