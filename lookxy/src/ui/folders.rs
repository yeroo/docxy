//! The left folder-tree pane: every folder in the store, well-known ones
//! pre-ranked (Inbox, Drafts, Sent, ...) by `Store::folders` itself, each
//! annotated with its unread count.

use crate::app::{App, Pane};
use crate::ui::border_style;

use mailcore::store::FolderRow;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::Folders;
    let block = Block::default()
        .title("Folders")
        .borders(Borders::ALL)
        .border_style(border_style(focused));

    let items: Vec<ListItem> = app.folders.iter().map(|f| ListItem::new(line(f))).collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().bg(Color::Blue).fg(Color::White));

    let mut state = ListState::default();
    if !app.folders.is_empty() {
        state.select(Some(app.folder_index));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// `"Inbox (3)"` when there's unread mail, else just `"Inbox"`.
fn line(f: &FolderRow) -> String {
    if f.unread_count > 0 {
        format!("{} ({})", f.display_name, f.unread_count)
    } else {
        f.display_name.clone()
    }
}
