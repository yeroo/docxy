//! Shared no-file start dialog: a centered accent card rendering an
//! app-supplied item list, returning the chosen index. Self-contained — no
//! dependency on the backstage types.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// One selectable choice on the start card.
pub struct StartItem {
    pub label: String,
    pub desc: Option<String>,
}

/// Outcome of feeding a key/mouse event to a [`Start`] dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartEvent {
    None,
    Choose(usize),
    Quit,
}

/// A centered accent card offering an app-supplied list of choices.
pub struct Start {
    title: String,
    items: Vec<StartItem>,
    sel: usize,
    accent: Color,
    btns: Vec<Rect>,
}

impl Start {
    pub fn new(title: impl Into<String>, items: Vec<StartItem>, accent: Color) -> Start {
        Start {
            title: title.into(),
            items,
            sel: 0,
            accent,
            btns: Vec::new(),
        }
    }

    pub fn sel(&self) -> usize {
        self.sel
    }

    /// Handle a key while the start card is shown.
    pub fn key(&mut self, key: KeyEvent) -> StartEvent {
        match key.code {
            KeyCode::Up => {
                self.sel = self.sel.saturating_sub(1);
                StartEvent::None
            }
            KeyCode::Down => {
                self.sel = (self.sel + 1).min(self.items.len().saturating_sub(1));
                StartEvent::None
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = c as usize - '1' as usize;
                if idx < self.items.len() {
                    self.sel = idx;
                    StartEvent::Choose(idx)
                } else {
                    StartEvent::None
                }
            }
            KeyCode::Enter => StartEvent::Choose(self.sel),
            KeyCode::Esc | KeyCode::Char('q') => StartEvent::Quit,
            _ => StartEvent::None,
        }
    }

    /// Handle a mouse click at `(x, y)` (absolute frame coordinates).
    pub fn mouse(&mut self, x: u16, y: u16) -> StartEvent {
        let pos = Position { x, y };
        for (i, rect) in self.btns.iter().enumerate() {
            if rect.contains(pos) {
                self.sel = i;
                return StartEvent::Choose(i);
            }
        }
        StartEvent::None
    }

    /// Render the centered start card into `area`.
    pub fn draw(&mut self, f: &mut Frame, area: Rect) {
        self.btns.clear();

        let card_w = 50u16.min(area.width);
        let card_h = (self.items.len() as u16 + 4).min(area.height).max(4);
        let card = Rect {
            x: area.x + area.width.saturating_sub(card_w) / 2,
            y: area.y + area.height.saturating_sub(card_h) / 2,
            width: card_w,
            height: card_h,
        };

        let accent_style = Style::default().fg(self.accent);
        let rev = Style::default().add_modifier(Modifier::REVERSED);
        let dim = Style::default().add_modifier(Modifier::DIM);

        f.render_widget(Clear, card);
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_style(accent_style)
                .title(format!(" {} ", self.title)),
            card,
        );

        let inner = Rect {
            x: card.x + 1,
            y: card.y + 1,
            width: card.width.saturating_sub(2),
            height: card.height.saturating_sub(2),
        };
        let rows = Layout::vertical([
            Constraint::Length(self.items.len() as u16),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);

        for (i, item) in self.items.iter().enumerate() {
            let row = Rect {
                x: rows[0].x,
                y: rows[0].y + i as u16,
                width: rows[0].width,
                height: 1,
            };
            if row.y < inner.y + inner.height {
                self.btns.push(row);
            }
            let style = if i == self.sel { rev } else { Style::default() };
            let mut spans = vec![Span::styled(format!(" {}. {}", i + 1, item.label), style)];
            if let Some(desc) = &item.desc {
                spans.push(Span::styled(format!("  {desc}"), dim));
            }
            f.render_widget(Paragraph::new(Line::from(spans)), row);
        }

        let footer = format!("1..{} to pick · ↑↓ · Enter · q quits", self.items.len());
        f.render_widget(
            Paragraph::new(footer)
                .alignment(Alignment::Center)
                .style(dim),
            rows[2],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};
    use ratatui::style::Color;

    fn items() -> Vec<StartItem> {
        vec![
            StartItem {
                label: "New".into(),
                desc: None,
            },
            StartItem {
                label: "Open".into(),
                desc: Some("browse".into()),
            },
            StartItem {
                label: "Quit".into(),
                desc: None,
            },
        ]
    }
    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::from(c)
    }

    #[test]
    fn number_key_chooses_that_index() {
        let mut s = Start::new("xlsxy", items(), Color::Green);
        assert!(matches!(
            s.key(key(KeyCode::Char('2'))),
            StartEvent::Choose(1)
        ));
    }

    #[test]
    fn arrows_then_enter_choose_selection() {
        let mut s = Start::new("xlsxy", items(), Color::Green);
        s.key(key(KeyCode::Down));
        s.key(key(KeyCode::Down));
        assert_eq!(s.sel(), 2);
        assert!(matches!(s.key(key(KeyCode::Enter)), StartEvent::Choose(2)));
    }

    #[test]
    fn esc_and_q_quit() {
        let mut s = Start::new("xlsxy", items(), Color::Green);
        assert!(matches!(s.key(key(KeyCode::Esc)), StartEvent::Quit));
        assert!(matches!(s.key(key(KeyCode::Char('q'))), StartEvent::Quit));
    }

    #[test]
    fn draws_without_panic() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut s = Start::new("xlsxy", items(), Color::Green);
        term.draw(|f| {
            let a = f.area();
            s.draw(f, a);
        })
        .unwrap();
    }
}
