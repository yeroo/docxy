//! AutoFill and Home › Fill in the sheet tab (#668): the fill handle's drag
//! (in all four directions, dragged back inside to clear, Ctrl swapping copy
//! and series, the right-drag menu, the double-click), the Auto Fill Options
//! button, the Series dialog, Justify and the custom lists. The rules are
//! gridcore's ([`gridcore::edit::autofill`], [`gridcore::edit::series_changes_for`]);
//! this is where the grid gathers its input and keeps its undo steps.

use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::{DocTab, Docxy, SheetView, Surface};
use gridcore::edit::{
    FillKind, FillReq, FillTarget, Filled, JUSTIFY_OVERFLOW, SeriesSpec, SeriesType, fill_target,
};

/// The Auto Fill Options button after a fill: what that fill was, so a
/// choice from its menu runs it again as another kind. It stands only while
/// the workbook and the selection are as the fill left it (`view`,
/// `edit_gen`, `sel`); once it has not, it is gone for good (`gone`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FillOptions {
    pub view: u64,
    pub edit_gen: u64,
    pub src: (u32, u32, u32, u32),
    pub to: (u32, u32),
    pub kind: FillKind,
    /// Ctrl was held: a choice runs again with it held.
    pub ctrl: bool,
    /// The filled cells; the button sits at their bottom-right.
    pub dest: (u32, u32, u32, u32),
    /// The selection the fill left: another selection dismisses the button.
    pub sel: (u32, u32, u32, u32),
    /// Whether the source holds dates, numbers: which kinds the menu offers.
    pub dates: bool,
    pub numbers: bool,
    /// Set the first time it does not stand: a return to the same
    /// selection does not bring it back (#707 r2 i2).
    pub gone: std::cell::Cell<bool>,
}

impl FillOptions {
    /// Whether the button still stands on `v`: the workbook and the
    /// selection are as the fill left them.
    pub fn stands(&self, v: &SheetView) -> bool {
        let stands = !self.gone.get()
            && v.id == self.view
            && v.edit_gen == self.edit_gen
            && v.range() == self.sel
            && !v.multi_area();
        self.gone.set(!stands);
        stands
    }

    /// The kinds its menu offers.
    pub fn kinds(&self) -> Vec<FillKind> {
        crate::sheet_menus::fill_kinds(self.dates, self.numbers)
    }
}

impl SheetView {
    /// The source's kinds of seed: (dates, plain numbers).
    pub(crate) fn seed_kinds(&self, src: (u32, u32, u32, u32)) -> (bool, bool) {
        let (r0, c0, r1, c1) = src;
        let (mut dates, mut numbers) = (false, false);
        for (&(_, c), cell) in self.sheet().cells.range((r0, 0)..=(r1, u32::MAX)) {
            if !(c0..=c1).contains(&c) || cell.formula.is_some() {
                continue;
            }
            if let gridcore::sheet::CellValue::Number(_) = cell.value {
                let xf = self.pkg.workbook.styles.xf(cell.style);
                if gridcore::entry::is_date(&xf) {
                    dates = true;
                } else {
                    numbers = true;
                }
            }
        }
        (dates, numbers)
    }

    /// A fill-handle drag of `req` as one undo step: the cells fill (or, the
    /// handle dragged back inside, clear) and the selection becomes what the
    /// gesture leaves. `Ok(None)` when nothing changes; refused, with nothing
    /// changed, over part of an array.
    pub(crate) fn fill_drag(&mut self, req: &FillReq) -> Result<Option<Filled>, String> {
        let target = fill_target(req.src, req.to);
        let area = match target {
            FillTarget::None => return Ok(None),
            FillTarget::Extend { dest, .. } | FillTarget::Clear(dest) => dest,
        };
        let s = self.active;
        if self.engine.refuses_area(&self.pkg.workbook, s, area) {
            return Err(gridcore::engine::PART_OF_ARRAY.to_string());
        }
        self.push_undo();
        let filled = gridcore::edit::autofill(&mut self.pkg.workbook, s, req);
        let (r0, c0, r1, c1) = target.selection(req.src);
        self.anchor = (r0, c0);
        self.sel = (r1, c1);
        self.clear_areas();
        // Filled formulas were re-based, so their copied results are stale.
        self.engine = crate::sheet_engine(&self.pkg.workbook);
        self.engine.recalc_all(&mut self.pkg.workbook);
        Ok(filled)
    }

