//! The committed case scripts must parse, and must point at files that exist.
//!
//! Running them needs a desktop session for `PrintWindow`, so a machine with
//! no display cannot check what they *do*. It can still check that they are
//! well formed — which is most of what goes wrong with a script — and that
//! nothing in them reaches outside the harness's own directory for a fixture.

use std::path::{Path, PathBuf};
use uiharness::script::{Action, parse_script};

fn cases_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cases")
}

fn case_files() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(cases_dir())
        .expect("the cases directory is there")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "uit"))
        .collect();
    out.sort();
    out
}

#[test]
fn every_committed_case_parses() {
    let files = case_files();
    assert!(!files.is_empty(), "there are cases to run");
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        let script = parse_script(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(!script.cases.is_empty(), "{} has no cases", path.display());
    }
}

/// Every `open` in a case resolves, and resolves to something under the
/// harness's own tree. A case that reached into the user's documents would
/// pass on the machine it was written on and nowhere else — and would be
/// reading files the harness has no business reading.
#[test]
fn every_opened_file_is_the_harnesss_own_and_is_there() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .unwrap();
    for path in case_files() {
        let text = std::fs::read_to_string(&path).unwrap();
        let script = parse_script(&text).unwrap();
        let base = path.parent().unwrap_or(Path::new("."));
        let mut opened = 0;
        for case in &script.cases {
            for step in &case.steps {
                let Action::Open(rel) = &step.action else {
                    continue;
                };
                opened += 1;
                let full = base.join(rel);
                let full = full.canonicalize().unwrap_or_else(|e| {
                    panic!("{}:{}: {}: {e}", path.display(), step.line, full.display())
                });
                assert!(
                    full.starts_with(&crate_dir),
                    "{}:{} opens {}, which is outside {}",
                    path.display(),
                    step.line,
                    full.display(),
                    crate_dir.display()
                );
            }
        }
        assert!(opened > 0, "{} opens nothing", path.display());
    }
}
