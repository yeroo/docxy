//! The middle message-list pane: sender, subject, received time, and
//! flag/attachment markers for every message in the selected folder, newest
//! first (as `Store::messages_in_folder` already orders them). Unread
//! messages render bold.

use crate::app::{App, Pane};
use crate::ui::{border_style, truncate_width};

use mailcore::store::MessageRow;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::List;
    let block = Block::default()
        .title("Messages")
        .borders(Borders::ALL)
        .border_style(border_style(focused));

    // Inner width available for text once the left/right borders are taken
    // out, so long senders/subjects truncate instead of wrapping/overrunning.
    let inner_width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = app
        .messages
        .iter()
        .map(|m| ListItem::new(line(m, inner_width)))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().bg(Color::Blue).fg(Color::White));

    let mut state = ListState::default();
    if !app.messages.is_empty() {
        state.select(Some(app.msg_index));
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
    let text = format!("{flagged}{attached} {time}  {} — {}", m.from_name, m.subject);
    let truncated = truncate_width(&text, width);

    let mut style = Style::default();
    if !m.is_read {
        style = style.add_modifier(Modifier::BOLD);
    }
    Line::from(Span::styled(truncated, style))
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
