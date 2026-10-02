//! Data › Outline (#693): Group and Ungroup, Show and Hide Detail, the
//! level buttons, Auto Outline, Clear Outline, the outline Settings and the
//! Subtotal dialog, over [`gridcore::outline`] and [`gridcore::edit`].
//!
//! Every command acts on the tab and is one undo step when it changed the
//! sheet. A refusal (nothing to ungroup, eight levels already) changes
//! nothing, pushes no undo step and says why in the status bar. The dialogs
//! sit on the tab's [`crate::dialog::DialogStack`], so the harness's
//! `dialog-read`/`dialog-set`/`dialog-click` drive them: Subtotal (OK, Remove
//! All, Cancel), Settings, and the Rows/Columns question Group and Ungroup
//! ask when the selection is neither whole rows nor whole columns.
//!
//! The grid draws no outline gutter yet, and it does not hide collapsed
//! columns (it never consults `col_hidden`): column commands change the
//! sheet, and Excel shows the result, but this grid does not until that
//! follow-up lands.

use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::{DocTab, SheetView, Surface};
use gridcore::edit::{
    Area, SubtotalFunc, SubtotalOptions, numeric_columns, remove_subtotals, subtotal,
    subtotal_columns, subtotal_region,
};
use gridcore::outline::{self, Axis, OutlineError};
use gridcore::sheet::{MAX_COLS, MAX_ROWS, Sheet};

/// An outline command from the ribbon or the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cmd {
    Group,
    Ungroup,
    ShowDetail,
    HideDetail,
    /// A level button, 1..=8.
    ShowLevel(u8),
    AutoOutline,
    ClearOutline,
}

/// Alt+Shift+Right groups and Alt+Shift+Left ungroups (`Some(true)` for
/// ungroup). Anything else, Shift+Right among them, is not an outline key.
pub(crate) fn group_key(key: &str, ctrl: bool, shift: bool, alt: bool) -> Option<bool> {
    if !(alt && shift) || ctrl {
        return None;
    }
    match key {
        "right" => Some(false),
        "left" => Some(true),
        _ => None,
    }
}

/// The axis and span a selection names on its own: whole rows or whole
/// columns. `None` for an ordinary block, which makes Group ask.
fn selected_axis(v: &SheetView) -> Option<(Axis, u32, u32)> {
    let (r0, c0, r1, c1) = v.range();
    let whole_rows = c0 == 0 && c1 >= MAX_COLS - 1;
    let whole_cols = r0 == 0 && r1 >= MAX_ROWS - 1;
    match (whole_rows, whole_cols) {
        (true, false) => Some((Axis::Rows, r0, r1)),
        (false, true) => Some((Axis::Cols, c0, c1)),
        _ => None,
    }
}

/// Excel's refusal on a protected sheet: the outline is an edit.
const PROTECTED: &str =
    "The sheet is protected: unprotect it (Review › Protect Sheet) to change the outline.";

/// What one outline edit compares to tell whether it changed anything.
type OutlineState = (
    std::collections::BTreeMap<u32, String>,
    Vec<gridcore::sheet::ColDef>,
    gridcore::sheet::OutlineSettings,
);

fn outline_state(s: &Sheet) -> OutlineState {
    (s.row_attrs.clone(), s.col_defs.clone(), s.outline)
}

/// Run `op` on the active sheet as one undo step. `Err` or no change pushes
/// nothing. `Ok(true)` when the sheet changed.
fn edit_sheet(
    tab: &mut DocTab,
    op: impl FnOnce(&mut Sheet) -> Result<(), OutlineError>,
) -> Result<bool, String> {
    let Surface::Sheet(v) = &mut tab.surface else {
        return Err("the outline needs a spreadsheet".into());
    };
    let s = v.active;
    if v.pkg.workbook.sheets[s].is_protected() {
        return Err(PROTECTED.into());
    }
    let snap = v.snapshot();
    let before = outline_state(&v.pkg.workbook.sheets[s]);
    op(&mut v.pkg.workbook.sheets[s]).map_err(|e| e.to_string())?;
    if outline_state(&v.pkg.workbook.sheets[s]) == before {
        return Ok(false);
    }
    v.push_undo_snapshot(snap);
    // Hidden rows change what SUBTOTAL(101..111) and AGGREGATE return.
    v.engine = crate::sheet_engine(&v.pkg.workbook);
    tab.dirty = true;
    Ok(true)
}

