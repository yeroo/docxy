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
use unicode_segmentation::UnicodeSegmentation;

pub(crate) mod catalog;
pub(crate) use catalog::DialogId;

/// What an accept button applies the dialog to. The app matches it in
/// `apply_dialog`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DialogOwner {
    /// Delete this summary task and its subtasks.
    DeleteSummary {
        uid: i32,
    },
    /// Project's Task Information, for one task (#1368).
    TaskInfo {
        uid: i32,
    },
    /// A dialog a UI plugin's command asked for (#1532): OK sends `command`
    /// again with the values (`plugin_ui::dialog_click`).
    Plugin {
        command: &'static str,
        /// The plugin's dialog spec id, asked again for the dialog's state
        /// (#1602).
        spec: &'static str,
    },
    /// Word's Page Setup, for the caret's sections (#649).
    PageSetup,
    /// Word's Columns, for the caret's sections (#649).
    Columns,
    /// Word's Line Numbers, for the caret's sections (#747).
    LineNumbers,
    /// Word's Hyphenation, on the document's settings (#1265).
    Hyphenation,
    /// Word's Manual Hyphenation, on the walk's current word (#1266).
    ManualHyphen,
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
    /// The Header & Footer tab's Date and Time and Alignment Tab dialogs
    /// (#768), for the edited story's caret, and Online Pictures, whose
    /// From a File... runs Pictures... (`Docxy::online_pictures_click`).
    HfDateTime,
    HfAlignmentTab,
    HfOnlinePictures,
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
    /// "Do you want to reopen it?" before an open discards the tab's unsaved
    /// changes (#610); Yes reloads the tab's file in `mode`.
    Reopen {
        mode: crate::open_mode::OpenMode,
    },
    /// The Mailings tab's dialogs (#628), applied by `mailings_dialogs`:
    /// Envelopes (Create) and Envelope Options (Start Mail Merge), and its
    /// confirm before it replaces the document with an envelope of `size`.
    MailEnvelopes,
    MailEnvelopeOptions,
    MailEnvelopesReplace(docxcore::merge::envelope::EnvelopeSize),
    /// Labels (Create), Label Options (Start Mail Merge), and its confirm.
    MailLabels,
    MailLabelOptions,
    MailLabelsReplace(docxcore::merge::labels::LabelSpec),
    MailRecipients,
    MailAddressBlock,
    MailGreetingLine,
    MailMatchFields,
    MailFind,
    MailCheckErrors,
    MailMergeToNew,
    /// "Opening this document will run the following SQL command": Yes reads
    /// the document's own data source, then runs `then`.
    MailAttach {
        then: crate::mailings_tab::MailAct,
    },
    /// A mail-merge report (Check for Errors' result): OK closes it.
    MailReport,
    /// The Design tab's dialogs (#651), applied by `design_dialogs`: Page
    /// Color's More Colors and Fill Effects, Custom Watermark, and Page
    /// Borders.
    DesignMoreColors,
    DesignFillEffects,
    DesignWatermark,
    DesignPageBorders,
    /// Page Borders' Options...: its OK writes back into Page Borders
    /// ([`ChildDialog::PageBorderOptions`]), never into the document.
    DesignBorderOptions,
    /// Settings' User name... (#620): the reviewer name and initials new
    /// comments carry. An app setting, applied by `user_name::click`.
    UserName,
    /// Document Recovery (#1225), after a crash: which recovered documents
    /// keep their AutoRecover copy. The app's (`doc_recovery::click`).
    DocumentRecovery,
    /// File > Account's About docxy suite (#1023): the build's rows, Copy and
    /// Close. Its presses are the app's (`Docxy::about_click`), before the tab's.
    About,
    /// Excel's Subtotal dialog (#693) over the region `(r1, c1, r2, c2)` of
    /// `sheet`: its column choices start at `c1` (at most 64 of them), and
    /// `c2` ends the region, so total rows are found only in `c1..=c2`;
    /// `header` when `r1` is a header row. OK and Remove All apply in
    /// `sheet_outline::click`.
    Subtotal {
        sheet: usize,
        r1: u32,
        r2: u32,
        c1: u32,
        c2: u32,
        header: bool,
    },
    /// Excel's Consolidate (#694), writing at `area` of `sheet`: the
    /// selection it opened on, which limits the output when it spans more
    /// than one cell (#988). Add, Delete and OK apply in
    /// `sheet_consolidate::click`.
    Consolidate {
        sheet: usize,
        area: (u32, u32, u32, u32),
    },
    /// Excel's Data Validation dialog (#689) over `range` of `sheet`. OK and
    /// Clear All apply in `sheet_validation::click`.
    DataValidation {
        sheet: usize,
        range: (u32, u32, u32, u32),
    },
    /// The alert a typed entry that breaks its cell's rule raises (#687); its
    /// presses are handled in `sheet_validation::alert_click`.
    DataValidationAlert,
    /// Excel's Format Cells (#1165) over the selection of `sheet`. OK
    /// applies in `sheet_format_cells::click`.
    FormatCells {
        sheet: usize,
    },
    /// A filter button's drop-down on absolute column `col` of `sheet`'s
    /// AutoFilter (#690); its buttons apply in `sheet_filter::click`.
    FilterMenu {
        sheet: usize,
        col: u32,
    },
    /// The Project table's AutoFilter drop-down on entry column `col`
    /// (#1533); its buttons apply in `project::autofilter_click`.
    ProjectFilter {
        col: usize,
    },
    /// Custom AutoFilter and Top 10 AutoFilter for that column.
    CustomFilter {
        sheet: usize,
        col: u32,
        /// The column holds dates: a typed `YYYY-MM-DD` compares as a date.
        date: bool,
    },
    Top10Filter {
        sheet: usize,
        col: u32,
    },
    /// Advanced Filter on `sheet`.
    AdvancedFilter {
        sheet: usize,
    },
    /// Excel's Sort dialog over `area` of `sheet` (#691), applied in
    /// `sheet_sort::click`.
    SortLevels {
        sheet: usize,
        area: (u32, u32, u32, u32),
    },
    /// The Sort Warning: the selection `sel` sits in the wider list
    /// `region` (`header` when its first row is one); `then` is the sort
    /// waiting on the answer.
    SortWarning {
        sheet: usize,
        sel: (u32, u32, u32, u32),
        region: (u32, u32, u32, u32),
        header: bool,
        then: crate::sheet_sort::SortThen,
    },
    /// The outline Settings (#693): where summary rows and columns sit.
    OutlineSettings,
    /// Page Layout's Page Setup dialog for the active sheet (#1019), applied
    /// in `sheet_page_setup::click`.
    SheetPageSetup,
    /// Formulas › Calculation › More Calculation Options... (#674) and Set
    /// precision as displayed's warning, applied in `sheet_calc::click`.
    SheetCalcOptions,
    SheetPrecisionWarning,
    /// Home › Find & Select › Go To… and Go To Special… (#671), applied in
    /// `sheet_goto::click`.
    GoTo,
    GoToSpecial,
    /// Formulas › Error Checking (#675), applied in `sheet_audit::click`: the
    /// walk began at `start` and shows the flagged cell `cur`.
    ErrorChecking {
        start: (u32, u32),
        cur: (u32, u32),
    },
    /// Error Checking's "The error check is complete" box.
    ErrorCheckDone,
    /// Formulas › Evaluate Formula (#675), applied in `sheet_audit::click`:
    /// the evaluation is the sheet view's.
    EvaluateFormula,
    /// Home › Fill › Series… (#668); its OK needs the app's custom lists,
    /// so `Docxy::fill_dialog_click` applies it.
    Series,
    /// Justify's "Text will extend below selected range." (#668).
    JustifyOverflow,
    /// File › Options › Edit Custom Lists (#668): an app setting, applied by
    /// `Docxy::fill_dialog_click`.
    CustomLists,
    /// Paste Special… (#669); `clip` when a copy is live (else only text
    /// is on the clipboard). Applied by `Docxy::paste_dialog_click`.
    PasteSpecial {
        clip: bool,
    },
    /// A document's Paste Special… (#753), whose "As" list holds what the
    /// clipboard held when it opened. Applied by
    /// `Docxy::doc_paste_dialog_click`, which reads the clipboard again.
    DocPasteSpecial,
    /// Edit Field (#1241), Shift+F9 on a field: its code. Applied by
    /// `doc_fields::click`, which updates the field's result too.
    EditField,
    /// "There's already data here. Do you want to replace it?" before a
    /// drop by the selection's border (#670), applied by
    /// `Docxy::drop_dialog_click`.
    DropReplace,
    /// Group's (Ungroup's) "Rows or Columns?" over a block selection (#693).
    OutlineAxis {
        ungroup: bool,
    },
    /// A message whose only button is OK, which just closes it (Flash
    /// Fill's "didn't see a pattern", #666).
    Message,
    /// The AutoCorrect dialog, its Exceptions and its "redefine it?"
    /// question (#667): app settings, pressed in `sheet_autocorrect::click`.
    AutoCorrect,
    AutoCorrectExceptions,
    AutoCorrectRedefine,
    /// Word's "Save your changes to this file?" before a dirty tab closes
    /// (#629), or for a workbook or a Project "Save changes to … before
    /// closing?"; `quit` when it is one of the window close's questions
    /// (#630). Every press is handled in `close::close_prompt_click`.
    SaveOnClose {
        quit: bool,
    },
    /// A dialog the model tests build; the app never applies one.
    #[cfg(test)]
    Test,
    /// A test child whose OK writes back into its parent.
    #[cfg(test)]
    TestChild,
}

