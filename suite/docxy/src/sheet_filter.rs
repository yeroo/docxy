//! Data › Sort & Filter's filters (#690): the AutoFilter commands, the
//! filter button's drop-down, Custom AutoFilter, Top 10 and Advanced
//! Filter, over [`gridcore::filter`].
//!
//! The drop-down and its child dialogs sit on the tab's
//! [`crate::dialog::DialogStack`], so the harness's `dialog-*` verbs drive
//! them. Dialogs are modal and centred, the drop-down too: it is not
//! anchored under its button. Every command is one undo step, leaves the
//! tab clean when it changed nothing, and puts `<n> of <m> records found`
//! in the status line.

use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::{DocTab, Surface};
use gridcore::filter::{AdvancedFilter, ByCell, ColumnFilter, FilterError, FilterOutcome, Submenu};
use gridcore::sheet::{Workbook, cell_name, col_name};

type Area = (u32, u32, u32, u32);

/// Excel's refusal on a protected sheet.
const PROTECTED: &str =
    "The sheet is protected: unprotect it (Review › Protect Sheet) to filter it.";

/// Whether sheet `s` differs between `a` and `b` in what a filter command
/// writes: the rows' attributes and hidden flags, the filter, the cells (a
/// copy to another range) and the defined names (`_FilterDatabase`,
/// `Criteria`, `Extract`).
fn filter_changed(a: &Workbook, b: &Workbook, s: usize) -> bool {
    let (x, y) = (&a.sheets[s], &b.sheets[s]);
    x.row_attrs != y.row_attrs
        || x.filtered_rows != y.filtered_rows
        || x.auto_filter != y.auto_filter
        || x.cells != y.cells
        || a.defined_names != b.defined_names
}

/// Run filter command `op` on the tab's sheet as one undo step: commit an
/// open cell editor first, recalculate (SUBTOTAL skips filtered rows), mark
/// the tab dirty only when something changed, and say what it found.
pub(crate) fn run(
    tab: &mut DocTab,
    op: impl FnOnce(&mut Workbook, usize, f64) -> Result<FilterOutcome, FilterError>,
) -> Result<FilterOutcome, String> {
    let Surface::Sheet(v) = &mut tab.surface else {
        return Err("Filter needs a spreadsheet".into());
    };
    let s = v.active;
    if v.pkg.workbook.sheets[s].is_protected() {
        return Err(PROTECTED.into());
    }
    // The typed value is an edit whatever the command then does.
    crate::sheet_sort::commit_first(tab)?;
    let Surface::Sheet(v) = &mut tab.surface else {
        return Err("Filter needs a spreadsheet".into());
    };
    let today = v
        .engine
        .clock
        .or_else(gridcore::clock::local_now_serial)
        .unwrap_or(0.0);
    let snap = v.snapshot();
    let outcome = match op(&mut v.pkg.workbook, s, today) {
        Ok(o) => o,
        Err(e) => {
            v.restore(snap);
            tab.status = e.to_string().into();
            return Err(e.to_string());
        }
    };
    let changed = filter_changed(snap.workbook(), &v.pkg.workbook, s);
    if changed {
        v.push_undo_snapshot(snap);
        // The rebuilt engine keeps the clock the old one had.
        let clock = v.engine.clock;
        v.engine = crate::sheet_engine(&v.pkg.workbook);
        v.engine.clock = clock;
        v.engine.recalc_all(&mut v.pkg.workbook);
    }
    if changed {
        tab.set_dirty();
    }
    tab.status = gridcore::filter::status_text(&outcome).into();
    Ok(outcome)
}

/// Data › Filter (Ctrl+Shift+L): buttons on over the selection, or the list
/// around the cursor; off when the sheet has them.
pub(crate) fn toggle(tab: &mut DocTab) -> Result<(), String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("Filter needs a spreadsheet".into());
    };
    let on = v.sheet().auto_filter.is_some();
    let (sel, at) = (crate::sel_range(v.sel, v.anchor), v.sel);
    let multi = (sel.0, sel.1) != (sel.2, sel.3);
    let range = v.sheet().auto_filter.as_ref().map(|a| a.range);
    run(tab, |wb, s, _| {
        let r = if on {
            gridcore::filter::auto_filter_off(wb, s);
            range.unwrap_or_default()
        } else if multi {
            gridcore::filter::auto_filter_on_range(wb, s, sel)?
        } else {
            gridcore::filter::auto_filter_on(wb, s, at)?
        };
        let total = r.2.saturating_sub(r.0) as usize;
        Ok(FilterOutcome {
            shown: total,
            total,
        })
    })?;
    tab.status = if on { "Filter off" } else { "Filter on" }.into();
    Ok(())
}

