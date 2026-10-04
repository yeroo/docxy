//! Data ▸ Form…: Excel's data form over a list, one record at a time.
//!
//! The dialog holds the shown record's fields as text and edits them; the
//! app reads the sheet into it ([`DataForm::show`]) and does every write
//! (gridcore's `edit::dataform`). Tab/Shift+Tab move between the fields and
//! buttons, ↑/↓ and PgUp/PgDn are Find Prev/Find Next, Enter on a field
//! commits and moves on (Find Next under criteria), Enter or Space presses
//! a button. Esc restores a record with changes and otherwise closes.

use crate::outlinedlg::{heading, hint, item, modal};
use gridcore::edit::{Area, is_formula_field};
use gridcore::entry::seed_text;
use gridcore::sheet::{Sheet, Styles, format_with};
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;

/// The record a form shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rec {
    /// The record in this sheet row.
    Row(u32),
    /// A blank record that New appends below the list.
    New,
}

/// Fields show the record, or the criteria Find Prev and Find Next use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Form,
    Criteria,
}

/// One field: a column of the list.
#[derive(Clone, Debug)]
pub struct Field {
    pub col: u32,
    pub label: String,
    /// What the field shows: the cell's input text, or a computed field's
    /// value as the grid shows it.
    pub text: String,
    /// `text` as the sheet holds it, for Restore and to tell an edit.
    pub orig: String,
    /// Computed in the shown record: read-only.
    pub formula: bool,
    /// This column's criterion.
    pub criterion: String,
}

/// The form's buttons, in their order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    New,
    Delete,
    Restore,
    FindPrev,
    FindNext,
    /// Criteria, or Form while the criteria are shown.
    Criteria,
    /// Clears the criteria (criteria only).
    Clear,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Field(usize),
    Button(Button),
}

/// What a key asks the app to do; the app commits the shown record's
/// changes first wherever Excel does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    /// Close (committing).
    Close,
    /// Find Next (`true`) or Find Prev.
    Step(bool),
    /// New: commit, then show a blank record.
    New,
    /// Delete the shown record (after asking).
    Delete,
    /// Commit, then show the criteria.
    Criteria,
    /// Back from the criteria to the record shown before them.
    Form,
}

/// The data form over `area` on sheet `sheet`.
#[derive(Clone, Debug)]
pub struct DataForm {
    pub sheet: usize,
    pub sheet_name: String,
    /// The list: its first row labels the fields, the rows below are the
    /// records. Fixed when the form opens; New and Delete move its bottom.
    pub area: Area,
    pub rec: Rec,
    pub mode: Mode,
    pub fields: Vec<Field>,
    /// The record shown when the criteria were: Find Prev and Find Next
    /// search from it.
    pub back: Rec,
    focus: usize,
    /// Char position in the focused field.
    cursor: usize,
}

impl DataForm {
    /// A form over `area` labelled by `labels` (`(col, label)`), not yet
    /// showing a record.
    pub fn new(
        sheet: usize,
        sheet_name: String,
        area: Area,
        labels: Vec<(u32, String)>,
    ) -> DataForm {
        let fields = labels
            .into_iter()
            .map(|(col, label)| Field {
                col,
                label,
                text: String::new(),
                orig: String::new(),
                formula: false,
                criterion: String::new(),
            })
            .collect();
        DataForm {
            sheet,
            sheet_name,
            area,
            rec: Rec::New,
            mode: Mode::Form,
            fields,
            back: Rec::New,
            focus: 0,
            cursor: 0,
        }
    }

    /// Show `rec`, read from sheet `s`: every field's text as the sheet holds
    /// it, no changes pending. Focus stays on the same field or button (the
    /// nearest editable field when its field is computed in this record); a
    /// new record focuses its first editable field.
    pub fn show(&mut self, s: &Sheet, styles: &Styles, date1904: bool, rec: Rec) {
        // What has the focus, not where: the fields Tab visits differ
        // between records and between the criteria and the form.
        let prev = if rec == Rec::New {
            Item::Field(0)
        } else {
            self.focused()
        };
        self.rec = rec;
        self.mode = Mode::Form;
        for i in 0..self.fields.len() {
            let (text, formula) = self.read_field(s, styles, date1904, self.fields[i].col);
            let f = &mut self.fields[i];
            f.text = text.clone();
            f.orig = text;
            f.formula = formula;
        }
        self.focus_on(prev);
    }