    /// Run the fill the Auto Fill Options button stands for again as
    /// `kind`, replacing the first: its undo step is taken back in place and
    /// the new fill takes it, so one undo returns to before either.
    pub(crate) fn refill(
        &mut self,
        opts: &FillOptions,
        kind: FillKind,
        lists: &[Vec<String>],
    ) -> Result<Option<Filled>, String> {
        if !opts.stands(self) {
            return Err("The fill has changed since; there is nothing to redo".into());
        }
        let req = FillReq {
            src: opts.src,
            to: opts.to,
            kind,
            ctrl: opts.ctrl,
            lists,
        };
        self.redo_last_step(|v| v.fill_drag(&req))
            .ok_or_else(|| "There is no fill to redo".to_string())?
    }

    /// Take the last undo step back in place and run `again` instead, as its
    /// replacement: one undo still returns to before either. When `again`
    /// fails (an array in the way, say), the step and the state it undid are
    /// put back as they were (#707 r1 M2). `None` when there is no step.
    pub(crate) fn redo_last_step<T>(
        &mut self,
        again: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Option<Result<T, String>> {
        let snap = self.undo.pop()?;
        let edit_gen = self.edit_gen;
        let now = self.snapshot_like(&snap);
        self.restore(snap);
        let before = self.snapshot_like(&now);
        let done = again(self);
        if done.is_err() {
            self.restore(now);
            self.undo.push(before);
            // Nothing changed after all: copy mode, which an edit ends,
            // stays on (#707 r2 m1).
            self.edit_gen = edit_gen;
        }
        Some(done)
    }

    /// Home › Fill › Series… over the selection, as one undo step. Refused
    /// over part of an array; `Ok(0)` when it wrote nothing.
    pub(crate) fn fill_series(
        &mut self,
        spec: &SeriesSpec,
        lists: &[Vec<String>],
    ) -> Result<usize, String> {
        let s = self.active;
        if self.sheet().is_protected() {
            return Err(crate::sheet_goto::SHEET_PROTECTED.into());
        }
        let changes =
            gridcore::edit::series_changes_for(&self.pkg.workbook, s, self.range(), spec, lists)?;
        if changes.is_empty() {
            return Ok(0);
        }
        if self.refuses(s, &changes) {
            return Err(gridcore::engine::PART_OF_ARRAY.to_string());
        }
        self.push_undo();
        let n = changes.len();
        self.engine
            .set_cells_prechecked(&mut self.pkg.workbook, s, changes);
        Ok(n)
    }

    /// Home › Fill › Justify: the text of the selection's first column,
    /// rewrapped to the selection's width in characters, one line a row.
    /// `Err(JUSTIFY_OVERFLOW)` when the lines need more rows than the
    /// selection has and `overflow` was not given (Excel asks, and OK writes
    /// on below it).
    pub(crate) fn justify(&mut self, overflow: bool) -> Result<bool, String> {
        let (r0, c0, r1, c1) = self.range();
        let s = self.active;
        if self.sheet().is_protected() {
            return Err(crate::sheet_goto::SHEET_PROTECTED.into());
        }
        let sheet = self.sheet();
        let texts: Vec<String> = (r0..=r1)
            .filter_map(|r| match sheet.cell(r, c0).map(|c| &c.value) {
                Some(gridcore::sheet::CellValue::Text(t)) => Some(t.clone()),
                Some(v) if !v.is_empty() => Some(self.cell_text(r, c0)),
                _ => None,
            })
            .collect();
        if texts.is_empty() {
            return Ok(false);
        }
        let width: f64 = (c0..=c1).map(|c| sheet.col_width(c)).sum();
        let lines = gridcore::edit::justify_lines(&texts, width.floor().max(1.0) as usize);
        let rows = (r1 - r0 + 1) as usize;
        if lines.len() > rows && !overflow {
            return Err(JUSTIFY_OVERFLOW.to_string());
        }
        let last = r0 as usize + rows.max(lines.len()) - 1;
        let mut changes = Vec::new();
        for (i, r) in (r0 as usize..=last).enumerate() {
            let r = r as u32;
            let style = sheet.cell(r, c0).map_or(0, |c| c.style);
            let mut cell = match lines.get(i) {
                Some(l) => gridcore::sheet::Cell::text(l),
                None => gridcore::sheet::Cell::default(),
            };
            cell.style = style;
            changes.push((r, c0, cell));
        }
        if self.refuses(s, &changes) {
            return Err(gridcore::engine::PART_OF_ARRAY.to_string());
        }
        self.push_undo();
        self.engine
            .set_cells_prechecked(&mut self.pkg.workbook, s, changes);
        Ok(true)
    }
}

const SERIES_IN: [&str; 2] = ["Rows", "Columns"];
const TYPES: [&str; 4] = ["Linear", "Growth", "Date", "AutoFill"];
const UNITS: [&str; 4] = ["Day", "Weekday", "Month", "Year"];

fn radio(name: &'static str, label: &str, items: &[&str], at: usize) -> Control {
    let mut c = Control::new(name, label, ControlKind::Radio, Value::Choice(Some(at)));
    c.items = items.iter().map(|s| s.to_string()).collect();
    c
}

/// Home › Fill › Series…: Series in, Type, Date unit, Trend, Step and Stop
/// value, Excel's defaults for the selection.
pub(crate) fn series_dialog(tab: &DocTab) -> Result<Dialog, String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("Series needs a spreadsheet".into());
    };
    if v.sheet().is_protected() {
        return Err(crate::sheet_goto::SHEET_PROTECTED.into());
    }
    let rows = gridcore::edit::series_rows_for(v.range());
    let mut d = Dialog::message(
        "series",
        "Series",
        String::new(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::Series,
    );
    d.text = None;
    d.controls = vec![
        radio("series-in", "Series in", &SERIES_IN, usize::from(!rows)),
        radio("type", "Type", &TYPES, 0),
        radio("unit", "Date unit", &UNITS, 0),
        Control::new("trend", "Trend", ControlKind::Checkbox, Value::Bool(false)),
        Control::new(
            "step",
            "Step value:",
            ControlKind::Text,
            Value::Text("1".into()),
        ),
        Control::new(
            "stop",
            "Stop value:",
            ControlKind::Text,
            Value::Text(String::new()),
        ),
    ];
    d.react = Some(crate::dialog::Reaction(series_react));
    series_react(&mut d, 0, &Value::Bool(false));
    d.mark_opened();
    Ok(d)
}

