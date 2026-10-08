//! Data › Sort & Filter's sorts (#691): Sort A to Z / Z to A, Excel's Sort
//! dialog (levels on values, a custom list, cell colour, font colour or a
//! conditional-formatting icon; My data has headers; Case sensitive; Sort
//! left to right), the Sort Warning for a selection inside a wider list,
//! and the cell menu's Put Selected … On Top, over
//! [`gridcore::edit::sort_range`].
//!
//! The dialogs sit on the tab's [`crate::dialog::DialogStack`], so the
//! harness drives them; each sort is one undo step.

use crate::dialog::DialogId;
use crate::dialog::catalog;
use crate::dialog::{
    Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Reaction, Value,
};
use crate::{DocTab, Surface};
use gridcore::edit::{
    SORT_WARNING, SortLevel, SortOn, SortOptions, sort_range, sort_region, sort_warning,
};
use gridcore::sheet::{Workbook, col_name};

type Area = (u32, u32, u32, u32);

/// What a sort waiting on the Sort Warning does once it is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortThen {
    /// Sort A to Z / Z to A by column `col`.
    Quick { col: u32, asc: bool },
    /// Open the Sort dialog on the range chosen.
    Dialog,
    /// Put the selected cell's fill / font colour / icon on top.
    OnTop { by: OnTop, at: (u32, u32) },
}

/// The cell menu's Put Selected … On Top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnTop {
    CellColor,
    FontColor,
    Icon,
}

const PROTECTED: &str = "The sheet is protected: unprotect it (Review › Protect Sheet) to sort it.";

/// Sort `area` of the view's sheet by `levels` as one undo step. [`run`]
/// commits an open cell editor first; here the commit is a guard for a
/// direct caller, and an editor that can't commit refuses the sort. A
/// refusal (that editor, a cut spill, merged cells of different sizes)
/// changes nothing, and its reason is the error and the view's
/// `entry_error`. `Ok` gives the rows (or columns) sorted and whether the
/// sort changed anything.
pub(crate) fn sort_view(
    v: &mut crate::SheetView,
    area: Area,
    levels: &[SortLevel],
    opts: &SortOptions,
) -> Result<(usize, bool), String> {
    let s = v.active;
    if v.pkg.workbook.sheets[s].is_protected() {
        return Err(PROTECTED.into());
    }
    v.commit_edit();
    if v.editing.is_some() {
        // Sorting would move the cell being typed in.
        return Err(v
            .entry_error
            .clone()
            .unwrap_or_else(|| "Finish the cell you are typing in first".into()));
    }
    let snap = v.snapshot();
    match sort_range(&mut v.pkg.workbook, s, area, levels, opts) {
        Ok(n) => {
            let changed = snap.workbook().sheets[s].cells != v.pkg.workbook.sheets[s].cells;
            if changed {
                v.push_undo_snapshot(snap);
                // Validation circles name cells, and the rows just moved.
                v.circles.clear();
                // The rebuilt engine keeps the clock the old one had.
                let clock = v.engine.clock;
                v.engine = crate::sheet_engine(&v.pkg.workbook);
                v.engine.clock = clock;
                v.engine.recalc_all(&mut v.pkg.workbook);
            }
            Ok((n, changed))
        }
        Err(e) => {
            v.restore(snap);
            v.entry_error = Some(e.message().into());
            Err(e.message().into())
        }
    }
}

/// [`sort_view`] on the tab's sheet: marks the tab dirty when something
/// changed and says what it did (or why not) in the status line.
pub(crate) fn run(
    tab: &mut DocTab,
    area: Area,
    levels: &[SortLevel],
    opts: &SortOptions,
) -> Result<(), String> {
    // A typed value the sort commits is an edit even if the sort refuses.
    commit_first(tab)?;
    let Surface::Sheet(v) = &mut tab.surface else {
        return Err("Sort needs a spreadsheet".into());
    };
    match sort_view(v, area, levels, opts) {
        Ok((n, changed)) => {
            if changed {
                tab.set_dirty();
            }
            tab.status = format!(
                "Sorted {n} {}",
                if opts.left_to_right {
                    "columns"
                } else {
                    "rows"
                }
            )
            .into();
            Ok(())
        }
        Err(e) => {
            tab.status = e.clone().into();
            Err(e)
        }
    }
}