/// A dialog a button opens on top of its own, built from the parent when the
/// button is pressed. An enum rather than a constructor function so buttons
/// stay comparable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildDialog {
    /// Page Borders' Border and Shading Options (#651).
    PageBorderOptions,
    #[cfg(test)]
    Test,
    #[cfg(test)]
    TestWriteBack,
}

impl ChildDialog {
    fn build(self, parent: &Dialog) -> Dialog {
        match self {
            Self::PageBorderOptions => crate::design_dialogs::border_options_dialog(parent),
            #[cfg(test)]
            Self::Test => tests_support::child(parent),
            #[cfg(test)]
            Self::TestWriteBack => tests_support::write_back_child(parent),
        }
    }

    /// The child an owner belongs to when its OK hands the child's values
    /// back to the dialog under it rather than to an owner: a sub-dialog of
    /// options the parent applies with its own OK.
    fn writing_back(owner: DialogOwner) -> Option<ChildDialog> {
        match owner {
            DialogOwner::DesignBorderOptions => Some(Self::PageBorderOptions),
            #[cfg(test)]
            DialogOwner::TestChild => Some(Self::TestWriteBack),
            _ => None,
        }
    }

    /// Copy an accepted child's values into its parent.
    fn write_back(self, child: &Dialog, parent: &mut Dialog) {
        match self {
            Self::PageBorderOptions => {
                crate::design_dialogs::border_options_write_back(child, parent)
            }
            #[cfg(test)]
            Self::Test => {}
            #[cfg(test)]
            Self::TestWriteBack => {
                if let (Some(v), Some(c)) = (
                    child.value("note").cloned(),
                    parent.controls.iter_mut().find(|c| c.name == "name"),
                ) {
                    c.value = v;
                }
            }
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
    /// Stable, for scripts and a plugin's own buttons (#1602): the folded
    /// label unless the builder gives one. `dialog-click` takes either.
    pub id: String,
    pub label: String,
    pub enabled: bool,
    /// Enter presses it.
    pub default: bool,
    pub role: ButtonRole,
}

impl Button {
    pub fn new(label: &str, role: ButtonRole) -> Self {
        Self {
            id: fold(label),
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
    /// A grid column drawn as Project's check mark (#1602): `true` or
    /// `false`, toggled by a click or Space, never typed into.
    Check,
    Radio,
    Dropdown,
    List,
    /// Rows of cells, one column kind each (`Control::column_kinds`, text
    /// when unset); `dialog-set` addresses a cell as `name[row].column`
    /// (#1532). Drawn as a table whose rows the pointer and Up/Down select
    /// (`Control::row`, `dialog-select`) and whose cells the person types
    /// into (`Control::edit`, committed by Enter, dropped by Escape; Space
    /// toggles a check cell) (#1602).
    Grid,
    Label,
    /// A text field of several lines: Enter is a new line, not OK (#1532).
    Multiline,
    /// Items each checked or not, a tree by `Control::depths`: checking an
    /// item checks the items under it, and an item with items under it is
    /// checked when they all are (the AutoFilter value list, #690).
    CheckList,
}

impl ControlKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Date => "date",
            Self::Duration => "duration",
            Self::Checkbox => "checkbox",
            Self::Check => "check",
            Self::Radio => "radio",
            Self::Dropdown => "dropdown",
            Self::List => "list",
            Self::Grid => "grid",
            Self::Label => "label",
            Self::Multiline => "multiline",
            Self::CheckList => "checklist",
        }
    }
    pub fn has_items(self) -> bool {
        matches!(self, Self::Radio | Self::Dropdown | Self::List)
    }
    /// A field typed into.
    pub fn is_text(self) -> bool {
        matches!(
            self,
            Self::Text | Self::Number | Self::Date | Self::Duration | Self::Multiline
        )
    }
    /// A control the keyboard and pointer can edit: a label is never
    /// edited. A list's item is chosen by a click or Up/Down/Home/End
    /// (#988); a grid's row and cell by a click, the arrows and typing
    /// (#1602).
    pub fn is_editable(self) -> bool {
        self.is_text()
            || matches!(
                self,
                Self::Checkbox
                    | Self::Check
                    | Self::Radio
                    | Self::Dropdown
                    | Self::List
                    | Self::CheckList
                    | Self::Grid
            )
    }

    /// A cell kind a click or Space toggles.
    pub fn toggles(self) -> bool {
        matches!(self, Self::Checkbox | Self::Check)
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
    /// A check list's checks, one per item.
    Checks(Vec<bool>),
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
    /// A grid's column kinds, one per column (text when shorter), checked
    /// when a cell is set (#1532).
    pub column_kinds: Vec<ControlKind>,
    /// A check list's items' depths in its tree (0 at the top).
    pub depths: Vec<u8>,
    /// Contributed by a UI plugin (#1532): `dialog-read` says `"plugin":
    /// true`, and the catalogue check leaves it out.
    pub plugin: bool,
    /// A grid's selected row (#1602), `None` for none.
    pub row: Option<usize>,
    /// A grid's active column, the cell Space and typing act on (#1602).
    pub col: usize,
    /// The text typed into the grid's active cell so far (#1602): Enter
    /// commits it through the input handler, Escape drops it.
    pub edit: Option<String>,
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
            column_kinds: Vec::new(),
            depths: Vec::new(),
            plugin: false,
            row: None,
            col: 0,
            edit: None,
        }
    }

