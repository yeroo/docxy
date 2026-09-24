//! Validates the .mpp decoders against real sample files when they're present
//! locally (they're git-ignored — see corpus/mpp/README). In CI, where the
//! binaries are absent, this test skips gracefully.

fn corpus(path: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(path)
}

#[test]
fn decodes_real_mpp_task_names_when_present() {
    // (path, first task name, min count, first start prefix, links decode?).
    // The Azure plan is the newest MPP generation: names + dates decode, but its
    // link/outline tables use a layout not yet reversed, so links stay off.
    let cases = [
        (
            "corpus/mpp/projectlibre-construction.mpp",
            "Commercial Construction",
            100usize,
            "2000-01-03",
            true,
        ),
        (
            "corpus/mpp/saswat-part1.mpp",
            "Project1",
            10,
            "2020-01-02",
            true,
        ),
        (
            "corpus/mpp/msproject2003-deployment.mpp",
            "Microsoft Office Project 2003 Deployment",
            300,
            "2003-09-15",
            true,
        ),
        (
            "corpus/mpp/new-product.mpp",
            "Product #23 Development",
            40,
            "2004-07-19",
            true,
        ),
        (
            "corpus/mpp/azure-analytics.mpp",
            "Advanced Analytics Project",
            20,
            "2017-08-17",
            false,
        ),
    ];
    let mut checked = 0;
    for (path, first, min, first_start, has_links) in cases {
        let Ok(bytes) = std::fs::read(corpus(path)) else {
            continue;
        };
        // container + metadata must parse
        let info = mppread::read_mpp(&bytes).expect("read_mpp");
        assert!(!info.streams.is_empty(), "{path}: no streams");
        // task names decode in order
        let tasks = mppread::mpp::tasks(&bytes);
        assert!(
            tasks.len() >= min,
            "{path}: {} tasks (< {min})",
            tasks.len()
        );
        assert_eq!(tasks[0].name, first, "{path}: first task name");
        // dates decode: the first task's start matches, and every dated task
        // has start ≤ finish (the self-validating invariant the detector uses).
        assert_eq!(
            tasks[0].start.as_deref().map(|s| &s[..10]),
            Some(first_start),
            "{path}: first task start"
        );
        let dated = tasks.iter().filter(|t| t.start.is_some()).count();
        assert!(
            dated * 5 >= tasks.len() * 4,
            "{path}: only {dated}/{} dated",
            tasks.len()
        );
        for t in &tasks {
            if let (Some(s), Some(f)) = (&t.start, &t.finish) {
                assert!(s <= f, "{path}: {} start {s} > finish {f}", t.name);
            }
        }
        // Outline levels, when detected, form a valid tree: start at 1 and
        // deepen by at most one level per row (MS Project's WBS rule).
        if tasks[0].outline_level.is_some() {
            assert_eq!(tasks[0].outline_level, Some(1), "{path}: root not level 1");
            for w in tasks.windows(2) {
                if let (Some(a), Some(b)) = (w[0].outline_level, w[1].outline_level) {
                    assert!(
                        b <= a + 1,
                        "{path}: outline jumps {a}->{b} at {}",
                        w[1].name
                    );
                }
            }
        }
        // Links decode and are (nearly) self-consistent: the vast majority of FS
        // links have the successor starting on/after the predecessor finishes —
        // the oracle the decoder fits to (≥90%; a few genuine outliers exist in
        // real plans from manual date edits or constraints).
        let links: usize = tasks.iter().map(|t| t.predecessors.len()).sum();
        assert_eq!(links > 0, has_links, "{path}: link decode expectation");
        let (mut fs, mut fs_ok) = (0usize, 0usize);
        let by_uid: std::collections::HashMap<_, _> = tasks.iter().map(|t| (t.uid, t)).collect();
        for (i, t) in tasks.iter().enumerate() {
            for p in &t.predecessors {
                let predecessor = by_uid.get(&p.pred_uid);
                assert!(
                    predecessor.is_some_and(|task| !std::ptr::eq(*task, &tasks[i])),
                    "{path}: bad predecessor UID"
                );
                if p.kind == 1 {
                    if let (Some(pf), Some(ss)) = (&predecessor.unwrap().finish, &t.start) {
                        fs += 1;
                        if ss[..10] >= pf[..10] {
                            fs_ok += 1;
                        }
                    }
                }
            }
        }
        assert!(
            fs_ok * 10 >= fs * 9,
            "{path}: only {fs_ok}/{fs} FS links respect dates"
        );
        checked += 1;
    }
    eprintln!("real .mpp files validated: {checked}");
}