/// Commit an open cell editor before a sort or a filter command works out
/// its range (the row may move or hide), marking the tab dirty when it did:
/// the typed value is an edit whatever the command then does. Refused, with
/// the entry's own reason (`entry_error`), while the entry can't commit.
/// Whether it committed.
pub(crate) fn commit_first(tab: &mut DocTab) -> Result<bool, String> {
    let Surface::Sheet(v) = &mut tab.surface else {
        return Err("Sort needs a spreadsheet".into());
    };
    let committed = v.commit_edit();
    if committed {
        tab.set_dirty();
    }
    let Surface::Sheet(v) = &tab.surface else {
        return Ok(committed);
    };
    match &v.editing {
        Some(_) => Err(v
            .entry_error
            .clone()
            .unwrap_or_else(|| "Finish the cell you are typing in first".into())),
        None => Ok(committed),
    }
}

/// Sort `area` (its first row a header when `header`) by column `col`.
pub(crate) fn sort_now(
    tab: &mut DocTab,
    area: Area,
    header: bool,
    col: u32,
    asc: bool,
) -> Result<(), String> {
    let levels = [SortLevel {
        key: col,
        on: SortOn::Value { asc, list: None },
    }];
    let opts = SortOptions {
        header,
        ..SortOptions::default()
    };
    run(tab, area, &levels, &opts)
}

/// Where a sort from the cursor acts: a selection inside a wider list asks
/// first (`Err` carries the warning's dialog), else the selection, or the
/// list around the cursor, with Excel's header guess.
fn target(tab: &DocTab, then: SortThen) -> Result<Result<(Area, bool), Dialog>, String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("Sort needs a spreadsheet".into());
    };
    let wb = &v.pkg.workbook;
    let s = v.active;
    let sel = crate::sel_range(v.sel, v.anchor);
    if (sel.0, sel.1) != (sel.2, sel.3) {
        if let Some((region, header)) = sort_warning(wb, s, sel) {
            return Ok(Err(warning_dialog(s, sel, region, header, then)));
        }
        return Ok(Ok((sel, gridcore::edit::guess_header(wb, s, sel))));
    }
    let found = sort_region(wb, s, v.sel).ok_or("Select a cell in the data to sort")?;
    Ok(Ok(found))
}

/// Sort A to Z / Z to A by the cursor's column.
pub(crate) fn quick(tab: &mut DocTab, asc: bool) -> Result<(), String> {
    commit_first(tab)?;
    let col = match &tab.surface {
        Surface::Sheet(v) => v.sel.1,
        _ => return Err("Sort needs a spreadsheet".into()),
    };
    match target(tab, SortThen::Quick { col, asc })? {
        Ok((area, header)) => sort_now(tab, area, header, col, asc),
        Err(d) => {
            tab.dialogs.push(d);
            Ok(())
        }
    }
}

/// Data › Sort: the Sort dialog on the selection or the list (after the
/// Sort Warning, for a selection inside a wider list).
pub(crate) fn open_dialog(tab: &mut DocTab) -> Result<(), String> {
    commit_first(tab)?;
    let d = match target(tab, SortThen::Dialog)? {
        Ok((area, header)) => sort_dialog(tab, area, header)?,
        Err(d) => d,
    };
    tab.dialogs.push(d);
    Ok(())
}

/// Put Selected Cell Color / Font Color / Icon On Top, by the cursor's
/// column.
pub(crate) fn on_top(tab: &mut DocTab, by: OnTop) -> Result<(), String> {
    commit_first(tab)?;
    let at = match &tab.surface {
        Surface::Sheet(v) => v.sel,
        _ => return Err("Sort needs a spreadsheet".into()),
    };
    match target(tab, SortThen::OnTop { by, at })? {
        Ok((area, header)) => on_top_in(tab, area, header, by, at),
        Err(d) => {
            tab.dialogs.push(d);
            Ok(())
        }
    }
}

