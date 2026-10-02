//! Event-returning key/mouse handlers for [`crate::Backstage`], ported from
//! docxy's `main.rs` `backstage_key`/`bs_mouse`/`bs_menu_activate`/
//! `save_as_key`/`save_as_name_key`/`save_as_browser_key`/`bs_scroll_preview`.

use crate::{Backstage, BackstageEvent, BackstageHost, Item, OptLine, OptValue, Pane};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Position;

/// How many editable rows the host's Info page has.
pub(crate) fn info_rows(host: &dyn BackstageHost) -> usize {
    host.info_fields().len() + usize::from(host.info_custom_row())
}

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
            Pane::Export => {
                let last = self.save_types.len() + self.export_extra.len();
                match key.code {
                    KeyCode::Up => self.export_sel = self.export_sel.saturating_sub(1),
                    KeyCode::Down => self.export_sel = (self.export_sel + 1).min(last),
                    KeyCode::Enter => return self.export_activate(host),
                    KeyCode::Left => self.pane = Pane::Menu,
                    _ => {}
                }
                BackstageEvent::None
            }
            Pane::Options => {
                // A checkbox has no value to move, so Left still goes back to
                // the menu from one; a choice or a number takes Left/Right.
                let value_row = self
                    .options
                    .get(self.option_sel)
                    .is_some_and(|o| !matches!(o.value, OptValue::Check(_)));
                let int_row = self
                    .options
                    .get(self.option_sel)
                    .is_some_and(|o| matches!(o.value, OptValue::Int { .. }));
                match key.code {
                    KeyCode::Up => self.option_sel = self.option_sel.saturating_sub(1),
                    KeyCode::Down => {
                        self.option_sel =
                            (self.option_sel + 1).min(self.options.len().saturating_sub(1))
                    }
                    KeyCode::Char(' ') | KeyCode::Enter => self.toggle_option(),
                    KeyCode::Right if value_row => self.step_option(1),
                    KeyCode::Left if value_row => self.step_option(-1),
                    KeyCode::PageUp if int_row => self.step_option(10),
                    KeyCode::PageDown if int_row => self.step_option(-10),
                    KeyCode::Left => self.pane = Pane::Menu,
                    _ => {}
                }
                BackstageEvent::None
            }
            Pane::Info => {
                let rows = info_rows(host);
                match key.code {
                    KeyCode::Up => self.info_sel = self.info_sel.saturating_sub(1),
                    KeyCode::Down => {
                        self.info_sel = (self.info_sel + 1).min(rows.saturating_sub(1))
                    }
                    KeyCode::Enter if self.info_sel < rows => {
                        return BackstageEvent::EditInfo(self.info_sel);
                    }
                    KeyCode::Left => self.pane = Pane::Menu,
                    _ => {}
                }
                BackstageEvent::None
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
            // The Info pane is shown on the right; a host with editable
            // properties lets the keyboard into it.
            Item::Info => {
                let rows = info_rows(host);
                if rows > 0 {
                    self.pane = Pane::Info;
                    self.info_sel = self.info_sel.min(rows - 1);
                }
                BackstageEvent::None
            }
            Item::Save => BackstageEvent::Save,
            Item::SaveAs => {
                // Prefill the current file's name with the caret at its end,
                // and the type the host bound it to.
                self.begin_save_as(host.default_save_name(), None);
                if let Some(t) = host.default_save_type() {
                    self.preset(t);
                }
                BackstageEvent::None
            }
            Item::New => BackstageEvent::New,
            // With a type list, Export is a page: the host's quick export,
            // then Change File Type.
            Item::Export if !self.save_types.is_empty() => {
                self.pane = Pane::Export;
                BackstageEvent::None
            }
            Item::Export => BackstageEvent::Export,
            Item::Options => {
                self.pane = Pane::Options;
                BackstageEvent::None
            }
            Item::Exit => BackstageEvent::Exit,
        }
    }

    /// Run Export's highlighted row: the quick export
    /// ([`BackstageEvent::Export`]), one of the host's extra exports
    /// ([`BackstageEvent::ExportExtra`]), or Change File Type (Save As with
    /// that type picked).
    fn export_activate(&mut self, host: &dyn BackstageHost) -> BackstageEvent {
        let extra = self.export_extra.len();
        match self.export_sel.checked_sub(1) {
            None => BackstageEvent::Export,
            Some(i) if i < extra => BackstageEvent::ExportExtra(i),
            Some(i) => {
                self.begin_save_as(host.default_save_name(), Some(i - extra));
                BackstageEvent::None
            }
        }
    }

    /// Keys for the Save As dialog. Tab moves focus between the file-name field,
    /// the *Save as type* box (when the host has one) and the folder browser;
    /// each piece only reacts when it's focused. Enter commits the Save As
    /// (Esc, handled by the caller, cancels).
    fn save_as_key(&mut self, key: KeyEvent) -> BackstageEvent {
        match key.code {
            KeyCode::Enter => {
                return BackstageEvent::SaveAs {
                    dir: self.dir.clone(),
                    name: self.name_input.trim().to_string(),
                };
            }
            KeyCode::Tab | KeyCode::BackTab if self.save_types.is_empty() => {
                self.name_focus = !self.name_focus;
                return BackstageEvent::None;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                // name → type → folders → name (reversed with Shift+Tab).
                let at = if self.name_focus {
                    0
                } else if self.type_focus {
                    1
                } else {
                    2
                };
                let next = if key.code == KeyCode::Tab {
                    (at + 1) % 3
                } else {
                    (at + 2) % 3
                };
                self.name_focus = next == 0;
                self.type_focus = next == 1;
                return BackstageEvent::None;
            }
            _ => {}
        }
        if self.type_focus {
            match key.code {
                KeyCode::Up => self.pick_type(self.type_sel.saturating_sub(1)),
                KeyCode::Down => self.pick_type((self.type_sel + 1).min(self.save_types.len() - 1)),
                _ => {}
            }
            return BackstageEvent::None;
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
                if idx < self.items().len() {
                    let it = self.items()[idx];
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
        // The Save As dialog has three pieces (the folder list, the name box
        // and, when the host has types, the type box); clicking one focuses
        // it and deactivates the others.
        if self.pane == Pane::SaveAs {
            // The Save button (a clickable Enter).
            if self.layout.save_btn.contains(Position { x, y }) {
                return BackstageEvent::SaveAs {
                    dir: self.dir.clone(),
                    name: self.name_input.trim().to_string(),
                };
            }
            // Click in the type box: focus it (↑↓ then change the type).
            if !self.save_types.is_empty() && y >= self.layout.type_top {
                self.name_focus = false;
                self.type_focus = true;
                return BackstageEvent::None;
            }
            // Click inside the name box: focus the field and drop the caret at
            // the clicked character.
            if y >= self.layout.name_top {
                self.name_focus = true;
                self.type_focus = false;
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
                self.type_focus = false;
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
        // Export's rows: the quick export at y 3, the extra exports under it,
        // the types three rows after those (see `draw_export`). A click
        // selects; a click on the selection runs it.
        if self.item == Item::Export && !self.save_types.is_empty() {
            let extra = self.export_extra.len();
            let types_y = 6 + extra as u16;
            let row = match y {
                y if (3..3 + 1 + extra as u16).contains(&y) => Some((y - 3) as usize),
                y if y >= types_y => {
                    Some(self.layout.list_start + (y - types_y) as usize + 1 + extra)
                        .filter(|&r| r <= self.save_types.len() + extra)
                }
                _ => None,
            };
            if let Some(r) = row {
                let again = self.pane == Pane::Export && r == self.export_sel;
                self.pane = Pane::Export;
                self.export_sel = r;
                if again {
                    return self.export_activate(host);
                }
            }
            return BackstageEvent::None;
        }
        // An editable Info row: a click selects it, a click on the selection
        // edits it.
        if self.item == Item::Info {
            let rows = info_rows(host);
            let (top, end) = self.layout.info_view;
            // Only the box's inside: a row scrolled under a border is hidden.
            let row = (top..end)
                .contains(&y)
                .then(|| i32::from(y) - self.layout.info_top)
                .and_then(|i| usize::try_from(i).ok())
                .filter(|&i| i < rows);
            if let Some(i) = row {
                if self.pane == Pane::Info && self.info_sel == i {
                    return BackstageEvent::EditInfo(i);
                }
                self.info_sel = i;
                self.pane = Pane::Info;
            }
            return BackstageEvent::None;
        }
        // A click on an option row flips its checkbox or moves its value
        // on; the page's lines start below the panel's top edge.
        if self.item == Item::Options {
            let line = (y as usize)
                .checked_sub(1)
                .and_then(|l| self.option_lines().get(l).copied());
            if let Some(OptLine::Row(i)) = line {
                self.option_sel = i;
                self.pane = Pane::Options;
                self.toggle_option();
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

    /// A host with two editable Info rows and the custom-property row.
    struct EditableHost;
    impl BackstageHost for EditableHost {
        fn extensions(&self) -> &'static [&'static str] {
            &["xlsx"]
        }
        fn default_save_name(&self) -> String {
            "book.xlsx".into()
        }
        fn preview_lines(&self, _p: &Path, _w: usize) -> Vec<String> {
            Vec::new()
        }
        fn info_lines(&self) -> Vec<Line<'static>> {
            vec![Line::raw("info")]
        }
        fn accent(&self) -> Color {
            Color::Green
        }
        fn info_fields(&self) -> Vec<(String, String)> {
            vec![
                ("Title".into(), "Budget".into()),
                ("Tags".into(), String::new()),
            ]
        }
        fn info_custom_row(&self) -> bool {
            true
        }
    }

    fn on_info() -> Backstage {
        let mut bs = Backstage::open(std::env::temp_dir(), &["xlsx"]);
        bs.item = Item::Info;
        bs.pane = Pane::Menu;
        bs
    }

    const TYPES: &[crate::SaveType] = &[
        crate::SaveType {
            label: "Excel Workbook",
            ext: "xlsx",
        },
        crate::SaveType {
            label: "CSV UTF-8",
            ext: "csv",
        },
    ];

    #[test]
    fn extra_exports_sit_between_the_quick_export_and_the_types() {
        let mut bs = Backstage::open(std::env::temp_dir(), &["xlsx"])
            .with_save_types(TYPES, "Export CSV")
            .with_extra_exports(&["Export PDF"]);
        bs.item = Item::Export;
        bs.pane = Pane::Export;
        assert!(matches!(
            bs.key(key(KeyCode::Enter), &TestHost),
            BackstageEvent::Export
        ));
        bs.key(key(KeyCode::Down), &TestHost);
        assert!(matches!(
            bs.key(key(KeyCode::Enter), &TestHost),
            BackstageEvent::ExportExtra(0)
        ));
        // Down reaches the last type and stops there.
        for _ in 0..5 {
            bs.key(key(KeyCode::Down), &TestHost);
        }
        assert_eq!(bs.export_sel, 3);
        bs.key(key(KeyCode::Enter), &TestHost);
        assert_eq!(
            bs.type_sel, 1,
            "the second type, not shifted by the extra row"
        );

        // Clicks: the extra row at y 4, the first type at y 7.
        let mut bs = Backstage::open(std::env::temp_dir(), &["xlsx"])
            .with_save_types(TYPES, "Export CSV")
            .with_extra_exports(&["Export PDF"]);
        bs.item = Item::Export;
        bs.mouse(40, 4, &TestHost);
        assert_eq!(bs.export_sel, 1);
        assert!(matches!(
            bs.mouse(40, 4, &TestHost),
            BackstageEvent::ExportExtra(0)
        ));
        bs.mouse(40, 7, &TestHost);
        assert_eq!(bs.export_sel, 2);
    }

    #[test]
    fn info_without_fields_stays_read_only() {
        let mut bs = on_info();
        assert!(matches!(
            bs.key(key(KeyCode::Enter), &TestHost),
            BackstageEvent::None
        ));
        assert_eq!(bs.pane, Pane::Menu);
        bs.layout.info_top = 3;
        bs.layout.info_view = (2, 20);
        assert!(matches!(bs.mouse(20, 3, &TestHost), BackstageEvent::None));
        assert_eq!(bs.pane, Pane::Menu);
    }

    #[test]
    fn info_rows_take_the_focus_and_edit_on_enter() {
        let mut bs = on_info();
        assert!(matches!(
            bs.key(key(KeyCode::Right), &EditableHost),
            BackstageEvent::None
        ));
        assert_eq!(bs.pane, Pane::Info);
        assert_eq!(bs.info_sel, 0);
        bs.key(key(KeyCode::Down), &EditableHost);
        assert!(matches!(
            bs.key(key(KeyCode::Enter), &EditableHost),
            BackstageEvent::EditInfo(1)
        ));
        // Down stops on the custom-property row (index 2).
        for _ in 0..5 {
            bs.key(key(KeyCode::Down), &EditableHost);
        }
        assert_eq!(bs.info_sel, 2);
        assert!(matches!(
            bs.key(key(KeyCode::Enter), &EditableHost),
            BackstageEvent::EditInfo(2)
        ));
        bs.key(key(KeyCode::Left), &EditableHost);
        assert_eq!(bs.pane, Pane::Menu);
        assert!(matches!(
            bs.key(key(KeyCode::Esc), &EditableHost),
            BackstageEvent::Close
        ));
    }

    #[test]
    fn info_row_click_selects_then_edits() {
        let mut bs = on_info();
        bs.layout.info_top = 5;
        bs.layout.info_view = (2, 20);
        assert!(matches!(
            bs.mouse(20, 6, &EditableHost),
            BackstageEvent::None
        ));
        assert_eq!((bs.pane, bs.info_sel), (Pane::Info, 1));
        assert!(matches!(
            bs.mouse(20, 6, &EditableHost),
            BackstageEvent::EditInfo(1)
        ));
        // Below the rows: nothing.
        assert!(matches!(
            bs.mouse(20, 8, &EditableHost),
            BackstageEvent::None
        ));
        assert_eq!(bs.info_sel, 1);
    }

    #[test]
    fn info_clicks_outside_the_box_do_nothing() {
        let mut bs = on_info();
        // Scrolled: row 0 sits above the box, row 2 on its last inner row.
        bs.layout.info_top = 1;
        bs.layout.info_view = (2, 3);
        for y in [1, 3, 4] {
            assert!(matches!(
                bs.mouse(20, y, &EditableHost),
                BackstageEvent::None
            ));
            assert_eq!(bs.pane, Pane::Menu, "y {y}");
        }
        assert!(matches!(
            bs.mouse(20, 2, &EditableHost),
            BackstageEvent::None
        ));
        assert_eq!((bs.pane, bs.info_sel), (Pane::Info, 1));
    }

    #[test]
    fn focus_info_lands_on_a_row() {
        let mut bs = Backstage::open(std::env::temp_dir(), &["xlsx"]);
        bs.focus_info(2);
        assert_eq!((bs.item, bs.pane, bs.info_sel), (Item::Info, Pane::Info, 2));
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

    #[test]
    fn options_keys_change_value_rows_and_leave_from_a_checkbox() {
        use crate::OptRow;
        let mut bs = Backstage::open(std::env::temp_dir(), &["xlsx"]).with_option_rows(vec![
            OptRow::check("fixed", "Editing", "Fixed", true),
            OptRow::int("places", "Editing", "Places", 2, -300, 300),
            OptRow::choice("dir", "Editing", "Direction", &["Down", "Right", "Up"], 0),
        ]);
        bs.item = Item::Options;
        bs.pane = Pane::Options;
        bs.key(key(KeyCode::Down), &TestHost);
        bs.key(key(KeyCode::Right), &TestHost);
        assert_eq!(bs.option_int("places"), Some(3));
        bs.key(key(KeyCode::Left), &TestHost);
        bs.key(key(KeyCode::Left), &TestHost);
        assert_eq!(bs.option_int("places"), Some(1));
        assert_eq!(bs.pane, Pane::Options, "Left on a value row stays");
        bs.key(key(KeyCode::PageDown), &TestHost);
        assert_eq!(bs.option_int("places"), Some(-9));
        bs.key(key(KeyCode::PageUp), &TestHost);
        bs.key(key(KeyCode::PageUp), &TestHost);
        assert_eq!(bs.option_int("places"), Some(11));
        bs.key(key(KeyCode::Down), &TestHost);
        bs.key(key(KeyCode::Right), &TestHost);
        assert_eq!(bs.option_choice("dir"), Some(1));
        bs.key(key(KeyCode::Enter), &TestHost);
        assert_eq!(bs.option_choice("dir"), Some(2));
        bs.key(key(KeyCode::PageUp), &TestHost);
        assert_eq!(bs.option_choice("dir"), Some(2), "PgUp only moves a number");
        bs.key(key(KeyCode::Up), &TestHost);
        bs.key(key(KeyCode::Up), &TestHost);
        bs.key(key(KeyCode::Right), &TestHost);
        assert_eq!(
            bs.option_check("fixed"),
            Some(true),
            "Right leaves a checkbox"
        );
        bs.key(key(KeyCode::Char(' ')), &TestHost);
        assert_eq!(bs.option_check("fixed"), Some(false));
        bs.key(key(KeyCode::Left), &TestHost);
        assert_eq!(
            bs.pane,
            Pane::Menu,
            "Left on a checkbox returns to the menu"
        );
    }
}
