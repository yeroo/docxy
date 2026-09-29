//! Export a scheduled project as a Markdown Gantt chart.
//!
//! The chart is emitted as a [Mermaid `gantt`] block — the same diagram syntax
//! docxy already renders in Markdown — so the output drops straight into a
//! README, a PR description, or any Mermaid-aware viewer. Bars are driven by the
//! computed [`Schedule`]: each task's early start and duration, with critical
//! tasks flagged `crit`, milestones flagged `milestone`, and top-level summary
//! tasks becoming chart sections. The block excludes the project calendar's
//! non-working weekdays and holiday dates, and includes working exception
//! dates on otherwise excluded weekdays.
//! The accompanying table names each task as stored (line breaks flattened,
//! `\` and `|` escaped); only the chart strips `:,;#` from names. It
//! includes bold summary rows with rolled-up dates (a
//! manually scheduled summary's own dates) and working durations, each task's
//! Deadline (marked `⚠` when the row's Finish is after it), signed total slack,
//! and the scheduler's free slack. A task's Duration and slack show in the
//! unit its `DurationFormat` names (`1w`, `0.50w`, days by default); summaries
//! and elapsed or other non-working formats show whole days, hours or
//! minutes, slack in days.
//!
//! [Mermaid `gantt`]: https://mermaid.js.org/syntax/gantt.html

use crate::datetime::DateTime;
use crate::model::{Project, Task};
use crate::schedule::Schedule;

/// Render just the Mermaid `gantt` diagram body (no code fence).
pub fn to_mermaid(proj: &Project, sched: &Schedule) -> String {
    let mut out = String::new();
    out.push_str("gantt\n");
    let title = sanitize(&proj.title).or_else(|| sanitize(&proj.name));
    if let Some(t) = &title {
        out.push_str(&format!("    title {t}\n"));
    }
    out.push_str("    dateFormat YYYY-MM-DD\n");
    out.push_str("    axisFormat %m/%d\n");
    calendar_directives(proj, sched, &mut out);

    let mut section: Option<String> = None; // current section name (from summary)
    let mut emitted: Option<String> = None; // last section header written
    for task in &proj.tasks {
        // A blank row is not a task and never opens a section.
        if task.is_null {
            continue;
        }
        if task.summary {
            // Top-level summaries define sections; deeper ones just group under
            // the enclosing section.
            if task.outline_level <= 1 {
                section = sanitize(&task.name)
                    .or_else(|| summary_fallback(proj, task, sanitize))
                    .or(Some("Section".into()));
            }
            continue;
        }
        let Some(r) = sched.get(task.uid) else {
            continue;
        };
        let sec = section.clone().unwrap_or_else(|| "Tasks".into());
        if emitted.as_ref() != Some(&sec) {
            out.push_str(&format!("    section {sec}\n"));
            emitted = Some(sec);
        }

        let name = sanitize(&task.name).unwrap_or_else(|| format!("Task {}", task.uid));
        let p = r.early_start.parts();
        let date = format!("{:04}-{:02}-{:02}", p.year, p.month, p.day);
        let mut tags: Vec<&str> = Vec::new();
        if r.critical {
            tags.push("crit");
        }
        if task.is_milestone() {
            tags.push("milestone");
        }
        let tagstr = if tags.is_empty() {
            String::new()
        } else {
            format!("{}, ", tags.join(", "))
        };
        let duration_min =
            crate::schedule::summary_or_leaf_min(proj, task, r.early_start, r.early_finish);
        let dur = duration_str(proj, duration_min);
        out.push_str(&format!("    {name} :{tagstr}{date}, {dur}\n"));
    }
    out
}

