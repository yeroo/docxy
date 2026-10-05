//! Home › Clear and Find & Select's Go To and Go To Special (#671).
//!
//! Clear is a [`SheetView`] edit over every area of the selection
//! ([`gridcore::edit::clear_plan`]). Go To (F5, Ctrl+G) and Go To Special are
//! dialogs on the tab's [`crate::dialog::DialogStack`], so the harness's
//! `dialog-read`/`dialog-set`/`dialog-click` drive them: Go To's OK selects
//! the reference or name, its Special… opens Go To Special, whose OK selects
//! what it finds as a multi-area selection (#670).

use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::{DocTab, SheetView, Surface};
use gridcore::edit::{ClearWhat, GoSpecial, Types, apply_clear_sheet, clear_plan};

/// Excel's refusal of an edit on a protected sheet.
pub(crate) const SHEET_PROTECTED: &str =
    "The cell or chart you're trying to change is on a protected sheet.";

/// Excel's refusal of a Go To reference that names nothing.
pub(crate) const BAD_REFERENCE: &str = "Reference isn't valid.";

impl SheetView {
    /// The cells of the active sheet that carry a note or a comment.
    pub(crate) fn note_cells(&self) -> Vec<(u32, u32)> {
        let s = self.active;
        self.pkg
            .comments()
            .into_iter()
            .filter(|c| c.sheet == s)
            .map(|c| (c.row, c.col))
            .collect()
    }

    /// Home › Clear `what` over every area, as one undo step (a package
    /// step when notes go, since they live in the parts). `Ok(false)` when
    /// there was nothing to clear; refused when it would change part of an
    /// array or split a merge.
    pub(crate) fn clear_what(&mut self, what: ClearWhat) -> Result<bool, String> {
        let s = self.active;
        if self.sheet().is_protected() {
            return Err(SHEET_PROTECTED.into());
        }
        let areas = self.areas_all();
        let plan = clear_plan(self.sheet(), &areas, what, &self.note_cells())?;
        if plan.is_empty() {
            return Ok(false);
        }
        if self.refuses(s, &plan.cells) {
            return Err(gridcore::engine::PART_OF_ARRAY.into());
        }
        if plan.notes.is_empty() {
            self.push_undo();
        } else {
            self.push_undo_snapshot(self.snapshot_package());
        }
        self.engine
            .set_cells_prechecked(&mut self.pkg.workbook, s, plan.cells.clone());
        apply_clear_sheet(&mut self.pkg.workbook.sheets[s], &plan);
        for &(r, c) in &plan.notes {
            self.pkg.remove_comment(s, r, c);
        }
        Ok(true)
    }
}

/// Go To Special's choices, in the dialog's order.
const KINDS: [&str; 14] = [
    "Notes",
    "Constants",
    "Formulas",
    "Blanks",
    "Current region",
    "Current array",
    "Row differences",
    "Column differences",
    "Precedents",
    "Dependents",
    "Last cell",
    "Visible cells only",
    "Conditional formats",
    "Data validation",
];

fn sheet_view(tab: &DocTab) -> Result<&SheetView, String> {
    match &tab.surface {
        Surface::Sheet(v) => Ok(v),
        _ => Err("Go To needs a spreadsheet".into()),
    }
}

fn default_ok(mut d: Dialog) -> Dialog {
    d.buttons = d
        .buttons
        .into_iter()
        .map(|b| Button {
            default: b.label == "OK",
            ..b
        })
        .collect();
    d
}

/// Go To: the defined names, a Reference box and Special….
pub(crate) fn goto_dialog(tab: &DocTab) -> Result<Dialog, String> {
    let v = sheet_view(tab)?;
    let mut d = Dialog::message(
        "goto",
        "Go To",
        String::new(),
        &[
            ("Special\u{2026}", ButtonRole::Accept),
            ("OK", ButtonRole::Accept),
            ("Cancel", ButtonRole::Cancel),
        ],
        DialogOwner::GoTo,
    );
    d.text = None;
    let mut names = Control::new("names", "Go to:", ControlKind::List, Value::Choice(None));
    names.items = v
        .pkg
        .workbook
        .defined_names
        .iter()
        .filter(|n| n.scope.is_none_or(|s| s == v.active) && !n.name.starts_with("_xlnm"))
        .map(|n| n.name.clone())
        .collect();
    d.controls = vec![
        names,
        Control::new(
            "reference",
            "Reference:",
            ControlKind::Text,
            Value::Text(String::new()),
        ),
    ];
    let mut d = default_ok(d);
    d.mark_opened();
    Ok(d)
}

