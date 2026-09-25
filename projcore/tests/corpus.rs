//! Corpus conformance: read every generated MSPDI file, run the CPM scheduler,
//! and assert the computed Start/Finish, total slack and critical flag match
//! the oracle values embedded in each file (`corpus/mspdi/*.xml`, produced by
//! `corpus/tools/gen_mspdi_corpus.py`).
//!
//! Every embedded value in files 01-18 was checked against Project 2021 by
//! `corpus/tools/verify_mspdi_project.py` (issue #74), which has Project
//! schedule a copy of each file with the oracle elements removed. It also
//! reproduces the owner's earlier manual runs of files 05 and 14 (#53),
//! 16 (#58), 17 (#60) and 18 (#59). The exceptions are file 19 (issue #77),
//! whose manual-task slack and critical flags are hand-derived from our
//! scheduler, and file 20 (issue #80), whose task fields and blank row are
//! ours; neither is verified in Project yet. Blank rows carry no oracle. Slack invariants below also check
//! properties that do not depend on the embedded expectations.

use projcore::mspdi::{read_mspdi, write_mspdi};
use projcore::schedule::{level, schedule};
use projcore::yppx::{read_yppx, write_yppx};

fn corpus_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mspdi")
}

fn mspdi_files() -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(corpus_dir())
        .expect("corpus/mspdi should exist (run gen_mspdi_corpus.py)")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "xml").unwrap_or(false))
        .collect();
    v.sort();
    v
}

#[test]
fn every_file_parses_and_schedules() {
    let files = mspdi_files();
    assert!(
        files.len() >= 20,
        "expected the full seed corpus, got {}",
        files.len()
    );
    for path in files {
        let xml = std::fs::read_to_string(&path).unwrap();
        let proj = read_mspdi(&xml).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(!proj.tasks.is_empty(), "{}: no tasks", path.display());
        let _ = schedule(&proj); // must not panic
    }
}

#[test]
fn fnlt_conflict_matches_project_2021_negative_slack() {
    let xml =
        std::fs::read_to_string(corpus_dir().join("17-constraint-fnlt-conflict.xml")).unwrap();
    let proj = read_mspdi(&xml).unwrap();
    assert!(proj.honor_constraints);
    let sched = schedule(&proj);
    for uid in [1, 2] {
        let r = sched.get(uid).unwrap();
        assert_eq!(r.total_slack_min, -2400);
        assert!(r.critical);
    }
}

#[test]
fn summary_fixture_exports_phase_row() {
    let xml = std::fs::read_to_string(corpus_dir().join("10-summary.xml")).unwrap();
    let proj = read_mspdi(&xml).unwrap();
    let md = projcore::gantt::to_markdown(&proj, &schedule(&proj));
    let rows: Vec<_> = md
        .lines()
        .filter(|line| line.starts_with("| "))
        .skip(1)
        .collect();
    assert_eq!(
        rows,
        [
            "| **Phase** | 2026-03-02 08:00:00 | 2026-03-03 17:00:00 | 2d | 0d | 0d | ✓ |",
            "| A | 2026-03-02 08:00:00 | 2026-03-02 17:00:00 | 1d | 0d | 0d | ✓ |",
            "| B | 2026-03-03 08:00:00 | 2026-03-03 17:00:00 | 1d | 0d | 0d | ✓ |",
        ]
    );
}

