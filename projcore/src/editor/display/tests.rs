use super::*;

/// Tasks `(uid, name, level, duration days, start (month, day))`; level 0 is
/// a blank row. Every task is manual with its own start, so dates are known.
type Row<'a> = (i32, &'a str, u32, i64, (u32, u32));

fn plan(rows: &[Row]) -> Editor {
    Editor::new(Project {
        start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
        tasks: rows
            .iter()
            .map(|&(uid, name, outline_level, days, (m, d))| Task {
                uid,
                id: uid,
                name: name.into(),
                outline_level,
                duration_min: days * 480,
                is_null: outline_level == 0,
                manual: true,
                manual_start: Some(DateTime::from_ymd_hm(2026, m, d, 8, 0)),
                ..Task::default()
            })
            .collect(),
        ..Project::default()
    })
}

/// A, S { Cc, bb, T { Dd } }, blank, B.
fn sample() -> Editor {
    plan(&[
        (1, "A", 1, 3, (3, 9)),
        (2, "S", 1, 1, (2, 2)),
        (3, "Cc", 2, 5, (2, 20)),
        (4, "bb", 2, 2, (3, 2)),
        (5, "T", 2, 1, (1, 6)),
        (6, "Dd", 3, 4, (4, 1)),
        (7, "", 0, 0, (1, 1)),
        (8, "B", 1, 3, (3, 16)),
    ])
}

fn names(ed: &Editor) -> Vec<String> {
    ed.display_rows()
        .iter()
        .map(|&i| ed.project().tasks[i].name.clone())
        .collect()
}

fn texts(items: &[ValueItem]) -> Vec<&str> {
    items.iter().map(|v| v.text.as_str()).collect()
}

fn only(texts: &[&str]) -> Option<BTreeSet<String>> {
    Some(texts.iter().map(|t| t.to_string()).collect())
}

#[test]
fn without_sort_or_filter_the_display_is_the_visible_rows() {
    let mut ed = sample();
    assert_eq!(ed.display_rows(), ed.visible_rows());
    ed.set_collapsed(2, true).unwrap();
    assert_eq!(ed.display_rows(), ed.visible_rows());
}

#[test]
fn sort_orders_siblings_and_keeps_each_subtree() {
    let mut ed = sample();
    let by = |descending| {
        Some(Sort {
            field: Field::Name,
            descending,
        })
    };
    let rev = ed.revision();
    ed.set_sort(by(false));
    assert!(ed.revision() > rev);
    // Text is case-insensitive; S keeps Cc, bb, T and T's child under it.
    assert_eq!(
        names(&ed),
        ["A", "B", "S", "bb", "Cc", "T", "Dd", ""],
        "ascending, blank last"
    );
    ed.set_sort(by(true));
    assert_eq!(names(&ed), ["S", "T", "Dd", "Cc", "bb", "B", "A", ""]);
    ed.set_sort(None);
    assert_eq!(ed.display_rows(), ed.visible_rows());
}

#[test]
fn sort_uses_the_columns_own_type() {
    let mut ed = sample();
    ed.set_sort(Some(Sort {
        field: Field::Duration,
        descending: false,
    }));
    // 1 day, 3 days, 3 days (stable: A before B), blank; text order would
    // put "3 days" after "1 day" and "10 days" before "2 days".
    let top: Vec<_> = names(&ed)
        .into_iter()
        .filter(|n| ["A", "B", "S"].contains(&n.as_str()))
        .collect();
    assert_eq!(top, ["S", "A", "B"]);
    assert_eq!(names(&ed).last().unwrap(), "");

    ed.set_sort(Some(Sort {
        field: Field::Start,
        descending: true,
    }));
    // Dates, latest first: B (3/16), A (3/9), then S; S's children by date.
    let top: Vec<_> = names(&ed)
        .into_iter()
        .filter(|n| ["A", "B", "S"].contains(&n.as_str()))
        .collect();
    assert_eq!(top, ["B", "A", "S"]);
}

