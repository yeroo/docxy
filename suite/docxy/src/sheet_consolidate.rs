//! Data › Data Tools › Consolidate (#694): Excel's dialog over
//! [`gridcore::edit::consolidate`], writing at the cell it opened on.
//!
//! The dialog sits on the tab's [`crate::dialog::DialogStack`] (id
//! `consolidate`), so the harness's `dialog-set`/`dialog-click` drive it:
//! Function, Reference, All references, Top row, Left column, Create links
//! to source data, and the buttons Add, Delete, OK and Close. Add and
//! Delete edit the list and keep the dialog open; OK consolidates as one
//! undo step, and a refusal keeps the dialog open and says why. It opens on
//! the settings the sheet kept from its last Consolidate.

use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::{DocTab, Surface};
use gridcore::edit::{
    ConsolidateOptions, SubtotalFunc, canonical_consolidate_ref, consolidate, sheets_differ,
};
use gridcore::sheet::cell_name;

/// Excel's refusal on a protected sheet.
const PROTECTED: &str =
    "The sheet is protected: unprotect it (Review › Protect Sheet) to consolidate into it.";

/// The Consolidate dialog for the cursor's cell, starting from the
/// settings the sheet kept.
pub(crate) fn consolidate_dialog(tab: &DocTab) -> Result<Dialog, String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("Consolidate needs a spreadsheet".into());
    };
    let kept = v.sheet().consolidate.clone().unwrap_or_default();
    let (row, col) = v.sel;
    let mut d = Dialog::message(
        "consolidate",
        "Consolidate",
        String::new(),
        &[
            ("Add", ButtonRole::Apply),
            ("Delete", ButtonRole::Apply),
            ("OK", ButtonRole::Accept),
            ("Close", ButtonRole::Cancel),
        ],
        DialogOwner::Consolidate {
            sheet: v.active,
            row,
            col,
        },
    );
    d.text = None;
    d.buttons = d
        .buttons
        .into_iter()
        .map(|b| Button {
            default: b.label == "OK",
            ..b
        })
        .collect();
    let mut func = Control::new(
        "function",
        "Function:",
        ControlKind::Dropdown,
        Value::Choice(SubtotalFunc::ALL.iter().position(|&f| f == kept.func)),
    );
    func.items = SubtotalFunc::ALL.iter().map(|f| f.name().into()).collect();
    let mut refs = Control::new(
        "refs",
        "All references:",
        ControlKind::List,
        Value::Choice(None),
    );
    refs.items = kept.refs;
    d.controls = vec![
        func,
        Control::new(
            "reference",
            "Reference:",
            ControlKind::Text,
            Value::Text(String::new()),
        ),
        refs,
        Control::new(
            "use-labels",
            "Use labels in",
            ControlKind::Label,
            Value::Text(String::new()),
        ),
        Control::new(
            "top",
            "Top row",
            ControlKind::Checkbox,
            Value::Bool(kept.top_row),
        ),
        Control::new(
            "left",
            "Left column",
            ControlKind::Checkbox,
            Value::Bool(kept.left_col),
        ),
        Control::new(
            "links",
            "Create links to source data",
            ControlKind::Checkbox,
            Value::Bool(kept.links),
        ),
    ];
    d.mark_opened();
    Ok(d)
}

fn control<'a>(d: &'a Dialog, name: &str) -> Option<&'a Control> {
    d.controls.iter().find(|c| c.name == name)
}

fn control_mut<'a>(d: &'a mut Dialog, name: &str) -> Option<&'a mut Control> {
    d.controls.iter_mut().find(|c| c.name == name)
}

fn checked(d: &Dialog, name: &str) -> bool {
    control(d, name).is_some_and(|c| c.value == Value::Bool(true))
}

fn typed(d: &Dialog) -> String {
    match control(d, "reference").map(|c| &c.value) {
        Some(Value::Text(t)) => t.trim().to_string(),
        _ => String::new(),
    }
}

fn listed(d: &Dialog) -> Vec<String> {
    control(d, "refs")
        .map(|c| c.items.clone())
        .unwrap_or_default()
}