#[test]
fn every_corpus_project_has_a_critical_leaf() {
    for path in mspdi_files() {
        let proj = read_mspdi(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let sched = schedule(&proj);
        assert!(
            proj.tasks
                .iter()
                .filter(|t| !t.summary && !t.is_null)
                .any(|t| sched.get(t.uid).unwrap().critical),
            "{}: no critical leaf",
            path.display()
        );
    }
}

#[test]
fn every_corpus_finishing_leaf_has_nonpositive_slack() {
    for path in mspdi_files() {
        let proj = read_mspdi(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let sched = schedule(&proj);
        for t in proj.tasks.iter().filter(|t| !t.summary && !t.is_null) {
            let r = sched.get(t.uid).unwrap();
            if r.early_finish == sched.project_finish {
                assert!(
                    r.total_slack_min <= 0,
                    "{}: finishing task {} has {} minutes slack",
                    path.display(),
                    t.uid,
                    r.total_slack_min
                );
            }
        }
    }
}

#[test]
fn sf_fixture_matches_project_2021_slack_and_finish_instant() {
    let xml = std::fs::read_to_string(corpus_dir().join("05-link-sf.xml")).unwrap();
    let proj = read_mspdi(&xml).unwrap();
    let sched = schedule(&proj);
    let a = sched.get(1).unwrap();
    let b = sched.get(2).unwrap();
    assert_eq!(a.total_slack_min, 0);
    assert!(a.critical);
    assert_eq!(b.total_slack_min, 960);
    assert_eq!(b.early_start.to_mspdi(), "2026-03-03T08:00:00");
    assert_eq!(b.early_finish.to_mspdi(), "2026-03-04T08:00:00");
    let leveled = level(&proj);
    for t in &proj.tasks {
        let r = sched.get(t.uid).unwrap();
        assert_eq!(leveled.start(t.uid), Some(r.early_start));
        assert_eq!(leveled.finish(t.uid), Some(r.early_finish));
    }
}

#[test]
fn tasks_and_resources_round_trip_through_mspdi_and_yppx() {
    let files = mspdi_files();
    assert!(
        files
            .iter()
            .any(|p| p.file_name().unwrap() == "13-resource-fields.xml")
    );
    assert!(
        files
            .iter()
            .any(|p| p.file_name().unwrap() == "20-task-fields.xml")
    );
    for path in files {
        let xml = std::fs::read_to_string(&path).unwrap();
        let proj = read_mspdi(&xml).unwrap();
        let xml_back = read_mspdi(&write_mspdi(&proj)).unwrap();
        // Every task field the model holds, including #80's stored fields.
        assert_eq!(
            xml_back.tasks,
            proj.tasks,
            "{}: MSPDI tasks changed",
            path.display()
        );
        assert_eq!(
            xml_back.calendars,
            proj.calendars,
            "{}: MSPDI calendars changed",
            path.display()
        );
        assert_eq!(
            xml_back.resources,
            proj.resources,
            "{}: MSPDI resources changed",
            path.display()
        );
        let package_back = read_yppx(&write_yppx(&proj)).unwrap();
        assert_eq!(
            package_back.tasks,
            proj.tasks,
            "{}: .yppx tasks changed",
            path.display()
        );
        assert_eq!(
            package_back.calendars,
            proj.calendars,
            "{}: .yppx calendars changed",
            path.display()
        );
        assert_eq!(
            package_back.resources,
            proj.resources,
            "{}: .yppx resources changed",
            path.display()
        );
    }
}

#[test]
fn scheduler_matches_embedded_oracle() {
    for path in mspdi_files() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let xml = std::fs::read_to_string(&path).unwrap();
        let proj = read_mspdi(&xml).unwrap();
        let sched = schedule(&proj);

        for t in &proj.tasks {
            if t.is_null {
                assert!(
                    sched.get(t.uid).is_none(),
                    "{name}: blank row {} scheduled",
                    t.uid
                );
                continue;
            }
            let r = sched
                .get(t.uid)
                .unwrap_or_else(|| panic!("{name}: task {} not scheduled", t.uid));
            if let Some(exp) = t.stored_start {
                assert_eq!(
                    r.early_start.to_mspdi(),
                    exp.to_mspdi(),
                    "{name}: task {} '{}' start — CPM disagrees with oracle",
                    t.uid,
                    t.name
                );
            }
            if let Some(exp) = t.stored_finish {
                assert_eq!(
                    r.early_finish.to_mspdi(),
                    exp.to_mspdi(),
                    "{name}: task {} '{}' finish — CPM disagrees with oracle",
                    t.uid,
                    t.name
                );
            }
        }
    }
}

/// Text of the first `<tag>…</tag>` in `xml`, if any.
fn element<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let len = xml[start..].find(&format!("</{tag}>"))?;
    Some(&xml[start..start + len])
}

/// Project 2021's TotalSlack (tenths of a minute) and Critical for every task
/// but blank rows, read from the raw XML: `read_mspdi` deliberately ignores
/// both.
fn embedded_slack_oracle(name: &str, xml: &str) -> Vec<(i32, i64, bool)> {
    xml.split("<Task>")
        .skip(1)
        .filter(|task| element(task, "IsNull") != Some("1"))
        .map(|task| {
            let field =
                |tag| element(task, tag).unwrap_or_else(|| panic!("{name}: a task has no <{tag}>"));
            let uid = field("UID").parse().unwrap();
            let slack: i64 = field("TotalSlack").parse().unwrap();
            let critical = match field("Critical") {
                "0" => false,
                "1" => true,
                other => panic!("{name}: task {uid} has <Critical>{other}</Critical>"),
            };
            (uid, slack / 10, critical)
        })
        .collect()
}

