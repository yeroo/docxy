//! Export a scheduled project as a Markdown Gantt chart.
//!
//! The chart is emitted as a [Mermaid `gantt`] block — the same diagram syntax
//! docxy already renders in Markdown — so the output drops straight into a
//! README, a PR description, or any Mermaid-aware viewer. Bars are driven by the
//! computed [`Schedule`]: each task's early start and duration, with critical
//! tasks flagged `crit`, milestones flagged `milestone`, and top-level summary
//! tasks becoming chart sections. For a standard Mon–Fri calendar the block
//! carries `excludes weekends` so the bars skip non-working days the way the
//! schedule does.
//! The accompanying table includes bold summary rows with rolled-up dates and
//! working durations, signed total slack, and the scheduler's free slack.
//!
//! [Mermaid `gantt`]: https://mermaid.js.org/syntax/gantt.html

use crate::model::Project;
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
    if excludes_weekends(proj) {
        out.push_str("    excludes weekends\n");
    }

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
                section = sanitize(&task.name).or(Some("Section".into()));
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
        let dur = duration_str(proj, task.duration_min);
        out.push_str(&format!("    {name} :{tagstr}{date}, {dur}\n"));
    }
    out
}

/// Render a full Markdown document: a heading, the fenced Mermaid chart, and a
/// task table (start, finish, duration, total/free slack, critical) as a text
/// fallback for viewers that don't render Mermaid. Summary names are bold and
/// their durations are the working time between the rolled-up dates, measured
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

    out.push_str("| Task | Start | Finish | Duration | Total slack | Free slack | Critical |\n");
    out.push_str("|------|-------|--------|----------|-------------|------------|----------|\n");
    for task in &proj.tasks {
        let Some(r) = sched.get(task.uid) else {
            continue;
        };
        let name = sanitize(&task.name).unwrap_or_else(|| format!("Task {}", task.uid));
        let name = name.replace('|', "\\|");
        let name = if task.summary {
            format!("**{name}**")
        } else {
            name
        };
        let duration_min =
            crate::schedule::summary_or_leaf_min(proj, task, r.early_start, r.early_finish);
        let dur = duration_str(proj, duration_min);
        let slack = fmt_days(proj.minutes_to_days(r.total_slack_min));
        let free_slack = fmt_days(proj.minutes_to_days(r.free_slack_min));
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            name,
            r.early_start.to_mspdi().replace('T', " "),
            r.early_finish.to_mspdi().replace('T', " "),
            dur,
            slack,
            free_slack,
            if r.critical { "✓" } else { "" },
        ));
    }
    out
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

/// True when the project's default calendar takes both weekend days off, so the
/// Mermaid `excludes weekends` directive matches the schedule.
fn excludes_weekends(proj: &Project) -> bool {
    proj.calendars
        .iter()
        .find(|c| c.uid == proj.default_calendar_uid)
        .or_else(|| proj.calendars.first())
        .map(|c| !c.week[0].working() && !c.week[6].working())
        .unwrap_or(true)
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
        b.predecessors = vec![Predecessor {
            uid: 1,
            link: LinkType::FinishStart,
            lag_min: 0,
        }];
        let mut c = task(3, "C", 480);
        c.predecessors = vec![Predecessor {
            uid: 1,
            link: LinkType::FinishStart,
            lag_min: 0,
        }];
        let mut d = task(4, "D", 960);
        d.predecessors = vec![
            Predecessor {
                uid: 2,
                link: LinkType::FinishStart,
                lag_min: 0,
            },
            Predecessor {
                uid: 3,
                link: LinkType::FinishStart,
                lag_min: 0,
            },
        ];
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
        ms.predecessors = vec![Predecessor {
            uid: 2,
            link: LinkType::FinishStart,
            lag_min: 0,
        }];
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
        b.predecessors.push(Predecessor {
            uid: 2,
            link: LinkType::FinishStart,
            lag_min: 0,
        });
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
                    "4d",
                    "0d",
                    "0d",
                    "✓"
                ],
                [
                    "A",
                    "2026-03-05 08:00:00",
                    "2026-03-09 17:00:00",
                    "3d",
                    "0d",
                    "0d",
                    "✓"
                ],
                [
                    "**Nested**",
                    "2026-03-10 08:00:00",
                    "2026-03-10 17:00:00",
                    "1d",
                    "0d",
                    "0d",
                    "✓"
                ],
                [
                    "B",
                    "2026-03-10 08:00:00",
                    "2026-03-10 17:00:00",
                    "1d",
                    "0d",
                    "0d",
                    "✓"
                ],
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
                    "2d",
                    expected,
                    "0d",
                    "✓"
                ]
            );
        }
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
                "Duration",
                "Total slack",
                "Free slack",
                "Critical"
            ]
        );
        assert_eq!(
            rows[1..]
                .iter()
                .map(|row| (row[0], row[5]))
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
                    "1d",
                    "0d",
                    "0d",
                    "✓"
                ],
                [
                    r"A \| B",
                    "2026-03-02 08:00:00",
                    "2026-03-02 17:00:00",
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
        b.predecessors = vec![Predecessor {
            uid: 2,
            link: LinkType::FinishStart,
            lag_min: 0,
        }];
        for leaf in [&mut a, &mut b] {
            leaf.outline_level = 2;
            leaf.calendar_uid = Some(3);
        }
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![phase, a, b],
            calendars: vec![
                Calendar {
                    uid: 1,
                    name: "Closed".into(),
                    week: Default::default(),
                },
                Calendar::standard(3),
            ],
            ..Project::default()
        };
        let md = to_markdown(&proj, &schedule(&proj));
        assert_eq!(
            table_rows(&md)[1][..4],
            [
                "**Phase**",
                "2026-03-02 08:00:00",
                "2026-03-03 17:00:00",
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
}