fn on_top_in(
    tab: &mut DocTab,
    area: Area,
    header: bool,
    by: OnTop,
    at: (u32, u32),
) -> Result<(), String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("Sort needs a spreadsheet".into());
    };
    let (wb, s) = (&v.pkg.workbook, v.active);
    let unknown = || "The cell's colour can't be read".to_string();
    let on = match by {
        OnTop::CellColor => SortOn::CellColor {
            rgb: gridcore::cf::cell_fill(wb, s, at.0, at.1)
                .rgb()
                .ok_or_else(unknown)?,
            top: true,
        },
        OnTop::FontColor => SortOn::FontColor {
            rgb: gridcore::cf::cell_font_color(wb, s, at.0, at.1)
                .rgb()
                .ok_or_else(unknown)?,
            top: true,
        },
        OnTop::Icon => {
            let (set, id) =
                gridcore::cf::cell_icon(wb, s, at.0, at.1).ok_or("The cell shows no icon")?;
            SortOn::Icon { set, id, top: true }
        }
    };
    let opts = SortOptions {
        header,
        ..SortOptions::default()
    };
    run(tab, area, &[SortLevel { key: at.1, on }], &opts)
}

fn dialog(id: DialogId, title: &str, buttons: &[(&str, ButtonRole)], owner: DialogOwner) -> Dialog {
    let mut d = Dialog::message(id, title, String::new(), buttons, owner);
    d.text = None;
    d.buttons = d
        .buttons
        .into_iter()
        .map(|b| Button {
            default: matches!(b.label.as_str(), "OK" | "Sort"),
            ..b
        })
        .collect();
    d
}

/// Excel's Sort Warning.
fn warning_dialog(sheet: usize, sel: Area, region: Area, header: bool, then: SortThen) -> Dialog {
    let mut d = dialog(
        catalog::SORT_WARNING,
        "Sort Warning",
        &[("Sort", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::SortWarning {
            sheet,
            sel,
            region,
            header,
            then,
        },
    );
    d.text = Some(SORT_WARNING.into());
    let mut what = Control::new(
        "what",
        "What do you want to do?",
        ControlKind::Radio,
        Value::Choice(Some(0)),
    );
    what.items = vec![
        "Expand the selection".into(),
        "Continue with the current selection".into(),
    ];
    d.controls = vec![what];
    d.mark_opened();
    d
}

const SORT_ON: [&str; 4] = [
    "Cell Values",
    "Cell Color",
    "Font Color",
    "Conditional Formatting Icon",
];
const ORDER: [&str; 5] = ["A to Z", "Z to A", "Custom List...", "On Top", "On Bottom"];
const NONE: &str = "(none)";

/// The labels of `area`'s columns (header text, or `Column B`) and rows.
fn key_labels(
    wb: &Workbook,
    s: usize,
    (r1, c1, r2, c2): Area,
    header: bool,
) -> (Vec<String>, Vec<String>) {
    let sh = &wb.sheets[s];
    let cols = (c1..=c2)
        .map(|c| {
            let t = sh
                .cell(r1, c)
                .map(|x| {
                    gridcore::sheet::format_with(&wb.styles.xf(x.style), &x.value, wb.date1904)
                })
                .unwrap_or_default();
            if header && !t.trim().is_empty() {
                t
            } else {
                format!("Column {}", col_name(c))
            }
        })
        .collect();
    let rows = (r1..=r2).map(|r| format!("Row {}", r + 1)).collect();
    (cols, rows)
}

/// Excel's Sort dialog over `area`: three levels (Sort by, Then by, Then
/// by), each a key, what to sort on, the order, and a list / colour / icon;
/// My data has headers; Case sensitive; the orientation.
pub(crate) fn sort_dialog(tab: &DocTab, area: Area, header: bool) -> Result<Dialog, String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("Sort needs a spreadsheet".into());
    };
    let (cols, rows) = key_labels(&v.pkg.workbook, v.active, area, header);
    let mut d = dialog(
        catalog::SHEET_SORT,
        "Sort",
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::SortLevels {
            sheet: v.active,
            area,
        },
    );
    let mut controls = Vec::new();
    for (l, (by, on, order, with)) in [
        ("by1", "on1", "order1", "with1"),
        ("by2", "on2", "order2", "with2"),
        ("by3", "on3", "order3", "with3"),
    ]
    .into_iter()
    .enumerate()
    {
        let first = l == 0;
        let mut key = Control::new(
            by,
            if first { "Sort by" } else { "Then by" },
            ControlKind::Dropdown,
            Value::Choice(Some(0)),
        );
        key.items = if first {
            cols.clone()
        } else {
            std::iter::once(NONE.to_string())
                .chain(cols.clone())
                .collect()
        };
        let mut sort_on =
            Control::new(on, "Sort On", ControlKind::Dropdown, Value::Choice(Some(0)));
        sort_on.items = SORT_ON.iter().map(|s| s.to_string()).collect();
        let mut ord = Control::new(
            order,
            "Order",
            ControlKind::Dropdown,
            Value::Choice(Some(0)),
        );
        ord.items = ORDER.iter().map(|s| s.to_string()).collect();
        controls.extend([
            key,
            sort_on,
            ord,
            Control::new(
                with,
                "List, colour or icon",
                ControlKind::Text,
                Value::Text(String::new()),
            ),
        ]);
    }
    let mut orientation = Control::new(
        "orientation",
        "Orientation",
        ControlKind::Radio,
        Value::Choice(Some(0)),
    );
    orientation.items = vec!["Sort top to bottom".into(), "Sort left to right".into()];
    // The key labels for each orientation, which the reaction swaps in.
    let mut col_keys = Control::new("col-keys", "", ControlKind::List, Value::Choice(None));
    col_keys.items = cols;
    col_keys.visible = false;
    let mut row_keys = Control::new("row-keys", "", ControlKind::List, Value::Choice(None));
    row_keys.items = rows;
    row_keys.visible = false;
    controls.extend([
        Control::new(
            "headers",
            "My data has headers",
            ControlKind::Checkbox,
            Value::Bool(header),
        ),
        Control::new(
            "case",
            "Case sensitive",
            ControlKind::Checkbox,
            Value::Bool(false),
        ),
        orientation,
        col_keys,
        row_keys,
    ]);
    d.controls = controls;
    d.react = Some(Reaction(swap_keys));
    d.mark_opened();
    Ok(d)
}

