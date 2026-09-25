use super::*;
use core::prelude::v1::test;

fn day(y: i64, m: u32, d: u32) -> DateTime {
    DateTime::from_ymd_hm(y, m, d, 8, 0)
}

fn corpus_editor(name: &str) -> ProjectEditor {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../corpus/mspdi")
        .join(name);
    let xml = std::fs::read_to_string(path).unwrap();
    ProjectEditor::new(projcore::mspdi::read_mspdi(&xml).unwrap())
}

/// The contract every ruler keeps, whatever its span and width.
fn assert_well_formed(r: &TimelineRuler, width: f32) {
    for (at, label) in &r.ticks {
        assert!(at.is_finite() && (0. ..1.).contains(at), "{at} {label}");
    }
    for w in r.ticks.windows(2) {
        assert!(w[0].0 < w[1].0, "strictly increasing: {w:?}");
        let gap = (w[1].0 - w[0].0) * width;
        assert!(
            gap >= r.unit.unwrap().min_gap() - 1e-3,
            "{gap}px < budget: {w:?}"
        );
    }
    assert!(!r.start.is_empty() && !r.finish.is_empty());
}

#[test]
fn dates_read_as_project_writes_them() {
    assert_eq!(project_date(day(2026, 3, 2)), "Mon 3/2/26");
    assert_eq!(project_date(day(2026, 3, 5)), "Thu 3/5/26");
    assert_eq!(project_date(day(2030, 12, 31)), "Tue 12/31/30");
    assert_eq!(project_date(day(2005, 1, 9)), "Sun 1/9/05");
}

#[test]
fn the_corpus_plan_spans_monday_to_thursday() {
    let ed = corpus_editor("02-link-fs.xml");
    let r = timeline_ruler(&ed, 1200.);
    assert_eq!(
        (r.start.as_str(), r.finish.as_str()),
        ("Mon 3/2/26", "Thu 3/5/26")
    );
    assert_eq!(r.unit, Some(TickUnit::Day));
    assert_eq!(
        r.ticks,
        vec![
            (0., "Mon 3/2".to_string()),
            (0.25, "Tue 3/3".to_string()),
            (0.5, "Wed 3/4".to_string()),
            (0.75, "Thu 3/5".to_string()),
        ]
    );
    assert_well_formed(&r, 1200.);
}

#[test]
fn the_unit_coarsens_as_the_span_grows_or_the_pane_narrows() {
    let start = day(2026, 3, 2);
    for (finish, width, unit) in [
        (start, 1200., TickUnit::Day),
        (start, 300., TickUnit::Day),
        (day(2026, 3, 5), 300., TickUnit::Day),
        (day(2026, 3, 5), 150., TickUnit::Week),
        (day(2026, 8, 31), 1200., TickUnit::Week),
        (day(2026, 8, 31), 600., TickUnit::Month),
        (day(2026, 8, 31), 300., TickUnit::Quarter),
        (day(2036, 3, 2), 300., TickUnit::Year),
    ] {
        let r = ruler(start, finish, width);
        assert_eq!(r.unit, Some(unit), "{} at {width}px", r.finish);
        assert!(!r.ticks.is_empty(), "{} at {width}px", r.finish);
        assert_well_formed(&r, width);
    }
    let r = ruler(start, day(2026, 8, 31), 300.);
    assert_eq!(
        r.ticks.iter().map(|t| t.1.as_str()).collect::<Vec<_>>(),
        ["Q2 '26", "Q3 '26"]
    );
    let r = ruler(start, day(2026, 8, 31), 600.);
    assert_eq!(r.ticks[0].1, "Apr '26");
    let r = ruler(start, day(2026, 8, 31), 1200.);
    assert_eq!(r.ticks[0], (0., "3/2".to_string()));
}

#[test]
fn years_thin_out_on_a_long_plan_in_a_narrow_pane() {
    let r = ruler(day(2026, 1, 1), day(2075, 12, 31), 300.);
    assert_eq!(r.unit, Some(TickUnit::Year));
    assert!(r.ticks.len() < 50 && r.ticks.len() > 1, "{:?}", r.ticks);
    assert_well_formed(&r, 300.);
}

#[test]
fn a_degenerate_span_or_width_still_labels_both_ends() {
    let ed = ProjectEditor::new(untitled_project());
    assert_eq!(
        ed.schedule().project_start.day_number(),
        ed.disp_project_finish().day_number()
    );
    for width in [0., -5., 1., 1200.] {
        let r = timeline_ruler(&ed, width);
        assert_eq!(r.start, r.finish);
        assert_well_formed(&r, width);
    }
    assert!(timeline_ruler(&ed, 0.).ticks.is_empty());
    // A finish before the start (never scheduled, but cheap to survive).
    let r = ruler(day(2026, 3, 5), day(2026, 3, 2), 400.);
    assert_eq!(r.ticks, vec![(0., "Thu 3/5".to_string())]);
}
