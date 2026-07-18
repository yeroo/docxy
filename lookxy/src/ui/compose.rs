//! The full-screen compose view: To/Cc/Subject fields and a rich-text body
//! editor over `editcore::ops::Editor`, plus an action-bar footer. Drawn
//! instead of the three-pane layout whenever `App::compose` is open (see
//! `ui::draw`); its own key handling (`handle_key`) takes over ahead of
//! every other popup whenever it is (see `ui::handle_key`).
//!
//! What this module does NOT do: actually send/save/discard anything over
//! the network or the local store — that's the next task's job (drafts
//! resume, `SyncCommand::SaveDraft`/`SendDraft`, closing the composer).
//! Ctrl-Enter/Esc/Ctrl-D here don't act directly; they just record which
//! action was requested in `App::compose_action` for that wiring to read
//! (and clear, once it's carried out).

use crate::app::App;
use crate::ui::border_style;

use editcore::cursor::Pos;
use editcore::model::{Block, RichText, Run};
// `editcore`'s crate root has no re-export of `Editor` (same adaptation
// `mailcore::compose_html`'s tests note) — it lives at `editcore::ops::Editor`.
use editcore::ops::Editor;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as RBlock, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

/// Which field currently has keyboard focus in the composer. Tab cycles
/// `To` → `Cc` → `Subject` → `Body` → `To` (see `cycle_focus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeField {
    To,
    Cc,
    Subject,
    Body,
}

/// What the last Ctrl-Enter/Esc/Ctrl-D asked for — recorded on
/// `App::compose_action` (not on `Compose` itself, so `Compose`'s field set
/// stays exactly the header fields + editor + focus + draft id) for the
/// next task's wiring to consume; see the module doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeAction {
    Send,
    Save,
    Discard,
}

/// The compose view's state: the three header fields, the rich-text body
/// editor, which field has focus, and the id of the draft being edited (a
/// `local:<uuid>` id for a brand-new message, or the Graph id once a save
/// has reconciled it — opaque to this module either way).
pub struct Compose {
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub editor: Editor,
    pub focus: ComposeField,
    /// Opaque to this module: nothing here reads it back (yet — a later
    /// task's send/save wiring is what looks it up to know which draft to
    /// update/send). Silences `dead_code`, which can't see across tasks.
    #[allow(dead_code)]
    pub draft_id: String,
}

impl Compose {
    /// A blank composer over a fresh, empty `Editor` — the "new message"
    /// starting point. `draft_id` is opaque here; the caller supplies
    /// whatever id the store/Graph gave the draft.
    ///
    /// Not yet called from production code — a later task's entry points
    /// (`c` for a new message, resuming a Drafts-folder message) are what
    /// will call this; `cfg_attr` silences `dead_code` only outside tests,
    /// same pattern already used for `yppxy`/`xlsxy`'s ribbon modules.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(draft_id: String) -> Compose {
        Compose {
            to: String::new(),
            cc: String::new(),
            subject: String::new(),
            editor: Editor::new(),
            focus: ComposeField::To,
            draft_id,
        }
    }
}

/// Renders the full-screen composer when `app.compose` is open; a no-op
/// otherwise (mirrors every other conditional-draw function in `ui`, e.g.
/// `ui::signin::draw`). Layout, top to bottom: To / Cc / Subject (3 rows
/// each), the body editor (everything else), and a 1-row action-bar
/// footer.
pub fn draw_compose(f: &mut Frame, app: &App) {
    let Some(compose) = &app.compose else {
        return;
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_field(
        f,
        rows[0],
        "To",
        &compose.to,
        compose.focus == ComposeField::To,
    );
    draw_field(
        f,
        rows[1],
        "Cc",
        &compose.cc,
        compose.focus == ComposeField::Cc,
    );
    draw_field(
        f,
        rows[2],
        "Subject",
        &compose.subject,
        compose.focus == ComposeField::Subject,
    );
    draw_body(f, rows[3], compose);
    draw_action_bar(f, rows[4]);
}

/// One single-line To/Cc/Subject field: a bordered box, bright when
/// focused (`border_style`, shared with the three-pane view), with a
/// trailing `_` caret when it holds focus — the same "cursor is just the
/// last character" convention `ui::search`'s query prompt already uses,
/// since these fields have no interior cursor position of their own
/// (Backspace always removes the last character; see `handle_key`).
fn draw_field(f: &mut Frame, area: Rect, title: &str, value: &str, focused: bool) {
    let block = RBlock::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style(focused));
    let text = if focused {
        format!("{value}_")
    } else {
        value.to_string()
    };
    f.render_widget(Paragraph::new(text).block(block), area);
}