/// Excel greys out the Stop value for AutoFill, which fills the selection
/// only (#707 r2 M1), and the Date unit for anything but Date.
fn series_react(d: &mut Dialog, _changed: usize, _before: &Value) {
    let ty = choice(d, "type").unwrap_or(0);
    for c in &mut d.controls {
        match c.name {
            "stop" => c.enabled = ty != 3,
            "unit" => c.enabled = ty == 2,
            _ => {}
        }
    }
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

fn text(d: &Dialog, name: &str) -> String {
    d.controls
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.text())
        .unwrap_or_default()
}

/// What the Series dialog `d` stages, or why its numbers do not read.
pub(crate) fn staged_series(d: &Dialog) -> Result<SeriesSpec, String> {
    let num = |name: &str, label: &str| -> Result<Option<f64>, String> {
        let t = text(d, name);
        let t = t.trim();
        if t.is_empty() {
            return Ok(None);
        }
        t.parse::<f64>()
            .map(Some)
            .map_err(|_| format!("{label} must be a number"))
    };
    let unit = match choice(d, "unit").unwrap_or(0) {
        1 => FillKind::Weekdays,
        2 => FillKind::Months,
        3 => FillKind::Years,
        _ => FillKind::Days,
    };
    let kind = match choice(d, "type").unwrap_or(0) {
        1 => SeriesType::Growth,
        2 => SeriesType::Date(unit),
        3 => SeriesType::AutoFill,
        _ => SeriesType::Linear,
    };
    Ok(SeriesSpec {
        rows: choice(d, "series-in") == Some(0),
        kind,
        step: num("step", "Step value")?.unwrap_or(1.0),
        stop: num("stop", "Stop value")?,
        trend: d
            .controls
            .iter()
            .any(|c| c.name == "trend" && c.value == Value::Bool(true)),
    })
}

