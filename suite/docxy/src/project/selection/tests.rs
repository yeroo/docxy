use super::*;
use core::prelude::v1::test;
use ctlcore::json::Json;
use gpui::Modifiers;

fn tab() -> DocTab {
    let p = Project {
        start_date: Some(projcore::editor::default_anchor()),
        tasks: (1..=3)
            .map(|id| Task {
                uid: id * 10,
                id,
                name: format!("Task {id}"),
                duration_min: 480,
                ..Task::default()
            })
            .collect(),
        ..Project::default()
    };
    project_tab(
        "test.yppx".into(),
        None,
        Surface::Project(ProjectView::new(p, false)),
        false,
        "loaded".into(),
    )
}
fn v(t: &DocTab) -> &ProjectView {
    let Surface::Project(v) = &t.surface else {
        panic!()
    };
    v
}
fn vm(t: &mut DocTab) -> &mut ProjectView {
    let Surface::Project(v) = &mut t.surface else {
        panic!()
    };
    v
}
fn chord(t: &mut DocTab, key: &str, control: bool, shift: bool) {
    let m = Modifiers {
        control,
        shift,
        ..Modifiers::default()
    };
    let text = (key == "space").then_some(" ");
    assert_eq!(project_input(t, key, text, m), None);
}
fn state(t: &DocTab, key: &str) -> String {
    let (_, j) = selection_state(v(t))
        .into_iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("no state key {key}"));
    j.to_string()
}

