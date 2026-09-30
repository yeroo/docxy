//! Excel's Text Import Wizard and Convert Text to Columns Wizard: one modal,
//! three steps, over [`gridcore::textio`].
//!
//! - Step 1: Delimited or Fixed width; for an import also the start row and
//!   the file origin (encoding).
//! - Step 2: the delimiter boxes, Other, Treat consecutive delimiters as one
//!   and the text qualifier; or, for fixed width, the break lines (a ruler
//!   caret moved with ←/→, Space sets or clears a break).
//! - Step 3: each preview column's data format (General, Text, Date with its
//!   order, Do not import), the Advanced separators and trailing minus, and,
//!   for Text to Columns, the Destination.
//!
//! Tab / Shift+Tab move between steps, ↑/↓ between fields, ←/→ or Space
//! change the focused field, typing fills a text field. Enter is Finish,
//! Esc is Cancel. The dialog only stages options; the app applies them.

use gridcore::edit::TtcSource;
use gridcore::textio::{
    ColFormat, DateOrder, Delimiters, Origin, SplitKind, TextParse, decode, split_text, split_value,
};
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// What the wizard is for.
#[derive(Clone, Debug)]
pub enum Purpose {
    /// Opening a text file: its path and bytes.
    Import { path: String, bytes: Vec<u8> },
    /// Data › Text to Columns on a column of the open sheet: its cells'
    /// displayed text, for the preview.
    Columns { src: TtcSource, sample: Vec<String> },
}

/// One field of a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Kind,
    StartRow,
    Origin,
    Tab,
    Semicolon,
    Comma,
    Space,
    Other,
    Consecutive,
    Qualifier,
    Breaks,
    Column,
    Format,
    DateOrder,
    Decimal,
    Thousands,
    TrailingMinus,
    Destination,
}

/// What a key did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    Cancel,
    Finish,
}

const ORIGINS: [Origin; 4] = [
    Origin::Auto,
    Origin::Utf8,
    Origin::Utf16Le,
    Origin::Windows1252,
];
const QUALIFIERS: [Option<char>; 3] = [Some('"'), Some('\''), None];
const SEPARATORS: [char; 4] = ['.', ',', ' ', '\''];
/// Rows the preview shows.
const PREVIEW_ROWS: usize = 8;

pub struct TextDialog {
    pub purpose: Purpose,
    /// 0, 1 or 2: Excel's steps 1 to 3.
    pub step: usize,
    pub focus: usize,
    pub fixed: bool,
    pub delims: Delimiters,
    pub consecutive: bool,
    pub qualifier: Option<char>,
    pub breaks: Vec<usize>,
    /// The start row as typed: 0 while the field is emptied; [`parse`] reads
    /// it as at least 1.
    ///
    /// [`parse`]: TextDialog::parse
    pub start_row: usize,
    /// A digit has been typed into the focused field since it took the
    /// focus: the first digit replaces the value, later ones append.
    typed: bool,
    pub origin: Origin,
    pub columns: Vec<ColFormat>,
    pub decimal: char,
    pub thousands: char,
    pub trailing_minus: bool,
    /// The Destination cell (Text to Columns), as typed.
    pub dest: String,
    /// The selected preview column (step 3).
    pub col: usize,
    /// The fixed-width ruler caret (step 2).
    pub ruler: usize,
    /// The import's text under the current origin.
    text: String,
}

impl TextDialog {
    /// The wizard for opening a text file, with Excel's defaults: delimited
    /// by Tab, `"` qualifier, row 1, the origin detected.
    pub fn import(path: String, bytes: Vec<u8>) -> TextDialog {
        let text = decode(&bytes, Origin::Auto);
        let mut d = TextDialog::with(Purpose::Import { path, bytes }, TextParse::default());
        d.text = text;
        d
    }

    /// The wizard for Text to Columns over `src`, whose cells show `sample`.
    pub fn columns(src: TtcSource, sample: Vec<String>, dest: String) -> TextDialog {
        let mut d = TextDialog::with(Purpose::Columns { src, sample }, TextParse::default());
        d.dest = dest;
        d
    }

