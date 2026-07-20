//! The left folder-tree pane: the store's folders rendered as a collapsible
//! tree (`App::visible_folders` — well-known ones pre-ranked at the top level,
//! children indented under their parent), each annotated with a chevron and its
//! unread count.

use crate::app::{App, Pane};
use crate::ui::border_style;
use crate::ui::foldertree::VisibleFolder;

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

    let items: Vec<ListItem> = app
        .visible_folders
        .iter()
        .map(|v| ListItem::new(line(v)))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().bg(Color::Blue).fg(Color::White));

    let mut state = ListState::default();
    if !app.visible_folders.is_empty() {
        state.select(Some(app.folder_index));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// `"  ▾ Inbox (3)"` — two spaces of indent per depth level, a chevron (`▾`
/// expanded / `▸` collapsed / blank for a leaf, so names stay aligned), the
/// display name, and a `(N)` unread count when there is unread mail.
fn line(v: &VisibleFolder) -> String {
    let indent = "  ".repeat(v.depth);
    let chevron = if !v.has_children {
        ' '
    } else if v.expanded {
        '\u{25be}' // ▾
    } else {
        '\u{25b8}' // ▸
    };
    let count = if v.row.unread_count > 0 {
        format!(" ({})", v.row.unread_count)
    } else {
        String::new()
    };
    format!("{indent}{chevron} {}{count}", v.row.display_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn renders_chevron_and_indented_child() {
        let mut app = App::for_test_with_folder_tree();
        app.store.set_folder_expanded("inbox", true).unwrap();
        app.reload_folders();
        let mut term = Terminal::new(TestBackend::new(30, 10)).unwrap();
        term.draw(|f| draw(f, &app, f.area())).unwrap();
        let rows: Vec<String> = {
            let buf = term.backend().buffer().clone();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect()
        };
        let all = rows.join("\n");
        // Inbox is expanded → shows the ▾ chevron.
        assert!(
            all.contains('\u{25be}'),
            "expected expanded chevron in:\n{all}"
        );
        // EPAM is a child → rendered indented (two leading spaces before its
        // glyph column), inside the bordered pane.
        assert!(
            rows.iter().any(|r| r.contains("   EPAM")),
            "expected indented EPAM in:\n{all}"
        );
    }
}
