//! Hyperlinks follow structural edits (#1455).

use super::*;
use crate::sheet::Workbook;

fn book(links: &[(u32, u32)], refs: &[Area]) -> Workbook {
    let mut s = Sheet {
        name: "Sheet1".into(),
        ..Sheet::default()
    };
    for &rc in links {
        s.hyperlinks
            .insert(rc, format!("https://e.example/{}/{}", rc.0, rc.1));
    }
    for &rect in refs {
        s.hyperlink_refs.insert(
            (rect.0, rect.1),
            crate::sheet::LinkRef {
                loaded: rect,
                now: rect,
                target: s
                    .hyperlinks
                    .get(&(rect.0, rect.1))
                    .cloned()
                    .unwrap_or_default(),
            },
        );
    }
    let mut wb = Workbook::default();
    wb.sheets.push(s);
    wb
}

fn keys(wb: &Workbook) -> Vec<(u32, u32)> {
    wb.sheets[0].hyperlinks.keys().copied().collect()
}

#[test]
fn insert_rows_moves_links_at_or_below_the_edit() {
    let mut wb = book(&[(0, 0), (4, 0), (4, 2)], &[]);
    insert_rows(&mut wb, 0, 1, 1);
    assert_eq!(keys(&wb), vec![(0, 0), (5, 0), (5, 2)]);
    // The target travels with the cell.
    assert_eq!(wb.sheets[0].hyperlinks[&(5, 0)], "https://e.example/4/0");
}

#[test]
fn insert_at_the_linked_row_pushes_it_down() {
    let mut wb = book(&[(2, 1)], &[]);
    insert_rows(&mut wb, 0, 2, 3);
    assert_eq!(keys(&wb), vec![(5, 1)]);
}

#[test]
fn delete_rows_drops_the_span_and_pulls_later_links_up() {
    let mut wb = book(&[(0, 0), (1, 0), (2, 0), (5, 0)], &[]);
    delete_rows(&mut wb, 0, 1, 2);
    assert_eq!(keys(&wb), vec![(0, 0), (3, 0)]);
}

#[test]
fn column_edits_move_and_drop_links() {
    let mut wb = book(&[(0, 0), (0, 3), (2, 4)], &[]);
    insert_cols(&mut wb, 0, 3, 2);
    assert_eq!(keys(&wb), vec![(0, 0), (0, 5), (2, 6)]);
    delete_cols(&mut wb, 0, 5, 1);
    assert_eq!(keys(&wb), vec![(0, 0), (2, 5)]);
}

#[test]
fn a_link_pushed_off_the_sheet_is_dropped() {
    let mut wb = book(
        &[(MAX_ROWS - 1, 0), (0, MAX_COLS - 1)],
        &[(MAX_ROWS - 1, 0, MAX_ROWS - 1, 0)],
    );
    insert_rows(&mut wb, 0, 0, 1);
    assert_eq!(keys(&wb), vec![(1, MAX_COLS - 1)]);
    assert!(wb.sheets[0].hyperlink_refs.is_empty());
    insert_cols(&mut wb, 0, 0, 1);
    assert!(wb.sheets[0].hyperlinks.is_empty());
}

#[test]
fn an_unchanged_range_link_is_translated() {
    let mut wb = book(&[(0, 0), (1, 0), (2, 0)], &[(0, 0, 2, 0)]);
    insert_rows(&mut wb, 0, 0, 2);
    assert_eq!(keys(&wb), vec![(2, 0), (3, 0), (4, 0)]);
    let l = wb.sheets[0].hyperlink_refs[&(2, 0)].clone();
    assert_eq!((l.loaded, l.now), ((0, 0, 2, 0), (2, 0, 4, 0)));
    assert_eq!(wb.sheets[0].hyperlink_refs.len(), 1);
    let mut wb = book(&[(0, 1), (0, 2)], &[(0, 1, 0, 2)]);
    delete_cols(&mut wb, 0, 0, 1);
    assert_eq!(wb.sheets[0].hyperlink_refs[&(0, 0)].now, (0, 0, 0, 1));
}