/// Filter by Selected Cell's Value / Color / Font Color / Icon (the cell
/// menu).
pub(crate) fn by_cell(tab: &mut DocTab, by: ByCell) -> Result<(), String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("Filter needs a spreadsheet".into());
    };
    let at = v.sel;
    run(tab, |wb, s, today| {
        gridcore::filter::filter_by_cell(wb, s, at, by, today)
    })
    .map(|_| ())
}

/// The typed submenu's entries for each column type: an entry ending `...`
/// asks (Custom AutoFilter or Top 10), the others apply at once.
fn typed_entries(submenu: Submenu) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = match submenu {
        Submenu::Text => vec![
            "Equals...",
            "Does Not Equal...",
            "Begins With...",
            "Ends With...",
            "Contains...",
            "Does Not Contain...",
        ],
        Submenu::Number => vec![
            "Equals...",
            "Does Not Equal...",
            "Greater Than...",
            "Greater Than Or Equal To...",
            "Less Than...",
            "Less Than Or Equal To...",
            "Between...",
            "Top 10...",
            "Above Average",
            "Below Average",
        ],
        Submenu::Date => {
            let mut d = vec![
                "Equals...",
                "Before...",
                "After...",
                "Between...",
                "Tomorrow",
                "Today",
                "Yesterday",
                "Next Week",
                "This Week",
                "Last Week",
                "Next Month",
                "This Month",
                "Last Month",
                "Next Quarter",
                "This Quarter",
                "Last Quarter",
                "Next Year",
                "This Year",
                "Last Year",
                "Year to Date",
            ];
            d.extend(PERIODS.iter().map(|(label, _)| *label));
            d
        }
    };
    v.push("Custom Filter...");
    v
}

/// All Dates in the Period: the label and its `dynamicFilter` type.
const PERIODS: [(&str, &str); 16] = [
    ("Quarter 1", "Q1"),
    ("Quarter 2", "Q2"),
    ("Quarter 3", "Q3"),
    ("Quarter 4", "Q4"),
    ("January", "M1"),
    ("February", "M2"),
    ("March", "M3"),
    ("April", "M4"),
    ("May", "M5"),
    ("June", "M6"),
    ("July", "M7"),
    ("August", "M8"),
    ("September", "M9"),
    ("October", "M10"),
    ("November", "M11"),
    ("December", "M12"),
];

/// A typed entry that applies at once: its `dynamicFilter` type.
fn dynamic_of(entry: &str) -> Option<String> {
    if let Some((_, k)) = PERIODS.iter().find(|(l, _)| *l == entry) {
        return Some(k.to_string());
    }
    let k = match entry {
        "Above Average" => "aboveAverage",
        "Below Average" => "belowAverage",
        "Year to Date" => "yearToDate",
        "Tomorrow" => "tomorrow",
        "Today" => "today",
        "Yesterday" => "yesterday",
        e => {
            // "Next Week" → nextWeek, "This Quarter" → thisQuarter.
            let mut words = e.split(' ');
            let (a, b) = (words.next()?, words.next()?);
            if words.next().is_some() || !matches!(a, "Next" | "This" | "Last") {
                return None;
            }
            return Some(format!("{}{b}", a.to_lowercase()));
        }
    };
    Some(k.to_string())
}

/// The Custom AutoFilter dialog's operators, as Excel lists them, and the
/// stored `(operator, value pattern)` each makes.
pub(crate) const OPERATORS: [(&str, &str, &str); 12] = [
    ("equals", "equal", "{}"),
    ("does not equal", "notEqual", "{}"),
    ("is greater than", "greaterThan", "{}"),
    ("is greater than or equal to", "greaterThanOrEqual", "{}"),
    ("is less than", "lessThan", "{}"),
    ("is less than or equal to", "lessThanOrEqual", "{}"),
    ("begins with", "equal", "{}*"),
    ("does not begin with", "notEqual", "{}*"),
    ("ends with", "equal", "*{}"),
    ("does not end with", "notEqual", "*{}"),
    ("contains", "equal", "*{}*"),
    ("does not contain", "notEqual", "*{}*"),
];