/// Changing the orientation lists rows (left to right) or columns as keys.
fn swap_keys(d: &mut Dialog, i: usize, before: &Value) {
    if d.controls[i].name != "orientation" || d.controls[i].value == *before {
        return;
    }
    let ltr = d.controls[i].value == Value::Choice(Some(1));
    let src = if ltr { "row-keys" } else { "col-keys" };
    let Some(keys) = d
        .controls
        .iter()
        .find(|c| c.name == src)
        .map(|c| c.items.clone())
    else {
        return;
    };
    for c in d.controls.iter_mut() {
        match c.name {
            "by1" => {
                c.items = keys.clone();
                c.value = Value::Choice(Some(0));
            }
            "by2" | "by3" => {
                c.items = std::iter::once(NONE.to_string())
                    .chain(keys.clone())
                    .collect();
                c.value = Value::Choice(Some(0));
            }
            "headers" if ltr => c.value = Value::Bool(false),
            _ => {}
        }
    }
}

fn control<'a>(d: &'a Dialog, name: &str) -> Option<&'a Control> {
    d.controls.iter().find(|c| c.name == name)
}

fn choice(d: &Dialog, name: &str) -> Option<usize> {
    match control(d, name)?.value {
        Value::Choice(i) => i,
        _ => None,
    }
}

fn text(d: &Dialog, name: &str) -> String {
    match control(d, name).map(|c| &c.value) {
        Some(Value::Text(t)) => t.trim().to_string(),
        _ => String::new(),
    }
}

