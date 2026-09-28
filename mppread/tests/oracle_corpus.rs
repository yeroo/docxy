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

fn check_task_fields(
    a: &mppread::mpp::MppTask,
    imported: &projcore::Task,
    e: &projcore::Task,
    at: &dyn Fn(&str) -> String,
    custom_wbs_mask: bool,
    require_fields: bool,
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
    same!(guid);
    if custom_wbs_mask {
        // f8 has no per-task code: preserve the unknown value.
        assert_eq!(f.wbs, None, "{}", at("decoded wbs"));
        assert_eq!(imported.wbs, None, "{}", at("imported wbs"));
    } else {
        same!(wbs);
    }
    same!(task_type);
    same!(active);
    same!(effort_driven);
    same!(estimated);
    same!(priority);
    same!(level_assignments);
    same!(leveling_can_split);
    same!(leveling_delay);
    same!(leveling_delay_format);
    same!(ignore_resource_calendar);
    same!(earned_value_method);
    same!(recurring);
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
    // Snapshot MPP12 pairs may decode through the legacy index without
    // validated task fields; the current Project pairs must have them.
    let require_fields = source == Oracle::Project && !may_refuse;
    let bytes = std::fs::read(mpp).unwrap();
    let xml_text = std::fs::read_to_string(xml).unwrap();
    let oracle = projcore::mspdi::read_mspdi(&xml_text).unwrap();
    let nulls = null_uids(&xml_text);
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
        check_task_fields(a, t, e, &at, custom_wbs_mask, require_fields);
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
    let written = projcore::mspdi::write_mspdi(&imported);
    let round = projcore::mspdi::read_mspdi(&written).unwrap();
    for e in &expected {
        if nulls.contains(&e.uid) {
            continue;
        }
        let a = actual.iter().find(|a| a.uid as i32 == e.uid).unwrap();
        let t = round.tasks.iter().find(|t| t.uid == e.uid).unwrap();
        let at = |what: &str| format!("{}: uid {} round trip {what}", mpp.display(), e.uid);
        check_task_fields(a, t, e, &at, custom_wbs_mask, require_fields);
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
    let task_fields = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/task-fields");
    if task_fields.exists() {
        let cases = pairs(&task_fields, "");
        let expected_count = if task_fields.join("f6-recurring.mpp").exists() {
            11
        } else {
            10
        };
        assert_eq!(cases.len(), expected_count);
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
        for (mpp, xml) in &newest {
            // The two standalone XML exports were saved after a legacy-MPP
            // conversion: their GUIDs differ from the current .mpp, and
            // x-recurring's occurrence Manual values changed too. Compare
            // the stable field under study, not those mismatched properties.
            if mpp.file_stem().is_some_and(|s| s == "x-recurring") {
                let oracle =
                    projcore::mspdi::read_mspdi(&std::fs::read_to_string(xml).unwrap()).unwrap();
                let bytes = std::fs::read(mpp).unwrap();
                let decoded = mppread::mpp::decode_tasks(&bytes).unwrap();
                let imported = mppread::project::project_from_mpp(&bytes).unwrap();
                assert_eq!(decoded.len(), oracle.tasks.len());
                for expected in &oracle.tasks {
                    let raw = decoded
                        .iter()
                        .find(|t| t.uid as i32 == expected.uid)
                        .unwrap();
                    assert_eq!(raw.name, expected.name);
                    assert_eq!(
                        raw.fields.as_ref().and_then(|f| f.recurring),
                        expected.recurring
                    );
                    if expected.uid != 0 {
                        let task = imported
                            .tasks
                            .iter()
                            .find(|t| t.uid == expected.uid)
                            .unwrap();
                        assert_eq!(task.recurring, expected.recurring);
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
                assert_eq!(oracle.tasks.len(), 5);
                assert!(oracle.tasks.iter().all(|t| t.over_allocated == Some(false)));
                assert_eq!(decoded.len(), oracle.tasks.len());
                for expected in &oracle.tasks {
                    if expected.uid == 0 {
                        continue;
                    } // XML was renamed by the legacy save.
                    assert_eq!(
                        decoded
                            .iter()
                            .find(|t| t.uid as i32 == expected.uid)
                            .map(|t| t.name.as_str()),
                        Some(expected.name.as_str())
                    );
                }
                assert_eq!(
                    oracle
                        .resources
                        .iter()
                        .find(|r| r.name == "Alice")
                        .and_then(|r| r.over_allocated),
                    Some(true)
                );
                assert_eq!(
                    oracle
                        .resources
                        .iter()
                        .find(|r| r.name == "Bob")
                        .and_then(|r| r.over_allocated),
                    Some(false)
                );
            } else {
                check_pair(mpp, xml, false, Oracle::Project);
            }
        }
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
}