#[test]
fn a_clicked_cell_fills_itself_and_tints_its_id_and_heading_not_the_row() {
    let mut t = tab();
    project_cell_press(&mut t, 1, COL_DURATION, false);
    let look = v(&t).look();
    assert!(look.fill(Some(20), COL_DURATION));
    for col in (0..COLUMN_COUNT).filter(|&c| c != COL_DURATION) {
        assert!(!look.fill(Some(20), col), "col {col}");
    }
    assert!(!look.fill(Some(10), COL_DURATION) && !look.fill(Some(30), COL_DURATION));
    assert!(look.id_hot(20) && !look.id_hot(10));
    assert!(look.heading_hot(COL_DURATION) && !look.heading_hot(COL_NAME));
    assert_eq!(state(&t, "filled_cells"), r#"["2:Duration"]"#);
    assert_eq!(state(&t, "selected_columns"), r#"["Duration"]"#);
    assert_eq!(state(&t, "selected_rows"), "[2]");
    assert_eq!(state(&t, "hot_headings"), r#"["Duration"]"#);
    assert_eq!(state(&t, "cursor_border"), r#""2:Duration""#);
}

#[test]
fn an_id_press_fills_every_column_of_the_row() {
    let mut t = tab();
    project_cell_press(&mut t, 0, COL_ID, false);
    let look = v(&t).look();
    for col in 0..COLUMN_COUNT {
        assert!(look.fill(Some(10), col) && look.heading_hot(col));
        assert!(!look.fill(Some(20), col));
    }
    assert_eq!(state(&t, "selected_rows"), "[1]");
    assert_eq!(state(&t, "filled_cells").matches("1:").count(), COLUMN_COUNT);
}

#[test]
fn shift_space_selects_the_row_as_an_id_press_does() {
    let mut t = tab();
    project_cell_press(&mut t, 1, COL_START, false);
    chord(&mut t, "space", false, true);
    let (a, b) = (v(&t).look(), {
        let mut u = tab();
        project_cell_press(&mut u, 1, COL_ID, false);
        v(&u).look()
    });
    assert_eq!(a, b);
    assert!(v(&t).row_header, "Delete deletes the row, as for an ID press");
    assert_eq!(v(&t).col, COL_ID);
    assert!(a.fill(Some(20), COL_RESOURCES));
    // Nothing was typed into a cell.
    assert!(v(&t).cell.is_none());
}

#[test]
fn ctrl_space_and_a_heading_press_select_the_column() {
    let mut t = tab();
    project_cell_press(&mut t, 1, COL_FINISH, false);
    chord(&mut t, "space", true, false);
    let look = v(&t).look();
    assert_eq!(look.uids, [10, 20, 30]);
    assert_eq!(look.cols, COL_FINISH..=COL_FINISH);
    assert_eq!(v(&t).selected_uid(), Some(10), "the cursor is the first task");
    assert_eq!(state(&t, "selected_rows"), "[1,2,3]");
    assert!(look.heading_hot(COL_FINISH) && !look.heading_hot(COL_START));
    // The heading's left press is the same selection.
    let mut u = tab();
    project_column_select(&mut u, COL_FINISH);
    assert_eq!(v(&u).look(), look);
}

#[test]
fn ctrl_a_selects_the_whole_table_cursor_first_anchor_last() {
    let mut t = tab();
    project_cell_press(&mut t, 1, COL_NAME, false);
    chord(&mut t, "a", true, false);
    let s = v(&t).selection().unwrap();
    assert_eq!(s.count(), 3 * COLUMN_COUNT);
    assert_eq!((v(&t).selected_uid(), v(&t).col), (Some(10), 0));
    assert_eq!(v(&t).anchor, Some((30, COLUMN_COUNT - 1)));
    assert_eq!(state(&t, "selected_rows"), "[1,2,3]");
    assert_eq!(state(&t, "selected_columns").matches('"').count(), 2 * COLUMN_COUNT);
}

#[test]
fn ctrl_a_in_an_open_cell_still_selects_the_buffer() {
    let mut t = tab();
    project_cell_press(&mut t, 1, COL_NAME, false);
    vm(&mut t).open_cell(None).unwrap();
    let len = v(&t).cell.as_ref().unwrap().buf.len();
    assert!(len > 0, "Task 2");
    chord(&mut t, "a", true, false);
    assert_eq!(v(&t).cell.as_ref().unwrap().selection(), Some(0..len));
    assert!(v(&t).selection().is_none());
}

#[test]
fn ctrl_home_and_end_go_to_the_first_and_last_cell() {
    let mut t = tab();
    project_cell_press(&mut t, 1, COL_NAME, false);
    chord(&mut t, "end", true, false);
    assert_eq!((v(&t).selected_uid(), v(&t).col), (Some(30), COLUMN_COUNT - 1));
    chord(&mut t, "home", true, false);
    assert_eq!((v(&t).selected_uid(), v(&t).col), (Some(10), COL_ID));
}

#[test]
fn the_entry_row_fills_its_cursor_cell() {
    let mut t = tab();
    vm(&mut t).enter_entry_row();
    vm(&mut t).col = COL_NAME;
    let look = v(&t).look();
    assert!(look.fill(None, COL_NAME) && !look.fill(None, COL_START));
    assert!(look.uids.is_empty() && look.heading_hot(COL_NAME));
    assert_eq!(state(&t, "filled_cells"), r#"["entry:Name"]"#);
    // Shift+Space has no row to select there.
    chord(&mut t, "space", false, true);
    assert!(v(&t).on_entry_row());
}

#[test]
fn the_colours_are_washes_of_the_accent() {
    let (f, h) = (cell_fill_color(), header_tint_color());
    assert!(f.a > h.a && h.a > 0.);
    let c: Hsla = gpui::rgb(crate::accent::brand()).into();
    assert_eq!((f.h, f.s, f.l), (c.h, c.s, c.l));
}

#[test]
fn the_whole_entry_table_fits_the_default_window_and_a_narrow_one_scrolls() {
    let mut p = ProjectView::new(Project::default(), false);
    p.layout(1180.);
    assert!(p.table_total() <= p.table_w, "{} > {}", p.table_total(), p.table_w);
    assert_eq!(p.table_x.get(), 0.);
    assert!(project_state(&p, None).contains(&("table_fits".into(), Json::Bool(true))));
    // Every heading still holds its text, the arrow and the edge, but the two
    // longest names: those are cut off by the cell (overflow hidden).
    p.layout(900.);
    assert!(p.table_total() > p.table_w, "the table bar has a range");
    let (table, _) = p.pane_scrolls();
    assert!(table.content > table.viewport);
    let state = project_state(&p, None);
    assert!(state.contains(&("table_fits".into(), Json::Bool(false))));
    assert!(state.iter().any(|(k, _)| k == "selected_columns"));
    assert!(state.iter().any(|(k, _)| k == "selected_rows"));
    assert!(state.contains(&("table_w".into(), Json::Num(f64::from(p.table_w)))));
}

/// Each default width holds its heading beside the AutoFilter arrow and edge
/// (`pl_2` 8 + text + `ARROW_W` + `EDGE_W`) and the usual cell text with its
/// padding. Text widths are estimated per character at 12 px (6.8 bold
/// headings, 6.3 cells); the real measure needs a window.
#[test]
fn every_default_width_holds_its_heading_and_usual_cell_text() {
    const HEADING_CHAR: f32 = 6.8;
    const CELL_CHAR: f32 = 6.3;
    let cell_samples = [
        "9999",
        "Manually Scheduled",
        "",
        "999 days",
        "Mon 10/20/26",
        "Mon 10/20/26",
        "99FS+10 days",
        "",
    ];
    for col in 0..COLUMN_COUNT {
        let heading = 8. + COLUMNS[col].len() as f32 * HEADING_CHAR + ARROW_W + EDGE_W;
        let cell = cell_samples[col].len() as f32 * CELL_CHAR + 16.;
        assert!(
            DEFAULT_WIDTHS[col] >= heading.max(cell),
            "{}: {} < heading {heading} / cell {cell}",
            COLUMNS[col],
            DEFAULT_WIDTHS[col]
        );
    }
}
