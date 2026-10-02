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
    /// The host's options page (only hosts that ask for it, see
    /// [`Backstage::with_options`]).
    Options,
    Exit,
}

/// The items every host shows; [`Backstage::items`] adds any a host opted in to.
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
            Item::Options => "Options",
            Item::Exit => "Exit",
        }
    }
}

/// One entry of Save As's *Save as type* list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveType {
    pub label: &'static str,
    /// The extension the type gives the file name, without the dot.
    pub ext: &'static str,
}

/// `name` with its extension replaced by (or, without one, given) `ext`.
pub(crate) fn with_extension(name: &str, ext: &str) -> String {
    let stem = match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    };
    format!("{stem}.{ext}")
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
    /// The Options page's rows.
    Options,
    /// Export's list: the host's quick export, then Change File Type.
    Export,
    /// The Info page's editable rows (hosts with [`crate::BackstageHost::info_fields`]).
    Info,
}

/// Click rects and scroll offsets recorded by `draw` (Task 3) and read by
/// `mouse` (Task 2) — declared here so `Backstage` compiles standalone.
#[derive(Debug, Clone, Copy, Default)]
pub struct BackstageLayout {
    pub list_start: usize,
    pub save_btn: Rect,
    pub name_top: u16,
    /// Top row of the Save As *Save as type* box (when the host has types).
    pub type_top: u16,
    pub name_x0: u16,
    pub preview_h: usize,
    /// Screen row of the Info page's first editable row; above the box
    /// (even negative) when the page is scrolled down.
    pub info_top: i32,
    /// The Info box's inner rows, `[first, end)`: only a click there counts.
    pub info_view: (u16, u16),
}

/// What an Options row holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptValue {
    /// A checkbox.
    Check(bool),
    /// One of `items`, `at` the chosen one (a direction list).
    Choice { items: Vec<String>, at: usize },
    /// A whole number in `min..=max` (a count of places).
    Int { value: i32, min: i32, max: i32 },
}

/// One row of the Options page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptRow {
    /// A stable name the host reads the row back by (labels are UI text).
    pub key: String,
    pub label: String,
    /// The heading the row sits under; a new heading starts where the
    /// section changes from the row before.
    pub section: String,
    pub value: OptValue,
    /// The key of a checkbox this row only matters with: while that box is
    /// off the row is drawn dimmed (it stays editable, as in Excel).
    pub depends_on: Option<String>,
}

impl OptRow {
    /// A checkbox row.
    pub fn check(key: &str, section: &str, label: &str, on: bool) -> OptRow {
        OptRow {
            key: key.to_string(),
            label: label.to_string(),
            section: section.to_string(),
            value: OptValue::Check(on),
            depends_on: None,
        }
    }

    /// A row choosing one of `items`.
    pub fn choice(key: &str, section: &str, label: &str, items: &[&str], at: usize) -> OptRow {
        OptRow {
            value: OptValue::Choice {
                items: items.iter().map(|s| s.to_string()).collect(),
                at: at.min(items.len().saturating_sub(1)),
            },
            ..OptRow::check(key, section, label, false)
        }
    }

    /// A row holding a number in `min..=max`.
    pub fn int(key: &str, section: &str, label: &str, value: i32, min: i32, max: i32) -> OptRow {
        OptRow {
            value: OptValue::Int {
                value: value.clamp(min, max),
                min,
                max,
            },
            ..OptRow::check(key, section, label, false)
        }
    }

    /// This row, dimmed while the checkbox `key` is off.
    pub fn depends_on(mut self, key: &str) -> OptRow {
        self.depends_on = Some(key.to_string());
        self
    }

    /// Step the value: a checkbox flips, a choice moves `by` places
    /// (wrapping), a number moves `by` (clamped).
    pub fn step(&mut self, by: i32) {
        match &mut self.value {
            OptValue::Check(on) => *on = !*on,
            OptValue::Choice { items, at } => {
                let n = items.len() as i32;
                if n > 0 {
                    *at = (*at as i32 + by).rem_euclid(n) as usize;
                }
            }
            OptValue::Int { value, min, max } => *value = (*value + by).clamp(*min, *max),
        }
    }
}

