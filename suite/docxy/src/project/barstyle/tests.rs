use super::*;
use core::prelude::v1::test;
use projcore::editor::untitled_project;
use projcore::formatting::{BarRow, TaskBar};

fn task(uid: i32, days: i64, level: u32) -> Task {
    Task {
        uid,
        id: uid,
        name: format!("Task {uid}"),
        duration_min: days * 480,
        outline_level: level,
        ..Task::default()
    }
}

fn editor(tasks: Vec<Task>, bars: Vec<BarRow>, task_bars: Vec<TaskBar>) -> ProjectEditor {
    let mut p = untitled_project();
    p.tasks = tasks;
    p.formatting.set_bars(bars);
    for t in task_bars {
        p.formatting.set_task_bar(t);
    }
    ProjectEditor::new(p)
}

fn look(color: u32) -> PaletteLook {
    PaletteLook {
        color,
        shape: BarShape::Bar,
        inactive: false,
    }
}

fn row(name: &str, show_for: &[&str], band: u8) -> BarRow {
    BarRow {
        name: name.into(),
        show_for: show_for.iter().map(|s| s.to_string()).collect(),
        row: band,
        ..BarRow::default()
    }
}

fn draw(x0: f32, x1: f32, band: u8) -> DrawRow {
    DrawRow {
        name: "r".into(),
        band,
        x0,
        x1,
        start: Mark::default(),
        middle: Middle::default(),
        end: Mark::default(),
        texts: Vec::new(),
        color: 0x112233,
        palette_box: None,
    }
}

fn middle(parts: &[Part]) -> (f32, f32, f32, f32) {
    parts
        .iter()
        .find_map(|p| match p {
            Part::Middle { rect, .. } => Some(*rect),
            _ => None,
        })
        .unwrap()
}

#[test]
fn the_middle_takes_its_shape_and_alignment_in_the_bar_zone() {
    let at = |shape: &str| {
        let mut r = draw(22., 88., 1);
        r.middle.shape = Some(shape.into());
        middle(&bar_parts(&[r]))
    };
    // Full is the zone; an unset or unknown shape is full.
    assert_eq!(at("full"), (22., BAR_Y, 66., BAR_H));
    assert_eq!(at("zig"), at("full"));
    assert_eq!(middle(&bar_parts(&[draw(22., 88., 1)])), at("full"));
    assert_eq!(at("medium-top"), (22., BAR_Y, 66., 8.));
    assert_eq!(at("medium-middle"), (22., BAR_Y + 1., 66., 8.));
    assert_eq!(at("medium-bottom"), (22., BAR_Y + 2., 66., 8.));
    assert_eq!(at("narrow-top"), (22., BAR_Y, 66., 4.));
    assert_eq!(at("narrow-middle"), (22., BAR_Y + 3., 66., 4.));
    assert_eq!(at("narrow-bottom"), (22., BAR_Y + 6., 66., 4.));
    // The colour is the middle's own, else the row's (the task's).
    let mut r = draw(0., 22., 1);
    assert!(matches!(
        &bar_parts(&[r.clone()])[0],
        Part::Middle {
            color: 0x112233,
            ..
        }
    ));
    r.middle.color = Some(0xFF0000);
    r.middle.pattern = Some("dark-fill".into());
    assert!(matches!(
        &bar_parts(&[r])[0],
        Part::Middle { color: 0xFF0000, pattern: Some(p), .. } if p == "dark-fill"
    ));
}

