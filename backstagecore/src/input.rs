//! Event-returning key/mouse handlers for [`crate::Backstage`], ported from
//! docxy's `main.rs` `backstage_key`/`bs_mouse`/`bs_menu_activate`/
//! `save_as_key`/`save_as_name_key`/`save_as_browser_key`/`bs_scroll_preview`.

use crate::{Backstage, BackstageEvent, BackstageHost, ITEMS, Item, Pane};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Position;

impl Backstage {
    /// Handle a key while the backstage panel is open. `Esc` always closes it;
    /// otherwise the active pane (menu / folder browser / preview / Save As)
    /// interprets the key and the caller acts on the returned event.
    pub fn key(&mut self, key: KeyEvent, host: &dyn BackstageHost) -> BackstageEvent {
        if key.code == KeyCode::Esc {
            return BackstageEvent::Close;
        }
        // The Save As dialog has its own typed-filename handling.
        if self.pane == Pane::SaveAs {
            return self.save_as_key(key);
        }
        match self.pane {
            Pane::Menu => match key.code {
                KeyCode::Up => {
                    self.menu_move(false);
                    BackstageEvent::None
                }
                KeyCode::Down => {
                    self.menu_move(true);
                    BackstageEvent::None
                }
                KeyCode::Enter | KeyCode::Right => self.menu_activate(host),
                _ => BackstageEvent::None,
            },
            Pane::Browser => match key.code {
                KeyCode::Up => {
                    self.move_sel(false);
                    self.refresh_preview(host, self.preview_w);
                    BackstageEvent::None
                }
                KeyCode::Down => {
                    self.move_sel(true);
                    self.refresh_preview(host, self.preview_w);
                    BackstageEvent::None
                }
                KeyCode::Enter => {
                    if let Some(path) = self.enter() {
                        BackstageEvent::Open(path)
                    } else {
                        self.refresh_preview(host, self.preview_w);
                        BackstageEvent::None
                    }
                }
                KeyCode::Backspace => {
                    self.go_up();
                    self.refresh_preview(host, self.preview_w);
                    BackstageEvent::None
                }
                KeyCode::Left => {
                    self.pane = Pane::Menu;
                    BackstageEvent::None
                }
                // Step right into the read-only preview to scroll it.
                KeyCode::Right | KeyCode::Tab => {
                    if !self.preview.is_empty() {
                        self.pane = Pane::Preview;
                    }
                    BackstageEvent::None
                }
                _ => BackstageEvent::None,
            },
            Pane::Preview => {
                let page = self.layout.preview_h.saturating_sub(1).max(1) as isize;
                match key.code {
                    KeyCode::Up => {
                        self.scroll_preview(-1);
                        BackstageEvent::None
                    }
                    KeyCode::Down => {
                        self.scroll_preview(1);
                        BackstageEvent::None
                    }
                    KeyCode::PageUp => {
                        self.scroll_preview(-page);
                        BackstageEvent::None
                    }
                    KeyCode::PageDown => {
                        self.scroll_preview(page);
                        BackstageEvent::None
                    }
                    KeyCode::Home => {
                        self.scroll_preview(isize::MIN / 2);
                        BackstageEvent::None
                    }
                    KeyCode::End => {
                        self.scroll_preview(isize::MAX / 2);
                        BackstageEvent::None
                    }
                    KeyCode::Left | KeyCode::Tab => {
                        self.pane = Pane::Browser;
                        BackstageEvent::None
                    }
                    _ => BackstageEvent::None,
                }
            }
            // Handled above by save_as_key; here only to keep the match total.
            Pane::SaveAs => BackstageEvent::None,
        }
    }

    /// Activate the highlighted menu item.
    fn menu_activate(&mut self, host: &dyn BackstageHost) -> BackstageEvent {
        match self.item {
            Item::Open => {
                self.pane = Pane::Browser;
                self.refresh_preview(host, self.preview_w);
                BackstageEvent::None
            }
            // the Info pane is shown on the right; nothing to do
            Item::Info => BackstageEvent::None,
            Item::Save => BackstageEvent::Save,
            Item::SaveAs => {
                // Prefill the current file's name with the caret at its end.
                let name = host.default_save_name();
                self.name_cursor = name.chars().count();
                self.name_input = name;
                self.name_focus = true;
                self.pane = Pane::SaveAs;
                BackstageEvent::None
            }
            Item::New => BackstageEvent::New,
            Item::Export => BackstageEvent::Export,
            Item::Exit => BackstageEvent::Exit,
        }
    }