    /// Read the shown record again from sheet `s` after it changed in place,
    /// keeping the fields the user has changed (text, and whether it's
    /// computed) and the caret.
    pub fn refresh(&mut self, s: &Sheet, styles: &Styles, date1904: bool) {
        let (prev, cursor) = (self.focused(), self.cursor);
        for i in 0..self.fields.len() {
            if self.fields[i].text != self.fields[i].orig {
                continue;
            }
            let (text, formula) = self.read_field(s, styles, date1904, self.fields[i].col);
            let f = &mut self.fields[i];
            f.text = text.clone();
            f.orig = text;
            f.formula = formula;
        }
        self.focus_on(prev);
        if self.focused() == prev {
            let len = self.focused_text().map_or(0, |t| t.chars().count());
            self.cursor = cursor.min(len);
        }
    }

    /// The field of `col` in the shown record, as the sheet holds it: its
    /// input text, or a computed field's value as the grid shows it; and
    /// whether it is computed. A new record's computed fields are the last
    /// record's: they fill from it.
    fn read_field(&self, s: &Sheet, styles: &Styles, date1904: bool, col: u32) -> (String, bool) {
        let (top, _, last, _) = self.area;
        match self.rec {
            Rec::Row(r) => match s.cell(r, col) {
                Some(c) if c.formula.is_some() => {
                    (format_with(&styles.xf(c.style), &c.value, date1904), true)
                }
                Some(c) => (seed_text(c, &styles.xf(c.style)), false),
                None => (String::new(), false),
            },
            Rec::New => (String::new(), last > top && is_formula_field(s, last, col)),
        }
    }

    /// Show the criteria, the first field focused for typing.
    pub fn show_criteria(&mut self) {
        self.back = self.rec;
        self.mode = Mode::Criteria;
        self.focus_on(Item::Field(0));
    }

    /// The `(col, criterion)` pairs Find Prev and Find Next use: the
    /// non-empty criteria.
    pub fn criteria(&self) -> Vec<(u32, String)> {
        self.fields
            .iter()
            .filter(|f| !f.criterion.is_empty())
            .map(|f| (f.col, f.criterion.clone()))
            .collect()
    }

    /// The fields changed since the record was shown, as `(col, text)`.
    pub fn changes(&self) -> Vec<(u32, String)> {
        self.fields
            .iter()
            .filter(|f| !f.formula && f.text != f.orig)
            .map(|f| (f.col, f.text.clone()))
            .collect()
    }

    /// Whether the shown record has changes not yet written.
    pub fn dirty(&self) -> bool {
        self.mode == Mode::Form && !self.changes().is_empty()
    }

    /// Restore: drop the shown record's changes.
    pub fn restore(&mut self) {
        for f in &mut self.fields {
            f.text = f.orig.clone();
        }
        self.cursor = self.focused_text().map_or(0, |t| t.chars().count());
    }

    /// The number of records, and the shown one's place among them
    /// (`3 of 12`, `New Record`, `Criteria`).
    pub fn position(&self) -> String {
        match (self.mode, self.rec) {
            (Mode::Criteria, _) => "Criteria".into(),
            (_, Rec::New) => "New Record".into(),
            (_, Rec::Row(r)) => {
                let (top, _, bottom, _) = self.area;
                format!("{} of {}", r - top, bottom - top)
            }
        }
    }

    fn editable(&self, i: usize) -> bool {
        self.mode == Mode::Criteria || !self.fields[i].formula
    }

    fn buttons(&self) -> Vec<Button> {
        let mut b = vec![
            Button::New,
            Button::Delete,
            Button::Restore,
            Button::FindPrev,
            Button::FindNext,
            Button::Criteria,
        ];
        if self.mode == Mode::Criteria {
            b.push(Button::Clear);
        }
        b.push(Button::Close);
        b
    }

    /// What Tab visits: the editable fields, then the buttons.
    fn items(&self) -> Vec<Item> {
        let mut items: Vec<Item> = (0..self.fields.len())
            .filter(|&i| self.editable(i))
            .map(Item::Field)
            .collect();
        items.extend(self.buttons().into_iter().map(Item::Button));
        items
    }

    fn focused(&self) -> Item {
        let items = self.items();
        items[self.focus.min(items.len() - 1)]
    }

    /// Focus `item`; a field Tab doesn't visit here (computed in this
    /// record) gives way to the nearest one it does, the later on a tie;
    /// anything else missing, to the first item.
    fn focus_on(&mut self, item: Item) {
        let items = self.items();
        let nearest = |f: usize| {
            items
                .iter()
                .enumerate()
                .filter_map(|(k, it)| match it {
                    Item::Field(j) => Some((j.abs_diff(f), *j < f, k)),
                    Item::Button(_) => None,
                })
                .min()
                .map(|(.., k)| k)
        };
        self.focus = items
            .iter()
            .position(|i| *i == item)
            .or_else(|| match item {
                Item::Field(f) => nearest(f),
                Item::Button(_) => None,
            })
            .unwrap_or(0);
        self.cursor = self.focused_text().map_or(0, |t| t.chars().count());
    }