#[test]
fn a_row_draws_in_its_band_and_rows_keep_table_order() {
    let parts = bar_parts(&[draw(22., 88., 1), draw(44., 66., 3), draw(0., 22., 2)]);
    let bands: Vec<(u8, f32)> = parts
        .iter()
        .map(|p| match p {
            Part::Middle { band, rect, .. } => (*band, rect.1),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        bands,
        [(1, BAR_Y), (3, 2. * ROW_H + BAR_Y), (2, ROW_H + BAR_Y)]
    );
}

#[test]
fn marks_sit_on_the_ends_and_a_milestone_has_both_on_one_point() {
    let mut r = draw(22., 88., 2);
    r.start.shape = Some("diamond".into());
    r.start.kind = Some("framed".into());
    r.end.shape = Some("circle".into());
    r.end.color = Some(0x00FF00);
    let parts = bar_parts(&[r]);
    let marks: Vec<_> = parts
        .iter()
        .filter_map(|p| match p {
            Part::Mark {
                rect,
                shape,
                kind,
                color,
                ..
            } => Some((*rect, *shape, *kind, *color)),
            _ => None,
        })
        .collect();
    let y = ROW_H + BAR_Y;
    assert_eq!(
        marks,
        [
            (
                (22. - MARK / 2., y, MARK, MARK),
                MarkShape::Diamond,
                MarkKind::Framed,
                0x112233
            ),
            (
                (88. - MARK / 2., y, MARK, MARK),
                MarkShape::Circle,
                MarkKind::Solid,
                0x00FF00
            ),
        ]
    );
    // No span: a zero-width middle at the point, both marks on it.
    let mut m = draw(44., 44., 1);
    m.start.shape = Some("square".into());
    m.end.shape = Some("arrow-up".into());
    let parts = bar_parts(&[m]);
    assert_eq!(middle(&parts), (44., BAR_Y, 0., BAR_H));
    let xs: Vec<f32> = parts
        .iter()
        .filter_map(|p| match p {
            Part::Mark { rect, .. } => Some(rect.0),
            _ => None,
        })
        .collect();
    assert_eq!(xs, [39., 39.]);
    // A shape outside the vocabulary and `none` draw no mark.
    let mut none = draw(0., 22., 1);
    none.start.shape = Some("none".into());
    none.end.shape = Some("star".into());
    assert_eq!(bar_parts(&[none]).len(), 1);
}

#[test]
fn the_five_texts_sit_around_the_bar() {
    let mut r = draw(100., 200., 2);
    r.start.shape = Some("square".into());
    r.texts = ["left", "right", "top", "bottom", "inside"]
        .into_iter()
        .map(|p| (p, p.to_uppercase()))
        .collect();
    type Placed = ((f32, f32, f32, f32), Align, Option<u32>);
    let texts: std::collections::HashMap<&str, Placed> = bar_parts(&[r])
        .into_iter()
        .filter_map(|p| match p {
            Part::Text {
                pos,
                rect,
                align,
                on,
                ..
            } => Some((pos, (rect, align, on))),
            _ => None,
        })
        .collect();
    let band = ROW_H;
    let zone = band + BAR_Y;
    // Left ends before the start mark, right starts after the (absent) end
    // mark, top and bottom are centred on the span in the bands above and
    // below the zone, inside fills the middle over its colour.
    let (rect, align, on) = texts["left"];
    assert_eq!(
        (rect.0 + rect.2, rect.1, rect.3, align, on),
        (100. - 5. - 4., zone, BAR_H, Align::Right, None)
    );
    let (rect, align, _) = texts["right"];
    assert_eq!(
        (rect.0, rect.1, rect.3, align),
        (200. + 4., zone, BAR_H, Align::Left)
    );
    let (rect, align, _) = texts["top"];
    assert_eq!(
        (rect.0 + rect.2 / 2., rect.1, rect.3, align),
        (150., band, BAR_Y, Align::Center)
    );
    let (rect, _, _) = texts["bottom"];
    assert_eq!((rect.0 + rect.2 / 2., rect.1), (150., zone + BAR_H));
    assert!(rect.1 + rect.3 <= band + ROW_H, "{rect:?}");
    let (rect, align, on) = texts["inside"];
    assert_eq!(
        (rect, align, on),
        ((100., zone, 100., BAR_H), Align::Center, Some(0x112233))
    );
}

#[test]
fn mark_polygons_fit_their_square_and_dash_edges_ink_every_corner() {
    for shape in [
        MarkShape::Square,
        MarkShape::Diamond,
        MarkShape::Circle,
        MarkShape::TriangleUp,
        MarkShape::TriangleDown,
        MarkShape::ArrowLeft,
        MarkShape::ArrowRight,
        MarkShape::ArrowUp,
        MarkShape::ArrowDown,
    ] {
        let poly = shape.polygon();
        assert!(poly.len() >= 3, "{shape:?}");
        for (x, y) in &poly {
            assert!(
                x.abs() <= MARK / 2. + 1e-4 && y.abs() <= MARK / 2. + 1e-4,
                "{shape:?}"
            );
        }
    }
    assert!(MarkShape::None.polygon().is_empty());
    assert_eq!(MarkShape::parse(Some("star")), MarkShape::None);
    // Arrows point where they say: the tip is the far extreme.
    let tip = |s: MarkShape| {
        let p = s.polygon();
        (
            p.iter().map(|v| v.0).fold(f32::MIN, f32::max),
            p.iter().map(|v| v.0).fold(f32::MAX, f32::min),
            p.iter().map(|v| v.1).fold(f32::MIN, f32::max),
            p.iter().map(|v| v.1).fold(f32::MAX, f32::min),
        )
    };
    assert_eq!(tip(MarkShape::ArrowRight).0, 5.);
    assert_eq!(tip(MarkShape::ArrowLeft).1, -5.);
    assert_eq!(tip(MarkShape::ArrowDown).2, 5.);
    assert_eq!(tip(MarkShape::ArrowUp).3, -5.);
    // 10 px edges with 3 px dashes and 2 px gaps: dashes at 0 and 5; the
    // 14 px closing edge has three.
    let edges = dash_edges(&[(0., 0.), (10., 0.), (10., 10.)], 3., 2.);
    assert_eq!(edges.len(), 2 + 2 + 3);
    assert_eq!(edges[0], ((0., 0.), (3., 0.)));
    assert_eq!(edges[1], ((5., 0.), (8., 0.)));
    assert!(dash_edges(&[], 3., 2.).is_empty());
}

#[test]
fn rows_resolve_per_task_in_table_order_and_miss_dateless_ones() {
    let mut summary = task(1, 0, 1);
    summary.summary = true;
    let ed = editor(
        vec![summary, task(2, 2, 2), task(3, 0, 2)],
        vec![
            row("Every", &[], 1),
            row("Leaves", &["Normal"], 3),
            BarRow {
                from: "Actual Start".into(),
                ..row("Actual", &[], 2)
            },
        ],
        Vec::new(),
    );
    let scale = gantt_scale(&ed);
    let names = |uid: i32| -> Vec<String> {
        let t = ed.project().task(uid).unwrap();
        bar_rows(&ed, t, scale, look(0x123456))
            .unwrap()
            .into_iter()
            .map(|r| format!("{}@{}", r.name, r.band))
            .collect()
    };
    // The summary shows for "Every" only; the leaf for it and "Leaves"; the
    // milestone (no actual start either) for "Every". The "Actual" row has no
    // From date for any of them and draws nothing.
    assert_eq!(names(1), ["Every@1"]);
    assert_eq!(names(2), ["Every@1", "Leaves@3"]);
    assert_eq!(names(3), ["Every@1"]);
    // The leaf spans its days; the milestone is a point.
    let leaf = bar_rows(&ed, ed.project().task(2).unwrap(), scale, look(1)).unwrap();
    let bar = gantt_bar(&ed, ed.project().task(2).unwrap(), scale).unwrap();
    assert_eq!(leaf[0].x0, bar.start as f32 * scale.day_w);
    assert_eq!(leaf[0].x1, (bar.end + 1) as f32 * scale.day_w);
    let ms = bar_rows(&ed, ed.project().task(3).unwrap(), scale, look(1)).unwrap();
    assert_eq!(ms[0].x0, ms[0].x1);
    // The state lines name uid, band and row.
    assert_eq!(drawn_entries(2, &leaf), ["2:1:Every", "2:3:Leaves"]);
}

#[test]
fn an_override_merges_over_every_row_it_draws() {
    let mut first = row("A", &[], 1);
    first.middle.color = Some(0x111111);
    first.start.shape = Some("square".into());
    first.text.right = Some("Name".into());
    let mut second = row("B", &[], 2);
    second.end.shape = Some("circle".into());
    let over = TaskBar {
        uid: 2,
        middle: Middle {
            color: Some(0x00AA00),
            ..Middle::default()
        },
        start: Mark {
            color: Some(0xAA0000),
            ..Mark::default()
        },
        text: BarText {
            top: Some("Resource Names".into()),
            ..BarText::default()
        },
        ..TaskBar::default()
    };
    let ed = editor(
        vec![task(1, 2, 1), task(2, 2, 1)],
        vec![first, second],
        vec![over],
    );
    let scale = gantt_scale(&ed);
    let rows = bar_rows(&ed, ed.project().task(2).unwrap(), scale, look(7)).unwrap();
    assert_eq!(rows.len(), 2);
    for r in &rows {
        assert_eq!(r.middle.color, Some(0x00AA00));
        assert_eq!(r.start.color, Some(0xAA0000));
    }
    // A set part wins, an unset one keeps the row's.
    assert_eq!(rows[0].start.shape.as_deref(), Some("square"));
    assert_eq!(rows[1].end.shape.as_deref(), Some("circle"));
    assert_eq!(rows[0].texts, [("right", "Task 2".to_string())]);
    // The other task is untouched.
    let rows = bar_rows(&ed, ed.project().task(1).unwrap(), scale, look(7)).unwrap();
    assert_eq!(rows[0].middle.color, Some(0x111111));
    assert_eq!(rows[0].start.color, None);
}

#[test]
fn an_override_over_no_table_restyles_a_plain_leaf_bar_only() {
    let mut summary = task(1, 0, 1);
    summary.summary = true;
    let over = |uid| TaskBar {
        uid,
        middle: Middle {
            color: Some(0x00AA00),
            shape: Some("narrow-middle".into()),
            ..Middle::default()
        },
        ..TaskBar::default()
    };
    let ed = editor(
        vec![summary, task(2, 3, 2), task(3, 0, 2), task(4, 1, 2)],
        Vec::new(),
        vec![over(1), over(2), over(3)],
    );
    let scale = gantt_scale(&ed);
    let get = |uid: i32| bar_rows(&ed, ed.project().task(uid).unwrap(), scale, look(0x445566));
    // The leaf is drawn as one full-height row over its palette colour.
    let rows = get(2).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].band, rows[0].color), (1, 0x445566));
    let parts = bar_parts(&rows);
    assert!(matches!(
        &parts[0],
        Part::Middle {
            color: 0x00AA00,
            ..
        }
    ));
    assert_eq!(middle(&parts).3, NARROW_H);
    // A summary or milestone with an override keeps the palette drawing, and
    // so does a task without one.
    assert_eq!(get(1), None);
    assert_eq!(get(3), None);
    assert_eq!(get(4), None);
    // No table and no override: nothing is engaged anywhere.
    let none = editor(vec![task(1, 2, 1)], Vec::new(), Vec::new());
    assert_eq!(
        bar_rows(
            &none,
            none.project().task(1).unwrap(),
            gantt_scale(&none),
            look(1)
        ),
        None
    );
}

