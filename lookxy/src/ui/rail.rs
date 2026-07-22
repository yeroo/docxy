//! The level-0 rail: a narrow leftmost column with two icons — ✉ Mail and
//! 📅 Calendar — the active section highlighted. Selecting a section here (rail
//! Up/Down) is the only way to switch Mail⇄Calendar now that `g` is gone.

use crate::app::{App, Mode, Pane};
use crate::ui::border_style;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};

/// Width of the rail column (border + a couple of glyph cells).
pub const WIDTH: u16 = 5;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::Rail;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused));

    // ✉ Mail, 📅 Calendar. Highlight the active section.
    let rows = [(Mode::Mail, "\u{2709}"), (Mode::Calendar, "\u{1f4c5}")];
    let items: Vec<ListItem> = rows
        .iter()
        .map(|(mode, icon)| {
            let style = if *mode == app.mode {
                Style::new()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::new().add_modifier(Modifier::DIM)
            };
            ListItem::new(Line::styled(format!(" {icon}"), style))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(match app.mode {
        Mode::Mail => 0,
        Mode::Calendar => 1,
    }));
    f.render_stateful_widget(List::new(items).block(block), area, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn draws_both_section_icons() {
        let app = App::for_test_with_seeded_store();
        let mut term = Terminal::new(TestBackend::new(5, 10)).unwrap();
        term.draw(|f| draw(f, &app, f.area())).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains('\u{2709}')); // ✉
        assert!(text.contains('\u{1f4c5}')); // 📅
    }
}
