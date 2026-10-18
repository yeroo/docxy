//! The entry table's AutoFilter heading menu (#1533): a heading's arrow opens
//! a drop-down with Sort, `Clear Filter from <Field>`, and the column's value
//! checklist (a Year > Month > Day tree on a date column) over
//! `Editor::column_values`. Like the sheet's drop-down it sits on the tab's
//! dialog stack, so the harness's `dialog-*` verbs drive it.
//!
//! Group by and Filters are drawn disabled, so the menu keeps Project's shape;
//! both are follow-ups. Sort applies at once, like a menu item; OK applies the
//! checklist; Cancel discards it. Checked boxes are two-state (the dialog
//! check list's rule: a parent is checked when all its children are).
use super::*;
use crate::dialog::catalog;
use crate::dialog::{ButtonRole, Control, ControlKind, Dialog, DialogOwner, Popup, Value};
use crate::sheet_filter::{control, dialog};
use ctlcore::json::Json;
use projcore::editor::{Field, Sort, ValueItem};
use std::collections::BTreeSet;

/// The status bar's word while any column is filtered.
pub(crate) const AUTO_FILTER_STATUS: &str = "AutoFilter Applied";

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// How a column sorts, which words its Sort items use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColumnKind {
    Text,
    Number,
    Date,
}

pub(crate) fn column_kind(field: Field) -> ColumnKind {
    match field {
        Field::Id | Field::Duration => ColumnKind::Number,
        Field::Start | Field::Finish => ColumnKind::Date,
        _ => ColumnKind::Text,
    }
}

/// The two Sort items' labels, ascending first.
pub(crate) fn sort_labels(kind: ColumnKind) -> [&'static str; 2] {
    match kind {
        ColumnKind::Text => ["Sort A to Z", "Sort Z to A"],
        ColumnKind::Number => ["Sort Smallest to Largest", "Sort Largest to Smallest"],
        ColumnKind::Date => ["Sort Earliest to Latest", "Sort Latest to Earliest"],
    }
}

/// The checklist: `(Select All)`, then the values (a date column's as a
/// Year > Month > Day tree). `texts[k]` is the display text item `k` stands
/// for, `None` for a heading (the all item, a year, a month).
#[derive(Debug, PartialEq)]
pub(crate) struct Tree {
    pub labels: Vec<String>,
    pub depths: Vec<u8>,
    pub texts: Vec<Option<String>>,
}

impl Tree {
    fn push(&mut self, label: impl Into<String>, depth: u8, text: Option<String>) {
        self.labels.push(label.into());
        self.depths.push(depth);
        self.texts.push(text);
    }

    /// The value texts the checked leaves stand for.
    pub fn checked_texts(&self, checks: &[bool]) -> BTreeSet<String> {
        self.texts
            .iter()
            .zip(checks)
            .filter_map(|(t, &on)| t.clone().filter(|_| on))
            .collect()
    }

    pub fn leaf_count(&self) -> usize {
        self.texts.iter().flatten().count()
    }
}

/// The checklist for `values` (ascending, a blank last, as
/// `Editor::column_values` gives them).
pub(crate) fn build_tree(values: &[ValueItem], date: bool) -> Tree {
    let mut t = Tree {
        labels: Vec::new(),
        depths: Vec::new(),
        texts: Vec::new(),
    };
    t.push("(Select All)", 0, None);
    let (mut year, mut month) = (None, None);
    for v in values {
        match v.date.filter(|_| date) {
            Some((y, m, d)) => {
                if year != Some(y) {
                    year = Some(y);
                    month = None;
                    t.push(y.to_string(), 1, None);
                }
                if month != Some(m) {
                    month = Some(m);
                    t.push(MONTHS[(m as usize - 1).min(11)], 2, None);
                }
                t.push(d.to_string(), 3, Some(v.text.clone()));
            }
            None if v.text.is_empty() => t.push("(Blanks)", 1, Some(String::new())),
            None => t.push(v.text.clone(), 1, Some(v.text.clone())),
        }
    }
    t
}

/// The boxes for `tree`: a leaf is on when `allowed` holds its text (all of
/// them when the column is not filtered), a heading when everything under it
/// is.
pub(crate) fn tree_checks(tree: &Tree, allowed: Option<&BTreeSet<String>>) -> Vec<bool> {
    let mut checks: Vec<bool> = tree
        .texts
        .iter()
        .map(|t| {
            t.as_ref()
                .is_some_and(|t| allowed.is_none_or(|a| a.contains(t)))
        })
        .collect();
    let n = checks.len();
    for i in (0..n).rev() {
        if tree.texts[i].is_some() {
            continue;
        }
        let mut j = i + 1;
        let mut all = true;
        let mut any = false;
        while j < n && tree.depths[j] > tree.depths[i] {
            any = true;
            all &= checks[j];
            j += 1;
        }
        checks[i] = any && all;
    }
    checks
}

/// Where a heading's arrow is on screen: the heading's left edge and its
/// top and bottom, in window pixels (#1607).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Anchor {
    pub left: f32,
    pub top: f32,
    pub bottom: f32,
}

