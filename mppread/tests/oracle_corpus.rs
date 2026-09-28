//! Compare binary task rows with Microsoft Project's XML export of the same plan.
use std::path::{Path, PathBuf};

fn null_uids(xml: &str) -> std::collections::HashSet<i32> {
    let mut out = std::collections::HashSet::new();
    for after_start in xml.split("<Task>").skip(1) {
        let Some((task, _)) = after_start.split_once("</Task>") else {
            continue;
        };
        if task.contains("<IsNull>1</IsNull>") {
            let uid = task
                .split_once("<UID>")
                .and_then(|(_, s)| s.split_once("</UID>"))
                .and_then(|(s, _)| s.trim().parse::<i32>().ok())
                .expect("null MSPDI task UID");
            out.insert(uid);
        }
    }
    out
}

fn duration_formats(xml: &str) -> std::collections::HashMap<i32, i32> {
    let mut out = std::collections::HashMap::new();
    for after_start in xml.split("<Task>").skip(1) {
        let Some((task, _)) = after_start.split_once("</Task>") else {
            continue;
        };
        let direct = task.split("<Baseline>").next().unwrap();
        let uid = direct
            .split_once("<UID>")
            .and_then(|(_, s)| s.split_once("</UID>"))
            .and_then(|(s, _)| s.trim().parse::<i32>().ok());
        let format = direct
            .split_once("<DurationFormat>")
            .and_then(|(_, s)| s.split_once("</DurationFormat>"))
            .and_then(|(s, _)| s.trim().parse::<i32>().ok());
        if let (Some(uid), Some(format)) = (uid, format) {
            out.insert(uid, format);
        }
    }
    out
}

fn pairs(dir: &Path, suffix: &str) -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if path.extension().is_some_and(|e| e == "mpp")
                && stem.ends_with(suffix)
                && (!suffix.is_empty() || !stem.ends_with("-mpp12"))
            {
                let base = stem.strip_suffix(suffix).unwrap();
                let xml = path.with_file_name(format!("{base}.xml"));
                assert!(xml.exists(), "missing oracle for {}", path.display());
                out.push((path, xml));
            }
        }
    }
    out.sort();
    out
}

/// Who wrote a pair's XML: Project itself, or MPXJ (the external paired
/// corpus), which omits zero-valued progress elements.
#[derive(Clone, Copy, PartialEq)]
enum Oracle {
    Project,
    Mpxj,
}

/// Compare a task's decoded progress with its XML export, field by field.
/// Project writes every numeric progress element, zeros included, and omits
/// a date that is NA; MPXJ also omits zero numbers, which then count as 0.
fn check_progress(
    a: &mppread::mpp::MppTask,
    e: &projcore::Task,
    oracle: Oracle,
    at: &dyn Fn(&str) -> String,
) {
    let p = a
        .progress
        .as_ref()
        .unwrap_or_else(|| panic!("{}", at("progress")));
    let zero = |v: Option<i64>| match (v, oracle) {
        (None, Oracle::Mpxj) => Some(0),
        _ => v,
    };
    let zero_pct = |v: Option<u8>| match (v, oracle) {
        (None, Oracle::Mpxj) => Some(0),
        _ => v,
    };
    let zero_cost = |v: &Option<projcore::Rate>| match (v, oracle) {
        (None, Oracle::Mpxj) => projcore::Rate::parse("0"),
        _ => v.clone(),
    };
    let dt =
        |d: Option<projcore::DateTime>| d.map(|d| d.to_mspdi().replace('T', " ")[..16].to_string());
    let percents = [
        ("percent complete", p.percent_complete, e.percent_complete),
        (
            "percent work complete",
            p.percent_work_complete,
            e.percent_work_complete,
        ),
        (
            "physical percent complete",
            p.physical_percent_complete,
            e.physical_percent_complete,
        ),
    ];
    for (what, got, want) in percents {
        assert_eq!(Some(got), zero_pct(want), "{}", at(what));
    }
    let dates = [
        ("actual start", &p.actual_start, e.actual_start),
        ("actual finish", &p.actual_finish, e.actual_finish),
        ("stop", &p.stop, e.stop),
        ("resume", &p.resume, e.resume),
    ];
    for (what, got, want) in dates {
        assert_eq!(got.clone(), dt(want), "{}", at(what));
    }
    let minutes = [
        (
            "actual duration",
            p.actual_duration_min,
            e.actual_duration_min,
        ),
        (
            "remaining duration",
            p.remaining_duration_min,
            e.remaining_duration_min,
        ),
        ("work", p.work_min, e.work_min),
        ("actual work", p.actual_work_min, e.actual_work_min),
        ("remaining work", p.remaining_work_min, e.remaining_work_min),
    ];
    for (what, got, want) in minutes {
        assert_eq!(Some(got), zero(want), "{}", at(what));
    }
    let costs = [
        ("cost", &p.cost, &e.cost),
        ("actual cost", &p.actual_cost, &e.actual_cost),
        ("remaining cost", &p.remaining_cost, &e.remaining_cost),
    ];
    for (what, got, want) in costs {
        assert_eq!(Some(got.clone()), zero_cost(want), "{}", at(what));
    }
}