/// Excel's Justify question: OK writes on below the selection.
fn justify_question() -> Dialog {
    let mut d = Dialog::message(
        "justify",
        "Microsoft Excel",
        JUSTIFY_OVERFLOW.to_string(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::JustifyOverflow,
    );
    d.mark_opened();
    d
}

/// Edit Custom Lists: the built-in lists (read only) and the user's, an
/// entry box, Add, Delete, Import (from the cells selected when it opened),
/// OK and Cancel. The lists it edits live in a hidden grid, one row each.
pub(crate) fn custom_lists_dialog(lists: &[Vec<String>], import: Vec<String>) -> Dialog {
    let mut d = Dialog::message(
        "custom-lists",
        "Custom Lists",
        String::new(),
        &[
            ("Add", ButtonRole::Accept),
            ("Delete", ButtonRole::Accept),
            ("Import", ButtonRole::Accept),
            ("OK", ButtonRole::Accept),
            ("Cancel", ButtonRole::Cancel),
        ],
        DialogOwner::CustomLists,
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
    let mut shown = Control::new(
        "lists",
        "Custom lists:",
        ControlKind::List,
        Value::Choice(None),
    );
    let mut data = Control::new("data", "", ControlKind::Grid, Value::Rows(lists.to_vec()));
    data.visible = false;
    let mut imp = Control::new(
        "import",
        "",
        ControlKind::Label,
        Value::Text(import.join("\n")),
    );
    imp.visible = false;
    shown.items = list_labels(lists);
    d.controls = vec![
        shown,
        Control::new(
            "entries",
            "List entries:",
            ControlKind::Text,
            Value::Text(String::new()),
        ),
        data,
        imp,
    ];
    d.mark_opened();
    d
}

/// The list control's lines: NEW LIST, the built-ins, then the user's.
fn list_labels(lists: &[Vec<String>]) -> Vec<String> {
    std::iter::once("NEW LIST".to_string())
        .chain(gridcore::edit::builtin_lists().iter().map(|l| l.join(", ")))
        .chain(lists.iter().map(|l| l.join(", ")))
        .collect()
}

/// Built-in lists come after NEW LIST in the list control.
const FIRST_USER: usize = 5;

fn staged_lists(d: &Dialog) -> Vec<Vec<String>> {
    d.controls
        .iter()
        .find(|c| c.name == "data")
        .and_then(|c| match &c.value {
            Value::Rows(r) => Some(r.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn set_lists(d: &mut Dialog, lists: Vec<Vec<String>>) {
    let labels = list_labels(&lists);
    for c in &mut d.controls {
        match c.name {
            "data" => c.value = Value::Rows(lists.clone()),
            "lists" => {
                c.items = labels.clone();
                c.value = Value::Choice(None);
            }
            "entries" => c.value = Value::Text(String::new()),
            _ => {}
        }
    }
}

fn presses(d: &Dialog, button: &str, label: &str) -> bool {
    button.replace('&', "").trim().eq_ignore_ascii_case(label)
        && d.buttons.iter().any(|b| b.label == label && b.enabled)
}

/// What a press on the Custom Lists dialog leaves: `Some(lists)` to store
/// (OK). Add, Delete and Import edit the staged lists in place.
pub(crate) fn custom_lists_click(
    tab: &mut DocTab,
    button: &str,
) -> Option<Result<Option<Vec<Vec<String>>>, String>> {
    let top = tab.dialogs.top()?;
    if top.owner != DialogOwner::CustomLists {
        return None;
    }
    let mut lists = staged_lists(top);
    if presses(top, button, "Add") || presses(top, button, "Import") {
        let source = if presses(top, button, "Add") {
            text(top, "entries")
        } else {
            text(top, "import")
        };
        let items = gridcore::options::parse_list_entries(&source);
        if items.is_empty() {
            return Some(Err("Type the list's entries first".into()));
        }
        lists.push(items);
        let d = tab.dialogs.top_dialog_mut().ok()?;
        set_lists(d, lists);
        return Some(Ok(None));
    }
    if presses(top, button, "Delete") {
        let Some(i) = choice(top, "lists").filter(|&i| i >= FIRST_USER) else {
            return Some(Err("Only a list you added can be deleted".into()));
        };
        lists.remove(i - FIRST_USER);
        let d = tab.dialogs.top_dialog_mut().ok()?;
        set_lists(d, lists);
        return Some(Ok(None));
    }
    if presses(top, button, "OK") {
        tab.dialogs.pop();
        return Some(Ok(Some(lists)));
    }
    None
}

/// A press on the Justify question: OK justifies on below the selection.
pub(crate) fn justify_click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    if top.owner != DialogOwner::JustifyOverflow || !presses(top, button, "OK") {
        return None;
    }
    tab.dialogs.pop();
    let Surface::Sheet(v) = &mut tab.surface else {
        return Some(Err("Justify needs a spreadsheet".into()));
    };
    let done = v.justify(true).map(|changed| {
        if changed {
            tab.dirty = true;
        }
    });
    Some(done)
}

impl Docxy {
    /// The presses of the fill dialogs whose OK needs the app's custom lists
    /// (Series, whose AutoFill type reads them; Edit Custom Lists, which
    /// stores them), taken before the tab's own path.
    pub(crate) fn fill_dialog_click(&mut self, button: &str) -> Option<Result<(), String>> {
        let tab = self.tabs.get_mut(self.active)?;
        if let Some(done) = custom_lists_click(tab, button) {
            return Some(done.map(|stored| {
                if let Some(lists) = stored {
                    self.custom_lists = lists;
                    self.persist();
                }
            }));
        }
        if let Some(done) = justify_click(tab, button) {
            return Some(done);
        }
        let top = tab.dialogs.top()?;
        if top.owner != DialogOwner::Series || !presses(top, button, "OK") {
            return None;
        }
        let spec = match staged_series(top) {
            Ok(s) => s,
            Err(e) => return Some(Err(e)),
        };
        let Surface::Sheet(v) = &mut tab.surface else {
            return Some(Err("Series needs a spreadsheet".into()));
        };
        let lists = self.custom_lists.clone();
        match v.fill_series(&spec, &lists) {
            Ok(n) => {
                tab.dialogs.pop();
                if n > 0 {
                    tab.dirty = true;
                }
                Some(Ok(()))
            }
            Err(e) => Some(Err(e)),
        }
    }

    /// Open the Series dialog, the Justify question when Justify overflows,
    /// or Edit Custom Lists.
    pub(crate) fn fill_menu_act(&mut self, act: crate::SheetAct) {
        let lists = self.custom_lists.clone();
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        match act {
            crate::SheetAct::FillSeries => match series_dialog(tab) {
                Ok(d) => tab.dialogs.push(d),
                Err(e) => tab.status = e.into(),
            },
            crate::SheetAct::FillJustify => {
                let Surface::Sheet(v) = &mut tab.surface else {
                    return;
                };
                match v.justify(false) {
                    Ok(true) => tab.dirty = true,
                    Ok(false) => tab.status = "Nothing to justify".into(),
                    Err(e) if e == JUSTIFY_OVERFLOW => tab.dialogs.push(justify_question()),
                    Err(e) => tab.status = e.into(),
                }
            }
            crate::SheetAct::CustomLists => {
                let import = match &tab.surface {
                    Surface::Sheet(v) => {
                        let (r0, c0, r1, c1) = v.range();
                        (r0..=r1)
                            .flat_map(|r| (c0..=c1).map(move |c| (r, c)))
                            .map(|(r, c)| v.cell_text(r, c))
                            .filter(|t| !t.trim().is_empty())
                            .collect()
                    }
                    _ => Vec::new(),
                };
                tab.dialogs.push(custom_lists_dialog(&lists, import));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "sheet_fill_dialog_tests.rs"]
mod tests;