    /// A grid's cell text, empty off the rows.
    fn cell(&self, row: usize, col: usize) -> String {
        match &self.value {
            Value::Rows(rows) => rows
                .get(row)
                .and_then(|r| r.get(col))
                .cloned()
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// A grid's row count.
    fn row_count(&self) -> usize {
        match &self.value {
            Value::Rows(rows) => rows.len(),
            _ => 0,
        }
    }

    /// The kind of a grid's column `col` (text when none is set).
    pub fn column_kind(&self, col: usize) -> ControlKind {
        self.column_kinds
            .get(col)
            .copied()
            .unwrap_or(ControlKind::Text)
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
            Value::Checks(c) => format!(
                "{} of {} checked",
                c.iter().filter(|x| **x).count(),
                c.len()
            ),
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
            Value::Checks(c) => Json::Arr(c.iter().map(|x| Json::Bool(*x)).collect()),
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
            out.push((
                "column_kinds",
                Json::Arr(
                    (0..self.columns.len())
                        .map(|c| Json::Str(self.column_kind(c).name().into()))
                        .collect(),
                ),
            ));
            out.push(("rows", rows_json(rows)));
            out.push((
                "selected_row",
                self.row.map_or(Json::Null, |r| Json::Num(r as f64)),
            ));
            out.push(("column", Json::Num(self.col as f64)));
            out.push(("editing", self.edit.clone().map_or(Json::Null, Json::Str)));
        }
        if self.plugin {
            out.push(("plugin", Json::Bool(true)));
        }
        if self.kind == ControlKind::CheckList {
            out.push(("items", strs(&self.items)));
            out.push((
                "depths",
                Json::Arr(
                    self.depths
                        .iter()
                        .map(|d| Json::Num(f64::from(*d)))
                        .collect(),
                ),
            ));
        }
        Json::obj(out)
    }

    /// The control's input handler. `dialog-set` calls it, and so do the
    /// overlay's editable widgets, through [`Dialog::click_control`] and
    /// typing. `args` is the verb's argument object: `value`,
    /// or for a grid `{row, column, value}`, `{insert_row}` or `{delete_row}`;
    /// a radio group, dropdown or list also takes `{index}`, which picks one
    /// of two items that read the same.
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
            ControlKind::Text
            | ControlKind::Date
            | ControlKind::Duration
            | ControlKind::Multiline => Value::Text(text()?),
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
            ControlKind::Checkbox | ControlKind::Check => match value()? {
                Json::Bool(b) => Value::Bool(*b),
                _ => return Err(format!("'{label}' takes true or false")),
            },
            ControlKind::Radio | ControlKind::Dropdown | ControlKind::List
                if args.get("index").is_some() =>
            {
                match args.get("index") {
                    Some(Json::Num(n)) if n.fract() == 0.0 && *n >= 0.0 => {
                        let i = *n as usize;
                        if i >= self.items.len() {
                            return Err(format!("'{label}' has no item {i}"));
                        }
                        Value::Choice(Some(i))
                    }
                    _ => return Err(format!("'{label}' takes an item index")),
                }
            }
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
            ControlKind::CheckList => Value::Checks(self.set_checks(args)?),
        };
        Ok(())
    }

    /// A check list's `dialog-set`: `{value: true|false}` checks or clears
    /// every item; `{value: [labels]}` checks exactly those; `{item, checked}`
    /// checks or clears one (and the items under it).
    fn set_checks(&self, args: &Json) -> Result<Vec<bool>, String> {
        let label = shown(&self.label);
        let mut checks = match &self.value {
            Value::Checks(c) => c.clone(),
            _ => vec![false; self.items.len()],
        };
        checks.resize(self.items.len(), false);
        let find = |want: &str| {
            self.items
                .iter()
                .position(|item| fold(item.trim()) == fold(want.trim()))
                .ok_or_else(|| format!("'{label}' has no item '{want}'"))
        };
        // One item, by label or (for a label the tree repeats, a day under
        // two months) by index.
        let one = match (args.get_str("item"), args.get("index")) {
            (Some(item), _) => Some(find(item)?),
            (None, Some(i)) => Some(
                i.as_usize()
                    .filter(|&k| k < self.items.len())
                    .ok_or_else(|| format!("'{label}' has no item {i:?}"))?,
            ),
            _ => None,
        };
        if let Some(k) = one {
            let on = args
                .get("checked")
                .and_then(Json::as_bool)
                .ok_or("'checked' must be true or false")?;
            gridcore::filter::check_tree(&mut checks, &self.depths, k, on);
            return Ok(checks);
        }
        match args.get("value").ok_or("missing argument 'value'")? {
            Json::Bool(b) => checks.iter_mut().for_each(|c| *c = *b),
            Json::Arr(want) => {
                checks.iter_mut().for_each(|c| *c = false);
                for w in want {
                    let w = w
                        .as_str()
                        .ok_or_else(|| format!("'{label}' takes labels"))?;
                    let k = find(w)?;
                    gridcore::filter::check_tree(&mut checks, &self.depths, k, true);
                }
            }
            _ => return Err(format!("'{label}' takes true, false or a list of labels")),
        }
        Ok(checks)
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
        // A cell takes what its column's kind takes (#1532): a number
        // column a finite number (as text or a number), a checkbox column
        // true or false, the rest text.
        let kind = self.column_kind(column);
        let heading = self.columns.get(column).map_or("", String::as_str);
        let value = match (kind, args.get("value")) {
            (ControlKind::Checkbox | ControlKind::Check, Some(Json::Bool(b))) => b.to_string(),
            (ControlKind::Checkbox | ControlKind::Check, Some(Json::Str(s)))
                if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false") =>
            {
                s.to_ascii_lowercase()
            }
            (ControlKind::Checkbox | ControlKind::Check, _) => {
                return Err(format!("'{label}' column '{heading}' takes true or false"));
            }
            (ControlKind::Number, Some(Json::Num(n))) if n.is_finite() => n.to_string(),
            (ControlKind::Number, Some(Json::Str(s)))
                if s.trim().parse::<f64>().is_ok_and(f64::is_finite) || partial_number(s) =>
            {
                s.clone()
            }
            (ControlKind::Number, _) => {
                return Err(format!("'{label}' column '{heading}' takes a number"));
            }
            (_, Some(Json::Str(s))) => s.clone(),
            (_, Some(Json::Num(n))) => n.to_string(),
            _ => return Err(format!("'{label}' cells take text")),
        };
        if rows[row].len() < self.columns.len() {
            rows[row].resize(self.columns.len(), String::new());
        }
        rows[row][column] = value;
        Ok(())
    }
}

