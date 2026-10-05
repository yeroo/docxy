//! Data validation in the spreadsheet (#687, #689): the alert a typed entry
//! that breaks its cell's rule raises, and Excel's Data Validation dialog.
//!
//! Both sit on the tab's [`crate::dialog::DialogStack`], so the harness's
//! `dialog-set`/`dialog-click` drive them. The alert (`data-validation-alert`)
//! offers Retry/Cancel for a Stop rule, Yes/No/Cancel for a Warning and
//! OK/Cancel for an Information one; the entry waits on the view
//! ([`crate::SheetView::dv_pending`]) until it is answered. The dialog
//! (`data-validation`) has the Settings, Input Message and Error Alert tabs;
//! OK and Clear All apply as one undo step, a refusal keeps it open.

use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::{DocTab, Surface};
use gridcore::sheet::{AlertStyle, DataValidation};
use gridcore::validation::{
    ALERT_STYLES, DialogBoxes, KINDS, MESSAGE_MAX, OPERATORS, TITLE_MAX, Violation, first_label,
    takes_operator, takes_two,
};

const PROTECTED: &str =
    "The sheet is protected: unprotect it (Review › Protect Sheet) to change its data validation.";

const NOT_WRITABLE: &str = "This sheet's part can't hold data validation, so the rule could not be saved: nothing was changed.";

// ---- the alert -------------------------------------------------------------

/// The alert dialog for `v`: the rule's title and message, and its style's
/// buttons (a Warning's default is No, as in Excel).
pub(crate) fn alert_dialog(v: &Violation) -> Dialog {
    let buttons: &[(&str, ButtonRole)] = match v.style {
        AlertStyle::Stop => &[
            ("Retry", ButtonRole::Accept),
            ("Cancel", ButtonRole::Cancel),
        ],
        AlertStyle::Warning => &[
            ("Yes", ButtonRole::Accept),
            ("No", ButtonRole::Accept),
            ("Cancel", ButtonRole::Cancel),
        ],
        AlertStyle::Information => &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
    };
    let mut d = Dialog::message(
        "data-validation-alert",
        &v.title,
        v.message.clone(),
        buttons,
        DialogOwner::DataValidationAlert,
    );
    if v.style == AlertStyle::Warning {
        d.buttons = d
            .buttons
            .into_iter()
            .map(|b| Button {
                default: b.label == "No",
                ..b
            })
            .collect();
    }
    d
}

/// A press on the alert: Yes and OK let the entry in (and make the move the
/// key or click would have made), Retry and No go back to the editor with
/// its text, Cancel drops the entry. `None` for any other dialog.
pub(crate) fn alert_click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    if !matches!(top.owner, DialogOwner::DataValidationAlert) {
        return None;
    }
    let label = top
        .buttons
        .iter()
        .find(|b| b.label.eq_ignore_ascii_case(button.replace('&', "").trim()))
        .map(|b| b.label.clone())?;
    tab.dialogs.pop();
    let Surface::Sheet(v) = &mut tab.surface else {
        return Some(Ok(()));
    };
    match label.as_str() {
        "Yes" | "OK" => {
            if v.accept_pending() {
                tab.set_dirty();
            }
        }
        "Cancel" => {
            v.dv_pending = None;
            v.editing = None;
            v.end_cell_edit();
        }
        // Retry, No: the editor stays as it was.
        _ => v.dv_pending = None,
    }
    Some(Ok(()))
}

// ---- the dialog ------------------------------------------------------------

/// The index of the choice a dropdown control has picked (0 when none).
fn choice(d: &Dialog, name: &str) -> usize {
    match d.value(name) {
        Some(Value::Choice(Some(i))) => *i,
        _ => 0,
    }
}

fn text(d: &Dialog, name: &str) -> String {
    match d.value(name) {
        Some(Value::Text(t)) => t.clone(),
        _ => String::new(),
    }
}

fn checked(d: &Dialog, name: &str) -> bool {
    d.value(name) == Some(&Value::Bool(true))
}

/// Show what the "Allow" and "Data" choices make relevant: the operator for
/// bounded types, the bounds' boxes with their captions, Ignore blank and In-cell
/// dropdown; and hold a title or message to Excel's length limits.
fn react(d: &mut Dialog, i: usize, _before: &Value) {
    let kind = KINDS[choice(d, "allow").min(KINDS.len() - 1)].0;
    let op = OPERATORS[choice(d, "operator").min(OPERATORS.len() - 1)].0;
    let any = kind.is_empty();
    for c in &mut d.controls {
        match c.name {
            "operator" => c.visible = takes_operator(kind),
            "first" => {
                c.visible = !any;
                c.label = first_label(kind, op).into();
            }
            "second" => c.visible = takes_two(kind, op),
            "ignore-blank" => c.visible = !any,
            "dropdown" => c.visible = kind == "list",
            _ => {}
        }
    }
    let limit = match d.controls.get(i).map(|c| c.name) {
        Some("input-title" | "error-title") => TITLE_MAX,
        Some("input-message" | "error-message") => MESSAGE_MAX,
        _ => return,
    };
    if let Some(Control {
        value: Value::Text(t),
        ..
    }) = d.controls.get_mut(i)
    {
        if t.chars().count() > limit {
            *t = t.chars().take(limit).collect();
        }
    }
}

