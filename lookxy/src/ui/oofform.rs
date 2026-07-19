//! The automatic-replies (out-of-office) editor overlay. Modeled on
//! `ui::eventform`: a full-frame modal (opened by `O`) with Tab-navigated
//! fields, `Space`-cycled status/audience radios, two multi-line message
//! editors, and an inline error footer. Fetched on open and written through on
//! save — see `App::open_oof_form`/`save_oof_form` and the `Fetch/Set
//! AutomaticReplies` sync commands. This module renders and holds state; the
//! app owns the fetch/save wiring.

use crate::app::App;
use mailcore::graph::model::{AutomaticReplies, ExternalAudience, OofStatus};

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OofField {
    Status,
    Start,
    End,
    Audience,
    Internal,
    External,
}

/// The open automatic-replies editor. `start`/`end` hold local-time display
/// text (parsed on save, only when `status == Scheduled`); `internal`/
/// `external` are plain-text reply messages. `loading` is true from open until
/// `AutomaticRepliesFetched` prefills the form. `error` is the inline footer
/// validation message.
pub struct OofForm {
    pub loading: bool,
    pub status: OofStatus,
    pub start: String,
    pub end: String,
    pub audience: ExternalAudience,
    pub internal: String,
    pub external: String,
    pub focus: OofField,
    pub error: Option<String>,
}

impl OofForm {
    /// The freshly-opened, still-loading form (fields are placeholders until
    /// `prefill`). Status defaults to `Disabled`, audience to `All`.
    pub fn loading_default() -> OofForm {
        OofForm {
            loading: true,
            status: OofStatus::Disabled,
            start: String::new(),
            end: String::new(),
            audience: ExternalAudience::All,
            internal: String::new(),
            external: String::new(),
            focus: OofField::Status,
            error: None,
        }
    }

    /// Fill the fields from a fetched `AutomaticReplies`. `off` is the local
    /// UTC offset in minutes (`ui::calendar::local_offset_minutes()`); the
    /// scheduled UTC bounds are rendered to the form's local display text
    /// (empty when the bound is `""`).
    pub fn prefill(&mut self, r: &AutomaticReplies, off: i64) {
        self.status = r.status;
        self.audience = r.external_audience;
        self.internal = r.internal_message.clone();
        self.external = r.external_message.clone();
        self.start = utc_to_display(&r.scheduled_start_utc, off);
        self.end = utc_to_display(&r.scheduled_end_utc, off);
        self.error = None;
    }

    pub fn cycle_status(&mut self) {
        self.status = match self.status {
            OofStatus::Disabled => OofStatus::AlwaysEnabled,
            OofStatus::AlwaysEnabled => OofStatus::Scheduled,
            OofStatus::Scheduled => OofStatus::Disabled,
        };
    }

    pub fn cycle_audience(&mut self) {
        self.audience = match self.audience {
            ExternalAudience::None => ExternalAudience::ContactsOnly,
            ExternalAudience::ContactsOnly => ExternalAudience::All,
            ExternalAudience::All => ExternalAudience::None,
        };
    }

    pub fn next_field(&mut self) {
        self.focus = match self.focus {
            OofField::Status => OofField::Start,
            OofField::Start => OofField::End,
            OofField::End => OofField::Audience,
            OofField::Audience => OofField::Internal,
            OofField::Internal => OofField::External,
            OofField::External => OofField::Status,
        };
    }

    pub fn prev_field(&mut self) {
        self.focus = match self.focus {
            OofField::Status => OofField::External,
            OofField::Start => OofField::Status,
            OofField::End => OofField::Start,
            OofField::Audience => OofField::End,
            OofField::Internal => OofField::Audience,
            OofField::External => OofField::Internal,
        };
    }
}

/// Renders one canonical-UTC bound to the form's `YYYY-MM-DD HH:MM` local text,
/// or `""` when the bound is empty/unparseable. `utc_iso_to_local` (the same
/// `pub(crate)` inverse the timed event-edit path uses) returns
/// `Option<LocalDateTime>`, and an empty `utc` parses to `None`, so the whole
/// thing collapses to `""` via `unwrap_or_default`.
fn utc_to_display(utc: &str, off: i64) -> String {
    crate::datetime::utc_iso_to_local(utc, off)
        .map(crate::datetime::format_local)
        .unwrap_or_default()
}