/// Render a full Markdown document: a heading, the fenced Mermaid chart, and a
/// task table (start, finish, deadline, duration, total/free slack, critical) as a text
/// fallback for viewers that don't render Mermaid. Task names show as stored,
/// with line breaks flattened and `\` and `|` escaped. Summary names are bold and
/// their durations are the working time between their scheduled dates (rolled
/// up, or a manual summary's own), measured
/// as [`crate::schedule::task_duration_min`] measures them: on the project's
/// default calendar, or on its leaves' calendars when the default has no
/// working time.
pub fn to_markdown(proj: &Project, sched: &Schedule) -> String {
    let heading = sanitize(&proj.title)
        .or_else(|| sanitize(&proj.name))
        .unwrap_or_else(|| "Project schedule".into());
    let mut out = format!(
        "# {heading}\n\n```mermaid\n{}```\n\n",
        to_mermaid(proj, sched)
    );

    out.push_str(
        "| Task | Start | Finish | Deadline | Duration | Total slack | Free slack | Critical |\n",
    );
    out.push_str(
        "|------|-------|--------|----------|----------|-------------|------------|----------|\n",
    );
    for task in &proj.tasks {
        let Some(r) = sched.get(task.uid) else {
            continue;
        };
        let name = table_cell(&task.name)
            .or_else(|| summary_fallback(proj, task, table_cell))
            .unwrap_or_else(|| format!("Task {}", task.uid));
        let name = if task.summary {
            format!("**{name}**")
        } else {
            name
        };
        let duration_min =
            crate::schedule::summary_or_leaf_min(proj, task, r.early_start, r.early_finish);
        // A leaf shows its Duration and slack in the unit it was entered in
        // (a zero Duration stays `0d`); a summary keeps the default.
        let unit = task.duration_unit().filter(|_| !task.summary);
        let in_unit = |min| unit.and_then(|unit| proj.format_in_unit(min, unit, 2));
        let dur = in_unit(duration_min)
            .filter(|_| duration_min > 0)
            .unwrap_or_else(|| duration_str(proj, duration_min));
        let slack = in_unit(r.total_slack_min)
            .unwrap_or_else(|| fmt_days(proj.minutes_to_days(r.total_slack_min)));
        let free_slack = in_unit(r.free_slack_min)
            .unwrap_or_else(|| fmt_days(proj.minutes_to_days(r.free_slack_min)));
        // Judged on this row's own (CPM) Finish cell; yppxy's grid judges
        // its displayed, possibly leveled, finish instead.
        let deadline = task.deadline.map_or_else(String::new, |deadline| {
            let date = cell_date(deadline);
            if task.misses_deadline(r.early_finish) {
                format!("{date} ⚠")
            } else {
                date
            }
        });
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            name,
            cell_date(r.early_start),
            cell_date(r.early_finish),
            deadline,
            dur,
            slack,
            free_slack,
            if r.critical { "✓" } else { "" },
        ));
    }
    out
}

/// A table cell's date and time, `YYYY-MM-DD HH:MM:SS`.
fn cell_date(date: DateTime) -> String {
    date.to_mspdi().replace('T', " ")
}

/// A Mermaid duration token (`2d`, `4h`, `30m`) from working minutes.
fn duration_str(proj: &Project, min: i64) -> String {
    if min <= 0 {
        return "0d".into();
    }
    let days = proj.minutes_to_days(min);
    if (days.round() - days).abs() < 1e-9 {
        format!("{}d", days.round() as i64)
    } else if min % 60 == 0 {
        format!("{}h", min / 60)
    } else {
        format!("{min}m")
    }
}

fn fmt_days(days: f64) -> String {
    if (days.round() - days).abs() < 1e-9 {
        format!("{}d", days.round() as i64)
    } else {
        format!("{days:.2}d")
    }
}

/// Write weekly exclusions and dated overrides for the emitted bars' span.
fn calendar_directives(proj: &Project, sched: &Schedule, out: &mut String) {
    let Some(cal) = proj.project_shading_calendar() else {
        return;
    };
    let week = cal.week();
    let works = |weekday: usize| week[weekday].times.iter().any(|t| t.from < t.to);
    let weekends = !works(0) && !works(6);
    let mut excludes: Vec<String> = Vec::new();
    if weekends {
        excludes.push("weekends".into());
    }
    const DAYS: [&str; 7] = [
        "sunday",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
    ];
    for (weekday, name) in DAYS.into_iter().enumerate() {
        if !(works(weekday) || weekends && matches!(weekday, 0 | 6)) {
            excludes.push(name.into());
        }
    }
    let span = proj
        .tasks
        .iter()
        .filter(|t| !t.is_null && !t.summary)
        .filter_map(|t| sched.get(t.uid))
        .fold(None, |bounds: Option<(i64, i64)>, r| {
            let first = r.early_start.day_number();
            let last = r.early_finish.day_number();
            Some(bounds.map_or((first, last), |(a, b)| (a.min(first), b.max(last))))
        });
    let mut includes = Vec::new();
    if let Some((first, last)) = span {
        for day in first..=last {
            let dt = DateTime::from_minutes(day * 1440);
            let weekly_off = !works(dt.weekday() as usize);
            let actual_off = cal.day(day).is_empty();
            if weekly_off == actual_off {
                continue;
            }
            let p = dt.parts();
            let date = format!("{:04}-{:02}-{:02}", p.year, p.month, p.day);
            if actual_off {
                excludes.push(date);
            } else {
                includes.push(date);
            }
        }
    }
    if !excludes.is_empty() {
        out.push_str(&format!("    excludes {}\n", excludes.join(", ")));
    }
    if !includes.is_empty() {
        out.push_str(&format!("    includes {}\n", includes.join(", ")));
    }
}

