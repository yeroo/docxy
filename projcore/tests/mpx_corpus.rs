//! MPX against its MSPDI twin (#309): every `corpus/mpx/*.mpx` (MPXJ
//! 16.8.0's MPX of `corpus/mspdi/<same name>.xml`, see
//! `corpus/mpx/README.md`), in each of the three date orders, read and
//! scheduled, has the twin's tasks in the twin's order with the same names,
//! outline levels and durations, scheduled to the same start and finish.
//!
//! A field is left out of a pair only where the MPX file does not carry
//! what the XML does; `EXCLUDED` says what, for each.

use projcore::model::Project;
use projcore::mpx::read_mpx;
use projcore::mspdi::read_mspdi;
use projcore::schedule::schedule;
use std::path::{Path, PathBuf};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus")
}

/// What a pair is not compared on, and why.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    Dates,
    Duration,
}

const EXCLUDED: &[(&str, Field, &str)] = &[
    (
        "12-calendar-6day",
        Field::Dates,
        "the task's own calendar (SixDay): an MPX task row has no calendar field",
    ),
    (
        "16-24-hour-calendar",
        Field::Dates,
        "the task's own calendar (24 Hours): an MPX task row has no calendar field",
    ),
    (
        "19-manual-tasks",
        Field::Dates,
        "manually scheduled tasks: MPX has no Manual field",
    ),
    (
        "23-derived-calendar",
        Field::Dates,
        "the task's own calendar (Crew): an MPX task row has no calendar field; \
         MPXJ writes Crew only as Alice's resource calendar, and Alice has no assignment",
    ),
    (
        "25-derived-calendar-holiday",
        Field::Dates,
        "the tasks' own calendar (Alice's): an MPX task row has no calendar field",
    ),
    (
        "27-manual-summary",
        Field::Dates,
        "manually scheduled summaries: MPX has no Manual field",
    ),
    (
        "28-work-weeks",
        Field::Dates,
        "the tasks' own calendars (Alice, Bob, Carol): an MPX task row has no calendar field",
    ),
    (
        "28-work-weeks",
        Field::Duration,
        "MPXJ writes these durations in days to two decimals (90 minutes is 0.19d)",
    ),
    (
        "29-milestone-snet-fnet-instants",
        Field::Dates,
        "the constraint times (Wed 12:00, Sat 10:00, ...): MPX constraint dates are whole days",
    ),
    (
        "31-mfo-honor-constraints-off",
        Field::Dates,
        "Honor Constraints off: MPX does not carry the option, so 30 and 31 read to one plan, \
         which schedules as 30 (the option on, Project's default)",
    ),
];

/// Every exclusion names a file that exists and is still needed: a pair it
/// covers that matches without it fails here, so a stale entry goes.
#[test]
fn every_exclusion_is_needed() {
    for (stem, field, reason) in EXCLUDED {
        assert!(!reason.is_empty());
        let path = corpus().join("mpx").join(format!("{stem}.mpx"));
        let mpx = read_mpx(&std::fs::read(&path).unwrap()).unwrap();
        let xml =
            std::fs::read_to_string(corpus().join("mspdi").join(format!("{stem}.xml"))).unwrap();
        let xml = read_mspdi(&xml).unwrap();
        let unexcluded = differences_with(stem, &mpx, &xml, |s, f| excluded(s, f) && f != *field);
        assert!(
            !unexcluded.is_empty(),
            "{stem}: the {field:?} exclusion is not needed"
        );
    }
}

fn excluded(stem: &str, field: Field) -> bool {
    EXCLUDED.iter().any(|(s, f, _)| *s == stem && *f == field)
}

fn mpx_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "mpx"))
        .collect();
    files.sort();
    files
}

/// Each difference between the plan read from `mpx` and its twin, but in
/// the fields `EXCLUDED` leaves out.
fn differences(stem: &str, mpx: &Project, xml: &Project) -> Vec<String> {
    differences_with(stem, mpx, xml, excluded)
}

/// [`differences`] with `skip` deciding what is left out.
fn differences_with(
    stem: &str,
    mpx: &Project,
    xml: &Project,
    skip: impl Fn(&str, Field) -> bool,
) -> Vec<String> {
    let mut out = Vec::new();
    let uids = |p: &Project| p.tasks.iter().map(|t| t.uid).collect::<Vec<_>>();
    if uids(mpx) != uids(xml) {
        out.push(format!("UIDs {:?} != {:?}", uids(mpx), uids(xml)));
        return out;
    }
    let (ms, xs) = (schedule(mpx), schedule(xml));
    for (m, x) in mpx.tasks.iter().zip(&xml.tasks) {
        let uid = m.uid;
        if m.is_null != x.is_null {
            out.push(format!("UID {uid}: blank {} != {}", m.is_null, x.is_null));
            continue;
        }
        if m.is_null {
            continue;
        }
        if m.name != x.name {
            out.push(format!("UID {uid}: name {:?} != {:?}", m.name, x.name));
        }
        if m.outline_level != x.outline_level {
            out.push(format!(
                "UID {uid}: outline level {} != {}",
                m.outline_level, x.outline_level
            ));
        }
        if !skip(stem, Field::Duration) && m.duration_min != x.duration_min {
            out.push(format!(
                "UID {uid}: duration {} != {}",
                m.duration_min, x.duration_min
            ));
        }
        if skip(stem, Field::Dates) {
            continue;
        }
        match (ms.get(uid), xs.get(uid)) {
            (Some(a), Some(b)) => {
                if (a.early_start, a.early_finish) != (b.early_start, b.early_finish) {
                    out.push(format!(
                        "UID {uid}: scheduled {}..{} != {}..{}",
                        a.early_start.to_mspdi(),
                        a.early_finish.to_mspdi(),
                        b.early_start.to_mspdi(),
                        b.early_finish.to_mspdi()
                    ));
                }
            }
            (None, None) => {}
            (a, b) => out.push(format!(
                "UID {uid}: scheduled {} != {}",
                a.is_some(),
                b.is_some()
            )),
        }
    }
    out
}

fn compare_dir(dir: &Path) -> Vec<String> {
    let files = mpx_files(dir);
    assert_eq!(files.len(), 31, "{}: expected 31 MPX files", dir.display());
    let mut report = Vec::new();
    for path in files {
        let stem = path.file_stem().unwrap().to_str().unwrap().to_string();
        let mpx = read_mpx(&std::fs::read(&path).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let xml =
            std::fs::read_to_string(corpus().join("mspdi").join(format!("{stem}.xml"))).unwrap();
        let xml = read_mspdi(&xml).unwrap();
        for d in differences(&stem, &mpx, &xml) {
            report.push(format!("{}: {d}", path.display()));
        }
    }
    report
}

#[test]
fn every_mpx_file_schedules_as_its_mspdi_twin() {
    let mut report = Vec::new();
    for dir in ["mpx", "mpx/dmy", "mpx/ymd"] {
        report.extend(compare_dir(&corpus().join(dir)));
    }
    assert!(report.is_empty(), "\n{}", report.join("\n"));
}
