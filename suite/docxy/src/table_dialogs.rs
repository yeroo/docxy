//! The table dialogs (#646, #647): Insert Table..., Delete Cells...,
//! Split Cells..., Sort..., Convert to Text... and Convert Text to Table....
//!
//! Each is built from the edited story's editor when its command runs, holds
//! its staged values until OK, and applies them through one docxcore table
//! command, one undo step, on the same story.
use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use docxcore::editor::{CellSep, DeleteShift, Editor, SortKey, SortKind, SortSpec};
use docxcore::table::{AutoFit, GridMap};

fn ok_cancel() -> Vec<Button> {
    vec![
        Button {
            default: true,
            ..Button::new("OK", ButtonRole::Accept)
        },
        Button::new("Cancel", ButtonRole::Cancel),
    ]
}

fn form(id: &'static str, title: &str, owner: DialogOwner, controls: Vec<Control>) -> Dialog {
    let mut d = Dialog::message(id, title, String::new(), &[], owner);
    d.text = None;
    d.controls = controls;
    d.buttons = ok_cancel();
    d.mark_opened();
    d
}

fn number(name: &'static str, label: &str, v: usize) -> Control {
    Control::new(name, label, ControlKind::Number, Value::Text(v.to_string()))
}

fn choice(
    name: &'static str,
    label: &str,
    kind: ControlKind,
    items: &[&str],
    at: usize,
) -> Control {
    let mut c = Control::new(name, label, kind, Value::Choice(Some(at)));
    c.items = items.iter().map(|s| s.to_string()).collect();
    c
}

fn chosen(d: &Dialog, name: &str) -> Option<usize> {
    match d.value(name) {
        Some(Value::Choice(i)) => *i,
        _ => None,
    }
}