/// The action-bar footer: a reminder of the keys that aren't otherwise
/// visible on screen.
fn draw_action_bar(f: &mut Frame, area: Rect) {
    let text = "Send: Ctrl-Enter   Save: Esc   Discard: Ctrl-D   \
                Bold: Ctrl-B  Italic: Ctrl-I  Underline: Ctrl-U  List: Ctrl-L";
    f.render_widget(
        Paragraph::new(text).style(Style::new().fg(Color::White).bg(Color::DarkGray)),
        area,
    );
}

/// The body editor: one ratatui `Line` per `editcore` block, runs mapped to
/// styled `Span`s (`run_style`), list items indented and bulleted/numbered
/// (`block_prefix`), and — while `Body` has focus — a reversed-style
/// one-cell caret baked directly into the rendered spans (`block_line`) so
/// it's visible in the buffer itself, plus the real terminal cursor placed
/// at the same cell via `set_cursor_position`.
///
/// Every index this function (and its helpers) touches into
/// `compose.editor.text`/`sel` is defensively clamped — `editcore::ops`
/// guarantees its own ops never panic on a stale position, but this is a
/// separate, read-only walk over the buffer and must uphold the same
/// guarantee on its own.
fn draw_body(f: &mut Frame, area: Rect, compose: &Compose) {
    let focused = compose.focus == ComposeField::Body;
    let block = RBlock::default()
        .title("Body")
        .borders(Borders::ALL)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let text = &compose.editor.text;
    if text.blocks.is_empty() {
        return;
    }

    let caret = compose.editor.sel.caret;
    let caret_block = caret.block.min(text.blocks.len() - 1);

    let lines: Vec<Line> = (0..text.blocks.len())
        .map(|i| {
            let caret_here = focused && i == caret_block;
            block_line(text, i, caret_here.then_some(caret))
        })
        .collect();

    // Keep the caret's block visible: scroll just far enough that it's the
    // last visible row once the buffer grows past the pane's height.
    let body_h = inner.height.max(1) as usize;
    let scroll_top = caret_block.saturating_sub(body_h.saturating_sub(1));

    f.render_widget(Paragraph::new(lines).scroll((scroll_top as u16, 0)), inner);

    if focused {
        let row = caret_block.saturating_sub(scroll_top) as u16;
        if row < inner.height && inner.width > 0 {
            let col = caret_col(text, caret_block, caret).min(inner.width as usize - 1);
            f.set_cursor_position(Position {
                x: inner.x + col as u16,
                y: inner.y + row,
            });
        }
    }
}

/// The runs of block `idx` — a paragraph's or a list item's, whichever it
/// is. Callers keep `idx` in bounds (checked against `text.blocks.len()`
/// once, up front, at every entry point into this module's rendering).
fn block_runs(text: &RichText, idx: usize) -> &[Run] {
    match &text.blocks[idx] {
        Block::Paragraph(runs) => runs,
        Block::ListItem { runs, .. } => runs,
    }
}