#[test]
fn scheduler_matches_embedded_project_slack_and_critical() {
    for path in mspdi_files() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let xml = std::fs::read_to_string(&path).unwrap();
        let proj = read_mspdi(&xml).unwrap();
        let sched = schedule(&proj);
        let oracle = embedded_slack_oracle(&name, &xml);
        let tasks = proj.tasks.iter().filter(|t| !t.is_null).count();
        assert_eq!(oracle.len(), tasks, "{name}: task count");

        for (uid, slack_min, critical) in oracle {
            let r = sched
                .get(uid)
                .unwrap_or_else(|| panic!("{name}: task {uid} not scheduled"));
            assert_eq!(
                r.total_slack_min, slack_min,
                "{name}: task {uid} total slack — CPM disagrees with Project"
            );
            assert_eq!(
                r.critical, critical,
                "{name}: task {uid} critical — CPM disagrees with Project"
            );
        }
    }
}

/// A save writes TotalSlack and Critical from docxy's own schedule; for the
/// files verified in Project 2021 they are Project's values.
#[test]
fn saved_files_carry_project_slack_and_critical() {
    for path in mspdi_files() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let xml = std::fs::read_to_string(&path).unwrap();
        let proj = read_mspdi(&xml).unwrap();
        let oracle = embedded_slack_oracle(&name, &xml);
        let saved = write_mspdi(&proj);
        assert_eq!(
            embedded_slack_oracle(&name, &saved),
            oracle,
            "{name}: MSPDI"
        );
        let package = read_yppx(&write_yppx(&proj)).unwrap();
        assert_eq!(
            embedded_slack_oracle(&name, &write_mspdi(&package)),
            oracle,
            "{name}: .yppx"
        );
    }
}

#[test]
fn task_fields_fixture_keeps_fields_and_a_blank_row() {
    use projcore::{DateTime, TaskType};
    let xml = std::fs::read_to_string(corpus_dir().join("20-task-fields.xml")).unwrap();
    let proj = read_mspdi(&xml).unwrap();
    let types: Vec<_> = proj.tasks.iter().map(|t| t.task_type).collect();
    use TaskType::*;
    assert_eq!(
        types,
        [
            Some(FixedDuration),
            Some(FixedUnits),
            None,
            Some(FixedWork),
            Some(FixedDuration)
        ]
    );
    let active: Vec<_> = proj.tasks.iter().map(|t| t.active).collect();
    assert_eq!(
        active,
        [Some(true), Some(true), None, Some(true), Some(false)]
    );
    let pour = proj.task(4).unwrap();
    assert_eq!(
        pour.deadline,
        Some(DateTime::from_ymd_hm(2026, 3, 20, 17, 0))
    );
    assert_eq!(
        (pour.leveling_delay, pour.leveling_delay_format),
        (Some(4800), Some(7))
    );
    assert_eq!(proj.task(2).unwrap().work_min, Some(960));
    let blank = proj.task(3).unwrap();
    assert!(blank.is_null);
    assert_eq!((blank.id, blank.outline_level), (3, 0));
    assert!(blank.guid.is_some() && blank.create_date.is_some());
    // The blank row is not scheduled, and Pour follows Excavate as if it
    // were not there.
    let sched = schedule(&proj);
    assert!(sched.get(3).is_none());
    assert_eq!(
        sched.get(4).unwrap().early_start,
        DateTime::from_ymd_hm(2026, 3, 4, 8, 0)
    );
    let saved = write_mspdi(&proj);
    assert!(saved.contains("<IsNull>1</IsNull>"));
    assert_eq!(saved.matches("<IsNull>").count(), 1);
}

/// The full native pipeline on real files: MSPDI → .yppx package → back → the
/// scheduler still reproduces the embedded oracle, proving write_mspdi and the
/// OPC container are lossless for everything the scheduler depends on.
#[test]
fn yppx_package_round_trip_preserves_schedule() {
    for path in mspdi_files() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let xml = std::fs::read_to_string(&path).unwrap();
        let proj = read_mspdi(&xml).unwrap();

        let bytes = write_yppx(&proj);
        assert_eq!(&bytes[..2], b"PK", "{name}: .yppx is not a ZIP");
        let back = read_yppx(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));

        assert_eq!(
            back.tasks.len(),
            proj.tasks.len(),
            "{name}: task count changed"
        );
        let sched = schedule(&back);
        for t in back.tasks.iter().filter(|t| !t.is_null) {
            let r = sched.get(t.uid).unwrap();
            if let Some(exp) = t.stored_start {
                assert_eq!(
                    r.early_start.to_mspdi(),
                    exp.to_mspdi(),
                    "{name}: task {} start drifted after .yppx round-trip",
                    t.uid
                );
            }
            if let Some(exp) = t.stored_finish {
                assert_eq!(
                    r.early_finish.to_mspdi(),
                    exp.to_mspdi(),
                    "{name}: task {} finish drifted after .yppx round-trip",
                    t.uid
                );
            }
        }
    }
}