#[test]
fn first_middle_is_the_first_rows_middle() {
    let rows = vec![draw(22., 88., 2), draw(0., 44., 1)];
    assert_eq!(first_middle(&rows), Some((22., ROW_H + BAR_Y, 66., BAR_H)));
    assert_eq!(first_middle(&[]), None);
}

#[test]
fn the_task_row_grows_with_the_highest_band() {
    let mut p = untitled_project();
    p.tasks = vec![task(1, 2, 1)];
    let mut view = ProjectView::new(p, false);
    // No table, or rows only in band 1: the row is what it always was.
    assert_eq!(view.row_h(), ROW_H);
    let set = |view: &mut ProjectView, bands: &[u8]| {
        let rows = bands.iter().map(|&b| row("r", &[], b)).collect();
        view.ed.edit_formatting(|f| f.set_bars(rows));
    };
    set(&mut view, &[1]);
    assert_eq!(view.row_h(), ROW_H);
    // Band 3 makes every row three bands tall; a band past 4 clamps.
    set(&mut view, &[1, 3]);
    assert_eq!(view.row_h(), 3. * ROW_H);
    set(&mut view, &[9]);
    assert_eq!(view.row_h(), 4. * ROW_H);
    // The chart's maths follow it: rules at the bottom of each tall row, the
    // empty rows left below, and the rows a page holds.
    set(&mut view, &[1, 3]);
    let h = view.row_h();
    assert_eq!(
        ruled_rows_of(2. * h, 0., h),
        [(0, h - 1.), (1, 2. * h - 1.)]
    );
    assert_eq!(ruled_rows_of(h, h + 10., h), [(1, 2. * h - 1. - (h + 10.))]);
    assert_eq!(filler_rows_of(5. * h, 0., 2, h), 3);
    assert_eq!(page_rows_of(h * 4., h), 3);
    // Back to no table: the original height.
    view.ed.edit_formatting(|f| f.set_bars(Vec::new()));
    assert_eq!(view.row_h(), ROW_H);
}