    /// Keys for the Save As dialog. Tab moves focus between the file-name field
    /// and the folder browser; each piece only reacts when it's focused. Enter
    /// commits the Save As (Esc, handled by the caller, cancels).
    fn save_as_key(&mut self, key: KeyEvent) -> BackstageEvent {
        match key.code {
            KeyCode::Enter => {
                return BackstageEvent::SaveAs {
                    dir: self.dir.clone(),
                    name: self.name_input.trim().to_string(),
                };
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.name_focus = !self.name_focus;
                return BackstageEvent::None;
            }
            _ => {}
        }
        if self.name_focus {
            self.save_as_name_key(key);
        } else {
            self.save_as_browser_key(key);
        }
        BackstageEvent::None
    }

    /// Editing keys while the file-name field is focused.
    fn save_as_name_key(&mut self, key: KeyEvent) {
        let len = self.name_input.chars().count();
        match key.code {
            KeyCode::Char(c) => {
                let at = byte_index(&self.name_input, self.name_cursor);
                self.name_input.insert(at, c);
                self.name_cursor += 1;
            }
            KeyCode::Backspace => {
                if self.name_cursor > 0 {
                    let start = byte_index(&self.name_input, self.name_cursor - 1);
                    let end = byte_index(&self.name_input, self.name_cursor);
                    self.name_input.replace_range(start..end, "");
                    self.name_cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if self.name_cursor < len {
                    let start = byte_index(&self.name_input, self.name_cursor);
                    let end = byte_index(&self.name_input, self.name_cursor + 1);
                    self.name_input.replace_range(start..end, "");
                }
            }
            KeyCode::Left => self.name_cursor = self.name_cursor.saturating_sub(1),
            KeyCode::Right => self.name_cursor = (self.name_cursor + 1).min(len),
            KeyCode::Home => self.name_cursor = 0,
            KeyCode::End => self.name_cursor = len,
            _ => {}
        }
    }

    /// Navigation keys while the folder browser is focused (Save As dialog).
    /// Picking a file copies its name into the field and returns focus there.
    fn save_as_browser_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.move_sel(false),
            KeyCode::Down => self.move_sel(true),
            KeyCode::Left => self.go_up(),
            KeyCode::Right => {
                if self.selected().map(|e| e.is_dir).unwrap_or(false) {
                    let _ = self.enter();
                } else if let Some(e) = self.selected() {
                    let name = e.name.clone();
                    self.name_cursor = name.chars().count();
                    self.name_input = name;
                    self.name_focus = true;
                }
            }
            _ => {}
        }
    }

    /// Handle a mouse click at `(x, y)` within the backstage panel. The caller
    /// is responsible for the tab-strip row (`y == 0`) — this is only ever
    /// invoked for `y >= 1`.
    pub fn mouse(&mut self, x: u16, y: u16, host: &dyn BackstageHost) -> BackstageEvent {
        // Left menu column. Every item acts on a single click: Open / Save As /
        // Info switch the right pane (Save As prefills the name to type), while
        // Save / Export / Exit run straight away. Only New is guarded: it
        // discards the current document without a prompt, so it needs a
        // confirming second click on the already-selected row.
        if x < 14 {
            if y >= 1 {
                let idx = (y - 1) as usize;
                if idx < ITEMS.len() {
                    let it = ITEMS[idx];
                    let cur = self.item;
                    let guarded = matches!(it, Item::New);
                    self.item = it;
                    let ev = if !guarded || cur == it {
                        self.menu_activate(host)
                    } else {
                        self.pane = Pane::Menu;
                        BackstageEvent::None
                    };
                    self.refresh_preview(host, self.preview_w);
                    return ev;
                }
            }
            return BackstageEvent::None;
        }
        // The Save As dialog has two pieces; clicking one focuses it and
        // deactivates the other.
        if self.pane == Pane::SaveAs {
            // The Save button (a clickable Enter).
            if self.layout.save_btn.contains(Position { x, y }) {
                return BackstageEvent::SaveAs {
                    dir: self.dir.clone(),
                    name: self.name_input.trim().to_string(),
                };
            }
            // Click inside the name box: focus the field and drop the caret at
            // the clicked character.
            if y >= self.layout.name_top {
                self.name_focus = true;
                let off = x.saturating_sub(self.layout.name_x0) as usize;
                self.name_cursor = off.min(self.name_input.chars().count());
                return BackstageEvent::None;
            }
            // Click in the folder list: focus the browser (hiding the name
            // caret) and select the row. A folder steps in on a second click;
            // a file copies its name into the field as an overwrite target.
            if y < 2 {
                return BackstageEvent::None;
            }
            let idx = self.layout.list_start + (y - 2) as usize;
            if idx < self.entries.len() {
                let was_sel = idx == self.sel;
                self.name_focus = false;
                self.sel = idx;
                let is_dir = self.entries[idx].is_dir;
                if is_dir && was_sel {
                    let _ = self.enter();
                } else if !is_dir {
                    self.name_input = self.entries[idx].name.clone();
                    self.name_cursor = self.name_input.chars().count();
                }
            }
            return BackstageEvent::None;
        }
        // The list/preview only exist for the Open item.
        if self.item != Item::Open {
            return BackstageEvent::None;
        }
        if x < 48 {
            // File list: rows start below the box's top border (body y=1, +1).
            if y < 2 {
                return BackstageEvent::None;
            }
            let row = (y - 2) as usize;
            let idx = self.layout.list_start + row;
            if idx >= self.entries.len() {
                return BackstageEvent::None;
            }
            if idx == self.sel {
                // Second click on the highlighted row activates it.
                if let Some(path) = self.enter() {
                    BackstageEvent::Open(path)
                } else {
                    self.refresh_preview(host, self.preview_w);
                    BackstageEvent::None
                }
            } else {
                self.sel = idx;
                self.pane = Pane::Browser;
                self.refresh_preview(host, self.preview_w);
                BackstageEvent::None
            }
        } else {
            // Click in the preview gives it focus so the wheel/keys scroll it.
            self.pane = Pane::Preview;
            BackstageEvent::None
        }
    }

    /// Scroll the read-only preview by `delta` lines, clamped to its content.
    pub fn scroll_preview(&mut self, delta: isize) {
        let h = self.layout.preview_h.max(1);
        let max = self.preview.len().saturating_sub(h) as isize;
        let new = (self.preview_scroll as isize + delta).clamp(0, max.max(0));
        self.preview_scroll = new as usize;
    }

    /// Re-render the preview of the highlighted file if the selection or the
    /// render width changed since the last call; clears it when nothing
    /// openable is selected.
    pub fn refresh_preview(&mut self, host: &dyn BackstageHost, width: usize) {
        let sel = self.selected_file();
        if sel == self.preview_path && width == self.preview_w {
            return;
        }
        if sel != self.preview_path {
            self.preview_scroll = 0; // a new file starts at the top
        }
        self.preview_w = width;
        self.preview = match &sel {
            Some(path) => host.preview_lines(path, width),
            None => Vec::new(),
        };
        self.preview_path = sel;
    }
}

