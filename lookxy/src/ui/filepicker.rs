//! A filesystem-browser popup for choosing a file to attach: navigate
//! directories (`..` goes up, Enter on a directory descends), Enter on a file
//! selects it. Modeled on the other list popups (`message_list::draw_move_picker`).

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::ui::centered_rect;

/// One row in the file picker: a directory (including the synthetic `..`) or a file.
pub struct FileEntry {
    pub name: String,
    /// Read back by `FilePicker::enter` (to descend into a directory or
    /// return the chosen file) — not yet reachable from production code
    /// (nothing calls `FilePicker::open`/`enter` outside tests until Task 8
    /// wires attaching to the picker). Silences `dead_code`, same pattern as
    /// `Compose::draft_id`.
    #[allow(dead_code)]
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
}

/// The open file picker: the directory being browsed, its entries, and the cursor.
pub struct FilePicker {
    pub dir: PathBuf,
    pub entries: Vec<FileEntry>,
    pub index: usize,
}

impl FilePicker {
    /// Opens the picker on `dir`, listing its entries.
    ///
    /// Not yet called from production code — Task 8's "attach a file" entry
    /// point is what will call this to open the popup; `cfg_attr` silences
    /// `dead_code` only outside tests, same pattern already used for
    /// `Compose::new`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn open(dir: PathBuf) -> FilePicker {
        let entries = read_entries(&dir);
        FilePicker {
            dir,
            entries,
            index: 0,
        }
    }

    /// Moves the cursor, clamped to the entry list.
    pub fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let max = (self.entries.len() - 1) as isize;
        self.index = (self.index as isize + delta).clamp(0, max) as usize;
    }

    /// Enter on the selected entry: descends into a directory (re-lists, returns
    /// `None`) or selects a file (returns its path). `None` on an empty list.
    ///
    /// Not yet called from production code — `App::file_picker_enter` is a
    /// stub until Task 8 fills it in to actually call this and act on the
    /// chosen file; same `cfg_attr` pattern as `FilePicker::open`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn enter(&mut self) -> Option<PathBuf> {
        let entry = self.entries.get(self.index)?;
        if entry.is_dir {
            let dir = entry.path.clone();
            self.dir = dir.clone();
            self.entries = read_entries(&dir);
            self.index = 0;
            None
        } else {
            Some(entry.path.clone())
        }
    }
}

/// Lists `dir`: a synthetic `..` (its parent) first when there is one, then
/// subdirectories (sorted by name), then files (sorted by name). Unreadable
/// entries and the directory itself failing to read are skipped defensively.
fn read_entries(dir: &Path) -> Vec<FileEntry> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            match e.file_type() {
                Ok(ft) if ft.is_dir() => dirs.push(FileEntry {
                    name,
                    path,
                    is_dir: true,
                    size: 0,
                }),
                Ok(ft) if ft.is_file() => {
                    let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                    files.push(FileEntry {
                        name,
                        path,
                        is_dir: false,
                        size,
                    });
                }
                _ => {}
            }
        }
    }
    dirs.sort_by(|a, b| a.name.cmp(&b.name));
    files.sort_by(|a, b| a.name.cmp(&b.name));

    let mut out = Vec::new();
    if let Some(parent) = dir.parent() {
        out.push(FileEntry {
            name: "..".to_string(),
            path: parent.to_path_buf(),
            is_dir: true,
            size: 0,
        });
    }
    out.extend(dirs);
    out.extend(files);
    out
}

/// Renders the file picker as a centered overlay when `app.file_picker` is set.
pub fn draw(f: &mut Frame, app: &crate::app::App) {
    let Some(fp) = &app.file_picker else {
        return;
    };
    let area = centered_rect(60, 60, f.area());
    f.render_widget(Clear, area);
    let items: Vec<ListItem> = fp
        .entries
        .iter()
        .map(|e| {
            let line = if e.is_dir {
                format!("{}/", e.name)
            } else {
                format!("{}  {} B", e.name, e.size)
            };
            ListItem::new(line)
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .title(format!("Attach a file — {}", fp.dir.display()))
                .borders(Borders::ALL)
                .border_style(Style::new().fg(Color::Yellow)),
        )
        .highlight_style(Style::new().bg(Color::Blue).fg(Color::White));
    let mut state = ListState::default();
    if !fp.entries.is_empty() {
        state.select(Some(fp.index.min(fp.entries.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_parent_then_dirs_then_files_and_navigates() {
        let base = std::env::temp_dir().join(format!("lookxy-fp-{}", std::process::id()));
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(base.join("z.txt"), b"z").unwrap();
        std::fs::write(base.join("a.txt"), b"a").unwrap();

        let mut fp = FilePicker::open(base.clone());
        // ".." first, then the directory "sub", then files a.txt, z.txt (dirs before files, each sorted)
        assert_eq!(fp.entries[0].name, "..");
        let names: Vec<&str> = fp.entries.iter().map(|e| e.name.as_str()).collect();
        let sub_i = names.iter().position(|n| *n == "sub").unwrap();
        let a_i = names.iter().position(|n| *n == "a.txt").unwrap();
        assert!(sub_i < a_i, "directories sort before files");

        // navigating into "sub" (a directory) returns None and changes dir
        fp.index = sub_i;
        assert_eq!(fp.enter(), None);
        assert_eq!(fp.dir, sub);

        // selecting a file returns its path
        let mut fp2 = FilePicker::open(base.clone());
        let ai = fp2.entries.iter().position(|e| e.name == "a.txt").unwrap();
        fp2.index = ai;
        assert_eq!(fp2.enter(), Some(base.join("a.txt")));

        // move is clamped
        let mut fp3 = FilePicker::open(base.clone());
        fp3.move_selection(-1);
        assert_eq!(fp3.index, 0);

        let _ = std::fs::remove_dir_all(&base);
    }
}
