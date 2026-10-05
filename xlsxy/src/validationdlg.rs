//! Data ▸ Data Validation's dialog: the Settings, Input Message and Error
//! Alert tabs, with "Apply these changes to all other cells with the same
//! settings" and Clear All.
//!
//! Tab (Shift+Tab) / ↓ (↑) move between fields, ←/→ change a choice or the
//! tab, Space toggles a check box, and typing edits a text field. Enter is OK
//! (or the focused button), Esc is Cancel. The dialog only stages the rule;
//! the app applies it ([`gridcore::validation::set_validation`]).

use crate::outlinedlg::{heading, hint, item, modal};
use gridcore::sheet::{AlertStyle, DataValidation};
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;

/// Excel's limits on an input or error title and message.
pub const TITLE_MAX: usize = 32;
pub const MESSAGE_MAX: usize = 255;

/// "Allow:" choices, in Excel's order: the rule's `type` ("" is Any value).
const KINDS: [(&str, &str); 8] = [
    ("", "Any value"),
    ("whole", "Whole number"),
    ("decimal", "Decimal"),
    ("list", "List"),
    ("date", "Date"),
    ("time", "Time"),
    ("textLength", "Text length"),
    ("custom", "Custom"),
];

/// "Data:" choices, in Excel's order.
const OPERATORS: [(&str, &str); 8] = [
    ("between", "between"),
    ("notBetween", "not between"),
    ("equal", "equal to"),
    ("notEqual", "not equal to"),
    ("greaterThan", "greater than"),
    ("lessThan", "less than"),
    ("greaterThanOrEqual", "greater than or equal to"),
    ("lessThanOrEqual", "less than or equal to"),
];

const STYLES: [(AlertStyle, &str); 3] = [
    (AlertStyle::Stop, "Stop"),
    (AlertStyle::Warning, "Warning"),
    (AlertStyle::Information, "Information"),
];

const TABS: [&str; 3] = ["Settings", "Input Message", "Error Alert"];

/// What a key did.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Pending,
    Cancel,
    /// OK: the rule to apply (staged by [`ValidationDialog::rule`]).
    Ok,
    ClearAll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Tab,
    Allow,
    Operator,
    First,
    Second,
    IgnoreBlank,
    Dropdown,
    ApplyAll,
    ShowInput,
    PromptTitle,
    Prompt,
    ShowError,
    Style,
    ErrorTitle,
    Error,
    Ok,
    ClearAll,
    Cancel,
}

/// The dialog over `range` of sheet `sheet`.
#[derive(Clone, Debug)]
pub struct ValidationDialog {
    pub sheet: usize,
    pub range: (u32, u32, u32, u32),
    tab: usize,
    kind: usize,
    op: usize,
    first: String,
    second: String,
    ignore_blank: bool,
    dropdown: bool,
    pub apply_all: bool,
    show_input: bool,
    prompt_title: String,
    prompt: String,
    show_error: bool,
    style: usize,
    error_title: String,
    error: String,
    focus: usize,
}

/// A rule's formula as the box shows it: an inline list as its items, a
/// reference or any formula behind `=`.
fn shown(dv: &DataValidation, f: &str) -> String {
    if dv.kind == "list" {
        if let Some(items) = dv.list_values() {
            return items.join(",");
        }
        return format!("={f}");
    }
    f.to_string()
}

impl ValidationDialog {
    /// The dialog showing `current`, the rule on the selection's first cell
    /// (or Excel's defaults when it has none).
    pub fn new(
        sheet: usize,
        range: (u32, u32, u32, u32),
        current: Option<&DataValidation>,
    ) -> ValidationDialog {
        let blank = DataValidation {
            allow_blank: true,
            show_input: true,
            show_error: true,
            ..DataValidation::default()
        };
        let dv = current.unwrap_or(&blank);
        ValidationDialog {
            sheet,
            range,
            tab: 0,
            kind: KINDS.iter().position(|k| k.0 == dv.kind).unwrap_or(0),
            op: OPERATORS
                .iter()
                .position(|o| o.0 == dv.operator)
                .unwrap_or(0),
            first: shown(dv, &dv.formula1),
            second: dv.formula2.clone(),
            ignore_blank: dv.allow_blank,
            dropdown: dv.show_dropdown,
            apply_all: false,
            show_input: dv.show_input,
            prompt_title: dv.prompt_title.clone(),
            prompt: dv.prompt.clone().unwrap_or_default(),
            show_error: dv.show_error,
            style: STYLES
                .iter()
                .position(|s| s.0 == dv.error_style)
                .unwrap_or(0),
            error_title: dv.error_title.clone(),
            error: dv.error.clone(),
            focus: 0,
        }
    }

