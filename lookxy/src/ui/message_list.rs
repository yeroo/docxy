//! The middle message-list pane: sender, subject, received time, and
//! flag/attachment markers for every message in the selected folder, newest
//! first (as `Store::messages_in_folder` already orders them). Unread
//! messages render bold.

use crate::app::{App, Pane};
use crate::ui::{border_style, centered_rect, truncate_width};

use mailcore::store::MessageRow;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::List;
    draw_list(f, area, "Messages", focused, &app.messages, app.msg_index);
}

/// Renders a titled, bordered list of `messages` with `selected` highlighted
/// — the row widget shared by the normal folder-view message list
/// (`draw`, above) and the search-results view (`ui::search::draw`), so a
/// message row looks and behaves identically in both places. Bounds-safe:
/// `selected` is clamped into range (or left unselected on an empty list)
/// rather than indexed directly, so `ListState::select` can never panic.
pub(crate) fn draw_list(
    f: &mut Frame,
    area: Rect,
    title: &str,
    focused: bool,
    messages: &[MessageRow],
    selected: usize,
) {
    let block = Block::default()
        .title(title.to_string())
        .borders(Borders::ALL)
        .border_style(border_style(focused));

    // Inner width available for text once the left/right borders are taken
    // out, so long senders/subjects truncate instead of wrapping/overrunning.
    let inner_width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = messages
        .iter()
        .map(|m| ListItem::new(line(m, inner_width)))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().bg(Color::Blue).fg(Color::White));

    let mut state = ListState::default();
    if !messages.is_empty() {
        state.select(Some(selected.min(messages.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// One row: flag/attachment markers, a shortened received time, then
/// "sender — subject", truncated to `width` display columns. Unread messages
/// render bold.
fn line(m: &MessageRow, width: usize) -> Line<'static> {
    let flagged = if m.is_flagged { "!" } else { " " };
    let attached = if m.has_attachments { "@" } else { " " };
    let time = short_time(&m.received_at);
    let text = format!(
        "{flagged}{attached} {time}  {} — {}",
        m.from_name, m.subject
    );
    let truncated = truncate_width(&text, width);

    let mut style = Style::default();
    if !m.is_read {
        style = style.add_modifier(Modifier::BOLD);
    }
    Line::from(Span::styled(truncated, style))
}

/// Renders the move-folder popup (`v`) as a centered overlay, when
/// `app.move_picker` is open; a no-op otherwise. Drawn last by `ui::draw` so
/// it sits on top of the three panes and status bar. `Clear` wipes whatever
/// was already rendered under the popup's area first, so folder names don't
/// bleed through from the message list underneath. Bounds-safe on an empty
/// folder list (can't normally happen — `App::open_move_picker` refuses to
/// open the popup then — but `ListState::select` is left unset rather than
/// indexing `folders[0]` regardless).
pub fn draw_move_picker(f: &mut Frame, app: &App) {
    let Some(picker) = &app.move_picker else {
        return;
    };

    let area = centered_rect(50, 40, f.area());
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = picker
        .folders
        .iter()
        .map(|folder| ListItem::new(folder.display_name.clone()))
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .title("Move to…")
                .borders(Borders::ALL)
                .border_style(Style::new().fg(Color::Yellow)),
        )
        .highlight_style(Style::new().bg(Color::Blue).fg(Color::White));

    let mut state = ListState::default();
    if !picker.folders.is_empty() {
        state.select(Some(picker.index));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// Shortens an ISO-8601 `receivedDateTime` (`2026-07-16T10:00:00Z`) down to
/// `MM-DD HH:MM` for the list column; falls back to the raw string if it
/// doesn't have both slices (e.g. empty, in a test fixture, or — despite
/// Graph's `receivedDateTime` always being plain ASCII — any stray non-ASCII
/// byte that would otherwise land mid-character). `str::get` checks UTF-8
/// char boundaries as well as bounds, so this can never panic the way a
/// direct `&received_at[5..10]` byte-slice could.
fn short_time(received_at: &str) -> String {
    match (received_at.get(5..10), received_at.get(11..16)) {
        (Some(date), Some(time)) => format!("{date} {time}"),
        _ => received_at.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use mailcore::graph::model::MailFolder;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn move_picker_overlay_renders_folder_names_when_open() {
        let mut app = App::for_test_with_seeded_store();
        app.store
            .upsert_folder(&MailFolder {
                id: "archive".into(),
                display_name: "Archive".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some("archive".into()),
            })
            .expect("seed archive folder");
        app.open_move_picker();
        assert!(app.move_picker.is_some());

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw_move_picker(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Archive"));
        assert!(text.contains("Move to"));
    }

    #[test]
    fn move_picker_overlay_draws_nothing_when_closed() {
        let app = App::for_test_with_seeded_store();
        assert!(app.move_picker.is_none());

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw_move_picker(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(!text.contains("Move to"));
    }
}
