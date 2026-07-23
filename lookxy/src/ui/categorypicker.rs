//! The category picker overlay — one popup, two modes. Assign mode toggles
//! categories on the highlighted message (`Space`) and applies on `Enter`;
//! Filter mode picks one category to filter the folder view by (`Enter`).
//! Opened by `l` (Assign) / `L` (Filter); see `App::open_category_picker`.

use crate::app::App;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerMode {
    Assign,
    Filter,
}

pub struct CategoryItem {
    pub name: String,
    pub color: Color,
    pub selected: bool,
}

pub struct CategoryPicker {
    pub mode: PickerMode,
    /// The message being edited, in Assign mode; `None` in Filter mode.
    pub message_id: Option<String>,
    pub items: Vec<CategoryItem>,
    pub index: usize,
}

/// Renders the picker overlay when open; a no-op otherwise.
pub fn draw(f: &mut Frame, app: &App) {
    let Some(p) = &app.category_picker else {
        return;
    };
    let area = centered(f.area(), 50, 60);
    f.render_widget(Clear, area);
    let title = match p.mode {
        PickerMode::Assign => "Categories (Space: toggle, Enter: apply, Esc: cancel)",
        PickerMode::Filter => "Filter by category (Enter: apply, Esc: cancel)",
    };
    let items: Vec<ListItem> = if p.items.is_empty() {
        vec![ListItem::new("(no categories — define them in Outlook)")]
    } else {
        p.items
            .iter()
            .map(|it| {
                let mark = match p.mode {
                    PickerMode::Assign => {
                        if it.selected {
                            "[x] "
                        } else {
                            "[ ] "
                        }
                    }
                    PickerMode::Filter => "",
                };
                ListItem::new(Line::from(vec![
                    Span::raw(mark),
                    Span::styled("● ", Style::default().fg(it.color)),
                    Span::raw(it.name.clone()),
                ]))
            })
            .collect()
    };
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if !p.items.is_empty() {
        state.select(Some(p.index.min(p.items.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// A centered rect `pct_w`×`pct_h` percent of `area`.
fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_h) / 2),
            Constraint::Percentage(pct_h),
            Constraint::Percentage((100 - pct_h) / 2),
        ])
        .split(area)[1];
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_w) / 2),
            Constraint::Percentage(pct_w),
            Constraint::Percentage((100 - pct_w) / 2),
        ])
        .split(v)[1]
}

/// Key handling while the picker is open (routed ahead of the panes).
pub fn handle_key(app: &mut App, key: ratatui::crossterm::event::KeyEvent) {
    use ratatui::crossterm::event::KeyCode;
    match key.code {
        KeyCode::Esc => app.category_picker = None,
        KeyCode::Up | KeyCode::Char('k') => app.category_picker_select(-1),
        KeyCode::Down | KeyCode::Char('j') => app.category_picker_select(1),
        KeyCode::Char(' ') => app.category_picker_toggle(),
        KeyCode::Enter => app.apply_category_picker(),
        _ => {}
    }
}
