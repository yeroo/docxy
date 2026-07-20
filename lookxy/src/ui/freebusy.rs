//! The free/busy availability grid — an overlay opened by `Ctrl-B` in the
//! event form. Read-only: shows each attendee's `availabilityView` as a strip
//! of busy/free glyphs plus a combined "everyone free" row. State + glyph
//! mapping live here; the draw/key handling is below (Task 4).

use crate::app::App;
use mailcore::graph::model::ScheduleEntry;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

pub struct FreeBusyView {
    pub day_label: String,
    pub slot_count: usize,
    pub entries: Vec<ScheduleEntry>,
    pub loading: bool,
}

/// One availability digit → its grid glyph. `'0'` free, `'1'` tentative,
/// `'2'`/`'3'`/`'4'` busy/OOF/elsewhere, anything else blank.
pub fn slot_glyph(c: char) -> char {
    match c {
        '0' => '·',
        '1' => '▓',
        '2' | '3' | '4' => '█',
        _ => ' ',
    }
}

/// The combined `free?`-row glyph for one slot across all entries: `'✓'` when
/// everyone is free (`'0'`, or past the end of a short string), `'█'` when
/// anyone is busy (`'2'`/`'3'`/`'4'`), else `'░'` (only tentatives).
pub fn combined_glyph(entries: &[ScheduleEntry], slot: usize) -> char {
    let mut any_busy = false;
    let mut any_tentative = false;
    for e in entries {
        match e.availability.chars().nth(slot) {
            Some('2') | Some('3') | Some('4') => any_busy = true,
            Some('1') => any_tentative = true,
            _ => {} // '0' or missing = free
        }
    }
    if any_busy {
        '█'
    } else if any_tentative {
        '░'
    } else {
        '✓'
    }
}

/// Renders the free/busy overlay when `app.free_busy` is open; a no-op
/// otherwise. A centered bordered panel with an hour header, one row per
/// entry, and a combined `free?` row.
pub fn draw(f: &mut Frame, app: &App) {
    let Some(v) = &app.free_busy else {
        return;
    };
    let area = crate::ui::centered_rect(80, 60, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(
            "Availability \u{2014} {} (08:00\u{2013}18:00)  [Esc: back]",
            v.day_label
        ))
        .borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if v.loading {
        f.render_widget(Paragraph::new("loading\u{2026}"), inner);
        return;
    }

    // Width of the leading email label column.
    const LABEL_W: usize = 12;
    let pad = |s: &str| -> String {
        let mut t: String = s.chars().take(LABEL_W).collect();
        while t.chars().count() < LABEL_W {
            t.push(' ');
        }
        t
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    // Hour header: two slots per hour, so a digit every 2 slots (08..17).
    let mut header = pad("");
    for slot in 0..v.slot_count {
        header.push(if slot % 2 == 0 {
            std::char::from_digit((8 + slot as u32 / 2) % 10, 10).unwrap_or(' ')
        } else {
            ' '
        });
    }
    lines.push(Line::from(header));
    for e in &v.entries {
        let mut row = pad(&e.email);
        for slot in 0..v.slot_count {
            row.push(slot_glyph(e.availability.chars().nth(slot).unwrap_or('0')));
        }
        lines.push(Line::from(row));
    }
    let mut free_row = pad("free?");
    for slot in 0..v.slot_count {
        free_row.push(combined_glyph(&v.entries, slot));
    }
    lines.push(Line::from(free_row));
    if v.entries.is_empty() {
        lines.push(Line::from("(no attendees)"));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// Keys while the overlay is open: `Esc` closes it; other keys ignored.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    if key.code == KeyCode::Esc {
        app.close_free_busy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_renders_rows_and_free_row() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = App::for_test_with_seeded_store();
        app.free_busy = Some(FreeBusyView {
            day_label: "Mon Jul 21".into(),
            slot_count: 20,
            entries: vec![ScheduleEntry {
                email: "alice@x".into(),
                availability: "00222200000000000000".into(),
            }],
            loading: false,
        });
        let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Availability"));
        assert!(text.contains("alice@x"));
        assert!(text.contains("free?"));
    }

    #[test]
    fn glyph_mapping() {
        assert_eq!(slot_glyph('0'), '·');
        assert_eq!(slot_glyph('1'), '▓');
        assert_eq!(slot_glyph('2'), '█');
        let entries = vec![
            ScheduleEntry {
                email: "a".into(),
                availability: "02".into(),
            },
            ScheduleEntry {
                email: "b".into(),
                availability: "00".into(),
            },
        ];
        assert_eq!(combined_glyph(&entries, 0), '✓'); // both free
        assert_eq!(combined_glyph(&entries, 1), '█'); // a busy
        assert_eq!(combined_glyph(&entries, 5), '✓'); // past end = free
    }
}
