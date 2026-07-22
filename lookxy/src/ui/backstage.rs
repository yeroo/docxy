//! The File "backstage": a full-screen menu shown when the File tab is chosen,
//! in the spirit of `docxy/src/backstage.rs` but much smaller — a left menu
//! (Automatic Replies / Settings / Exit) and a right content pane that shows
//! the Settings toggles when Settings is selected. Pure state + rendering here;
//! the actions live on `App`.

use crate::app::App;
use crate::ui::centered_rect;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

/// The vertical menu items, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    AutoReplies,
    Settings,
    Exit,
}

pub const ITEMS: [Item; 3] = [Item::AutoReplies, Item::Settings, Item::Exit];

impl Item {
    pub fn label(self) -> &'static str {
        match self {
            Item::AutoReplies => "Automatic Replies",
            Item::Settings => "Settings",
            Item::Exit => "Exit",
        }
    }
}

/// The two toggle rows shown under Settings.
pub const SETTINGS_ROWS: usize = 2;

/// Backstage state: which menu item is highlighted, whether focus has moved
/// into the Settings toggles, and which toggle row.
pub struct Backstage {
    pub sel: usize,
    pub in_settings: bool,
    pub settings_sel: usize,
}

impl Backstage {
    pub fn new() -> Self {
        Backstage {
            sel: 0,
            in_settings: false,
            settings_sel: 0,
        }
    }

    pub fn selected_item(&self) -> Item {
        ITEMS[self.sel.min(ITEMS.len() - 1)]
    }
}

impl Default for Backstage {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders the backstage full-frame when open; a no-op otherwise.
pub fn draw(f: &mut Frame, app: &App) {
    let Some(bs) = &app.backstage else {
        return;
    };
    let area = centered_rect(80, 70, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title("File \u{2014} Esc to close")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(22), Constraint::Min(0)])
        .split(inner);

    // Left menu.
    let items: Vec<ListItem> = ITEMS
        .iter()
        .map(|it| ListItem::new(format!("  {}", it.label())))
        .collect();
    let menu = List::new(items).highlight_style(if bs.in_settings {
        Style::new().add_modifier(Modifier::DIM)
    } else {
        Style::new().bg(Color::Blue).fg(Color::White)
    });
    let mut state = ListState::default();
    state.select(Some(bs.sel));
    f.render_stateful_widget(menu, cols[0], &mut state);

    // Right content.
    let content: Vec<Line> = match bs.selected_item() {
        Item::AutoReplies => vec![
            Line::from("Automatic Replies (Out-of-Office)"),
            Line::from(""),
            Line::from("Enter to open the editor."),
        ],
        Item::Settings => {
            let mark = |on: bool| if on { "[x]" } else { "[ ]" };
            let rows = [
                ("Threaded conversation view", app.threaded),
                ("Reminder desktop notifications", app.reminders_notify),
            ];
            let mut lines = vec![Line::from("Settings"), Line::from("")];
            for (i, (label, on)) in rows.iter().enumerate() {
                let style = if bs.in_settings && bs.settings_sel == i {
                    Style::new().bg(Color::Blue).fg(Color::White)
                } else {
                    Style::new()
                };
                lines.push(Line::styled(format!("  {} {label}", mark(*on)), style));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(
                "Enter/\u{2192} to edit, Space/Enter to toggle, \u{2190}/Esc to go back.",
            ));
            lines
        }
        Item::Exit => vec![
            Line::from("Exit lookxy"),
            Line::from(""),
            Line::from("Enter to quit."),
        ],
    };
    f.render_widget(Paragraph::new(content), cols[1]);
}