fn control<'a>(d: &'a Dialog, name: &str) -> Option<&'a Control> {
    d.controls.iter().find(|c| c.name == name)
}

fn control_mut<'a>(d: &'a mut Dialog, name: &str) -> Option<&'a mut Control> {
    d.controls.iter_mut().find(|c| c.name == name)
}

fn text_of(d: &Dialog, name: &str) -> String {
    match control(d, name).map(|c| &c.value) {
        Some(Value::Text(t)) => t.trim().to_string(),
        _ => String::new(),
    }
}

fn choice_of(d: &Dialog, name: &str) -> Option<String> {
    let c = control(d, name)?;
    match c.value {
        Value::Choice(Some(i)) => c.items.get(i).cloned(),
        _ => None,
    }
}

fn checked(d: &Dialog, name: &str) -> bool {
    control(d, name).is_some_and(|c| c.value == Value::Bool(true))
}

fn dialog(
    id: &'static str,
    title: String,
    buttons: &[(&str, ButtonRole)],
    owner: DialogOwner,
) -> Dialog {
    let mut d = Dialog::message(id, &title, String::new(), buttons, owner);
    d.text = None;
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

/// The drop-down of column `col` of the sheet's AutoFilter: sort, clear,
/// the typed submenu, a search box and the value checklist.
pub(crate) fn menu_dialog(tab: &DocTab, col: u32) -> Result<Dialog, String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("Filter needs a spreadsheet".into());
    };
    let m =
        gridcore::filter::menu(&v.pkg.workbook, v.active, col, None).map_err(|e| e.to_string())?;
    let header = if m.header.is_empty() {
        format!("Column {}", col_name(col))
    } else {
        m.header.clone()
    };
    let mut d = dialog(
        "filter-menu",
        format!("Filter: {header}"),
        &[
            ("Sort A to Z", ButtonRole::Apply),
            ("Sort Z to A", ButtonRole::Apply),
            ("Clear Filter", ButtonRole::Apply),
            ("Sort by Color", ButtonRole::Apply),
            ("Filter by Color", ButtonRole::Apply),
            ("Apply Filter", ButtonRole::Apply),
            ("Search", ButtonRole::Apply),
            ("OK", ButtonRole::Accept),
            ("Cancel", ButtonRole::Cancel),
        ],
        DialogOwner::FilterMenu {
            sheet: v.active,
            col,
        },
    );
    let mut typed = Control::new(
        "typed",
        &format!("{}:", m.submenu.label()),
        ControlKind::Dropdown,
        Value::Choice(None),
    );
    typed.items = typed_entries(m.submenu)
        .into_iter()
        .map(String::from)
        .collect();
    let mut note = Control::new(
        "note",
        "",
        ControlKind::Label,
        Value::Text(if m.truncated {
            "Not all items showing".into()
        } else {
            String::new()
        }),
    );
    note.visible = m.truncated;
    // Sort by Color and Filter by Color: the colours the records show.
    let mut color = Control::new(
        "color",
        "By color:",
        ControlKind::Dropdown,
        Value::Choice(None),
    );
    color.items = m.colors.iter().map(|c| c.label()).collect();
    if !color.items.is_empty() {
        color.value = Value::Choice(Some(0));
    }
    color.enabled = !color.items.is_empty();
    for b in d.buttons.iter_mut() {
        if matches!(b.label.as_str(), "Sort by Color" | "Filter by Color") {
            b.enabled = !m.colors.is_empty();
        }
    }
    // The search the checklist was last built for: OK reads the list it
    // shows, not what the box says now.
    let mut searched = Control::new(
        "searched",
        "",
        ControlKind::Text,
        Value::Text(String::new()),
    );
    searched.visible = false;
    d.controls = vec![
        color,
        typed,
        Control::new(
            "search",
            "Search:",
            ControlKind::Text,
            Value::Text(String::new()),
        ),
        Control::new(
            "add",
            "Add current selection to filter",
            ControlKind::Checkbox,
            Value::Bool(false),
        ),
        values_control(&m, false),
        note,
        searched,
    ];
    d.mark_opened();
    Ok(d)
}