/// Tasks of 1, 5 and 2 days with the progress each is given, under `bars`.
fn progress_editor(percents: [u8; 3], bars: Vec<BarRow>) -> ProjectEditor {
    let tasks = [1, 5, 2]
        .into_iter()
        .zip(percents)
        .enumerate()
        .map(|(k, (days, pct))| {
            let mut t = task(k as i32 + 1, days, 1);
            t.percent_complete = Some(pct);
            t
        })
        .collect();
    editor(tasks, bars, Vec::new())
}

fn progress_row() -> BarRow {
    BarRow {
        to: "CompleteThrough".into(),
        ..row("Progress", &[], 1)
    }
}

#[test]
fn completed_through_lands_in_its_working_day_of_the_whole_day_bar() {
    // 100%: the progress ends where the task bar does, for 1 and 5 days.
    let ed = progress_editor([100, 100, 100], vec![progress_row()]);
    let scale = gantt_scale(&ed);
    for uid in [1, 2, 3] {
        let t = ed.project().task(uid).unwrap();
        let bar = gantt_bar(&ed, t, scale).unwrap();
        let r = bar_rows(&ed, t, scale, look(1)).unwrap();
        assert_eq!(r[0].x0, bar.start as f32 * scale.day_w, "task {uid}");
        assert_eq!(r[0].x1, (bar.end + 1) as f32 * scale.day_w, "task {uid}");
    }
    // 0%: nothing done, a zero-width middle at the start of the bar.
    let ed = progress_editor([0, 0, 0], vec![progress_row()]);
    let scale = gantt_scale(&ed);
    for uid in [1, 2, 3] {
        let t = ed.project().task(uid).unwrap();
        let r = bar_rows(&ed, t, scale, look(1)).unwrap();
        assert_eq!(r[0].x0, r[0].x1, "task {uid}");
        assert_eq!(first_middle(&r).unwrap().2, 0., "task {uid}");
    }
    // 50% of two days is exactly one day; 10% of five days is four of the nine
    // working hours of its first day, 10% of one day 48 of 540 minutes.
    let ed = progress_editor([10, 10, 50], vec![progress_row()]);
    let scale = gantt_scale(&ed);
    let w = |uid: i32| {
        let r = bar_rows(&ed, ed.project().task(uid).unwrap(), scale, look(1)).unwrap();
        r[0].x1 - r[0].x0
    };
    assert_eq!(w(3), scale.day_w);
    assert!((w(2) - scale.day_w * 4. / 9.).abs() < 1e-3, "{}", w(2));
    assert!((w(1) - scale.day_w * 0.8 / 9.).abs() < 1e-3, "{}", w(1));
}