/// `FF00B050`, `00B050`, `none` / `No Fill` / `automatic` (no colour).
fn colour(t: &str) -> Result<Option<(u8, u8, u8)>, String> {
    let l = t.trim().to_lowercase();
    if matches!(l.as_str(), "none" | "no fill" | "automatic" | "auto") {
        return Ok(None);
    }
    gridcore::format::hex_rgb(t)
        .map(Some)
        .ok_or_else(|| format!("\"{t}\" is not a colour (FF00B050, or No Fill)"))
}

/// The Sort dialog's levels and options.
fn levels_of(d: &Dialog, area: Area) -> Result<(Vec<SortLevel>, SortOptions), String> {
    let ltr = choice(d, "orientation") == Some(1);
    let first_key = if ltr { area.0 } else { area.1 };
    let mut levels = Vec::new();
    for l in 1..=3 {
        let (by, on, order, with) = (
            format!("by{l}"),
            format!("on{l}"),
            format!("order{l}"),
            format!("with{l}"),
        );
        let Some(k) = choice(d, &by) else { continue };
        // Then by's first item is (none).
        let k = if l == 1 {
            k
        } else if k == 0 {
            continue;
        } else {
            k - 1
        };
        let key = first_key + k as u32;
        let ord = choice(d, &order).unwrap_or(0);
        let top = ord != 4;
        let w = text(d, &with);
        let on = match choice(d, &on).unwrap_or(0) {
            0 => match ord {
                2 => {
                    if w.is_empty() {
                        return Err("Enter the custom list, e.g. Jan, Feb, Mar".into());
                    }
                    let list = gridcore::edit::builtin_sort_list(&w)
                        .unwrap_or_else(|| w.split(',').map(|s| s.trim().to_string()).collect());
                    SortOn::Value {
                        asc: true,
                        list: Some(list),
                    }
                }
                1 => SortOn::Value {
                    asc: false,
                    list: None,
                },
                _ => SortOn::Value {
                    asc: true,
                    list: None,
                },
            },
            1 => SortOn::CellColor {
                rgb: colour(&w)?,
                top,
            },
            2 => SortOn::FontColor {
                rgb: colour(&w)?,
                top,
            },
            _ => {
                let (set, id) = w
                    .split_once('/')
                    .ok_or("Enter the icon as set/index, e.g. 3Arrows/2")?;
                SortOn::Icon {
                    set: set.trim().to_string(),
                    id: id
                        .trim()
                        .parse()
                        .map_err(|_| "The icon index is a number")?,
                    top,
                }
            }
        };
        levels.push(SortLevel { key, on });
    }
    let opts = SortOptions {
        case_sensitive: control(d, "case").is_some_and(|c| c.value == Value::Bool(true)),
        left_to_right: ltr,
        header: control(d, "headers").is_some_and(|c| c.value == Value::Bool(true)),
    };
    Ok((levels, opts))
}

fn presses(d: &Dialog, button: &str, label: &str) -> bool {
    button.replace('&', "").trim().eq_ignore_ascii_case(label)
        && d.buttons.iter().any(|b| b.label == label && b.enabled)
}

/// A press the sort dialogs handle: the Sort dialog's OK, the Sort
/// Warning's Sort. `None` for any other press or dialog.
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    match top.owner {
        DialogOwner::SortLevels { area, .. } => {
            if !presses(top, button, "OK") {
                return None;
            }
            let (levels, opts) = match levels_of(top, area) {
                Ok(x) => x,
                Err(e) => return Some(Err(e)),
            };
            Some(run(tab, area, &levels, &opts).map(|()| {
                tab.dialogs.pop();
            }))
        }
        DialogOwner::SortWarning {
            sel,
            region,
            header,
            then,
            ..
        } => {
            if !presses(top, button, "Sort") {
                return None;
            }
            let expand = choice(top, "what") != Some(1);
            let (area, header) = if expand {
                (region, header)
            } else {
                (sel, false)
            };
            tab.dialogs.pop();
            Some(match then {
                SortThen::Quick { col, asc } => sort_now(tab, area, header, col, asc),
                SortThen::OnTop { by, at } => on_top_in(tab, area, header, by, at),
                SortThen::Dialog => sort_dialog(tab, area, header).map(|d| tab.dialogs.push(d)),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