    fn with(purpose: Purpose, opts: TextParse) -> TextDialog {
        let (fixed, delims, consecutive, breaks) = match opts.kind {
            SplitKind::Delimited {
                delims,
                consecutive,
            } => (false, delims, consecutive, Vec::new()),
            SplitKind::Fixed { breaks } => (true, Delimiters::only('\t'), false, breaks),
        };
        TextDialog {
            purpose,
            step: 0,
            focus: 0,
            fixed,
            delims,
            consecutive,
            qualifier: opts.qualifier,
            breaks,
            start_row: opts.start_row,
            typed: false,
            origin: Origin::Auto,
            columns: opts.columns,
            decimal: opts.decimal,
            thousands: opts.thousands,
            trailing_minus: opts.trailing_minus,
            dest: String::new(),
            col: 0,
            ruler: 0,
            text: String::new(),
        }
    }

    pub fn is_import(&self) -> bool {
        matches!(self.purpose, Purpose::Import { .. })
    }

    /// The options staged so far.
    pub fn parse(&self) -> TextParse {
        TextParse {
            kind: if self.fixed {
                SplitKind::Fixed {
                    breaks: self.breaks.clone(),
                }
            } else {
                SplitKind::Delimited {
                    delims: self.delims.clone(),
                    consecutive: self.consecutive,
                }
            },
            qualifier: self.qualifier,
            start_row: if self.is_import() {
                self.start_row.max(1)
            } else {
                1
            },
            columns: self.columns.clone(),
            decimal: self.decimal,
            thousands: self.thousands,
            trailing_minus: self.trailing_minus,
        }
    }

    /// The import's text under the chosen origin.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The fields of the current step, top to bottom.
    pub fn fields(&self) -> Vec<Field> {
        use Field::*;
        match self.step {
            0 if self.is_import() => vec![Kind, StartRow, Origin],
            0 => vec![Kind],
            1 if self.fixed => vec![Breaks],
            1 => vec![Tab, Semicolon, Comma, Space, Other, Consecutive, Qualifier],
            _ => {
                let mut v = vec![Column, Format, DateOrder, Decimal, Thousands, TrailingMinus];
                if !self.is_import() {
                    v.push(Destination);
                }
                v
            }
        }
    }

    pub fn focused(&self) -> Field {
        let f = self.fields();
        f[self.focus.min(f.len() - 1)]
    }

    /// The preview records under the staged options.
    pub fn preview(&self) -> Vec<Vec<String>> {
        let opts = self.parse();
        match &self.purpose {
            Purpose::Import { .. } => {
                let mut recs = split_text(&self.text, &opts);
                recs.truncate(PREVIEW_ROWS);
                recs
            }
            Purpose::Columns { sample, .. } => sample
                .iter()
                .take(PREVIEW_ROWS)
                .map(|t| split_value(t, &opts))
                .collect(),
        }
    }

    fn preview_width(&self) -> usize {
        self.preview()
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(1)
            .max(1)
    }

    /// The longest preview line, for the fixed-width ruler.
    fn line_width(&self) -> usize {
        match &self.purpose {
            Purpose::Import { .. } => self
                .text
                .lines()
                .take(PREVIEW_ROWS + self.start_row)
                .map(|l| l.chars().count())
                .max()
                .unwrap_or(0),
            Purpose::Columns { sample, .. } => {
                sample.iter().map(|l| l.chars().count()).max().unwrap_or(0)
            }
        }
    }

    fn format(&self, col: usize) -> ColFormat {
        self.columns.get(col).copied().unwrap_or_default()
    }

    fn set_format(&mut self, col: usize, f: ColFormat) {
        if self.columns.len() <= col {
            self.columns.resize(col + 1, ColFormat::General);
        }
        self.columns[col] = f;
    }

    fn set_origin(&mut self, origin: Origin) {
        self.origin = origin;
        if let Purpose::Import { bytes, .. } = &self.purpose {
            self.text = decode(bytes, origin);
        }
    }

