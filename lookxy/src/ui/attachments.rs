//! The attachments popup (`a`): lists `store.attachments(id)` — name,
//! content type, and size — as a centered overlay, the same overlay shape as
//! the move-folder popup (`ui::message_list::draw_move_picker`). While
//! metadata is still being fetched (`App::open_attachments_popup`'s
//! `SyncCommand::FetchAttachments` path, for a message whose attachments
//! haven't been pulled from Graph yet), shows a "Loading…" placeholder
//! instead of an empty list. Enter saves the highlighted attachment to the
//! Downloads directory (`App::save_attachment`); `o` saves then opens it
//! with the OS handler (`App::save_and_open_attachment`); Esc closes the
//! popup.

use crate::app::App;
use crate::ui::centered_rect;

use mailcore::graph::model::AttachmentMeta;

use ratatui::Frame;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

/// Renders the attachments popup when `app.attachments` is open; a no-op
/// otherwise. Drawn last (over the full frame) by `ui::draw`, same as the
/// move-folder popup. Bounds-safe on an empty attachment list (can't
/// normally happen for a non-loading popup — `App::open_attachments_popup`
/// refuses to open one with nothing in it and no fetch in flight — but
/// `ListState::select` is left unset rather than indexing `items[0]`
/// regardless).
pub fn draw(f: &mut Frame, app: &App) {
    let Some(popup) = &app.attachments else {
        return;
    };

    let area = centered_rect(60, 40, f.area());
    f.render_widget(Clear, area);

    if popup.loading && popup.items.is_empty() {
        let block = Block::default()
            .title("Attachments")
            .borders(Borders::ALL)
            .border_style(Style::new().fg(Color::Yellow));
        f.render_widget(Paragraph::new("Loading attachments…").block(block), area);
        return;
    }

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
    use mailcore::graph::model::{AttachmentKind, AttachmentMeta};
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
                    content_id: None,
                    kind: AttachmentKind::File,
                    source_url: None,
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

    #[test]
    fn popup_shows_a_loading_placeholder_while_metadata_fetch_is_in_flight() {
        use mailcore::graph::model::{Message, Recipient};

        let mut app = App::for_test_with_seeded_store();
        // "m1" has `has_attachments = true` but no local rows yet, so
        // `open_attachments_popup` opens a loading popup and fires
        // `SyncCommand::FetchAttachments` instead of no-op'ing.
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "m1".into(),
                    conversation_id: "c1".into(),
                    subject: "Hello".into(),
                    from: Recipient {
                        name: "Alice".into(),
                        address: "alice@example.com".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-16T10:00:00Z".into(),
                    sent: "2026-07-16T09:00:00Z".into(),
                    is_read: false,
                    is_flagged: false,
                    has_attachments: true,
                    importance: "normal".into(),
                    preview: "hi there".into(),
                    is_draft: false,
                },
            )
            .expect("update message to has_attachments=true");
        app.reload_messages();

        app.open_attachments_popup();
        assert!(app.attachments.as_ref().unwrap().loading);

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Loading"));
    }
}