#[test]
fn baselines_round_trip_through_mspdi_and_yppx() {
    let files = mspdi_files();
    assert!(
        files
            .iter()
            .any(|p| p.file_name().unwrap() == "15-baseline-slots.xml")
    );
    for path in files {
        let proj = read_mspdi(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let xml_back = read_mspdi(&write_mspdi(&proj)).unwrap();
        let package_back = read_yppx(&write_yppx(&proj)).unwrap();
        for (kind, back) in [("MSPDI", xml_back), (".yppx", package_back)] {
            assert_eq!(back.tasks.len(), proj.tasks.len());
            for (expected, actual) in proj.tasks.iter().zip(&back.tasks) {
                assert_eq!(actual.uid, expected.uid);
                assert_eq!(
                    actual.baselines,
                    expected.baselines,
                    "{}: {kind} task {} baselines changed",
                    path.display(),
                    expected.uid
                );
            }
        }
    }
}

#[test]
fn manual_task_mode_and_dates_survive_mspdi_and_yppx() {
    let files = mspdi_files();
    assert!(
        files
            .iter()
            .any(|p| p.file_name().unwrap() == "19-manual-tasks.xml")
    );
    for path in files {
        let proj = read_mspdi(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let xml_back = read_mspdi(&write_mspdi(&proj)).unwrap();
        let package_back = read_yppx(&write_yppx(&proj)).unwrap();
        for (kind, back) in [("MSPDI", xml_back), (".yppx", package_back)] {
            assert_eq!(
                back.new_tasks_are_manual,
                proj.new_tasks_are_manual,
                "{}: {kind} NewTasksAreManual changed",
                path.display()
            );
            assert_eq!(back.tasks.len(), proj.tasks.len());
            for (expected, actual) in proj.tasks.iter().zip(&back.tasks) {
                assert_eq!(
                    (
                        actual.manual,
                        actual.manual_start,
                        actual.manual_finish,
                        actual.manual_duration_min
                    ),
                    (
                        expected.manual,
                        expected.manual_start,
                        expected.manual_finish,
                        expected.manual_duration_min
                    ),
                    "{}: {kind} task {} manual fields changed",
                    path.display(),
                    expected.uid
                );
            }
        }
    }
}

#[test]
fn manual_fixture_pins_tasks_and_flags_the_violated_link() {
    use projcore::DateTime;
    let proj =
        read_mspdi(&std::fs::read_to_string(corpus_dir().join("19-manual-tasks.xml")).unwrap())
            .unwrap();
    assert!(proj.new_tasks_are_manual);
    let manual: Vec<_> = proj
        .tasks
        .iter()
        .filter(|t| t.manual)
        .map(|t| t.uid)
        .collect();
    assert_eq!(manual, vec![3, 4]);
    let review = proj.task(3).unwrap();
    assert_eq!(
        review.manual_start,
        Some(DateTime::from_ymd_hm(2026, 3, 3, 8, 0))
    );
    assert_eq!(review.manual_duration_min, Some(480));
    let sched = schedule(&proj);
    // Design finishes Wednesday; Review is pinned on Tuesday, two days early.
    assert_eq!(sched.get(3).unwrap().total_slack_min, -960);
    // Vendor, pinned after the link allows, drives Build and the finish.
    assert_eq!(sched.get(4).unwrap().total_slack_min, 0);
    // Leveling (no resources) keeps every pinned date.
    let leveled = level(&proj);
    for uid in [3, 4] {
        assert_eq!(
            leveled.start(uid),
            Some(sched.get(uid).unwrap().early_start)
        );
    }
}

#[test]
fn baseline_fixture_records_different_plans_and_missing_duration() {
    use projcore::{Baseline, DateTime};
    let proj =
        read_mspdi(&std::fs::read_to_string(corpus_dir().join("15-baseline-slots.xml")).unwrap())
            .unwrap();
    let task = &proj.tasks[0];
    assert_eq!(task.duration_min, 960);
    assert_eq!(
        task.baselines,
        vec![
            Baseline {
                number: 0,
                start: Some(DateTime::from_ymd_hm(2026, 3, 4, 8, 0)),
                finish: Some(DateTime::from_ymd_hm(2026, 3, 6, 17, 0)),
                duration_min: Some(1440)
            },
            Baseline {
                number: 1,
                start: Some(DateTime::from_ymd_hm(2026, 3, 9, 8, 0)),
                finish: Some(DateTime::from_ymd_hm(2026, 3, 13, 17, 0)),
                duration_min: Some(2400)
            },
            Baseline {
                number: 2,
                start: Some(DateTime::from_ymd_hm(2026, 3, 16, 8, 0)),
                finish: Some(DateTime::from_ymd_hm(2026, 3, 17, 17, 0)),
                duration_min: None
            },
        ]
    );
}
