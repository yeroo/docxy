//! The level-0 rail: a narrow leftmost column with two labels — `M` Mail and
//! 📅 Calendar — centered, the active section highlighted. Selecting a section
//! here (rail Up/Down) is the only way to switch Mail⇄Calendar now that `g` is
//! gone.

use crate::app::{App, Mode, Pane};
use crate::ui::border_style;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem};

/// Width of the rail column (2 border cells + a 3-wide label cell).
pub const WIDTH: u16 = 5;

/// Centers `label` (of display width `w`) within the `inner` columns, so the
/// letter/emoji sits in the middle of the rail rather than hugging the border.
fn centered(label: &str, w: usize, inner: usize) -> String {
    let pad = inner.saturating_sub(w);
    let left = pad / 2;
    format!("{}{}{}", " ".repeat(left), label, " ".repeat(pad - left))
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::Rail;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused));
    let inner = area.width.saturating_sub(2) as usize;

    // `M` Mail, 📅 Calendar (label, display width). Highlight the active section.
    let rows = [(Mode::Mail, "M", 1usize), (Mode::Calendar, "\u{1f4c5}", 2)];
    let items: Vec<ListItem> = rows
        .iter()
        .map(|(mode, label, w)| {
            let style = if *mode == app.mode {
                Style::new()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::new().add_modifier(Modifier::DIM)
            };
            ListItem::new(Line::styled(centered(label, *w, inner), style))
        })
        .collect();

    f.render_widget(List::new(items).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn centers_labels_within_the_column() {
        assert_eq!(centered("M", 1, 3), " M ");
        assert_eq!(centered("\u{1f4c5}", 2, 3), "\u{1f4c5} ");
    }

    #[test]
    fn draws_both_section_labels() {
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
        assert!(text.contains('M'));
        assert!(text.contains('\u{1f4c5}')); // 📅
    }
}