/// The options the dialog stages: the list, plus a reference typed but not
/// added, as Excel's OK takes it. The book name is filled in on apply.
pub(crate) fn consolidate_options(d: &Dialog) -> ConsolidateOptions {
    let mut refs = listed(d);
    let t = typed(d);
    if !t.is_empty() && !refs.iter().any(|r| r.eq_ignore_ascii_case(&t)) {
        refs.push(t);
    }
    let func = match control(d, "function").map(|c| &c.value) {
        Some(Value::Choice(Some(i))) => SubtotalFunc::ALL.get(*i).copied().unwrap_or_default(),
        _ => SubtotalFunc::default(),
    };
    ConsolidateOptions {
        func,
        refs,
        top_row: checked(d, "top"),
        left_col: checked(d, "left"),
        links: checked(d, "links"),
        book_name: String::new(),
    }
}

/// Add: the typed reference joins the list (once), as `canon` spells it,
/// and the box empties.
fn add(d: &mut Dialog, canon: &dyn Fn(&str) -> String) {
    let t = canon(&typed(d));
    if t.is_empty() {
        return;
    }
    if let Some(list) = control_mut(d, "refs") {
        if !list.items.iter().any(|r| r.eq_ignore_ascii_case(&t)) {
            list.items.push(t);
        }
        list.value = Value::Choice(None);
    }
    if let Some(r) = control_mut(d, "reference") {
        r.value = Value::Text(String::new());
    }
}

/// Delete: the chosen entry, or the one the box names, leaves the list.
fn delete(d: &mut Dialog, canon: &dyn Fn(&str) -> String) {
    let t = canon(&typed(d));
    let Some(list) = control_mut(d, "refs") else {
        return;
    };
    let at = match list.value {
        Value::Choice(Some(i)) if i < list.items.len() => Some(i),
        _ => list.items.iter().position(|r| r.eq_ignore_ascii_case(&t)),
    };
    if let Some(i) = at {
        list.items.remove(i);
        list.value = Value::Choice(None);
        if let Some(r) = control_mut(d, "reference") {
            r.value = Value::Text(String::new());
        }
    }
}

fn presses(d: &Dialog, button: &str, label: &str) -> bool {
    button.replace('&', "").trim().eq_ignore_ascii_case(label)
        && d.buttons.iter().any(|b| b.label == label && b.enabled)
}

/// A press the Consolidate dialog handles itself: Add, Delete and OK.
/// `None` for any other press or dialog (Close closes through the stack).
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    let DialogOwner::Consolidate { sheet, row, col } = top.owner else {
        return None;
    };
    if presses(top, button, "Add") || presses(top, button, "Delete") {
        let adding = presses(top, button, "Add");
        // A reference that parses is kept the way the list shows it, so
        // one spelled two ways is one entry.
        let names: Vec<String> = match &tab.surface {
            Surface::Sheet(v) => v
                .pkg
                .workbook
                .sheets
                .iter()
                .map(|s| s.name.clone())
                .collect(),
            _ => Vec::new(),
        };
        let canon = |text: &str| {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            canonical_consolidate_ref(&names, sheet, text)
        };
        let d = tab.dialogs.top_dialog_mut().ok()?;
        if adding {
            add(d, &canon);
        } else {
            delete(d, &canon);
        }
        return Some(Ok(()));
    }
    if !presses(top, button, "OK") {
        return None;
    }
    let opts = consolidate_options(top);
    Some(apply(tab, sheet, (row, col), opts))
}