#[test]
fn indexed_legacy_imports_when_present() {
    for (path, first, date) in [
        (
            "corpus/mpp/projectlibre-construction.mpp",
            "Commercial Construction",
            "2000-01-03",
        ),
        (
            "corpus/mpp/msproject2003-deployment.mpp",
            "Microsoft Office Project 2003 Deployment",
            "2003-09-15",
        ),
        (
            "corpus/mpp/new-product.mpp",
            "Product #23 Development",
            "2004-07-19",
        ),
    ] {
        let Ok(bytes) = std::fs::read(corpus(path)) else {
            continue;
        };
        let rows = mppread::mpp::decode_tasks(&bytes).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(rows[0].uid, 0, "{path}");
        assert_eq!(rows[0].name, first, "{path}");
        assert_eq!(
            rows[0].start.as_deref().map(|s| &s[..10]),
            Some(date),
            "{path}"
        );
        let imported =
            mppread::project::project_from_mpp(&bytes).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(imported.tasks.len(), rows.len() - 1, "{path}");
        assert!(imported.tasks.iter().all(|t| t.uid != 0), "{path}");
        assert_eq!(imported.tasks[0].outline_level, 1, "{path}");
        if path.ends_with("new-product.mpp") {
            assert_eq!(
                rows[1..3]
                    .iter()
                    .map(|t| (t.id, t.uid, t.name.as_str(), t.outline_level))
                    .collect::<Vec<_>>(),
                [
                    (1, 2, "Begin project", Some(1)),
                    (2, 1, "Design Phase", Some(1)),
                ]
            );
            assert_eq!(
                imported.tasks[..2]
                    .iter()
                    .map(|t| (t.id, t.uid, t.name.as_str(), t.summary))
                    .collect::<Vec<_>>(),
                [(1, 2, "Begin project", false), (2, 1, "Design Phase", true)]
            );
        }
    }
}

#[test]
fn legacy_dates_stay_correct_without_links_when_present() {
    let Ok(bytes) = std::fs::read(corpus("corpus/mpp/new-product.mpp")) else {
        return;
    };
    let cfb = mppread::Cfb::open(&bytes).unwrap();
    let prefix = cfb
        .paths()
        .into_iter()
        .find(|p| p.ends_with("TBkndTask/FixedMeta"))
        .unwrap()
        .trim_end_matches("FixedMeta")
        .to_string();
    let data = |name: &str| cfb.read_path(&format!("{prefix}{name}")).unwrap();
    let assemble = |fixed_data: Vec<u8>| {
        mppread::write_cfb_tree(&[mppread::Node::Storage(
            "   19",
            vec![mppread::Node::Storage(
                "TBkndTask",
                vec![
                    mppread::Node::Stream("FixedMeta", data("FixedMeta")),
                    mppread::Node::Stream("FixedData", fixed_data),
                    mppread::Node::Stream("VarMeta", data("VarMeta")),
                    mppread::Node::Stream("Var2Data", data("Var2Data")),
                ],
            )],
        )])
    };
    let fixed_data = data("FixedData");
    let stripped = assemble(fixed_data.clone());
    let rows = mppread::mpp::decode_tasks(&stripped).unwrap();
    assert_eq!(rows[0].start.as_deref(), Some("2004-07-19 08:00"));
    let design = rows.iter().find(|t| t.name == "Design Phase").unwrap();
    assert_eq!(design.finish.as_deref(), Some("2004-10-01 17:00"));
    let mut missing_finish = fixed_data;
    missing_finish[24 + 92 + 2..24 + 92 + 4].copy_from_slice(&0xffffu16.to_le_bytes());
    assert!(mppread::mpp::decode_tasks(&assemble(missing_finish)).is_err());
}