fn text(d: &Dialog, name: &str) -> String {
    match d.value(name) {
        Some(Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

fn checked(d: &Dialog, name: &str) -> bool {
    matches!(d.value(name), Some(Value::Bool(true)))
}

/// A whole number in `lo..=hi`, or why not.
fn count(d: &Dialog, name: &str, what: &str, lo: usize, hi: usize) -> Result<usize, String> {
    text(d, name)
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|n| (lo..=hi).contains(n))
        .ok_or_else(|| format!("{what} must be a whole number from {lo} to {hi}"))
}

fn need_table(ed: &Editor) -> Result<(), String> {
    if ed.in_table() {
        Ok(())
    } else {
        Err("the caret is not in a table".into())
    }
}

// ---- Insert Table ----

const FIT: [&str; 3] = [
    "Fixed column width",
    "AutoFit to contents",
    "AutoFit to window",
];

/// Insert Table...: 5 columns by 2 rows, Fixed column width Auto (Word's
/// defaults).
pub(crate) fn insert_table_dialog(_: &Editor) -> Result<Dialog, String> {
    Ok(form(
        "insert-table",
        "Insert Table",
        DialogOwner::InsertTable,
        vec![
            number("cols", "Number of &columns:", 5),
            number("rows", "Number of &rows:", 2),
            choice("fit", "AutoFit behavior", ControlKind::Radio, &FIT, 0),
            Control::new(
                "width",
                "Fixed column &width:",
                ControlKind::Text,
                Value::Text("Auto".into()),
            ),
        ],
    ))
}

/// Apply Insert Table... at the caret.
pub(crate) fn apply_insert_table(ed: &mut Editor, d: &Dialog) -> Result<bool, String> {
    let cols = count(d, "cols", "Number of columns", 1, 63)?;
    let rows = count(d, "rows", "Number of rows", 1, 32767)?;
    let fit = match chosen(d, "fit") {
        Some(1) => AutoFit::Contents,
        Some(2) => AutoFit::Window,
        _ => {
            let w = text(d, "width");
            if w.trim().eq_ignore_ascii_case("auto") {
                AutoFit::Default
            } else {
                let inches = w
                    .trim()
                    .trim_end_matches('"')
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && *v > 0.0 && *v <= 22.0)
                    .ok_or("Fixed column width must be Auto or a width in inches")?;
                AutoFit::Fixed(Some((inches * 1440.0).round() as u32))
            }
        }
    };
    ed.insert_table(rows, cols, fit)?;
    Ok(true)
}

// ---- Delete Cells ----

const SHIFTS: [&str; 4] = [
    "Shift cells left",
    "Shift cells up",
    "Delete entire row",
    "Delete entire column",
];

pub(crate) fn delete_cells_dialog(ed: &Editor) -> Result<Dialog, String> {
    need_table(ed)?;
    Ok(form(
        "delete-cells",
        "Delete Cells",
        DialogOwner::DeleteCells,
        vec![choice("shift", "", ControlKind::Radio, &SHIFTS, 0)],
    ))
}

pub(crate) fn apply_delete_cells(ed: &mut Editor, d: &Dialog) -> Result<bool, String> {
    let shift = match chosen(d, "shift") {
        Some(1) => DeleteShift::ShiftUp,
        Some(2) => DeleteShift::EntireRow,
        Some(3) => DeleteShift::EntireColumn,
        _ => DeleteShift::ShiftLeft,
    };
    ed.delete_cells(shift)?;
    Ok(true)
}

// ---- Split Cells ----

pub(crate) fn split_cells_dialog(ed: &Editor) -> Result<Dialog, String> {
    need_table(ed)?;
    let range = ed.cell_range().is_some();
    let mut merge = Control::new(
        "merge",
        "&Merge cells before split",
        ControlKind::Checkbox,
        Value::Bool(range),
    );
    merge.enabled = range;
    Ok(form(
        "split-cells",
        "Split Cells",
        DialogOwner::SplitCells,
        vec![
            number("cols", "Number of &columns:", 2),
            number("rows", "Number of &rows:", 1),
            merge,
        ],
    ))
}

pub(crate) fn apply_split_cells(ed: &mut Editor, d: &Dialog) -> Result<bool, String> {
    let cols = count(d, "cols", "Number of columns", 1, 63)?;
    let rows = count(d, "rows", "Number of rows", 1, 32767)?;
    ed.split_cells(cols, rows, checked(d, "merge"))?;
    Ok(true)
}

// ---- Sort ----

const KINDS: [&str; 3] = ["Text", "Number", "Date"];
const ORDERS: [&str; 2] = ["Ascending", "Descending"];
const HEADER: [&str; 2] = ["Header row", "No header row"];

/// Sort...: up to three keys by column, each Text/Number/Date and ascending
/// or descending, and whether the list has a header row.
pub(crate) fn sort_dialog(ed: &Editor) -> Result<Dialog, String> {
    need_table(ed)?;
    let r = ed.table_selection().ok_or("the caret is not in a table")?;
    let t = ed.table(&r.table).ok_or("no table")?;
    let width = GridMap::of(t).width(t);
    let columns: Vec<String> = (1..=width).map(|c| format!("Column {c}")).collect();
    let mut then: Vec<String> = vec!["(none)".into()];
    then.extend(columns.iter().cloned());
    let col = |name, label: &str, items: &[String], at| {
        let mut c = Control::new(name, label, ControlKind::Dropdown, Value::Choice(Some(at)));
        c.items = items.to_vec();
        c
    };
    Ok(form(
        "sort",
        "Sort",
        DialogOwner::SortTable,
        vec![
            col("sort1", "&Sort by", &columns, 0),
            choice("type1", "Type:", ControlKind::Dropdown, &KINDS, 0),
            choice("order1", "", ControlKind::Radio, &ORDERS, 0),
            col("sort2", "&Then by", &then, 0),
            choice("type2", "Type:", ControlKind::Dropdown, &KINDS, 0),
            choice("order2", "", ControlKind::Radio, &ORDERS, 0),
            col("sort3", "Then &by", &then, 0),
            choice("type3", "Type:", ControlKind::Dropdown, &KINDS, 0),
            choice("order3", "", ControlKind::Radio, &ORDERS, 0),
            choice("header", "My list has", ControlKind::Radio, &HEADER, 1),
        ],
    ))
}

pub(crate) fn sort_spec(d: &Dialog) -> SortSpec {
    let kind = |name| match chosen(d, name) {
        Some(1) => SortKind::Number,
        Some(2) => SortKind::Date,
        _ => SortKind::Text,
    };
    let mut keys = vec![SortKey {
        col: chosen(d, "sort1").unwrap_or(0),
        kind: kind("type1"),
        descending: chosen(d, "order1") == Some(1),
    }];
    for (s, ty, o) in [("sort2", "type2", "order2"), ("sort3", "type3", "order3")] {
        if let Some(c) = chosen(d, s).filter(|&c| c > 0) {
            keys.push(SortKey {
                col: c - 1,
                kind: kind(ty),
                descending: chosen(d, o) == Some(1),
            });
        }
    }
    SortSpec {
        header: chosen(d, "header") == Some(0),
        keys,
    }
}

pub(crate) fn apply_sort(ed: &mut Editor, d: &Dialog) -> Result<bool, String> {
    ed.sort_table(&sort_spec(d))?;
    Ok(true)
}

// ---- Convert ----

const SEPS: [&str; 4] = ["Paragraph marks", "Tabs", "Commas", "Other:"];

fn separator(d: &Dialog) -> Result<CellSep, String> {
    Ok(match chosen(d, "sep") {
        Some(0) => CellSep::Paragraph,
        Some(2) => CellSep::Char(','),
        Some(3) => {
            let other = text(d, "other");
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => CellSep::Char(c),
                _ => return Err("type one character to separate with".into()),
            }
        }
        _ => CellSep::Tab,
    })
}