/// Byte offset of char index `char_idx` in `s` (its length if past the end).
fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Backstage, Item, Pane};
    use ratatui::crossterm::event::{KeyCode, KeyEvent};
    use ratatui::style::Color;
    use ratatui::text::Line;
    use std::path::Path;

    struct TestHost;
    impl BackstageHost for TestHost {
        fn extensions(&self) -> &'static [&'static str] {
            &["docx"]
        }
        fn default_save_name(&self) -> String {
            "untitled.docx".into()
        }
        fn preview_lines(&self, _p: &Path, _w: usize) -> Vec<String> {
            vec!["preview".into()]
        }
        fn info_lines(&self) -> Vec<Line<'static>> {
            vec![Line::raw("info")]
        }
        fn accent(&self) -> Color {
            Color::Cyan
        }
    }
    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::from(c)
    }

    #[test]
    fn esc_closes() {
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        assert!(matches!(
            bs.key(key(KeyCode::Esc), &TestHost),
            BackstageEvent::Close
        ));
    }

    #[test]
    fn save_item_emits_save_event() {
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        bs.item = Item::Save;
        bs.pane = Pane::Menu;
        assert!(matches!(
            bs.key(key(KeyCode::Enter), &TestHost),
            BackstageEvent::Save
        ));
    }

    #[test]
    fn save_as_item_opens_dialog_prefilled() {
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        bs.item = Item::SaveAs;
        bs.pane = Pane::Menu;
        let e = bs.key(key(KeyCode::Enter), &TestHost);
        assert!(matches!(e, BackstageEvent::None));
        assert_eq!(bs.pane, Pane::SaveAs);
        assert_eq!(bs.name_input, "untitled.docx");
        assert!(bs.name_focus);
    }

    #[test]
    fn save_as_typing_edits_name_and_commits() {
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        bs.pane = Pane::SaveAs;
        bs.name_focus = true;
        bs.name_input.clear();
        bs.name_cursor = 0;
        for c in "ab".chars() {
            bs.key(key(KeyCode::Char(c)), &TestHost);
        }
        bs.key(key(KeyCode::Backspace), &TestHost);
        assert_eq!(bs.name_input, "a");
        let e = bs.key(key(KeyCode::Enter), &TestHost);
        match e {
            BackstageEvent::SaveAs { name, .. } => assert_eq!(name, "a"),
            _ => panic!("{e:?}"),
        }
    }

    #[test]
    fn guarded_new_needs_second_activation_via_mouse() {
        // First click on New (not yet selected) selects it but does NOT fire.
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        bs.item = Item::Open;
        bs.layout.list_start = 0;
        // menu column is x<14; New is row idx 0 → y=1
        let first = bs.mouse(2, 1, &TestHost);
        assert!(matches!(first, BackstageEvent::None));
        assert_eq!(bs.item, Item::New);
        let second = bs.mouse(2, 1, &TestHost);
        assert!(matches!(second, BackstageEvent::New));
    }
}
