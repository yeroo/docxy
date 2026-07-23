//! The left folder-tree pane: the store's folders rendered as a collapsible
//! tree (`App::visible_folders` — well-known ones pre-ranked at the top level,
//! children indented under their parent), each annotated with a chevron and its
//! unread count.

use crate::app::{App, Pane};
use crate::ui::border_style;
use crate::ui::foldertree::VisibleFolder;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::Folders;
    let block = Block::default()
        .title("Folders")
        .borders(Borders::ALL)
        .border_style(border_style(focused));

    let inner_width = area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = app
        .visible_folders
        .iter()
        .map(|v| ListItem::new(folder_line(v, inner_width)))
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

/// `"  ▾ Inbox            2"` — two spaces of indent per depth level, a chevron
/// (`▾` expanded / `▸` collapsed / blank for a leaf, so names stay aligned),
/// the display name, and the unread count as a **bold blue** number flush to
/// the right edge of the pane (Outlook style). Folders with no unread mail show
/// no number. On the highlighted row the list's highlight style repaints the
/// count white-on-blue, so it stays readable.
fn folder_line(v: &VisibleFolder, inner_width: usize) -> Line<'static> {
    let indent = "  ".repeat(v.depth);
    let chevron = if !v.has_children {
        ' '
    } else if v.expanded {
        '\u{25be}' // ▾
    } else {
        '\u{25b8}' // ▸
    };
    let left = format!("{indent}{chevron} {}", v.row.display_name);

    if v.row.unread_count == 0 {
        return Line::from(left);
    }
    let count = v.row.unread_count.to_string();
    // Keep at least one space between the name and the right-aligned count;
    // truncate an over-long name rather than push the count off-screen.
    let budget = inner_width.saturating_sub(count.chars().count() + 1);
    let left: String = left.chars().take(budget).collect();
    let pad = inner_width.saturating_sub(left.chars().count() + count.chars().count());
    Line::from(vec![
        Span::raw(format!("{left}{}", " ".repeat(pad))),
        Span::styled(
            count,
            Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcore::store::FolderRow;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn unread_count_is_bold_blue_and_right_aligned() {
        let v = VisibleFolder {
            row: FolderRow {
                id: "i".into(),
                parent_id: None,
                display_name: "Inbox".into(),
                total_count: 0,
                unread_count: 2,
                delta_link: None,
                well_known_name: Some("inbox".into()),
                sort_order: None,
                is_expanded: false,
            },
            depth: 0,
            has_children: false,
            expanded: false,
        };
        let line = folder_line(&v, 20);
        let last = line.spans.last().unwrap();
        assert_eq!(last.content, "2");
        assert_eq!(last.style.fg, Some(Color::Blue));
        assert!(last.style.add_modifier.contains(Modifier::BOLD));
        // Right-aligned: the spans together fill the whole inner width, so the
        // count sits flush against the right edge.
        let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(total, 20);
    }

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
