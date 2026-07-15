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
            "2000-01-04",
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
            "2003-09-16",
            true,
        ),
        (
            "corpus/mpp/new-product.mpp",
            "Product #23 Development",
            40,
            "2004-07-20",
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
        for (i, t) in tasks.iter().enumerate() {
            for p in &t.predecessors {
                assert!(
                    p.pred < tasks.len() && p.pred != i,
                    "{path}: bad link index"
                );
                if p.kind == 1 {
                    if let (Some(pf), Some(ss)) = (&tasks[p.pred].finish, &t.start) {
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
