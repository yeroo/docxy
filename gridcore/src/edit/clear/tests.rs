use super::*;
use crate::sheet::{CellValue, Workbook, Xf, parse_cell_name};

fn book() -> Workbook {
    let mut wb = Workbook::default();
    wb.styles.xfs.push(Xf::default());
    let bold = wb.styles.intern(Xf {
        bold: true,
        ..Xf::default()
    });
    let mut s = Sheet {
        name: "Sheet1".into(),
        ..Sheet::default()
    };
    let mut a1 = Cell::number(1.0);
    a1.style = bold;
    s.set_cell(0, 0, a1);
    s.set_cell(0, 1, Cell::formula("A1*2"));
    s.set_cell(5, 5, Cell::text("outside"));
    s.hyperlinks.insert((0, 0), "https://example.com".into());
    s.hyperlink_refs.insert((0, 0), (0, 0, 0, 0));
    wb.sheets.push(s);
    wb
}

fn cell(wb: &Workbook, name: &str) -> Cell {
    let (r, c) = parse_cell_name(name).unwrap();
    wb.sheets[0].cell(r, c).cloned().unwrap_or_default()
}

const A1B2: Rect = (0, 0, 1, 1);
const NOTES: &[(u32, u32)] = &[(0, 0), (5, 5)];

#[test]
fn clear_all_takes_everything_but_moves_nothing() {
    let mut wb = book();
    let plan = clear_areas(&mut wb, 0, &[A1B2], ClearWhat::All, NOTES).unwrap();
    assert_eq!(cell(&wb, "A1"), Cell::default());
    assert_eq!(cell(&wb, "B1"), Cell::default());
    assert!(wb.sheets[0].hyperlinks.is_empty());
    assert_eq!(plan.notes, vec![(0, 0)]);
    assert_eq!(cell(&wb, "F6").value, CellValue::Text("outside".into()));
}

#[test]
fn clear_formats_keeps_contents_notes_and_links() {
    let mut wb = book();
    let plan = clear_areas(&mut wb, 0, &[A1B2], ClearWhat::Formats, NOTES).unwrap();
    let a1 = cell(&wb, "A1");
    assert_eq!((a1.value, a1.style), (CellValue::Number(1.0), 0));
    assert_eq!(wb.sheets[0].hyperlinks.len(), 1);
    assert!(plan.notes.is_empty());
}

#[test]
fn clear_contents_keeps_the_format() {
    let mut wb = book();
    let style = cell(&wb, "A1").style;
    clear_areas(&mut wb, 0, &[A1B2], ClearWhat::Contents, NOTES).unwrap();
    let a1 = cell(&wb, "A1");
    assert_eq!((a1.value, a1.style), (CellValue::Empty, style));
    assert_eq!(cell(&wb, "B1").formula, None);
    assert_eq!(wb.sheets[0].hyperlinks.len(), 1);
}

#[test]
fn clear_comments_only_names_the_notes() {
    let mut wb = book();
    let before = wb.sheets[0].cells.clone();
    let plan = clear_areas(&mut wb, 0, &[A1B2], ClearWhat::Comments, NOTES).unwrap();
    assert_eq!(plan.notes, vec![(0, 0)]);
    assert_eq!(wb.sheets[0].cells, before);
}

#[test]
fn clear_hyperlinks_keeps_the_style_and_remove_resets_it() {
    let mut wb = book();
    let style = cell(&wb, "A1").style;
    clear_areas(&mut wb, 0, &[A1B2], ClearWhat::Hyperlinks, NOTES).unwrap();
    assert!(wb.sheets[0].hyperlinks.is_empty());
    assert_eq!(cell(&wb, "A1").style, style);
    assert_eq!(wb.sheets[0].hyperlinks_removed, vec![(0, 0, 0, 0)]);
    let mut wb = book();
    clear_areas(&mut wb, 0, &[A1B2], ClearWhat::RemoveHyperlinks, NOTES).unwrap();
    assert!(wb.sheets[0].hyperlinks.is_empty());
    let a1 = cell(&wb, "A1");
    assert_eq!((a1.value, a1.style), (CellValue::Number(1.0), 0));
}

#[test]
fn every_area_is_cleared() {
    let mut wb = book();
    clear_areas(
        &mut wb,
        0,
        &[(0, 0, 0, 0), (5, 5, 5, 5)],
        ClearWhat::Contents,
        &[],
    )
    .unwrap();
    assert_eq!(cell(&wb, "A1").value, CellValue::Empty);
    assert_eq!(cell(&wb, "F6").value, CellValue::Empty);
    assert!(cell(&wb, "B1").formula.is_some(), "between the areas");
}

#[test]
fn merges_inside_go_and_a_split_one_is_refused() {
    let mut wb = book();
    wb.sheets[0].merges.push((0, 0, 0, 1));
    wb.sheets[0].merges.push((4, 4, 6, 6));
    assert_eq!(
        clear_areas(&mut wb, 0, &[A1B2], ClearWhat::All, &[]).map(|p| p.unmerge),
        Ok(vec![(0, 0, 0, 1)])
    );
    assert_eq!(wb.sheets[0].merges, vec![(4, 4, 6, 6)]);
    let before = wb.clone();
    assert_eq!(
        clear_areas(&mut wb, 0, &[(5, 5, 5, 5)], ClearWhat::Formats, &[]),
        Err(MERGED_PART)
    );
    assert_eq!(wb.sheets[0].cells, before.sheets[0].cells);
    // Contents leave merges alone.
    assert!(clear_areas(&mut wb, 0, &[(5, 5, 5, 5)], ClearWhat::Contents, &[]).is_ok());
}

#[test]
fn labels_round_trip() {
    for w in ClearWhat::ALL {
        assert_eq!(ClearWhat::from_label(w.label()), Some(w));
    }
    assert_eq!(ClearWhat::from_label("formats"), Some(ClearWhat::Formats));
}
