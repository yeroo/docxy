//! The right reading pane: headers plus the rendered message body for the
//! selected (opened) message. Body rendering itself (HTML→styled-text, or
//! plain-text wrapping) lives in `mailcore::htmlrender` so it's testable
//! without ratatui; this module's job is just headers, the
//! loading/no-body placeholders, and mapping `mailcore`'s neutral
//! `StyledLine`/`StyledSpan` onto ratatui's `Line`/`Span`/`Style`.
//!
//! Body loading itself (cache-hit vs. `SyncCommand::FetchBody` vs.
//! `SyncEvent::BodyReady`) is `App::open_message`/`reload_body` (see
//! `app.rs`) and `main::drain_events` — this module only reads the result
//! (`app.body`/`app.body_loading`).

use crate::app::{App, Pane};
use crate::ui::border_style;

use mailcore::htmlrender::{self, StyledLine, StyledSpan};
use mailcore::store::MessageRow;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::Reading;
    let block = Block::default()
        .title("Reading Pane")
        .borders(Borders::ALL)
        .border_style(border_style(focused));

    // Inner width once the left/right borders are taken out — what the
    // body is wrapped to, so wrapped lines never overrun the pane.
    let inner_width = area.width.saturating_sub(2) as usize;

    let lines = match selected_message(app) {
        Some(m) => {
            let mut lines = header_lines(m);
            lines.push(Line::from(""));
            lines.extend(body_lines(app, inner_width));
            lines
        }
        None => vec![Line::from("(no message selected — press Enter on a message)")],
    };

    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
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
    ]
}

/// The opened message's body, rendered to `width` columns: `loading…`
/// while a `FetchBody` is outstanding (`App::body_loading`), the HTML- or
/// plain-text-rendered body once `App::body` has it, or a placeholder if
/// neither (the store lookup itself failed — see `App::reload_body`).
fn body_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    match (&app.body, app.body_loading) {
        (_, true) => vec![Line::from("loading…")],
        (Some(body), false) if body.content_type.eq_ignore_ascii_case("html") => {
            htmlrender::render_html(&body.content, width).iter().map(to_ratatui_line).collect()
        }
        (Some(body), false) => {
            htmlrender::render_text(&body.content, width).iter().map(to_ratatui_line).collect()
        }
        (None, false) => vec![Line::from("(no body)")],
    }
}

/// Maps one `StyledLine` to a ratatui `Line`: `indent` becomes that many
/// `htmlrender::INDENT_SPACES`-wide groups of leading spaces (the same
/// figure `htmlrender` already subtracted from its wrap width, so this
/// plus the wrapped text still fits within what `body_lines` asked for).
fn to_ratatui_line(line: &StyledLine) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    let indent = line.indent as usize * htmlrender::INDENT_SPACES;
    if indent > 0 {
        spans.push(Span::raw(" ".repeat(indent)));
    }
    spans.extend(line.spans.iter().map(to_ratatui_span));
    Line::from(spans)
}

/// Maps one `StyledSpan` to a ratatui `Span`: bold/italic/underline become
/// the matching `Modifier`; a link (footnote reference or the footnote
/// appendix line itself) renders in cyan so it stands out from plain body
/// text.
fn to_ratatui_span(span: &StyledSpan) -> Span<'static> {
    let mut style = Style::default();
    if span.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if span.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if span.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if span.link.is_some() {
        style = style.fg(Color::Cyan);
    }
    Span::styled(span.text.clone(), style)
}