/// The checklist: `(Select All)` (or, searching, `(Select All Search
/// Results)`) over the values.
fn values_control(m: &gridcore::filter::FilterMenu, searching: bool) -> Control {
    let mut c = Control::new(
        "values",
        "Values",
        ControlKind::CheckList,
        Value::Checks(Vec::new()),
    );
    let all = if searching {
        "(Select All Search Results)"
    } else {
        "(Select All)"
    };
    c.items = std::iter::once(all.to_string())
        .chain(m.items.iter().map(|i| i.label.clone()))
        .collect();
    c.depths = std::iter::once(0)
        .chain(m.items.iter().map(|i| i.depth + 1))
        .collect();
    let checks: Vec<bool> = m.items.iter().map(|i| searching || i.checked).collect();
    c.value = Value::Checks(
        std::iter::once(checks.iter().all(|x| *x))
            .chain(checks)
            .collect(),
    );
    c
}

/// Custom AutoFilter for column `col`, its first operator preset.
pub(crate) fn custom_dialog(
    sheet: usize,
    col: u32,
    header: &str,
    op1: usize,
    date: bool,
) -> Dialog {
    let mut d = dialog(
        "custom-autofilter",
        "Custom AutoFilter".into(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::CustomFilter { sheet, col, date },
    );
    let ops: Vec<String> = OPERATORS.iter().map(|o| o.0.to_string()).collect();
    let mut first = Control::new(
        "op1",
        &format!("Show rows where {header}:"),
        ControlKind::Dropdown,
        Value::Choice(Some(op1)),
    );
    first.items = ops.clone();
    let mut join = Control::new("join", "", ControlKind::Radio, Value::Choice(Some(0)));
    join.items = vec!["And".into(), "Or".into()];
    let mut second = Control::new("op2", "", ControlKind::Dropdown, Value::Choice(None));
    second.items = ops;
    d.controls = vec![
        first,
        Control::new("val1", "", ControlKind::Text, Value::Text(String::new())),
        join,
        second,
        Control::new("val2", "", ControlKind::Text, Value::Text(String::new())),
        Control::new(
            "hint",
            "",
            ControlKind::Label,
            Value::Text("Use ? for any single character, * for any series of characters".into()),
        ),
    ];
    d.mark_opened();
    d
}

/// Top 10 AutoFilter for column `col`.
pub(crate) fn top10_dialog(sheet: usize, col: u32) -> Dialog {
    let mut d = dialog(
        "top10-autofilter",
        "Top 10 AutoFilter".into(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::Top10Filter { sheet, col },
    );
    let mut which = Control::new(
        "which",
        "Show",
        ControlKind::Dropdown,
        Value::Choice(Some(0)),
    );
    which.items = vec!["Top".into(), "Bottom".into()];
    let mut unit = Control::new("unit", "", ControlKind::Dropdown, Value::Choice(Some(0)));
    unit.items = vec!["Items".into(), "Percent".into()];
    d.controls = vec![
        which,
        Control::new("n", "", ControlKind::Number, Value::Text("10".into())),
        unit,
    ];
    d.mark_opened();
    d
}

/// Advanced Filter, its list the region around the cursor.
pub(crate) fn advanced_dialog(tab: &DocTab) -> Result<Dialog, String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("Advanced Filter needs a spreadsheet".into());
    };
    let list = gridcore::edit::sort_region(&v.pkg.workbook, v.active, v.sel)
        .map(|(a, _)| area_text(a))
        .unwrap_or_default();
    let mut d = dialog(
        "advanced-filter",
        "Advanced Filter".into(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::AdvancedFilter { sheet: v.active },
    );
    let mut action = Control::new(
        "action",
        "Action",
        ControlKind::Radio,
        Value::Choice(Some(0)),
    );
    action.items = vec![
        "Filter the list, in-place".into(),
        "Copy to another location".into(),
    ];
    d.controls = vec![
        action,
        Control::new("list", "List range:", ControlKind::Text, Value::Text(list)),
        Control::new(
            "criteria",
            "Criteria range:",
            ControlKind::Text,
            Value::Text(String::new()),
        ),
        Control::new(
            "copy",
            "Copy to:",
            ControlKind::Text,
            Value::Text(String::new()),
        ),
        Control::new(
            "unique",
            "Unique records only",
            ControlKind::Checkbox,
            Value::Bool(false),
        ),
    ];
    d.mark_opened();
    Ok(d)
}

fn area_text((r1, c1, r2, c2): (u32, u32, u32, u32)) -> String {
    format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
}

/// A range typed in a dialog (`A1:C9`, `$E$1:$E$2`, `Sheet2!A1`) as (sheet,
/// area); a bare one is on `default`.
fn typed_range(wb: &Workbook, text: &str, default: usize) -> Result<(usize, Area), String> {
    let text = text.trim().trim_start_matches('=');
    let (sheet, refs) = match text.rsplit_once('!') {
        Some((sh, r)) => {
            let name = sh.trim().trim_matches('\'').replace("''", "'");
            let i = wb
                .sheets
                .iter()
                .position(|s| s.name.eq_ignore_ascii_case(&name))
                .ok_or_else(|| format!("There is no sheet named {name}"))?;
            (i, r)
        }
        None => (default, text),
    };
    let refs = refs.replace('$', "");
    gridcore::sheet::parse_range_name(&refs)
        .or_else(|| gridcore::sheet::parse_cell_name(&refs).map(|(r, c)| (r, c, r, c)))
        .map(|a| (sheet, a))
        .ok_or_else(|| format!("The reference \"{text}\" isn't valid"))
}

/// A date typed into Custom AutoFilter (`2024-03-13`) compares as its
/// serial against date cells; anything else is kept as typed.
fn operand(text: &str, date1904: bool) -> String {
    let t = text.trim();
    let mut p = t.split('-');
    if let (Some(y), Some(m), Some(d), None) = (p.next(), p.next(), p.next(), p.next()) {
        if let (Ok(y), Ok(m), Ok(d)) = (y.parse::<i64>(), m.parse::<u32>(), d.parse::<u32>()) {
            if (1..=12).contains(&m) && (1..=31).contains(&d) && y > 0 {
                let serial = gridcore::sheet::parts_to_serial(y, m, d, 0, date1904);
                return format!("{serial}");
            }
        }
    }
    t.to_string()
}

/// The Custom AutoFilter dialog's criteria. On a date column (`date`) a
/// typed `YYYY-MM-DD` compares as that date; a wildcard pattern (begins
/// with, contains, …) and any other column keep the text as typed.
fn custom_criteria(d: &Dialog, date1904: bool, date: bool) -> Result<ColumnFilter, String> {
    let mut conds = Vec::new();
    for (op, val) in [("op1", "val1"), ("op2", "val2")] {
        let Some(label) = choice_of(d, op) else {
            continue;
        };
        let Some((_, stored, pat)) = OPERATORS.iter().find(|o| o.0 == label) else {
            continue;
        };
        let typed = text_of(d, val);
        let v = if date && *pat == "{}" {
            operand(&typed, date1904)
        } else {
            typed
        };
        conds.push((stored.to_string(), pat.replace("{}", &v)));
    }
    if conds.is_empty() {
        return Err("Choose a condition".into());
    }
    Ok(ColumnFilter::Custom {
        and: choice_of(d, "join").as_deref() != Some("Or"),
        conds,
    })
}

fn presses(d: &Dialog, button: &str, label: &str) -> bool {
    button.replace('&', "").trim().eq_ignore_ascii_case(label)
        && d.buttons.iter().any(|b| b.label == label && b.enabled)
}

/// A press the filter dialogs handle themselves. `None` for any other press
/// or dialog (Cancel closes through the stack).
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    match top.owner {
        DialogOwner::FilterMenu { sheet, col } => menu_click(tab, sheet, col, button),
        DialogOwner::CustomFilter { col, date, .. } => {
            if !presses(top, button, "OK") {
                return None;
            }
            let date1904 = match &tab.surface {
                Surface::Sheet(v) => v.pkg.workbook.date1904,
                _ => false,
            };
            let f = match custom_criteria(top, date1904, date) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            Some(apply_and_close(tab, move |wb, s, today| {
                gridcore::filter::set_criterion(wb, s, col, Some(f), today)
            }))
        }
        DialogOwner::Top10Filter { col, .. } => {
            if !presses(top, button, "OK") {
                return None;
            }
            let val = match text_of(top, "n").parse::<f64>() {
                Ok(n) if n > 0.0 => n,
                _ => return Some(Err("Enter a number greater than 0".into())),
            };
            let f = ColumnFilter::Top10 {
                top: choice_of(top, "which").as_deref() != Some("Bottom"),
                percent: choice_of(top, "unit").as_deref() == Some("Percent"),
                val,
                filter_val: None,
            };
            Some(apply_and_close(tab, move |wb, s, today| {
                gridcore::filter::set_criterion(wb, s, col, Some(f), today)
            }))
        }
        DialogOwner::AdvancedFilter { sheet } => {
            if !presses(top, button, "OK") {
                return None;
            }
            let Surface::Sheet(v) = &tab.surface else {
                return Some(Err("Advanced Filter needs a spreadsheet".into()));
            };
            let wb = &v.pkg.workbook;
            let built = (|| -> Result<AdvancedFilter, String> {
                let (ls, list) = typed_range(wb, &text_of(top, "list"), sheet)?;
                if ls != sheet {
                    return Err("The list range must be on this sheet".into());
                }
                let criteria = match text_of(top, "criteria").as_str() {
                    "" => None,
                    t => Some(typed_range(wb, t, sheet)?),
                };
                let copy = choice_of(top, "action").as_deref() == Some("Copy to another location");
                let copy_to = match (copy, text_of(top, "copy").as_str()) {
                    (false, _) => None,
                    (true, "") => return Err("Enter the range to copy to".into()),
                    (true, t) => Some(typed_range(wb, t, sheet)?),
                };
                Ok(AdvancedFilter {
                    list,
                    criteria,
                    copy_to,
                    unique: checked(top, "unique"),
                })
            })();
            let a = match built {
                Ok(a) => a,
                Err(e) => return Some(Err(e)),
            };
            Some(apply_and_close(tab, move |wb, s, _| {
                gridcore::filter::advanced(wb, s, &a)
            }))
        }
        _ => None,
    }
}

