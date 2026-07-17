//! The bottom, single-row status bar: signed-in account, sync state,
//! folder/message counts, and the last attachment save/open notice.

use crate::app::App;

use mailcore::sync::engine::SyncState;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let account = app.account.as_deref().unwrap_or("not signed in");
    let status = match &app.status {
        SyncState::Idle => "Idle".to_string(),
        SyncState::Syncing => "Syncing...".to_string(),
        SyncState::Offline => "Offline".to_string(),
        SyncState::PendingOps(n) => format!("{n} pending"),
        SyncState::SignInRequired => "Sign-in required".to_string(),
    };
    let folder = app
        .selected_folder
        .as_ref()
        .and_then(|id| app.folders.iter().find(|f| &f.id == id))
        .map(|f| f.display_name.as_str())
        .unwrap_or("(no folder)");
    // `visible_message_count` (not `app.messages.len()`) so the count
    // reflects the search results while a query is active, not the
    // underlying folder's real size.
    let mut line = format!(
        "{account} — {status} — {folder} — {} folder(s), {} message(s)",
        app.folders.len(),
        app.visible_message_count()
    );
    if let Some(notice) = &app.attachment_notice {
        line.push_str(" — ");
        line.push_str(notice);
    }
    f.render_widget(
        Paragraph::new(line).style(Style::new().fg(Color::White).bg(Color::DarkGray)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use ratatui::{Terminal, backend::TestBackend};

    fn render(app: &App) -> String {
        let mut term = Terminal::new(TestBackend::new(120, 5)).unwrap();
        term.draw(|f| draw(f, app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn shows_not_signed_in_placeholder_with_no_account() {
        let app = App::for_test_with_seeded_store();
        assert!(app.account.is_none());
        assert!(render(&app).contains("not signed in"));
    }

    #[test]
    fn shows_folder_and_message_counts() {
        let app = App::for_test_with_seeded_store();
        let text = render(&app);
        assert!(text.contains("1 folder(s)"));
        assert!(text.contains("1 message(s)"));
    }
}