/// Tasks starting Thursday 8 January 2026 (the project start) with the
/// bar `bar`.
fn thursday_editor(tasks: Vec<Task>, bar: BarRow) -> ProjectEditor {
    let mut p = untitled_project();
    p.start_date = Some(projcore::DateTime::from_ymd_hm(2026, 1, 8, 8, 0));
    p.tasks = tasks;
    p.formatting.set_bars(vec![bar]);
    ProjectEditor::new(p)
}

#[test]
fn completed_through_spans_a_weekend_and_a_stop_at_the_end_of_a_day() {
    let days = |ed: &ProjectEditor, uid: i32| {
        let scale = gantt_scale(ed);
        let r = bar_rows(ed, ed.project().task(uid).unwrap(), scale, look(1)).unwrap();
        (r[0].x1 - r[0].x0) / scale.day_w
    };
    // Thu to Tue, four working days, 50% done: the end of Friday, two
    // calendar days in (the weekend is not worked and not counted in).
    let mut t = task(1, 4, 1);
    t.percent_complete = Some(50);
    let ed = thursday_editor(vec![t], progress_row());
    assert_eq!(days(&ed, 1), 2.);
    // 75% is the end of Monday: five calendar days, the weekend inside.
    let mut t = task(1, 4, 1);
    t.percent_complete = Some(75);
    let ed = thursday_editor(vec![t], progress_row());
    assert_eq!(days(&ed, 1), 5.);
    // A stop at the end of day 1 of 2 is exactly one day.
    let mut t = task(1, 2, 1);
    t.stop = Some(projcore::DateTime::from_ymd_hm(2026, 1, 8, 17, 0));
    let ed = thursday_editor(vec![t], progress_row());
    assert_eq!(days(&ed, 1), 1.);
    // Monday to Wednesday with a stop on Tuesday 17:00 is two days.
    let mut t = task(1, 3, 1);
    t.stop = Some(projcore::DateTime::from_ymd_hm(2026, 1, 13, 17, 0));
    let mut p = untitled_project();
    p.start_date = Some(projcore::DateTime::from_ymd_hm(2026, 1, 12, 8, 0));
    p.tasks = vec![t];
    p.formatting.set_bars(vec![progress_row()]);
    let ed = ProjectEditor::new(p);
    assert_eq!(days(&ed, 1), 2.);
    // A stop at noon is the middle of its working day's frame.
    let mut t = task(1, 2, 1);
    t.stop = Some(projcore::DateTime::from_ymd_hm(2026, 1, 8, 12, 30));
    let ed = thursday_editor(vec![t], progress_row());
    assert_eq!(days(&ed, 1), 0.5);
}