fn check_pair(mpp: &Path, xml: &Path, may_refuse: bool, source: Oracle) -> bool {
    let bytes = std::fs::read(mpp).unwrap();
    let xml_text = std::fs::read_to_string(xml).unwrap();
    let oracle = projcore::mspdi::read_mspdi(&xml_text).unwrap();
    let nulls = null_uids(&xml_text);
    let formats = duration_formats(&xml_text);
    let decoded = match mppread::mpp::decode_tasks(&bytes) {
        Ok(tasks) => tasks,
        Err(error) if may_refuse => {
            assert!(
                mppread::project::project_from_mpp(&bytes).is_err(),
                "{}: {error}",
                mpp.display()
            );
            return false;
        }
        Err(error) => panic!("{}: {error}", mpp.display()),
    };
    let actual: Vec<_> = decoded.iter().filter(|t| t.uid != 0).collect();
    let expected: Vec<_> = oracle
        .tasks
        .iter()
        .filter(|t| t.uid != 0 && !nulls.contains(&t.uid))
        .collect();
    assert_eq!(
        actual.len(),
        expected.len(),
        "{}: task count",
        mpp.display()
    );
    for (a, e) in actual.iter().zip(&expected) {
        assert_eq!(a.id as i32, e.id, "{}: uid {} row ID", mpp.display(), e.uid);
        assert_eq!(a.uid as i32, e.uid, "{}: uid", mpp.display());
        assert_eq!(a.name, e.name, "{}: uid {} name", mpp.display(), e.uid);
        assert_eq!(
            a.outline_level,
            Some(e.outline_level),
            "{}: uid {} level",
            mpp.display(),
            e.uid
        );
        let dt = |value: projcore::DateTime| {
            value
                .to_mspdi()
                .replace('T', " ")
                .get(..16)
                .unwrap()
                .to_string()
        };
        assert_eq!(
            a.start.as_deref(),
            e.stored_start.map(dt).as_deref(),
            "{}: uid {} start",
            mpp.display(),
            e.uid
        );
        assert_eq!(
            a.finish.as_deref(),
            e.stored_finish.map(dt).as_deref(),
            "{}: uid {} finish",
            mpp.display(),
            e.uid
        );
        let mut got: Vec<_> = a
            .predecessors
            .iter()
            .map(|p| {
                (
                    p.pred_uid as i32,
                    p.kind as i64,
                    p.lag,
                    i64::from(p.lag_format),
                )
            })
            .collect();
        let mut want: Vec<_> = e
            .predecessors
            .iter()
            .map(|p| (p.uid, p.link.code(), p.lag, p.lag_format.code()))
            .collect();
        got.sort();
        want.sort();
        assert_eq!(got, want, "{}: uid {} predecessors", mpp.display(), e.uid);
        let at = |what: &str| format!("{}: uid {} {what}", mpp.display(), e.uid);
        check_progress(a, e, source, &at);
        if !e.summary && a.progress.is_some() {
            assert_eq!(
                a.duration_min,
                Some(e.duration_min),
                "{}",
                at("stored duration")
            );
            assert_eq!(
                a.duration_format.map(i32::from),
                formats.get(&e.uid).copied(),
                "{}",
                at("stored DurationFormat")
            );
            assert_eq!(a.calendar_uid, e.calendar_uid, "{}", at("CalendarUID"));
        }
        assert_eq!(a.manual, e.manual, "{}", at("manual"));
        if e.manual {
            assert_eq!(
                a.manual_start.as_deref(),
                e.manual_start.map(dt).as_deref(),
                "{}",
                at("manual start")
            );
            assert_eq!(
                a.manual_finish.as_deref(),
                e.manual_finish.map(dt).as_deref(),
                "{}",
                at("manual finish")
            );
            assert_eq!(
                a.manual_duration_min,
                e.manual_duration_min,
                "{}",
                at("manual duration")
            );
        } else {
            // An auto task's manual fields are not decoded: Project's export
            // derives them from its Start, Finish and Duration, or omits them.
            assert_eq!(
                (&a.manual_start, &a.manual_finish, a.manual_duration_min),
                (&None, &None, None),
                "{}",
                at("auto task manual fields")
            );
            assert!(
                e.manual_start.is_none_or(|d| Some(d) == e.stored_start)
                    && e.manual_finish.is_none_or(|d| Some(d) == e.stored_finish)
                    && e.manual_duration_min.is_none_or(|d| d == e.duration_min),
                "{}",
                at("oracle derives an auto task's manual fields")
            );
        }
    }
    let imported = mppread::project::project_from_mpp(&bytes)
        .unwrap_or_else(|e| panic!("{}: decoded import: {e}", mpp.display()));
    if xml_text.contains("<Calendars>") {
        let fields = |c: &projcore::Calendar| {
            (
                c.uid,
                c.name.clone(),
                c.base_calendar_uid,
                c.week.clone(),
                c.exceptions.clone(),
                c.work_weeks.clone(),
            )
        };
        let expected_calendars: Vec<_> = oracle
            .calendars
            .iter()
            .filter(|cal| {
                // MPXJ adds an implicit resource-0 calendar omitted by Project XML.
                !(source == Oracle::Mpxj
                    && cal.name == "Unnamed Resource"
                    && cal.base_calendar_uid.is_some()
                    && cal.week.iter().all(Option::is_none)
                    && !imported.calendars.iter().any(|got| got.uid == cal.uid))
            })
            .map(fields)
            .collect();
        assert_eq!(
            imported.calendars.iter().map(fields).collect::<Vec<_>>(),
            expected_calendars,
            "{}: calendars",
            mpp.display()
        );
        assert_eq!(
            imported.default_calendar_uid,
            oracle.default_calendar_uid,
            "{}: default calendar UID",
            mpp.display()
        );
    }
    assert_eq!(
        imported.tasks.iter().map(|t| t.id).collect::<Vec<_>>(),
        actual.iter().map(|t| t.id as i32).collect::<Vec<_>>(),
        "{}: imported task IDs",
        mpp.display()
    );
    for (t, e) in imported.tasks.iter().zip(&expected) {
        assert_eq!(
            t.manual,
            e.manual,
            "{}: uid {} imported mode",
            mpp.display(),
            e.uid
        );
        if t.manual && !t.summary {
            assert_eq!(
                t.duration_min,
                e.duration_min,
                "{}: uid {} imported manual duration",
                mpp.display(),
                e.uid
            );
        }
    }
    // Clearing the newly imported assignments/resources reproduces this
    // importer's previous scheduling inputs. Their presence must not move a
    // task, including plans that already carry a delayed assignment.
    let mut previous_inputs = imported.clone();
    previous_inputs.assignments.clear();
    previous_inputs.resources.clear();
    let old_schedule = projcore::schedule::schedule(&previous_inputs);
    let new_schedule = projcore::schedule::schedule(&imported);
    for task in &imported.tasks {
        let old = old_schedule.get(task.uid).unwrap();
        let new = new_schedule.get(task.uid).unwrap();
        assert_eq!(
            (new.early_start, new.early_finish),
            (old.early_start, old.early_finish),
            "{}: imported assignments moved task UID {}",
            mpp.display(),
            task.uid
        );
    }
    if source == Oracle::Project {
        let scheduled = projcore::schedule::schedule(&imported);
        let exceptions = &oracle
            .calendar(oracle.default_calendar_uid)
            .unwrap()
            .exceptions;
        let mut selected = 0usize;
        let mut duration_compared = 0usize;
        let mut split_duration_compared = false;
        let mut dropped = [0usize; 5]; // split, delayed assignment, elapsed, recurring exception, nonworking start
        let mut compared_uids = Vec::new();
        for (task, expected_task) in imported.tasks.iter().zip(&expected) {
            if task.summary || task.manual || expected_task.milestone {
                continue;
            }
            if formats
                .get(&task.uid)
                .is_some_and(|&format| mppread::mpp::working_duration_format(format as u16))
            {
                assert_eq!(
                    task.duration_min,
                    expected_task.duration_min,
                    "{}: stored working duration UID {}",
                    mpp.display(),
                    task.uid
                );
                duration_compared += 1;
                if mpp.file_stem().is_some_and(|stem| stem == "13-split-task") && task.uid == 23 {
                    assert_eq!(task.duration_min, 1920);
                    split_duration_compared = true;
                }
            }
            // Splits are not decoded from .mpp; this known task has a work
            // interruption that makes its duration shorter than its span.
            if task.uid == 23 && expected_task.name == "S13 split task" {
                dropped[0] += 1;
                continue;
            }
            if task.uid == 55 && expected_task.name == "S43 delayed start" {
                // Project's delayed assignment makes this task's Duration
                // shorter than the working span; the assignment delay is not decoded.
                assert_eq!(task.duration_min, expected_task.duration_min);
                dropped[1] += 1;
                continue;
            }
            if formats
                .get(&task.uid)
                .is_some_and(|format| matches!(format & !32, 4 | 6 | 8 | 10 | 12))
            {
                dropped[2] += 1;
                continue;
            }
            let (start, finish) = (task.stored_start.unwrap(), task.stored_finish.unwrap());
            if exceptions
                .iter()
                .filter(|e| e.scheduled().is_none())
                .any(|exception| {
                    exception.from.zip(exception.to).is_some_and(|(from, to)| {
                        from.day_number() <= finish.day_number()
                            && to.day_number() >= start.day_number()
                    })
                })
            {
                dropped[3] += 1;
                continue;
            }
            selected += 1;
            assert_eq!(
                task.duration_min,
                expected_task.duration_min,
                "{}: calendar duration UID {}",
                mpp.display(),
                task.uid
            );
            let calendar = imported
                .calendar(task.calendar_uid.unwrap_or(imported.default_calendar_uid))
                .map(|cal| imported.resolved_calendar(cal))
                .unwrap_or_else(|| imported.project_calendar());
            let working_start = calendar
                .day(start.day_number())
                .iter()
                .any(|slot| slot.from <= start.minute_of_day() && start.minute_of_day() < slot.to);
            if !working_start {
                // Project can retain a MustStartOn timestamp before work starts;
                // projcore moves the scheduled start to the first working slot.
                dropped[4] += 1;
                continue;
            }
            let result = scheduled.get(task.uid).unwrap();
            assert_eq!(
                (result.early_start, result.early_finish),
                (task.stored_start.unwrap(), task.stored_finish.unwrap()),
                "{}: scheduled UID {}",
                mpp.display(),
                task.uid
            );
            compared_uids.push(task.uid);
        }
        if dropped.iter().any(|&n| n > 0) {
            eprintln!(
                "{}: duration-compared {duration_compared}, date-selected {selected}, exclusions split/delayed-assignment/elapsed/recurring-exception/nonworking-start = {dropped:?}",
                mpp.display()
            );
        }
        let stem = mpp.file_stem().unwrap().to_string_lossy();
        if stem == "13-split-task" {
            assert!(
                split_duration_compared,
                "{}: split task duration was not compared",
                mpp.display()
            );
        }
        if stem == "21-task-calendar" {
            assert!(
                compared_uids.contains(&31),
                "{}: UID 31 date was not compared",
                mpp.display()
            );
        }
        if stem == "32-calendar-6day" {
            assert!(
                compared_uids.contains(&49),
                "{}: UID 49 date was not compared",
                mpp.display()
            );
        }
        if stem == "43-assignment-delay" {
            assert_eq!(
                dropped[1],
                1,
                "{}: delayed assignment was not excluded from date check",
                mpp.display()
            );
        }
        let recurrence = oracle
            .calendar(oracle.default_calendar_uid)
            .and_then(|cal| cal.exceptions.first());
        match stem.as_ref() {
            "e6-monthly-position" | "k5-monthly-position" => {
                let e = recurrence.unwrap();
                assert_eq!(
                    (e.kind, e.occurrences, e.month_position),
                    (Some(5), Some(7), Some(1))
                );
            }
            "e7-yearly-date" | "k6-yearly-date" => {
                let e = recurrence.unwrap();
                assert_eq!(
                    (e.kind, e.occurrences, e.month),
                    (Some(2), Some(5), Some(2))
                );
            }
            "e8-yearly-position" | "k7-yearly-position" => {
                let e = recurrence.unwrap();
                assert_eq!(
                    (e.kind, e.occurrences, e.month, e.month_position),
                    (Some(3), Some(5), Some(2), Some(1))
                );
            }
            _ => {}
        }
        if stem == "33-calendar-holiday" {
            let task = imported.tasks.iter().find(|t| t.uid == 5).unwrap();
            assert_eq!(
                task.duration_min,
                4800,
                "{}: holiday UID 5 duration",
                mpp.display()
            );
            assert!(
                compared_uids.contains(&5),
                "{}: holiday UID 5 was not compared",
                mpp.display()
            );
        }
        if mpp
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|name| name == "workweeks")
        {
            // These probes have one intended auto task. A generic selected
            // count could pass after dropping the task we meant to check.
            assert!(
                compared_uids.contains(&1),
                "{}: work-week task was not date-compared",
                mpp.display()
            );
            let task = imported.tasks.iter().find(|t| t.uid == 1).unwrap();
            let expected_task = expected.iter().find(|t| t.uid == 1).unwrap();
            assert_eq!(task.duration_min, expected_task.duration_min);
            let expected_calendar = match stem.as_ref() {
                "w7-derived" => Some(3),
                "w12-task-calendar" => Some(5),
                "w8-inherited" => Some(2),
                _ => None,
            };
            assert_eq!(task.calendar_uid, expected_calendar);
            if stem == "w12-task-calendar" {
                let calendar = imported.calendar(5).unwrap();
                assert!(calendar.base_calendar_uid.is_none());
                assert_ne!(imported.default_calendar_uid, 5);
                assert!(oracle.resources.iter().all(|r| r.calendar_uid != Some(5)));
            }
            if [
                "w8-inherited",
                "w12-task-calendar",
                "m1-com-summer",
                "m2-com-out-of-order",
            ]
            .contains(&stem.as_ref())
            {
                let mut without_work_weeks = imported.clone();
                for calendar in &mut without_work_weeks.calendars {
                    calendar.work_weeks.clear();
                }
                assert_ne!(
                    projcore::schedule::schedule(&without_work_weeks)
                        .get(1)
                        .unwrap()
                        .early_finish,
                    scheduled.get(1).unwrap().early_finish,
                    "{}: task does not depend on an alternate work week",
                    mpp.display()
                );
            }
        }
        if [
            "e1-range",
            "e2-weekend-working",
            "e10-several-unicode",
            "e11-unnamed",
            "k1-one-off",
            "k9-unnamed",
        ]
        .contains(&stem.as_ref())
        {
            assert!(
                compared_uids.contains(&1),
                "{}: one-off exception task was not compared",
                mpp.display()
            );
        }
        if [
            "31-calendar-hours",
            "32-calendar-6day",
            "38-resource-calendar",
        ]
        .contains(&stem.as_ref())
            || stem.starts_with("c1-")
            || stem.starts_with("c2-")
            || stem.starts_with("c3-")
            || stem.starts_with("c4-")
            || stem.starts_with("c5-")
        {
            assert!(
                selected > 0,
                "{}: no selected calendar duration oracle",
                mpp.display()
            );
        }
    }
    assert_eq!(
        mppread::mpp::decode_new_tasks_are_manual(&bytes),
        Ok(oracle.new_tasks_are_manual),
        "{}: NewTasksAreManual",
        mpp.display()
    );
    assert_eq!(
        imported.new_tasks_are_manual,
        oracle.new_tasks_are_manual,
        "{}: imported NewTasksAreManual",
        mpp.display()
    );
    true
}

