//! Corpus conformance: read every generated MSPDI file, run the CPM scheduler,
//! and assert the computed Start/Finish match the oracle values embedded in each
//! file (`corpus/mspdi/*.xml`, produced by `corpus/tools/gen_mspdi_corpus.py`).
//!
//! These are hand-derived expectations. The owner checked the SF shapes in
//! files 05 and 14 against Project 2021 (issue #53); the other fixtures have not
//! been independently verified against Project. Slack invariants below also
//! check properties that do not depend on the embedded date expectations.

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
        files.len() >= 14,
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
                .filter(|t| !t.summary)
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
        for t in proj.tasks.iter().filter(|t| !t.summary) {
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
fn resources_round_trip_through_mspdi_and_yppx() {
    let files = mspdi_files();
    assert!(
        files
            .iter()
            .any(|p| p.file_name().unwrap() == "13-resource-fields.xml")
    );
    for path in files {
        let xml = std::fs::read_to_string(&path).unwrap();
        let proj = read_mspdi(&xml).unwrap();
        let xml_back = read_mspdi(&write_mspdi(&proj)).unwrap();
        assert_eq!(
            xml_back.resources,
            proj.resources,
            "{}: MSPDI resources changed",
            path.display()
        );
        let package_back = read_yppx(&write_yppx(&proj)).unwrap();
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
        for t in &back.tasks {
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