/// Run `op` and, when it worked, close the top dialog; a refusal keeps it
/// open and says why.
fn apply_and_close(
    tab: &mut DocTab,
    op: impl FnOnce(&mut Workbook, usize, f64) -> Result<FilterOutcome, FilterError>,
) -> Result<(), String> {
    run(tab, op)?;
    tab.dialogs.pop();
    Ok(())
}

fn menu_click(
    tab: &mut DocTab,
    sheet: usize,
    col: u32,
    button: &str,
) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    let label = [
        "Sort A to Z",
        "Sort Z to A",
        "Sort by Color",
        "Filter by Color",
        "Clear Filter",
        "Apply Filter",
        "Search",
        "OK",
    ]
    .into_iter()
    .find(|l| presses(top, button, l))?;
    let Surface::Sheet(v) = &tab.surface else {
        return Some(Err("Filter needs a spreadsheet".into()));
    };
    // Search builds the list for the box's text; everything else reads the
    // list the checklist shows, built for the last Search.
    let search = if label == "Search" {
        text_of(top, "search")
    } else {
        text_of(top, "searched")
    };
    let m = match gridcore::filter::menu(
        &v.pkg.workbook,
        sheet,
        col,
        (!search.is_empty()).then_some(search.as_str()),
    ) {
        Ok(m) => m,
        Err(e) => return Some(Err(e.to_string())),
    };
    let header = m.header.clone();
    Some(match label {
        "Sort A to Z" | "Sort Z to A" => {
            let asc = label == "Sort A to Z";
            // The list as the filter takes it: rows typed below included.
            let range = gridcore::filter::filter_range(&v.pkg.workbook, sheet);
            tab.dialogs.pop();
            match range {
                Some(r) => crate::sheet_sort::sort_now(tab, r, true, col, asc),
                None => Err(FilterError::NoFilter.to_string()),
            }
        }
        "Clear Filter" => apply_and_close(tab, move |wb, s, today| {
            gridcore::filter::clear(wb, s, Some(col), today)
        }),
        "Sort by Color" | "Filter by Color" => {
            let Some(choice) = choice_of(top, "color")
                .and_then(|l| m.colors.iter().find(|c| c.label() == l).cloned())
            else {
                return Some(Err("Choose a colour".into()));
            };
            if label == "Filter by Color" {
                // One colour per column: it replaces the column's criteria.
                return Some(apply_and_close(tab, move |wb, s, today| {
                    gridcore::filter::set_criterion(wb, s, col, Some(choice.criterion()), today)
                }));
            }
            let range = gridcore::filter::filter_range(&v.pkg.workbook, sheet);
            tab.dialogs.pop();
            match range {
                Some(r) => {
                    let level = [gridcore::edit::SortLevel {
                        key: col,
                        on: choice.sort_on(),
                    }];
                    let opts = gridcore::edit::SortOptions {
                        header: true,
                        ..Default::default()
                    };
                    crate::sheet_sort::run(tab, r, &level, &opts)
                }
                None => Err(FilterError::NoFilter.to_string()),
            }
        }
        "Search" => {
            // The list narrows to what the search matches, all checked.
            let searching = !search.is_empty();
            if let Ok(d) = tab.dialogs.top_dialog_mut() {
                if let Some(c) = control_mut(d, "values") {
                    *c = values_control(&m, searching);
                }
                if let Some(c) = control_mut(d, "searched") {
                    c.value = Value::Text(search);
                }
            }
            Ok(())
        }
        "Apply Filter" => {
            let Some(entry) = choice_of(top, "typed") else {
                return Some(Err("Choose a filter".into()));
            };
            if let Some(kind) = dynamic_of(&entry) {
                return Some(apply_and_close(tab, move |wb, s, today| {
                    let f = ColumnFilter::Dynamic {
                        kind,
                        val: None,
                        max_val: None,
                    };
                    gridcore::filter::set_criterion(wb, s, col, Some(f), today)
                }));
            }
            let child =
                if entry == "Top 10..." {
                    top10_dialog(sheet, col)
                } else {
                    let op =
                        match entry.as_str() {
                            "Does Not Equal..." => 1,
                            "Greater Than..." | "After..." | "Between..." => {
                                if entry == "Between..." { 3 } else { 2 }
                            }
                            "Greater Than Or Equal To..." => 3,
                            "Less Than..." | "Before..." => 4,
                            "Less Than Or Equal To..." => 5,
                            "Begins With..." => 6,
                            "Ends With..." => 8,
                            "Contains..." => 10,
                            "Does Not Contain..." => 11,
                            _ => 0,
                        };
                    let date = m.submenu == Submenu::Date;
                    let mut d = custom_dialog(sheet, col, &header, op, date);
                    if entry == "Between..." {
                        if let Some(c) = control_mut(&mut d, "op2") {
                            c.value = Value::Choice(Some(5));
                        }
                    }
                    d
                };
            tab.dialogs.pop();
            tab.dialogs.push(child);
            Ok(())
        }
        _ => {
            // OK: the checklist as shown makes the criteria, a search's
            // results with their checks (added to the column's checklist
            // with "Add current selection"); an untouched list leaves the
            // column's criterion as it is (a Top 10 or a colour filter shows
            // there as nothing checked).
            let checks = match control(top, "values").map(|c| &c.value) {
                Some(Value::Checks(c)) => c.clone(),
                _ => Vec::new(),
            };
            let add = checked(top, "add");
            if search.is_empty() && !top.changed("values") {
                tab.dialogs.pop();
                Ok(())
            } else {
                let f = gridcore::filter::checklist_criteria(
                    &v.pkg.workbook,
                    sheet,
                    col,
                    (!search.is_empty()).then_some(search.as_str()),
                    &m.items,
                    checks.get(1..).unwrap_or_default(),
                    m.truncated,
                    add,
                );
                match f {
                    Ok(f) => apply_and_close(tab, move |wb, s, today| {
                        gridcore::filter::set_criterion(wb, s, col, f, today)
                    }),
                    Err(e) => Err(e.to_string()),
                }
            }
        }
    })
}

#[cfg(test)]
pub(crate) mod tests;
