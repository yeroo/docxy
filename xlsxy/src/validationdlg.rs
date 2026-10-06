//! Data ▸ Data Validation's dialog: the Settings, Input Message and Error
//! Alert tabs, with "Apply these changes to all other cells with the same
//! settings" and Clear All.
//!
//! Tab (Shift+Tab) / ↓ (↑) move between fields, ←/→ change a choice or the
//! tab, Space toggles a check box, and typing edits a text field. Enter is OK
//! (or the focused button), Esc is Cancel. The dialog only stages the rule;
//! the app applies it ([`gridcore::validation::set_validation`]).

use crate::outlinedlg::{heading, hint, item, modal};
use gridcore::sheet::DataValidation;
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;

use gridcore::validation::{ALERT_STYLES as STYLES, DialogBoxes, KINDS, OPERATORS};
pub use gridcore::validation::{MESSAGE_MAX, TITLE_MAX};

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
    /// What the boxes hold.
    b: DialogBoxes,
    /// The rule the dialog opened on, which a bound left as shown keeps.
    current: Option<DataValidation>,
    pub apply_all: bool,
    focus: usize,
}

impl ValidationDialog {
    /// The dialog showing `current`, the rule on the selection's first cell
    /// (or Excel's defaults when it has none).
    pub fn new(
        sheet: usize,
        range: (u32, u32, u32, u32),
        current: Option<&DataValidation>,
        date1904: bool,
    ) -> ValidationDialog {
        ValidationDialog {
            sheet,
            range,
            tab: 0,
            b: DialogBoxes::of(current, (range.0, range.1), date1904),
            current: current.cloned(),
            apply_all: false,
            focus: 0,
        }
    }