/// Consolidate into `sheet` at `at` as one undo step and close the dialog.
/// Linked detail rows carry the workbook's name, the tab's file stem. A
/// refusal changes nothing and keeps the dialog open.
fn apply(
    tab: &mut DocTab,
    sheet: usize,
    at: (u32, u32),
    mut opts: ConsolidateOptions,
) -> Result<(), String> {
    opts.book_name = std::path::Path::new(&*tab.title)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Surface::Sheet(v) = &mut tab.surface else {
        return Err("Consolidate needs a spreadsheet".into());
    };
    if v.pkg
        .workbook
        .sheets
        .get(sheet)
        .is_some_and(|s| s.is_protected())
    {
        return Err(PROTECTED.into());
    }
    let snap = v.snapshot();
    let before = v.pkg.workbook.sheets.clone();
    let (r1, c1, r2, c2) = match consolidate(&mut v.pkg.workbook, sheet, at, &opts) {
        Ok(area) => area,
        Err(e) => {
            // gridcore refuses before it changes anything.
            v.restore(snap);
            return Err(e.to_string());
        }
    };
    tab.dialogs.pop();
    if !sheets_differ(&before, &v.pkg.workbook.sheets) {
        tab.status = "Consolidate changed nothing".into();
        return Ok(());
    }
    v.push_undo_snapshot(snap);
    // Linked output is formulas: evaluate them now, or they show (and
    // save) blank.
    v.engine = crate::sheet_engine(&v.pkg.workbook);
    v.engine.recalc_all(&mut v.pkg.workbook);
    tab.set_dirty();
    tab.status = format!(
        "Consolidated into {}:{}",
        cell_name(r1, c1),
        cell_name(r2, c2)
    )
    .into();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Kind, SheetView, new_sheet_surface};
    use ctlcore::json::Json;
    use gridcore::sheet::{Cell, CellValue, Sheet};

    fn tab() -> DocTab {
        DocTab {
            kind: Kind::Xlsx,
            title: "Sales.xlsx".into(),
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

    /// East and West (Jan over A1:B3) and an empty Summary, active, with
    /// the cursor on A1.
    fn book() -> DocTab {
        let mut t = tab();
        let v = view(&mut t);
        let wb = &mut v.pkg.workbook;
        wb.sheets[0].name = "East".into();
        for name in ["West", "Summary"] {
            wb.sheets.push(Sheet {
                name: name.into(),
                ..Sheet::default()
            });
        }
        for (s, rows) in [
            (0, [("A", 1.0), ("B", 2.0)]),
            (1, [("b", 10.0), ("C", 20.0)]),
        ] {
            let sh = &mut wb.sheets[s];
            sh.set_cell(0, 1, Cell::text("Jan"));
            for (i, (k, n)) in rows.iter().enumerate() {
                sh.set_cell(i as u32 + 1, 0, Cell::text(k));
                sh.set_cell(i as u32 + 1, 1, Cell::number(*n));
            }
        }
        v.active = 2;
        v.anchor = (0, 0);
        v.sel = (0, 0);
        v.engine = crate::sheet_engine(&v.pkg.workbook);
        t
    }

    fn open(t: &mut DocTab) {
        let d = consolidate_dialog(t).unwrap();
        t.dialogs.push(d);
    }

    fn press(t: &mut DocTab, button: &str) -> Result<(), String> {
        crate::dialog_host::dialog_click(t, button)
    }

    fn set(t: &mut DocTab, control: &str, value: Json) {
        t.dialogs
            .set(control, &Json::obj(vec![("value", value)]))
            .unwrap();
    }

    fn add(t: &mut DocTab, r: &str) {
        set(t, "reference", Json::Str(r.into()));
        press(t, "Add").unwrap();
    }

    fn value(t: &mut DocTab, sheet: usize, r: u32, c: u32) -> Option<CellValue> {
        view(t).pkg.workbook.sheets[sheet]
            .cell(r, c)
            .map(|c| c.value.clone())
    }

    #[test]
    fn the_consolidate_dialog_maps_to_options() {
        let mut t = book();
        open(&mut t);
        let top = t.dialogs.top().unwrap();
        assert_eq!(top.id, "consolidate");
        assert_eq!(consolidate_options(top), ConsolidateOptions::default());
        set(&mut t, "function", Json::Str("Count Numbers".into()));
        add(&mut t, "East!A1:B3");
        set(&mut t, "reference", Json::Str("West!A1:B3".into()));
        set(&mut t, "top", Json::Bool(true));
        set(&mut t, "links", Json::Bool(true));
        assert_eq!(
            consolidate_options(t.dialogs.top().unwrap()),
            ConsolidateOptions {
                func: SubtotalFunc::CountNums,
                // Added, a reference is kept the way the list shows it; a
                // typed one counts on OK without Add.
                refs: vec!["East!$A$1:$B$3".into(), "West!A1:B3".into()],
                top_row: true,
                left_col: false,
                links: true,
                book_name: String::new(),
            }
        );
    }

    #[test]
    fn add_and_delete_edit_the_reference_list_and_keep_the_dialog() {
        let mut t = book();
        open(&mut t);
        add(&mut t, "East!A1:B3");
        add(&mut t, "West!A1:B3");
        // The same range spelled another way is not a second entry; a
        // reference that does not parse is kept as typed, for OK to report.
        add(&mut t, "east!$a$1:b3");
        add(&mut t, "Nowhere!A1");
        assert_eq!(
            listed(t.dialogs.top().unwrap()),
            ["East!$A$1:$B$3", "West!$A$1:$B$3", "Nowhere!A1"]
        );
        assert_eq!(typed(t.dialogs.top().unwrap()), "");
        // Delete takes the chosen entry...
        set(&mut t, "refs", Json::Str("Nowhere!A1".into()));
        press(&mut t, "Delete").unwrap();
        set(&mut t, "refs", Json::Str("East!$A$1:$B$3".into()));
        press(&mut t, "Delete").unwrap();
        assert_eq!(listed(t.dialogs.top().unwrap()), ["West!$A$1:$B$3"]);
        // ...or the one the box names, in any spelling.
        set(&mut t, "reference", Json::Str("west!A1:B3".into()));
        press(&mut t, "Delete").unwrap();
        assert!(listed(t.dialogs.top().unwrap()).is_empty());
        assert_eq!(t.dialogs.top().map(|d| d.id), Some("consolidate"));
        assert_eq!(view(&mut t).undo.len(), 0);
        press(&mut t, "Close").unwrap();
        assert!(t.dialogs.top().is_none());
    }

    #[test]
    fn consolidate_ok_writes_and_is_one_undo_step() {
        let mut t = book();
        open(&mut t);
        add(&mut t, "East!A1:B3");
        add(&mut t, "West!A1:B3");
        set(&mut t, "top", Json::Bool(true));
        set(&mut t, "left", Json::Bool(true));
        set(&mut t, "links", Json::Bool(true));
        press(&mut t, "OK").unwrap();
        assert!(t.dialogs.top().is_none());
        assert!(t.dirty);
        assert_eq!(view(&mut t).undo.len(), 1);
        assert_eq!(value(&mut t, 2, 0, 2), Some(CellValue::Text("Jan".into())));
        assert_eq!(
            value(&mut t, 2, 1, 1),
            Some(CellValue::Text("Sales".into()))
        );
        assert_eq!(value(&mut t, 2, 2, 0), Some(CellValue::Text("A".into())));
        let s = &view(&mut t).pkg.workbook.sheets[2];
        assert_eq!(
            s.cell(5, 2).and_then(|c| c.formula.clone()).as_deref(),
            Some("SUM(C4:C5)")
        );
        assert!(s.row_hidden(1) && s.row_collapsed(2));
        // The formulas are evaluated: B's summary is East 2 + West's b 10.
        assert_eq!(value(&mut t, 2, 5, 2), Some(CellValue::Number(12.0)));
        assert_eq!(value(&mut t, 2, 2, 2), Some(CellValue::Number(1.0)));
        // It reopens on what OK kept.
        open(&mut t);
        let top = t.dialogs.top().unwrap();
        assert_eq!(listed(top), ["East!$A$1:$B$3", "West!$A$1:$B$3"]);
        assert!(checked(top, "top") && checked(top, "left") && checked(top, "links"));
        press(&mut t, "Close").unwrap();
        assert!(view(&mut t).undo_step());
        assert_eq!(value(&mut t, 2, 0, 2), None);
        assert_eq!(view(&mut t).pkg.workbook.sheets[2].consolidate, None);
    }

    #[test]
    fn a_refused_consolidate_leaves_every_sheet_and_keeps_the_dialog() {
        let mut t = book();
        let before = view(&mut t).pkg.workbook.sheets.clone();
        open(&mut t);
        // Links with a source on the destination sheet.
        add(&mut t, "East!A1:B3");
        add(&mut t, "A1:B3");
        set(&mut t, "links", Json::Bool(true));
        let err = press(&mut t, "OK").unwrap_err();
        assert_eq!(
            err,
            gridcore::edit::ConsolidateError::LinksOnDestSheet.to_string()
        );
        assert_eq!(t.dialogs.top().map(|d| d.id), Some("consolidate"));
        // An unknown sheet.
        set(&mut t, "links", Json::Bool(false));
        add(&mut t, "Nowhere!A1");
        let err = press(&mut t, "OK").unwrap_err();
        assert!(err.contains("Nowhere!A1"), "{err}");
        assert!(!sheets_differ(&before, &view(&mut t).pkg.workbook.sheets));
        assert_eq!(view(&mut t).undo.len(), 0);
        assert!(!t.dirty);
        // A protected destination refuses it.
        press(&mut t, "Close").unwrap();
        view(&mut t).pkg.workbook.sheets[2].set_protected(true);
        open(&mut t);
        add(&mut t, "East!A1:B3");
        assert_eq!(press(&mut t, "OK").unwrap_err(), PROTECTED);
        assert!(!sheets_differ(
            &before[..2],
            &view(&mut t).pkg.workbook.sheets[..2]
        ));
        assert_eq!(view(&mut t).pkg.workbook.sheets[2].cell(0, 0), None);
    }
}
