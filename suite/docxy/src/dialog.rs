//! Dialogs as app state (#393).
//!
//! A dialog is a value on its document tab's [`DialogStack`], never a native
//! modal loop: the window draws the top one over everything, the keyboard
//! reaches it through [`DialogStack::key_button`], and the harness verbs
//! `dialog-read`, `dialog-set`, `dialog-tab` and `dialog-click` drive it. The
//! drawn buttons, Enter/Escape and `dialog-click` all press through
//! [`DialogStack::click`]. A form's controls are editable widgets (#649): a
//! click focuses a field, toggles a checkbox, picks a radio item or steps a
//! dropdown, and typed keys edit the focused field. Every one of those, and
//! `dialog-set`, changes a value through `Control::set`. A modal loop would
//! block the harness pump; this cannot.
//!
//! Staged values live in the [`Dialog`] until an accept button hands it to its
//! owner, so Cancel discards by construction: it drops the dialog.
//!
//! The vocabulary exists before the dialogs that need most of it (the issue
//! asks for it first), so the control kinds and the nested-dialog role have no
//! caller outside the tests until the first form dialog ships.
#![cfg_attr(not(test), allow(dead_code))]

use ctlcore::json::Json;

/// What an accept button applies the dialog to. The app matches it in
/// `apply_dialog`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DialogOwner {
    /// Delete this summary task and its subtasks.
    DeleteSummary {
        uid: i32,
    },
    /// Word's Page Setup, for the caret's sections (#649).
    PageSetup,
    /// Word's Columns, for the caret's sections (#649).
    Columns,
    /// The Header & Footer tab's Header from Top (`is_header`) or Footer from
    /// Bottom Custom... box, for one section (#641).
    HfDistance {
        is_header: bool,
        section: usize,
    },
    /// Word's Page Number Format, for one section (#650).
    PageNumberFormat {
        section: usize,
    },
    /// The table dialogs (#646, #647), for the edited story's caret or
    /// selection.
    InsertTable,
    DeleteCells,
    SplitCells,
    SortTable,
    TableToText,
    TextToTable,
    /// Excel's Convert Text to Columns Wizard over one column (#692).
    TextToColumns {
        sheet: usize,
        col: u32,
        r1: u32,
        r2: u32,
    },
    /// Its "Do you want to replace the contents of the destination cells?".
    TextToColumnsReplace,
    /// A dialog the model tests build; the app never applies one.
    #[cfg(test)]
    Test,
}

/// A dialog a button opens on top of its own, built from the parent when the
/// button is pressed. An enum rather than a constructor function so buttons
/// stay comparable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildDialog {
    #[cfg(test)]
    Test,
}