/// The list marker prefix for block `idx` — empty for a `Paragraph`,
/// `"  "×level + "• "` for an unordered item, `"  "×level + "N. "` for an
/// ordered item (`N` counted back over the contiguous run of same-level
/// ordered items immediately preceding it — mirroring how
/// `mailcore::compose_html::to_html` groups consecutive same-`ordered`
/// items into one `<ol>`/`<ul>`).
fn block_prefix(text: &RichText, idx: usize) -> String {
    match &text.blocks[idx] {
        Block::Paragraph(_) => String::new(),
        Block::ListItem { ordered, level, .. } => {
            let indent = "  ".repeat(*level as usize);
            if *ordered {
                format!("{indent}{}. ", ordinal(text, idx, *level))
            } else {
                format!("{indent}\u{2022} ")
            }
        }
    }
}

/// How many contiguous same-level ordered items (counting backwards from,
/// and including, `idx`) precede it — i.e. `idx`'s 1-based position within
/// its list.
fn ordinal(text: &RichText, idx: usize, level: u8) -> usize {
    let mut n = 1;
    let mut i = idx;
    while i > 0 {
        i -= 1;
        match &text.blocks[i] {
            Block::ListItem {
                ordered: true,
                level: l,
                ..
            } if *l == level => n += 1,
            _ => break,
        }
    }
    n
}

/// Maps one `Run`'s bold/italic/underline/link flags to a ratatui `Style` —
/// mirrors `ui::reading::to_ratatui_span`'s mapping for the read-only body
/// (same modifiers, same cyan link color), but over an editable
/// `editcore::model::Run` instead of `mailcore::htmlrender`'s `StyledSpan`.
fn run_style(run: &Run) -> Style {
    let mut style = Style::default();
    if run.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if run.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if run.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if run.link.is_some() {
        style = style.fg(Color::Cyan);
    }
    style
}

/// Clamps `pos` into a valid `(run, offset)` within `runs` (non-empty —
/// callers check first), then — if the offset lands exactly at the end of
/// a run that has a following run — normalizes it to `(run + 1, 0)`. Both
/// forms address the same visual caret position (right between two runs);
/// picking the latter uniformly means the caret `block_line` renders is
/// always either mid-run or at the very end of the block's last run, never
/// ambiguously "between runs".
fn caret_run_offset(runs: &[Run], pos: Pos) -> (usize, usize) {
    let run = pos.run.min(runs.len() - 1);
    let text = &runs[run].text;
    let mut off = pos.offset.min(text.len());
    while off > 0 && !text.is_char_boundary(off) {
        off -= 1;
    }
    if off == text.len() && run + 1 < runs.len() {
        (run + 1, 0)
    } else {
        (run, off)
    }
}

/// The 0-based display column of `caret` within block `block`'s rendered
/// line: the prefix's width, plus the width of every run before the
/// caret's (normalized) run, plus the width of that run's text up to its
/// offset. Used only to place the real terminal cursor
/// (`set_cursor_position`); `block_line`'s in-buffer caret highlight is
/// computed independently from the same clamped/normalized position.
fn caret_col(text: &RichText, block: usize, caret: Pos) -> usize {
    let prefix = block_prefix(text, block);
    let mut col = UnicodeWidthStr::width(prefix.as_str());
    let runs = block_runs(text, block);
    if runs.is_empty() {
        return col;
    }
    let (run, off) = caret_run_offset(runs, caret);
    for r in &runs[..run] {
        col += UnicodeWidthStr::width(r.text.as_str());
    }
    col += UnicodeWidthStr::width(&runs[run].text[..off]);
    col
}