fn compare_assignment_oracle(mpp: &Path, xml: &Path) {
    use std::collections::HashMap;
    let imported = mppread::project::project_from_mpp(&std::fs::read(mpp).unwrap()).unwrap();
    let expected = projcore::mspdi::read_mspdi(&std::fs::read_to_string(xml).unwrap()).unwrap();
    let expected_assignments: HashMap<_, _> = expected
        .assignments
        .iter()
        .filter(|a| a.task_uid != 0)
        .map(|a| (a.uid, a))
        .collect();
    assert_eq!(
        imported.assignments.len(),
        expected_assignments.len(),
        "{} assignment count",
        mpp.display()
    );
    let close = |a: Option<&projcore::Rate>, b: Option<&projcore::Rate>, what: &str| match (a, b) {
        (None, None) => {}
        (Some(a), Some(b)) => assert!(
            (a.to_f64().unwrap() - b.to_f64().unwrap()).abs() <= 0.005,
            "{} {what}: {} vs {}",
            mpp.display(),
            a.as_str(),
            b.as_str()
        ),
        _ => panic!("{} {what}: {a:?} vs {b:?}", mpp.display()),
    };
    for a in &imported.assignments {
        let e = expected_assignments
            .get(&a.uid)
            .unwrap_or_else(|| panic!("{} missing assignment UID {}", mpp.display(), a.uid));
        assert_eq!(
            (a.task_uid, a.resource_uid),
            (e.task_uid, e.resource_uid),
            "{} assignment UID {}",
            mpp.display(),
            a.uid
        );
        assert!(
            (a.units - e.units).abs() < 1e-6,
            "{} assignment UID {} units {} vs {}",
            mpp.display(),
            a.uid,
            a.units,
            e.units
        );
        assert_eq!(
            a.work_min,
            e.work_min,
            "{} assignment UID {} work",
            mpp.display(),
            a.uid
        );
        assert_eq!(
            (a.start, a.finish),
            (e.start, e.finish),
            "{} assignment UID {} dates",
            mpp.display(),
            a.uid
        );
        assert_eq!(
            a.baselines.len(),
            e.baselines.len(),
            "{} assignment UID {} baseline count",
            mpp.display(),
            a.uid
        );
        for slot in 0..=10 {
            let got = a.baseline(slot);
            let want = e.baseline(slot);
            assert_eq!(
                got.is_some(),
                want.is_some(),
                "{} assignment UID {} baseline slot {slot}",
                mpp.display(),
                a.uid
            );
            if let (Some(g), Some(w)) = (got, want) {
                assert_eq!(
                    (g.start, g.finish, g.work_min),
                    (w.start, w.finish, w.work_min),
                    "{} assignment UID {} baseline slot {slot}",
                    mpp.display(),
                    a.uid
                );
                close(g.cost.as_ref(), w.cost.as_ref(), "baseline cost");
                assert!(
                    g.bcws.is_none() && g.bcwp.is_none() && w.bcws.is_none() && w.bcwp.is_none(),
                    "Project XML carries no baseline BCWS/BCWP"
                );
            }
        }
        if a.resource_uid != -65535 {
            let got = imported
                .resources
                .iter()
                .find(|r| r.uid == a.resource_uid)
                .unwrap();
            let want = expected
                .resources
                .iter()
                .find(|r| r.uid == a.resource_uid)
                .unwrap();
            assert_eq!(
                (got.uid, got.id, &got.name, got.kind),
                (want.uid, want.id, &want.name, want.kind),
                "{} resource UID {}",
                mpp.display(),
                got.uid
            );
        }
    }
    // The same records survive the actual MSPDI Save As path.
    let saved = projcore::mspdi::read_mspdi(&projcore::mspdi::write_mspdi(&imported)).unwrap();
    for a in &imported.assignments {
        assert_eq!(
            saved
                .assignments
                .iter()
                .find(|s| s.uid == a.uid)
                .unwrap()
                .baselines,
            a.baselines,
            "{} save UID {}",
            mpp.display(),
            a.uid
        );
    }
}