#[test]
fn a_stretched_or_cut_range_link_is_forgotten_but_its_cells_stay() {
    // An insert inside stretches it; the cells move individually.
    let mut wb = book(&[(0, 0), (1, 0), (2, 0)], &[(0, 0, 2, 0)]);
    insert_rows(&mut wb, 0, 1, 1);
    assert!(wb.sheets[0].hyperlink_refs.is_empty());
    assert_eq!(keys(&wb), vec![(0, 0), (2, 0), (3, 0)]);
    // A delete inside shrinks it.
    let mut wb = book(&[(0, 0), (1, 0), (2, 0)], &[(0, 0, 2, 0)]);
    delete_rows(&mut wb, 0, 1, 1);
    assert!(wb.sheets[0].hyperlink_refs.is_empty());
    assert_eq!(keys(&wb), vec![(0, 0), (1, 0)]);
    // A delete over all of it removes it.
    let mut wb = book(&[(1, 0), (2, 0)], &[(1, 0, 2, 0)]);
    delete_rows(&mut wb, 0, 0, 4);
    assert!(wb.sheets[0].hyperlink_refs.is_empty());
    assert!(wb.sheets[0].hyperlinks.is_empty());
}

#[test]
fn a_range_link_too_big_to_spread_stretches_and_re_anchors() {
    // Only the anchor is in `hyperlinks`, as the loader spreads no more than
    // its limit.
    let big = (0, 0, 9999, 3);
    let mut wb = book(&[(0, 0)], &[big]);
    insert_rows(&mut wb, 0, 5, 1);
    assert_eq!(wb.sheets[0].hyperlink_refs[&(0, 0)].now, (0, 0, 10000, 3));
    assert_eq!(wb.sheets[0].hyperlink_refs[&(0, 0)].loaded, big);
    insert_rows(&mut wb, 0, 0, 1);
    assert_eq!(wb.sheets[0].hyperlink_refs[&(1, 0)].now, (1, 0, 10001, 3));
    assert_eq!(keys(&wb), vec![(1, 0)]);
    // The anchor's row deleted: the link stays on what is left.
    delete_rows(&mut wb, 0, 1, 1);
    assert_eq!(wb.sheets[0].hyperlink_refs[&(1, 0)].now, (1, 0, 10000, 3));
    assert_eq!(keys(&wb), vec![(1, 0)]);
    // Deleted whole: gone.
    delete_rows(&mut wb, 0, 0, 20000);
    assert!(wb.sheets[0].hyperlink_refs.is_empty());
    assert!(wb.sheets[0].hyperlinks.is_empty());
}

#[test]
fn a_removed_link_record_is_not_shifted() {
    let mut wb = book(&[], &[]);
    wb.sheets[0].hyperlinks_removed.push((4, 0, 4, 0));
    insert_rows(&mut wb, 0, 0, 2);
    delete_cols(&mut wb, 0, 0, 1);
    assert_eq!(wb.sheets[0].hyperlinks_removed, vec![(4, 0, 4, 0)]);
}

#[test]
fn removing_a_translated_range_link_removes_every_cell() {
    let mut wb = book(&[(0, 0), (1, 0), (2, 0)], &[(0, 0, 2, 0)]);
    insert_rows(&mut wb, 0, 0, 1);
    let plan = clear_plan(
        &wb.sheets[0],
        &[(2, 0, 2, 0)],
        ClearWhat::RemoveHyperlinks,
        &[],
    )
    .unwrap();
    apply_clear_sheet(&mut wb.sheets[0], &plan);
    assert!(wb.sheets[0].hyperlinks.is_empty());
}

#[test]
fn the_cells_of_a_cut_range_link_are_single_links_afterwards() {
    let mut wb = book(&[(0, 0), (1, 0), (2, 0)], &[(0, 0, 2, 0)]);
    insert_rows(&mut wb, 0, 1, 1);
    let plan = clear_plan(
        &wb.sheets[0],
        &[(0, 0, 0, 0)],
        ClearWhat::RemoveHyperlinks,
        &[],
    )
    .unwrap();
    apply_clear_sheet(&mut wb.sheets[0], &plan);
    assert_eq!(keys(&wb), vec![(2, 0), (3, 0)]);
}