    fn focused_text(&self) -> Option<&String> {
        match self.focused() {
            Item::Field(i) if self.mode == Mode::Criteria => Some(&self.fields[i].criterion),
            Item::Field(i) => Some(&self.fields[i].text),
            Item::Button(_) => None,
        }
    }

    fn focused_text_mut(&mut self) -> Option<&mut String> {
        match self.focused() {
            Item::Field(i) if self.mode == Mode::Criteria => Some(&mut self.fields[i].criterion),
            Item::Field(i) => Some(&mut self.fields[i].text),
            Item::Button(_) => None,
        }
    }

    pub fn key(&mut self, code: KeyCode) -> Outcome {
        let n = self.items().len();
        match code {
            KeyCode::Esc if self.dirty() => self.restore(),
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Tab => {
                self.focus = (self.focus.min(n - 1) + 1) % n;
                self.cursor = self.focused_text().map_or(0, |t| t.chars().count());
            }
            KeyCode::BackTab => {
                self.focus = (self.focus.min(n - 1) + n - 1) % n;
                self.cursor = self.focused_text().map_or(0, |t| t.chars().count());
            }
            KeyCode::Up | KeyCode::PageUp => return Outcome::Step(false),
            KeyCode::Down | KeyCode::PageDown => return Outcome::Step(true),
            KeyCode::Enter => {
                return match self.focused() {
                    Item::Button(b) => self.press(b),
                    Item::Field(_) => match (self.mode, self.rec) {
                        (Mode::Form, Rec::New) => Outcome::New,
                        _ => Outcome::Step(true),
                    },
                };
            }
            KeyCode::Char(' ') if matches!(self.focused(), Item::Button(_)) => {
                if let Item::Button(b) = self.focused() {
                    return self.press(b);
                }
            }
            _ => self.edit_key(code),
        }
        Outcome::Pending
    }

    fn press(&mut self, b: Button) -> Outcome {
        match b {
            Button::New => Outcome::New,
            Button::Delete => Outcome::Delete,
            Button::Restore => {
                self.restore();
                Outcome::Pending
            }
            Button::FindPrev => Outcome::Step(false),
            Button::FindNext => Outcome::Step(true),
            Button::Criteria if self.mode == Mode::Criteria => Outcome::Form,
            Button::Criteria => Outcome::Criteria,
            Button::Clear => {
                for f in &mut self.fields {
                    f.criterion.clear();
                }
                Outcome::Pending
            }
            Button::Close => Outcome::Close,
        }
    }

    /// A key in the focused field: type, delete, move the cursor.
    fn edit_key(&mut self, code: KeyCode) {
        let cursor = self.cursor;
        let Some(text) = self.focused_text_mut() else {
            return;
        };
        let len = text.chars().count();
        let at = |t: &String, i: usize| t.char_indices().nth(i).map_or(t.len(), |(b, _)| b);
        let cursor = match code {
            KeyCode::Char(ch) => {
                text.insert(at(text, cursor), ch);
                cursor + 1
            }
            KeyCode::Backspace if cursor > 0 => {
                text.remove(at(text, cursor - 1));
                cursor - 1
            }
            KeyCode::Delete if cursor < len => {
                text.remove(at(text, cursor));
                cursor
            }
            KeyCode::Left => cursor.saturating_sub(1),
            KeyCode::Right => (cursor + 1).min(len),
            KeyCode::Home => 0,
            KeyCode::End => len,
            _ => cursor,
        };
        self.cursor = cursor;
    }

    /// The column of the focused field.
    #[cfg(test)]
    pub fn focused_col(&self) -> Option<u32> {
        match self.focused() {
            Item::Field(i) => Some(self.fields[i].col),
            Item::Button(_) => None,
        }
    }

    #[cfg(test)]
    pub fn focused_button(&self) -> Option<Button> {
        match self.focused() {
            Item::Button(b) => Some(b),
            Item::Field(_) => None,
        }
    }

