//! F1 help (#1856): the editor's manual as text in a full-screen reader.
//! `helpdocs` owns the layout and the reader's state; this module finds the
//! manual, maps keys and the mouse to it and draws it. xlsxy and yppxy have
//! the same module for their manuals.

use helpdocs::reader::{Key, Outcome, Reader};
use helpdocs::text::Look;
use helpdocs::{Manual, Target};
use ratatui::Frame;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use std::sync::Arc;
use unicode_width::UnicodeWidthStr;

/// The manual this editor reads (`docs/manual/<editor>/`).
pub(crate) const EDITOR: &str = "word";

/// The manual built in from `docs/manual/word/` by `build.rs` (empty until
/// one is published there).
#[cfg_attr(test, allow(dead_code))]
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/manual.rs"));
}

/// The installed manual, else the built-in one, read once per process.
/// Unit tests never read either: they put a manual on the `App`.
#[cfg(not(test))]
pub(crate) fn manual() -> Option<Arc<Manual>> {
    static MANUAL: std::sync::OnceLock<Option<Arc<Manual>>> = std::sync::OnceLock::new();
    MANUAL
        .get_or_init(|| helpdocs::locate::load(EDITOR, embedded::MANUAL).map(Arc::new))
        .clone()
}

/// A reader on `target`'s page.
fn open(manual: Arc<Manual>, target: Option<Target>) -> Option<Reader> {
    let page = target?.page;
    Reader::open(manual, &page, None)
}

/// F1: the page of `area` (the active ribbon tab's name), else the root.
pub(crate) fn open_area(manual: Arc<Manual>, area: Option<&str>) -> Option<Reader> {
    let target = helpdocs::resolve_area(&manual, area);
    open(manual, target)
}

/// Help › Help: the manual's `help` page.
pub(crate) fn open_help(manual: Arc<Manual>) -> Option<Reader> {
    let target = helpdocs::resolve(&manual, "help", Some("Help"));
    open(manual, target)
}

/// Whether `key` is F1 pressed alone, which opens help.
pub(crate) fn is_f1(key: &KeyEvent) -> bool {
    key.code == KeyCode::F(1) && key.modifiers.is_empty() && key.kind == KeyEventKind::Press
}

/// The reader's key for a terminal key; `None` for every other key, which the
/// reader swallows all the same.
fn reader_key(key: &KeyEvent) -> Option<Key> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    Some(match key.code {
        KeyCode::Esc | KeyCode::Char('q') if !ctrl && !alt => Key::Close,
        KeyCode::Left if alt => Key::Back,
        KeyCode::Right if alt => Key::Forward,
        KeyCode::Backspace => Key::Back,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown | KeyCode::Char(' ') => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::Tab => Key::NextLink,
        KeyCode::BackTab => Key::PrevLink,
        KeyCode::Enter => Key::Follow,
        KeyCode::Char('c') if !ctrl && !alt => Key::Contents,
        KeyCode::F(1) if key.kind == KeyEventKind::Press => Key::Root,
        _ => return None,
    })
}

/// A key while help is open: it owns every key. `Close` when it asks to.
pub(crate) fn key(reader: &mut Reader, key: KeyEvent) -> Outcome {
    if key.kind == KeyEventKind::Release {
        return Outcome::Stay;
    }
    match reader_key(&key) {
        Some(k) => reader.key(k),
        None => Outcome::Stay,
    }
}

/// Lines a wheel notch scrolls.
const WHEEL: isize = 3;

/// A mouse event while help is open: the wheel scrolls, the rest is swallowed.
pub(crate) fn mouse(reader: &mut Reader, m: &MouseEvent) {
    match m.kind {
        MouseEventKind::ScrollDown => reader.wheel(WHEEL),
        MouseEventKind::ScrollUp => reader.wheel(-WHEEL),
        _ => {}
    }
}