/// One line of the Options page, top to bottom: what both the drawing and a
/// click's hit test lay out (see [`Backstage::option_lines`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptLine {
    /// A section's heading (the index of its first row).
    Heading(usize),
    Blank,
    /// The row at this index of [`Backstage::options`].
    Row(usize),
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
    /// In Save As: true when the file-name field is focused (accepting edits).
    /// Save As has three focus targets: this field, the *Save as type* box
    /// ([`Backstage::type_focus`], when the host has types) and the folder
    /// browser (neither flag set).
    pub name_focus: bool,
    /// The menu, in display order.
    items: Vec<Item>,
    /// The Options page's rows, grouped by their sections.
    pub options: Vec<OptRow>,
    /// The highlighted row.
    pub option_sel: usize,
    /// Save As's *Save as type* list (empty: no list, as docxy has none).
    pub save_types: &'static [SaveType],
    /// The type shown in Save As.
    pub type_sel: usize,
    /// The user picked the type (rather than it following the name).
    pub type_touched: bool,
    /// The type shown is the one the host bound the document to
    /// ([`crate::BackstageHost::default_save_type`]).
    pub type_preset: bool,
    /// In Save As: the type box is focused.
    pub type_focus: bool,
    /// Export's highlighted row: 0 is the quick export, then each type.
    pub export_sel: usize,
    /// The quick export's label on the Export page.
    pub export_quick: String,
    /// More exports the host offers, listed under the quick export.
    pub export_extra: Vec<String>,
    /// The highlighted editable row of the Info page.
    pub info_sel: usize,
    /// The Info page's first line on screen, kept between draws so the
    /// page only moves when the selection would leave the box.
    pub info_scroll: usize,
    /// A line the host shows on the Info page: how the last edit went.
    pub info_message: Option<String>,
    // Filled by `draw` (Task 3) and read by `mouse` (Task 2, `input.rs`), a
    // sibling module — needs crate-wide visibility, not just within `state`.
    pub(crate) layout: BackstageLayout,
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
            items: ITEMS.to_vec(),
            options: Vec::new(),
            option_sel: 0,
            save_types: &[],
            type_sel: 0,
            type_touched: false,
            type_preset: false,
            type_focus: false,
            export_sel: 0,
            export_quick: String::new(),
            export_extra: Vec::new(),
            info_sel: 0,
            info_scroll: 0,
            info_message: None,
            layout: BackstageLayout::default(),
        };
        b.refresh();
        b
    }

    /// Add an Options page (before Exit) with `title` and these checkboxes,
    /// each keyed by its label. The host reads [`Backstage::options`] back
    /// after each key or click.
    pub fn with_options(self, title: &str, options: Vec<(String, bool)>) -> Backstage {
        let rows = options
            .into_iter()
            .map(|(label, on)| OptRow::check(&label, title, &label, on))
            .collect();
        self.with_option_rows(rows)
    }

    /// Add an Options page (before Exit) with these rows, which may mix
    /// checkboxes, choices and numbers under several sections.
    pub fn with_option_rows(mut self, rows: Vec<OptRow>) -> Backstage {
        if !self.items.contains(&Item::Options) {
            let at = self.items.len().saturating_sub(1);
            self.items.insert(at, Item::Options);
        }
        self.options = rows;
        self
    }

    /// The Options page's lines: each section's heading and a blank line,
    /// then its rows, with a blank line between sections.
    pub fn option_lines(&self) -> Vec<OptLine> {
        let mut lines = Vec::new();
        for (i, row) in self.options.iter().enumerate() {
            if i == 0 || self.options[i - 1].section != row.section {
                if i > 0 {
                    lines.push(OptLine::Blank);
                }
                lines.push(OptLine::Heading(i));
                lines.push(OptLine::Blank);
            }
            lines.push(OptLine::Row(i));
        }
        lines
    }

    /// The row keyed `key`.
    pub fn option(&self, key: &str) -> Option<&OptValue> {
        self.options.iter().find(|o| o.key == key).map(|o| &o.value)
    }

    /// The checkbox keyed `key`, if there is one.
    pub fn option_check(&self, key: &str) -> Option<bool> {
        match self.option(key)? {
            OptValue::Check(on) => Some(*on),
            _ => None,
        }
    }

    /// The chosen index of the choice keyed `key`.
    pub fn option_choice(&self, key: &str) -> Option<usize> {
        match self.option(key)? {
            OptValue::Choice { at, .. } => Some(*at),
            _ => None,
        }
    }

    /// The number keyed `key`.
    pub fn option_int(&self, key: &str) -> Option<i32> {
        match self.option(key)? {
            OptValue::Int { value, .. } => Some(*value),
            _ => None,
        }
    }

    /// Is row `i` dimmed: does it depend on a checkbox that is off?
    pub fn option_dimmed(&self, i: usize) -> bool {
        self.options
            .get(i)
            .and_then(|o| o.depends_on.as_deref())
            .and_then(|k| self.option_check(k))
            .is_some_and(|on| !on)
    }

    /// Give Save As a *Save as type* list, and Export a page listing
    /// `quick_export` (the host's own export) and then Change File Type.
    pub fn with_save_types(mut self, types: &'static [SaveType], quick_export: &str) -> Backstage {
        self.save_types = types;
        self.export_quick = quick_export.to_string();
        self
    }

    /// List more exports on the Export page, under the quick export; running
    /// one is [`crate::BackstageEvent::ExportExtra`] with its index.
    pub fn with_extra_exports(mut self, labels: &[&str]) -> Backstage {
        self.export_extra = labels.iter().map(|l| l.to_string()).collect();
        self
    }

    /// The type the user picked in Save As, if they picked one; otherwise
    /// the host decides from the name.
    pub fn chosen_type(&self) -> Option<usize> {
        (self.type_touched && self.type_sel < self.save_types.len()).then_some(self.type_sel)
    }

    /// The host's preselected type, while the user has not picked another.
    /// The host decides whether it still applies to the typed name.
    pub fn preset_type(&self) -> Option<usize> {
        (self.type_preset && !self.type_touched && self.type_sel < self.save_types.len())
            .then_some(self.type_sel)
    }

    /// Show `t` as the host's preselected type.
    pub fn preset(&mut self, t: usize) {
        if t < self.save_types.len() {
            self.type_sel = t;
            self.type_preset = true;
        }
    }

    /// Pick a Save As type: the name's extension follows it.
    pub fn pick_type(&mut self, i: usize) {
        let Some(t) = self.save_types.get(i) else {
            return;
        };
        self.type_sel = i;
        self.type_touched = true;
        self.name_input = with_extension(&self.name_input, t.ext);
        self.name_cursor = self.name_input.chars().count();
    }

    /// Open Save As on `name`, with type `ty` picked (or the one the name's
    /// extension suggests).
    pub fn begin_save_as(&mut self, name: String, ty: Option<usize>) {
        self.name_cursor = name.chars().count();
        self.name_input = name;
        self.name_focus = true;
        self.type_focus = false;
        self.item = Item::SaveAs;
        self.pane = Pane::SaveAs;
        self.type_touched = false;
        self.type_preset = false;
        let ext = self
            .name_input
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase());
        self.type_sel = self
            .save_types
            .iter()
            .position(|t| Some(t.ext) == ext.as_deref())
            .unwrap_or(0);
        if let Some(i) = ty {
            self.pick_type(i);
        }
    }

    /// Show Info with its editable row `row` focused (where a host returns
    /// after editing one).
    pub fn focus_info(&mut self, row: usize) {
        self.item = Item::Info;
        self.pane = Pane::Info;
        self.info_sel = row;
    }

    /// The menu items, in display order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Flip the highlighted checkbox, or move its choice or number on by one.
    pub fn toggle_option(&mut self) {
        self.step_option(1);
    }

    /// Step the highlighted row by `by` ([`OptRow::step`]).
    pub fn step_option(&mut self, by: i32) {
        if let Some(o) = self.options.get_mut(self.option_sel) {
            o.step(by);
        }
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
        let i = self.items.iter().position(|x| *x == self.item).unwrap_or(0);
        let ni = if down {
            (i + 1).min(self.items.len() - 1)
        } else {
            i.saturating_sub(1)
        };
        self.item = self.items[ni];
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
    fn options_are_opt_in_and_sit_before_exit() {
        let plain = Backstage::open(std::env::temp_dir(), &["docx"]);
        // docxy, lookxy and yppxy keep their seven items.
        assert_eq!(plain.items(), ITEMS);
        let mut bs = Backstage::open(std::env::temp_dir(), &["xlsx"])
            .with_options("Data", vec![("One".into(), true), ("Two".into(), false)]);
        assert_eq!(bs.items().len(), 8);
        assert_eq!(bs.items()[6], Item::Options);
        assert_eq!(*bs.items().last().unwrap(), Item::Exit);
        bs.item = Item::Export;
        bs.menu_move(true);
        assert_eq!(bs.item, Item::Options);
        bs.option_sel = 1;
        bs.toggle_option();
        assert_eq!(bs.option_check("Two"), Some(true));
        assert_eq!(bs.options[1].section, "Data");
    }

    fn rows() -> Vec<OptRow> {
        vec![
            OptRow::check("a", "Data", "A", true),
            OptRow::check("fixed", "Editing", "Fixed", false),
            OptRow::int("places", "Editing", "Places", 2, -3, 3).depends_on("fixed"),
            OptRow::choice("dir", "Editing", "Direction", &["Down", "Right", "Up"], 0),
        ]
    }

    #[test]
    fn option_lines_head_each_section() {
        let bs = Backstage::open(std::env::temp_dir(), &["xlsx"]).with_option_rows(rows());
        use OptLine::*;
        assert_eq!(
            bs.option_lines(),
            vec![
                Heading(0),
                Blank,
                Row(0),
                Blank,
                Heading(1),
                Blank,
                Row(1),
                Row(2),
                Row(3)
            ]
        );
        assert!(bs.option_dimmed(2), "Places depends on the off box");
        assert!(!bs.option_dimmed(3));
    }

    #[test]
    fn value_rows_step_clamped_or_wrapping() {
        let mut bs = Backstage::open(std::env::temp_dir(), &["xlsx"]).with_option_rows(rows());
        bs.option_sel = 2;
        bs.step_option(1);
        assert_eq!(bs.option_int("places"), Some(3));
        bs.step_option(1);
        assert_eq!(bs.option_int("places"), Some(3), "clamped at max");
        bs.step_option(-10);
        assert_eq!(bs.option_int("places"), Some(-3), "clamped at min");
        bs.option_sel = 3;
        bs.step_option(-1);
        assert_eq!(bs.option_choice("dir"), Some(2), "wraps backwards");
        bs.toggle_option();
        assert_eq!(bs.option_choice("dir"), Some(0), "wraps forwards");
        assert_eq!(bs.option_check("dir"), None, "not a checkbox");
    }

    const TYPES: &[SaveType] = &[
        SaveType {
            label: "Excel Workbook",
            ext: "xlsx",
        },
        SaveType {
            label: "CSV UTF-8 (Comma delimited)",
            ext: "csv",
        },
    ];

    #[test]
    fn a_save_type_sets_the_extension() {
        let mut bs = Backstage::open(std::env::temp_dir(), &["xlsx"]).with_save_types(TYPES, "Q");
        bs.begin_save_as("book.xlsx".into(), None);
        assert_eq!(bs.type_sel, 0);
        assert_eq!(bs.chosen_type(), None);
        bs.pick_type(1);
        assert_eq!(bs.name_input, "book.csv");
        assert_eq!(bs.chosen_type(), Some(1));
        bs.begin_save_as("data".into(), Some(1));
        assert_eq!(bs.name_input, "data.csv");
        assert_eq!(with_extension("a.b.c", "txt"), "a.b.txt");
        // A host's preselected type shows, and counts until one is picked.
        bs.begin_save_as("u.txt".into(), None);
        bs.preset(1);
        assert_eq!(
            (bs.type_sel, bs.preset_type(), bs.chosen_type()),
            (1, Some(1), None)
        );
        bs.pick_type(0);
        assert_eq!((bs.preset_type(), bs.chosen_type()), (None, Some(0)));
        assert_eq!(with_extension(".hidden", "csv"), ".hidden.csv");
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
