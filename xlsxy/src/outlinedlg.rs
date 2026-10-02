//! Data ▸ Outline's dialogs: Subtotal (with Remove All), Settings, and the
//! Rows/Columns choice Group and Ungroup ask when the selection is neither
//! whole rows nor whole columns.
//!
//! ↑/↓ (Tab/Shift+Tab) move between fields, ←/→ change a choice, Space
//! toggles a check box or presses the focused button. Enter is OK (or the
//! focused button), Esc is Cancel. The dialogs only stage options; the app
//! applies them.

use gridcore::edit::{Area, SubtotalFunc, SubtotalOptions};
use gridcore::outline::{Axis, OutlineSettings};
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// What a key did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    Cancel,
    /// Subtotal's OK.
    Subtotal(SubtotalOptions),
    /// Subtotal's Remove All.
    RemoveAll,
    /// Settings' OK.
    Settings(OutlineSettings),
    /// The Rows/Columns choice made.
    Axis(Axis),
}

/// The Subtotal dialog over one region.
#[derive(Clone, Debug)]
pub struct SubtotalDialog {
    /// The sheet the dialog opened on: OK and Remove All act there.
    pub sheet: usize,
    /// The region (rows and columns) and whether its first row is a header.
    pub area: Area,
    pub has_header: bool,
    /// The region's columns and how the dialog names them.
    pub cols: Vec<(u32, String)>,
    /// Index into `cols` of "At each change in".
    pub group: usize,
    pub func: SubtotalFunc,
    /// One check box per entry of `cols`: "Add subtotal to".
    pub add_to: Vec<bool>,
    pub replace: bool,
    pub page_breaks: bool,
    pub summary_below: bool,
    pub focus: usize,
}

/// A Subtotal dialog row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubField {
    Group,
    Func,
    AddTo(usize),
    Replace,
    PageBreaks,
    SummaryBelow,
    Ok,
    RemoveAll,
    Cancel,
}

impl SubtotalDialog {
    /// The dialog over `area`, its `cols`, showing `defaults` (Excel's are
    /// [`SubtotalOptions::new`]).
    pub fn new(
        sheet: usize,
        area: Area,
        cols: Vec<(u32, String)>,
        defaults: &SubtotalOptions,
    ) -> SubtotalDialog {
        let group = cols
            .iter()
            .position(|(c, _)| *c == defaults.group_col)
            .unwrap_or(0);
        let add_to = cols
            .iter()
            .map(|(c, _)| defaults.add_to.contains(c))
            .collect();
        SubtotalDialog {
            sheet,
            area,
            has_header: defaults.has_header,
            cols,
            group,
            func: defaults.func,
            add_to,
            replace: defaults.replace,
            page_breaks: defaults.page_breaks,
            summary_below: defaults.summary_below,
            focus: 0,
        }
    }

    fn fields(&self) -> Vec<SubField> {
        let mut f = vec![SubField::Group, SubField::Func];
        f.extend((0..self.cols.len()).map(SubField::AddTo));
        f.extend([
            SubField::Replace,
            SubField::PageBreaks,
            SubField::SummaryBelow,
            SubField::Ok,
            SubField::RemoveAll,
            SubField::Cancel,
        ]);
        f
    }

    /// The options OK applies.
    pub fn options(&self) -> SubtotalOptions {
        SubtotalOptions {
            group_col: self.cols.get(self.group).map_or(0, |(c, _)| *c),
            func: self.func,
            add_to: self
                .cols
                .iter()
                .zip(&self.add_to)
                .filter(|(_, on)| **on)
                .map(|((c, _), _)| *c)
                .collect(),
            replace: self.replace,
            page_breaks: self.page_breaks,
            summary_below: self.summary_below,
            has_header: self.has_header,
        }
    }

