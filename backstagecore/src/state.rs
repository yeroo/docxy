use ratatui::layout::Rect;
use std::path::PathBuf;

/// The vertical menu items, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    New,
    Open,
    Info,
    Save,
    SaveAs,
    Export,
    Exit,
}

pub const ITEMS: [Item; 7] = [
    Item::New,
    Item::Open,
    Item::Info,
    Item::Save,
    Item::SaveAs,
    Item::Export,
    Item::Exit,
];

impl Item {
    pub fn label(self) -> &'static str {
        match self {
            Item::New => "New",
            Item::Open => "Open",
            Item::Info => "Info",
            Item::Save => "Save",
            Item::SaveAs => "Save As",
            Item::Export => "Export",
            Item::Exit => "Exit",
        }
    }
}

/// One folder-browser row.
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    /// The `..` parent row.
    pub is_parent: bool,
    pub size: u64,
    /// A lock/temp file (`~$…`) we list but don't open.
    pub locked: bool,
}

impl Entry {
    /// `"12.0 KB"`-style size, blank for folders.
    pub fn size_str(&self) -> String {
        if self.is_dir {
            return String::new();
        }
        let b = self.size as f64;
        if b < 1024.0 {
            format!("{} B", self.size)
        } else if b < 1024.0 * 1024.0 {
            format!("{:.0} KB", b / 1024.0)
        } else {
            format!("{:.1} MB", b / (1024.0 * 1024.0))
        }
    }
}

/// Which pane has keyboard focus inside the backstage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The left vertical menu.
    Menu,
    /// The Open folder browser.
    Browser,
    /// The read-only document preview (scrollable).
    Preview,
    /// The Save As dialog (folder browser + typed file name).
    SaveAs,
}

/// Click rects and scroll offsets recorded by `draw` (Task 3) and read by
/// `mouse` (Task 2) — declared here so `Backstage` compiles standalone.
#[derive(Debug, Clone, Copy, Default)]
pub struct BackstageLayout {
    pub list_start: usize,
    pub save_btn: Rect,
    pub name_top: u16,
    pub name_x0: u16,
    pub preview_h: usize,
}

pub struct Backstage {
    pub item: Item,
    pub pane: Pane,
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    pub sel: usize,
    /// Case-insensitive extensions (no leading dot) this backstage lists/opens.
    exts: &'static [&'static str],
    /// Rendered preview lines for the highlighted file (filled by the app).
    pub preview: Vec<String>,
    pub preview_path: Option<PathBuf>,
    /// Cell width the current preview was rendered at (re-render when it changes).
    pub preview_w: usize,
    /// Top line of the preview scroll window.
    pub preview_scroll: usize,
    /// The filename being typed in the Save As dialog.
    pub name_input: String,
    /// Caret position (char index) within `name_input`.
    pub name_cursor: usize,
    /// In Save As: true when the file-name field is focused (accepting edits),
    /// false when the folder browser is focused.
    pub name_focus: bool,
    // Filled by `draw` (Task 3) and read by `mouse` (Task 2); unread until
    // those land, so silence the interim dead-code warning.
    #[allow(dead_code)]
    layout: BackstageLayout,
}