#[test]
fn the_project_summary_row_stays_first() {
    let mut ed = plan(&[
        (0, "Plan", 0, 0, (1, 1)),
        (1, "B", 1, 1, (1, 6)),
        (2, "A", 1, 1, (1, 6)),
    ]);
    ed.set_show_summary_task(true);
    // Level 0 reads as a blank row here; make UID 0 the real summary.
    let mut proj = ed.project().clone();
    proj.tasks[0].is_null = false;
    proj.tasks[0].outline_level = 0;
    let mut ed = Editor::new(proj);
    ed.set_show_summary_task(true);
    ed.set_sort(Some(Sort {
        field: Field::Name,
        descending: true,
    }));
    assert_eq!(names(&ed), ["Plan", "B", "A"]);
    ed.set_sort(Some(Sort {
        field: Field::Name,
        descending: false,
    }));
    assert_eq!(names(&ed), ["Plan", "A", "B"]);
    ed.set_show_summary_task(false);
    assert_eq!(names(&ed), ["A", "B"], "hidden, its tasks still sort");
}

#[test]
fn filter_keeps_a_summary_with_a_passing_child_and_ands_columns() {
    let mut ed = sample();
    ed.set_filter(Field::Name, only(&["Dd"]));
    // Dd passes: T and S stay for it; A, B, bb, Cc go; the blank stays.
    assert_eq!(names(&ed), ["S", "T", "Dd", ""]);
    assert!(ed.is_filtered());
    assert_eq!(ed.filtered_fields(), [Field::Name]);

    // Collapse still wins: S collapsed shows S alone, though Dd passes.
    ed.set_collapsed(2, true).unwrap();
    assert_eq!(names(&ed), ["S", ""]);
    ed.set_collapsed(2, false).unwrap();

    // A second column ANDs: Dd is 4 days, nothing here is 1 day.
    ed.set_filter(Field::Duration, only(&["4 days"]));
    assert_eq!(names(&ed), ["S", "T", "Dd", ""]);
    ed.set_filter(Field::Duration, only(&["1 day"]));
    assert_eq!(names(&ed), [""]);
    ed.set_filter(Field::Duration, only(&["4 days"]));
    ed.set_filter(Field::Name, only(&["Dd", "A"]));
    assert_eq!(names(&ed), ["S", "T", "Dd", ""], "A is 3 days: it fails");
    ed.set_filter(Field::Name, only(&["A"]));
    assert_eq!(names(&ed), [""], "Dd fails the name, A the duration");
    ed.set_filter(Field::Name, None);
    assert_eq!(names(&ed), ["S", "T", "Dd", ""], "only Duration left");
    ed.clear_filters();
    assert!(!ed.is_filtered());
    assert_eq!(ed.display_rows(), ed.visible_rows());
}

#[test]
fn filtered_and_sorted_compose_and_step_in_display_order() {
    let mut ed = sample();
    ed.set_filter(Field::Name, only(&["A", "B", "bb"]));
    ed.set_sort(Some(Sort {
        field: Field::Name,
        descending: true,
    }));
    assert_eq!(names(&ed), ["S", "bb", "B", "A", ""]);
    let rows = ed.display_rows();
    assert_eq!(ed.visible_step(rows[0], 1), rows[1]);
    assert_eq!(ed.visible_step(rows[1], -1), rows[0]);
    assert_eq!(ed.visible_step(rows[0], -1), rows[0]);
    ed.select_first_visible();
    assert_eq!(ed.project().tasks[ed.sel()].name, "S");
    ed.select_last_visible();
    assert_eq!(ed.project().tasks[ed.sel()].name, "");
}

#[test]
fn a_selection_the_filter_hides_moves_to_the_first_displayed_row() {
    let mut ed = sample();
    ed.select(0);
    ed.set_filter(Field::Name, only(&["B"]));
    assert_eq!(ed.project().tasks[ed.sel()].name, "B");
}