    pub fn key(&mut self, code: KeyCode) -> Outcome {
        let fields = self.fields();
        let n = fields.len();
        let field = fields[self.focus.min(n - 1)];
        match code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Up | KeyCode::BackTab => self.focus = (self.focus + n - 1) % n,
            KeyCode::Down | KeyCode::Tab => self.focus = (self.focus + 1) % n,
            KeyCode::Left | KeyCode::Right => {
                let step = if code == KeyCode::Left { -1 } else { 1 };
                match field {
                    SubField::Group if !self.cols.is_empty() => {
                        self.group = cycle(self.group, self.cols.len(), step);
                    }
                    SubField::Func => {
                        let all = SubtotalFunc::ALL;
                        let i = all.iter().position(|f| *f == self.func).unwrap_or(0);
                        self.func = all[cycle(i, all.len(), step)];
                    }
                    _ => {}
                }
            }
            KeyCode::Char(' ') => match field {
                SubField::AddTo(i) => self.add_to[i] = !self.add_to[i],
                SubField::Replace => self.replace = !self.replace,
                SubField::PageBreaks => self.page_breaks = !self.page_breaks,
                SubField::SummaryBelow => self.summary_below = !self.summary_below,
                SubField::Ok => return Outcome::Subtotal(self.options()),
                SubField::RemoveAll => return Outcome::RemoveAll,
                SubField::Cancel => return Outcome::Cancel,
                _ => {}
            },
            KeyCode::Enter => {
                return match field {
                    SubField::RemoveAll => Outcome::RemoveAll,
                    SubField::Cancel => Outcome::Cancel,
                    _ => Outcome::Subtotal(self.options()),
                };
            }
            _ => {}
        }
        Outcome::Pending
    }

    fn line(&self, f: SubField) -> String {
        let name = |i: usize| self.cols.get(i).map_or("", |(_, n)| n.as_str());
        match f {
            SubField::Group => format!("At each change in:   < {} >", name(self.group)),
            SubField::Func => format!("Use function:        < {} >", self.func.name()),
            SubField::AddTo(i) => format!("  {} {}", check(self.add_to[i]), name(i)),
            SubField::Replace => format!("{} Replace current subtotals", check(self.replace)),
            SubField::PageBreaks => {
                format!("{} Page break between groups", check(self.page_breaks))
            }
            SubField::SummaryBelow => {
                format!("{} Summary below data", check(self.summary_below))
            }
            SubField::Ok => "[ OK ]".into(),
            SubField::RemoveAll => "[ Remove All ]".into(),
            SubField::Cancel => "[ Cancel ]".into(),
        }
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        let fields = self.fields();
        let mut lines = Vec::new();
        for (i, fld) in fields.iter().enumerate() {
            if *fld == SubField::AddTo(0) {
                lines.push(heading("Add subtotal to:"));
            }
            lines.push(item(self.line(*fld), i == self.focus));
        }
        lines.push(hint(
            "Up/Down field  Left/Right choose  Space toggle  Enter OK  Esc Cancel",
        ));
        modal(f, area, " Subtotal ", lines, 58);
    }
}

/// The outline Settings dialog.
#[derive(Clone, Debug)]
pub struct SettingsDialog {
    /// The sheet the dialog opened on.
    pub sheet: usize,
    pub settings: OutlineSettings,
    pub focus: usize,
}

impl SettingsDialog {
    pub fn new(sheet: usize, settings: OutlineSettings) -> SettingsDialog {
        SettingsDialog {
            sheet,
            settings,
            focus: 0,
        }
    }

