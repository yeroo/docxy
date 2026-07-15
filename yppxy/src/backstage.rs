//! The File "backstage": a full-screen menu (New / Open / Info / Save / Save As
//! / Export / Exit) shown instead of the schedule when the File tab is chosen.
//! Open drives a folder browser listing subfolders and project files
//! (`.xml` / `.yppx` / `.mpp`); the app renders a live preview of the
//! highlighted project.
//!
//! This module holds pure state and navigation; rendering the preview and
//! performing actions (load/save/export) live in the app.

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
            Item::Export => "Export Gantt",
            Item::Exit => "Exit",
        }
    }
}

/// One folder-browser row.
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub is_parent: bool,
    pub size: u64,
    /// A lock/temp file (`~$…`) we list but don't open.
    pub locked: bool,
}

impl Entry {
    /// `"12 KB"`-style size, blank for folders.
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
    Menu,
    Browser,
    Preview,
    SaveAs,
}

pub struct Backstage {
    pub item: Item,
    pub pane: Pane,
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    pub sel: usize,
    /// Rendered preview lines for the highlighted project (filled by the app).
    pub preview: Vec<String>,
    pub preview_path: Option<PathBuf>,
    pub preview_scroll: usize,
    /// The filename being typed in the Save As dialog.
    pub name_input: String,
}

/// Whether `name` is a project file the browser can open.
pub fn is_openable(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".xml") || n.ends_with(".yppx") || n.ends_with(".mpp")
}

impl Backstage {
    pub fn open(dir: PathBuf) -> Backstage {
        let mut b = Backstage {
            item: Item::Open,
            pane: Pane::Menu,
            dir,
            entries: Vec::new(),
            sel: 0,
            preview: Vec::new(),
            preview_path: None,
            preview_scroll: 0,
            name_input: String::new(),
        };
        b.refresh();
        b
    }

    /// Re-read the current directory: subfolders + openable files, folders first.
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
                } else if is_openable(&name) {
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

    /// The full path of the highlighted project, for preview/opening.
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

    /// Activate the highlighted row. Returns `Some(path)` to open a project;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_order_and_labels() {
        assert_eq!(ITEMS.len(), 7);
        assert_eq!(Item::Open.label(), "Open");
        assert_eq!(Item::Export.label(), "Export Gantt");
        assert_eq!(*ITEMS.last().unwrap(), Item::Exit);
    }

    #[test]
    fn is_openable_project_files() {
        assert!(is_openable("plan.xml"));
        assert!(is_openable("plan.YPPX"));
        assert!(is_openable("legacy.mpp"));
        assert!(!is_openable("notes.txt"));
    }

    #[test]
    fn lists_projects_and_folders_folders_first() {
        let tmp = std::env::temp_dir().join("yppxy_bs_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("a.xml"), b"x").unwrap();
        std::fs::write(tmp.join("b.yppx"), b"x").unwrap();
        std::fs::write(tmp.join("c.txt"), b"x").unwrap();
        let bs = Backstage::open(tmp.clone());
        let names: Vec<&str> = bs.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&".."));
        assert!(names.contains(&"sub"));
        assert!(names.contains(&"a.xml"));
        assert!(names.contains(&"b.yppx"));
        assert!(!names.contains(&"c.txt"));
        let di = names.iter().position(|n| *n == "sub").unwrap();
        let fi = names.iter().position(|n| *n == "a.xml").unwrap();
        assert!(di < fi);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