/// Report a command's outcome in the status bar.
fn report(tab: &mut DocTab, outcome: Result<bool, String>, done: &str) {
    tab.status = match outcome {
        Ok(true) => done.to_string().into(),
        Ok(false) => "Nothing to change".into(),
        Err(e) => e.into(),
    };
}

/// Run an outline command on the tab.
pub(crate) fn run(tab: &mut DocTab, cmd: Cmd) {
    let Surface::Sheet(v) = &tab.surface else {
        return;
    };
    let (r0, c0, r1, c1) = v.range();
    let (row, col) = v.sel;
    let has_range = v.has_range();
    let axis = selected_axis(v);
    match cmd {
        Cmd::Group | Cmd::Ungroup => {
            let ungroup = cmd == Cmd::Ungroup;
            match axis {
                Some((axis, a, b)) => group_span(tab, ungroup, axis, a, b),
                None => tab.dialogs.push(axis_dialog(ungroup)),
            }
        }
        Cmd::ShowDetail | Cmd::HideDetail => {
            let show = cmd == Cmd::ShowDetail;
            let detail = |s: &mut Sheet, axis: Axis, i: u32| {
                if show {
                    outline::show_detail(s, axis, i)
                } else {
                    outline::hide_detail(s, axis, i)
                }
            };
            // Whole columns act on columns; otherwise the cursor's row, then
            // its column when no row group is there.
            let outcome = edit_sheet(tab, |s| match axis {
                Some((Axis::Cols, _, _)) => detail(s, Axis::Cols, col),
                _ => detail(s, Axis::Rows, row).or_else(|_| detail(s, Axis::Cols, col)),
            });
            report(
                tab,
                outcome,
                if show {
                    "Detail shown"
                } else {
                    "Detail hidden"
                },
            );
        }
        Cmd::ShowLevel(n) => {
            // One set of level buttons: each axis with an outline goes to `n`.
            let outcome = edit_sheet(tab, |s| {
                let axes: Vec<Axis> = [Axis::Rows, Axis::Cols]
                    .into_iter()
                    .filter(|&a| outline::max_level(s, a) > 0)
                    .collect();
                if axes.is_empty() {
                    return Err(OutlineError::NoOutline);
                }
                for a in axes {
                    outline::show_level(s, a, n);
                }
                Ok(())
            });
            report(tab, outcome, &format!("Showing outline level {n}"));
        }
        Cmd::AutoOutline => {
            let area = has_range.then_some((r0, c0, r1, c1));
            let outcome = edit_sheet(tab, |s| outline::auto_outline(s, area));
            report(tab, outcome, "Outline created");
        }
        Cmd::ClearOutline => {
            let outcome = edit_sheet(tab, outline::clear_outline);
            report(tab, outcome, "Outline cleared");
        }
    }
}

/// Group or ungroup rows (columns) `a..=b`.
fn group_span(tab: &mut DocTab, ungroup: bool, axis: Axis, a: u32, b: u32) {
    let outcome = edit_sheet(tab, |s| {
        if ungroup {
            outline::ungroup(s, axis, a, b)
        } else {
            outline::group(s, axis, a, b)
        }
    });
    let what = match axis {
        Axis::Rows => "Rows",
        Axis::Cols => "Columns",
    };
    let done = format!("{what} {}", if ungroup { "ungrouped" } else { "grouped" });
    report(tab, outcome, &done);
}

const AXES: [&str; 2] = ["Rows", "Columns"];