impl Backstage {
    pub fn open(dir: PathBuf, exts: &'static [&'static str]) -> Backstage {
        let mut b = Backstage {
            item: Item::Open,
            // Land on the vertical menu so the keyboard flows straight down it
            // (New → Open → … → Exit). Activating Open with Enter, or clicking a
            // file, moves into the browser.
            pane: Pane::Menu,
            dir,
            entries: Vec::new(),
            sel: 0,
            exts,
            preview: Vec::new(),
            preview_path: None,
            preview_w: 0,
            preview_scroll: 0,
            name_input: String::new(),
            name_cursor: 0,
            name_focus: false,
            layout: BackstageLayout::default(),
        };
        b.refresh();
        b
    }

    /// Re-read the current directory: subfolders + matching files, folders first.
    pub fn refresh(&mut self) {
        let mut dirs: Vec<Entry> = Vec::new();
        let mut files: Vec<Entry> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let meta = e.metadata();
                let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                if is_dir {
                    if !name.starts_with('.') {
                        dirs.push(Entry {
                            name,
                            is_dir: true,
                            is_parent: false,
                            size: 0,
                            locked: false,
                        });
                    }
                } else if self.exts.iter().any(|ext| {
                    let dot = format!(".{}", ext.to_ascii_lowercase());
                    name.to_ascii_lowercase().ends_with(&dot)
                }) {
                    files.push(Entry {
                        size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                        locked: name.starts_with("~$"),
                        name,
                        is_dir: false,
                        is_parent: false,
                    });
                }
            }
        }
        dirs.sort_by_key(|e| e.name.to_lowercase());
        files.sort_by_key(|e| e.name.to_lowercase());
        self.entries.clear();
        if self.dir.parent().is_some() {
            self.entries.push(Entry {
                name: "..".to_string(),
                is_dir: true,
                is_parent: true,
                size: 0,
                locked: false,
            });
        }
        self.entries.extend(dirs);
        self.entries.extend(files);
        self.sel = self.sel.min(self.entries.len().saturating_sub(1));
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.entries.get(self.sel)
    }

    /// The full path of the highlighted file, for preview/opening.
    pub fn selected_file(&self) -> Option<PathBuf> {
        let e = self.selected()?;
        (!e.is_dir && !e.locked).then(|| self.dir.join(&e.name))
    }

    pub fn move_sel(&mut self, down: bool) {
        if self.entries.is_empty() {
            return;
        }
        if down {
            self.sel = (self.sel + 1).min(self.entries.len() - 1);
        } else {
            self.sel = self.sel.saturating_sub(1);
        }
    }

    /// Activate the highlighted row. Returns `Some(path)` to open a document;
    /// otherwise navigates into a folder (or up) and returns `None`.
    pub fn enter(&mut self) -> Option<PathBuf> {
        let e = self.entries.get(self.sel)?;
        if e.is_parent {
            self.go_up();
            return None;
        }
        if e.is_dir {
            self.dir = self.dir.join(&e.name);
            self.sel = 0;
            self.refresh();
            return None;
        }
        (!e.locked).then(|| self.dir.join(&e.name))
    }

    pub fn go_up(&mut self) {
        if let Some(p) = self.dir.parent() {
            self.dir = p.to_path_buf();
            self.sel = 0;
            self.refresh();
        }
    }

    pub fn menu_move(&mut self, down: bool) {
        let i = ITEMS.iter().position(|x| *x == self.item).unwrap_or(0);
        let ni = if down {
            (i + 1).min(ITEMS.len() - 1)
        } else {
            i.saturating_sub(1)
        };
        self.item = ITEMS[ni];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_order_and_labels() {
        assert_eq!(ITEMS.len(), 7);
        assert_eq!(Item::Open.label(), "Open");
        assert_eq!(Item::SaveAs.label(), "Save As");
        // Exit is the last item.
        assert_eq!(*ITEMS.last().unwrap(), Item::Exit);
    }

    #[test]
    fn lists_docx_and_folders_only_folders_first() {
        let tmp = std::env::temp_dir().join("docxy_bs_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("a.docx"), b"x").unwrap();
        std::fs::write(tmp.join("b.txt"), b"x").unwrap();
        std::fs::write(tmp.join("~$a.docx"), b"x").unwrap();
        let bs = Backstage::open(tmp.clone(), &["docx"]);
        let names: Vec<&str> = bs.entries.iter().map(|e| e.name.as_str()).collect();
        // ".." then the folder then the docx files; the .txt is excluded.
        assert!(names.contains(&".."));
        assert!(names.contains(&"sub"));
        assert!(names.contains(&"a.docx"));
        assert!(!names.contains(&"b.txt"));
        // folders come before files
        let di = names.iter().position(|n| *n == "sub").unwrap();
        let fi = names.iter().position(|n| *n == "a.docx").unwrap();
        assert!(di < fi);
        // the lock file is listed but not openable
        let lock = bs.entries.iter().find(|e| e.name == "~$a.docx").unwrap();
        assert!(lock.locked);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn size_formatting() {
        let e = |size| Entry {
            name: String::new(),
            is_dir: false,
            is_parent: false,
            size,
            locked: false,
        };
        assert_eq!(e(512).size_str(), "512 B");
        assert_eq!(e(2048).size_str(), "2 KB");
        assert!(e(3 * 1024 * 1024).size_str().ends_with("MB"));
    }

    #[test]
    fn lists_multiple_extensions_case_insensitively() {
        let tmp = std::env::temp_dir().join("bscore_multiext");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.XLSX"), b"x").unwrap();
        std::fs::write(tmp.join("b.csv"), b"x").unwrap();
        std::fs::write(tmp.join("c.docx"), b"x").unwrap();
        let bs = Backstage::open(tmp.clone(), &["xlsx", "csv"]);
        let names: Vec<&str> = bs.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"a.XLSX")); // case-insensitive
        assert!(names.contains(&"b.csv"));
        assert!(!names.contains(&"c.docx")); // not in ext list
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn menu_move_walks_and_clamps() {
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        bs.item = Item::New;
        bs.menu_move(false); // already first — clamps
        assert_eq!(bs.item, Item::New);
        bs.menu_move(true);
        assert_eq!(bs.item, Item::Open);
    }
}
