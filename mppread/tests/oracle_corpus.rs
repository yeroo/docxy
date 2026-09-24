//! Compare binary task rows with Microsoft Project's XML export of the same plan.
use std::path::{Path, PathBuf};

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

fn check_pair(mpp: &Path, xml: &Path, may_refuse: bool) -> bool {
    let bytes = std::fs::read(mpp).unwrap();
    let oracle = projcore::mspdi::read_mspdi(&std::fs::read_to_string(xml).unwrap()).unwrap();
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
        .filter(|t| t.uid != 0 && !t.name.is_empty())
        .collect();
    assert_eq!(
        actual.len(),
        expected.len(),
        "{}: task count",
        mpp.display()
    );
    for (a, e) in actual.iter().zip(expected) {
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
            .map(|p| (p.pred_uid as i32, p.kind as i64, p.lag_min))
            .collect();
        let mut want: Vec<_> = e
            .predecessors
            .iter()
            .map(|p| (p.uid, p.link.code(), p.lag_min))
            .collect();
        got.sort();
        want.sort();
        assert_eq!(got, want, "{}: uid {} predecessors", mpp.display(), e.uid);
    }
    assert!(
        mppread::project::project_from_mpp(&bytes).is_ok(),
        "{}: decoded import",
        mpp.display()
    );
    true
}

#[test]
fn project_2021_oracles() {
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
            check_pair(mpp, xml, false);
        }
        let matched = older
            .iter()
            .filter(|(mpp, xml)| check_pair(mpp, xml, true))
            .count();
        eprintln!("MPP12 matched {matched}, refused {}", older.len() - matched);
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
            check_pair(mpp, xml, false);
        }
    }
}