    pub fn key(&mut self, code: KeyCode) -> Outcome {
        const N: usize = 4; // the two boxes, OK, Cancel
        match code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Up | KeyCode::BackTab => self.focus = (self.focus + N - 1) % N,
            KeyCode::Down | KeyCode::Tab => self.focus = (self.focus + 1) % N,
            KeyCode::Char(' ') => match self.focus {
                0 => self.settings.summary_below = !self.settings.summary_below,
                1 => self.settings.summary_right = !self.settings.summary_right,
                2 => return Outcome::Settings(self.settings),
                _ => return Outcome::Cancel,
            },
            KeyCode::Enter if self.focus == 3 => return Outcome::Cancel,
            KeyCode::Enter => return Outcome::Settings(self.settings),
            _ => {}
        }
        Outcome::Pending
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        let s = self.settings;
        let items = [
            format!("{} Summary rows below detail", check(s.summary_below)),
            format!(
                "{} Summary columns to right of detail",
                check(s.summary_right)
            ),
            "[ OK ]".into(),
            "[ Cancel ]".into(),
        ];
        let mut lines = vec![heading("Direction")];
        for (i, t) in items.into_iter().enumerate() {
            lines.push(item(t, i == self.focus));
        }
        lines.push(hint("Up/Down  Space toggle  Enter OK  Esc Cancel"));
        modal(f, area, " Settings ", lines, 46);
    }
}

/// Group / Ungroup's "Rows or Columns?" when the selection doesn't say.
#[derive(Clone, Copy, Debug)]
pub struct AxisDialog {
    pub ungroup: bool,
    pub rows: bool,
}

impl AxisDialog {
    pub fn new(ungroup: bool) -> AxisDialog {
        AxisDialog {
            ungroup,
            rows: true,
        }
    }

    pub fn key(&mut self, code: KeyCode) -> Outcome {
        match code {
            KeyCode::Esc => Outcome::Cancel,
            KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab | KeyCode::Char(' ') => {
                self.rows = !self.rows;
                Outcome::Pending
            }
            KeyCode::Char('r') | KeyCode::Char('R') => Outcome::Axis(Axis::Rows),
            KeyCode::Char('c') | KeyCode::Char('C') => Outcome::Axis(Axis::Cols),
            KeyCode::Enter => Outcome::Axis(if self.rows { Axis::Rows } else { Axis::Cols }),
            _ => Outcome::Pending,
        }
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        let radio = |on: bool| if on { "(•)" } else { "( )" };
        let lines = vec![
            item(format!("{} Rows", radio(self.rows)), self.rows),
            item(format!("{} Columns", radio(!self.rows)), !self.rows),
            hint("Up/Down choose  Enter OK  Esc Cancel"),
        ];
        let title = if self.ungroup { " Ungroup " } else { " Group " };
        modal(f, area, title, lines, 40);
    }
}

/// One open outline dialog.
#[derive(Clone, Debug)]
pub enum Dialog {
    Subtotal(SubtotalDialog),
    Settings(SettingsDialog),
    Axis(AxisDialog),
}

impl Dialog {
    pub fn key(&mut self, code: KeyCode) -> Outcome {
        match self {
            Dialog::Subtotal(d) => d.key(code),
            Dialog::Settings(d) => d.key(code),
            Dialog::Axis(d) => d.key(code),
        }
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        match self {
            Dialog::Subtotal(d) => d.draw(f, area),
            Dialog::Settings(d) => d.draw(f, area),
            Dialog::Axis(d) => d.draw(f, area),
        }
    }
}

fn cycle(i: usize, n: usize, step: i32) -> usize {
    ((i as i64 + step as i64).rem_euclid(n as i64)) as usize
}

fn check(on: bool) -> &'static str {
    if on { "[x]" } else { "[ ]" }
}

fn item(text: String, focused: bool) -> Line<'static> {
    let style = if focused {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new()
    };
    Line::from(Span::styled(text, style))
}

fn heading(text: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        text,
        Style::new().add_modifier(Modifier::BOLD),
    ))
}