/// `name[row].column` (#1532): a grid cell's address, as `dialog-set` takes
/// it; `column` is a heading or an index. `None` for a plain control name.
pub(crate) fn grid_address(control: &str) -> Option<(&str, usize, &str)> {
    let open = control.find('[')?;
    let close = control[open..].find(']')? + open;
    let name = &control[..open];
    let row = control[open + 1..close].trim().parse::<usize>().ok()?;
    let column = control[close + 1..].strip_prefix('.')?.trim();
    (!name.is_empty() && !column.is_empty()).then_some((name, row, column))
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

/// [`fold`], for the dialogs' owners outside this module.
pub(crate) fn fold_label(label: &str) -> String {
    fold(label)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Dialog {
    /// Stable, for scripts: `delete-summary`. Only [`catalog`] makes one, so
    /// every dialog carries a catalogued id (#1029).
    pub id: DialogId,
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
    /// The caret in the focused text field, as a character index; `None` is
    /// the end of the text. Reset whenever the focus moves or a value is set.
    caret: Option<usize>,
    /// The other end of the selection the caret extends from, when one is
    /// made (Shift+arrow, Ctrl+A); `None` is no selection.
    anchor: Option<usize>,
    /// The control `caret` and `anchor` belong to: they mean nothing once the
    /// focus is on another (however it got there).
    caret_for: Option<usize>,
    /// The values the dialog opened on, one per control (see
    /// [`Dialog::changed`]); empty for a dialog that does not track them.
    pub opened: Vec<Value>,
    /// The owner's reaction to a change of control `i` (whose value was
    /// `before`): Page Setup's paper size following its sides, say. Set by the
    /// dialog's builder.
    pub react: Option<Reaction>,
    /// The dialog stays open while the document and the selection change
    /// under it (#1602): no backdrop, the pointer reaches the document, and
    /// the keys reach the dialog only while it is `active` (the last click
    /// was in it). Only a UI plugin's dialog is one so far.
    pub modeless: bool,
    /// Whether a modeless dialog has the keyboard (#1602); a modal one
    /// always has.
    pub active: bool,
    /// A plugin control changed since the plugin's state was last asked
    /// (#1602, #1567): the host asks again before the next read or frame.
    pub stale: bool,
    /// The plan revision and selected task a modeless plugin dialog's values
    /// were last asked for (#1602).
    pub seen: Option<(u64, Option<i32>)>,
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

/// The grapheme-cluster boundary before `at` (a character index): the
/// largest boundary strictly less than `at`, or 0. A caret stranded inside
/// a cluster lands on the cluster's near side.
fn prev_stop(text: &str, at: usize) -> usize {
    let mut stop = 0;
    for g in text.graphemes(true) {
        let next = stop + g.chars().count();
        if next >= at {
            break;
        }
        stop = next;
    }
    stop
}

/// The grapheme-cluster boundary after `at` (a character index): the
/// smallest boundary strictly greater than `at`, or the character count.
fn next_stop(text: &str, at: usize) -> usize {
    let mut stop = 0;
    for g in text.graphemes(true) {
        stop += g.chars().count();
        if stop > at {
            return stop;
        }
    }
    stop
}

impl Dialog {
    /// A message box: a title, a message and buttons. The first button is the
    /// default, as in Office's message boxes.
    pub fn message(
        id: DialogId,
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
            caret: None,
            anchor: None,
            caret_for: None,
            opened: Vec::new(),
            react: None,
            modeless: false,
            active: true,
            stale: false,
            seen: None,
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

    /// The tab label control `c` sits on; `None` on every tab.
    fn page_label(&self, c: &Control) -> Option<&str> {
        c.page.and_then(|p| self.tabs.get(p)).map(String::as_str)
    }

    /// `field-read`: field `name` as it is edited and drawn (#1029): its
    /// text, whether it has the focus, and the caret and selection the
    /// overlay draws from (`caret_at`, `selection`), `null` while another
    /// control has the focus. On any tab, so a script can see a field it has
    /// not reached yet is there.
    pub fn field_json(&self, name: &str) -> Result<Json, String> {
        let i = self
            .controls
            .iter()
            .position(|c| c.name == name)
            .ok_or_else(|| {
                format!(
                    "no field '{name}' in the open dialog ({})",
                    self.id.as_str()
                )
            })?;
        let c = &self.controls[i];
        let focused = self.focus == Some(i) && self.focused().is_some();
        let text = focused && c.kind.is_text();
        let num = |n: usize| Json::Num(n as f64);
        Ok(Json::obj(vec![
            ("name", Json::Str(name.into())),
            ("kind", Json::Str(c.kind.name().into())),
            ("value", Json::Str(c.text())),
            ("focused", Json::Bool(focused)),
            (
                "caret",
                if text {
                    num(self.caret_at())
                } else {
                    Json::Null
                },
            ),
            (
                "selection",
                match self.selection().filter(|_| text) {
                    Some((a, b)) => Json::Arr(vec![num(a), num(b)]),
                    None => Json::Null,
                },
            ),
            (
                "tab",
                self.page_label(c)
                    .map_or(Json::Null, |t| Json::Str(t.into())),
            ),
            (
                "index",
                match c.value {
                    Value::Choice(Some(i)) => num(i),
                    _ => Json::Null,
                },
            ),
            ("items", num(c.items.len())),
            ("on_page", Json::Bool(self.on_page(c))),
            ("enabled", Json::Bool(c.enabled)),
            ("visible", Json::Bool(c.visible)),
        ]))
    }

    /// `dialog-catalog-check`: this dialog's editable controls on every tab,
    /// as (name, kind, tab), against its catalogue entry's fields, in order
    /// (#1029). The error lists each difference, so a field added to a
    /// dialog without its entry fails the case that opens it.
    pub fn catalog_check(&self) -> Result<Json, String> {
        let entry = self
            .id
            .entry()
            .ok_or_else(|| format!("'{}' is a test-only dialog", self.id.as_str()))?;
        // A UI plugin's controls (#1532) are not the catalogue's to list.
        let have: Vec<(&str, ControlKind, Option<&str>)> = self
            .controls
            .iter()
            .filter(|c| c.kind.is_editable() && !c.plugin)
            .map(|c| (c.name, c.kind, self.page_label(c)))
            .collect();
        let want: Vec<(&str, ControlKind, Option<&str>)> = entry
            .fields
            .iter()
            .map(|f| (f.name, f.kind, f.tab))
            .collect();
        if have == want {
            return Ok(Json::obj(vec![
                ("id", Json::Str(self.id.as_str().into())),
                ("key", Json::Str(self.id.key().into())),
                ("fields", Json::Num(have.len() as f64)),
            ]));
        }
        let show = |(name, kind, tab): &(&str, ControlKind, Option<&str>)| match tab {
            Some(t) => format!("{name} ({}, {t})", kind.name()),
            None => format!("{name} ({})", kind.name()),
        };
        let list = |v: &[(&str, ControlKind, Option<&str>)]| {
            v.iter().map(show).collect::<Vec<_>>().join(", ")
        };
        Err(format!(
            "dialog '{}' ({}) does not match its catalogue entry: it has [{}], the catalogue lists [{}]; update suite/docxy/src/dialog/catalog/entries.rs",
            self.id.as_str(),
            self.id.key(),
            list(&have),
            list(&want)
        ))
    }

    /// Set one control through its input handler. A grid cell may be named
    /// `name[row].column` (#1532), the same as `{control: name, row, column}`.
    pub fn set(&mut self, control: &str, args: &Json) -> Result<(), String> {
        if let Some((name, row, column)) = grid_address(control) {
            let i = self.control_index(name)?;
            if self.controls[i].kind != ControlKind::Grid {
                return Err(format!(
                    "'{}' is not a grid; only a grid cell is named name[row].column",
                    shown(&self.controls[i].label)
                ));
            }
            let mut fields = vec![
                ("row".to_string(), Json::Num(row as f64)),
                (
                    "column".to_string(),
                    column
                        .parse::<f64>()
                        .map_or_else(|_| Json::Str(column.to_string()), Json::Num),
                ),
            ];
            if let Json::Obj(given) = args {
                fields.extend(given.iter().filter(|(k, _)| k == "value").cloned());
            }
            return self.set_at(i, &Json::Obj(fields));
        }
        let i = self.control_index(control)?;
        self.set_at(i, args)
    }

    /// Every change to a control's value comes here: its input handler, then
    /// the owner's reaction (Page Setup's paper size follows its width).
    fn set_at(&mut self, i: usize, args: &Json) -> Result<(), String> {
        let before = self.controls[i].value.clone();
        self.controls[i].set(args)?;
        self.caret = None;
        self.anchor = None;
        if self.controls[i].plugin {
            // The plugin's state is asked again before the next read (#1602).
            self.stale = true;
        }
        if let Some(Reaction(react)) = self.react {
            react(self, i, &before);
        }
        Ok(())
    }

    /// Select row `row` of grid `name` (#1602), the focus moving to it; an
    /// edit of a cell in progress is dropped.
    pub fn select_row(&mut self, name: &str, row: usize) -> Result<(), String> {
        let i = self.control_index(name)?;
        let c = &self.controls[i];
        if c.kind != ControlKind::Grid {
            return Err(format!("'{}' is not a grid", shown(&c.label)));
        }
        if !self.focusable().contains(&i) {
            return Err(format!("'{}' cannot be edited", shown(&c.label)));
        }
        let rows = c.row_count();
        if row >= rows {
            return Err(no_row(&shown(&c.label), row, rows));
        }
        self.focus = Some(i);
        let c = &mut self.controls[i];
        c.row = Some(row);
        c.edit = None;
        Ok(())
    }

    /// Toggle the selected row's cell in the active column of grid `i`, a
    /// check or checkbox column; nothing otherwise.
    fn grid_toggle(&mut self, i: usize) -> Result<(), String> {
        let c = &self.controls[i];
        let Some(row) = c.row else {
            return Ok(());
        };
        let col = c.col;
        if !c.column_kind(col).toggles() {
            return Ok(());
        }
        let on = c.cell(row, col) != "true";
        self.controls[i].edit = None;
        self.set_at(
            i,
            &Json::obj(vec![
                ("row", Json::Num(row as f64)),
                ("column", Json::Num(col as f64)),
                ("value", Json::Bool(on)),
            ]),
        )
    }

    /// Commit `text` into the selected row's active cell of grid `i` through
    /// the input handler (the column's kind checks it); the edit in progress
    /// ends when it takes.
    fn grid_commit(&mut self, i: usize, text: &str) -> Result<(), String> {
        let c = &self.controls[i];
        let Some(row) = c.row else {
            return Ok(());
        };
        let col = c.col;
        self.set_at(
            i,
            &Json::obj(vec![
                ("row", Json::Num(row as f64)),
                ("column", Json::Num(col as f64)),
                ("value", Json::Str(text.to_string())),
            ]),
        )?;
        self.controls[i].edit = None;
        Ok(())
    }

    /// A key on a focused grid (#1602): Up and Down select a row, Left and
    /// Right the active column, Space toggles a check cell, typing starts
    /// (and goes on) editing the cell, Enter commits the edit, Escape drops
    /// it, Backspace shortens the cell's text, Delete clears the cell.
    /// `Ok(false)` when the focus is not on a grid or the key is not a
    /// grid's: Enter and Escape with no edit open press the dialog's
    /// buttons, Tab moves the focus.
    pub fn grid_key(&mut self, key: &str, typed: Option<&str>) -> Result<bool, String> {
        let Some(i) = self.focus.filter(|i| self.focusable().contains(i)) else {
            return Ok(false);
        };
        if self.controls[i].kind != ControlKind::Grid {
            return Ok(false);
        }
        let c = &self.controls[i];
        let (rows, cols) = (c.row_count(), c.columns.len());
        let (row, col, editing) = (c.row, c.col, c.edit.clone());
        let toggles = c.column_kind(col).toggles();
        match key {
            "up" | "down" => {
                if rows > 0 {
                    let next = match (row, key) {
                        (None, _) => 0,
                        (Some(r), "down") => (r + 1).min(rows - 1),
                        (Some(r), _) => r.saturating_sub(1),
                    };
                    let c = &mut self.controls[i];
                    c.edit = None;
                    c.row = Some(next);
                }
                Ok(true)
            }
            "left" | "right" => {
                if editing.is_none() && cols > 0 {
                    self.controls[i].col = if key == "right" {
                        (col + 1).min(cols - 1)
                    } else {
                        col.saturating_sub(1)
                    };
                }
                Ok(true)
            }
            "space" if toggles => {
                self.grid_toggle(i)?;
                Ok(true)
            }
            "enter" => match editing {
                Some(buf) => {
                    self.grid_commit(i, &buf)?;
                    Ok(true)
                }
                None => Ok(false),
            },
            "escape" => match editing {
                Some(_) => {
                    self.controls[i].edit = None;
                    Ok(true)
                }
                None => Ok(false),
            },
            "backspace" => {
                if let Some(r) = row
                    && !toggles
                {
                    let mut buf = editing.unwrap_or_else(|| self.controls[i].cell(r, col));
                    buf.pop();
                    self.controls[i].edit = Some(buf);
                }
                Ok(true)
            }
            "delete" => {
                if editing.is_none() && row.is_some() && !toggles {
                    self.grid_commit(i, "")?;
                }
                Ok(true)
            }
            "tab" | "home" | "end" => Ok(false),
            _ => {
                let Some(t) = typed.filter(|t| t.chars().all(|c| !c.is_control())) else {
                    return Ok(false);
                };
                if row.is_some() && !toggles {
                    // Typing replaces the cell's text, as in a sheet.
                    let mut buf = editing.unwrap_or_default();
                    buf.push_str(t);
                    self.controls[i].edit = Some(buf);
                }
                Ok(true)
            }
        }
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

    /// How many Tab presses move the focus from where it is now to control
    /// `name` (#1029), as [`Dialog::focus_step`] walks: an error when Tab
    /// never reaches it (another tab's, hidden or disabled).
    pub fn tabs_to(&self, name: &str) -> Result<usize, String> {
        let order = self.focusable();
        let target = order
            .iter()
            .position(|&i| self.controls[i].name == name)
            .ok_or_else(|| format!("Tab never reaches '{name}' on this tab of the dialog"))?;
        let n = order.len();
        let presses = match self.focused().and(self.focus) {
            Some(f) => {
                // Already there: a whole round, so Tab still has to work.
                let at = order.iter().position(|&i| i == f).unwrap_or_default();
                match (target + n - at) % n {
                    0 => n,
                    k => k,
                }
            }
            None => target + 1,
        };
        Ok(presses)
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
        self.caret = None;
        self.anchor = None;
    }

    /// The focused text field's text, when the focus is on one.
    fn focused_text(&self) -> Option<String> {
        self.focused()
            .filter(|c| c.kind.is_text())
            .map(Control::text)
    }

    /// The caret, when it belongs to the focused control.
    fn live_caret(&self) -> Option<usize> {
        self.caret.filter(|_| self.caret_for == self.focus)
    }

    /// Put the caret at character `at` of the focused control, with no selection.
    fn put_caret(&mut self, at: usize) {
        self.caret = Some(at);
        self.anchor = None;
        self.caret_for = self.focus;
    }

    /// The caret in the focused field: a character index, at most the text's
    /// length (the text may have been set shorter since).
    pub fn caret_at(&self) -> usize {
        let len = self.focused_text().map_or(0, |t| t.chars().count());
        self.live_caret().unwrap_or(len).min(len)
    }

    /// The selected character range `(from, to)` in the focused field.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let len = self.focused_text()?.chars().count();
        let anchor = self.live_caret().and(self.anchor)?;
        let (a, c) = (anchor.min(len), self.caret_at());
        (a != c).then(|| (a.min(c), a.max(c)))
    }

    /// Replace `from..to` (characters) of the focused field with `with`,
    /// through its input handler, and put the caret after it. A refusal (a
    /// letter in a number) is the error and leaves the text and caret as they were.
    fn replace_range(&mut self, from: usize, to: usize, with: &str) -> Result<(), String> {
        let Some(text) = self.focused_text() else {
            return Ok(());
        };
        let chars: Vec<char> = text.chars().collect();
        let (from, to) = (from.min(chars.len()), to.min(chars.len()));
        let mut next: String = chars[..from].iter().collect();
        next.push_str(with);
        next.extend(&chars[to..]);
        let i = self.focus.unwrap_or_default();
        self.set_at(i, &Json::obj(vec![("value", Json::Str(next))]))?;
        self.put_caret(from + with.chars().count());
        Ok(())
    }

    /// The range an edit replaces: the selection, else `fallback` around the
    /// caret.
    fn edit_range(&self, fallback: impl FnOnce(usize) -> (usize, usize)) -> (usize, usize) {
        self.selection()
            .unwrap_or_else(|| fallback(self.caret_at()))
    }

    /// Text typed or pasted at the caret, replacing the selection. A
    /// character the field refuses (a letter in a number) is the error, and
    /// changes nothing.
    pub fn insert_text(&mut self, text: &str) -> Result<(), String> {
        let (from, to) = self.edit_range(|c| (c, c));
        self.replace_range(from, to, text)
    }

    /// A typed character at the caret.
    pub fn type_char(&mut self, ch: char) -> Result<(), String> {
        self.insert_text(ch.encode_utf8(&mut [0; 4]))
    }

    /// Backspace in the focused field: the selection, else the character
    /// (grapheme cluster) before the caret.
    pub fn backspace(&mut self) -> Result<(), String> {
        let text = self.focused_text().unwrap_or_default();
        let (from, to) = self.edit_range(|c| (prev_stop(&text, c), c));
        self.replace_range(from, to, "")
    }

    /// Delete in the focused field: the selection, else the character
    /// (grapheme cluster) after the caret.
    pub fn delete(&mut self) -> Result<(), String> {
        let text = self.focused_text().unwrap_or_default();
        let (from, to) = self.edit_range(|c| (c, next_stop(&text, c)));
        self.replace_range(from, to, "")
    }

    /// Move the caret in the focused field to character `to`; `extend` keeps
    /// (or starts) a selection from where it was, else the selection is dropped.
    pub fn move_caret_to(&mut self, to: usize, extend: bool) {
        let Some(len) = self.focused_text().map(|t| t.chars().count()) else {
            return;
        };
        let from = self.caret_at();
        let anchor = self.live_caret().and(self.anchor).or(Some(from));
        self.put_caret(to.min(len));
        self.anchor = anchor.filter(|_| extend);
    }

    /// Left or Right: one character (grapheme cluster), or to the selection's
    /// edge when one is made and `extend` is off.
    pub fn move_caret(&mut self, right: bool, extend: bool) {
        let at = self.caret_at();
        let text = self.focused_text().unwrap_or_default();
        if !extend {
            if let Some((from, to)) = self.selection() {
                return self.move_caret_to(if right { to } else { from }, false);
            }
        }
        let to = if right {
            next_stop(&text, at)
        } else {
            prev_stop(&text, at)
        };
        self.move_caret_to(to, extend);
    }

    /// Home or End.
    pub fn move_caret_edge(&mut self, end: bool, extend: bool) {
        let len = self.focused_text().map_or(0, |t| t.chars().count());
        self.move_caret_to(if end { len } else { 0 }, extend);
    }

    /// The selected text of the focused field: what Ctrl+C copies (#1029).
    pub fn selected_text(&self) -> Option<String> {
        let (from, to) = self.selection()?;
        Some(
            self.focused_text()?
                .chars()
                .skip(from)
                .take(to - from)
                .collect(),
        )
    }

    /// Ctrl+A: the whole text selected, the caret at its end.
    pub fn select_all(&mut self) {
        let Some(len) = self.focused_text().map(|t| t.chars().count()) else {
            return;
        };
        self.put_caret(len);
        self.anchor = Some(0);
    }

    /// A press on a control, as the pointer makes it: a field takes the
    /// focus, a checkbox toggles, a radio or list picks `item`, a dropdown
    /// steps to its next item (or picks `item`). Each change goes through
    /// `Control::set`.
    pub fn click_control(&mut self, index: usize, item: Option<usize>) -> Result<(), String> {
        let c = self.controls.get(index).ok_or("no such control")?;
        if !self.focusable().contains(&index) {
            return Err(format!("'{}' cannot be edited", shown(&c.label)));
        }
        if self.focus != Some(index) {
            self.caret = None;
            self.anchor = None;
        }
        self.focus = Some(index);
        let value = match (c.kind, &c.value) {
            // A grid cell (#1602): `item` is `row * 256 + column`; the row
            // and column are selected, and a check cell toggles.
            (ControlKind::Grid, Value::Rows(rows)) => {
                let Some(code) = item else {
                    return Ok(());
                };
                let (row, col) = (code / 256, code % 256);
                if row >= rows.len() || col >= c.columns.len().max(1) {
                    return Err("no such cell".into());
                }
                let c = &mut self.controls[index];
                if c.row != Some(row) || c.col != col {
                    c.edit = None;
                }
                c.row = Some(row);
                c.col = col;
                if c.column_kind(col).toggles() {
                    return self.grid_toggle(index);
                }
                return Ok(());
            }
            (ControlKind::Checkbox | ControlKind::Check, Value::Bool(b)) => Json::Bool(!b),
            (ControlKind::CheckList, Value::Checks(c)) => {
                let Some(k) = item else {
                    return Ok(());
                };
                let on = !c.get(k).copied().ok_or("no such item")?;
                return self.set_at(
                    index,
                    &Json::obj(vec![
                        ("index", Json::Num(k as f64)),
                        ("checked", Json::Bool(on)),
                    ]),
                );
            }
            (
                ControlKind::Radio | ControlKind::Dropdown | ControlKind::List,
                Value::Choice(cur),
            ) => {
                let n = c.items.len().max(1);
                let pick = match (item, c.kind) {
                    (Some(i), _) => i,
                    (None, ControlKind::Dropdown) => cur.map_or(0, |i| (i + 1) % n),
                    (None, _) => return Ok(()),
                };
                if pick >= c.items.len() {
                    return Err("no such item".into());
                }
                return self.set_at(index, &Json::obj(vec![("index", Json::Num(pick as f64))]));
            }
            _ => return Ok(()),
        };
        self.set_at(index, &Json::obj(vec![("value", value)]))
    }

    /// Up or Down on the focused radio group, dropdown or list: the item
    /// before or after the chosen one.
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

    /// Home or End on the focused list: its first or last item. `false`
    /// (nothing done) when the focus is not on a list, whose Home and End
    /// move a field's caret instead.
    pub fn list_edge(&mut self, end: bool) -> Result<bool, String> {
        let Some(c) = self.focused().filter(|c| c.kind == ControlKind::List) else {
            return Ok(false);
        };
        let Some(last) = c.items.len().checked_sub(1) else {
            return Ok(true);
        };
        let i = self.focus.unwrap_or_default();
        self.click_control(i, Some(if end { last } else { 0 }))?;
        Ok(true)
    }

    /// Space: toggles a focused checkbox; a field takes it as a character.
    pub fn space(&mut self) -> Result<(), String> {
        match self.focused().map(|c| c.kind) {
            Some(ControlKind::Checkbox | ControlKind::Check) => {
                let i = self.focus.unwrap_or_default();
                self.click_control(i, None)
            }
            Some(ControlKind::Grid) => {
                self.grid_key("space", Some(" "))?;
                Ok(())
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
            .position(|b| fold(&b.label) == fold(want) || b.id.eq_ignore_ascii_case(want.trim()))
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
            ("id", Json::Str(self.id.as_str().into())),
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
                                ("id", Json::Str(b.id.clone())),
                                ("label", Json::Str(shown(&b.label))),
                                ("enabled", Json::Bool(b.enabled)),
                                ("default", Json::Bool(b.default)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("modeless", Json::Bool(self.modeless)),
            ("active", Json::Bool(self.active)),
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
    /// changed some other way (a reload, a save).
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Dismiss the modal dialogs without applying any: the document under
    /// them changed some other way (an agent's edit). A modeless dialog
    /// stays, marked stale so its plugin's state is asked again (#1602).
    pub fn clear_modal(&mut self) {
        self.0.retain(|d| d.modeless);
        for d in &mut self.0 {
            d.stale = true;
        }
    }

    /// Whether a dialog that blocks the window is on top: a modeless one
    /// lets the pointer and the automation through (#1602).
    pub fn modal_open(&self) -> bool {
        self.top().is_some_and(|d| !d.modeless)
    }

    /// Whether the top dialog takes the keys: a modal one always, a
    /// modeless one while it is active (#1602).
    pub fn top_takes_keys(&self) -> bool {
        self.top().is_some_and(|d| !d.modeless || d.active)
    }

    /// Give the top dialog the keyboard, or take it away (a click in the
    /// document): a modeless dialog's `active` (#1602).
    pub fn activate(&mut self, on: bool) {
        if let Some(d) = self.0.last_mut() {
            d.active = on;
        }
    }

    /// `dialog-select`: row `row` of grid `control` on the top dialog.
    pub fn select_row(&mut self, control: &str, row: usize) -> Result<(), String> {
        self.top_mut()?.select_row(control, row)
    }

    fn top_mut(&mut self) -> Result<&mut Dialog, String> {
        self.0.last_mut().ok_or_else(|| NONE_OPEN.into())
    }

    /// The top dialog's id, or `none`: the `dialog` state key.
    pub fn top_id(&self) -> &'static str {
        self.top().map_or("none", |d| d.id.as_str())
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
    /// dialog open with its staged values, and the refusal is the error. The
    /// exception is a write-back child ([`ChildDialog::writing_back`]): its
    /// Accept never reaches `apply`; it copies its values into the dialog
    /// under it, which stays open, and closes.
    pub fn click(
        &mut self,
        button: &str,
        apply: impl FnOnce(&Dialog) -> Result<(), String>,
    ) -> Result<(), String> {
        let depth = self.0.len();
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
            ButtonRole::Accept if depth > 1 && ChildDialog::writing_back(top.owner).is_some() => {
                let child = self.0.pop().expect("the top dialog");
                let parent = self.0.last_mut().expect("a parent under it");
                if let Some(kind) = ChildDialog::writing_back(child.owner) {
                    kind.write_back(&child, parent);
                }
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
        // Enter in a multiline field is a new line (#1532), not OK.
        if key == "enter"
            && top
                .focused()
                .is_some_and(|c| c.kind == ControlKind::Multiline)
        {
            return None;
        }
        // A grid cell being typed into takes Enter and Escape (#1602).
        if matches!(key, "enter" | "escape")
            && top
                .focused()
                .is_some_and(|c| c.kind == ControlKind::Grid && c.edit.is_some())
        {
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

    /// A child whose OK writes its note into the parent's Name field.
    pub(super) fn write_back_child(parent: &Dialog) -> Dialog {
        Dialog {
            owner: DialogOwner::TestChild,
            ..child(parent)
        }
    }

    /// A child that shows the parent's title, so a test can see it was built
    /// from the parent it was opened over.
    pub(super) fn child(parent: &Dialog) -> Dialog {
        Dialog {
            id: catalog::TEST_CHILD,
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
            caret: None,
            anchor: None,
            caret_for: None,
            opened: Vec::new(),
            react: None,
            modeless: false,
            active: true,
            stale: false,
            seen: None,
        }
    }
}

#[cfg(test)]
mod tests;