#[test]
fn assignment_oracles() {
    let generated = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/assnbaseline");
    if generated.exists() {
        let cases = pairs(&generated, "");
        assert_eq!(cases.len(), 3);
        for (mpp, xml) in &cases {
            compare_assignment_oracle(mpp, xml);
        }
    }
    if let Ok(paired) = std::env::var("MPP_PAIRED_CORPUS") {
        for stem in [
            "18-resource-assignment",
            "19-two-assignments",
            "20-overallocation",
            "21-material-resource",
            "22-resource-rates",
            "24-baseline",
            "25-progress",
        ] {
            let dir = Path::new(&paired);
            compare_assignment_oracle(
                &dir.join(format!("{stem}.mpp")),
                &dir.join(format!("{stem}.xml")),
            );
        }
    }
}

#[test]
fn project_2024_oracles() {
    let snapshots = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/snapshots");
    if snapshots.join("01-empty.mpp").exists() {
        let newest = pairs(&snapshots, "")
            .into_iter()
            .filter(|(p, _)| !p.file_stem().unwrap().to_string_lossy().ends_with("-mpp12"))
            .collect::<Vec<_>>();
        let older = pairs(&snapshots, "-mpp12");
        assert_eq!(newest.len(), 46);
        assert_eq!(older.len(), 46);
        for (mpp, xml) in &newest {
            check_pair(mpp, xml, false, Oracle::Project);
        }
        let matched = older
            .iter()
            .filter(|(mpp, xml)| check_pair(mpp, xml, true, Oracle::Project))
            .count();
        eprintln!("MPP12 matched {matched}, refused {}", older.len() - matched);
    }
    let order = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/order");
    if order.exists() {
        let cases = pairs(&order, "");
        assert_eq!(cases.len(), 5);
        for (mpp, xml) in &cases {
            check_pair(mpp, xml, false, Oracle::Project);
        }
    }
    // Percentage, elapsed and estimated lags (#104): each is compared in its
    // own kind and format, not only as minutes.
    let lag = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/lag");
    if lag.exists() {
        let cases = pairs(&lag, "");
        assert_eq!(cases.len(), 2);
        for (mpp, xml) in &cases {
            check_pair(mpp, xml, false, Oracle::Project);
        }
    }
    let manual = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/manual");
    if manual.exists() {
        let cases = pairs(&manual, "");
        assert_eq!(cases.len(), 9);
        for (mpp, xml) in &cases {
            check_pair(mpp, xml, false, Oracle::Project);
        }
    }
    let exceptions = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/exceptions");
    if exceptions.exists() {
        let cases = pairs(&exceptions, "");
        assert_eq!(
            cases.len(),
            19 + usize::from(exceptions.join("k8-period-300.mpp").exists())
        );
        for (mpp, xml) in &cases {
            check_pair(mpp, xml, false, Oracle::Project);
        }
    }
    // Recorded progress (#181): states, work, costs, variances after a
    // baseline, a split, and fractional values.
    let progress = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/progress");
    if progress.exists() {
        let cases = pairs(&progress, "");
        assert_eq!(cases.len(), 6);
        for (mpp, xml) in &cases {
            check_pair(mpp, xml, false, Oracle::Project);
        }
    }
    if let Ok(paired) = std::env::var("MPP_PAIRED_CORPUS") {
        let dir = Path::new(&paired);
        assert!(
            dir.is_dir(),
            "MPP_PAIRED_CORPUS is not a directory: {}",
            dir.display()
        );
        let cases = pairs(dir, "");
        assert_eq!(cases.len(), 27);
        for (mpp, xml) in &cases {
            check_pair(mpp, xml, false, Oracle::Mpxj);
        }
    }
    let calendar = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/calendar");
    if calendar.exists() {
        let cases = pairs(&calendar, "");
        assert_eq!(cases.len(), 5);
        assert_eq!(std::fs::read_dir(&calendar).unwrap().flatten().count(), 10);
        for (mpp, xml) in &cases {
            check_pair(mpp, xml, false, Oracle::Project);
        }
        let c1 = std::fs::read(calendar.join("c1-default-night.mpp")).unwrap();
        assert_ne!(
            mppread::project::project_from_mpp(&c1)
                .unwrap()
                .default_calendar_uid,
            1
        );
    }
    let workweeks = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/workweeks");
    if workweeks.exists() {
        let cases = pairs(&workweeks, "");
        assert_eq!(cases.len(), 14);
        let expected: &[(&str, i32, &[&str])] = &[
            ("w1-summer", 1, &["Summer"]),
            ("w2-partial", 1, &["Partial"]),
            ("w3-weekend", 1, &["Weekend"]),
            ("w4-five-periods", 1, &["Five"]),
            ("w5-daylong", 1, &["Daylong"]),
            ("w6-two", 1, &["Earlier", "Later"]),
            ("w7-derived", 3, &["Crew"]),
            ("w8-inherited", 1, &["Inherited"]),
            ("w9-unicode", 1, &["Fête 日本語"]),
            ("w10-unnamed", 1, &[""]),
            ("w11-exception", 1, &["With holiday"]),
            ("w12-task-calendar", 5, &["Task summer"]),
            ("m1-com-summer", 1, &["COM Summer"]),
            ("m2-com-out-of-order", 1, &["Earlier", "Later"]),
        ];
        for (mpp, xml) in &cases {
            let stem = mpp.file_stem().unwrap().to_str().unwrap();
            let (_, uid, names) = expected.iter().find(|(name, _, _)| *name == stem).unwrap();
            let oracle =
                projcore::mspdi::read_mspdi(&std::fs::read_to_string(xml).unwrap()).unwrap();
            let weeks = &oracle.calendar(*uid).unwrap().work_weeks;
            assert_eq!(
                weeks
                    .iter()
                    .map(|w| w.name.as_deref().unwrap_or(""))
                    .collect::<Vec<_>>(),
                *names
            );
            for (index, week) in weeks.iter().enumerate() {
                let (first, last) =
                    if (stem == "w6-two" || stem == "m2-com-out-of-order") && index == 0 {
                        (9, 13)
                    } else if (stem == "w6-two" || stem == "m2-com-out-of-order") && index == 1 {
                        (16, 20)
                    } else {
                        (9, 20)
                    };
                assert_eq!(
                    week.from.unwrap().to_mspdi(),
                    format!("2026-03-{first:02}T00:00:00")
                );
                assert_eq!(
                    week.to.unwrap().to_mspdi(),
                    format!("2026-03-{last:02}T23:59:00")
                );
            }
            check_pair(mpp, xml, false, Oracle::Project);
        }
    }
}