/// Excel's Group (Ungroup) question: rows or columns of the selection.
fn axis_dialog(ungroup: bool) -> Dialog {
    let mut d = Dialog::message(
        if ungroup { "ungroup" } else { "group" },
        if ungroup { "Ungroup" } else { "Group" },
        String::new(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::OutlineAxis { ungroup },
    );
    d.text = None;
    let mut c = Control::new("axis", "Group", ControlKind::Radio, Value::Choice(Some(0)));
    c.items = AXES.iter().map(|s| s.to_string()).collect();
    d.controls.push(c);
    d.mark_opened();
    d
}

/// The column the Subtotal dialog and its checkboxes can name: 64 is far
/// more than a subtotal region holds in practice.
const MAX_SUBTOTAL_COLS: usize = 64;
/// Control names have to outlive the dialog.
const ADD_NAMES: [&str; MAX_SUBTOTAL_COLS] = [
    "add-0", "add-1", "add-2", "add-3", "add-4", "add-5", "add-6", "add-7", "add-8", "add-9",
    "add-10", "add-11", "add-12", "add-13", "add-14", "add-15", "add-16", "add-17", "add-18",
    "add-19", "add-20", "add-21", "add-22", "add-23", "add-24", "add-25", "add-26", "add-27",
    "add-28", "add-29", "add-30", "add-31", "add-32", "add-33", "add-34", "add-35", "add-36",
    "add-37", "add-38", "add-39", "add-40", "add-41", "add-42", "add-43", "add-44", "add-45",
    "add-46", "add-47", "add-48", "add-49", "add-50", "add-51", "add-52", "add-53", "add-54",
    "add-55", "add-56", "add-57", "add-58", "add-59", "add-60", "add-61", "add-62", "add-63",
];

/// Excel's Subtotal dialog over the region around the cursor, or why it
/// cannot open.
pub(crate) fn subtotal_dialog(tab: &DocTab) -> Result<Dialog, String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("Subtotal needs a spreadsheet".into());
    };
    let s = v.sheet();
    let (row, col) = v.sel;
    let (area, header) = subtotal_region(s, row, col)
        .ok_or("Subtotal: put the cursor in the list of data to total")?;
    let (r1, c1, r2, c2) = area;
    // The dialog lists at most MAX_SUBTOTAL_COLS columns; Remove All and the
    // total-row test still see the whole region.
    let mut cols = subtotal_columns(s, area, header);
    cols.truncate(MAX_SUBTOTAL_COLS);
    let labels: Vec<String> = cols.iter().map(|(_, n)| n.clone()).collect();
    let numeric = numeric_columns(s, (r1 + u32::from(header), c1, r2, c2), col);
    let defaults = SubtotalOptions::new(col, numeric, header);
    let mut controls = Vec::new();
    let mut group = Control::new(
        "group",
        "At each change in:",
        ControlKind::Dropdown,
        Value::Choice(cols.iter().position(|&(c, _)| c == defaults.group_col)),
    );
    group.items = labels.clone();
    controls.push(group);
    let mut func = Control::new(
        "function",
        "Use function:",
        ControlKind::Dropdown,
        Value::Choice(SubtotalFunc::ALL.iter().position(|&f| f == defaults.func)),
    );
    func.items = SubtotalFunc::ALL.iter().map(|f| f.name().into()).collect();
    controls.push(func);
    controls.push(Control::new(
        "add-to",
        "Add subtotal to:",
        ControlKind::Label,
        Value::Text(String::new()),
    ));
    for (i, (c, _)) in cols.iter().enumerate() {
        controls.push(Control::new(
            ADD_NAMES[i],
            &labels[i],
            ControlKind::Checkbox,
            Value::Bool(defaults.add_to.contains(c)),
        ));
    }
    for (name, label, on) in [
        ("replace", "Replace current subtotals", defaults.replace),
        (
            "page-breaks",
            "Page break between groups",
            defaults.page_breaks,
        ),
        ("below", "Summary below data", defaults.summary_below),
    ] {
        controls.push(Control::new(
            name,
            label,
            ControlKind::Checkbox,
            Value::Bool(on),
        ));
    }
    let mut d = Dialog::message(
        "subtotal",
        "Subtotal",
        String::new(),
        &[
            ("Remove All", ButtonRole::Accept),
            ("OK", ButtonRole::Accept),
            ("Cancel", ButtonRole::Cancel),
        ],
        DialogOwner::Subtotal {
            sheet: v.active,
            r1,
            r2,
            c1,
            c2,
            header,
        },
    );
    d.text = None;
    // OK is the default, not Remove All.
    d.buttons = d
        .buttons
        .into_iter()
        .map(|b| Button {
            default: b.label == "OK",
            ..b
        })
        .collect();
    d.controls = controls;
    d.mark_opened();
    Ok(d)
}

fn choice(d: &Dialog, name: &str) -> Option<usize> {
    d.controls
        .iter()
        .find(|c| c.name == name)
        .and_then(|c| match c.value {
            Value::Choice(i) => i,
            _ => None,
        })
}

fn checked(d: &Dialog, name: &str) -> bool {
    d.controls
        .iter()
        .any(|c| c.name == name && c.value == Value::Bool(true))
}