    /// Handle a key.
    pub fn key(&mut self, code: KeyCode) -> Outcome {
        match code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Enter => return Outcome::Finish,
            KeyCode::Tab => self.goto_step(self.step + 1),
            KeyCode::BackTab => self.goto_step(self.step.saturating_sub(1)),
            KeyCode::Up => {
                self.focus = self.focus.saturating_sub(1);
                self.typed = false;
            }
            KeyCode::Down => {
                self.focus = (self.focus + 1).min(self.fields().len() - 1);
                self.typed = false;
            }
            KeyCode::Left => self.change(-1),
            KeyCode::Right => self.change(1),
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Char(c) => self.type_char(c),
            KeyCode::Backspace => self.backspace(),
            _ => {}
        }
        Outcome::Pending
    }

    pub fn goto_step(&mut self, step: usize) {
        self.step = step.min(2);
        self.focus = 0;
        self.typed = false;
        self.col = self.col.min(self.preview_width() - 1);
    }

    fn change(&mut self, by: i32) {
        let step = |i: usize, n: usize| (i as i32 + by).rem_euclid(n as i32) as usize;
        match self.focused() {
            Field::Kind => self.fixed = !self.fixed,
            Field::StartRow => {
                self.start_row = (self.start_row as i64 + by as i64).max(1) as usize;
                self.typed = false;
            }
            Field::Origin => {
                let i = ORIGINS.iter().position(|o| *o == self.origin).unwrap_or(0);
                self.set_origin(ORIGINS[step(i, ORIGINS.len())]);
            }
            Field::Qualifier => {
                let i = QUALIFIERS
                    .iter()
                    .position(|q| *q == self.qualifier)
                    .unwrap_or(0);
                self.qualifier = QUALIFIERS[step(i, QUALIFIERS.len())];
            }
            Field::Breaks => {
                let max = self.line_width().max(1) as i64;
                self.ruler = (self.ruler as i64 + by as i64).clamp(0, max) as usize;
            }
            Field::Column => {
                self.col = step(self.col, self.preview_width());
            }
            Field::Format => {
                const CYCLE: [ColFormat; 4] = [
                    ColFormat::General,
                    ColFormat::Text,
                    ColFormat::Date(DateOrder::Mdy),
                    ColFormat::Skip,
                ];
                let cur = match self.format(self.col) {
                    ColFormat::Date(_) => 2,
                    f => CYCLE.iter().position(|c| *c == f).unwrap_or(0),
                };
                self.set_format(self.col, CYCLE[step(cur, CYCLE.len())]);
            }
            Field::DateOrder => {
                let cur = match self.format(self.col) {
                    ColFormat::Date(o) => DateOrder::ALL.iter().position(|x| *x == o).unwrap_or(0),
                    _ => DateOrder::ALL.len() - 1,
                };
                let o = DateOrder::ALL[step(cur, DateOrder::ALL.len())];
                self.set_format(self.col, ColFormat::Date(o));
            }
            Field::Decimal => {
                let i = SEPARATORS
                    .iter()
                    .position(|c| *c == self.decimal)
                    .unwrap_or(0);
                self.decimal = SEPARATORS[step(i, SEPARATORS.len())];
            }
            Field::Thousands => {
                let i = SEPARATORS
                    .iter()
                    .position(|c| *c == self.thousands)
                    .unwrap_or(0);
                self.thousands = SEPARATORS[step(i, SEPARATORS.len())];
            }
            _ => self.toggle(),
        }
    }

    fn toggle(&mut self) {
        match self.focused() {
            Field::Kind => self.fixed = !self.fixed,
            Field::Tab => self.delims.tab = !self.delims.tab,
            Field::Semicolon => self.delims.semicolon = !self.delims.semicolon,
            Field::Comma => self.delims.comma = !self.delims.comma,
            Field::Space => self.delims.space = !self.delims.space,
            Field::Other => self.delims.other = None,
            Field::Consecutive => self.consecutive = !self.consecutive,
            Field::TrailingMinus => self.trailing_minus = !self.trailing_minus,
            Field::Breaks => {
                if self.ruler > 0 {
                    match self.breaks.iter().position(|&b| b == self.ruler) {
                        Some(i) => {
                            self.breaks.remove(i);
                        }
                        None => {
                            self.breaks.push(self.ruler);
                            self.breaks.sort_unstable();
                        }
                    }
                }
            }
            Field::Destination => self.dest.push(' '),
            _ => self.change(1),
        }
    }

    fn type_char(&mut self, c: char) {
        match self.focused() {
            Field::Other => self.delims.other = Some(c),
            Field::Decimal => self.decimal = c,
            Field::Thousands => self.thousands = c,
            Field::Destination => self.dest.push(c),
            Field::StartRow => {
                if let Some(d) = c.to_digit(10) {
                    let kept = if self.typed { self.start_row } else { 0 };
                    let n = kept.saturating_mul(10).saturating_add(d as usize);
                    self.start_row = n.min(1_048_576);
                    self.typed = true;
                }
            }
            Field::Format | Field::Column => match c.to_ascii_lowercase() {
                'g' => self.set_format(self.col, ColFormat::General),
                't' => self.set_format(self.col, ColFormat::Text),
                'd' => self.set_format(self.col, ColFormat::Date(DateOrder::Mdy)),
                's' => self.set_format(self.col, ColFormat::Skip),
                _ => {}
            },
            _ => {}
        }
    }

    fn backspace(&mut self) {
        match self.focused() {
            Field::Other => self.delims.other = None,
            Field::Destination => {
                self.dest.pop();
            }
            Field::StartRow => {
                self.start_row /= 10;
                self.typed = true;
            }
            _ => {}
        }
    }

    fn field_line(&self, f: Field) -> String {
        let check = |on: bool| if on { "[x]" } else { "[ ]" };
        let sep = |c: char| match c {
            ' ' => "space".to_string(),
            c => format!("'{c}'"),
        };
        match f {
            Field::Kind => format!(
                "Original data type: ({}) Delimited  ({}) Fixed width",
                if self.fixed { " " } else { "o" },
                if self.fixed { "o" } else { " " }
            ),
            Field::StartRow => format!(
                "Start import at row: {}",
                if self.start_row == 0 {
                    String::new()
                } else {
                    self.start_row.to_string()
                }
            ),
            Field::Origin => format!("File origin: {}", self.origin.name()),
            Field::Tab => format!("{} Tab", check(self.delims.tab)),
            Field::Semicolon => format!("{} Semicolon", check(self.delims.semicolon)),
            Field::Comma => format!("{} Comma", check(self.delims.comma)),
            Field::Space => format!("{} Space", check(self.delims.space)),
            Field::Other => format!(
                "{} Other: {}",
                check(self.delims.other.is_some()),
                self.delims.other.map(String::from).unwrap_or_default()
            ),
            Field::Consecutive => {
                format!(
                    "{} Treat consecutive delimiters as one",
                    check(self.consecutive)
                )
            }
            Field::Qualifier => format!(
                "Text qualifier: {}",
                match self.qualifier {
                    Some(q) => q.to_string(),
                    None => "{none}".into(),
                }
            ),
            Field::Breaks => format!(
                "Break lines at: {}  (caret {})",
                if self.breaks.is_empty() {
                    "none".to_string()
                } else {
                    self.breaks
                        .iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                },
                self.ruler
            ),
            Field::Column => format!("Column: {} of {}", self.col + 1, self.preview_width()),
            Field::Format => format!(
                "Column data format: {}",
                match self.format(self.col) {
                    ColFormat::General => "General",
                    ColFormat::Text => "Text",
                    ColFormat::Date(_) => "Date",
                    ColFormat::Skip => "Do not import column (skip)",
                }
            ),
            Field::DateOrder => format!(
                "Date order: {}",
                match self.format(self.col) {
                    ColFormat::Date(o) => o.name(),
                    _ => "-",
                }
            ),
            Field::Decimal => format!("Decimal separator: {}", sep(self.decimal)),
            Field::Thousands => format!("Thousands separator: {}", sep(self.thousands)),
            Field::TrailingMinus => format!(
                "{} Trailing minus for negative numbers",
                check(self.trailing_minus)
            ),
            Field::Destination => format!("Destination: {}", self.dest),
        }
    }

    /// Draw the wizard centred over `area`.
    pub fn draw(&self, f: &mut Frame, area: Rect) {
        let w = 78u16.min(area.width.saturating_sub(2));
        let h = 24u16.min(area.height);
        if w < 30 || h < 12 {
            return;
        }
        let rect = Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        );
        f.render_widget(Clear, rect);
        let title = format!(
            " {} - Step {} of 3 ",
            if self.is_import() {
                "Text Import Wizard"
            } else {
                "Convert Text to Columns Wizard"
            },
            self.step + 1
        );
        let iw = w.saturating_sub(2) as usize;
        let mut lines: Vec<Line> = Vec::new();
        for (i, fld) in self.fields().into_iter().enumerate() {
            let style = if i == self.focus {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            };
            lines.push(Line::from(Span::styled(
                clip(&self.field_line(fld), iw),
                style,
            )));
        }
        lines.push(Line::from(Span::styled(
            "Data preview",
            Style::new().add_modifier(Modifier::BOLD),
        )));
        let preview_h = (h as usize).saturating_sub(lines.len() + 4);
        lines.extend(self.preview_lines(iw, preview_h));
        while lines.len() < (h as usize).saturating_sub(3) {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            clip(
                "Tab next step  Shift+Tab back  Up/Down field  Left/Right/Space change  Enter Finish  Esc Cancel",
                iw,
            ),
            Style::new().fg(Color::DarkGray),
        )));
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::new().fg(Color::Green));
        f.render_widget(Paragraph::new(lines).block(block), rect);
    }

    fn preview_lines(&self, iw: usize, max: usize) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        if max == 0 {
            return out;
        }
        if self.step == 1 && self.fixed {
            // The raw lines under a ruler of break marks and the caret.
            let mut ruler: String = (0..iw)
                .map(|i| if self.breaks.contains(&i) { '|' } else { '.' })
                .collect();
            if self.ruler < iw {
                ruler.replace_range(
                    ruler.char_indices().nth(self.ruler).map_or(0, |(i, _)| i)
                        ..ruler
                            .char_indices()
                            .nth(self.ruler + 1)
                            .map_or(ruler.len(), |(i, _)| i),
                    "^",
                );
            }
            out.push(Line::from(Span::styled(
                ruler,
                Style::new().fg(Color::Yellow),
            )));
            let raw: Vec<String> = match &self.purpose {
                Purpose::Import { .. } => self
                    .text
                    .lines()
                    .skip(self.start_row.saturating_sub(1))
                    .map(str::to_string)
                    .collect(),
                Purpose::Columns { sample, .. } => sample.clone(),
            };
            for l in raw.into_iter().take(max.saturating_sub(1)) {
                out.push(Line::from(clip(&l, iw)));
            }
            return out;
        }
        let recs = self.preview();
        let ncols = self.preview_width();
        let widths: Vec<usize> = (0..ncols)
            .map(|c| {
                recs.iter()
                    .filter_map(|r| r.get(c))
                    .map(|s| s.chars().count())
                    .max()
                    .unwrap_or(0)
                    .clamp(4, 14)
            })
            .collect();
        let cell_style = |c: usize| {
            if self.step == 2 && c == self.col {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            }
        };
        if self.step == 2 {
            let spans: Vec<Span> = (0..ncols)
                .map(|c| {
                    let name = match self.format(c) {
                        ColFormat::General => "General".to_string(),
                        ColFormat::Text => "Text".to_string(),
                        ColFormat::Date(o) => o.name().to_string(),
                        ColFormat::Skip => "Skip".to_string(),
                    };
                    Span::styled(pad(&name, widths[c] + 1), cell_style(c).fg(Color::Cyan))
                })
                .collect();
            out.push(Line::from(spans));
        }
        for rec in recs.iter().take(max.saturating_sub(out.len())) {
            let spans: Vec<Span> = (0..ncols)
                .map(|c| {
                    let v = rec.get(c).map(String::as_str).unwrap_or("");
                    Span::styled(pad(v, widths[c] + 1), cell_style(c))
                })
                .collect();
            out.push(Line::from(spans));
        }
        out
    }
}