fn measure(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// How a span of the page looks; the selected link is reversed.
fn style(look: &Look, selected: Option<usize>, accent: Color) -> Style {
    let mut s = Style::default();
    if look.heading.is_some() {
        s = s.fg(accent).add_modifier(Modifier::BOLD);
    }
    if look.bold {
        s = s.add_modifier(Modifier::BOLD);
    }
    if look.italic || look.image {
        s = s.add_modifier(Modifier::ITALIC);
    }
    if look.code {
        s = s.fg(Color::Cyan);
    }
    if look.keys {
        s = s.add_modifier(Modifier::REVERSED);
    }
    if let Some(n) = look.link {
        s = s.fg(accent).add_modifier(Modifier::UNDERLINED);
        if Some(n) == selected {
            s = s.add_modifier(Modifier::REVERSED);
        }
    }
    s
}

/// Draw help over the whole frame: the editor under it is not drawn. A frame
/// too small for the border and a line draws only what fits.
pub(crate) fn draw(f: &mut Frame, reader: &mut Reader, accent: Color) {
    let area = f.area();
    f.render_widget(Clear, area);
    let title = format!(" Help: {} ", reader.title());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent))
        .title(Span::styled(
            title,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    // A column of margin each side, when there is room for it.
    let inner = if inner.width > 2 {
        Rect {
            x: inner.x + 1,
            width: inner.width - 2,
            ..inner
        }
    } else {
        inner
    };
    // The page above, the footer on the last inner row.
    let body_h = inner.height.saturating_sub(1);
    reader.layout(inner.width as usize, body_h as usize, &measure);
    let view = reader.view();
    let lines: Vec<Line> = view
        .lines
        .iter()
        .take(body_h as usize)
        .map(|l| {
            Line::from(
                l.spans
                    .iter()
                    .map(|s| Span::styled(s.text.clone(), style(&s.look, view.selected, accent)))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let body = Rect {
        height: body_h,
        ..inner
    };
    f.render_widget(Paragraph::new(lines), body);
    if inner.height > 0 {
        let footer = Rect {
            y: inner.y + body_h,
            height: 1,
            ..inner
        };
        let dim = Style::default().add_modifier(Modifier::DIM);
        f.render_widget(Paragraph::new(Span::styled(view.footer, dim)), footer);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::Path;

    /// `helpdocs/tests/fixture/<editor>` as a manual.
    pub(crate) fn fixture(editor: &str) -> Arc<Manual> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../helpdocs/tests/fixture")
            .join(editor);
        Arc::new(Manual::load(&dir).expect("the helpdocs fixture loads"))
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn keys_map_to_the_reader() {
        let none = KeyModifiers::NONE;
        let alt = KeyModifiers::ALT;
        let cases = [
            (press(KeyCode::Esc, none), Some(Key::Close)),
            (press(KeyCode::Char('q'), none), Some(Key::Close)),
            (press(KeyCode::Backspace, none), Some(Key::Back)),
            (press(KeyCode::Left, alt), Some(Key::Back)),
            (press(KeyCode::Right, alt), Some(Key::Forward)),
            (press(KeyCode::Left, none), None),
            (press(KeyCode::Char(' '), none), Some(Key::PageDown)),
            (press(KeyCode::Tab, none), Some(Key::NextLink)),
            (
                press(KeyCode::BackTab, KeyModifiers::SHIFT),
                Some(Key::PrevLink),
            ),
            (press(KeyCode::Enter, none), Some(Key::Follow)),
            (press(KeyCode::Char('c'), none), Some(Key::Contents)),
            (press(KeyCode::Char('c'), KeyModifiers::CONTROL), None),
            (press(KeyCode::F(1), none), Some(Key::Root)),
            (press(KeyCode::Char('x'), none), None),
        ];
        for (k, want) in cases {
            assert_eq!(reader_key(&k), want, "{k:?}");
        }
        // A held F1 does not keep jumping to the root.
        let mut held = press(KeyCode::F(1), none);
        held.kind = KeyEventKind::Repeat;
        assert_eq!(reader_key(&held), None);
        assert!(!is_f1(&held));
        assert!(is_f1(&press(KeyCode::F(1), none)));
        assert!(!is_f1(&press(KeyCode::F(1), KeyModifiers::SHIFT)));
    }

    #[test]
    fn the_wheel_scrolls() {
        let mut r = open_area(fixture(EDITOR), Some("Home")).unwrap();
        r.layout(30, 3, &measure);
        let wheel = |kind| MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        mouse(&mut r, &wheel(MouseEventKind::ScrollDown));
        assert_eq!(r.scroll(), 3);
        mouse(&mut r, &wheel(MouseEventKind::ScrollUp));
        assert_eq!(r.scroll(), 0);
    }
}