/// The options the Subtotal dialog `d` stages.
pub(crate) fn subtotal_options(d: &Dialog) -> Result<SubtotalOptions, String> {
    let DialogOwner::Subtotal { c1, header, .. } = d.owner else {
        return Err("not a Subtotal dialog".into());
    };
    let group = choice(d, "group").ok_or("choose a column in 'At each change in'")?;
    let func = choice(d, "function")
        .and_then(|i| SubtotalFunc::ALL.get(i).copied())
        .unwrap_or_default();
    let add_to: Vec<u32> = ADD_NAMES
        .iter()
        .enumerate()
        .filter(|(_, name)| checked(d, name))
        .map(|(i, _)| c1 + i as u32)
        .collect();
    Ok(SubtotalOptions {
        group_col: c1 + group as u32,
        func,
        add_to,
        replace: checked(d, "replace"),
        page_breaks: checked(d, "page-breaks"),
        summary_below: checked(d, "below"),
        has_header: header,
    })
}

/// Excel's outline Settings: where summary rows and columns sit.
pub(crate) fn settings_dialog(tab: &DocTab) -> Result<Dialog, String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("outline settings need a spreadsheet".into());
    };
    let now = v.sheet().outline;
    let mut d = Dialog::message(
        "outline-settings",
        "Settings",
        String::new(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::OutlineSettings,
    );
    d.text = None;
    d.controls = vec![
        Control::new(
            "below",
            "Summary rows below detail",
            ControlKind::Checkbox,
            Value::Bool(now.summary_below),
        ),
        Control::new(
            "right",
            "Summary columns to right of detail",
            ControlKind::Checkbox,
            Value::Bool(now.summary_right),
        ),
    ];
    d.mark_opened();
    Ok(d)
}

fn presses(d: &Dialog, button: &str, label: &str) -> bool {
    button.replace('&', "").trim().eq_ignore_ascii_case(label)
        && d.buttons.iter().any(|b| b.label == label && b.enabled)
}

/// A press the outline dialogs handle themselves: Subtotal's OK and Remove
/// All, Settings' OK and the Rows/Columns question's OK. `None` for any
/// other press (Cancel closes through the stack).
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    let owner = top.owner;
    let ok = presses(top, button, "OK");
    let remove = presses(top, button, "Remove All");
    match owner {
        DialogOwner::Subtotal {
            sheet,
            r1,
            r2,
            c1,
            c2,
            ..
        } if ok || remove => {
            let staged = if ok {
                match subtotal_options(top) {
                    Ok(o) => Some(o),
                    Err(e) => return Some(Err(e)),
                }
            } else {
                None
            };
            Some(apply_subtotal(
                tab,
                sheet,
                (r1, c1, r2, c2),
                staged.as_ref(),
            ))
        }
        DialogOwner::OutlineSettings if ok => {
            let below = checked(top, "below");
            let right = checked(top, "right");
            let outcome = edit_sheet(tab, |s| {
                s.outline.summary_below = below;
                s.outline.summary_right = right;
                Ok(())
            });
            report(tab, outcome, "Outline settings changed");
            tab.dialogs.pop();
            Some(Ok(()))
        }
        DialogOwner::OutlineAxis { ungroup } if ok => {
            let axis = match choice(top, "axis") {
                Some(1) => Axis::Cols,
                _ => Axis::Rows,
            };
            tab.dialogs.pop();
            let Surface::Sheet(v) = &tab.surface else {
                return Some(Err("the outline needs a spreadsheet".into()));
            };
            let (r0, c0, r1, c1) = v.range();
            let (a, b) = match axis {
                Axis::Rows => (r0, r1),
                Axis::Cols => (c0, c1),
            };
            group_span(tab, ungroup, axis, a, b);
            Some(Ok(()))
        }
        _ => None,
    }
}