/// Go To Special: what to find, and the options of the kinds that take one.
pub(crate) fn special_dialog(tab: &DocTab) -> Result<Dialog, String> {
    sheet_view(tab)?;
    let mut d = Dialog::message(
        "goto-special",
        "Go To Special",
        String::new(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::GoToSpecial,
    );
    d.text = None;
    let mut kind = Control::new(
        "select",
        "Select",
        ControlKind::Radio,
        Value::Choice(Some(0)),
    );
    kind.items = KINDS.iter().map(|s| s.to_string()).collect();
    let mut levels = Control::new("levels", "", ControlKind::Radio, Value::Choice(Some(0)));
    levels.items = vec!["Direct only".into(), "All levels".into()];
    let mut rules = Control::new("rules", "", ControlKind::Radio, Value::Choice(Some(0)));
    rules.items = vec!["All".into(), "Same".into()];
    d.controls = vec![
        kind,
        Control::new(
            "numbers",
            "Numbers",
            ControlKind::Checkbox,
            Value::Bool(true),
        ),
        Control::new("text", "Text", ControlKind::Checkbox, Value::Bool(true)),
        Control::new(
            "logicals",
            "Logicals",
            ControlKind::Checkbox,
            Value::Bool(true),
        ),
        Control::new("errors", "Errors", ControlKind::Checkbox, Value::Bool(true)),
        levels,
        rules,
    ];
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

fn text(d: &Dialog, name: &str) -> String {
    d.controls
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.text())
        .unwrap_or_default()
}

/// The kind Go To Special's controls stage.
pub(crate) fn staged_kind(d: &Dialog) -> GoSpecial {
    let types = Types {
        numbers: checked(d, "numbers"),
        text: checked(d, "text"),
        logicals: checked(d, "logicals"),
        errors: checked(d, "errors"),
    };
    let all = choice(d, "levels") == Some(1);
    let same = choice(d, "rules") == Some(1);
    match choice(d, "select").unwrap_or(0) {
        1 => GoSpecial::Constants(types),
        2 => GoSpecial::Formulas(types),
        3 => GoSpecial::Blanks,
        4 => GoSpecial::CurrentRegion,
        5 => GoSpecial::CurrentArray,
        6 => GoSpecial::RowDifferences,
        7 => GoSpecial::ColumnDifferences,
        8 => GoSpecial::Precedents { all },
        9 => GoSpecial::Dependents { all },
        10 => GoSpecial::LastCell,
        11 => GoSpecial::VisibleCells,
        12 => GoSpecial::ConditionalFormats { same },
        13 => GoSpecial::DataValidation { same },
        _ => GoSpecial::Notes,
    }
}

fn presses(d: &Dialog, button: &str, label: &str) -> bool {
    let b = button.replace('&', "");
    let b = b
        .trim()
        .trim_end_matches('\u{2026}')
        .trim_end_matches("...");
    let l = label.trim_end_matches('\u{2026}');
    b.eq_ignore_ascii_case(l) && d.buttons.iter().any(|x| x.label == label && x.enabled)
}

/// Select `rect` on `sheet` as Go To does: the active cell at its top-left.
fn go_to(v: &mut SheetView, sheet: usize, (r0, c0, r1, c1): (u32, u32, u32, u32)) {
    v.active = sheet;
    v.sel = (r0, c0);
    v.anchor = (r1, c1);
    v.clear_areas();
    v.end_cell_edit();
    // Scrolled into view, as a jump to a precedent is (#707 r5 M3).
    v.reveal((r0, c0, r1, c1));
}

/// A press the Go To dialogs handle: Go To's OK and Special…, Go To
/// Special's OK. `None` for any other press (Cancel closes through the
/// stack).
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    match top.owner {
        DialogOwner::GoTo if presses(top, button, "Special\u{2026}") => {
            tab.dialogs.pop();
            Some(special_dialog(tab).map(|d| tab.dialogs.push(d)))
        }
        DialogOwner::GoTo if presses(top, button, "OK") => {
            let typed = text(top, "reference");
            let named = choice(top, "names").and_then(|i| {
                top.controls
                    .iter()
                    .find(|c| c.name == "names")
                    .and_then(|c| c.items.get(i).cloned())
            });
            let target = if typed.trim().is_empty() {
                named.unwrap_or_default()
            } else {
                typed
            };
            let Surface::Sheet(v) = &mut tab.surface else {
                return Some(Err("Go To needs a spreadsheet".into()));
            };
            let Some((sheet, rect)) =
                gridcore::edit::resolve_reference(&v.pkg.workbook, v.active, &target)
            else {
                return Some(Err(BAD_REFERENCE.into()));
            };
            go_to(v, sheet, rect);
            tab.dialogs.pop();
            Some(Ok(()))
        }
        DialogOwner::GoToSpecial if presses(top, button, "OK") => {
            let kind = staged_kind(top);
            tab.dialogs.pop();
            let Surface::Sheet(v) = &mut tab.surface else {
                return Some(Err("Go To Special needs a spreadsheet".into()));
            };
            let found = gridcore::edit::go_to_special(
                &v.pkg.workbook,
                v.active,
                &v.areas_all(),
                v.sel,
                kind,
                &v.note_cells(),
            );
            match found {
                Ok(rects) => {
                    v.set_areas(&rects);
                    tab.status = format!(
                        "{} area{} selected",
                        rects.len(),
                        if rects.len() == 1 { "" } else { "s" }
                    )
                    .into();
                }
                // Excel's message; the selection stays as it was.
                Err(why) => tab.status = why.into(),
            }
            Some(Ok(()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Kind, new_sheet_surface};
    use gridcore::sheet::{Cell, CellValue, parse_cell_name};

    fn tab() -> DocTab {
        DocTab {
            kind: Kind::Xlsx,
            title: "book.xlsx".into(),
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
        parse_cell_name(name).unwrap()
    }

    fn put(t: &mut DocTab, name: &str, cell: Cell) {
        let (r, c) = at(name);
        let v = view(t);
        let s = v.active;
        v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), cell);
    }

    fn set(t: &mut DocTab, name: &str, value: Value) {
        let d = t.dialogs.top_dialog_mut().expect("a dialog");
        let c = d.controls.iter_mut().find(|c| c.name == name).unwrap();
        c.value = value;
    }

    #[test]
    fn go_to_selects_a_reference_and_refuses_a_bad_one() {
        let mut t = tab();
        t.dialogs.push(goto_dialog(&t).unwrap());
        set(&mut t, "reference", Value::Text("B2:C4".into()));
        assert_eq!(click(&mut t, "OK"), Some(Ok(())));
        assert!(t.dialogs.top().is_none());
        let v = view(&mut t);
        assert_eq!((v.sel, v.anchor), (at("B2"), at("C4")));
        t.dialogs.push(goto_dialog(&t).unwrap());
        set(&mut t, "reference", Value::Text("banana".into()));
        assert_eq!(click(&mut t, "OK"), Some(Err(BAD_REFERENCE.into())));
        assert!(t.dialogs.top().is_some(), "stays open");
    }

    /// #707 r5 M3: Go To scrolls the target into view, on its own sheet.
    #[test]
    fn go_to_reveals_the_target_and_switches_sheets() {
        let mut t = tab();
        let v = view(&mut t);
        v.pkg.add_sheet("Far");
        t.dialogs.push(goto_dialog(&t).unwrap());
        set(&mut t, "reference", Value::Text("Far!A5000:C5001".into()));
        assert_eq!(click(&mut t, "OK"), Some(Ok(())));
        let v = view(&mut t);
        assert_eq!(v.active, 1);
        assert_eq!(v.sel, at("A5000"));
        assert_eq!(v.reveal_col, Some(2), "the far column waits for the render");
    }

    #[test]
    fn go_to_lists_the_names_and_goes_to_one() {
        let mut t = tab();
        view(&mut t)
            .pkg
            .workbook
            .defined_names
            .push(gridcore::sheet::DefinedName {
                name: "Rate".into(),
                scope: None,
                formula: "Sheet1!$D$9".into(),
            });
        t.dialogs.push(goto_dialog(&t).unwrap());
        let names = &t.dialogs.top().unwrap().controls[0];
        assert_eq!(names.items, vec!["Rate".to_string()]);
        set(&mut t, "names", Value::Choice(Some(0)));
        assert_eq!(click(&mut t, "OK"), Some(Ok(())));
        assert_eq!(view(&mut t).sel, at("D9"));
    }

    /// #671's QA case through the dialogs: Go To › Special… › Blanks over
    /// A1:A10 selects A2:A4 and A6:A9, with A2 active (R6); `=A1` and
    /// Ctrl+Enter then fill the grouping column from above.
    #[test]
    fn special_blanks_then_ctrl_enter_fills_the_grouping_column() {
        let mut t = tab();
        for n in ["A1", "A5", "A10"] {
            put(&mut t, n, Cell::text(n));
        }
        let v = view(&mut t);
        v.anchor = at("A1");
        v.sel = at("A10");
        t.dialogs.push(goto_dialog(&t).unwrap());
        assert_eq!(click(&mut t, "Special..."), Some(Ok(())));
        assert_eq!(t.dialogs.top().unwrap().owner, DialogOwner::GoToSpecial);
        set(&mut t, "select", Value::Choice(Some(3)));
        assert_eq!(click(&mut t, "OK"), Some(Ok(())));
        let v = view(&mut t);
        assert_eq!(v.sel, at("A2"));
        assert_eq!(
            v.areas_all(),
            vec![(5, 0, 8, 0), (1, 0, 3, 0)],
            "the active area (last) is the one with A2"
        );
        v.begin_cell_edit(Some("=A1".into()));
        assert!(v.commit_edit_to_selection());
        let f = |v: &SheetView, n: &str| {
            let (r, c) = at(n);
            v.sheet().cell(r, c).and_then(|c| c.formula.clone())
        };
        assert_eq!(f(v, "A2").as_deref(), Some("A1"));
        assert_eq!(f(v, "A4").as_deref(), Some("A3"));
        assert_eq!(f(v, "A6").as_deref(), Some("A5"));
        assert_eq!(f(v, "A9").as_deref(), Some("A8"));
        assert_eq!(f(v, "A5"), None, "a constant between stays");
    }

    #[test]
    fn special_with_nothing_found_says_so_and_keeps_the_selection() {
        let mut t = tab();
        put(&mut t, "A1", Cell::number(1.0));
        let v = view(&mut t);
        v.anchor = at("A1");
        v.sel = at("A1");
        t.dialogs.push(special_dialog(&t).unwrap());
        set(&mut t, "select", Value::Choice(Some(2)));
        assert_eq!(click(&mut t, "OK"), Some(Ok(())));
        assert_eq!(t.status.to_string(), gridcore::edit::NO_CELLS);
        assert_eq!(view(&mut t).areas_all(), vec![(0, 0, 0, 0)]);
    }

    #[test]
    fn clear_items_take_their_part_of_every_area_as_one_step() {
        let mut t = tab();
        put(&mut t, "A1", Cell::number(1.0));
        put(&mut t, "C3", Cell::number(3.0));
        let v = view(&mut t);
        let bold = v.pkg.workbook.styles.intern(gridcore::sheet::Xf {
            bold: true,
            ..Default::default()
        });
        for (r, c) in [(0, 0), (2, 2)] {
            v.pkg.workbook.sheets[0]
                .cells
                .get_mut(&(r, c))
                .unwrap()
                .style = bold;
        }
        v.anchor = at("A1");
        v.sel = at("A1");
        v.add_area(at("C3"));
        assert_eq!(v.clear_what(ClearWhat::Formats), Ok(true));
        assert_eq!(v.undo.len(), 1);
        for (r, c) in [(0, 0), (2, 2)] {
            let cell = v.sheet().cell(r, c).unwrap();
            assert_eq!((cell.style, cell.value.clone()), (0, cell.value.clone()));
            assert!(matches!(cell.value, CellValue::Number(_)));
        }
        assert_eq!(v.clear_what(ClearWhat::Formats), Ok(false), "nothing left");
        assert_eq!(v.undo.len(), 1, "no step for nothing");
        assert_eq!(v.clear_what(ClearWhat::All), Ok(true));
        assert!(v.sheet().cell(2, 2).is_none());
    }

    #[test]
    fn clear_comments_removes_the_note_as_a_package_step() {
        let mut t = tab();
        let v = view(&mut t);
        v.anchor = at("B2");
        v.sel = at("B2");
        assert!(v.pkg.set_comment(0, 1, 1, "Ann", "a note"));
        assert_eq!(v.note_cells(), vec![(1, 1)]);
        assert_eq!(v.clear_what(ClearWhat::Comments), Ok(true));
        assert!(v.note_cells().is_empty());
        assert!(v.undo_step(), "undo brings it back");
        assert_eq!(v.note_cells(), vec![(1, 1)]);
    }

    #[test]
    fn clear_hyperlinks_keeps_the_style_and_remove_drops_it() {
        for (what, keeps) in [
            (ClearWhat::Hyperlinks, true),
            (ClearWhat::RemoveHyperlinks, false),
        ] {
            let mut t = tab();
            put(&mut t, "A1", Cell::text("link"));
            let v = view(&mut t);
            let blue = v.pkg.workbook.styles.intern(gridcore::sheet::Xf {
                italic: true,
                ..Default::default()
            });
            v.pkg.workbook.sheets[0]
                .cells
                .get_mut(&(0, 0))
                .unwrap()
                .style = blue;
            v.pkg.workbook.sheets[0]
                .hyperlinks
                .insert((0, 0), "https://example.com".into());
            v.anchor = at("A1");
            v.sel = at("A1");
            assert_eq!(v.clear_what(what), Ok(true));
            assert!(v.sheet().hyperlinks.is_empty());
            let style = v.sheet().cell(0, 0).unwrap().style;
            assert_eq!(style == blue, keeps, "{what:?}");
        }
    }
}