/// Convert to Text...: separate cells with tabs (Word's default), paragraph
/// marks, commas or another character.
pub(crate) fn table_to_text_dialog(ed: &Editor) -> Result<Dialog, String> {
    need_table(ed)?;
    Ok(form(
        "convert-to-text",
        "Convert Table To Text",
        DialogOwner::TableToText,
        vec![
            choice("sep", "Separate text with", ControlKind::Radio, &SEPS, 1),
            Control::new(
                "other",
                "Other:",
                ControlKind::Text,
                Value::Text("-".into()),
            ),
        ],
    ))
}

pub(crate) fn apply_table_to_text(ed: &mut Editor, d: &Dialog) -> Result<bool, String> {
    ed.table_to_text(separator(d)?)?;
    Ok(true)
}

/// Convert Text to Table...: the number of columns guessed from the tabs in
/// the selected paragraphs (Word's guess), separated by tabs when there are
/// any, else by paragraphs.
pub(crate) fn text_to_table_dialog(ed: &Editor) -> Result<Dialog, String> {
    let spans = ed.selection_spans();
    if spans.is_empty() || ed.cell_range().is_some() {
        return Err("select the text to convert".into());
    }
    let text = ed.selection_text();
    let most_tabs = text
        .split('\n')
        .map(|l| l.matches('\t').count())
        .max()
        .unwrap_or(0);
    let (cols, sep) = if most_tabs > 0 {
        (most_tabs + 1, 1)
    } else {
        (1, 0)
    };
    Ok(form(
        "convert-text-to-table",
        "Convert Text to Table",
        DialogOwner::TextToTable,
        vec![
            number("cols", "Number of &columns:", cols),
            choice("sep", "Separate text at", ControlKind::Radio, &SEPS, sep),
            Control::new(
                "other",
                "Other:",
                ControlKind::Text,
                Value::Text("-".into()),
            ),
        ],
    ))
}

pub(crate) fn apply_text_to_table(ed: &mut Editor, d: &Dialog) -> Result<bool, String> {
    let cols = count(d, "cols", "Number of columns", 1, 63)?;
    ed.text_to_table(separator(d)?, Some(cols))?;
    Ok(true)
}