impl ChildDialog {
    fn build(self, _parent: &Dialog) -> Dialog {
        match self {
            #[cfg(test)]
            Self::Test => tests_support::child(_parent),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ButtonRole {
    /// Hand the staged values to the owner and close (OK, Yes).
    Accept,
    /// Hand them to the owner and stay open (Apply).
    Apply,
    /// Close and discard (Cancel, No). Escape presses it.
    Cancel,
    /// Open a nested dialog on top of this one.
    Open(ChildDialog),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Button {
    pub label: String,
    pub enabled: bool,
    /// Enter presses it.
    pub default: bool,
    pub role: ButtonRole,
}

impl Button {
    pub fn new(label: &str, role: ButtonRole) -> Self {
        Self {
            label: label.into(),
            enabled: true,
            default: false,
            role,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlKind {
    Text,
    Number,
    Date,
    Duration,
    Checkbox,
    Radio,
    Dropdown,
    List,
    Grid,
    Label,
}

impl ControlKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Date => "date",
            Self::Duration => "duration",
            Self::Checkbox => "checkbox",
            Self::Radio => "radio",
            Self::Dropdown => "dropdown",
            Self::List => "list",
            Self::Grid => "grid",
            Self::Label => "label",
        }
    }
    fn has_items(self) -> bool {
        matches!(self, Self::Radio | Self::Dropdown | Self::List)
    }
    /// A field typed into.
    pub fn is_text(self) -> bool {
        matches!(
            self,
            Self::Text | Self::Number | Self::Date | Self::Duration
        )
    }
    /// A control the keyboard and pointer can edit: a list and a grid are
    /// still read-only widgets, and a label is never edited.
    pub fn is_editable(self) -> bool {
        self.is_text() || matches!(self, Self::Checkbox | Self::Radio | Self::Dropdown)
    }
}

/// Text a number field accepts while it is being typed: a number, or the
/// start of one (empty, a sign, a trailing point). The owner parses it on OK.
fn partial_number(s: &str) -> bool {
    let t = s.trim();
    let body = t.strip_prefix(['-', '+']).unwrap_or(t);
    body.chars().all(|c| c.is_ascii_digit() || c == '.') && body.matches('.').count() <= 1
}

/// A control's staged value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    /// Text, number, date and duration fields, and a label's text. Dates and
    /// durations stay text until the owner parses them on OK, as Project's
    /// own dialogs report a bad date only then.
    Text(String),
    Bool(bool),
    /// The selected item of a radio group, dropdown or list.
    Choice(Option<usize>),
    /// A grid's rows, one string per column.
    Rows(Vec<Vec<String>>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Control {
    /// Stable, for scripts; the label is what a person reads.
    pub name: &'static str,
    pub label: String,
    pub kind: ControlKind,
    pub value: Value,
    pub enabled: bool,
    pub visible: bool,
    /// The tab it sits on; `None` shows on every tab.
    pub page: Option<usize>,
    /// Radio, dropdown and list choices, in order.
    pub items: Vec<String>,
    /// A grid's column headings.
    pub columns: Vec<String>,
}

impl Control {
    pub fn new(name: &'static str, label: &str, kind: ControlKind, value: Value) -> Self {
        Self {
            name,
            label: label.into(),
            kind,
            value,
            enabled: true,
            visible: true,
            page: None,
            items: Vec::new(),
            columns: Vec::new(),
        }
    }

    /// What the control shows.
    pub fn text(&self) -> String {
        match &self.value {
            Value::Text(s) => s.clone(),
            Value::Bool(b) => if *b { "checked" } else { "unchecked" }.into(),
            Value::Choice(i) => i
                .and_then(|i| self.items.get(i))
                .cloned()
                .unwrap_or_default(),
            Value::Rows(rows) => match rows.len() {
                1 => "1 row".into(),
                n => format!("{n} rows"),
            },
        }
    }

    fn to_json(&self) -> Json {
        let value = match &self.value {
            Value::Text(s) if self.kind == ControlKind::Number => s
                .trim()
                .parse::<f64>()
                .map_or_else(|_| Json::Str(s.clone()), Json::Num),
            Value::Text(s) => Json::Str(s.clone()),
            Value::Bool(b) => Json::Bool(*b),
            Value::Choice(i) => i
                .and_then(|i| self.items.get(i))
                .map_or(Json::Null, |s| Json::Str(s.clone())),
            Value::Rows(rows) => rows_json(rows),
        };
        let mut out = vec![
            ("name", Json::Str(self.name.into())),
            ("label", Json::Str(shown(&self.label))),
            ("kind", Json::Str(self.kind.name().into())),
            ("value", value),
            ("text", Json::Str(self.text())),
            ("enabled", Json::Bool(self.enabled)),
            ("visible", Json::Bool(self.visible)),
        ];
        if self.kind.has_items() {
            out.push(("items", strs(&self.items)));
            out.push((
                "selected",
                match &self.value {
                    Value::Choice(Some(i)) => Json::Num(*i as f64),
                    _ => Json::Null,
                },
            ));
        }
        if let Value::Rows(rows) = &self.value {
            out.push(("columns", strs(&self.columns)));
            out.push(("rows", rows_json(rows)));
        }
        Json::obj(out)
    }

    /// The control's input handler. `dialog-set` calls it, and so will a form's
    /// editable widget once one is drawn (today's overlay draws controls
    /// read-only). `args` is the verb's argument object: `value`,
    /// or for a grid `{row, column, value}`, `{insert_row}` or `{delete_row}`.
    fn set(&mut self, args: &Json) -> Result<(), String> {
        let label = &shown(&self.label);
        if !self.visible {
            return Err(format!("'{label}' is hidden"));
        }
        if !self.enabled {
            return Err(format!("'{label}' is disabled"));
        }
        let value = || args.get("value").ok_or("missing argument 'value'");
        let text = || match value()? {
            Json::Str(s) => Ok(s.clone()),
            _ => Err(format!("'{label}' takes text")),
        };
        self.value = match self.kind {
            ControlKind::Label => return Err(format!("'{label}' is a label; it cannot be set")),
            ControlKind::Text | ControlKind::Date | ControlKind::Duration => Value::Text(text()?),
            ControlKind::Number => match value()? {
                Json::Num(n) if n.is_finite() => Value::Text(n.to_string()),
                // Rust parses "NaN", "inf" and "1e999"; a field takes none of them.
                Json::Str(s)
                    if s.trim().parse::<f64>().is_ok_and(f64::is_finite) || partial_number(s) =>
                {
                    Value::Text(s.clone())
                }
                _ => return Err(format!("'{label}' takes a number")),
            },
            ControlKind::Checkbox => match value()? {
                Json::Bool(b) => Value::Bool(*b),
                _ => return Err(format!("'{label}' takes true or false")),
            },
            ControlKind::Radio | ControlKind::Dropdown | ControlKind::List => {
                let want = text()?;
                let i = self
                    .items
                    .iter()
                    .position(|item| fold(item) == fold(&want))
                    .ok_or_else(|| {
                        format!(
                            "'{label}' has no item '{want}'; items: {}",
                            self.items.join(", ")
                        )
                    })?;
                Value::Choice(Some(i))
            }
            ControlKind::Grid => {
                let mut rows = match &self.value {
                    Value::Rows(rows) => rows.clone(),
                    _ => Vec::new(),
                };
                self.edit_grid(&mut rows, args)?;
                Value::Rows(rows)
            }
        };
        Ok(())
    }

    fn edit_grid(&self, rows: &mut Vec<Vec<String>>, args: &Json) -> Result<(), String> {
        let label = &shown(&self.label);
        let index = |key: &str| {
            args.get(key)
                .map(|v| {
                    v.as_usize()
                        .ok_or_else(|| format!("'{key}' must be a whole number, not below zero"))
                })
                .transpose()
        };
        if let Some(at) = index("insert_row")? {
            if at > rows.len() {
                return Err(format!(
                    "'{label}' has {} rows; insert at 0..={}",
                    rows.len(),
                    rows.len()
                ));
            }
            rows.insert(at, vec![String::new(); self.columns.len()]);
            return Ok(());
        }
        if let Some(at) = index("delete_row")? {
            if at >= rows.len() {
                return Err(no_row(label, at, rows.len()));
            }
            rows.remove(at);
            return Ok(());
        }
        let row = index("row")?
            .ok_or("a grid takes {row, column, value}, {insert_row} or {delete_row}")?;
        if row >= rows.len() {
            return Err(no_row(label, row, rows.len()));
        }
        let column = match args.get("column") {
            Some(Json::Str(name)) => self
                .columns
                .iter()
                .position(|c| fold(c) == fold(name))
                .ok_or_else(|| {
                    format!(
                        "'{label}' has no column '{name}'; columns: {}",
                        self.columns.join(", ")
                    )
                })?,
            Some(v) => v
                .as_usize()
                .filter(|c| *c < self.columns.len())
                .ok_or_else(|| {
                    format!(
                        "'column' must be a column name or 0..{}",
                        self.columns.len()
                    )
                })?,
            None => return Err("missing argument 'column'".into()),
        };
        let Some(Json::Str(value)) = args.get("value") else {
            return Err(format!("'{label}' cells take text"));
        };
        rows[row][column] = value.clone();
        Ok(())
    }
}

fn no_row(label: &str, row: usize, len: usize) -> String {
    match len {
        0 => format!("'{label}' has no rows"),
        n => format!("'{label}' has no row {row}; rows 0..{}", n - 1),
    }
}

fn strs(items: &[String]) -> Json {
    Json::Arr(items.iter().cloned().map(Json::Str).collect())
}

fn rows_json(rows: &[Vec<String>]) -> Json {
    Json::Arr(rows.iter().map(|r| strs(r)).collect())
}

/// A label as it is drawn: without its `&` accelerator mark.
fn shown(label: &str) -> String {
    label.replace('&', "")
}

/// How a label is matched: case-insensitive, without `&` accelerator marks or
/// a trailing colon, so `"&Name:"` answers to `name`.
fn fold(label: &str) -> String {
    label
        .replace('&', "")
        .trim()
        .trim_end_matches(':')
        .trim()
        .to_lowercase()
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Dialog {
    /// Stable, for scripts: `delete-summary`.
    pub id: &'static str,
    pub title: String,
    /// A message box's message.
    pub text: Option<String>,
    /// Tab (page) labels; empty for a single-page dialog.
    pub tabs: Vec<String>,
    pub tab: usize,
    pub controls: Vec<Control>,
    pub buttons: Vec<Button>,
    pub owner: DialogOwner,
    /// The control typed keys go to (an index into `controls`).
    pub focus: Option<usize>,
    /// The values the dialog opened on, one per control (see
    /// [`Dialog::changed`]); empty for a dialog that does not track them.
    pub opened: Vec<Value>,
    /// The owner's reaction to a change of control `i` (whose value was
    /// `before`): Page Setup's paper size following its sides, say. Set by the
    /// dialog's builder.
    pub react: Option<Reaction>,
}

/// See [`Dialog::react`]. Two dialogs compare equal whatever their
/// reactions: a function pointer has no meaningful identity.
#[derive(Clone, Copy)]
pub(crate) struct Reaction(pub fn(&mut Dialog, usize, &Value));

impl PartialEq for Reaction {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for Reaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reaction")
    }
}

impl Dialog {
    /// A message box: a title, a message and buttons. The first button is the
    /// default, as in Office's message boxes.
    pub fn message(
        id: &'static str,
        title: &str,
        text: String,
        buttons: &[(&str, ButtonRole)],
        owner: DialogOwner,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            text: Some(text),
            tabs: Vec::new(),
            tab: 0,
            controls: Vec::new(),
            buttons: buttons
                .iter()
                .enumerate()
                .map(|(i, (label, role))| Button {
                    default: i == 0,
                    ..Button::new(label, *role)
                })
                .collect(),
            owner,
            focus: None,
            opened: Vec::new(),
            react: None,
        }
    }

    /// The controls on the current tab, as a person sees them.
    fn on_page(&self, c: &Control) -> bool {
        c.page.is_none_or(|p| p == self.tab)
    }

    /// The indexes of the controls drawn on the current tab: on it, and
    /// visible.
    pub fn shown_indices(&self) -> Vec<usize> {
        (0..self.controls.len())
            .filter(|&i| {
                let c = &self.controls[i];
                c.visible && self.on_page(c)
            })
            .collect()
    }

    pub fn page_controls(&self) -> impl Iterator<Item = &Control> {
        self.controls.iter().filter(|c| self.on_page(c))
    }

    /// Find a control by its visible label, then by its name. A label two
    /// controls share is refused rather than guessed.
    fn control_index(&self, want: &str) -> Result<usize, String> {
        let by_label: Vec<usize> = (0..self.controls.len())
            .filter(|&i| fold(&self.controls[i].label) == fold(want))
            .collect();
        let found = match by_label.as_slice() {
            [i] => Some(*i),
            [] => self.controls.iter().position(|c| c.name == want),
            many => {
                let names: Vec<&str> = many.iter().map(|&i| self.controls[i].name).collect();
                return Err(format!(
                    "'{want}' names {} controls; use a name: {}",
                    many.len(),
                    names.join(", ")
                ));
            }
        };
        let i = found.ok_or_else(|| {
            let names: Vec<String> = self
                .page_controls()
                .filter(|c| c.visible)
                .map(|c| shown(&c.label))
                .collect();
            match names.as_slice() {
                [] => format!("no control '{want}'; this dialog has none"),
                names => format!("no control '{want}'; controls: {}", names.join(", ")),
            }
        })?;
        let c = &self.controls[i];
        if !self.on_page(c) {
            let page = c
                .page
                .and_then(|p| self.tabs.get(p))
                .map_or("", String::as_str);
            return Err(format!(
                "'{}' is on the '{page}' tab; switch with dialog-tab first",
                shown(&c.label)
            ));
        }
        Ok(i)
    }

    /// Set one control through its input handler.
    pub fn set(&mut self, control: &str, args: &Json) -> Result<(), String> {
        let i = self.control_index(control)?;
        self.set_at(i, args)
    }

    /// Every change to a control's value comes here: its input handler, then
    /// the owner's reaction (Page Setup's paper size follows its width).
    fn set_at(&mut self, i: usize, args: &Json) -> Result<(), String> {
        let before = self.controls[i].value.clone();
        self.controls[i].set(args)?;
        if let Some(Reaction(react)) = self.react {
            react(self, i, &before);
        }
        Ok(())
    }

    /// Whether a control's value differs from the one the dialog opened on.
    /// Page Setup and Columns use it for the fields OK writes only when they
    /// changed (section start, gutter position, multiple pages, the column
    /// layout); Page Setup's page part has its own rule, in `page_setup`.
    pub fn changed(&self, name: &str) -> bool {
        let Some(i) = self.controls.iter().position(|c| c.name == name) else {
            return false;
        };
        self.opened.get(i) != Some(&self.controls[i].value)
    }

    /// Take the current values as the ones the dialog opened on.
    pub fn mark_opened(&mut self) {
        self.opened = self.controls.iter().map(|c| c.value.clone()).collect();
    }

    /// The controls Tab steps through on the current tab, in order.
    fn focusable(&self) -> Vec<usize> {
        (0..self.controls.len())
            .filter(|&i| {
                let c = &self.controls[i];
                self.on_page(c) && c.visible && c.enabled && c.kind.is_editable()
            })
            .collect()
    }

    /// The focused control, when it is still on the current tab.
    pub fn focused(&self) -> Option<&Control> {
        self.focus
            .filter(|i| self.focusable().contains(i))
            .map(|i| &self.controls[i])
    }

    /// Move the focus to the next (or previous) editable control, wrapping.
    pub fn focus_step(&mut self, back: bool) {
        let order = self.focusable();
        if order.is_empty() {
            self.focus = None;
            return;
        }
        let at = self.focus.and_then(|f| order.iter().position(|&i| i == f));
        let next = match (at, back) {
            (None, false) => 0,
            (None, true) => order.len() - 1,
            (Some(i), false) => (i + 1) % order.len(),
            (Some(i), true) => (i + order.len() - 1) % order.len(),
        };
        self.focus = Some(order[next]);
    }

    /// Change the focused field's text through its input handler.
    fn edit_focused(&mut self, edit: impl FnOnce(&mut String)) -> Result<(), String> {
        let Some(c) = self.focused() else {
            return Ok(());
        };
        if !c.kind.is_text() {
            return Ok(());
        }
        let mut text = c.text();
        edit(&mut text);
        let i = self.focus.unwrap_or_default();
        self.set_at(i, &Json::obj(vec![("value", Json::Str(text))]))
    }

    /// A typed character: appended to the focused field. A character the
    /// field refuses (a letter in a number) is the error, and changes nothing.
    pub fn type_char(&mut self, ch: char) -> Result<(), String> {
        self.edit_focused(|t| t.push(ch))
    }

    /// Backspace in the focused field.
    pub fn backspace(&mut self) -> Result<(), String> {
        self.edit_focused(|t| {
            t.pop();
        })
    }

    /// A press on a control, as the pointer makes it: a field takes the
    /// focus, a checkbox toggles, a radio picks `item`, a dropdown steps to its
    /// next item (or picks `item`). Each change goes through `Control::set`.
    pub fn click_control(&mut self, index: usize, item: Option<usize>) -> Result<(), String> {
        let c = self.controls.get(index).ok_or("no such control")?;
        if !self.focusable().contains(&index) {
            return Err(format!("'{}' cannot be edited", shown(&c.label)));
        }
        self.focus = Some(index);
        let value = match (c.kind, &c.value) {
            (ControlKind::Checkbox, Value::Bool(b)) => Json::Bool(!b),
            (ControlKind::Radio | ControlKind::Dropdown, Value::Choice(cur)) => {
                let n = c.items.len().max(1);
                let pick = match (item, c.kind) {
                    (Some(i), _) => i,
                    (None, ControlKind::Dropdown) => cur.map_or(0, |i| (i + 1) % n),
                    (None, _) => return Ok(()),
                };
                Json::Str(c.items.get(pick).ok_or("no such item")?.clone())
            }
            _ => return Ok(()),
        };
        self.set_at(index, &Json::obj(vec![("value", value)]))
    }

    /// Up or Down on the focused radio group or dropdown: the item before or
    /// after the chosen one.
    pub fn step_focused(&mut self, down: bool) -> Result<(), String> {
        let Some(c) = self.focused() else {
            return Ok(());
        };
        let (Value::Choice(cur), true) = (&c.value, c.kind.has_items()) else {
            return Ok(());
        };
        let n = c.items.len();
        if n == 0 {
            return Ok(());
        }
        let pick = match (cur, down) {
            (None, _) => 0,
            (Some(i), true) => (i + 1).min(n - 1),
            (Some(i), false) => i.saturating_sub(1),
        };
        let i = self.focus.unwrap_or_default();
        self.click_control(i, Some(pick))
    }

    /// Space: toggles a focused checkbox; a field takes it as a character.
    pub fn space(&mut self) -> Result<(), String> {
        match self.focused().map(|c| c.kind) {
            Some(ControlKind::Checkbox) => {
                let i = self.focus.unwrap_or_default();
                self.click_control(i, None)
            }
            _ => self.type_char(' '),
        }
    }

    /// Switch to a tab by its label.
    pub fn select_tab(&mut self, want: &str) -> Result<(), String> {
        if self.tabs.is_empty() {
            return Err(format!("'{}' has no tabs", self.title));
        }
        self.tab = self
            .tabs
            .iter()
            .position(|t| fold(t) == fold(want))
            .ok_or_else(|| format!("no tab '{want}'; tabs: {}", self.tabs.join(", ")))?;
        if self.focused().is_none() {
            self.focus = None;
        }
        Ok(())
    }

    fn button_index(&self, want: &str) -> Result<usize, String> {
        let i = self
            .buttons
            .iter()
            .position(|b| fold(&b.label) == fold(want))
            .ok_or_else(|| {
                let labels: Vec<String> = self.buttons.iter().map(|b| shown(&b.label)).collect();
                format!("no button '{want}'; buttons: {}", labels.join(", "))
            })?;
        if !self.buttons[i].enabled {
            return Err(format!("'{}' is disabled", shown(&self.buttons[i].label)));
        }
        Ok(i)
    }

    /// The staged value of a control, by name, for the owner applying it.
    pub fn value(&self, name: &str) -> Option<&Value> {
        self.controls
            .iter()
            .find(|c| c.name == name)
            .map(|c| &c.value)
    }

    fn to_json(&self, depth: usize) -> Json {
        Json::obj(vec![
            ("open", Json::Bool(true)),
            ("id", Json::Str(self.id.into())),
            ("depth", Json::Num(depth as f64)),
            ("title", Json::Str(self.title.clone())),
            ("text", self.text.clone().map_or(Json::Null, Json::Str)),
            ("tabs", strs(&self.tabs)),
            (
                "tab",
                self.tabs
                    .get(self.tab)
                    .cloned()
                    .map_or(Json::Null, Json::Str),
            ),
            (
                "controls",
                Json::Arr(self.page_controls().map(Control::to_json).collect()),
            ),
            (
                "buttons",
                Json::Arr(
                    self.buttons
                        .iter()
                        .map(|b| {
                            Json::obj(vec![
                                ("label", Json::Str(shown(&b.label))),
                                ("enabled", Json::Bool(b.enabled)),
                                ("default", Json::Bool(b.default)),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }
}

/// A tab's open dialogs, the top one last. Only the top one takes input.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DialogStack(Vec<Dialog>);

impl DialogStack {
    pub fn top(&self) -> Option<&Dialog> {
        self.0.last()
    }
    pub fn is_open(&self) -> bool {
        !self.0.is_empty()
    }
    pub fn depth(&self) -> usize {
        self.0.len()
    }
    pub fn push(&mut self, dialog: Dialog) {
        self.0.push(dialog);
    }
    /// The dialog under the top one (a message box's parent).
    pub fn under_top(&self) -> Option<&Dialog> {
        self.0.iter().rev().nth(1)
    }
    /// Close the top dialog: its owner has handled it.
    pub fn pop(&mut self) {
        self.0.pop();
    }
    /// Dismiss every dialog without applying any: the document under them
    /// changed some other way (an agent's edit, a reload).
    pub fn clear(&mut self) {
        self.0.clear();
    }

    fn top_mut(&mut self) -> Result<&mut Dialog, String> {
        self.0.last_mut().ok_or_else(|| NONE_OPEN.into())
    }

    /// The top dialog's id, or `none`: the `dialog` state key.
    pub fn top_id(&self) -> &'static str {
        self.top().map_or("none", |d| d.id)
    }

    /// `dialog-read`: the top dialog, or `{open: false}`.
    pub fn to_json(&self) -> Json {
        match self.top() {
            Some(d) => d.to_json(self.depth()),
            None => Json::obj(vec![("open", Json::Bool(false))]),
        }
    }

    pub fn set(&mut self, control: &str, args: &Json) -> Result<(), String> {
        self.top_mut()?.set(control, args)
    }

    pub fn select_tab(&mut self, tab: &str) -> Result<(), String> {
        self.top_mut()?.select_tab(tab)
    }

    /// The top dialog, for a widget's input (typing, a click on a control).
    pub fn top_dialog_mut(&mut self) -> Result<&mut Dialog, String> {
        self.top_mut()
    }

    /// Press a button on the top dialog. Cancel drops it; Open pushes its
    /// child; Accept and Apply hand it to `apply` (the owner), and Accept
    /// closes it once the owner took it. An owner that refuses leaves the
    /// dialog open with its staged values, and the refusal is the error.
    pub fn click(
        &mut self,
        button: &str,
        apply: impl FnOnce(&Dialog) -> Result<(), String>,
    ) -> Result<(), String> {
        let top = self.top_mut()?;
        let role = top.buttons[top.button_index(button)?].role;
        match role {
            ButtonRole::Cancel => {
                self.0.pop();
            }
            ButtonRole::Open(child) => {
                let child = child.build(top);
                self.0.push(child);
            }
            ButtonRole::Accept | ButtonRole::Apply => {
                apply(top)?;
                if role == ButtonRole::Accept {
                    self.0.pop();
                }
            }
        }
        Ok(())
    }

    /// The button a key presses on the top dialog: Enter the default, Escape
    /// the cancel button. `None` for every other key, which the dialog
    /// swallows: nothing under it may see a key while it is open.
    pub fn key_button(&self, key: &str, plain: bool) -> Option<String> {
        let top = self.top()?;
        if !plain {
            return None;
        }
        let b = match key {
            "enter" => top.buttons.iter().find(|b| b.default),
            "escape" => top.buttons.iter().find(|b| b.role == ButtonRole::Cancel),
            _ => None,
        }?;
        b.enabled.then(|| b.label.clone())
    }
}

pub(crate) const NONE_OPEN: &str = "no dialog is open";

#[cfg(test)]
mod tests_support {
    use super::*;

    /// A child that shows the parent's title, so a test can see it was built
    /// from the parent it was opened over.
    pub(super) fn child(parent: &Dialog) -> Dialog {
        Dialog {
            id: "child",
            title: format!("{} › Details", parent.title),
            text: None,
            tabs: Vec::new(),
            tab: 0,
            controls: vec![Control::new(
                "note",
                "Note:",
                ControlKind::Text,
                Value::Text(String::new()),
            )],
            buttons: vec![
                Button {
                    default: true,
                    ..Button::new("OK", ButtonRole::Accept)
                },
                Button::new("Cancel", ButtonRole::Cancel),
            ],
            owner: DialogOwner::Test,
            focus: None,
            opened: Vec::new(),
            react: None,
        }
    }
}

#[cfg(test)]
mod tests;