/// Strip characters that would break a Mermaid task line (`:,;#` and newlines),
/// collapse whitespace, and trim. Returns `None` for an empty result.
fn sanitize(s: &str) -> Option<String> {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if matches!(c, ':' | ',' | ';' | '#' | '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let out = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    (!out.is_empty()).then_some(out)
}

/// A name as a Markdown table cell: line breaks and tabs flattened to one
/// space each, ends trimmed, then `\` and `|` backslash-escaped so the row
/// keeps its cells. Everything else is kept as stored. Returns `None` for an
/// empty result.
fn table_cell(s: &str) -> Option<String> {
    let flat = s.replace("\r\n", " ").replace(['\r', '\n', '\t'], " ");
    let out = flat.trim().replace('\\', "\\\\").replace('|', "\\|");
    (!out.is_empty()).then_some(out)
}

/// A pathless project can still name its UID 0 row from project metadata,
/// formatted by `fmt` for where it is written.
fn summary_fallback(
    proj: &Project,
    task: &Task,
    fmt: fn(&str) -> Option<String>,
) -> Option<String> {
    (task.is_project_summary() && task.name.trim().is_empty())
        .then(|| fmt(&proj.title).or_else(|| fmt(&proj.name)))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datetime::DateTime;
    use crate::model::*;
    use crate::schedule::schedule;

    fn task(uid: i32, name: &str, dur: i64) -> Task {
        Task {
            uid,
            id: uid,
            name: name.into(),
            outline_level: 1,
            duration_min: dur,
            ..Task::default()
        }
    }

    fn diamond() -> Project {
        let mut b = task(2, "B", 1440);
        b.predecessors = vec![Predecessor::fs(1)];
        let mut c = task(3, "C", 480);
        c.predecessors = vec![Predecessor::fs(1)];
        let mut d = task(4, "D", 960);
        d.predecessors = vec![Predecessor::fs(2), Predecessor::fs(3)];
        Project {
            name: "Demo".into(),
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![task(1, "A", 960), b, c, d],
            ..Project::default()
        }
    }

    #[test]
    fn blank_rows_are_not_rendered() {
        let mut proj = diamond();
        let mut blank = task(9, "Ghost", 480);
        blank.is_null = true;
        blank.summary = true;
        blank.outline_level = 0;
        proj.tasks.insert(2, blank);
        let s = schedule(&proj);
        let plain = diamond();
        let plain_s = schedule(&plain);
        assert_eq!(to_mermaid(&proj, &s), to_mermaid(&plain, &plain_s));
        assert_eq!(to_markdown(&proj, &s), to_markdown(&plain, &plain_s));
        assert!(!to_markdown(&proj, &s).contains("Ghost"));
    }

    #[test]
    fn mermaid_shapes() {
        let proj = diamond();
        let s = schedule(&proj);
        let m = to_mermaid(&proj, &s);
        assert!(m.starts_with("gantt\n"));
        assert!(m.contains("title Demo"));
        assert!(m.contains("dateFormat YYYY-MM-DD"));
        assert!(m.contains("excludes weekends"));
        assert!(m.contains("section Tasks"));
        // A is critical and starts on the anchor Monday for 2 days.
        assert!(m.contains("A :crit, 2026-03-02, 2d"), "got:\n{m}");
        // C is not critical.
        assert!(m.contains("C :2026-03-04, 1d"), "got:\n{m}");
    }

    #[test]
    fn milestone_and_sections() {
        let mut sum = task(1, "Phase 1", 0);
        sum.summary = true;
        let mut a = task(2, "A", 480);
        a.outline_level = 2;
        let mut ms = task(3, "Sign-off", 0);
        ms.outline_level = 2;
        ms.milestone = true;
        ms.predecessors = vec![Predecessor::fs(2)];
        let proj = Project {
            name: "P".into(),
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![sum, a, ms],
            ..Project::default()
        };
        let s = schedule(&proj);
        let m = to_mermaid(&proj, &s);
        assert!(m.contains("section Phase 1"));
        assert!(m.contains("Sign-off :"));
        assert!(m.contains("milestone"));
        assert!(m.contains(", 0d"));
    }

    #[test]
    fn unnamed_project_summary_uses_metadata_in_table_and_section() {
        for (title, project_name, expected_row, expected_section) in [
            (
                "Warehouse fit-out",
                "Other",
                "Warehouse fit-out",
                "Warehouse fit-out",
            ),
            ("", "site-plan", "site-plan", "site-plan"),
            ("", "", "Task 0", "Section"),
        ] {
            let mut summary = task(0, "", 0);
            summary.summary = true;
            summary.outline_level = 0;
            let proj = Project {
                title: title.into(),
                name: project_name.into(),
                start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
                tasks: vec![summary, task(5, "", 480)],
                ..Project::default()
            };
            let sched = schedule(&proj);
            let md = to_markdown(&proj, &sched);
            assert!(md.contains(&format!("| **{expected_row}** |")), "{md}");
            assert!(md.contains("| Task 5 |"), "{md}");
            assert!(
                to_mermaid(&proj, &sched).contains(&format!("    section {expected_section}\n"))
            );
        }
    }

    #[test]
    fn unnamed_non_project_summary_keeps_section_fallback() {
        let mut summary = task(1, "", 0);
        summary.summary = true;
        let mut leaf = task(2, "A", 480);
        leaf.outline_level = 2;
        let proj = Project {
            title: "Warehouse fit-out".into(),
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![summary, leaf],
            ..Project::default()
        };
        assert!(to_mermaid(&proj, &schedule(&proj)).contains("    section Section\n"));
    }

    #[test]
    fn unnamed_non_summary_uid_zero_keeps_task_fallback() {
        let proj = Project {
            title: "Warehouse fit-out".into(),
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![task(0, "", 480)],
            ..Project::default()
        };
        let sched = schedule(&proj);
        assert!(to_markdown(&proj, &sched).contains("| Task 0 |"));
        assert!(to_mermaid(&proj, &sched).contains("    Task 0 :"));
    }

    #[test]
    fn markdown_wraps_chart_and_table() {
        let proj = diamond();
        let s = schedule(&proj);
        let md = to_markdown(&proj, &s);
        assert!(md.starts_with("# Demo\n"));
        assert!(md.contains("```mermaid\ngantt"));
        assert!(md.contains("| Task | Start | Finish |"));
        assert!(md.contains("2026-03-02 08:00:00"));
    }

    fn table_rows(md: &str) -> Vec<Vec<&str>> {
        md.lines()
            .filter(|line| line.starts_with("| "))
            .map(|line| {
                let boundaries: Vec<_> = line
                    .match_indices('|')
                    .filter(|(index, _)| {
                        line[..*index]
                            .chars()
                            .rev()
                            .take_while(|&c| c == '\\')
                            .count()
                            % 2
                            == 0
                    })
                    .map(|(index, _)| index)
                    .collect();
                boundaries
                    .windows(2)
                    .map(|pair| line[pair[0] + 1..pair[1]].trim())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn markdown_includes_summary_rows() {
        let mut parent = task(1, "P", 1); // Deliberately stale stored duration.
        parent.summary = true;
        let mut a = task(2, "A", 1440);
        a.outline_level = 2;
        let mut nested = task(3, "Nested", 1);
        nested.summary = true;
        nested.outline_level = 2;
        let mut b = task(4, "B", 480);
        b.outline_level = 3;
        b.predecessors.push(Predecessor::fs(2));
        let mut empty = task(5, "Empty", 1);
        empty.summary = true;
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 5, 8, 0)),
            tasks: vec![parent, a, nested, b, empty],
            ..Project::default()
        };
        let md = to_markdown(&proj, &schedule(&proj));
        assert_eq!(
            &table_rows(&md)[1..],
            [
                [
                    "**P**",
                    "2026-03-05 08:00:00",
                    "2026-03-10 17:00:00",
                    "",
                    "4d",
                    "0d",
                    "0d",
                    "✓"
                ],
                [
                    "A",
                    "2026-03-05 08:00:00",
                    "2026-03-09 17:00:00",
                    "",
                    "3d",
                    "0d",
                    "0d",
                    "✓"
                ],
                [
                    "**Nested**",
                    "2026-03-10 08:00:00",
                    "2026-03-10 17:00:00",
                    "",
                    "1d",
                    "0d",
                    "0d",
                    "✓"
                ],
                [
                    "B",
                    "2026-03-10 08:00:00",
                    "2026-03-10 17:00:00",
                    "",
                    "1d",
                    "0d",
                    "0d",
                    "✓"
                ],
            ]
        );
    }

    #[test]
    fn markdown_shows_duration_and_slack_in_each_task_s_unit() {
        let proj = crate::mspdi::read_mspdi(crate::mspdi::DURATION_FORMATS_PLAN).unwrap();
        let sched = schedule(&proj);
        assert_eq!(sched.get(5).unwrap().total_slack_min, 1200);
        let md = to_markdown(&proj, &sched);
        let row = |name: &str| {
            table_rows(&md)
                .into_iter()
                .find(|row| row[0] == name)
                .unwrap()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        for (name, start, finish, cells) in [
            (
                "S",
                "2026-03-02 08:00:00",
                "2026-03-02 17:00:00",
                ["1d", "0d", "0d", "✓"],
            ),
            (
                "Half",
                "2026-03-03 08:00:00",
                "2026-03-03 12:00:00",
                ["4h", "4h", "4h", ""],
            ),
            (
                "WkShort",
                "2026-03-05 08:00:00",
                "2026-03-11 17:00:00",
                ["1w", "0.50w", "0.50w", ""],
            ),
            (
                "WkLong",
                "2026-03-05 08:00:00",
                "2026-03-16 12:00:00",
                ["1.50w", "0w", "0w", "✓"],
            ),
        ] {
            let mut expected = vec![name, start, finish, ""];
            expected.extend(cells);
            assert_eq!(row(name), expected, "{name}");
        }
        // The Mermaid bars keep their day/hour tokens.
        assert!(md.contains("WkLong :crit, 2026-03-05, 60h\n"), "{md}");
    }

    #[test]
    fn markdown_keeps_summaries_and_zero_durations_out_of_the_unit() {
        let mut parent = task(1, "P", 1);
        parent.summary = true;
        parent.duration_format = Some(9);
        let mut a = task(2, "A", 240);
        a.outline_level = 2;
        a.duration_format = Some(9);
        let mut m = task(3, "M", 0);
        m.duration_format = Some(9);
        m.deadline = Some(DateTime::from_ymd_hm(2026, 3, 2, 12, 0));
        // Estimated weeks: the estimated bit does not change the unit.
        let mut late = task(4, "Late", 2400);
        late.duration_format = Some(41);
        late.deadline = Some(DateTime::from_ymd_hm(2026, 3, 4, 12, 0));
        // An elapsed format keeps the default display.
        let mut elapsed = task(5, "Elapsed", 240);
        elapsed.duration_format = Some(8);
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![parent, a, m, late, elapsed],
            ..Project::default()
        };
        let sched = schedule(&proj);
        assert_eq!(sched.get(4).unwrap().total_slack_min, -1200);
        let md = to_markdown(&proj, &sched);
        let cells = |row: &Vec<&str>| row[4..7].join(" | ");
        let rows = table_rows(&md);
        assert_eq!(cells(&rows[1]), "4h | 4.50d | 4.50d", "summary");
        assert_eq!(cells(&rows[2]), "0.10w | 0.90w | 0.90w", "leaf");
        assert_eq!(cells(&rows[3]), "0d | 0.10w | 0.10w", "milestone");
        assert_eq!(cells(&rows[4]), "1w | -0.50w | 0w", "negative");
        assert_eq!(cells(&rows[5]), "4h | 4.50d | 4.50d", "elapsed");
    }

    #[test]
    fn markdown_shows_a_manual_summary_s_own_dates() {
        let mut parent = task(1, "P", 1);
        parent.summary = true;
        parent.manual = true;
        parent.manual_start = Some(DateTime::from_ymd_hm(2026, 3, 5, 8, 0));
        parent.manual_finish = Some(DateTime::from_ymd_hm(2026, 3, 6, 17, 0));
        let mut a = task(2, "A", 1440);
        a.outline_level = 2;
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 5, 8, 0)),
            tasks: vec![parent, a],
            ..Project::default()
        };
        let md = to_markdown(&proj, &schedule(&proj));
        assert_eq!(
            table_rows(&md)[1],
            [
                "**P**",
                "2026-03-05 08:00:00",
                "2026-03-06 17:00:00",
                "",
                "2d",
                "0d",
                "0d",
                "✓"
            ]
        );
    }

    #[test]
    fn markdown_reports_negative_total_slack() {
        for (finish, minutes, expected) in [
            (DateTime::from_ymd_hm(2026, 2, 26, 17, 0), -480, "-1d"),
            (DateTime::from_ymd_hm(2026, 2, 27, 12, 0), -240, "-0.50d"),
        ] {
            // Keep link precedence to test integer and fractional negative
            // slack formatting against the original SF dates.
            let mut proj = crate::mspdi::read_mspdi(include_str!(
                "../../corpus/mspdi/14-link-sf-before-start.xml"
            ))
            .unwrap();
            proj.honor_constraints = false;
            proj.tasks[0].name = "Late".into();
            proj.tasks[0].constraint = ConstraintType::FinishNoLaterThan;
            proj.tasks[0].constraint_date = Some(finish);
            let sched = schedule(&proj);
            assert_eq!(sched.get(1).unwrap().total_slack_min, minutes);
            let md = to_markdown(&proj, &sched);
            assert_eq!(
                table_rows(&md)[1],
                [
                    "Late",
                    "2026-02-26 08:00:00",
                    "2026-03-02 08:00:00",
                    "",
                    "2d",
                    expected,
                    "0d",
                    "✓"
                ]
            );
        }
    }

    #[test]
    fn markdown_marks_a_missed_deadline_and_reports_negative_total_slack() {
        let proj =
            crate::mspdi::read_mspdi(include_str!("../../corpus/mspdi/21-deadline-missed.xml"))
                .unwrap();
        let md = to_markdown(&proj, &schedule(&proj));
        let rows = table_rows(&md);
        assert_eq!(
            rows[1..],
            [
                [
                    "A",
                    "2026-03-02 08:00:00",
                    "2026-03-06 17:00:00",
                    "",
                    "5d",
                    "-5d",
                    "0d",
                    "✓"
                ],
                [
                    "B",
                    "2026-03-09 08:00:00",
                    "2026-03-13 17:00:00",
                    "2026-03-06 17:00:00 ⚠",
                    "5d",
                    "-5d",
                    "0d",
                    "✓"
                ]
            ]
        );
    }

    #[test]
    fn markdown_shows_a_met_deadline_without_the_marker() {
        let proj = crate::mspdi::read_mspdi(include_str!("../../corpus/mspdi/20-task-fields.xml"))
            .unwrap();
        let md = to_markdown(&proj, &schedule(&proj));
        let pour = table_rows(&md)
            .into_iter()
            .find(|row| row[0] == "Pour")
            .unwrap();
        assert_eq!(pour[2..4], ["2026-03-04 17:00:00", "2026-03-20 17:00:00"]);
    }

    #[test]
    fn markdown_reports_free_slack() {
        let proj = diamond();
        let md = to_markdown(&proj, &schedule(&proj));
        let rows = table_rows(&md);
        assert_eq!(
            rows[0],
            [
                "Task",
                "Start",
                "Finish",
                "Deadline",
                "Duration",
                "Total slack",
                "Free slack",
                "Critical"
            ]
        );
        assert_eq!(
            rows[1..]
                .iter()
                .map(|row| (row[0], row[6]))
                .collect::<Vec<_>>(),
            [("A", "0d"), ("B", "0d"), ("C", "2d"), ("D", "0d")]
        );
    }

    #[test]
    fn markdown_distinguishes_free_slack_from_total_slack() {
        // A -> B -> D and C -> D: A has float, but no room before B must move.
        let mut proj = diamond();
        proj.tasks[0].duration_min = 480;
        proj.tasks[1].duration_min = 480;
        proj.tasks[2].duration_min = 1920;
        proj.tasks[2].predecessors.clear();
        let sched = schedule(&proj);
        let a = sched.get(1).unwrap();
        assert_eq!((a.total_slack_min, a.free_slack_min), (960, 0));
        let md = to_markdown(&proj, &sched);
        assert_eq!(
            table_rows(&md)[1],
            [
                "A",
                "2026-03-02 08:00:00",
                "2026-03-02 17:00:00",
                "",
                "1d",
                "2d",
                "0d",
                ""
            ]
        );
    }

    #[test]
    fn markdown_escapes_pipes_in_leaf_and_summary_names() {
        let mut parent = task(1, "P | Q", 0);
        parent.summary = true;
        let mut leaf = task(2, "A | B", 480);
        leaf.outline_level = 2;
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![parent, leaf],
            ..Project::default()
        };
        let sched = schedule(&proj);
        let md = to_markdown(&proj, &sched);
        assert_eq!(
            &table_rows(&md)[1..],
            [
                [
                    r"**P \| Q**",
                    "2026-03-02 08:00:00",
                    "2026-03-02 17:00:00",
                    "",
                    "1d",
                    "0d",
                    "0d",
                    "✓"
                ],
                [
                    r"A \| B",
                    "2026-03-02 08:00:00",
                    "2026-03-02 17:00:00",
                    "",
                    "1d",
                    "0d",
                    "0d",
                    "✓"
                ],
            ]
        );
        // Escaping applies only to table cells, not Mermaid labels.
        let mermaid = to_mermaid(&proj, &sched);
        assert!(mermaid.contains("    section P | Q\n"));
        assert!(mermaid.contains("    A | B :crit, 2026-03-02, 1d\n"));
    }

    #[test]
    fn markdown_measures_summaries_on_leaf_calendars_when_default_has_none() {
        let mut phase = task(1, "Phase", 0);
        phase.summary = true;
        let mut a = task(2, "A", 480);
        let mut b = task(3, "B", 480);
        b.predecessors = vec![Predecessor::fs(2)];
        for leaf in [&mut a, &mut b] {
            leaf.outline_level = 2;
            leaf.calendar_uid = Some(3);
        }
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![phase, a, b],
            calendars: vec![
                Calendar::base(1, "Closed", Default::default()),
                Calendar::standard(3),
            ],
            ..Project::default()
        };
        let md = to_markdown(&proj, &schedule(&proj));
        assert_eq!(
            table_rows(&md)[1][..5],
            [
                "**Phase**",
                "2026-03-02 08:00:00",
                "2026-03-03 17:00:00",
                "",
                "2d"
            ]
        );
    }

    #[test]
    fn names_with_special_chars_are_sanitized() {
        let mut a = task(1, "Design: phase, one", 480);
        a.id = 1;
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![a],
            ..Project::default()
        };
        let s = schedule(&proj);
        let m = to_mermaid(&proj, &s);
        // colon/comma removed from the name so the task line stays parseable.
        assert!(m.contains("Design phase one :"), "got:\n{m}");
    }

    /// A parsed cell after one GFM-style unescape pass (each `\` escapes the
    /// next character), i.e. the text a GFM renderer shows for it.
    fn unescape(cell: &str) -> String {
        let mut out = String::new();
        let mut chars = cell.chars();
        while let Some(c) = chars.next() {
            out.push(if c == '\\' {
                chars.next().unwrap_or(c)
            } else {
                c
            });
        }
        out
    }

    /// The Markdown and Mermaid output of a project with one 1d task per name.
    fn name_rows(names: &[&str]) -> (String, String) {
        let tasks = names
            .iter()
            .zip(1..)
            .map(|(name, uid)| task(uid, name, 480))
            .collect();
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks,
            ..Project::default()
        };
        let sched = schedule(&proj);
        (to_markdown(&proj, &sched), to_mermaid(&proj, &sched))
    }

    #[test]
    fn table_keeps_punctuation_in_task_names() {
        let names = [
            "Dig foundation, east wing: phase 2; lot #7",
            "Coordinate the structural, mechanical and electrical inspections for the east wing",
        ];
        let (md, mermaid) = name_rows(&names);
        let rows = table_rows(&md);
        assert_eq!(rows.len(), 3, "{md}");
        for (row, name) in rows[1..].iter().zip(names) {
            assert_eq!(row[0], name, "{md}");
        }
        assert!(
            mermaid.contains("    Dig foundation east wing phase 2 lot 7 :"),
            "{mermaid}"
        );
        assert!(md.contains("    Dig foundation east wing phase 2 lot 7 :"));
    }

    #[test]
    fn table_escapes_pipes_backslashes_and_flattens_line_breaks() {
        let names = ["in\\|out", "a | b", "dir\\", "C:\\temp"];
        let (md, _) = name_rows(&names);
        let rows = table_rows(&md);
        assert_eq!(rows.len(), 1 + names.len(), "{md}");
        for (row, name) in rows[1..].iter().zip(names) {
            assert_eq!(row.len(), 8, "{md}");
            assert_eq!(unescape(row[0]), name, "{md}");
        }
        assert!(md.contains("| a \\| b |"), "{md}");
        assert!(md.contains("| dir\\\\ |"), "{md}");

        let (md, _) = name_rows(&["one\r\ntwo", "one\ntwo", "one\rtwo", "one\ttwo"]);
        let rows = table_rows(&md);
        assert_eq!(rows.len(), 5, "{md}");
        for row in &rows[1..] {
            assert_eq!(row.len(), 8, "{md}");
            assert_eq!(row[0], "one two", "{md}");
        }

        let mut summary = task(1, "A | b, c", 0);
        summary.summary = true;
        let mut leaf = task(2, "Leaf", 480);
        leaf.outline_level = 2;
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![summary, leaf],
            ..Project::default()
        };
        let md = to_markdown(&proj, &schedule(&proj));
        let rows = table_rows(&md);
        assert_eq!(rows[1].len(), 8, "{md}");
        assert_eq!(rows[1][0], "**A \\| b, c**", "{md}");
    }

    #[test]
    fn table_break_only_names_fall_back() {
        let (md, _) = name_rows(&["\r\n", "\t", ""]);
        let cells: Vec<_> = table_rows(&md)[1..].iter().map(|row| row[0]).collect();
        assert_eq!(cells, ["Task 1", "Task 2", "Task 3"], "{md}");
    }

    #[test]
    fn weekend_check_resolves_a_derived_project_calendar() {
        // The base works Saturdays; the project calendar derives from it.
        let mut six_day = Calendar::standard(3);
        six_day.week[6] = six_day.week[1].clone();
        let derived = |own_saturday: Option<crate::model::DayWorking>| Calendar {
            base_calendar_uid: Some(3),
            week: [None, None, None, None, None, None, own_saturday],
            ..Calendar::standard(1)
        };
        let mut proj = Project {
            calendars: vec![six_day, derived(None)],
            ..Project::default()
        };
        assert!(proj.project_calendar().week()[6].working());
        proj.calendars[1] = derived(Some(crate::model::DayWorking::default()));
        assert!(to_mermaid(&proj, &schedule(&proj)).contains("excludes weekends"));
    }

    #[test]
    fn weekend_check_uses_standard_when_the_default_calendar_is_missing() {
        // The scheduler synthesizes Standard for a missing default; the only
        // calendar present works every day and must not be consulted.
        let mut every_day = Calendar::standard(3);
        every_day.week[0] = every_day.week[1].clone();
        every_day.week[6] = every_day.week[1].clone();
        let a = task(1, "A", 480);
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![a],
            calendars: vec![every_day],
            ..Project::default()
        };
        assert!(!proj.project_calendar().week()[6].working());
        assert!(to_mermaid(&proj, &schedule(&proj)).contains("excludes weekends"));
    }

    #[test]
    fn mermaid_exports_holidays_and_working_weekends_inside_task_span() {
        let mut cal = Calendar::standard(1);
        let holiday = DateTime::from_ymd_hm(2026, 3, 4, 0, 0);
        let saturday = DateTime::from_ymd_hm(2026, 3, 7, 0, 0);
        let later_holiday = DateTime::from_ymd_hm(2026, 3, 11, 0, 0);
        cal.exceptions.push(CalendarException::date_range(
            holiday,
            holiday,
            DayWorking::default(),
        ));
        cal.exceptions.push(CalendarException::date_range(
            saturday,
            saturday,
            cal.week[1].clone().unwrap(),
        ));
        cal.exceptions.push(CalendarException::date_range(
            later_holiday,
            later_holiday,
            DayWorking::default(),
        ));
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![task(1, "Work", 6 * 480)],
            calendars: vec![cal],
            ..Project::default()
        };
        let m = to_mermaid(&proj, &schedule(&proj));
        assert!(m.contains("excludes weekends, 2026-03-04"), "{m}");
        assert!(m.contains("includes 2026-03-07"), "{m}");
        assert!(!m.contains("2026-03-11"), "outside the bar span: {m}");
    }

    #[test]
    fn mermaid_exception_span_extends_through_later_linked_bar() {
        let holiday = DateTime::from_ymd_hm(2026, 3, 6, 0, 0);
        let mut cal = Calendar::standard(1);
        cal.exceptions.push(CalendarException::date_range(
            holiday,
            holiday,
            DayWorking::default(),
        ));
        let mut second = task(2, "Second", 4 * 480);
        second.predecessors.push(Predecessor::fs(1));
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![task(1, "First", 2 * 480), second],
            calendars: vec![cal],
            ..Project::default()
        };
        let sched = schedule(&proj);
        assert!(sched.get(1).unwrap().early_finish < holiday);
        assert!(sched.get(2).unwrap().early_start < holiday);
        assert!(sched.get(2).unwrap().early_finish > holiday);
        let m = to_mermaid(&proj, &sched);
        assert!(m.contains("excludes weekends, 2026-03-06"), "{m}");
    }

    #[test]
    fn mermaid_names_nonstandard_weekdays_and_omits_empty_directives() {
        let mut cal = Calendar::standard(1);
        cal.week[0] = cal.week[1].clone();
        cal.week[5] = Some(DayWorking::default());
        let mut proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![task(1, "Work", 480)],
            calendars: vec![cal],
            ..Project::default()
        };
        let m = to_mermaid(&proj, &schedule(&proj));
        assert!(m.contains("excludes friday, saturday"), "{m}");
        proj.calendars[0].week[5] = proj.calendars[0].week[1].clone();
        proj.calendars[0].week[6] = proj.calendars[0].week[1].clone();
        let m = to_mermaid(&proj, &schedule(&proj));
        assert!(!m.contains("excludes ") && !m.contains("includes "), "{m}");
    }

    #[test]
    fn mermaid_resolves_base_holiday_and_ignores_unscheduled_recurrence() {
        let holiday = DateTime::from_ymd_hm(2026, 3, 4, 0, 0);
        let mut base = Calendar::standard(3);
        base.exceptions.push(CalendarException::date_range(
            holiday,
            holiday,
            DayWorking::default(),
        ));
        let recurring = CalendarException {
            kind: Some(2),
            from: Some(DateTime::from_ymd_hm(2026, 3, 5, 0, 0)),
            to: Some(DateTime::from_ymd_hm(2026, 3, 5, 0, 0)),
            day: DayWorking::default(),
            ..CalendarException::default()
        };
        base.exceptions.push(recurring);
        let derived = Calendar {
            base_calendar_uid: Some(3),
            week: std::array::from_fn(|_| None),
            ..Calendar::standard(1)
        };
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![task(1, "Work", 5 * 480)],
            calendars: vec![base, derived],
            ..Project::default()
        };
        let m = to_mermaid(&proj, &schedule(&proj));
        let excludes = m
            .lines()
            .find(|line| line.trim_start().starts_with("excludes "))
            .unwrap();
        assert!(excludes.contains("2026-03-04"), "{m}");
        assert!(!excludes.contains("2026-03-05"), "{m}");

        let empty = Project {
            tasks: Vec::new(),
            ..proj
        };
        let m = to_mermaid(&empty, &schedule(&empty));
        assert_eq!(
            m.lines()
                .find(|line| line.trim_start().starts_with("excludes ")),
            Some("    excludes weekends")
        );
    }

    #[test]
    fn closed_project_week_does_not_exclude_every_mermaid_day() {
        let closed = Calendar::base(1, "Closed", std::array::from_fn(|_| DayWorking::default()));
        let mut leaf = task(1, "Leaf", 480);
        leaf.calendar_uid = Some(3);
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            calendars: vec![closed, Calendar::standard(3)],
            tasks: vec![leaf],
            ..Project::default()
        };
        assert!(proj.project_shading_calendar().is_none());
        let m = to_mermaid(&proj, &schedule(&proj));
        assert!(m.contains("Leaf :"), "{m}");
        assert!(!m.contains("excludes ") && !m.contains("includes "), "{m}");
    }

    #[test]
    fn zero_length_weekly_slots_do_not_create_project_exclusions() {
        let zero = DayWorking {
            times: vec![WorkingTime {
                from: 8 * 60,
                to: 8 * 60,
            }],
        };
        let closed = Calendar::base(1, "Zero", std::array::from_fn(|_| zero.clone()));
        let mut leaf = task(1, "Leaf", 480);
        leaf.calendar_uid = Some(3);
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            calendars: vec![closed, Calendar::standard(3)],
            tasks: vec![leaf],
            ..Project::default()
        };
        assert!(proj.project_shading_calendar().is_none());
        let m = to_mermaid(&proj, &schedule(&proj));
        assert!(m.contains("Leaf :"), "{m}");
        assert!(!m.contains("excludes ") && !m.contains("includes "), "{m}");
    }
}