/// Apply an accepted table dialog to the edited story's editor.
pub(crate) fn apply_table_dialog(ed: &mut Editor, d: &Dialog) -> Result<bool, String> {
    match d.owner {
        DialogOwner::InsertTable => apply_insert_table(ed, d),
        DialogOwner::DeleteCells => apply_delete_cells(ed, d),
        DialogOwner::SplitCells => apply_split_cells(ed, d),
        DialogOwner::SortTable => apply_sort(ed, d),
        DialogOwner::TableToText => apply_table_to_text(ed, d),
        DialogOwner::TextToTable => apply_text_to_table(ed, d),
        _ => Err("not a table dialog".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctlcore::json::Json;
    use docxcore::editor::Caret;
    use docxcore::model::{Block, Document, Inline, Paragraph, Run, RunProps};
    use docxcore::table::new_table;

    fn doc_with_table(rows: usize, cols: usize) -> Editor {
        let mut t = new_table(rows, cols, 9000, AutoFit::Default);
        for (r, row) in t.rows.iter_mut().enumerate() {
            for (c, cell) in row.cells.iter_mut().enumerate() {
                cell.blocks = vec![Block::Paragraph(Paragraph {
                    content: vec![Inline::Run(Run {
                        text: format!("{}", (rows - r) * 10 + c),
                        props: RunProps::default(),
                    })],
                    ..Paragraph::default()
                })];
            }
        }
        let mut ed = Editor::new(Document {
            body: vec![Block::Table(t), Block::Paragraph(Paragraph::default())],
        });
        ed.caret = Caret::at(vec![0, 0, 0, 0], 0);
        ed
    }

    fn set(d: &mut Dialog, name: &str, v: Json) {
        d.set(name, &Json::obj(vec![("value", v)])).unwrap();
    }

    #[test]
    fn insert_table_defaults_and_apply() {
        let mut ed = Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph::default())],
        });
        let mut d = insert_table_dialog(&ed).unwrap();
        assert_eq!(text(&d, "cols"), "5");
        assert_eq!(text(&d, "rows"), "2");
        assert_eq!(chosen(&d, "fit"), Some(0));
        assert_eq!(text(&d, "width"), "Auto");
        set(&mut d, "cols", Json::Num(3.0));
        apply_insert_table(&mut ed, &d).unwrap();
        let t = ed.table(&[0]).unwrap();
        assert_eq!((t.rows.len(), t.rows[0].cells.len()), (2, 3));
        assert!(t.raw_tblpr.as_deref().unwrap().contains("w:type=\"auto\""));
        assert!(ed.undo());
        set(&mut d, "width", Json::Str("1.5".into()));
        apply_insert_table(&mut ed, &d).unwrap();
        assert_eq!(ed.table(&[0]).unwrap().grid, vec![2160; 3]);
        set(&mut d, "cols", Json::Num(0.0));
        assert!(apply_insert_table(&mut ed, &d).is_err());
    }

    #[test]
    fn delete_and_split_cells() {
        let mut ed = doc_with_table(2, 2);
        let mut d = delete_cells_dialog(&ed).unwrap();
        set(&mut d, "shift", Json::Str("Delete entire row".into()));
        apply_delete_cells(&mut ed, &d).unwrap();
        assert_eq!(ed.table(&[0]).unwrap().rows.len(), 1);

        let mut ed = doc_with_table(1, 1);
        let d = split_cells_dialog(&ed).unwrap();
        assert!(!checked(&d, "merge"), "one cell: nothing to merge first");
        apply_split_cells(&mut ed, &d).unwrap();
        assert_eq!(ed.table(&[0]).unwrap().rows[0].cells.len(), 2);

        let outside = Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph::default())],
        });
        assert!(delete_cells_dialog(&outside).is_err());
    }

    #[test]
    fn sort_dialog_builds_the_spec() {
        let mut ed = doc_with_table(3, 2);
        let mut d = sort_dialog(&ed).unwrap();
        let spec = sort_spec(&d);
        assert_eq!(spec.keys.len(), 1);
        assert!(!spec.header);
        set(&mut d, "type1", Json::Str("Number".into()));
        set(&mut d, "sort2", Json::Str("Column 2".into()));
        set(&mut d, "order2", Json::Str("Descending".into()));
        let spec = sort_spec(&d);
        assert_eq!(spec.keys[0].kind, SortKind::Number);
        assert_eq!((spec.keys[1].col, spec.keys[1].descending), (1, true));
        apply_sort(&mut ed, &d).unwrap();
        let first = ed.table(&[0]).unwrap().rows[0].cells[0].blocks[0].plain_text();
        assert_eq!(first, "10");
    }

    #[test]
    fn convert_both_ways_through_the_dialogs() {
        let mut ed = doc_with_table(2, 2);
        let d = table_to_text_dialog(&ed).unwrap();
        assert_eq!(chosen(&d, "sep"), Some(1), "tabs by default");
        apply_table_to_text(&mut ed, &d).unwrap();
        assert_eq!(ed.doc.body[0].plain_text(), "20\t21");
        // The converted paragraphs stay selected: back to a table.
        let d = text_to_table_dialog(&ed).unwrap();
        assert_eq!(text(&d, "cols"), "2");
        assert_eq!(chosen(&d, "sep"), Some(1));
        apply_text_to_table(&mut ed, &d).unwrap();
        assert_eq!(ed.table(&[0]).unwrap().rows.len(), 2);
        let mut d = table_to_text_dialog(&ed).unwrap();
        set(&mut d, "sep", Json::Str("Other:".into()));
        set(&mut d, "other", Json::Str("ab".into()));
        assert!(apply_table_to_text(&mut ed, &d).is_err());
    }
}