#[test]
fn a_point_row_sits_at_its_date_by_kind() {
    let point = |from: &str, to: &str| BarRow {
        from: from.into(),
        to: to.into(),
        ..row("P", &[], 1)
    };
    let mut t = task(1, 3, 1);
    t.deadline = Some(projcore::DateTime::from_ymd_hm(2026, 1, 9, 17, 0));
    let ed = editor(
        vec![t],
        vec![
            point("Finish", "Finish"),
            point("Start", "Start"),
            point("Deadline", "Deadline"),
        ],
        Vec::new(),
    );
    let scale = gantt_scale(&ed);
    let tk = ed.project().task(1).unwrap();
    let bar = gantt_bar(&ed, tk, scale).unwrap();
    let rows = bar_rows(&ed, tk, scale, look(1)).unwrap();
    let dw = scale.day_w;
    // Finish/Finish is the bar's end, Start/Start its start, a deadline its
    // instant (the overlay's deadline_x).
    assert_eq!(
        (rows[0].x0, rows[0].x1),
        ((bar.end + 1) as f32 * dw, (bar.end + 1) as f32 * dw)
    );
    assert_eq!(
        (rows[1].x0, rows[1].x1),
        (bar.start as f32 * dw, bar.start as f32 * dw)
    );
    let deadline = instant_x(scale, 0., tk.deadline.unwrap());
    assert_eq!((rows[2].x0, rows[2].x1), (deadline, deadline));
}

#[test]
fn a_milestone_row_is_centred_in_its_day_like_the_palette_diamond() {
    let mut ms = row("M", &[], 1);
    ms.start.shape = Some("diamond".into());
    let ed = editor(vec![task(1, 2, 1), task(2, 0, 1)], vec![ms], Vec::new());
    let scale = gantt_scale(&ed);
    let t = ed.project().task(2).unwrap();
    let bar = gantt_bar(&ed, t, scale).unwrap();
    let rows = bar_rows(&ed, t, scale, look(1)).unwrap();
    // The diamond's 14 px box starts at milestone_x; its centre is the mark's.
    let centre = milestone_x(bar.start, scale.day_w) + 7.;
    assert_eq!((rows[0].x0, rows[0].x1), (centre, centre));
    let marks: Vec<f32> = bar_parts(&rows)
        .iter()
        .filter_map(|p| match p {
            Part::Mark { rect, .. } => Some(rect.0 + rect.2 / 2.),
            _ => None,
        })
        .collect();
    assert_eq!(marks, [centre]);
    assert_eq!(first_middle(&rows).unwrap().2, 0.);
}

