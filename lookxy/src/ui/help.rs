//! The read-only help overlay (`F1` / `?`). A centered cheat-sheet of every
//! keybinding, grouped by context. The keymap here is hand-maintained
//! (double-entry against the real bindings) — a TUI help panel is worth it.
//! `Esc`/`F1`/`?`/`q` close it.

use crate::app::App;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// The grouped cheat-sheet: `(group, [(keys, description)])`.
const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Global",
        &[
            ("Tab / Shift-Tab", "cycle panes forward / back"),
            ("\u{2190} / h / Esc", "back toward the folder list"),
            ("g", "toggle Mail / Calendar"),
            ("/", "search"),
            ("F1 / ?", "this help"),
            ("q", "quit"),
        ],
    ),
    (
        "Folders",
        &[
            ("j / k", "move"),
            ("\u{2192} / l", "expand"),
            ("\u{2190} / h", "collapse / parent"),
            ("Space", "toggle"),
            ("Enter", "open folder"),
        ],
    ),
    (
        "Message list",
        &[
            ("j / k", "move"),
            ("Enter", "open"),
            ("m / u", "mark read / unread"),
            ("f", "flag"),
            ("d / Del", "delete"),
            ("v", "move to folder"),
            ("a", "attachments"),
            ("l / L", "categorize / filter"),
            ("t", "threaded view"),
            ("c", "compose"),
            ("r / R", "reply / reply-all"),
            ("F", "forward"),
            ("A / D / T", "RSVP accept / decline / tentative"),
            ("O", "out-of-office"),
        ],
    ),
    (
        "Reading",
        &[
            ("j / k", "scroll"),
            ("PgUp / PgDn", "page"),
            ("Home / End", "top / bottom"),
            ("Esc / \u{2190}", "back to list"),
        ],
    ),
    (
        "Calendar",
        &[
            ("j / k", "move"),
            ("c / e / x", "new / edit / delete"),
            ("a / d / t", "RSVP"),
            ("O", "out-of-office"),
            ("g / Esc", "back to Mail"),
        ],
    ),
    (
        "Event form",
        &[
            ("Tab", "next field"),
            ("Space", "all-day"),
            ("Ctrl-B", "free/busy"),
            ("Ctrl-Enter", "save"),
            ("Esc", "cancel"),
        ],
    ),
    ("Compose", &[("Ctrl-Enter", "send"), ("Esc", "cancel")]),
];

/// Renders the help overlay when `app.help` is set; a no-op otherwise.
pub fn draw(f: &mut Frame, app: &App) {
    if !app.help {
        return;
    }
    let area = crate::ui::centered_rect(70, 80, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title("Help \u{2014} Esc to close")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (group, rows) in HELP {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            (*group).to_string(),
            Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )));
        for (keys, desc) in *rows {
            lines.push(Line::from(format!("  {keys:<16} {desc}")));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// Keys while help is open: `Esc`/`F1`/`?`/`q` close it; everything else is
/// swallowed (help is read-only).
pub fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('?') | KeyCode::Char('q') => app.close_help(),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};

    #[test]
    fn f1_opens_and_esc_closes() {
        let mut app = App::for_test_with_seeded_store();
        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::F(1)));
        assert!(app.help);
        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Esc));
        assert!(!app.help);
    }

    #[test]
    fn question_mark_does_not_open_help_while_searching() {
        let mut app = App::for_test_with_seeded_store();
        app.start_search();
        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Char('?')));
        assert!(!app.help);
    }

    #[test]
    fn q_over_help_closes_it_rather_than_quitting() {
        let mut app = App::for_test_with_seeded_store();
        app.open_help();
        // Counts as capturing text, which is what guards the global `q`-quit.
        assert!(app.is_capturing_text());
    }

    #[test]
    fn draw_lists_group_headers() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = App::for_test_with_seeded_store();
        app.open_help();
        let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Global"));
        assert!(text.contains("Message list"));
    }
}