/// Renders the OOF editor overlay when `app.oof_form` is open; a no-op
/// otherwise (mirrors `eventform::draw`).
pub fn draw(f: &mut Frame, app: &App) {
    let Some(form) = &app.oof_form else {
        return;
    };
    let area = f.area();
    f.render_widget(Clear, area);

    if form.loading {
        f.render_widget(
            Paragraph::new("Automatic Replies — loading…").block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Automatic Replies"),
            ),
            area,
        );
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Status
            Constraint::Length(3), // Start
            Constraint::Length(3), // End
            Constraint::Length(3), // Audience
            Constraint::Min(3),    // Internal
            Constraint::Min(3),    // External
            Constraint::Length(1), // Footer
        ])
        .split(area);

    let scheduled = form.status == OofStatus::Scheduled;
    draw_radio(
        f,
        rows[0],
        "Status",
        &[
            ("Off", form.status == OofStatus::Disabled),
            ("On", form.status == OofStatus::AlwaysEnabled),
            ("Scheduled", scheduled),
        ],
        form.focus == OofField::Status,
        true,
    );
    draw_field(
        f,
        rows[1],
        "Start",
        &form.start,
        form.focus == OofField::Start,
        scheduled,
    );
    draw_field(
        f,
        rows[2],
        "End",
        &form.end,
        form.focus == OofField::End,
        scheduled,
    );
    draw_radio(
        f,
        rows[3],
        "External audience",
        &[
            ("None", form.audience == ExternalAudience::None),
            ("Contacts", form.audience == ExternalAudience::ContactsOnly),
            ("All", form.audience == ExternalAudience::All),
        ],
        form.focus == OofField::Audience,
        true,
    );
    draw_field(
        f,
        rows[4],
        "Internal reply",
        &form.internal,
        form.focus == OofField::Internal,
        true,
    );
    draw_field(
        f,
        rows[5],
        "External reply",
        &form.external,
        form.focus == OofField::External,
        true,
    );

    let footer = form
        .error
        .clone()
        .unwrap_or_else(|| "Tab: next  Space: toggle  Ctrl-S: save  Esc: cancel".to_string());
    f.render_widget(Paragraph::new(footer), rows[6]);
}

/// A titled radio row: `Label: (x) A  ( ) B  ( ) C`. `enabled=false` dims it.
fn draw_radio(
    f: &mut Frame,
    area: Rect,
    label: &str,
    opts: &[(&str, bool)],
    focused: bool,
    enabled: bool,
) {
    let mut spans = format!("{label}: ");
    for (name, on) in opts {
        spans.push_str(if *on { "(x) " } else { "( ) " });
        spans.push_str(name);
        spans.push_str("  ");
    }
    let style = field_style(focused, enabled);
    f.render_widget(
        Paragraph::new(Line::from(spans))
            .block(border(focused))
            .style(style),
        area,
    );
}

/// A titled single-line text field. `enabled=false` (e.g. Start/End when not
/// Scheduled) dims it.
fn draw_field(f: &mut Frame, area: Rect, label: &str, value: &str, focused: bool, enabled: bool) {
    f.render_widget(
        Paragraph::new(value.to_string())
            .block(border(focused).title(label.to_string()))
            .style(field_style(focused, enabled)),
        area,
    );
}

fn border(focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(crate::ui::border_style(focused))
}

fn field_style(focused: bool, enabled: bool) -> Style {
    if !enabled {
        Style::default().fg(Color::DarkGray)
    } else if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use mailcore::graph::model::{AutomaticReplies, ExternalAudience, OofStatus};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn draw_renders_radios_and_message_labels() {
        let mut app = App::for_test_with_seeded_store();
        let mut form = OofForm::loading_default();
        form.loading = false;
        form.prefill(
            &AutomaticReplies {
                status: OofStatus::Scheduled,
                external_audience: ExternalAudience::All,
                internal_message: "Away".into(),
                external_message: "Out".into(),
                scheduled_start_utc: "2026-07-20T09:00:00Z".into(),
                scheduled_end_utc: "2026-07-27T17:00:00Z".into(),
            },
            0, // UTC offset for the test
        );
        app.oof_form = Some(form);

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Automatic Replies") || text.contains("Status"));
        assert!(text.contains("Scheduled"));
        assert!(text.contains("Internal"));
        assert!(text.contains("External"));
        assert!(text.contains("Away"));
    }

    #[test]
    fn cycling_status_and_audience_wraps() {
        let mut form = OofForm::loading_default();
        assert_eq!(form.status, OofStatus::Disabled);
        form.cycle_status();
        assert_eq!(form.status, OofStatus::AlwaysEnabled);
        form.cycle_status();
        assert_eq!(form.status, OofStatus::Scheduled);
        form.cycle_status();
        assert_eq!(form.status, OofStatus::Disabled);

        form.audience = ExternalAudience::None;
        form.cycle_audience();
        assert_eq!(form.audience, ExternalAudience::ContactsOnly);
        form.cycle_audience();
        assert_eq!(form.audience, ExternalAudience::All);
        form.cycle_audience();
        assert_eq!(form.audience, ExternalAudience::None);
    }
}