/// The Data Validation dialog over the selection, showing the rule on its
/// first cell (Excel's defaults when it has none).
pub(crate) fn dialog(tab: &DocTab) -> Result<Dialog, String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("Data Validation needs a spreadsheet".into());
    };
    if v.sheet().is_protected() {
        return Err(PROTECTED.into());
    }
    let range = v.range();
    let current = gridcore::validation::validation_at(v.sheet(), range.0, range.1);
    let b = DialogBoxes::of(current, (range.0, range.1), v.pkg.workbook.date1904);
    let mut d = Dialog::message(
        "data-validation",
        "Data Validation",
        String::new(),
        &[
            ("OK", ButtonRole::Accept),
            ("Clear All", ButtonRole::Apply),
            ("Cancel", ButtonRole::Cancel),
        ],
        DialogOwner::DataValidation {
            sheet: v.active,
            range,
        },
    );
    d.text = None;
    d.tabs = vec![
        "Settings".into(),
        "Input Message".into(),
        "Error Alert".into(),
    ];
    let page = |mut c: Control, p: usize| {
        c.page = Some(p);
        c
    };
    let dropdown = |name, label: &str, items: Vec<String>, at: usize| {
        let mut c = Control::new(name, label, ControlKind::Dropdown, Value::Choice(Some(at)));
        c.items = items;
        c
    };
    let text = |name, label: &str, v: &str| {
        Control::new(name, label, ControlKind::Text, Value::Text(v.into()))
    };
    let check = |name, label: &str, on: bool| {
        Control::new(name, label, ControlKind::Checkbox, Value::Bool(on))
    };
    d.controls = vec![
        page(
            dropdown(
                "allow",
                "Allow:",
                KINDS.iter().map(|k| k.1.to_string()).collect(),
                b.kind,
            ),
            0,
        ),
        page(
            dropdown(
                "operator",
                "Data:",
                OPERATORS.iter().map(|o| o.1.to_string()).collect(),
                b.operator,
            ),
            0,
        ),
        page(text("first", "Minimum:", &b.first), 0),
        page(text("second", "Maximum:", &b.second), 0),
        page(check("ignore-blank", "Ignore blank", b.ignore_blank), 0),
        page(check("dropdown", "In-cell dropdown", b.dropdown), 0),
        page(
            check(
                "apply-all",
                "Apply these changes to all other cells with the same settings",
                false,
            ),
            0,
        ),
        page(
            check(
                "show-input",
                "Show input message when cell is selected",
                b.show_input,
            ),
            1,
        ),
        page(text("input-title", "Title:", &b.prompt_title), 1),
        page(text("input-message", "Input message:", &b.prompt), 1),
        page(
            check(
                "show-error",
                "Show error alert after invalid data is entered",
                b.show_error,
            ),
            2,
        ),
        page(
            dropdown(
                "error-style",
                "Style:",
                ALERT_STYLES.iter().map(|s| s.1.to_string()).collect(),
                b.style,
            ),
            2,
        ),
        page(text("error-title", "Title:", &b.error_title), 2),
        page(text("error-message", "Error message:", &b.error), 2),
    ];
    d.react = Some(crate::dialog::Reaction(react));
    react(&mut d, usize::MAX, &Value::Bool(false));
    d.mark_opened();
    Ok(d)
}

/// The rule OK applies (its ranges left empty, its formulas written for the
/// cell the dialog opened on), or why the boxes can't make one.
pub(crate) fn rule(
    d: &Dialog,
    ctx: &gridcore::entry::EntryCtx,
    current: Option<&DataValidation>,
    at: (u32, u32),
) -> Result<DataValidation, String> {
    DialogBoxes {
        kind: choice(d, "allow"),
        operator: choice(d, "operator"),
        first: text(d, "first"),
        second: text(d, "second"),
        ignore_blank: checked(d, "ignore-blank"),
        dropdown: checked(d, "dropdown"),
        show_input: checked(d, "show-input"),
        prompt_title: text(d, "input-title"),
        prompt: text(d, "input-message"),
        show_error: checked(d, "show-error"),
        style: choice(d, "error-style"),
        error_title: text(d, "error-title"),
        error: text(d, "error-message"),
    }
    .rule(ctx, current, at)
}

