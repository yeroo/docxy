//! The right reading pane: headers plus a plain-text preview for the
//! selected (opened) message. Full HTML rendering of the body lands in
//! Task 14 — this just proves the pane wiring and shows enough (from,
//! subject, received, preview snippet) to be useful meanwhile.

use crate::app::{App, Pane};
use crate::ui::border_style;

use mailcore::store::MessageRow;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::Reading;
    let block = Block::default()
        .title("Reading Pane")
        .borders(Borders::ALL)
        .border_style(border_style(focused));

    let lines = match selected_message(app) {
        Some(m) => header_lines(m),
        None => vec![Line::from("(no message selected — press Enter on a message)")],
    };

    f.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        area,
    );
}

/// The message named by `App::selected_msg`, if it's still in the currently
/// loaded (visible-folder) message list.
fn selected_message(app: &App) -> Option<&MessageRow> {
    let id = app.selected_msg.as_deref()?;
    app.messages.iter().find(|m| m.id == id)
}

fn header_lines(m: &MessageRow) -> Vec<Line<'static>> {
    vec![
        Line::from(format!("From: {} <{}>", m.from_name, m.from_addr)),
        Line::from(format!("Subject: {}", m.subject)),
        Line::from(format!("Received: {}", m.received_at)),
        Line::from(""),
        Line::from(m.preview.clone()),
    ]
}
