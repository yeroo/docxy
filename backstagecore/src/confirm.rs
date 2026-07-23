//! A shared modal Yes/No confirmation dialog, used by every app for the
//! exit-confirm (and any other yes/no prompt). Ported from docxy's original
//! `Confirm` + `draw_confirm`/`confirm_key`/`confirm_mouse`; generalized over
//! the action carried on Yes and parameterized by the app's accent colour.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Alignment, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

/// What feeding an event to a [`Confirm`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmOutcome<A> {
    /// Still open (a no-op key, or the selection was toggled).
    Pending,
    /// Yes was chosen — run `A`.
    Confirmed(A),
    /// No / Esc — dismiss without acting.
    Cancelled,
}

/// A modal Yes/No dialog carrying the action to run when Yes is chosen.
pub struct Confirm<A> {
    prompt: String,
    /// The selected button: true = Yes, false = No.
    yes: bool,
    action: A,
    accent: Color,
    /// `[yes, no]` button rects, recorded by [`Confirm::draw`] for `mouse`.
    btns: [Rect; 2],
}

impl<A: Clone> Confirm<A> {
    /// A new dialog with Yes preselected.
    pub fn new(prompt: impl Into<String>, action: A, accent: Color) -> Confirm<A> {
        Confirm {
            prompt: prompt.into(),
            yes: true,
            action,
            accent,
            btns: [Rect::ZERO; 2],
        }
    }

    /// Whether Yes is currently selected (for tests / cursor placement).
    pub fn yes_selected(&self) -> bool {
        self.yes
    }

    /// Keys: ←/→/Tab move between Yes/No, y/n choose directly, Enter confirms
    /// the selection, Esc cancels.
    pub fn key(&mut self, key: KeyEvent) -> ConfirmOutcome<A> {
        match key.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                self.yes = !self.yes;
                ConfirmOutcome::Pending
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                ConfirmOutcome::Confirmed(self.action.clone())
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => ConfirmOutcome::Cancelled,
            KeyCode::Enter => {
                if self.yes {
                    ConfirmOutcome::Confirmed(self.action.clone())
                } else {
                    ConfirmOutcome::Cancelled
                }
            }
            _ => ConfirmOutcome::Pending,
        }
    }

    /// A click on the Yes button confirms; a click on No cancels; elsewhere is a
    /// no-op. Reads the rects recorded by the last [`Confirm::draw`].
    pub fn mouse(&mut self, x: u16, y: u16) -> ConfirmOutcome<A> {
        let p = Position { x, y };
        if self.btns[0].contains(p) {
            return ConfirmOutcome::Confirmed(self.action.clone());
        }
        if self.btns[1].contains(p) {
            return ConfirmOutcome::Cancelled;
        }
        ConfirmOutcome::Pending
    }

    /// Render the centered modal into `area` and record the button rects.
    pub fn draw(&mut self, f: &mut Frame, area: Rect) {
        let w = ((self.prompt.chars().count() as u16) + 6)
            .clamp(28, area.width.saturating_sub(4).max(28))
            .min(area.width);
        let h = 7u16.min(area.height);
        let rect = Rect {
            x: area.x + area.width.saturating_sub(w) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.accent))
            .title(" Confirm ");
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        // prompt, centered above the buttons
        let prompt_area = Rect {
            height: inner.height.saturating_sub(2),
            ..inner
        };
        f.render_widget(
            Paragraph::new(self.prompt.clone())
                .wrap(Wrap { trim: true })
                .alignment(Alignment::Center),
            prompt_area,
        );

        // [ Yes ] [ No ] buttons, centered on the bottom row
        let yes_lbl = "  Yes  ";
        let no_lbl = "  No  ";
        let (yw, nw) = (yes_lbl.len() as u16, no_lbl.len() as u16);
        let total = yw + 2 + nw;
        let bx = inner.x + inner.width.saturating_sub(total) / 2;
        let by = inner.y + inner.height.saturating_sub(1);
        let yes_rect = Rect {
            x: bx,
            y: by,
            width: yw,
            height: 1,
        };
        let no_rect = Rect {
            x: bx + yw + 2,
            y: by,
            width: nw,
            height: 1,
        };
        let sel = Style::default().fg(Color::Black).bg(self.accent);
        let unsel = Style::default().add_modifier(Modifier::REVERSED);
        f.render_widget(
            Paragraph::new(yes_lbl).style(if self.yes { sel } else { unsel }),
            yes_rect,
        );
        f.render_widget(
            Paragraph::new(no_lbl).style(if self.yes { unsel } else { sel }),
            no_rect,
        );
        self.btns = [yes_rect, no_rect];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use ratatui::{Terminal, backend::TestBackend};

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Act {
        Exit,
        Other,
    }
    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::from(c)
    }

    #[test]
    fn enter_on_yes_confirms_esc_and_n_cancel() {
        let mut c = Confirm::new("Exit?", Act::Exit, Color::Cyan);
        assert!(c.yes_selected(), "Yes is preselected");
        assert_eq!(
            c.key(key(KeyCode::Enter)),
            ConfirmOutcome::Confirmed(Act::Exit)
        );
        let mut c = Confirm::new("Exit?", Act::Exit, Color::Cyan);
        assert_eq!(c.key(key(KeyCode::Esc)), ConfirmOutcome::Cancelled);
        let mut c = Confirm::new("Exit?", Act::Exit, Color::Cyan);
        assert_eq!(c.key(key(KeyCode::Char('n'))), ConfirmOutcome::Cancelled);
    }

    #[test]
    fn arrows_toggle_then_enter_cancels_on_no() {
        let mut c = Confirm::new("Exit?", Act::Exit, Color::Cyan);
        assert_eq!(c.key(key(KeyCode::Left)), ConfirmOutcome::Pending);
        assert!(!c.yes_selected(), "toggled to No");
        assert_eq!(c.key(key(KeyCode::Enter)), ConfirmOutcome::Cancelled);
    }

    #[test]
    fn y_confirms_directly() {
        let mut c = Confirm::new("Exit?", Act::Other, Color::Green);
        assert_eq!(
            c.key(key(KeyCode::Char('y'))),
            ConfirmOutcome::Confirmed(Act::Other)
        );
    }

    #[test]
    fn clicking_yes_confirms_and_no_cancels() {
        let mut c = Confirm::new("Exit lookxy?", Act::Exit, Color::Yellow);
        let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
        term.draw(|f| {
            let a = f.area();
            c.draw(f, a);
        })
        .unwrap();
        // The recorded button rects are hit-tested by mouse().
        let yes = c.btns[0];
        let no = c.btns[1];
        assert_eq!(c.mouse(yes.x, yes.y), ConfirmOutcome::Confirmed(Act::Exit));
        assert_eq!(c.mouse(no.x, no.y), ConfirmOutcome::Cancelled);
        assert_eq!(c.mouse(0, 0), ConfirmOutcome::Pending);
    }
}
