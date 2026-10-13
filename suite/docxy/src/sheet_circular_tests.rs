//! Circular references in the Sheet tab (#759): the warning when a workbook
//! with a circle opens or an edit makes a new one, and the status bar's
//! `Circular References: <ref>` while any exists, as xlsxy shows them. The
//! edits run through `SheetView` and then [`DocTab::sheet_edited`], where
//! `Docxy::mark_sheet_dirty` ends every sheet edit.

use super::*;
use core::prelude::v1::test;
use gridcore::sheet::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A workbook file under the target directory, removed when dropped.
struct Book(PathBuf);

impl Book {
    fn new(pkg: &gridcore::xlsx::SheetPackage) -> Self {
        Self::from_bytes(&gridcore::xlsx::save_xlsx(pkg))
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/sheet-circular-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!(
            "{}-{}.xlsx",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }

    /// The workbook opened as File › Open opens it.
    fn open(&self) -> DocTab {
        sheet_tab_from_path(&self.0, false)
    }
}

impl Drop for Book {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn view(t: &mut DocTab) -> &mut SheetView {
    match &mut t.surface {
        Surface::Sheet(v) => v,
        _ => panic!("a sheet: {}", t.status),
    }
}

/// Type `text` into `cell` and commit it, then end the edit as the window
/// does.
fn type_into(t: &mut DocTab, cell: &str, text: &str) {
    let v = view(t);
    let at = gridcore::sheet::parse_cell_name(cell).unwrap();
    v.sel = at;
    v.anchor = at;
    v.begin_cell_edit(Some(String::new()));
    v.edit_caret = 0;
    for ch in text.chars() {
        v.edit_type(&ch.to_string());
    }
    assert!(v.commit_edit(), "{cell} took {text}");
    t.sheet_edited();
}

fn note(t: &mut DocTab) -> Option<String> {
    sheet_circular_word(view(t))
}

fn blank() -> gridcore::xlsx::SheetPackage {
    gridcore::xlsx::new_xlsx()
}

#[test]
fn opening_a_circle_warns() {
    let mut pkg = blank();
    pkg.workbook.sheets[0].set_cell(0, 0, Cell::formula("B1"));
    pkg.workbook.sheets[0].set_cell(0, 1, Cell::formula("A1"));
    let book = Book::new(&pkg);
    let mut t = book.open();
    assert_eq!(
        t.status.as_ref(),
        format!("loaded — 1 sheet. {CIRCULAR_WARNING}")
    );
    assert_eq!(note(&mut t).as_deref(), Some("Circular References: A1"));
    // Opened, the circle is not new: the first edit does not warn again.
    t.status = "".into();
    type_into(&mut t, "D4", "7");
    assert_eq!(t.status.as_ref(), "");
}

#[test]
fn an_edit_that_makes_a_circle_warns_once() {
    let book = Book::new(&blank());
    let mut t = book.open();
    assert_eq!(t.status.as_ref(), "loaded — 1 sheet");
    assert_eq!(note(&mut t), None);
    // An ordinary commit sets no status of its own, so the warning takes
    // the place of the one the open left.
    type_into(&mut t, "E1", "=E1+1");
    assert!(t.dirty);
    assert_eq!(t.status.as_ref(), CIRCULAR_WARNING);
    assert_eq!(note(&mut t).as_deref(), Some("Circular References: E1"));
    // An edit that makes no new circle, even one reading the circle, does
    // not warn again.
    t.status = "before".into();
    type_into(&mut t, "A4", "7");
    type_into(&mut t, "H1", "=E1+5");
    assert_eq!(t.status.as_ref(), "before");
    // Breaking the circle clears the note; an undo brings it back without a
    // warning, since it made nothing new.
    type_into(&mut t, "E1", "1");
    assert_eq!(note(&mut t), None);
    assert!(view(&mut t).undo_step());
    t.sheet_edited();
    assert_eq!(t.status.as_ref(), "before");
    assert_eq!(note(&mut t).as_deref(), Some("Circular References: E1"));
    // A second circle is new again.
    type_into(&mut t, "J2", "=K2");
    type_into(&mut t, "K2", "=J2");
    assert_eq!(t.status.as_ref(), CIRCULAR_WARNING);
}

#[test]
fn inserting_a_row_above_a_circle_does_not_warn() {
    let mut pkg = blank();
    pkg.workbook.sheets[0].set_cell(2, 4, Cell::formula("E3+1"));
    let book = Book::new(&pkg);
    let mut t = book.open();
    assert_eq!(note(&mut t).as_deref(), Some("Circular References: E3"));
    t.status = "before".into();
    let v = view(&mut t);
    v.sel = (0, 0);
    v.anchor = (0, 0);
    assert!(v.structural_edit(StructOp::InsertRow));
    t.sheet_edited();
    // The circle moved down a row; it is not a new one.
    assert_eq!(note(&mut t).as_deref(), Some("Circular References: E4"));
    assert_eq!(t.status.as_ref(), "before");
}

#[test]
fn iterative_book_shows_no_circle() {
    let mut pkg = blank();
    pkg.workbook.sheets[0].set_cell(0, 3, Cell::formula("D1/2+5"));
    // Iteration is a `<calcPr>` setting of the file.
    let mut saved = gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&pkg)).unwrap();
    let wb_xml = String::from_utf8(saved.part("xl/workbook.xml").unwrap().to_vec()).unwrap();
    assert!(wb_xml.contains("<calcPr "), "{wb_xml}");
    let wb_xml = wb_xml.replacen("<calcPr ", "<calcPr iterate=\"1\" iterateCount=\"100\" ", 1);
    saved.set_part("xl/workbook.xml", wb_xml.into_bytes());
    let book = Book::from_bytes(&gridcore::xlsx::save_xlsx(&saved));
    let mut t = book.open();
    assert!(view(&mut t).pkg.workbook.iterate.is_some());
    assert_eq!(t.status.as_ref(), "loaded — 1 sheet");
    assert_eq!(note(&mut t), None);
    // A new circle by an edit does not warn either.
    type_into(&mut t, "E1", "=E1+1");
    assert_eq!(t.status.as_ref(), "loaded — 1 sheet");
    assert_eq!(note(&mut t), None);
}

