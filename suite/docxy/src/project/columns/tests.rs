use super::*;
use core::prelude::v1::test;

#[test]
fn widths_clamp_to_whole_px_within_the_limits() {
    assert_eq!(clamp_width(10.), MIN_WIDTH);
    assert_eq!(clamp_width(9000.), MAX_WIDTH);
    assert_eq!(clamp_width(100.4), 100.);
}

#[test]
fn best_fit_takes_the_widest_with_padding() {
    // The heading needs its arrow and the edge beside it.
    assert_eq!(
        best_fit_width(40., [10., 20.]),
        40. + ARROW_W + EDGE_W + FIT_PAD
    );
    // A wide cell wins.
    assert_eq!(best_fit_width(40., [10., 200.]), 200. + FIT_PAD);
    // Clamped.
    assert_eq!(best_fit_width(40., [5000.]), MAX_WIDTH);
    assert_eq!(
        best_fit_width(0., []),
        (ARROW_W + EDGE_W + FIT_PAD).max(MIN_WIDTH)
    );
}

fn view() -> ProjectView {
    ProjectView::new(Project::default(), false)
}

#[test]
fn a_new_view_draws_the_default_widths() {
    let v = view();
    assert_eq!(v.widths(), DEFAULT_WIDTHS);
    assert_eq!(v.table_total(), TABLE_W);
}

#[test]
fn a_drag_shows_live_and_is_one_undo_step_on_release() {
    let mut v = view();
    v.begin_width_drag(COL_NAME, 300.);
    assert!(v.move_width_drag(340.));
    assert_eq!(v.widths()[COL_NAME], DEFAULT_WIDTHS[COL_NAME] + 40.);
    assert_eq!(v.table_total(), TABLE_W + 40.);
    assert!(v.ed.project().table_view.is_empty(), "not recorded yet");
    assert!(!v.ed.dirty());
    assert!(v.move_width_drag(250.));
    assert!(v.end_width_drag());
    assert_eq!(v.widths()[COL_NAME], DEFAULT_WIDTHS[COL_NAME] - 50.);
    // Only that column changed.
    assert_eq!(v.widths()[COL_DURATION], DEFAULT_WIDTHS[COL_DURATION]);
    assert!(v.ed.undo());
    assert_eq!(v.widths(), DEFAULT_WIDTHS);
    assert!(!v.ed.undo(), "the drag was one step");
}

#[test]
fn a_press_and_release_in_place_resizes_nothing() {
    let mut v = view();
    v.begin_width_drag(COL_NAME, 300.);
    assert!(!v.move_width_drag(300.));
    assert!(!v.end_width_drag());
    assert!(!v.ed.dirty());
    assert!(v.width_drag.is_none());
    assert!(!v.move_width_drag(310.), "no drag in flight");
}

#[test]
fn a_drag_stops_at_the_limits() {
    let mut v = view();
    v.begin_width_drag(COL_ID, 0.);
    v.move_width_drag(-500.);
    assert_eq!(v.widths()[COL_ID], MIN_WIDTH);
    v.move_width_drag(5000.);
    assert_eq!(v.widths()[COL_ID], MAX_WIDTH);
}

#[test]
fn narrowing_columns_narrows_the_entry_table_pane() {
    let mut v = view();
    v.layout(2400.);
    let wide = v.table_w;
    assert_eq!(wide, TABLE_W);
    v.set_column_width(COL_RESOURCES, 40.);
    v.layout(2400.);
    assert_eq!(v.table_w, TABLE_W - DEFAULT_WIDTHS[COL_RESOURCES] + 40.);
    // The scroll range follows the content.
    assert_eq!(v.pane_scrolls().0.content, v.table_total());
}

#[test]
fn set_column_width_is_undoable_and_a_repeat_records_nothing() {
    let mut v = view();
    assert!(v.set_column_width(COL_NAME, 300.));
    assert!(!v.set_column_width(COL_NAME, 300.));
    assert!(v.ed.undo());
    assert_eq!(v.widths(), DEFAULT_WIDTHS);
}

#[test]
fn widths_survive_a_yppx_round_trip_and_an_mspdi_save_has_none() {
    let mut v = view();
    v.set_column_width(COL_NAME, 311.);
    let bytes = projcore::yppx::write_yppx(v.ed.project()).unwrap();
    let back = ProjectView::new(projcore::yppx::read_yppx(&bytes).unwrap(), false);
    assert_eq!(back.widths()[COL_NAME], 311.);
    assert_eq!(back.widths()[COL_ID], DEFAULT_WIDTHS[COL_ID]);
    let xml = projcore::mspdi::write_mspdi(v.ed.project());
    assert!(!xml.contains("311"), "MSPDI carries no widths");
    let again = ProjectView::new(projcore::mspdi::read_mspdi(&xml).unwrap(), false);
    assert_eq!(again.widths(), DEFAULT_WIDTHS);
}