fn hint(text: &'static str) -> Line<'static> {
    Line::from(Span::styled(text, Style::new().fg(Color::DarkGray)))
}

/// Draw `lines` in a bordered box centred over `area`, scrolled so the
/// focused (reversed) line stays in view when the box is too short.
fn modal(f: &mut Frame, area: Rect, title: &str, lines: Vec<Line<'static>>, width: u16) {
    let w = width.min(area.width.saturating_sub(2));
    let h = (lines.len() as u16 + 2).min(area.height);
    if w < 20 || h < 4 {
        return;
    }
    let rect = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    let inner = (h - 2) as usize;
    let focused = lines
        .iter()
        .position(|l| {
            l.spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::REVERSED))
        })
        .unwrap_or(0);
    let scroll = focused.saturating_sub(inner.saturating_sub(2)) as u16;
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string())
        .border_style(Style::new().fg(Color::Green));
    f.render_widget(Paragraph::new(lines).block(block).scroll((scroll, 0)), rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> SubtotalDialog {
        SubtotalDialog::new(
            0,
            (0, 0, 5, 2),
            vec![(0, "Grp".into()), (1, "Amt".into()), (2, "Qty".into())],
            &SubtotalOptions::new(0, vec![1], true),
        )
    }

    #[test]
    fn subtotal_dialog_maps_its_fields_to_options() {
        let mut d = dialog();
        assert_eq!(d.options(), SubtotalOptions::new(0, vec![1], true));
        d.key(KeyCode::Right); // At each change in: Amt
        d.key(KeyCode::Down);
        d.key(KeyCode::Right); // Count
        d.key(KeyCode::Right); // Average
        d.key(KeyCode::Down); // Grp box
        d.key(KeyCode::Down); // Amt box
        d.key(KeyCode::Char(' ')); // off
        d.key(KeyCode::Down); // Qty box
        d.key(KeyCode::Char(' ')); // on
        d.key(KeyCode::Down); // Replace
        d.key(KeyCode::Char(' '));
        d.key(KeyCode::Down); // Page break
        d.key(KeyCode::Char(' '));
        d.key(KeyCode::Down); // Summary below
        d.key(KeyCode::Char(' '));
        let o = match d.key(KeyCode::Enter) {
            Outcome::Subtotal(o) => o,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            o,
            SubtotalOptions {
                group_col: 1,
                func: SubtotalFunc::Average,
                add_to: vec![2],
                replace: false,
                page_breaks: true,
                summary_below: false,
                has_header: true,
            }
        );
    }

    #[test]
    fn subtotal_dialog_buttons() {
        let mut d = dialog();
        // Up from the first field wraps to Cancel, then Remove All.
        d.key(KeyCode::Up);
        assert_eq!(d.key(KeyCode::Enter), Outcome::Cancel);
        d.key(KeyCode::Up);
        assert_eq!(d.key(KeyCode::Enter), Outcome::RemoveAll);
        assert_eq!(d.key(KeyCode::Char(' ')), Outcome::RemoveAll);
        assert_eq!(d.key(KeyCode::Esc), Outcome::Cancel);
        // Function cycles both ways through all eleven.
        let mut d = dialog();
        d.key(KeyCode::Down);
        d.key(KeyCode::Left);
        assert_eq!(d.func, SubtotalFunc::VarP);
    }

    #[test]
    fn settings_dialog_toggles_and_confirms() {
        let mut d = SettingsDialog::new(0, OutlineSettings::default());
        d.key(KeyCode::Char(' '));
        d.key(KeyCode::Down);
        d.key(KeyCode::Char(' '));
        assert_eq!(
            d.key(KeyCode::Enter),
            Outcome::Settings(OutlineSettings {
                summary_below: false,
                summary_right: false
            })
        );
        d.key(KeyCode::Down);
        d.key(KeyCode::Down); // Cancel
        assert_eq!(d.key(KeyCode::Enter), Outcome::Cancel);
    }

    #[test]
    fn axis_dialog_picks_rows_or_columns() {
        let mut d = AxisDialog::new(false);
        assert_eq!(d.key(KeyCode::Enter), Outcome::Axis(Axis::Rows));
        d.key(KeyCode::Down);
        assert_eq!(d.key(KeyCode::Enter), Outcome::Axis(Axis::Cols));
        assert_eq!(d.key(KeyCode::Char('r')), Outcome::Axis(Axis::Rows));
        assert_eq!(d.key(KeyCode::Esc), Outcome::Cancel);
    }
}