#[test]
fn column_values_ignore_their_own_filter_but_not_others() {
    let mut ed = sample();
    let all = ed.column_values(Field::Name);
    assert_eq!(texts(&all), ["A", "B", "bb", "Cc", "Dd", "S", "T"]);
    ed.set_filter(Field::Name, only(&["A"]));
    assert_eq!(texts(&ed.column_values(Field::Name)), texts(&all));
    // Other columns see the Name filter: only A's 3 days remain.
    assert_eq!(texts(&ed.column_values(Field::Duration)), ["3 days"]);
}

#[test]
fn date_values_carry_year_month_day_for_the_tree() {
    let ed = sample();
    let starts = ed.column_values(Field::Start);
    let dates: Vec<_> = starts.iter().map(|v| v.date).collect();
    assert_eq!(dates.len(), 7);
    assert!(dates.windows(2).all(|w| w[0] <= w[1]), "earliest first");
    assert_eq!(dates[0], Some((2026, 1, 6)));
    assert_eq!(dates[6], Some((2026, 4, 1)));
    // A name column has no date.
    assert!(
        ed.column_values(Field::Name)
            .iter()
            .all(|v| v.date.is_none())
    );
}

#[test]
fn filtering_a_month_leaves_exactly_its_rows() {
    let mut ed = sample();
    let march: BTreeSet<String> = ed
        .column_values(Field::Start)
        .into_iter()
        .filter(|v| v.date.is_some_and(|(_, m, _)| m == 3))
        .map(|v| v.text)
        .collect();
    assert_eq!(march.len(), 3);
    ed.set_filter(Field::Start, Some(march));
    assert_eq!(names(&ed), ["A", "S", "bb", "", "B"]);
}

#[test]
fn position_dependent_edits_are_refused_while_sorted() {
    let mut ed = sample();
    ed.set_filter(Field::Name, only(&["A", "B"]));
    assert!(ed.add_task(Some(1), "N", 480, false).is_ok());
    ed.set_sort(Some(Sort {
        field: Field::Name,
        descending: false,
    }));
    let before = ed.project().tasks.len();
    let err = Err("Clear the sort first".to_string());
    assert_eq!(ed.indent(1, 1), err);
    assert_eq!(ed.add_task(Some(1), "M", 480, false).map(|_| ()), err);
    assert_eq!(ed.insert_blank_row(Some(1)).map(|_| ()), err);
    assert_eq!(
        ed.insert_tasks(Some(1), &[Task::default()], &[])
            .map(|_| ()),
        err
    );
    assert_eq!(ed.project().tasks.len(), before);
    // Appending is not position-dependent.
    assert!(ed.add_task(None, "Z", 480, false).is_ok());
    ed.set_sort(None);
    assert!(ed.indent(1, 1).is_ok());
}

#[test]
fn a_new_document_clears_sort_and_filter() {
    let mut ed = sample();
    ed.set_sort(Some(Sort {
        field: Field::Name,
        descending: false,
    }));
    ed.set_filter(Field::Name, only(&["A"]));
    ed.replace_project(sample().project().clone());
    assert!(ed.sort().is_none() && !ed.is_filtered());
}

#[test]
fn widths_are_one_undo_step_and_no_change_records_nothing() {
    let mut ed = sample();
    ed.edit_table_view(|v| v.set_width("Name", 300.));
    assert_eq!(ed.project().table_view.width("Name"), Some(300.));
    assert!(ed.dirty());
    ed.edit_table_view(|v| v.set_width("Name", 300.));
    assert!(ed.undo());
    assert_eq!(ed.project().table_view.width("Name"), None);
    assert!(!ed.undo(), "the repeated set recorded nothing");
    assert!(ed.redo());
    assert_eq!(ed.project().table_view.width("Name"), Some(300.));
}