    /// Focus the field of `col` (after a refused commit, say).
    pub fn focus_col(&mut self, col: u32) {
        if let Some(i) = self.fields.iter().position(|f| f.col == col) {
            self.focus_on(Item::Field(i));
        }
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        let focused = self.focused();
        let w = self
            .fields
            .iter()
            .map(|f| f.label.chars().count())
            .max()
            .unwrap_or(0)
            .min(20);
        let mut lines = vec![heading(self.position()), heading("")];
        for (i, fld) in self.fields.iter().enumerate() {
            let label: String = fld.label.chars().take(w).collect();
            let has_focus = focused == Item::Field(i);
            let value = if self.mode == Mode::Criteria {
                &fld.criterion
            } else {
                &fld.text
            };
            let mut value = value.clone();
            if has_focus {
                let b = value
                    .char_indices()
                    .nth(self.cursor)
                    .map_or(value.len(), |(b, _)| b);
                value.insert(b, '▏');
            }
            let line = if self.editable(i) {
                format!("{label:>w$}: [{value}]")
            } else {
                format!("{label:>w$}:  {value}")
            };
            lines.push(item(line, has_focus));
        }
        lines.push(heading(""));
        for b in self.buttons() {
            lines.push(item(
                format!("[ {} ]", self.button_name(b)),
                focused == Item::Button(b),
            ));
        }
        lines.push(hint(
            "Tab field/button  Up/Down record  Enter commit  Esc restore/close",
        ));
        modal(
            f,
            area,
            &format!(" Data Form — {} ", self.sheet_name),
            lines,
            60,
        );
    }

    fn button_name(&self, b: Button) -> &'static str {
        match b {
            Button::New => "New",
            Button::Delete => "Delete",
            Button::Restore => "Restore",
            Button::FindPrev => "Find Prev",
            Button::FindNext => "Find Next",
            Button::Criteria if self.mode == Mode::Criteria => "Form",
            Button::Criteria => "Criteria",
            Button::Clear => "Clear",
            Button::Close => "Close",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridcore::sheet::Cell;

    fn form() -> (DataForm, Sheet, Styles) {
        let mut s = Sheet::default();
        for (c, h) in ["Name", "Qty", "Total"].iter().enumerate() {
            s.set_cell(0, c as u32, Cell::text(h));
        }
        s.set_cell(1, 0, Cell::text("Ann"));
        s.set_cell(1, 1, Cell::number(5.0));
        let mut f = Cell::formula("B2*2");
        f.value = gridcore::sheet::CellValue::Number(10.0);
        s.set_cell(1, 2, f);
        let labels = vec![(0, "Name".into()), (1, "Qty".into()), (2, "Total".into())];
        let mut d = DataForm::new(0, "Sheet1".into(), (0, 0, 1, 2), labels);
        let styles = Styles::default();
        d.show(&s, &styles, false, Rec::Row(1));
        (d, s, styles)
    }

    #[test]
    fn tab_skips_the_computed_field_and_edits_the_buffer() {
        let (mut d, ..) = form();
        assert_eq!(d.position(), "1 of 1");
        assert_eq!(d.fields[2].text, "10");
        assert_eq!(d.focused(), Item::Field(0));
        d.key(KeyCode::Tab);
        assert_eq!(d.focused(), Item::Field(1));
        d.key(KeyCode::Tab);
        assert_eq!(d.focused(), Item::Button(Button::New), "Total is read-only");
        d.key(KeyCode::BackTab);
        d.key(KeyCode::Backspace);
        d.key(KeyCode::Char('7'));
        d.key(KeyCode::Home);
        d.key(KeyCode::Char('1'));
        d.key(KeyCode::Char('2'));
        assert_eq!(d.changes(), [(1, "127".to_string())]);
        d.key(KeyCode::Right);
        d.key(KeyCode::Left);
        d.key(KeyCode::Delete);
        assert_eq!(d.changes(), [(1, "12".to_string())]);
        assert!(d.dirty());
        // Esc first restores, then closes.
        assert_eq!(d.key(KeyCode::Esc), Outcome::Pending);
        assert!(!d.dirty());
        assert_eq!(d.key(KeyCode::Esc), Outcome::Close);
    }

    #[test]
    fn criteria_edit_every_field_and_clear() {
        let (mut d, ..) = form();
        d.show_criteria();
        assert_eq!(d.position(), "Criteria");
        for _ in 0..2 {
            d.key(KeyCode::Tab);
        }
        assert_eq!(
            d.focused(),
            Item::Field(2),
            "a computed field takes criteria"
        );
        d.key(KeyCode::Char('>'));
        d.key(KeyCode::Char('5'));
        assert_eq!(d.criteria(), [(2, ">5".to_string())]);
        assert!(!d.dirty(), "criteria are not record changes");
        assert_eq!(d.key(KeyCode::Enter), Outcome::Step(true));
        d.focus_on(Item::Button(Button::Criteria));
        assert_eq!(d.key(KeyCode::Enter), Outcome::Form);
        d.focus_on(Item::Button(Button::Clear));
        d.key(KeyCode::Char(' '));
        assert!(d.criteria().is_empty());
    }
}
