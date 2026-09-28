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
        if source == Oracle::Project {
            let format = formats.get(&e.uid).expect("Project DurationFormat");
            let duration = if matches!(format & !32, 4 | 6 | 8 | 10 | 12 | 19 | 20) {
                None
            } else {
                Some(e.duration_min)
            };
            assert_eq!(a.duration_min, duration, "{}", at("stored duration"));
            assert_eq!(
                a.calendar_uid,
                e.calendar_uid.filter(|&uid| uid >= 0),
                "{}",
                at("task calendar UID")
            );
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
    if source == Oracle::Project {
        let scheduled = projcore::schedule::schedule(&imported);
        let project_cal = imported.project_calendar();
        let exceptions = &oracle
            .calendar(oracle.default_calendar_uid)
            .unwrap()
            .exceptions;
        let mut selected = 0usize;
        let mut dropped = [0usize; 5]; // split, delayed assignment, elapsed, recurring exception, nonworking start
        let mut compared_uids = Vec::new();
        for (task, expected_task) in imported.tasks.iter().zip(&expected) {
            if task.summary || task.manual || expected_task.milestone {
                continue;
            }
            if formats
                .get(&task.uid)
                .is_some_and(|format| matches!(format & !32, 4 | 6 | 8 | 10 | 12))
            {
                dropped[2] += 1;
                continue;
            }
            assert_eq!(
                task.duration_min,
                expected_task.duration_min,
                "{}: stored duration UID {}",
                mpp.display(),
                task.uid
            );
            // projcore's CPM does not model splits or delayed assignments.
            // Split decoding and assignment import (#342) are follow-ups;
            // delayed-assignment finish calculation needs a CPM follow-up.
            if task.uid == 23 && expected_task.name == "S13 split task" {
                dropped[0] += 1;
                continue;
            }
            if task.uid == 55 && expected_task.name == "S43 delayed start" {
                dropped[1] += 1;
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
            let task_cal = task
                .calendar_uid
                .and_then(|uid| imported.calendar(uid))
                .map(|cal| imported.resolved_calendar(cal))
                .unwrap_or_else(|| project_cal.clone());
            let working_start = task_cal
                .day(start.day_number())
                .iter()
                .any(|slot| slot.from <= start.minute_of_day() && start.minute_of_day() < slot.to);
            if !working_start {
                // Project can retain a MustStartOn timestamp before work starts;
                // projcore moves the scheduled start to the first working slot.
                dropped[4] += 1;
                continue;
            }
            selected += 1;
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
                "{}: selected {selected}, exclusions split/delayed-assignment/elapsed/recurring-exception/nonworking-start = {dropped:?}",
                mpp.display()
            );
        }
        let stem = mpp.file_stem().unwrap().to_string_lossy();
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
        if stem == "43-assignment-delay" {
            let task = imported.tasks.iter().find(|t| t.uid == 55).unwrap();
            assert_eq!(task.duration_min, 960, "delayed assignment UID 55 duration");
        }
        if stem == "21-task-calendar" {
            assert!(
                compared_uids.contains(&31),
                "task-calendar UID 31 was not compared"
            );
        }
        if stem == "32-calendar-6day" {
            assert!(
                compared_uids.contains(&49),
                "six-day calendar UID 49 was not compared"
            );
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
}