#[test]
fn replace_all_that_closes_a_circle_keeps_its_count() {
    // #759: Replace All says how many it replaced, then warns.
    let mut pkg = blank();
    pkg.workbook.sheets[0].set_cell(0, 0, Cell::formula("B1+1"));
    let book = Book::new(&pkg);
    let mut t = book.open();
    replace_all_in(&mut t, "B1", "A1");
    assert_eq!(t.status.as_ref(), format!("replaced 1. {CIRCULAR_WARNING}"));
    assert_eq!(note(&mut t).as_deref(), Some("Circular References: A1"));
}

#[test]
fn a_command_that_says_the_same_words_again_keeps_them() {
    // #759 r2: the second Replace All also says "replaced 1"; it is still
    // that command's own status, with the warning after it.
    let mut pkg = blank();
    pkg.workbook.sheets[0].set_cell(0, 0, Cell::formula("B1+1"));
    pkg.workbook.sheets[0].set_cell(4, 4, Cell::text("X"));
    let book = Book::new(&pkg);
    let mut t = book.open();
    replace_all_in(&mut t, "X", "Y");
    assert_eq!(t.status.as_ref(), "replaced 1");
    replace_all_in(&mut t, "B1", "A1");
    assert_eq!(t.status.as_ref(), format!("replaced 1. {CIRCULAR_WARNING}"));
}

#[test]
fn an_entry_a_command_commits_first_warns_after_the_commands_status() {
    // #759 r2: a ribbon act commits the open entry, then runs: the
    // circle the entry made is warned about after the act's own status.
    // `Docxy::run_sheet_act` wraps the act as one command (it needs a
    // window); this is that nesting: the act's commit, then Sort A to Z,
    // which `sheet_data_cmd` runs as a command of its own.
    let mut pkg = blank();
    for (r, n) in [3.0, 1.0, 2.0].into_iter().enumerate() {
        pkg.workbook.sheets[0].set_cell(r as u32, 1, Cell::number(n));
    }
    let book = Book::new(&pkg);
    let mut t = book.open();
    let act = t.circle_cmd_begin();
    let v = view(&mut t);
    v.sel = (5, 3);
    v.anchor = (5, 3);
    v.begin_cell_edit(Some(String::new()));
    for ch in "=D6".chars() {
        v.edit_type(&ch.to_string());
    }
    assert_eq!(v.close_editor_for_act(), Some(true));
    t.sheet_edited();
    let v = view(&mut t);
    v.sel = (0, 1);
    v.anchor = (0, 1);
    let data = t.circle_cmd_begin();
    crate::sheet_sort::quick(&mut t, true).unwrap();
    t.circle_cmd_end(data);
    t.circle_cmd_end(act);
    let status = t.status.to_string();
    assert!(status.starts_with("Sorted 3 rows. "), "{status}");
    assert!(status.ends_with(CIRCULAR_WARNING), "{status}");
}

#[test]
fn an_entry_save_commits_warns_after_saves_status() {
    // #759 r2: Ctrl+S with `=A1` still in A1's editor commits it, then
    // saves: the warning follows Save's status.
    let book = Book::new(&blank());
    let mut t = book.open();
    let v = view(&mut t);
    v.begin_cell_edit(Some(String::new()));
    for ch in "=A1".chars() {
        v.edit_type(&ch.to_string());
    }
    assert!(save_sheet_tab(&mut t, true, false, |_| None, |_| true));
    let status = t.status.to_string();
    assert!(status.starts_with("saved"), "{status}");
    assert!(
        status.ends_with(&format!(". {CIRCULAR_WARNING}")),
        "{status}"
    );
    assert_eq!(note(&mut t).as_deref(), Some("Circular References: A1"));
}
