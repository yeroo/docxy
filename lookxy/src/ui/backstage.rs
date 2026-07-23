//! The File "backstage": a full-screen menu shown when the File tab is chosen,
//! in the spirit of `docxy/src/backstage.rs` but much smaller — a left menu
//! (Automatic Replies / Settings / Exit) and a right content pane that shows
//! the Settings toggles when Settings is selected. Pure state + rendering here;
//! the actions live on `App`.

use crate::app::App;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

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
}

impl Default for Backstage {
    fn default() -> Self {
        Self::new()
    }
}

/// The screen y (relative to the content rect's top) of the first Settings
/// toggle row — content lines are: "Settings", blank, toggle0, toggle1, …
pub const SETTINGS_FIRST_ROW: u16 = 2;

/// Renders the backstage full-frame when open; a no-op otherwise. Matches the
/// docxy/xlsxy backstage: the ribbon tab strip on top (File selected, the other
/// tabs still visible + clickable), a left menu, and a right content pane.
/// Records the menu/content rects for mouse hit-testing.
pub fn draw(f: &mut Frame, app: &mut App) {
    let Some((sel, in_settings, settings_sel)) = app
        .backstage
        .as_ref()
        .map(|b| (b.sel, b.in_settings, b.settings_sel))
    else {
        return;
    };
    let (threaded, reminders) = (app.threaded, app.reminders_notify);

    let area = f.area();
    f.render_widget(Clear, area);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let accent = Style::default().fg(Color::Black).bg(Color::Cyan);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    // Tab strip with File selected; the other tabs stay visible so a click
    // leaves the backstage (see `App::backstage_mouse`).
    let mut tabline = app.ribbon.render_tabs_as(0);
    tabline
        .spans
        .push(Span::styled("   (click a tab or Esc to leave)", dim));
    f.render_widget(Paragraph::new(tabline), rows[0]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(20), Constraint::Min(10)])
        .split(rows[1]);
    app.bs_menu_rect = cols[0];
    app.bs_content_rect = cols[1];

    // Left menu.
    let labels: Vec<&str> = ITEMS.iter().map(|it| it.label()).collect();
    backstagecore::draw_menu_column(f, cols[0], &labels, sel, !in_settings, Color::Cyan, 16);

    // Right content.
    let content: Vec<Line> = match ITEMS[sel.min(ITEMS.len() - 1)] {
        Item::AutoReplies => vec![
            Line::from(""),
            Line::from("  Automatic Replies (Out-of-Office)"),
            Line::from(""),
            Line::from("  Enter to open the editor."),
        ],
        Item::Settings => {
            let mark = |on: bool| if on { "[x]" } else { "[ ]" };
            let toggles = [
                ("Threaded conversation view", threaded),
                ("Reminder desktop notifications", reminders),
            ];
            // Line 0 = "Settings", line 1 blank, then the toggles at
            // SETTINGS_FIRST_ROW — kept in lockstep with mouse hit-testing.
            let mut lines = vec![Line::from("  Settings"), Line::from("")];
            for (i, (label, on)) in toggles.iter().enumerate() {
                let style = if in_settings && settings_sel == i {
                    accent
                } else {
                    Style::default()
                };
                lines.push(Line::styled(format!("  {} {label}", mark(*on)), style));
            }
            lines.push(Line::from(""));
            lines.push(Line::styled(
                "  Enter to edit \u{b7} Space/Enter toggle \u{b7} \u{2190}/Esc back",
                dim,
            ));
            lines
        }
        Item::Exit => vec![
            Line::from(""),
            Line::from("  Exit lookxy"),
            Line::from(""),
            Line::from("  Enter to quit."),
        ],
    };
    f.render_widget(Paragraph::new(content), cols[1]);
}