/// One rendered `Line` for block `idx`: the list-marker prefix (dim gray)
/// followed by its runs mapped to styled `Span`s. When `caret` is `Some`,
/// bakes a reversed-style one-cell highlight into whichever run contains it
/// (via `caret_run_offset`) — a caret on a block with no runs at all (an
/// empty paragraph) gets a single reversed space instead.
fn block_line(text: &RichText, idx: usize, caret: Option<Pos>) -> Line<'static> {
    let runs = block_runs(text, idx);
    let mut spans = Vec::with_capacity(runs.len() + 1);
    let prefix = block_prefix(text, idx);
    if !prefix.is_empty() {
        spans.push(Span::styled(prefix, Style::default().fg(Color::DarkGray)));
    }
    if runs.is_empty() {
        if caret.is_some() {
            spans.push(cursor_span(" "));
        }
        return Line::from(spans);
    }
    let caret_at = caret.map(|pos| caret_run_offset(runs, pos));
    for (ri, run) in runs.iter().enumerate() {
        let style = run_style(run);
        match caret_at {
            Some((cr, coff)) if cr == ri => {
                let before = &run.text[..coff];
                if !before.is_empty() {
                    spans.push(Span::styled(before.to_string(), style));
                }
                let rest = &run.text[coff..];
                if let Some(c) = rest.chars().next() {
                    let clen = c.len_utf8();
                    spans.push(Span::styled(
                        c.to_string(),
                        style.add_modifier(Modifier::REVERSED),
                    ));
                    let after = &rest[clen..];
                    if !after.is_empty() {
                        spans.push(Span::styled(after.to_string(), style));
                    }
                } else {
                    spans.push(cursor_span_styled(style));
                }
            }
            _ => spans.push(Span::styled(run.text.clone(), style)),
        }
    }
    Line::from(spans)
}

/// A single reversed-style space — the caret glyph used on an empty block.
fn cursor_span(s: &'static str) -> Span<'static> {
    Span::styled(s, Style::default().add_modifier(Modifier::REVERSED))
}

/// A single reversed-style space carrying `style`'s other attributes (bold
/// etc.) — the caret glyph used at the very end of a non-empty run.
fn cursor_span_styled(style: Style) -> Span<'static> {
    Span::styled(" ", style.add_modifier(Modifier::REVERSED))
}

/// The compose view's key handling: Tab cycles fields; printable characters
/// and Backspace edit whichever field has focus (in `Body`, they drive the
/// `editcore` ops instead of a plain string — Enter splits the paragraph,
/// arrows move the caret, extending the selection when Shift is held).
/// Ctrl-B/I/U/L drive the style/list ops, only while `Body` has focus (they
/// have no meaning over a plain header field). Ctrl-Enter/Esc/Ctrl-D don't
/// act directly — see the module doc comment — they just record
/// `App::compose_action`, checked (and cleared) by the next task's wiring.
/// Called from `ui::handle_key` whenever `app.compose` is open, ahead of
/// every other popup.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    if app.compose.is_none() {
        return;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    if ctrl && key.code == KeyCode::Enter {
        app.compose_action = Some(ComposeAction::Send);
        return;
    }
    if ctrl && matches!(key.code, KeyCode::Char('d') | KeyCode::Char('D')) {
        app.compose_action = Some(ComposeAction::Discard);
        return;
    }
    if key.code == KeyCode::Esc {
        app.compose_action = Some(ComposeAction::Save);
        return;
    }

    let Some(compose) = app.compose.as_mut() else {
        return;
    };

    if ctrl {
        if compose.focus == ComposeField::Body {
            if let KeyCode::Char(c) = key.code {
                match c.to_ascii_lowercase() {
                    'b' => compose.editor.toggle_bold(),
                    'i' => compose.editor.toggle_italic(),
                    'u' => compose.editor.toggle_underline(),
                    'l' => compose.editor.list_toggle(false),
                    _ => {}
                }
            }
        }
        return;
    }

    let extend = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Tab => cycle_focus(compose),
        KeyCode::Char(c) => match compose.focus {
            ComposeField::To => compose.to.push(c),
            ComposeField::Cc => compose.cc.push(c),
            ComposeField::Subject => compose.subject.push(c),
            ComposeField::Body => compose.editor.insert_text(&c.to_string()),
        },
        KeyCode::Backspace => match compose.focus {
            ComposeField::To => {
                compose.to.pop();
            }
            ComposeField::Cc => {
                compose.cc.pop();
            }
            ComposeField::Subject => {
                compose.subject.pop();
            }
            ComposeField::Body => compose.editor.delete_backward(),
        },
        KeyCode::Enter if compose.focus == ComposeField::Body => compose.editor.split_paragraph(),
        KeyCode::Left if compose.focus == ComposeField::Body => compose.editor.move_left(extend),
        KeyCode::Right if compose.focus == ComposeField::Body => compose.editor.move_right(extend),
        KeyCode::Up if compose.focus == ComposeField::Body => compose.editor.move_up(extend),
        KeyCode::Down if compose.focus == ComposeField::Body => compose.editor.move_down(extend),
        KeyCode::Home if compose.focus == ComposeField::Body => compose.editor.move_home(extend),
        KeyCode::End if compose.focus == ComposeField::Body => compose.editor.move_end(extend),
        _ => {}
    }
}

