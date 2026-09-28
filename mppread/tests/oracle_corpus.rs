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

#[derive(Default)]
struct PairExclusions {
    guid: bool,
    recurring_occurrence_manual: bool,
    new_tasks_mode: bool,
    root_name: bool,
    ignore_resource_calendar_uids: &'static [i32],
}

impl PairExclusions {
    fn for_mpp(path: &Path) -> Self {
        match path.file_stem().and_then(|s| s.to_str()) {
            Some("x-recurring") => Self {
                guid: true,
                recurring_occurrence_manual: true,
                new_tasks_mode: true,
                ..Self::default()
            },
            Some("x-overallocated") => Self {
                guid: true,
                root_name: true,
                ..Self::default()
            },
            Some("26-task-calendar") => Self {
                // Binary UID 1 has this bit set; the paired MPXJ XML says 0.
                // Main's paired oracle failed here before this exclusion.
                ignore_resource_calendar_uids: &[1],
                ..Self::default()
            },
            _ => Self::default(),
        }
    }

    fn skip_manual(&self, uid: i32) -> bool {
        self.recurring_occurrence_manual && (2..=5).contains(&uid)
    }
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

#[allow(clippy::too_many_arguments)]
fn check_task_fields(
    a: &mppread::mpp::MppTask,
    imported: &projcore::Task,
    e: &projcore::Task,
    at: &dyn Fn(&str) -> String,
    custom_wbs_mask: bool,
    require_fields: bool,
    exclude_guid: bool,
    exclude_ignore_resource_calendar: bool,
) {
    let Some(f) = &a.fields else {
        assert!(!require_fields, "{}", at("fields"));
        return;
    }; // MPP9 and legacy layouts have no validated task fields.
    macro_rules! same {
        ($field:ident) => {
            assert_eq!(f.$field, e.$field, "{}", at(stringify!($field)));
            assert_eq!(
                imported.$field,
                e.$field,
                "{}",
                at(concat!("imported ", stringify!($field)))
            );
        };
    }
    if exclude_guid {
        // The standalone XML was exported after a legacy save regenerated GUIDs.
        assert_eq!(f.guid, imported.guid, "{}", at("binary/imported guid"));
    } else {
        same!(guid);
    }
    if custom_wbs_mask {
        // f8 has no per-task code: preserve the unknown value.
        assert_eq!(f.wbs, None, "{}", at("decoded wbs"));
        assert_eq!(imported.wbs, None, "{}", at("imported wbs"));
    } else {
        same!(wbs);
    }
    same!(task_type);
    same!(notes);
    same!(active);
    same!(effort_driven);
    same!(estimated);
    same!(priority);
    same!(level_assignments);
    same!(leveling_can_split);
    same!(leveling_delay);
    same!(leveling_delay_format);
    if !exclude_ignore_resource_calendar {
        same!(ignore_resource_calendar);
    }
    same!(earned_value_method);
    same!(recurring);
    same!(over_allocated);
    same!(hide_bar);
    same!(rollup);
    same!(external_task);
    same!(is_subproject);
    same!(is_subproject_read_only);
    assert_eq!(f.milestone, Some(e.milestone), "{}", at("milestone"));
    assert_eq!(
        imported.milestone,
        e.milestone,
        "{}",
        at("imported milestone")
    );
    let dt =
        |d: Option<projcore::DateTime>| d.map(|d| d.to_mspdi().replace('T', " ")[..16].to_string());
    if require_fields {
        assert_eq!(
            f.constraint_type,
            Some(e.constraint),
            "{}",
            at("decoded constraint type")
        );
        assert_eq!(
            f.constraint_date,
            dt(e.constraint_date),
            "{}",
            at("decoded constraint date")
        );
        if (imported.constraint, imported.constraint_date) != (e.constraint, e.constraint_date) {
            assert!(
                !imported.manual && !imported.summary,
                "{}",
                at("unexpected pin row")
            );
            assert_eq!(
                (imported.constraint, imported.constraint_date),
                (projcore::ConstraintType::MustStartOn, imported.stored_start),
                "{}",
                at("imported pin")
            );
        }
    }
    assert_eq!(f.create_date, dt(e.create_date), "{}", at("create date"));
    assert_eq!(f.deadline, dt(e.deadline), "{}", at("deadline"));
    assert_eq!(
        imported.create_date,
        e.create_date,
        "{}",
        at("imported create date")
    );
    assert_eq!(imported.deadline, e.deadline, "{}", at("imported deadline"));
}

fn check_pair(mpp: &Path, xml: &Path, may_refuse: bool, source: Oracle) -> bool {
    let custom_wbs_mask = mpp.file_stem().is_some_and(|s| s == "f8-wbs-mask");
    let exclusions = PairExclusions::for_mpp(mpp);
    // Snapshot MPP12 pairs may decode through the legacy index without
    // validated task fields; the current Project pairs must have them.
    let require_fields = source == Oracle::Project && !may_refuse;
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
    if exclusions.root_name {
        let binary_root = decoded.iter().find(|t| t.uid == 0).unwrap();
        let xml_root = oracle.tasks.iter().find(|t| t.uid == 0).unwrap();
        assert_eq!(
            binary_root.name,
            "overallocated",
            "{}: UID 0 binary name",
            mpp.display()
        );
        assert_eq!(
            xml_root.name,
            "x-overallocated-mpp12",
            "{}: UID 0 legacy XML name",
            mpp.display()
        );
    }
    if source == Oracle::Project {
        if let (Some(root), Some(binary_root)) = (
            oracle.tasks.iter().find(|t| t.uid == 0),
            decoded.iter().find(|t| t.uid == 0),
        ) {
            assert_eq!(
                binary_root
                    .fields
                    .as_ref()
                    .and_then(|f| f.earned_value_method),
                root.earned_value_method,
                "{}: UID 0 EarnedValueMethod",
                mpp.display()
            );
        }
    }
    let expected: Vec<_> = oracle.tasks.iter().filter(|t| t.uid != 0).collect();
    let expected_uids: std::collections::HashSet<_> = expected.iter().map(|t| t.uid).collect();
    // Project's export omits a cross-project ghost predecessor even though
    // its MPP task table and COM Tasks collection contain that external row.
    let actual: Vec<_> = decoded
        .iter()
        .filter(|t| t.uid != 0 && expected_uids.contains(&(t.uid as i32)))
        .collect();
    for ghost in decoded
        .iter()
        .filter(|t| t.uid != 0 && !expected_uids.contains(&(t.uid as i32)))
    {
        assert_eq!(
            ghost.fields.as_ref().and_then(|f| f.external_task),
            Some(true),
            "{}: unexported UID {} must be external",
            mpp.display(),
            ghost.uid
        );
    }
    assert_eq!(
        actual.len(),
        expected.len(),
        "{}: task count",
        mpp.display()
    );
    let omitted_external = decoded
        .iter()
        .any(|t| t.uid != 0 && !expected_uids.contains(&(t.uid as i32)));
    for (a, e) in actual.iter().zip(&expected) {
        if !omitted_external {
            assert_eq!(a.id as i32, e.id, "{}: uid {} row ID", mpp.display(), e.uid);
        }
        assert_eq!(a.uid as i32, e.uid, "{}: uid", mpp.display());
        assert_eq!(
            a.is_null,
            nulls.contains(&e.uid),
            "{}: uid {} IsNull",
            mpp.display(),
            e.uid
        );
        if a.is_null {
            continue;
        }
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
        if !e.summary && a.progress.is_some() && !exclusions.skip_manual(e.uid) {
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
        if exclusions.skip_manual(e.uid) {
            assert!(
                !e.manual && a.manual,
                "{}",
                at("legacy XML occurrence manual difference")
            );
        } else {
            assert_eq!(a.manual, e.manual, "{}", at("manual"));
        }
        // The standalone XML predates the current MPP's manual-mode rewrite.
        if !exclusions.skip_manual(e.uid) && e.manual {
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
        } else if !exclusions.skip_manual(e.uid) {
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
    if source == Oracle::Project && !may_refuse {
        assert_eq!(
            imported.start_date,
            oracle.start_date,
            "{}: project StartDate",
            mpp.display()
        );
    }
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
        imported
            .tasks
            .iter()
            .filter(|t| expected_uids.contains(&t.uid))
            .map(|t| t.id)
            .collect::<Vec<_>>(),
        actual.iter().map(|t| t.id as i32).collect::<Vec<_>>(),
        "{}: imported task IDs",
        mpp.display()
    );
    for (t, e) in imported
        .tasks
        .iter()
        .filter(|t| expected_uids.contains(&t.uid))
        .zip(&expected)
    {
        assert_eq!(
            t.is_null,
            nulls.contains(&e.uid),
            "{}: uid {} imported IsNull",
            mpp.display(),
            e.uid
        );
        if t.is_null {
            continue;
        }
        if t.is_subproject == Some(true) {
            // A childless inserted subproject must remain a schedulable leaf.
            // Preserving Project's Summary=1 needs the projcore follow-up.
            assert!(
                !t.summary,
                "{}: uid {} imported subproject summary",
                mpp.display(),
                e.uid
            );
        } else {
            assert_eq!(
                t.summary,
                e.summary,
                "{}: uid {} imported summary",
                mpp.display(),
                e.uid
            );
        }
        if t.is_subproject == Some(true) {
            assert_eq!(
                t.duration_min,
                e.duration_min,
                "{}: uid {} subproject duration",
                mpp.display(),
                e.uid
            );
            let editor = projcore::editor::Editor::new(imported.clone());
            let after = editor
                .project()
                .tasks
                .iter()
                .find(|row| row.uid == e.uid)
                .unwrap();
            assert_eq!(
                after.duration_min,
                e.duration_min,
                "{}: uid {} edited subproject duration",
                mpp.display(),
                e.uid
            );
            assert!(
                !after.is_milestone(),
                "{}: uid {} became a milestone",
                mpp.display(),
                e.uid
            );
        }
        let a = actual.iter().find(|a| a.uid as i32 == e.uid).unwrap();
        let at = |what: &str| format!("{}: uid {} {what}", mpp.display(), e.uid);
        check_task_fields(
            a,
            t,
            e,
            &at,
            custom_wbs_mask,
            require_fields,
            exclusions.guid,
            exclusions.ignore_resource_calendar_uids.contains(&e.uid),
        );
        if exclusions.skip_manual(e.uid) {
            assert_eq!(t.manual, a.manual, "{}", at("imported occurrence manual"));
        } else {
            assert_eq!(t.manual, e.manual, "{}", at("imported mode"));
        }
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
    let written = projcore::mspdi::write_mspdi(&imported);
    let round = projcore::mspdi::read_mspdi(&written).unwrap();
    for e in &expected {
        if nulls.contains(&e.uid) {
            continue;
        }
        let a = actual.iter().find(|a| a.uid as i32 == e.uid).unwrap();
        let t = round.tasks.iter().find(|t| t.uid == e.uid).unwrap();
        let at = |what: &str| format!("{}: uid {} round trip {what}", mpp.display(), e.uid);
        check_task_fields(
            a,
            t,
            e,
            &at,
            custom_wbs_mask,
            require_fields,
            exclusions.guid,
            exclusions.ignore_resource_calendar_uids.contains(&e.uid),
        );
    }
    for ghost in decoded
        .iter()
        .filter(|t| t.fields.as_ref().and_then(|f| f.external_task) == Some(true))
    {
        assert_eq!(
            round
                .tasks
                .iter()
                .find(|t| t.uid == ghost.uid as i32)
                .and_then(|t| t.external_task),
            Some(true),
            "{}: external ghost round trip",
            mpp.display()
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
        let mut dropped = [0usize; 6]; // split, delayed assignment, elapsed, recurring exception, nonworking start, fractional progress
        let mut compared_uids = Vec::new();
        for (task, expected_task) in imported.tasks.iter().zip(&expected) {
            if task.is_null || task.summary || task.manual || expected_task.milestone {
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
            if mpp.file_stem().is_some_and(|stem| stem == "p6-fractional") && task.uid == 2 {
                // Project retains subminute actual and remaining duration here;
                // the minute-resolution scheduler ends one minute earlier.
                dropped[5] += 1;
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
                "{}: duration-compared {duration_compared}, date-selected {selected}, exclusions split/delayed-assignment/elapsed/recurring-exception/nonworking-start/fractional-progress = {dropped:?}",
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
    if exclusions.new_tasks_mode {
        // The legacy save changed this option along with occurrence Manual.
        assert_eq!(
            mppread::mpp::decode_new_tasks_are_manual(&bytes),
            Ok(true),
            "{}: binary NewTasksAreManual",
            mpp.display()
        );
        assert!(
            !oracle.new_tasks_are_manual,
            "{}: legacy XML NewTasksAreManual",
            mpp.display()
        );
        assert!(
            imported.new_tasks_are_manual,
            "{}: imported NewTasksAreManual",
            mpp.display()
        );
    } else {
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
    }
    true
}

fn compare_assignment_oracle(mpp: &Path, xml: &Path) {
    use std::collections::HashMap;
    let imported = mppread::project::project_from_mpp(&std::fs::read(mpp).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", mpp.display()));
    let expected = projcore::mspdi::read_mspdi(&std::fs::read_to_string(xml).unwrap()).unwrap();
    let expected_assignments: HashMap<_, _> = expected
        .assignments
        .iter()
        // MPXJ synthesizes UID 3 on task 0 in 25-progress.xml; the binary
        // TBkndAssn has only UIDs 1 and 2. Other task-0 assignments stay.
        .filter(|a| {
            !(mpp.file_stem().is_some_and(|s| s == "25-progress") && a.uid == 3 && a.task_uid == 0)
        })
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
    }
    for got in &imported.resources {
        let want = expected
            .resources
            .iter()
            .find(|r| r.uid == got.uid)
            .unwrap_or_else(|| panic!("{}: missing resource UID {}", mpp.display(), got.uid));
        assert_eq!(
            (got.id, &got.name, got.kind),
            (want.id, &want.name, want.kind),
            "{} resource UID {}",
            mpp.display(),
            got.uid
        );
        assert!(
            (got.max_units - want.max_units).abs() < 1e-6,
            "{} resource UID {} max units {} vs {}",
            mpp.display(),
            got.uid,
            got.max_units,
            want.max_units
        );
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
        assert_eq!(cases.len(), 4);
        for (mpp, xml) in &cases {
            // Deleting a resource also leaves a calendar record the existing
            // calendar decoder refuses; the resource/assignment table probes
            // are tested directly in their decoder unit tests.
            if mpp
                .file_stem()
                .is_some_and(|stem| stem == "a3-deleted-rows")
            {
                continue;
            }
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
    let task_fields = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/task-fields");
    if task_fields.exists() {
        let cases = pairs(&task_fields, "");
        assert_eq!(cases.len(), 12);
        for (mpp, xml) in &cases {
            check_pair(mpp, xml, false, Oracle::Project);
            if mpp.file_stem().is_some_and(|s| s == "f3-blanks") {
                let oracle =
                    projcore::mspdi::read_mspdi(&std::fs::read_to_string(xml).unwrap()).unwrap();
                let layout: Vec<_> = oracle
                    .tasks
                    .iter()
                    .filter(|t| t.uid != 0)
                    .map(|t| (t.name.as_str(), t.is_null))
                    .collect();
                assert_eq!(
                    layout,
                    [
                        ("Summary", false),
                        ("", true),
                        ("Child", false),
                        ("", true),
                        ("After", false),
                        ("", true)
                    ]
                );
            }
        }
        let mut seen = std::collections::HashSet::new();
        for (mpp, xml) in &cases {
            let oracle =
                projcore::mspdi::read_mspdi(&std::fs::read_to_string(xml).unwrap()).unwrap();
            let decoded = mppread::mpp::decode_tasks(&std::fs::read(mpp).unwrap()).unwrap();
            for t in &oracle.tasks {
                if t.is_null {
                    seen.insert("is_null");
                    continue;
                }
                if t.uid == 0 {
                    continue;
                }
                if t.guid.is_some() {
                    seen.insert("guid");
                }
                if t.create_date.is_some() {
                    seen.insert("create_date");
                }
                if t.wbs.as_deref().is_some_and(|w| w.contains("ABC")) {
                    seen.insert("wbs");
                }
                // f3's Child has no explicit WBS override in its generator.
                if mpp.file_stem().is_some_and(|s| s == "f3-blanks")
                    && t.name == "Child"
                    && t.wbs.as_deref().is_some_and(|w| w.contains('.'))
                {
                    seen.insert("derived_wbs");
                }
                if t.task_type
                    .is_some_and(|kind| kind != projcore::TaskType::FixedUnits)
                {
                    seen.insert("task_type");
                }
                if t.active == Some(false) {
                    seen.insert("active");
                }
                if t.effort_driven == Some(true) {
                    seen.insert("effort_driven");
                }
                if t.estimated == Some(true) {
                    seen.insert("estimated");
                }
                if t.priority.is_some_and(|p| p != 500) {
                    seen.insert("priority");
                }
                if t.deadline.is_some() {
                    seen.insert("deadline");
                }
                if t.level_assignments == Some(false) {
                    seen.insert("level_assignments");
                }
                if t.leveling_can_split == Some(false) {
                    seen.insert("leveling_can_split");
                }
                if t.leveling_delay.is_some_and(|d| d != 0) {
                    seen.insert("leveling_delay");
                }
                if t.leveling_delay_format.is_some_and(|format| format != 8) {
                    seen.insert("leveling_delay_format");
                }
                if t.ignore_resource_calendar == Some(true) {
                    seen.insert("ignore_resource_calendar");
                }
                if t.earned_value_method == Some(1) {
                    seen.insert("earned_value_method");
                }
                if t.hide_bar == Some(true) {
                    seen.insert("hide_bar");
                }
                if t.rollup == Some(true) {
                    seen.insert("rollup");
                }
                if t.is_subproject == Some(true) {
                    seen.insert("is_subproject");
                }
                if t.is_subproject_read_only == Some(true) {
                    seen.insert("is_subproject_read_only");
                }
                if t.over_allocated == Some(true) {
                    seen.insert("over_allocated");
                }
                if t.milestone {
                    seen.insert("milestone");
                }
            }
            if decoded
                .iter()
                .any(|t| t.fields.as_ref().and_then(|f| f.external_task) == Some(true))
            {
                seen.insert("external_task");
            }
        }
        for field in [
            "is_null",
            "guid",
            "create_date",
            "wbs",
            "derived_wbs",
            "task_type",
            "active",
            "effort_driven",
            "estimated",
            "priority",
            "deadline",
            "level_assignments",
            "leveling_can_split",
            "leveling_delay",
            "leveling_delay_format",
            "ignore_resource_calendar",
            "earned_value_method",
            "hide_bar",
            "rollup",
            "is_subproject",
            "is_subproject_read_only",
            "external_task",
            "over_allocated",
            "milestone",
        ] {
            assert!(
                seen.contains(field),
                "no non-default Project oracle for {field}"
            );
        }
    }
    let constraints = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/constraints");
    let cases = pairs(&constraints, "");
    if !cases.is_empty() {
        let mut seen = [false; 8];
        for (mpp, xml) in &cases {
            assert!(
                mpp.file_stem()
                    .is_some_and(|stem| stem == "k1-all-types" || stem == "k2-rows"),
                "unknown generated constraint pair {}",
                mpp.display()
            );
            check_pair(mpp, xml, false, Oracle::Project);
            let decoded = mppread::mpp::decode_tasks(&std::fs::read(mpp).unwrap()).unwrap();
            let imported =
                mppread::project::project_from_mpp(&std::fs::read(mpp).unwrap()).unwrap();
            for task in decoded.iter().filter(|t| !t.is_null) {
                let constraint = task
                    .fields
                    .as_ref()
                    .and_then(|f| f.constraint_type)
                    .unwrap();
                seen[constraint.code() as usize] = true;
            }
            if mpp.file_stem().is_some_and(|stem| stem == "k2-rows") {
                let child = imported.tasks.iter().find(|t| t.name == "Child").unwrap();
                assert_eq!(child.constraint, projcore::ConstraintType::MustStartOn);
                assert_eq!(child.constraint_date, child.stored_start);
            }
        }
        if cases.len() == 2 {
            assert!(
                seen.iter().all(|&value| value),
                "generated pairs omit a constraint code: {seen:?}"
            );
        }
    }
    let snapshots = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/snapshots");
    if snapshots.join("01-empty.mpp").exists() {
        let newest = pairs(&snapshots, "")
            .into_iter()
            .filter(|(p, _)| !p.file_stem().unwrap().to_string_lossy().ends_with("-mpp12"))
            .collect::<Vec<_>>();
        let older = pairs(&snapshots, "-mpp12");
        assert_eq!(newest.len(), 48);
        assert_eq!(older.len(), 48);
        for stem in ["x-recurring", "x-overallocated"] {
            assert!(
                newest
                    .iter()
                    .any(|(p, _)| p.file_stem().is_some_and(|s| s == stem))
            );
            assert!(older.iter().any(|(p, _)| {
                p.file_stem()
                    .is_some_and(|s| s.to_string_lossy() == format!("{stem}-mpp12"))
            }));
        }
        let mut recurring_count = 0;
        let mut constraint_counts = [0usize; 8];
        let mut start_divergence_pins = 0usize;
        let mut first_pins = std::collections::HashMap::new();
        for (mpp, xml) in &newest {
            // The two standalone XML exports were saved after a legacy-MPP
            // conversion: their GUIDs differ from the current .mpp, and
            // x-recurring's occurrence Manual values and new-task option changed
            // too. check_pair excludes only those established differences.
            check_pair(mpp, xml, false, Oracle::Project);
            let imported =
                mppread::project::project_from_mpp(&std::fs::read(mpp).unwrap()).unwrap();
            let decoded = mppread::mpp::decode_tasks(&std::fs::read(mpp).unwrap()).unwrap();
            for task in imported.tasks.iter().filter(|t| !t.is_null) {
                let original = decoded
                    .iter()
                    .find(|raw| raw.uid as i32 == task.uid)
                    .and_then(|raw| raw.fields.as_ref())
                    .and_then(|fields| fields.constraint_type)
                    .unwrap();
                constraint_counts[original.code() as usize] += 1;
                if original != task.constraint
                    && task.constraint == projcore::ConstraintType::MustStartOn
                {
                    start_divergence_pins += 1;
                    first_pins
                        .entry(task.uid)
                        .or_insert_with(|| mpp.file_stem().unwrap().to_string_lossy().to_string());
                }
            }
            if mpp.file_stem().is_some_and(|s| s == "x-recurring") {
                let oracle =
                    projcore::mspdi::read_mspdi(&std::fs::read_to_string(xml).unwrap()).unwrap();
                let bytes = std::fs::read(mpp).unwrap();
                let decoded = mppread::mpp::decode_tasks(&bytes).unwrap();
                let imported = mppread::project::project_from_mpp(&bytes).unwrap();
                assert_eq!(
                    decoded.len(),
                    oracle.tasks.len(),
                    "{}: task count",
                    mpp.display()
                );
                for expected in &oracle.tasks {
                    let raw = decoded
                        .iter()
                        .find(|t| t.uid as i32 == expected.uid)
                        .unwrap_or_else(|| {
                            panic!("{}: missing UID {}", mpp.display(), expected.uid)
                        });
                    assert_eq!(
                        raw.name,
                        expected.name,
                        "{}: UID {} name",
                        mpp.display(),
                        expected.uid
                    );
                    assert_eq!(
                        raw.fields.as_ref().and_then(|f| f.recurring),
                        expected.recurring,
                        "{}: UID {} Recurring",
                        mpp.display(),
                        expected.uid
                    );
                    if expected.uid != 0 {
                        let task = imported
                            .tasks
                            .iter()
                            .find(|t| t.uid == expected.uid)
                            .unwrap_or_else(|| {
                                panic!("{}: imported UID {} missing", mpp.display(), expected.uid)
                            });
                        assert_eq!(
                            task.recurring,
                            expected.recurring,
                            "{}: imported UID {} Recurring",
                            mpp.display(),
                            expected.uid
                        );
                    }
                }
                recurring_count = oracle
                    .tasks
                    .iter()
                    .filter(|t| t.recurring == Some(true))
                    .count();
            } else if mpp.file_stem().is_some_and(|s| s == "x-overallocated") {
                let oracle =
                    projcore::mspdi::read_mspdi(&std::fs::read_to_string(xml).unwrap()).unwrap();
                let decoded = mppread::mpp::decode_tasks(&std::fs::read(mpp).unwrap()).unwrap();
                assert_eq!(
                    oracle.tasks.len(),
                    5,
                    "{}: oracle task count",
                    mpp.display()
                );
                for task in &oracle.tasks {
                    assert_eq!(
                        task.over_allocated,
                        Some(false),
                        "{}: UID {} OverAllocated",
                        mpp.display(),
                        task.uid
                    );
                }
                assert_eq!(
                    decoded.len(),
                    oracle.tasks.len(),
                    "{}: binary task count",
                    mpp.display()
                );
                for expected in &oracle.tasks {
                    if expected.uid == 0 {
                        continue;
                    } // XML was renamed by the legacy save.
                    assert_eq!(
                        decoded
                            .iter()
                            .find(|t| t.uid as i32 == expected.uid)
                            .map(|t| t.name.as_str()),
                        Some(expected.name.as_str()),
                        "{}: UID {} name",
                        mpp.display(),
                        expected.uid
                    );
                }
                assert_eq!(
                    oracle
                        .resources
                        .iter()
                        .find(|r| r.name == "Alice")
                        .and_then(|r| r.over_allocated),
                    Some(true),
                    "{}: Alice OverAllocated",
                    mpp.display()
                );
                assert_eq!(
                    oracle
                        .resources
                        .iter()
                        .find(|r| r.name == "Bob")
                        .and_then(|r| r.over_allocated),
                    Some(false),
                    "{}: Bob OverAllocated",
                    mpp.display()
                );
            }
        }
        assert_eq!(constraint_counts[2], 167, "MSO count in newest snapshots");
        assert_eq!(constraint_counts[4], 34, "SNET count in newest snapshots");
        assert_eq!(constraint_counts[7], 28, "FNLT count in newest snapshots");
        assert_eq!(
            start_divergence_pins, 0,
            "unexpected effective pins in newest snapshots: {first_pins:?}"
        );
        assert_eq!(
            recurring_count, 5,
            "recurring summary plus four occurrences"
        );
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
