//! The attachments popup (`a`): lists `store.attachments(id)` — name,
//! content type, and size — as a centered overlay, the same overlay shape as
//! the move-folder popup (`ui::message_list::draw_move_picker`). Enter saves
//! the highlighted attachment to the Downloads directory
//! (`App::save_attachment`); `o` saves then opens it with the OS handler
//! (`App::save_and_open_attachment`); Esc closes the popup.

use crate::app::App;
use crate::ui::centered_rect;

use mailcore::graph::model::AttachmentMeta;

use ratatui::Frame;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

/// Renders the attachments popup when `app.attachments` is open; a no-op
/// otherwise. Drawn last (over the full frame) by `ui::draw`, same as the
/// move-folder popup. Bounds-safe on an empty attachment list (can't
/// normally happen — `App::open_attachments_popup` refuses to open the
/// popup with nothing in it — but `ListState::select` is left unset rather
/// than indexing `items[0]` regardless).
pub fn draw(f: &mut Frame, app: &App) {
    let Some(popup) = &app.attachments else {
        return;
    };

    let area = centered_rect(60, 40, f.area());
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = popup.items.iter().map(|a| ListItem::new(line(a))).collect();
    let list = List::new(items)
        .block(
            Block::default()
                .title("Attachments (Enter: save, o: save+open)")
                .borders(Borders::ALL)
                .border_style(Style::new().fg(Color::Yellow)),
        )
        .highlight_style(Style::new().bg(Color::Blue).fg(Color::White));

    let mut state = ListState::default();
    if !popup.items.is_empty() {
        state.select(Some(popup.index.min(popup.items.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// One row: `name  (content-type, N.N KB)`.
fn line(a: &AttachmentMeta) -> String {
    let kb = a.size as f64 / 1024.0;
    format!("{}  ({}, {kb:.1} KB)", a.name, a.content_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcore::graph::model::AttachmentMeta;
    use ratatui::{Terminal, backend::TestBackend};

    fn seed_attachment(app: &mut App) {
        app.store
            .put_attachments(
                "m1",
                &[AttachmentMeta {
                    id: "a1".into(),
                    name: "budget.xlsx".into(),
                    content_type: "application/vnd.ms-excel".into(),
                    size: 2048,
                    is_inline: false,
                }],
            )
            .expect("seed attachment");
    }

    #[test]
    fn popup_overlay_renders_attachment_names_when_open() {
        let mut app = App::for_test_with_seeded_store();
        seed_attachment(&mut app);
        app.open_attachments_popup();
        assert!(app.attachments.is_some());

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("budget.xlsx"));
        assert!(text.contains("2.0 KB"));
    }

    #[test]
    fn popup_overlay_draws_nothing_when_closed() {
        let app = App::for_test_with_seeded_store();
        assert!(app.attachments.is_none());

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(!text.contains("Attachments"));
    }

    #[test]
    fn does_not_open_when_the_highlighted_message_has_no_attachments() {
        let mut app = App::for_test_with_seeded_store();
        app.open_attachments_popup();
        assert!(app.attachments.is_none());
    }
}