#[test]
fn a_colour_only_override_keeps_the_palette_bar_in_place_and_size() {
    let over = |uid, shape: Option<&str>| TaskBar {
        uid,
        middle: Middle {
            color: Some(0x00AA00),
            shape: shape.map(str::to_string),
            ..Middle::default()
        },
        ..TaskBar::default()
    };
    let mut inactive = task(2, 2, 1);
    inactive.active = Some(false);
    let ed = editor(
        vec![task(1, 3, 1), inactive, task(3, 3, 1)],
        Vec::new(),
        vec![over(1, None), over(2, None), over(3, Some("narrow-top"))],
    );
    let scale = gantt_scale(&ed);
    let rect_of = |uid: i32| {
        let t = ed.project().task(uid).unwrap();
        let bar = gantt_bar(&ed, t, scale).unwrap();
        let rows = bar_rows(&ed, t, scale, PaletteLook::of(Some(bar), None)).unwrap();
        (bar_rect(&bar, scale.day_w), rows)
    };
    // Same rectangle as the palette bar.
    let (palette, rows) = rect_of(1);
    assert_eq!(first_middle(&rows), Some(palette));
    assert!(matches!(
        &bar_parts(&rows)[0],
        Part::Middle {
            color: 0x00AA00,
            pattern: None,
            ..
        }
    ));
    // An inactive task stays an outline.
    let (palette, rows) = rect_of(2);
    assert_eq!(first_middle(&rows), Some(palette));
    assert!(matches!(&bar_parts(&rows)[0], Part::Middle { pattern: Some(p), .. } if p == "hollow"));
    // Naming a shape moves it to the standard bar zone.
    let (_, rows) = rect_of(3);
    assert_eq!(first_middle(&rows).unwrap().1, BAR_Y);
    assert_eq!(first_middle(&rows).unwrap().3, NARROW_H);
    // A thin palette bar keeps its place too.
    let thin = PaletteLook {
        shape: BarShape::Thin,
        ..look(1)
    };
    let t = ed.project().task(1).unwrap();
    let r = bar_rows(&ed, t, scale, thin).unwrap();
    assert_eq!(first_middle(&r).map(|m| (m.1, m.3)), Some((11., 6.)));
    // The table's presence ends the stand-in: BAR_Y zone, no palette box.
    let with_table = editor(
        vec![task(1, 3, 1)],
        vec![row("A", &[], 1)],
        vec![over(1, None)],
    );
    let r = bar_rows(
        &with_table,
        with_table.project().task(1).unwrap(),
        gantt_scale(&with_table),
        look(1),
    )
    .unwrap();
    assert_eq!(first_middle(&r).map(|m| (m.1, m.3)), Some((BAR_Y, BAR_H)));
    // An override keeps the baseline and delay marks, a table drops them.
    let kept = bar_styled(&ed, ed.project().task(1).unwrap(), scale, look(1)).unwrap();
    assert!(kept.marks);
    let dropped = bar_styled(
        &with_table,
        with_table.project().task(1).unwrap(),
        gantt_scale(&with_table),
        look(1),
    )
    .unwrap();
    assert!(!dropped.marks);
}

#[test]
fn bars_drawn_lists_the_shown_rows_in_drawn_order() {
    let mut p = untitled_project();
    p.tasks = vec![task(1, 0, 1), task(2, 2, 2), task(3, 2, 1)];
    p.tasks[0].summary = true;
    p.formatting.set_bars(vec![row("R", &[], 1)]);
    let mut view = ProjectView::new(p, false);
    let scale = view.chart_scale();
    assert_eq!(bars_drawn(&view, scale), ["1:1:R", "2:1:R", "3:1:R"]);
    // Collapsing the summary hides task 2: it is no longer drawn or listed.
    view.ed.set_collapsed(1, true).unwrap();
    assert_eq!(bars_drawn(&view, scale), ["1:1:R", "3:1:R"]);
}

#[test]
fn only_a_milestones_own_dates_are_centred() {
    let mut t = task(1, 0, 1);
    t.deadline = Some(projcore::DateTime::from_ymd_hm(2026, 1, 9, 17, 0));
    let point = |from: &str, to: &str| BarRow {
        from: from.into(),
        to: to.into(),
        ..row("P", &[], 1)
    };
    let ed = editor(
        vec![t],
        vec![
            point("Deadline", "Deadline"),
            point("Task Start", "Task Finish"),
        ],
        Vec::new(),
    );
    let scale = gantt_scale(&ed);
    let tk = ed.project().task(1).unwrap();
    let rows = bar_rows(&ed, tk, scale, look(1)).unwrap();
    let deadline = instant_x(scale, 0., tk.deadline.unwrap());
    assert_eq!((rows[0].x0, rows[0].x1), (deadline, deadline));
    let bar = gantt_bar(&ed, tk, scale).unwrap();
    let centre = milestone_x(bar.start, scale.day_w) + 7.;
    assert_eq!((rows[1].x0, rows[1].x1), (centre, centre));
}

#[test]
fn only_painted_pieces_take_the_bars_press() {
    // A bar from 22 to 66 and a point row far to the right with an end mark,
    // in one band: the empty chart between them is in no hit box, so a press
    // there opens the chart's menu, not the bar's.
    let mut bar = draw(22., 66., 1);
    bar.start.shape = Some("square".into());
    bar.texts = vec![("right", "Name".into())];
    let mut far = draw(600., 600., 1);
    far.end.shape = Some("circle".into());
    let parts = bar_parts(&[bar, far]);
    let boxes: Vec<_> = parts.iter().filter_map(hit_box).collect();
    // The bar's middle and start mark, the point's end mark; no text, and
    // no box for the point row's zero-width middle.
    assert_eq!(boxes.len(), 3);
    let hit = |x: f32, y: f32| {
        boxes
            .iter()
            .any(|b| x >= b.0 && x < b.0 + b.2 && y >= b.1 && y < b.1 + b.3)
    };
    let y = BAR_Y + 2.;
    assert!(hit(30., y) && hit(600., y));
    assert!(!hit(300., y), "the gap is the chart's");
    assert!(!hit(100., y), "the text beside the bar is not pressable");
    assert!(!hit(30., BAR_Y + BAR_H + 4.));
}

