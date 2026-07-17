//! The bottom, single-row status bar: sync state and folder/message counts.
//! A simple line for now — Task 17 enriches it (account, richer sync
//! progress) once sign-in is wired up.

use crate::app::App;

use mailcore::sync::engine::SyncState;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
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
    let line = format!(
        "{status} — {folder} — {} folder(s), {} message(s)",
        app.folders.len(),
        app.messages.len()
    );
    f.render_widget(
        Paragraph::new(line).style(Style::new().fg(Color::White).bg(Color::DarkGray)),
        area,
    );
}