/// `To` → `Cc` → `Subject` → `Body` → `To`.
fn cycle_focus(compose: &mut Compose) {
    compose.focus = match compose.focus {
        ComposeField::To => ComposeField::Cc,
        ComposeField::Cc => ComposeField::Subject,
        ComposeField::Subject => ComposeField::Body,
        ComposeField::Body => ComposeField::To,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use editcore::cursor::Selection;
    use ratatui::{Terminal, backend::TestBackend};

    /// A composer pre-filled the way opening a draft (a later task's
    /// concern) would leave it: header fields set, and a one-paragraph body
    /// already typed in — the "seeded draft" the brief's Step 1 test asks
    /// for, built directly (no store round-trip; that's the next task).
    fn seeded_compose() -> Compose {
        let mut editor = Editor::new();
        editor.insert_text("Hello body");
        Compose {
            to: "alice@example.com".into(),
            cc: String::new(),
            subject: "Re: Hi".into(),
            editor,
            focus: ComposeField::Body,
            draft_id: "d1".into(),
        }
    }

    fn render_text(app: &App) -> String {
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw_compose(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn draw_compose_renders_fields_and_body_without_panic() {
        let mut app = App::for_test_with_seeded_store();
        app.compose = Some(seeded_compose());

        let text = render_text(&app);

        assert!(text.contains("alice@example.com"));
        assert!(text.contains("Re: Hi"));
        assert!(text.contains("Hello body"));
    }

    #[test]
    fn empty_compose_renders_without_panic() {
        let mut app = App::for_test_with_seeded_store();
        app.compose = Some(Compose::new("new".into()));

        let text = render_text(&app);

        assert!(text.contains("To"));
        assert!(text.contains("Subject"));
    }

    #[test]
    fn draw_compose_is_a_no_op_when_compose_is_closed() {
        let app = App::for_test_with_seeded_store();
        assert!(app.compose.is_none());
        let text = render_text(&app);
        assert!(!text.contains("Send: Ctrl-Enter"));
    }

    #[test]
    fn typing_a_char_in_body_appends_to_editor_text() {
        let mut app = App::for_test_with_seeded_store();
        app.compose = Some(seeded_compose()); // focus already Body

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('!')));

        assert!(
            app.compose
                .as_ref()
                .unwrap()
                .editor
                .text
                .plain()
                .ends_with('!')
        );
    }

    #[test]
    fn ctrl_b_toggles_bold_on_a_selection() {
        let mut app = App::for_test_with_seeded_store();
        let mut compose = seeded_compose();
        compose.editor.sel = Selection {
            anchor: Pos {
                block: 0,
                run: 0,
                offset: 0,
            },
            caret: Pos {
                block: 0,
                run: 0,
                offset: 5,
            },
        }; // select "Hello"
        app.compose = Some(compose);

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        );

        let runs = match &app.compose.as_ref().unwrap().editor.text.blocks[0] {
            Block::Paragraph(r) => r,
            _ => panic!("expected a paragraph"),
        };
        assert!(runs.iter().any(|r| r.bold && r.text.contains("Hello")));
    }

    #[test]
    fn tab_cycles_focus_to_cc_subject_body() {
        let mut app = App::for_test_with_seeded_store();
        app.compose = Some(Compose::new("new".into()));
        assert_eq!(app.compose.as_ref().unwrap().focus, ComposeField::To);

        handle_key(&mut app, KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.compose.as_ref().unwrap().focus, ComposeField::Cc);

        handle_key(&mut app, KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.compose.as_ref().unwrap().focus, ComposeField::Subject);

        handle_key(&mut app, KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.compose.as_ref().unwrap().focus, ComposeField::Body);

        handle_key(&mut app, KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.compose.as_ref().unwrap().focus, ComposeField::To);
    }

    #[test]
    fn printable_and_backspace_edit_the_to_field() {
        let mut app = App::for_test_with_seeded_store();
        app.compose = Some(Compose::new("new".into()));

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('a')));
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('b')));
        assert_eq!(app.compose.as_ref().unwrap().to, "ab");

        handle_key(&mut app, KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.compose.as_ref().unwrap().to, "a");
    }

    #[test]
    fn enter_in_body_splits_the_paragraph() {
        let mut app = App::for_test_with_seeded_store();
        app.compose = Some(seeded_compose());

        handle_key(&mut app, KeyEvent::from(KeyCode::Enter));

        assert_eq!(app.compose.as_ref().unwrap().editor.text.blocks.len(), 2);
    }

    #[test]
    fn ctrl_l_toggles_the_current_block_into_a_bulleted_list_item() {
        let mut app = App::for_test_with_seeded_store();
        app.compose = Some(seeded_compose());

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
        );

        assert!(matches!(
            app.compose.as_ref().unwrap().editor.text.blocks[0],
            Block::ListItem { ordered: false, .. }
        ));
    }

    #[test]
    fn esc_requests_save_and_ctrl_enter_requests_send_and_ctrl_d_requests_discard() {
        let mut app = App::for_test_with_seeded_store();
        app.compose = Some(Compose::new("d1".into()));
        assert!(app.compose_action.is_none());

        handle_key(&mut app, KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.compose_action, Some(ComposeAction::Save));
        // This module only *records* the request — it must not close the
        // composer itself; that's the next task's job.
        assert!(app.compose.is_some());

        app.compose_action = None;
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
        );
        assert_eq!(app.compose_action, Some(ComposeAction::Send));

        app.compose_action = None;
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.compose_action, Some(ComposeAction::Discard));
    }

    #[test]
    fn handle_key_is_a_no_op_when_compose_is_closed() {
        let mut app = App::for_test_with_seeded_store();
        assert!(app.compose.is_none());
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('x'))); // must not panic
        assert!(app.compose.is_none());
    }

    #[test]
    fn empty_compose_boundary_keys_do_not_panic() {
        let mut app = App::for_test_with_seeded_store();
        let mut compose = Compose::new("new".into());
        compose.focus = ComposeField::Body;
        app.compose = Some(compose);

        let keys = [
            KeyCode::Backspace,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Enter,
        ];
        for code in keys {
            handle_key(&mut app, KeyEvent::from(code));
        }
        // Must render cleanly too — an empty/boundary-mutated buffer still
        // has to draw without panicking.
        let _ = render_text(&app);
    }

    #[test]
    fn list_item_with_no_runs_renders_a_caret_without_panicking() {
        // A list item whose only run was deleted down to nothing still has
        // to render (and place a caret on) an empty block.
        let mut app = App::for_test_with_seeded_store();
        let mut compose = Compose::new("new".into());
        compose.editor = Editor::from(RichText {
            blocks: vec![Block::ListItem {
                ordered: true,
                level: 1,
                runs: vec![],
            }],
        });
        compose.focus = ComposeField::Body;
        app.compose = Some(compose);

        let text = render_text(&app);
        assert!(text.contains('1'));
    }
}