#[test]
fn the_models_vocabularies_are_the_ones_the_chart_draws() {
    use projcore::formatting::bars::{MARK_KINDS, MARK_SHAPES, MIDDLE_SHAPES};
    for name in MARK_SHAPES {
        let shape = MarkShape::parse(Some(name));
        assert_eq!(shape == MarkShape::None, name == "none", "{name}");
        assert_eq!(shape.polygon().is_empty(), name == "none", "{name}");
    }
    let kinds: Vec<MarkKind> = MARK_KINDS
        .iter()
        .map(|k| MarkKind::parse(Some(k)))
        .collect();
    assert_eq!(kinds, [MarkKind::Solid, MarkKind::Dashed, MarkKind::Framed]);
    let full = middle_geometry(Some("full"));
    assert_eq!(full, (0., BAR_H));
    assert_eq!(middle_geometry(Some("unknown")), full);
    assert_eq!(MIDDLE_SHAPES[0], "full");
    for name in &MIDDLE_SHAPES[1..] {
        let (y, h) = middle_geometry(Some(name));
        assert_ne!((y, h), full, "{name}");
        assert!(h == MEDIUM_H || h == NARROW_H, "{name}");
        assert!(y >= 0. && y + h <= BAR_H, "{name}");
    }
}

/// A Monday 13:00 start (Start No Earlier Than) for a task of `days`.
fn afternoon_editor(days: i64, pct: u8) -> ProjectEditor {
    let mut t = task(1, days, 1);
    t.percent_complete = Some(pct);
    t.constraint = projcore::ConstraintType::StartNoEarlierThan;
    t.constraint_date = Some(projcore::DateTime::from_ymd_hm(2026, 1, 5, 13, 0));
    let mut p = untitled_project();
    p.start_date = Some(projcore::DateTime::from_ymd_hm(2026, 1, 5, 8, 0));
    p.tasks = vec![t];
    p.formatting.set_bars(vec![progress_row()]);
    let ed = ProjectEditor::new(p);
    assert_eq!(
        ed.disp_start(1),
        Some(projcore::DateTime::from_ymd_hm(2026, 1, 5, 13, 0))
    );
    ed
}

#[test]
fn progress_is_placed_against_the_bar_not_the_whole_working_day() {
    let progress = |ed: &ProjectEditor| {
        let scale = gantt_scale(ed);
        let t = ed.project().task(1).unwrap();
        let bar = gantt_bar(ed, t, scale).unwrap();
        let r = bar_rows(ed, t, scale, look(1)).unwrap();
        (r[0].x0, r[0].x1, bar, scale.day_w)
    };
    // A 13:00 start at 0%: nothing done, a zero-width middle at the bar's
    // start, not 5/9 of a day of progress.
    let (x0, x1, bar, dw) = progress(&afternoon_editor(1, 0));
    assert_eq!((x0, x1), (bar.start as f32 * dw, bar.start as f32 * dw));
    // One day from Monday 13:00 finishes Tuesday 12:00; at 100% the progress
    // ends where the bar does, not 5/9 of a day short.
    let ed = afternoon_editor(1, 100);
    assert_eq!(
        ed.disp_finish(1),
        Some(projcore::DateTime::from_ymd_hm(2026, 1, 6, 12, 0))
    );
    let (x0, x1, bar, dw) = progress(&ed);
    assert_eq!(x0, bar.start as f32 * dw);
    assert_eq!(x1, (bar.end + 1) as f32 * dw);
    // 50% is four of its eight working hours: the end of Monday, one day in.
    let (x0, x1, _, dw) = progress(&afternoon_editor(1, 50));
    assert_eq!(x1 - x0, dw);
    // 25% is two hours into Monday's four: half of Monday.
    let (x0, x1, _, dw) = progress(&afternoon_editor(1, 25));
    assert_eq!(x1 - x0, dw * 0.5);
}