/// Subtotal (`Some(options)`) or Remove All (`None`) over `area`, the region
/// `(r1, c1, r2, c2)`, as one undo step, and close the dialog. Total rows are
/// found only in columns `c1..=c2`, so a SUBTOTAL beside the list is data. A
/// refusal keeps the dialog open.
fn apply_subtotal(
    tab: &mut DocTab,
    sheet: usize,
    area: Area,
    opts: Option<&SubtotalOptions>,
) -> Result<(), String> {
    let Surface::Sheet(v) = &mut tab.surface else {
        return Err("Subtotal needs a spreadsheet".into());
    };
    if v.pkg.workbook.sheets[sheet].is_protected() {
        return Err(PROTECTED.into());
    }
    let snap = v.snapshot();
    let before = v.pkg.workbook.sheets.clone();
    let status = match opts {
        Some(o) => match subtotal(&mut v.pkg.workbook, sheet, area, o) {
            Ok(n) => format!("Inserted {n} subtotal row{}", if n == 1 { "" } else { "s" }),
            Err(e) => {
                // gridcore refuses before it changes anything; should that
                // ever slip, a row delete reaches every sheet, so the whole
                // workbook goes back.
                v.restore(snap);
                return Err(e.to_string());
            }
        },
        None => {
            let n = remove_subtotals(&mut v.pkg.workbook, sheet, area);
            format!("Removed {n} subtotal row{}", if n == 1 { "" } else { "s" })
        }
    };
    // Remove All also drops page breaks, levels and hidden flags, so only a
    // comparison tells whether it did anything.
    if !gridcore::edit::sheets_differ(&before, &v.pkg.workbook.sheets) {
        tab.dialogs.pop();
        tab.status = "There are no subtotals to remove".into();
        return Ok(());
    }
    v.push_undo_snapshot(snap);
    v.engine = crate::sheet_engine(&v.pkg.workbook);
    tab.dirty = true;
    tab.status = status.into();
    tab.dialogs.pop();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Kind, new_sheet_surface};
    use ctlcore::json::Json;
    use gridcore::sheet::{Cell, CellValue};

    fn tab() -> DocTab {
        DocTab {
            kind: Kind::Xlsx,
            title: "book.xlsx".into(),
            path: None,
            surface: new_sheet_surface(),
            dirty: false,
            status: "".into(),
            comments: vec![],
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

    fn sheet(t: &mut DocTab) -> &Sheet {
        view(t).sheet()
    }

    fn select(t: &mut DocTab, r0: u32, c0: u32, r1: u32, c1: u32) {
        let v = view(t);
        v.anchor = (r0, c0);
        v.sel = (r1, c1);
    }

    fn undo_len(t: &mut DocTab) -> usize {
        view(t).undo.len()
    }

    /// Grp/Amt with three A rows and two B rows (A1:B6), cursor in the data.
    fn sales() -> DocTab {
        let mut t = tab();
        let v = view(&mut t);
        let s = &mut v.pkg.workbook.sheets[0];
        s.set_cell(0, 0, Cell::text("Grp"));
        s.set_cell(0, 1, Cell::text("Amt"));
        for (r, g, n) in [
            (1, "A", 1.0),
            (2, "A", 2.0),
            (3, "A", 3.0),
            (4, "B", 4.0),
            (5, "B", 5.0),
        ] {
            s.set_cell(r, 0, Cell::text(g));
            s.set_cell(r, 1, Cell::number(n));
        }
        v.engine = crate::sheet_engine(&v.pkg.workbook);
        select(&mut t, 2, 0, 2, 0);
        t
    }

    fn press(t: &mut DocTab, button: &str) {
        crate::dialog_host::dialog_click(t, button).unwrap();
    }

    fn set(t: &mut DocTab, control: &str, value: Json) {
        t.dialogs
            .set(control, &Json::obj(vec![("value", value)]))
            .unwrap();
    }

    #[test]
    fn alt_shift_arrows_are_group_and_ungroup_and_shift_arrows_are_not() {
        assert_eq!(group_key("right", false, true, true), Some(false));
        assert_eq!(group_key("left", false, true, true), Some(true));
        // Shift+Right still extends the selection; Alt+Right scrolls nothing
        // here; Ctrl+Alt+Shift is another chord.
        assert_eq!(group_key("right", false, true, false), None);
        assert_eq!(group_key("right", false, false, true), None);
        assert_eq!(group_key("right", true, true, true), None);
        assert_eq!(group_key("up", false, true, true), None);
    }

    #[test]
    fn whole_rows_group_at_once_and_undo_takes_it_back() {
        let mut t = tab();
        select(&mut t, 1, 0, 3, MAX_COLS - 1);
        run(&mut t, Cmd::Group);
        assert!(t.dialogs.top().is_none(), "whole rows need no question");
        assert_eq!(
            (0..5)
                .map(|r| sheet(&mut t).row_outline(r))
                .collect::<Vec<_>>(),
            [0, 1, 1, 1, 0]
        );
        assert!(t.dirty);
        assert_eq!(undo_len(&mut t), 1);
        assert!(view(&mut t).undo_step());
        assert_eq!(sheet(&mut t).max_row_outline(), 0);
    }

    #[test]
    fn whole_columns_group_columns_on_the_model() {
        let mut t = tab();
        select(&mut t, 0, 2, MAX_ROWS - 1, 3);
        run(&mut t, Cmd::Group);
        assert_eq!(sheet(&mut t).col_outline(2), 1);
        assert_eq!(sheet(&mut t).col_outline(3), 1);
        assert_eq!(sheet(&mut t).col_outline(4), 0);
        // Hide Detail at a whole-column selection folds the columns: the
        // grid does not draw that yet, but the sheet (and the file) has it.
        run(&mut t, Cmd::HideDetail);
        assert!(sheet(&mut t).col_hidden(2) && sheet(&mut t).col_hidden(3));
        assert!(sheet(&mut t).col_collapsed(4));
        run(&mut t, Cmd::ShowLevel(2));
        assert!(!sheet(&mut t).col_hidden(2));
        run(&mut t, Cmd::Ungroup);
        assert_eq!(sheet(&mut t).max_col_outline(), 0);
    }

    #[test]
    fn a_block_asks_rows_or_columns() {
        let mut t = tab();
        select(&mut t, 1, 1, 2, 3);
        run(&mut t, Cmd::Group);
        assert_eq!(t.dialogs.top().map(|d| d.id), Some("group"));
        set(&mut t, "axis", Json::Str("Columns".into()));
        press(&mut t, "OK");
        assert!(t.dialogs.top().is_none());
        assert_eq!(
            (0..5)
                .map(|c| sheet(&mut t).col_outline(c))
                .collect::<Vec<_>>(),
            [0, 1, 1, 1, 0]
        );
        assert_eq!(sheet(&mut t).max_row_outline(), 0);
        // Ungroup asks too; Rows is the default.
        run(&mut t, Cmd::Ungroup);
        assert_eq!(t.dialogs.top().map(|d| d.id), Some("ungroup"));
        press(&mut t, "Cancel");
        assert!(t.dialogs.top().is_none());
        assert_eq!(sheet(&mut t).max_col_outline(), 1, "Cancel changes nothing");
    }

    #[test]
    fn a_refusal_pushes_no_undo_step_and_says_why() {
        let mut t = tab();
        select(&mut t, 1, 0, 3, MAX_COLS - 1);
        run(&mut t, Cmd::Ungroup);
        assert_eq!(undo_len(&mut t), 0);
        assert_eq!(&*t.status, OutlineError::NotGrouped.to_string());
        assert!(!t.dirty);
        for _ in 0..7 {
            run(&mut t, Cmd::Group);
        }
        assert_eq!(undo_len(&mut t), 7);
        run(&mut t, Cmd::Group);
        assert_eq!(undo_len(&mut t), 7);
        assert_eq!(&*t.status, OutlineError::TooDeep.to_string());
        run(&mut t, Cmd::ClearOutline);
        assert_eq!(undo_len(&mut t), 8);
        run(&mut t, Cmd::ClearOutline);
        assert_eq!(undo_len(&mut t), 8, "nothing left to clear");
        assert_eq!(&*t.status, OutlineError::NoOutline.to_string());
        run(&mut t, Cmd::ShowLevel(1));
        assert_eq!(undo_len(&mut t), 8, "no outline to show a level of");
        assert_eq!(&*t.status, "There is no outline on this sheet.");
    }

    #[test]
    fn a_protected_sheet_refuses_the_outline() {
        let mut t = tab();
        let v = view(&mut t);
        let s = v.active;
        v.pkg.workbook.sheets[s].set_protected(true);
        select(&mut t, 1, 0, 3, MAX_COLS - 1);
        run(&mut t, Cmd::Group);
        assert_eq!(sheet(&mut t).max_row_outline(), 0);
        assert_eq!(undo_len(&mut t), 0);
        assert_eq!(&*t.status, PROTECTED);
    }

    #[test]
    fn levels_and_detail_hide_and_show_rows() {
        let mut t = tab();
        select(&mut t, 1, 0, 3, MAX_COLS - 1);
        run(&mut t, Cmd::Group);
        run(&mut t, Cmd::ShowLevel(1));
        assert!((1..=3).all(|r| sheet(&mut t).row_hidden(r)));
        assert!(sheet(&mut t).row_collapsed(4));
        select(&mut t, 4, 0, 4, 0);
        run(&mut t, Cmd::ShowDetail);
        assert!(!(1..=3).any(|r| sheet(&mut t).row_hidden(r)));
        run(&mut t, Cmd::HideDetail);
        assert!((1..=3).all(|r| sheet(&mut t).row_hidden(r)));
        assert_eq!(undo_len(&mut t), 4);
    }

    #[test]
    fn auto_outline_builds_from_the_totals() {
        let mut t = tab();
        let v = view(&mut t);
        let s = &mut v.pkg.workbook.sheets[0];
        s.set_cell(0, 0, Cell::number(1.0));
        s.set_cell(1, 0, Cell::number(2.0));
        s.set_cell(2, 0, Cell::formula("SUM(A1:A2)"));
        run(&mut t, Cmd::AutoOutline);
        assert_eq!(sheet(&mut t).row_outline(0), 1);
        assert_eq!(undo_len(&mut t), 1);
    }

    #[test]
    fn the_subtotal_dialog_maps_to_options() {
        let t = sales();
        let mut d = subtotal_dialog(&t).unwrap();
        assert_eq!(
            d.owner,
            DialogOwner::Subtotal {
                sheet: 0,
                r1: 0,
                r2: 5,
                c1: 0,
                c2: 1,
                header: true
            }
        );
        let o = subtotal_options(&d).unwrap();
        assert_eq!(
            o,
            SubtotalOptions::new(0, vec![1], true),
            "Excel's defaults"
        );
        // The columns are named by their headers.
        assert_eq!(d.controls[0].items, ["Grp", "Amt"]);
        d.set(
            "function",
            &Json::obj(vec![("value", Json::Str("Average".into()))]),
        )
        .unwrap();
        d.set("Amt", &Json::obj(vec![("value", Json::Bool(false))]))
            .unwrap();
        d.set("Grp", &Json::obj(vec![("value", Json::Bool(true))]))
            .unwrap();
        for (name, on) in [("replace", false), ("page-breaks", true), ("below", false)] {
            d.set(name, &Json::obj(vec![("value", Json::Bool(on))]))
                .unwrap();
        }
        d.set(
            "group",
            &Json::obj(vec![("value", Json::Str("Amt".into()))]),
        )
        .unwrap();
        let o = subtotal_options(&d).unwrap();
        assert_eq!(
            o,
            SubtotalOptions {
                group_col: 1,
                func: SubtotalFunc::Average,
                add_to: vec![0],
                replace: false,
                page_breaks: true,
                summary_below: false,
                has_header: true,
            }
        );
    }

    #[test]
    fn subtotal_ok_inserts_and_remove_all_takes_it_out_each_one_undo_step() {
        let mut t = sales();
        t.dialogs.push(subtotal_dialog(&t).unwrap());
        press(&mut t, "OK");
        assert!(t.dialogs.top().is_none());
        assert_eq!(undo_len(&mut t), 1);
        let label = |t: &mut DocTab, r: u32| match sheet(t).cell(r, 0).map(|c| c.value.clone()) {
            Some(CellValue::Text(s)) => s,
            _ => String::new(),
        };
        assert_eq!(label(&mut t, 4), "A Total");
        assert_eq!(label(&mut t, 8), "Grand Total");
        assert_eq!(sheet(&mut t).row_outline(1), 2);
        // Remove All works on the region around the cursor, totals included.
        select(&mut t, 4, 0, 4, 0);
        let d = subtotal_dialog(&t).unwrap();
        assert!(matches!(
            d.owner,
            DialogOwner::Subtotal { r1: 0, r2: 8, .. }
        ));
        t.dialogs.push(d);
        press(&mut t, "Remove All");
        assert!(t.dialogs.top().is_none());
        assert_eq!(undo_len(&mut t), 2);
        assert_eq!(label(&mut t, 4), "B");
        assert_eq!(sheet(&mut t).max_row_outline(), 0);
        // Undo brings the subtotals back.
        assert!(view(&mut t).undo_step());
        assert_eq!(label(&mut t, 4), "A Total");
    }

    #[test]
    fn a_subtotal_with_no_column_is_refused_and_the_dialog_stays() {
        let mut t = sales();
        t.dialogs.push(subtotal_dialog(&t).unwrap());
        set(&mut t, "Amt", Json::Bool(false));
        let err = crate::dialog_host::dialog_click(&mut t, "OK").unwrap_err();
        assert_eq!(err, gridcore::edit::SubtotalError::NoColumns.to_string());
        assert_eq!(t.dialogs.top().map(|d| d.id), Some("subtotal"));
        assert_eq!(undo_len(&mut t), 0);
    }

    /// Remove All on the region around the cursor, through the dialog.
    fn remove_all(t: &mut DocTab) {
        select(t, 2, 0, 2, 0);
        t.dialogs.push(subtotal_dialog(t).unwrap());
        press(t, "Remove All");
        assert!(t.dialogs.top().is_none());
    }

    #[test]
    fn remove_all_records_breaks_and_outline_it_took_and_nothing_else() {
        // (a) Only a manual page break in the list, no outline.
        let mut t = sales();
        let s = view(&mut t).active;
        gridcore::print::area::insert_page_break(&mut view(&mut t).pkg.workbook.sheets[s], 3, 0);
        remove_all(&mut t);
        assert!(
            gridcore::print::area::manual_breaks(sheet(&mut t))
                .0
                .is_empty()
        );
        assert_eq!(undo_len(&mut t), 1);
        assert!(t.dirty);
        // (b) A collapsed group and no total rows: ungrouped and shown.
        let mut t = sales();
        {
            let sh = &mut view(&mut t).pkg.workbook.sheets[s];
            outline::group(sh, Axis::Rows, 1, 3).unwrap();
            let g = outline::groups(sh, Axis::Rows)[0];
            outline::collapse_group(sh, Axis::Rows, &g);
        }
        remove_all(&mut t);
        assert_eq!(sheet(&mut t).max_row_outline(), 0);
        assert!(!(1..=3).any(|r| sheet(&mut t).row_hidden(r)));
        assert_eq!(undo_len(&mut t), 1);
        assert!(t.dirty);
        // (c) An outline elsewhere on the sheet, nothing in the region.
        let mut t = sales();
        outline::group(&mut view(&mut t).pkg.workbook.sheets[s], Axis::Rows, 20, 22).unwrap();
        remove_all(&mut t);
        assert_eq!(undo_len(&mut t), 0);
        assert!(!t.dirty);
        assert_eq!(&*t.status, "There are no subtotals to remove");
        assert_eq!(sheet(&mut t).row_outline(21), 1);
    }

    #[test]
    fn a_refused_replace_leaves_every_sheet_as_it_was() {
        // A header and nothing but total rows; another sheet points at the
        // grand total.
        let mut t = tab();
        let v = view(&mut t);
        let s = &mut v.pkg.workbook.sheets[0];
        s.set_cell(0, 0, Cell::text("Grp"));
        s.set_cell(0, 1, Cell::text("Amt"));
        s.set_cell(1, 0, Cell::text("A Total"));
        s.set_cell(1, 1, Cell::formula("SUBTOTAL(9,B2:B2)"));
        s.set_cell(2, 0, Cell::text("Grand Total"));
        s.set_cell(2, 1, Cell::formula("SUBTOTAL(9,B2:B3)"));
        let mut other = Sheet {
            name: "Other".into(),
            ..Sheet::default()
        };
        other.set_cell(0, 0, Cell::formula("Sheet1!B3"));
        v.pkg.workbook.sheets.push(other);
        select(&mut t, 1, 0, 1, 0);
        t.dialogs.push(subtotal_dialog(&t).unwrap());
        set(&mut t, "Amt", Json::Bool(true));
        let err = crate::dialog_host::dialog_click(&mut t, "OK").unwrap_err();
        assert_eq!(err, gridcore::edit::SubtotalError::Empty.to_string());
        let f = view(&mut t).pkg.workbook.sheets[1]
            .cell(0, 0)
            .unwrap()
            .formula
            .clone();
        assert_eq!(f.as_deref(), Some("Sheet1!B3"));
        assert_eq!(sheet(&mut t).used_size().0, 3);
        assert_eq!(undo_len(&mut t), 0);
    }

    #[test]
    fn subtotal_outside_the_data_does_not_open() {
        let mut t = sales();
        select(&mut t, 20, 0, 20, 0);
        assert!(subtotal_dialog(&t).is_err());
    }

    #[test]
    fn settings_change_the_summary_sides() {
        let mut t = tab();
        t.dialogs.push(settings_dialog(&t).unwrap());
        set(&mut t, "below", Json::Bool(false));
        press(&mut t, "OK");
        assert!(!sheet(&mut t).outline.summary_below);
        assert!(sheet(&mut t).outline.summary_right);
        assert_eq!(undo_len(&mut t), 1);
        // An untouched OK is no undo step.
        t.dialogs.push(settings_dialog(&t).unwrap());
        press(&mut t, "OK");
        assert_eq!(undo_len(&mut t), 1);
    }
}