/// The popup's width, the height of a menu row and of a rule, and the height
/// of everything but the value list (menu rows, rules, the OK / Cancel row
/// and padding).
pub(crate) const POPUP_W: f32 = 250.;
pub(crate) const ROW_H: f32 = 26.;
pub(crate) const RULE_H: f32 = 7.;
pub(crate) const FIXED_H: f32 = 6. * ROW_H + 3. * RULE_H + 40. + 8.;
const MIN_LIST_H: f32 = 80.;
const MAX_LIST_H: f32 = 220.;
const MARGIN: f32 = 4.;

/// Where the popup goes for `anchor` in a window of `window` pixels: below the
/// heading, else above when the room below cannot hold the menu and a
/// usable value list and there is more room above; clamped into the window
/// (the end of the menu stays on screen even where the room is short), the
/// value list as tall as the room allows.
pub(crate) fn place(anchor: Anchor, window: (f32, f32)) -> Popup {
    let below = window.1 - anchor.bottom - MARGIN;
    let above_room = anchor.top - MARGIN;
    let above = below < FIXED_H + MIN_LIST_H && above_room > below;
    let room = if above { above_room } else { below };
    let list_h = (room - FIXED_H).clamp(MIN_LIST_H, MAX_LIST_H);
    let x = anchor.left.min(window.0 - POPUP_W - MARGIN).max(MARGIN);
    let wanted = if above {
        anchor.top - FIXED_H - list_h
    } else {
        anchor.bottom
    };
    // Whatever the room, OK / Cancel stay inside the window.
    let y = wanted.min(window.1 - FIXED_H - list_h - MARGIN).max(MARGIN);
    Popup {
        x,
        y,
        width: POPUP_W,
        list_h,
    }
}

/// The drop-down of entry column `col`, drawn as Project's popup at `popup`.
pub(crate) fn menu_dialog(v: &ProjectView, col: usize, popup: Popup) -> Dialog {
    let field = ENTRY_FIELDS[col];
    let name = COLUMNS[col];
    let kind = column_kind(field);
    let [asc, desc] = sort_labels(kind);
    let clear = format!("Clear Filter from {name}");
    let mut d = dialog(
        catalog::PROJECT_FILTER_MENU,
        String::new(),
        &[
            (asc, ButtonRole::Apply),
            (desc, ButtonRole::Apply),
            ("Group by", ButtonRole::Apply),
            ("No Group", ButtonRole::Apply),
            (clear.as_str(), ButtonRole::Apply),
            ("Filters", ButtonRole::Apply),
            ("OK", ButtonRole::Accept),
            ("Cancel", ButtonRole::Cancel),
        ],
        DialogOwner::ProjectFilter { col },
    );
    d.popup = Some(popup);
    for b in &mut d.buttons {
        (b.icon, b.submenu, b.rule) = match b.label.as_str() {
            l if l == asc => ("sort-az", false, false),
            l if l == desc => ("sort-za", false, false),
            "Group by" => ("", true, true),
            "No Group" => ("", false, false),
            l if l == clear => ("", false, true),
            "Filters" => ("", true, false),
            _ => ("", false, false),
        };
    }
    for b in &mut d.buttons {
        b.enabled = match b.label.as_str() {
            "Group by" | "No Group" | "Filters" => false,
            l if l == clear => v.ed.filter(field).is_some(),
            _ => true,
        };
    }
    let values = v.ed.column_values(field);
    let tree = build_tree(&values, kind == ColumnKind::Date);
    let checks = tree_checks(&tree, v.ed.filter(field));
    let mut list = Control::new(
        "values",
        "Values",
        ControlKind::CheckList,
        Value::Checks(checks),
    );
    list.items = tree.labels;
    list.depths = tree.depths;
    d.controls = vec![list];
    d.mark_opened();
    d
}

fn sort_words(sort: Sort) -> String {
    let [asc, desc] = sort_labels(column_kind(sort.field));
    let label = if sort.descending { desc } else { asc };
    format!(
        "{} by {}",
        label.trim_start_matches("Sort "),
        sort.field.name()
    )
}