fn clip(s: &str, w: usize) -> String {
    s.chars().take(w).collect()
}

fn pad(s: &str, w: usize) -> String {
    let body: String = s
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .take(w.saturating_sub(1))
        .collect();
    format!("{body:<w$}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wizard(text: &str) -> TextDialog {
        TextDialog::import("in.txt".into(), text.as_bytes().to_vec())
    }

    #[test]
    fn the_wizard_stages_excels_three_steps() {
        let mut d = wizard("02134\t03/04/2024\t1.234,5-\tx\n");
        assert_eq!(d.fields(), [Field::Kind, Field::StartRow, Field::Origin]);
        d.key(KeyCode::Tab);
        assert_eq!(d.focused(), Field::Tab);
        d.key(KeyCode::Tab);
        assert_eq!(d.focused(), Field::Column);
        assert_eq!(d.preview_width(), 4);
        // Column 1: Text; column 2: Date DMY; column 4: skip.
        d.key(KeyCode::Char('t'));
        d.key(KeyCode::Right);
        d.key(KeyCode::Down);
        d.key(KeyCode::Right); // Format: General -> Text
        d.key(KeyCode::Right); // Text -> Date
        d.key(KeyCode::Down);
        d.key(KeyCode::Right); // MDY -> DMY
        d.key(KeyCode::Up);
        d.key(KeyCode::Up);
        d.key(KeyCode::Right);
        d.key(KeyCode::Right); // column 4
        d.key(KeyCode::Char('s'));
        d.key(KeyCode::Down);
        d.key(KeyCode::Down);
        d.key(KeyCode::Down);
        d.key(KeyCode::Char(','));
        d.key(KeyCode::Down);
        d.key(KeyCode::Char('.'));
        let opts = d.parse();
        assert_eq!(
            opts.columns,
            [
                ColFormat::Text,
                ColFormat::Date(DateOrder::Dmy),
                ColFormat::General,
                ColFormat::Skip
            ]
        );
        assert_eq!((opts.decimal, opts.thousands), (',', '.'));
        assert!(opts.trailing_minus);
        assert_eq!(d.key(KeyCode::Enter), Outcome::Finish);
        assert_eq!(d.key(KeyCode::Esc), Outcome::Cancel);
    }

    #[test]
    fn step_two_sets_delimiters_or_break_lines() {
        let mut d = wizard("a,b;c\n");
        d.goto_step(1);
        d.key(KeyCode::Char(' ')); // untick Tab
        d.key(KeyCode::Down);
        d.key(KeyCode::Char(' ')); // Semicolon
        d.key(KeyCode::Down);
        d.key(KeyCode::Char(' ')); // Comma
        assert_eq!(d.preview(), [vec!["a", "b", "c"]]);
        d.key(KeyCode::Down);
        d.key(KeyCode::Down);
        d.key(KeyCode::Char('|')); // Other
        assert_eq!(d.delims.other, Some('|'));
        d.key(KeyCode::Down);
        d.key(KeyCode::Down);
        d.key(KeyCode::Right); // qualifier " -> '
        assert_eq!(d.qualifier, Some('\''));
        // Fixed width: breaks at the caret.
        let mut d = wizard("ABC12xyz\n");
        d.key(KeyCode::Right); // Delimited -> Fixed
        assert!(d.fixed);
        d.goto_step(1);
        for _ in 0..3 {
            d.key(KeyCode::Right);
        }
        d.key(KeyCode::Char(' '));
        d.key(KeyCode::Right);
        d.key(KeyCode::Right);
        d.key(KeyCode::Char(' '));
        assert_eq!(d.breaks, [3, 5]);
        assert_eq!(d.preview(), [vec!["ABC", "12", "xyz"]]);
        d.key(KeyCode::Char(' ')); // clears the break under the caret
        assert_eq!(d.breaks, [3]);
    }

    #[test]
    fn step_one_sets_start_row_and_origin() {
        let mut d = TextDialog::import("in.txt".into(), b"h\n\xFCber\n".to_vec());
        d.key(KeyCode::Down);
        d.key(KeyCode::Right);
        assert_eq!(d.parse().start_row, 2);
        assert_eq!(d.preview(), [vec!["\u{fc}ber"]]);
        d.key(KeyCode::Down);
        d.key(KeyCode::Right); // Auto -> UTF-8 (lossy)
        assert_eq!(d.origin, Origin::Utf8);
        assert_eq!(d.preview(), [vec!["\u{fffd}ber"]]);
        d.key(KeyCode::Right);
        d.key(KeyCode::Right); // -> Windows-1252
        assert_eq!(d.preview(), [vec!["\u{fc}ber"]]);
    }

    /// The first digit typed replaces the start row; Backspace can empty it.
    #[test]
    fn typing_a_start_row_replaces_it() {
        let mut d = wizard("a\nb\nc\nd\n");
        d.key(KeyCode::Down); // Start import at row
        d.key(KeyCode::Char('3'));
        assert_eq!(d.parse().start_row, 3);
        d.key(KeyCode::Char('1'));
        assert_eq!(d.parse().start_row, 31);
        d.key(KeyCode::Backspace);
        d.key(KeyCode::Backspace);
        assert_eq!(d.start_row, 0);
        // An emptied field still imports from row 1.
        assert_eq!(d.parse().start_row, 1);
        d.key(KeyCode::Char('2'));
        assert_eq!(d.parse().start_row, 2);
        assert_eq!(d.preview(), [vec!["b"], vec!["c"], vec!["d"]]);
        // Leaving and coming back: the next digit replaces again.
        d.key(KeyCode::Up);
        d.key(KeyCode::Down);
        d.key(KeyCode::Char('4'));
        assert_eq!(d.parse().start_row, 4);
    }

    #[test]
    fn text_to_columns_has_no_import_fields_and_a_destination() {
        let src = TtcSource::new(0, (0, 0, 1, 0)).unwrap();
        let mut d = TextDialog::columns(src, vec!["a,b".into(), "c".into()], "A1".into());
        assert_eq!(d.fields(), [Field::Kind]);
        d.goto_step(2);
        assert_eq!(d.fields().last(), Some(&Field::Destination));
        for _ in 0..6 {
            d.key(KeyCode::Down);
        }
        d.key(KeyCode::Backspace);
        d.key(KeyCode::Backspace);
        d.key(KeyCode::Char('C'));
        d.key(KeyCode::Char('3'));
        assert_eq!(d.dest, "C3");
        assert_eq!(d.parse().start_row, 1);
    }

    #[test]
    fn the_wizard_renders_its_step_and_preview() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut d = wizard("name\tqty\nPen\t4\n");
        let mut term = Terminal::new(TestBackend::new(90, 26)).unwrap();
        let draw = |d: &TextDialog, term: &mut Terminal<TestBackend>| {
            term.draw(|f| d.draw(f, f.area())).unwrap();
            let buf = term.backend().buffer();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let text = draw(&d, &mut term);
        assert!(text.contains("Text Import Wizard - Step 1 of 3"), "{text}");
        assert!(text.contains("Delimited"), "{text}");
        d.goto_step(2);
        let text = draw(&d, &mut term);
        assert!(text.contains("Step 3 of 3"), "{text}");
        assert!(text.contains("General"), "{text}");
        assert!(text.contains("Pen"), "{text}");
        d.fixed = true;
        d.goto_step(1);
        let text = draw(&d, &mut term);
        assert!(text.contains("Break lines at: none"), "{text}");
    }
}