    fn kind_name(&self) -> &'static str {
        KINDS[self.kind].0
    }

    fn takes_operator(&self) -> bool {
        matches!(
            self.kind_name(),
            "whole" | "decimal" | "date" | "time" | "textLength"
        )
    }

    fn takes_two(&self) -> bool {
        self.takes_operator() && matches!(OPERATORS[self.op].0, "between" | "notBetween")
    }

    fn fields(&self) -> Vec<Field> {
        let mut f = vec![Field::Tab];
        match self.tab {
            0 => {
                f.push(Field::Allow);
                if self.kind_name() != "" {
                    if self.takes_operator() {
                        f.push(Field::Operator);
                    }
                    f.push(Field::First);
                    if self.takes_two() {
                        f.push(Field::Second);
                    }
                    f.push(Field::IgnoreBlank);
                    if self.kind_name() == "list" {
                        f.push(Field::Dropdown);
                    }
                }
                f.push(Field::ApplyAll);
            }
            1 => f.extend([Field::ShowInput, Field::PromptTitle, Field::Prompt]),
            _ => f.extend([
                Field::ShowError,
                Field::Style,
                Field::ErrorTitle,
                Field::Error,
            ]),
        }
        f.extend([Field::Ok, Field::ClearAll, Field::Cancel]);
        f
    }

    fn focused(&self) -> Field {
        let fields = self.fields();
        fields[self.focus.min(fields.len() - 1)]
    }

    /// The text box the focus is in, with its length limit.
    fn text_box(&mut self) -> Option<(&mut String, usize)> {
        let field = self.focused();
        match field {
            Field::First => Some((&mut self.first, MESSAGE_MAX)),
            Field::Second => Some((&mut self.second, MESSAGE_MAX)),
            Field::PromptTitle => Some((&mut self.prompt_title, TITLE_MAX)),
            Field::Prompt => Some((&mut self.prompt, MESSAGE_MAX)),
            Field::ErrorTitle => Some((&mut self.error_title, TITLE_MAX)),
            Field::Error => Some((&mut self.error, MESSAGE_MAX)),
            _ => None,
        }
    }

    pub fn key(&mut self, code: KeyCode) -> Outcome {
        let n = self.fields().len();
        let field = self.focused();
        match code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Up | KeyCode::BackTab => self.focus = (self.focus + n - 1) % n,
            KeyCode::Down | KeyCode::Tab => self.focus = (self.focus + 1) % n,
            KeyCode::Left | KeyCode::Right => {
                let step: i64 = if code == KeyCode::Left { -1 } else { 1 };
                let cyc = |i: usize, len: usize| (i as i64 + step).rem_euclid(len as i64) as usize;
                match field {
                    Field::Tab => self.tab = cyc(self.tab, TABS.len()),
                    Field::Allow => self.kind = cyc(self.kind, KINDS.len()),
                    Field::Operator => self.op = cyc(self.op, OPERATORS.len()),
                    Field::Style => self.style = cyc(self.style, STYLES.len()),
                    _ => {}
                }
            }
            KeyCode::Enter => {
                return match field {
                    Field::ClearAll => Outcome::ClearAll,
                    Field::Cancel => Outcome::Cancel,
                    _ => Outcome::Ok,
                };
            }
            KeyCode::Backspace => {
                if let Some((text, _)) = self.text_box() {
                    text.pop();
                }
            }
            KeyCode::Char(ch) => {
                if let Some((text, max)) = self.text_box() {
                    if text.chars().count() < max {
                        text.push(ch);
                    }
                } else if ch == ' ' {
                    match field {
                        Field::IgnoreBlank => self.ignore_blank = !self.ignore_blank,
                        Field::Dropdown => self.dropdown = !self.dropdown,
                        Field::ApplyAll => self.apply_all = !self.apply_all,
                        Field::ShowInput => self.show_input = !self.show_input,
                        Field::ShowError => self.show_error = !self.show_error,
                        Field::Ok => return Outcome::Ok,
                        Field::ClearAll => return Outcome::ClearAll,
                        Field::Cancel => return Outcome::Cancel,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        // The field list changes with the type and operator: keep the focus
        // on a field that exists.
        let n = self.fields().len();
        self.focus = self.focus.min(n - 1);
        Outcome::Pending
    }

    /// The rule OK applies, its ranges left empty; the reason it can't be
    /// applied when a bound is missing.
    pub fn rule(&self) -> Result<DataValidation, String> {
        let kind = self.kind_name();
        let mut dv = DataValidation {
            kind: kind.to_string(),
            allow_blank: self.ignore_blank,
            show_dropdown: self.dropdown,
            show_input: self.show_input,
            prompt_title: self.prompt_title.clone(),
            prompt: (!self.prompt.is_empty()).then(|| self.prompt.clone()),
            show_error: self.show_error,
            error_style: STYLES[self.style].0,
            error_title: self.error_title.clone(),
            error: self.error.clone(),
            ..DataValidation::default()
        };
        if kind.is_empty() {
            // Any value: only the messages remain.
            dv.allow_blank = true;
            return Ok(dv);
        }
        let first = self.first.trim();
        if first.is_empty() {
            return Err(match kind {
                "list" => "Data validation: enter the list's source".to_string(),
                "custom" => "Data validation: enter a formula".to_string(),
                _ if self.takes_two() => "Data validation: enter a minimum".to_string(),
                _ => "Data validation: enter a value".to_string(),
            });
        }
        if self.takes_operator() {
            dv.operator = OPERATORS[self.op].0.to_string();
        }
        match kind {
            "list" => {
                dv.formula1 = match first.strip_prefix('=') {
                    Some(f) => f.trim().to_string(),
                    None => {
                        let items: Vec<&str> = first
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .collect();
                        if items.is_empty() {
                            return Err("Data validation: enter the list's source".to_string());
                        }
                        format!("\"{}\"", items.join(","))
                    }
                };
            }
            _ => dv.formula1 = first.strip_prefix('=').unwrap_or(first).trim().to_string(),
        }
        if self.takes_two() {
            let second = self.second.trim();
            if second.is_empty() {
                return Err("Data validation: enter a maximum".to_string());
            }
            dv.formula2 = second
                .strip_prefix('=')
                .unwrap_or(second)
                .trim()
                .to_string();
        }
        Ok(dv)
    }

    fn line(&self, field: Field) -> String {
        let check = |on: bool| if on { "[x]" } else { "[ ]" };
        let first_label = match self.kind_name() {
            "list" => "Source:",
            "custom" => "Formula:",
            _ if self.takes_two() => "Minimum:",
            "date" => "Date:",
            "time" => "Time:",
            "textLength" => "Length:",
            _ => "Value:",
        };
        let second_label = "Maximum:";
        match field {
            Field::Tab => {
                let tabs: Vec<String> = TABS
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        if i == self.tab {
                            format!("[{t}]")
                        } else {
                            format!(" {t} ")
                        }
                    })
                    .collect();
                format!("Tab:  < {} >", tabs.join(""))
            }
            Field::Allow => format!("Allow:        < {} >", KINDS[self.kind].1),
            Field::Operator => format!("Data:         < {} >", OPERATORS[self.op].1),
            Field::First => format!("{first_label:<13} {}", self.first),
            Field::Second => format!("{second_label:<13} {}", self.second),
            Field::IgnoreBlank => format!("{} Ignore blank", check(self.ignore_blank)),
            Field::Dropdown => format!("{} In-cell dropdown", check(self.dropdown)),
            Field::ApplyAll => format!(
                "{} Apply these changes to all other cells with the same settings",
                check(self.apply_all)
            ),
            Field::ShowInput => {
                format!(
                    "{} Show input message when cell is selected",
                    check(self.show_input)
                )
            }
            Field::PromptTitle => format!("{:<13} {}", "Title:", self.prompt_title),
            Field::Prompt => format!("{:<13} {}", "Input message:", self.prompt),
            Field::ShowError => {
                format!(
                    "{} Show error alert after invalid data is entered",
                    check(self.show_error)
                )
            }
            Field::Style => format!("Style:        < {} >", STYLES[self.style].1),
            Field::ErrorTitle => format!("{:<13} {}", "Title:", self.error_title),
            Field::Error => format!("{:<13} {}", "Error message:", self.error),
            Field::Ok => "[ OK ]".into(),
            Field::ClearAll => "[ Clear All ]".into(),
            Field::Cancel => "[ Cancel ]".into(),
        }
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        let mut lines = vec![heading("Data Validation")];
        for (i, fld) in self.fields().into_iter().enumerate() {
            let mut text = self.line(fld);
            if i == self.focus && self.text_box_at(fld) {
                text.push('_');
            }
            lines.push(item(text, i == self.focus));
        }
        lines.push(hint(
            "Tab field  Left/Right choose  Space toggle  Enter OK  Esc Cancel",
        ));
        modal(f, area, " Data Validation ", lines, 78);
    }

    fn text_box_at(&self, field: Field) -> bool {
        matches!(
            field,
            Field::First
                | Field::Second
                | Field::PromptTitle
                | Field::Prompt
                | Field::ErrorTitle
                | Field::Error
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_into(d: &mut ValidationDialog, text: &str) {
        for ch in text.chars() {
            d.key(KeyCode::Char(ch));
        }
    }

    #[test]
    fn whole_between_with_an_error_alert() {
        let mut d = ValidationDialog::new(0, (1, 1, 9, 1), None);
        assert!(d.rule().unwrap().kind.is_empty());
        d.key(KeyCode::Down); // Allow
        d.key(KeyCode::Right); // Whole number
        d.key(KeyCode::Down); // Data
        d.key(KeyCode::Down); // Minimum
        type_into(&mut d, "10");
        d.key(KeyCode::Down); // Maximum
        type_into(&mut d, "=90");
        let dv = d.rule().unwrap();
        assert_eq!(
            (dv.kind.as_str(), dv.operator.as_str()),
            ("whole", "between")
        );
        assert_eq!((dv.formula1.as_str(), dv.formula2.as_str()), ("10", "90"));
        assert!(dv.allow_blank && dv.show_error);
    }

    #[test]
    fn a_missing_bound_is_refused_and_a_list_source_reads_both_ways() {
        let mut d = ValidationDialog::new(0, (1, 1, 9, 1), None);
        d.key(KeyCode::Down);
        d.key(KeyCode::Right);
        assert!(d.rule().is_err());
        // Allow: List.
        d.key(KeyCode::Right);
        d.key(KeyCode::Right);
        d.key(KeyCode::Down);
        type_into(&mut d, "Yes, No ,Maybe");
        assert_eq!(d.rule().unwrap().formula1, "\"Yes,No,Maybe\"");
        let mut d2 = ValidationDialog::new(0, (1, 1, 9, 1), Some(&d.rule().unwrap()));
        assert_eq!(d2.first, "Yes,No,Maybe");
        d2.first = "=$A$1:$A$5".into();
        assert_eq!(d2.rule().unwrap().formula1, "$A$1:$A$5");
    }

    #[test]
    fn message_and_title_limits_hold() {
        let mut d = ValidationDialog::new(0, (1, 1, 9, 1), None);
        d.key(KeyCode::Left); // tab strip: Error Alert
        d.key(KeyCode::Down); // Show error
        d.key(KeyCode::Down); // Style
        d.key(KeyCode::Down); // Title
        type_into(&mut d, &"t".repeat(40));
        d.key(KeyCode::Down); // Message
        type_into(&mut d, &"m".repeat(300));
        let dv = d.rule().unwrap();
        assert_eq!(dv.error_title.chars().count(), TITLE_MAX);
        assert_eq!(dv.error.chars().count(), MESSAGE_MAX);
    }

    #[test]
    fn clear_all_and_apply_all() {
        let mut d = ValidationDialog::new(0, (1, 1, 9, 1), None);
        d.key(KeyCode::Down);
        d.key(KeyCode::Down); // Apply-all is the last Settings box for "Any value"
        d.key(KeyCode::Char(' '));
        assert!(d.apply_all);
        d.key(KeyCode::Down);
        d.key(KeyCode::Down); // Clear All
        assert_eq!(d.key(KeyCode::Enter), Outcome::ClearAll);
    }
}