/// `menu-read` / `menu-open` of the popup (#1607), shaped like every menu's
/// reply: `items` are the menu rows in the order drawn, a `{"separator":true}`
/// entry of its own where a rule is drawn, `submenu` `[]` on a row that opens
/// one (none is built) and null otherwise; `controls` is the value list and
/// `buttons` OK / Cancel.
pub(crate) fn menu_json(d: &Dialog) -> Json {
    let all = d.to_json(0);
    let field = |k: &str| all.get(k).cloned().unwrap_or(Json::Null);
    let column = match d.owner {
        DialogOwner::ProjectFilter { col } => COLUMNS[col],
        _ => "",
    };
    let mut items = Vec::new();
    let mut foot = Vec::new();
    for b in &d.buttons {
        if b.role != ButtonRole::Apply {
            foot.push(Json::obj(vec![
                ("id", Json::Str(b.id.clone())),
                ("label", Json::Str(b.label.clone())),
                ("enabled", Json::Bool(b.enabled)),
                ("default", Json::Bool(b.default)),
            ]));
            continue;
        }
        if b.rule {
            items.push(Json::obj(vec![("separator", Json::Bool(true))]));
        }
        let mut f = vec![
            ("id", Json::Str(b.id.clone())),
            ("label", Json::Str(b.label.clone())),
            ("enabled", Json::Bool(b.enabled)),
            (
                "submenu",
                if b.submenu {
                    Json::Arr(Vec::new())
                } else {
                    Json::Null
                },
            ),
        ];
        if !b.icon.is_empty() {
            f.push(("icon", Json::Str(b.icon.into())));
        }
        items.push(Json::obj(f));
    }
    Json::obj(vec![
        ("open", Json::Bool(true)),
        ("target", Json::Str("filter".into())),
        ("column", Json::Str(column.into())),
        ("items", Json::Arr(items)),
        ("controls", field("controls")),
        ("buttons", Json::Arr(foot)),
    ])
}

/// A press the drop-down handles itself; `None` for any other dialog or
/// press (Cancel closes through the stack).
pub(crate) fn autofilter_click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    let DialogOwner::ProjectFilter { col } = top.owner else {
        return None;
    };
    let name = COLUMNS[col];
    let field = ENTRY_FIELDS[col];
    let [asc, desc] = sort_labels(column_kind(field));
    let clear = format!("Clear Filter from {name}");
    let pressed = |label: &str| {
        button.replace('&', "").trim().eq_ignore_ascii_case(label)
            && top.buttons.iter().any(|b| b.label == label && b.enabled)
    };
    let Surface::Project(v) = &mut tab.surface else {
        return Some(Err("this dialog belongs to a Project".into()));
    };
    if pressed(asc) || pressed(desc) {
        let sort = Sort {
            field,
            descending: pressed(desc),
        };
        v.ed.set_sort(Some(sort));
        tab.dialogs.pop();
        tab.status = sort_words(sort).into();
        return Some(Ok(()));
    }
    if pressed(&clear) {
        v.ed.set_filter(field, None);
        tab.dialogs.pop();
        tab.status = format!("Filter cleared from {name}").into();
        return Some(Ok(()));
    }
    if !pressed("OK") {
        return None;
    }
    // The list as the drop-down showed it: nothing else changed it while the
    // dialog was open.
    if !top.changed("values") {
        tab.dialogs.pop();
        return Some(Ok(()));
    }
    let checks = match control(top, "values").map(|c| &c.value) {
        Some(Value::Checks(c)) => c.clone(),
        _ => Vec::new(),
    };
    let tree = build_tree(
        &v.ed.column_values(field),
        column_kind(field) == ColumnKind::Date,
    );
    let allowed = tree.checked_texts(&checks);
    if allowed.is_empty() {
        return Some(Err("Select at least one value".into()));
    }
    // Everything checked is no filter at all.
    let all = allowed.len() == tree.leaf_count();
    v.ed.set_filter(field, (!all).then_some(allowed));
    tab.dialogs.pop();
    tab.status = if all {
        format!("Filter cleared from {name}")
    } else {
        format!("Filtered by {name}")
    }
    .into();
    Some(Ok(()))
}

/// The anchor of column `col`'s heading in the frame on screen. Every heading
/// is laid out and clipped by the table pane, so its probe holds bounds
/// outside the pane when it is scrolled out of view: the left edge is clamped
/// into the pane (`project-body` and the table's width). With no heading
/// probe yet (the surface's first frame), the window's top left stands in.
fn anchor_of(probes: &crate::Probes, col: usize, table_w: f32) -> Anchor {
    let Some(b) = probes.current(&format!("project-heading:{col}")) else {
        return Anchor {
            left: 0.,
            top: 0.,
            bottom: 0.,
        };
    };
    let mut left = f32::from(b.origin.x);
    if let Some(body) = probes.current("project-body") {
        let pane_left = f32::from(body.origin.x);
        left = left.clamp(pane_left, pane_left + table_w);
    }
    Anchor {
        left,
        top: f32::from(b.origin.y),
        bottom: f32::from(b.origin.y + b.size.height),
    }
}

impl Docxy {
    /// A heading's arrow (or `menu-open {"filter"}`): commit an open cell
    /// edit, then open column `col`'s drop-down.
    pub(crate) fn open_project_filter(
        &mut self,
        col: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.press_project(window, cx, |tab| {
            commit_project_cell(tab);
        })?;
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return Err("no tab is open".into());
        };
        let Surface::Project(v) = &tab.surface else {
            return Err("the active tab is not a Project".into());
        };
        let popup = place(anchor_of(&self.probes.borrow(), col, v.table_w), {
            let size = window.viewport_size();
            (f32::from(size.width), f32::from(size.height))
        });
        let d = menu_dialog(v, col, popup);
        tab.dialogs.push(d);
        cx.notify();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