    fn kind_name(&self) -> &'static str {
        KINDS[self.b.kind].0
    }

    fn takes_operator(&self) -> bool {
        gridcore::validation::takes_operator(self.kind_name())
    }

    fn takes_two(&self) -> bool {
        gridcore::validation::takes_two(self.kind_name(), OPERATORS[self.b.operator].0)
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
            Field::First => Some((&mut self.b.first, MESSAGE_MAX)),
            Field::Second => Some((&mut self.b.second, MESSAGE_MAX)),
            Field::PromptTitle => Some((&mut self.b.prompt_title, TITLE_MAX)),
            Field::Prompt => Some((&mut self.b.prompt, MESSAGE_MAX)),
            Field::ErrorTitle => Some((&mut self.b.error_title, TITLE_MAX)),
            Field::Error => Some((&mut self.b.error, MESSAGE_MAX)),
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
                    Field::Allow => self.b.kind = cyc(self.b.kind, KINDS.len()),
                    Field::Operator => self.b.operator = cyc(self.b.operator, OPERATORS.len()),
                    Field::Style => self.b.style = cyc(self.b.style, STYLES.len()),
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
                        Field::IgnoreBlank => self.b.ignore_blank = !self.b.ignore_blank,
                        Field::Dropdown => self.b.dropdown = !self.b.dropdown,
                        Field::ApplyAll => self.apply_all = !self.apply_all,
                        Field::ShowInput => self.b.show_input = !self.b.show_input,
                        Field::ShowError => self.b.show_error = !self.b.show_error,
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
    /// applied when a bound is missing or isn't a date or time.
    pub fn rule(&self, ctx: &gridcore::entry::EntryCtx) -> Result<DataValidation, String> {
        self.b
            .rule(ctx, self.current.as_ref(), (self.range.0, self.range.1))
    }

    fn line(&self, field: Field) -> String {
        let check = |on: bool| if on { "[x]" } else { "[ ]" };
        let first_label =
            gridcore::validation::first_label(self.kind_name(), OPERATORS[self.b.operator].0);
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
            Field::Allow => format!("Allow:        < {} >", KINDS[self.b.kind].1),
            Field::Operator => format!("Data:         < {} >", OPERATORS[self.b.operator].1),
            Field::First => format!("{first_label:<13} {}", self.b.first),
            Field::Second => format!("{second_label:<13} {}", self.b.second),
            Field::IgnoreBlank => format!("{} Ignore blank", check(self.b.ignore_blank)),
            Field::Dropdown => format!("{} In-cell dropdown", check(self.b.dropdown)),
            Field::ApplyAll => format!(
                "{} Apply these changes to all other cells with the same settings",
                check(self.apply_all)
            ),
            Field::ShowInput => {
                format!(
                    "{} Show input message when cell is selected",
                    check(self.b.show_input)
                )
            }
            Field::PromptTitle => format!("{:<13} {}", "Title:", self.b.prompt_title),
            Field::Prompt => format!("{:<13} {}", "Input message:", self.b.prompt),
            Field::ShowError => {
                format!(
                    "{} Show error alert after invalid data is entered",
                    check(self.b.show_error)
                )
            }
            Field::Style => format!("Style:        < {} >", STYLES[self.b.style].1),
            Field::ErrorTitle => format!("{:<13} {}", "Title:", self.b.error_title),
            Field::Error => format!("{:<13} {}", "Error message:", self.b.error),
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

    fn ctx() -> gridcore::entry::EntryCtx {
        gridcore::entry::EntryCtx::default()
    }

    fn type_into(d: &mut ValidationDialog, text: &str) {
        for ch in text.chars() {
            d.key(KeyCode::Char(ch));
        }
    }

    #[test]
    fn whole_between_with_an_error_alert() {
        let mut d = ValidationDialog::new(0, (1, 1, 9, 1), None, false);
        assert!(d.rule(&ctx()).unwrap().kind.is_empty());
        d.key(KeyCode::Down); // Allow
        d.key(KeyCode::Right); // Whole number
        d.key(KeyCode::Down); // Data
        d.key(KeyCode::Down); // Minimum
        type_into(&mut d, "10");
        d.key(KeyCode::Down); // Maximum
        type_into(&mut d, "=90");
        let dv = d.rule(&ctx()).unwrap();
        assert_eq!(
            (dv.kind.as_str(), dv.operator.as_str()),
            ("whole", "between")
        );
        assert_eq!((dv.formula1.as_str(), dv.formula2.as_str()), ("10", "90"));
        assert!(dv.allow_blank && dv.show_error);
    }

    #[test]
    fn a_missing_bound_is_refused_and_a_list_source_reads_both_ways() {
        let mut d = ValidationDialog::new(0, (1, 1, 9, 1), None, false);
        d.key(KeyCode::Down);
        d.key(KeyCode::Right);
        assert!(d.rule(&ctx()).is_err());
        // Allow: List.
        d.key(KeyCode::Right);
        d.key(KeyCode::Right);
        d.key(KeyCode::Down);
        type_into(&mut d, "Yes, No ,Maybe");
        assert_eq!(d.rule(&ctx()).unwrap().formula1, "\"Yes,No,Maybe\"");
        let mut d2 = ValidationDialog::new(0, (1, 1, 9, 1), Some(&d.rule(&ctx()).unwrap()), false);
        assert_eq!(d2.b.first, "Yes,No,Maybe");
        d2.b.first = "=$A$1:$A$5".into();
        assert_eq!(d2.rule(&ctx()).unwrap().formula1, "$A$1:$A$5");
    }

    #[test]
    fn message_and_title_limits_hold() {
        let mut d = ValidationDialog::new(0, (1, 1, 9, 1), None, false);
        d.key(KeyCode::Left); // tab strip: Error Alert
        d.key(KeyCode::Down); // Show error
        d.key(KeyCode::Down); // Style
        d.key(KeyCode::Down); // Title
        type_into(&mut d, &"t".repeat(40));
        d.key(KeyCode::Down); // Message
        type_into(&mut d, &"m".repeat(300));
        let dv = d.rule(&ctx()).unwrap();
        assert_eq!(dv.error_title.chars().count(), TITLE_MAX);
        assert_eq!(dv.error.chars().count(), MESSAGE_MAX);
    }

    #[test]
    fn clear_all_and_apply_all() {
        let mut d = ValidationDialog::new(0, (1, 1, 9, 1), None, false);
        d.key(KeyCode::Down);
        d.key(KeyCode::Down); // Apply-all is the last Settings box for "Any value"
        d.key(KeyCode::Char(' '));
        assert!(d.apply_all);
        d.key(KeyCode::Down);
        d.key(KeyCode::Down); // Clear All
        assert_eq!(d.key(KeyCode::Enter), Outcome::ClearAll);
    }

    #[test]
    fn a_date_bound_is_typed_as_a_date_and_a_relative_formula_is_seen_from_the_cell() {
        let mut d = ValidationDialog::new(0, (1, 1, 9, 1), None, false);
        d.key(KeyCode::Down); // Allow
        for _ in 0..4 {
            d.key(KeyCode::Right); // Date
        }
        d.key(KeyCode::Down); // Data
        d.key(KeyCode::Right);
        d.key(KeyCode::Right);
        d.key(KeyCode::Right);
        d.key(KeyCode::Right); // greater than
        d.key(KeyCode::Down); // Value
        type_into(&mut d, "1/1/2020");
        let dv = d.rule(&ctx()).unwrap();
        assert_eq!((dv.kind.as_str(), dv.formula1.as_str()), ("date", "43831"));
        let d2 = ValidationDialog::new(0, (1, 1, 9, 1), Some(&dv), false);
        assert_eq!(d2.b.first, "1/1/2020");
        // A relative formula is shown from the selected cell.
        let custom = DataValidation {
            ranges: vec![(1, 1, 9, 1)],
            kind: "custom".into(),
            formula1: "B2>A2".into(),
            ..DataValidation::default()
        };
        let d3 = ValidationDialog::new(0, (4, 1, 5, 1), Some(&custom), false);
        assert_eq!(d3.b.first, "B5>A5");
    }
}