/// A press the Data Validation dialog handles itself: OK and Clear All.
/// `None` for any other press or dialog (Cancel closes through the stack).
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    let DialogOwner::DataValidation { sheet, range } = top.owner else {
        return None;
    };
    let press = button.replace('&', "");
    let press = press.trim();
    let clear = press.eq_ignore_ascii_case("Clear All");
    if !clear && !press.eq_ignore_ascii_case("OK") {
        return None;
    }
    let apply_all = checked(top, "apply-all");
    let rule = if clear {
        None
    } else {
        let Surface::Sheet(v) = &tab.surface else {
            return Some(Err("Data Validation needs a spreadsheet".into()));
        };
        // The workbook's own context: a fixed decimal point shifts what is
        // typed into a cell, not a bound typed into this dialog.
        let ctx = gridcore::entry::entry_ctx(&v.pkg.workbook, v.engine.clock);
        let current = v
            .pkg
            .workbook
            .sheets
            .get(sheet)
            .and_then(|s| gridcore::validation::validation_at(s, range.0, range.1));
        match rule(top, &ctx, current, (range.0, range.1)) {
            Ok(r) => Some(r),
            Err(e) => return Some(Err(e)),
        }
    };
    let Surface::Sheet(v) = &mut tab.surface else {
        return Some(Err("Data Validation needs a spreadsheet".into()));
    };
    // A part that can't hold `<dataValidations>` would lose the rule on save.
    if rule.is_some() && !v.pkg.takes_validations(sheet) {
        return Some(Err(NOT_WRITABLE.into()));
    }
    if v.pkg
        .workbook
        .sheets
        .get(sheet)
        .is_some_and(|s| s.is_protected())
    {
        return Some(Err(PROTECTED.into()));
    }
    let snap = v.snapshot();
    let before = v.pkg.workbook.sheets[sheet].validations.clone();
    {
        let s = &mut v.pkg.workbook.sheets[sheet];
        match &rule {
            Some(r) => gridcore::validation::set_validation(s, range, r, apply_all),
            None => gridcore::validation::clear_all_validation(s, range, apply_all),
        }
    }
    tab.dialogs.pop();
    if before == v.pkg.workbook.sheets[sheet].validations {
        tab.status = "Data validation unchanged".into();
        return Some(Ok(()));
    }
    v.push_undo_snapshot(snap);
    v.prune_circles();
    tab.set_dirty();
    tab.status = if clear {
        "Data validation cleared"
    } else {
        "Data validation applied"
    }
    .into();
    Some(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Kind, SheetView, new_sheet_surface};
    use ctlcore::json::Json;
    use gridcore::sheet::{Cell, CellValue};

    fn tab() -> DocTab {
        DocTab {
            kind: Kind::Xlsx,
            title: "Scores.xlsx".into(),
            path: None,
            surface: new_sheet_surface(),
            dirty: false,
            status: "".into(),
            comments: vec![],
            tracked_comment_ids: Default::default(),
            comments_removed_all: false,
            used_comment_ids: Default::default(),
            pkg: None,
            notes: vec![],
            markdown: false,
            hf_edit: None,
            bundle_html: None,
            load_failed: false,
            dialogs: crate::dialog::DialogStack::default(),
            access: crate::open_mode::Access::default(),
            last_hot: Default::default(),
            converted_docx: None,
            pending_conversion: false,
            mail: Default::default(),
            import: Default::default(),
        }
    }

    fn view(t: &mut DocTab) -> &mut SheetView {
        match &mut t.surface {
            Surface::Sheet(v) => v,
            _ => panic!("a sheet"),
        }
    }

    fn at(name: &str) -> (u32, u32) {
        gridcore::sheet::parse_cell_name(name).unwrap()
    }

    fn put(t: &mut DocTab, name: &str, cell: Cell) {
        let (r, c) = at(name);
        let v = view(t);
        let s = v.active;
        v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), cell);
    }

    fn value(t: &mut DocTab, name: &str) -> CellValue {
        let (r, c) = at(name);
        view(t)
            .sheet()
            .cell(r, c)
            .map(|c| c.value.clone())
            .unwrap_or_default()
    }

    /// The issue's book: B2 = 50 and a whole-number rule on B2:B10.
    fn book(style: AlertStyle) -> DocTab {
        let mut t = tab();
        put(&mut t, "B2", Cell::number(50.0));
        view(&mut t).pkg.workbook.sheets[0]
            .validations
            .push(DataValidation {
                ranges: vec![(1, 1, 9, 1)],
                kind: "whole".into(),
                operator: "between".into(),
                formula1: "10".into(),
                formula2: "90".into(),
                allow_blank: true,
                show_error: true,
                error_style: style,
                error_title: "Score".into(),
                error: "10 to 90 only".into(),
                ..Default::default()
            });
        t
    }

    fn select(t: &mut DocTab, name: &str) {
        let v = view(t);
        v.sel = at(name);
        v.anchor = v.sel;
    }

    /// Type `text` into the selected cell and commit it as Enter does; the
    /// alert it raises goes on the tab's dialogs, as `sheet_show_alert` does.
    fn enter(t: &mut DocTab, text: &str) -> Option<bool> {
        let v = view(t);
        v.begin_cell_edit(Some(String::new()));
        v.edit_caret = 0;
        for ch in text.chars() {
            v.edit_type(&ch.to_string());
        }
        let res = v.commit_and_move(1, 0);
        if let Some(p) = &view(t).dv_pending {
            let d = alert_dialog(&p.violation);
            t.dialogs.push(d);
            view(t).entry_error = None;
        }
        res
    }

    fn press(t: &mut DocTab, button: &str) -> Result<(), String> {
        crate::dialog_host::dialog_click(t, button)
    }

    fn set(t: &mut DocTab, control: &str, value: Json) {
        t.dialogs
            .set(control, &Json::obj(vec![("value", value)]))
            .unwrap();
    }

    fn ranges(t: &mut DocTab) -> Vec<Vec<(u32, u32, u32, u32)>> {
        view(t)
            .sheet()
            .validations
            .iter()
            .map(|d| d.ranges.clone())
            .collect()
    }

    // ---- #687 ----

    #[test]
    fn a_stop_alert_refuses_the_issue_entries() {
        let mut t = book(AlertStyle::Stop);
        select(&mut t, "B2");
        assert_eq!(enter(&mut t, "250"), None);
        let top = t.dialogs.top().expect("the alert is up");
        assert_eq!(
            (top.id, top.title.as_str()),
            ("data-validation-alert", "Score")
        );
        assert_eq!(top.text.as_deref(), Some("10 to 90 only"));
        let labels: Vec<_> = top.buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["Retry", "Cancel"]);
        assert_eq!(value(&mut t, "B2"), CellValue::Number(50.0));
        assert_eq!(view(&mut t).sel, at("B2"), "nothing moved");

        // Retry: the alert closes, the editor keeps its text.
        press(&mut t, "Retry").unwrap();
        assert!(t.dialogs.top().is_none());
        assert_eq!(view(&mut t).editing.as_deref(), Some("250"));
        assert_eq!(value(&mut t, "B2"), CellValue::Number(50.0));

        // Commit again, then Cancel: the edit is dropped, the old value kept.
        let v = view(&mut t);
        assert_eq!(v.commit_and_move(1, 0), None);
        let d = alert_dialog(&v.dv_pending.clone().unwrap().violation);
        t.dialogs.push(d);
        press(&mut t, "Cancel").unwrap();
        assert!(view(&mut t).editing.is_none());
        assert_eq!(value(&mut t, "B2"), CellValue::Number(50.0));

        // 45.5 into B3 is refused as well.
        select(&mut t, "B3");
        assert_eq!(enter(&mut t, "45.5"), None);
        assert_eq!(value(&mut t, "B3"), CellValue::Empty);
        press(&mut t, "Cancel").unwrap();
        // A good entry goes in and moves.
        select(&mut t, "B3");
        assert_eq!(enter(&mut t, "45"), Some(true));
        assert_eq!(value(&mut t, "B3"), CellValue::Number(45.0));
        assert_eq!(view(&mut t).sel, at("B4"));
    }

    #[test]
    fn a_warning_asks_yes_no_and_an_information_alert_lets_the_entry_in() {
        let mut t = book(AlertStyle::Warning);
        select(&mut t, "B2");
        enter(&mut t, "250");
        let top = t.dialogs.top().unwrap();
        let labels: Vec<_> = top.buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["Yes", "No", "Cancel"]);
        assert!(
            top.buttons
                .iter()
                .find(|b| b.default)
                .is_some_and(|b| b.label == "No")
        );
        press(&mut t, "No").unwrap();
        assert_eq!(view(&mut t).editing.as_deref(), Some("250"));
        assert_eq!(value(&mut t, "B2"), CellValue::Number(50.0));
        let v = view(&mut t);
        v.commit_and_move(1, 0);
        let d = alert_dialog(&v.dv_pending.clone().unwrap().violation);
        t.dialogs.push(d);
        press(&mut t, "Yes").unwrap();
        assert_eq!(value(&mut t, "B2"), CellValue::Number(250.0));
        assert_eq!(view(&mut t).sel, at("B3"), "Yes makes the move");
        assert!(t.dirty);

        let mut t = book(AlertStyle::Information);
        select(&mut t, "B2");
        enter(&mut t, "250");
        let labels: Vec<_> = t
            .dialogs
            .top()
            .unwrap()
            .buttons
            .iter()
            .map(|b| b.label.clone())
            .collect();
        assert_eq!(labels, ["OK", "Cancel"]);
        press(&mut t, "OK").unwrap();
        assert_eq!(value(&mut t, "B2"), CellValue::Number(250.0));
        // One undo step puts the old value back.
        assert!(view(&mut t).undo_step());
        assert_eq!(value(&mut t, "B2"), CellValue::Number(50.0));
    }

    #[test]
    fn show_error_off_lets_anything_in_and_ctrl_enter_checks_the_active_cell_once() {
        let mut t = book(AlertStyle::Stop);
        view(&mut t).pkg.workbook.sheets[0].validations[0].show_error = false;
        select(&mut t, "B2");
        assert_eq!(enter(&mut t, "250"), Some(true));
        assert_eq!(value(&mut t, "B2"), CellValue::Number(250.0));

        // Ctrl+Enter: the typed entry is checked against the active cell's
        // rule, then fills the range.
        let mut t = book(AlertStyle::Stop);
        let v = view(&mut t);
        v.anchor = at("B2");
        v.sel = at("B4");
        v.begin_cell_edit(Some(String::new()));
        v.edit_caret = 0;
        v.edit_type("5");
        assert!(!v.commit_edit_to_selection());
        let p = v.dv_pending.clone().expect("held for its alert");
        assert_eq!(p.cells.len(), 3);
        assert_eq!(p.then, None);
        t.dialogs.push(alert_dialog(&p.violation));
        press(&mut t, "Cancel").unwrap();
        assert_eq!(value(&mut t, "B3"), CellValue::Empty);
    }

    // ---- #688 ----

    #[test]
    fn pasting_an_unvalidated_cell_splits_the_rule_and_saves_it() {
        let mut t = book(AlertStyle::Stop);
        put(&mut t, "H2", Cell::number(1000.0));
        let v = view(&mut t);
        v.anchor = at("H2");
        v.sel = at("H2");
        let clip = v.grid_clip(false).unwrap();
        v.anchor = at("B6");
        v.sel = at("B6");
        assert_eq!(v.paste_grid_clip(&clip), Ok(crate::GridPasted::Done));
        assert_eq!(value(&mut t, "B6"), CellValue::Number(1000.0));
        assert_eq!(ranges(&mut t), vec![vec![(1, 1, 4, 1), (6, 1, 9, 1)]]);
        let saved = gridcore::xlsx::save_xlsx(&view(&mut t).pkg);
        let re = gridcore::xlsx::load_xlsx(&saved).unwrap();
        assert_eq!(
            re.workbook.sheets[0].validations[0].ranges,
            vec![(1, 1, 4, 1), (6, 1, 9, 1)]
        );
        // One undo step restores the cell and the rule.
        assert_eq!(view(&mut t).undo.len(), 1, "one undo step");
        assert!(view(&mut t).undo_step());
        assert_eq!(ranges(&mut t), vec![vec![(1, 1, 9, 1)]]);
    }

    #[test]
    fn a_validated_cell_pastes_its_rule_and_a_cut_moves_it() {
        let mut t = book(AlertStyle::Stop);
        let v = view(&mut t);
        v.anchor = at("B2");
        v.sel = at("B2");
        let clip = v.grid_clip(false).unwrap();
        v.anchor = at("F4");
        v.sel = at("F4");
        v.paste_grid_clip(&clip).unwrap();
        let (r, c) = at("F4");
        assert!(gridcore::validation::validation_at(v.sheet(), r, c).is_some());

        // Cut B3, paste at B13: the rule goes from B3 to B13.
        let v = view(&mut t);
        v.anchor = at("B3");
        v.sel = at("B3");
        let clip = v.grid_clip(true).unwrap();
        v.anchor = at("B13");
        v.sel = at("B13");
        assert_eq!(v.paste_grid_clip(&clip), Ok(crate::GridPasted::Done));
        let sheet = view(&mut t).sheet();
        assert!(gridcore::validation::validation_at(sheet, 12, 1).is_some());
        assert!(gridcore::validation::validation_at(sheet, 2, 1).is_none());
        assert!(view(&mut t).undo_step());
        let sheet = view(&mut t).sheet();
        assert!(gridcore::validation::validation_at(sheet, 2, 1).is_some());
        assert!(gridcore::validation::validation_at(sheet, 12, 1).is_none());
    }

    // ---- #689 ----

    fn open(t: &mut DocTab) {
        let d = dialog(t).unwrap();
        t.dialogs.push(d);
    }

    #[test]
    fn the_dialog_creates_a_rule_as_one_undo_step() {
        let mut t = tab();
        let v = view(&mut t);
        v.anchor = at("B2");
        v.sel = at("B10");
        open(&mut t);
        assert_eq!(t.dialogs.top_id(), "data-validation");
        set(&mut t, "allow", Json::Str("Whole number".into()));
        set(&mut t, "operator", Json::Str("between".into()));
        set(&mut t, "first", Json::Str("10".into()));
        set(&mut t, "second", Json::Str("90".into()));
        t.dialogs.select_tab("Error Alert").unwrap();
        set(&mut t, "error-style", Json::Str("Warning".into()));
        set(&mut t, "error-title", Json::Str("Score".into()));
        set(&mut t, "error-message", Json::Str("10 to 90 only".into()));
        press(&mut t, "OK").unwrap();
        assert!(t.dialogs.top().is_none());
        let dv = &view(&mut t).sheet().validations[0];
        assert_eq!(dv.ranges, vec![(1, 1, 9, 1)]);
        assert_eq!(
            (dv.kind.as_str(), dv.formula1.as_str(), dv.formula2.as_str()),
            ("whole", "10", "90")
        );
        assert_eq!(dv.error_style, AlertStyle::Warning);
        assert!(dv.show_error && dv.allow_blank);
        assert_eq!(view(&mut t).undo.len(), 1);
        assert!(t.dirty);
        // It saves and reloads.
        let saved = gridcore::xlsx::save_xlsx(&view(&mut t).pkg);
        let re = gridcore::xlsx::load_xlsx(&saved).unwrap();
        assert_eq!(re.workbook.sheets[0].validations[0].error_title, "Score");
        // And the entry is now checked.
        select(&mut t, "B2");
        assert_eq!(enter(&mut t, "250"), None);
        press(&mut t, "Cancel").unwrap();
        assert!(view(&mut t).undo_step());
        assert!(view(&mut t).sheet().validations.is_empty());
    }

    #[test]
    fn the_dialog_follows_its_allow_choice_and_holds_the_limits() {
        let mut t = tab();
        select(&mut t, "B2");
        open(&mut t);
        let top = t.dialogs.top().unwrap();
        let visible = |d: &crate::dialog::Dialog, name: &str| {
            d.controls.iter().find(|c| c.name == name).unwrap().visible
        };
        // "Any value": no bounds.
        assert!(!visible(top, "first") && !visible(top, "operator"));
        set(&mut t, "allow", Json::Str("List".into()));
        let top = t.dialogs.top().unwrap();
        assert!(visible(top, "first") && visible(top, "dropdown") && !visible(top, "operator"));
        set(&mut t, "allow", Json::Str("Date".into()));
        let top = t.dialogs.top().unwrap();
        assert!(visible(top, "operator") && visible(top, "second") && !visible(top, "dropdown"));
        set(&mut t, "operator", Json::Str("greater than".into()));
        assert!(!visible(t.dialogs.top().unwrap(), "second"));
        // Excel's limits: a 32-character title, a 255-character message.
        set(&mut t, "first", Json::Str("43831".into()));
        t.dialogs.select_tab("Input Message").unwrap();
        set(&mut t, "input-title", Json::Str("t".repeat(40)));
        set(&mut t, "input-message", Json::Str("m".repeat(300)));
        let top = t.dialogs.top().unwrap();
        assert_eq!(
            rule(top, &Default::default(), None, (1, 1))
                .map(|r| (r.prompt_title.len(), r.prompt.unwrap().len())),
            Ok((32, 255))
        );
        // A bound missing keeps the dialog open and says why.
        t.dialogs.select_tab("Settings").unwrap();
        set(&mut t, "first", Json::Str(String::new()));
        assert!(press(&mut t, "OK").is_err());
        assert_eq!(t.dialogs.top_id(), "data-validation");
    }

    #[test]
    fn apply_to_all_and_clear_all() {
        let mut t = book(AlertStyle::Stop);
        let mut other = view(&mut t).sheet().validations[0].clone();
        other.ranges = vec![(1, 5, 4, 5)]; // F2:F5, the same settings
        view(&mut t).pkg.workbook.sheets[0].validations.push(other);
        select(&mut t, "B2");
        open(&mut t);
        // The dialog shows the rule on B2.
        assert_eq!(
            t.dialogs.top().unwrap().value("first"),
            Some(&Value::Text("10".into()))
        );
        set(&mut t, "first", Json::Str("11".into()));
        set(&mut t, "apply-all", Json::Bool(true));
        press(&mut t, "OK").unwrap();
        let rules = &view(&mut t).sheet().validations;
        assert!(rules.iter().all(|d| d.formula1 == "11"));
        assert!(rules.iter().any(|d| d.covers(2, 5)) && rules.iter().any(|d| d.covers(9, 1)));

        // Clear All with apply-all removes every cell with those settings.
        select(&mut t, "B3");
        open(&mut t);
        set(&mut t, "apply-all", Json::Bool(true));
        press(&mut t, "Clear All").unwrap();
        assert!(view(&mut t).sheet().validations.is_empty());
        assert!(view(&mut t).undo_step());
        // The apply-to-all OK had joined both ranges into one rule.
        assert_eq!(ranges(&mut t), vec![vec![(1, 1, 9, 1), (1, 5, 4, 5)]]);
    }

    #[test]
    fn circles_are_view_state_that_follow_the_cells() {
        let mut t = book(AlertStyle::Stop);
        put(&mut t, "B3", Cell::number(250.0)); // got in some other way
        assert_eq!(view(&mut t).circle_invalid(), 1);
        assert_eq!(view(&mut t).circles, vec![(0, 2, 1)]);
        // A save knows nothing of them, and fixing the cell drops its circle.
        let saved = gridcore::xlsx::save_xlsx(&view(&mut t).pkg);
        assert!(gridcore::xlsx::load_xlsx(&saved).is_ok());
        select(&mut t, "B3");
        assert_eq!(enter(&mut t, "20"), Some(true));
        assert!(view(&mut t).circles.is_empty());
        put(&mut t, "B4", Cell::number(1.0));
        assert_eq!(view(&mut t).circle_invalid(), 1);
        view(&mut t).clear_circles();
        assert!(view(&mut t).circles.is_empty());
    }

    #[test]
    fn a_protected_sheet_refuses_the_dialog() {
        let mut t = book(AlertStyle::Stop);
        view(&mut t).pkg.workbook.sheets[0].protection = Some("sheet=\"1\"".into());
        assert!(dialog(&t).is_err());
    }

    #[test]
    fn the_dialog_shows_and_keeps_a_relative_formula_from_the_selected_cell() {
        let mut t = tab();
        view(&mut t).pkg.workbook.sheets[0]
            .validations
            .push(DataValidation {
                ranges: vec![(1, 1, 9, 1)], // B2:B10
                kind: "custom".into(),
                formula1: "B2>A2".into(),
                allow_blank: true,
                show_error: true,
                ..Default::default()
            });
        let v = view(&mut t);
        v.anchor = at("B5");
        v.sel = at("B6");
        open(&mut t);
        // Seen from B5, the formula is B5>A5.
        assert_eq!(
            t.dialogs.top().unwrap().value("first"),
            Some(&Value::Text("B5>A5".into()))
        );
        t.dialogs.select_tab("Error Alert").unwrap();
        set(&mut t, "error-message", Json::Str("new".into()));
        press(&mut t, "OK").unwrap();
        let rules = &view(&mut t).sheet().validations;
        let b5 = rules.iter().find(|d| d.covers(4, 1)).unwrap();
        assert_eq!(b5.formula1, "B5>A5");
        let b3 = rules.iter().find(|d| d.covers(2, 1)).unwrap();
        assert_eq!(b3.formula1, "B2>A2");
    }

    #[test]
    fn date_bounds_are_typed_as_dates_and_shown_as_dates() {
        let mut t = tab();
        select(&mut t, "B2");
        open(&mut t);
        set(&mut t, "allow", Json::Str("Date".into()));
        set(
            &mut t,
            "operator",
            Json::Str("greater than or equal to".into()),
        );
        set(&mut t, "first", Json::Str("1/1/2020".into()));
        press(&mut t, "OK").unwrap();
        assert_eq!(view(&mut t).sheet().validations[0].formula1, "43831");
        // Reopened, the box shows the date again.
        open(&mut t);
        assert_eq!(
            t.dialogs.top().unwrap().value("first"),
            Some(&Value::Text("1/1/2020".into()))
        );
        press(&mut t, "Cancel").unwrap();
        // And the rule checks entries against it.
        select(&mut t, "B2");
        assert_eq!(enter(&mut t, "2019-12-31"), None);
    }

    #[test]
    fn a_date_bound_is_not_shifted_by_the_fixed_decimal_point() {
        let mut t = tab();
        view(&mut t).edit_opts.fixed_decimal = true;
        select(&mut t, "B2");
        open(&mut t);
        set(&mut t, "allow", Json::Str("Whole number".into()));
        set(&mut t, "operator", Json::Str("greater than".into()));
        set(&mut t, "first", Json::Str("1234".into()));
        press(&mut t, "OK").unwrap();
        assert_eq!(view(&mut t).sheet().validations[0].formula1, "1234");
        let mut t = tab();
        view(&mut t).edit_opts.fixed_decimal = true;
        select(&mut t, "B2");
        open(&mut t);
        set(&mut t, "allow", Json::Str("Date".into()));
        set(&mut t, "operator", Json::Str("greater than".into()));
        set(&mut t, "first", Json::Str("43831".into()));
        press(&mut t, "OK").unwrap();
        assert_eq!(view(&mut t).sheet().validations[0].formula1, "43831");
    }

    #[test]
    fn a_formula_date_bound_survives_ok_and_untouched_bounds_stay_as_stored() {
        let mut t = tab();
        view(&mut t).pkg.workbook.sheets[0]
            .validations
            .push(DataValidation {
                ranges: vec![(1, 1, 9, 1)],
                kind: "date".into(),
                operator: "between".into(),
                formula1: "TODAY()".into(),
                formula2: "43831.5".into(),
                show_error: true,
                ..Default::default()
            });
        select(&mut t, "B2");
        open(&mut t);
        assert_eq!(
            t.dialogs.top().unwrap().value("first"),
            Some(&Value::Text("=TODAY()".into()))
        );
        t.dialogs.select_tab("Error Alert").unwrap();
        set(
            &mut t,
            "error-message",
            Json::Str("only the message".into()),
        );
        press(&mut t, "OK").unwrap();
        let dv = gridcore::validation::validation_at(view(&mut t).sheet(), 1, 1).unwrap();
        assert_eq!(
            (dv.formula1.as_str(), dv.formula2.as_str()),
            ("TODAY()", "43831.5")
        );
        assert_eq!(dv.error, "only the message");
    }

    /// A sheet whose part has no place for `<dataValidations>`.
    fn unwritable(t: &mut DocTab) {
        let v = view(t);
        v.pkg.set_part(
            "xl/worksheets/sheet1.xml",
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/><autoFilter ref="A1:B2"><filterColumn colId="0"><pageMargins left="0.7"/></worksheet>"#.to_vec(),
        );
        assert!(!v.pkg.takes_validations(0));
    }

    #[test]
    fn a_paste_onto_a_sheet_that_cannot_hold_rules_says_so_and_prunes_circles() {
        let mut t = book(AlertStyle::Stop);
        put(&mut t, "B3", Cell::number(250.0));
        assert_eq!(view(&mut t).circle_invalid(), 1);
        unwritable(&mut t);
        let v = view(&mut t);
        v.anchor = at("B2");
        v.sel = at("B2");
        let clip = v.grid_clip(false).unwrap();
        v.anchor = at("B3");
        v.sel = at("B3");
        let res = v.paste_grid_clip(&clip);
        assert!(
            matches!(res, Ok(crate::GridPasted::WithoutRules(_))),
            "{res:?}"
        );
        assert_eq!(value(&mut t, "B3"), CellValue::Number(50.0));
        assert!(view(&mut t).circles.is_empty(), "B3 is valid now");
        // The rules stayed where they were.
        assert_eq!(ranges(&mut t), vec![vec![(1, 1, 9, 1)]]);

        // A cut that arrives as a copy says what it lost as well.
        let v = view(&mut t);
        v.anchor = at("B2");
        v.sel = at("B2");
        let mut clip = v.grid_clip(true).unwrap();
        clip.view = v.id + 1;
        v.anchor = at("B4");
        v.sel = at("B4");
        match v.paste_grid_clip(&clip) {
            Ok(crate::GridPasted::KeptAsCopy(why)) => {
                assert!(why.contains("data validation"), "{why}")
            }
            other => panic!("{other:?}"),
        }
    }

    // ---- review r10 ----

    #[test]
    fn a_refused_save_leaves_no_stale_alert_for_a_later_ctrl_enter() {
        let mut t = book(AlertStyle::Warning);
        select(&mut t, "B2");
        let v = view(&mut t);
        v.begin_cell_edit(Some(String::new()));
        v.edit_caret = 0;
        v.edit_type("2");
        v.edit_type("5");
        v.edit_type("0");
        // Save's commit: refused, the editor kept, nothing held for an alert.
        let saved = crate::close::commit_changed_cell(&mut t);
        assert!(saved.is_err());
        assert!(view(&mut t).dv_pending.is_none());
        // Esc ends the editor; a later Ctrl+Enter over a range, refused for
        // another reason, shows that reason and holds no stale entry.
        let v = view(&mut t);
        v.end_cell_edit();
        v.anchor = at("B2");
        v.sel = at("B4");
        v.begin_cell_edit(Some(String::new()));
        v.edit_caret = 0;
        for ch in "=SUM(B1".chars() {
            v.edit_type(&ch.to_string());
        }
        assert!(!v.commit_edit_to_selection());
        assert!(v.dv_pending.is_none(), "no stale alert to show");
        assert!(
            v.entry_error.is_some(),
            "the formula error is what is shown"
        );
        assert_eq!(value(&mut t, "B2"), CellValue::Number(50.0));
    }

    #[test]
    fn a_structural_edit_refused_over_a_breaking_entry_holds_nothing() {
        let mut t = book(AlertStyle::Information);
        select(&mut t, "B2");
        let v = view(&mut t);
        v.begin_cell_edit(Some(String::new()));
        v.edit_caret = 0;
        v.edit_type("2");
        v.edit_type("5");
        v.edit_type("0");
        assert!(!v.structural_edit(crate::StructOp::InsertRow));
        assert!(v.dv_pending.is_none());
    }

    #[test]
    fn ctrl_enter_over_a_range_with_yes_fills_it_in_one_undo_step() {
        let mut t = book(AlertStyle::Warning);
        let v = view(&mut t);
        v.anchor = at("B2");
        v.sel = at("B4");
        v.begin_cell_edit(Some(String::new()));
        v.edit_caret = 0;
        for ch in "250".chars() {
            v.edit_type(&ch.to_string());
        }
        assert!(!v.commit_edit_to_selection());
        let p = v.dv_pending.clone().expect("held for its alert");
        t.dialogs.push(alert_dialog(&p.violation));
        press(&mut t, "Yes").unwrap();
        for cell in ["B2", "B3", "B4"] {
            assert_eq!(value(&mut t, cell), CellValue::Number(250.0), "{cell}");
        }
        let v = view(&mut t);
        assert_eq!(
            (v.anchor, v.sel),
            (at("B2"), at("B4")),
            "the selection stays"
        );
        assert_eq!(v.undo.len(), 1);
        assert!(v.undo_step());
        assert_eq!(value(&mut t, "B2"), CellValue::Number(50.0));
        assert_eq!(value(&mut t, "B3"), CellValue::Empty);
    }

    /// Type `text` into B2 and commit it the way a click away from it does
    /// (`then` is the click), putting up the alert as the app does.
    fn click_away(t: &mut DocTab, text: &str, then: crate::DvThen) -> Option<bool> {
        select(t, "B2");
        let v = view(t);
        v.begin_cell_edit(Some(String::new()));
        v.edit_caret = 0;
        for ch in text.chars() {
            v.edit_type(&ch.to_string());
        }
        let res = v.commit_then(then);
        if let Some(p) = &view(t).dv_pending {
            let d = alert_dialog(&p.violation);
            t.dialogs.push(d);
            view(t).entry_error = None;
        }
        res
    }

    #[test]
    fn a_click_away_over_a_breaking_entry_waits_for_the_alert() {
        use crate::DvThen::{AddArea, Extend, Select};
        // Yes: the entry goes in, then the click's selection change is made.
        for (then, check) in [
            (Select(4, 3), "select"),
            (Extend(4, 3), "extend"),
            (AddArea(4, 3), "area"),
        ] {
            let mut t = book(AlertStyle::Warning);
            assert_eq!(click_away(&mut t, "250", then), None, "{check}");
            // The editor and the held entry survive until the alert is answered.
            assert!(view(&mut t).editing.is_some() && view(&mut t).dv_pending.is_some());
            assert_eq!(view(&mut t).sel, at("B2"), "{check}: nothing moved yet");
            press(&mut t, "Yes").unwrap();
            assert_eq!(value(&mut t, "B2"), CellValue::Number(250.0), "{check}");
            let v = view(&mut t);
            match check {
                "select" => assert_eq!((v.sel, v.anchor), (at("D5"), at("D5"))),
                "extend" => assert_eq!((v.anchor, v.sel), (at("B2"), at("D5"))),
                _ => assert_eq!(v.sel, at("D5"), "a new area at the clicked cell"),
            }
        }
        // No (and Retry): the text stays in the editor and the selection stays.
        let mut t = book(AlertStyle::Warning);
        click_away(&mut t, "250", Select(4, 3));
        press(&mut t, "No").unwrap();
        let v = view(&mut t);
        assert_eq!(v.editing.as_deref(), Some("250"));
        assert_eq!(v.sel, at("B2"));
        assert_eq!(value(&mut t, "B2"), CellValue::Number(50.0));
    }

    #[test]
    fn circles_go_when_rows_move_under_them() {
        let mut t = book(AlertStyle::Stop);
        put(&mut t, "B3", Cell::number(250.0));
        assert_eq!(view(&mut t).circle_invalid(), 1);
        select(&mut t, "A1");
        assert!(view(&mut t).structural_edit(crate::StructOp::InsertRow));
        assert!(view(&mut t).circles.is_empty());
    }
}
