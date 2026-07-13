//! Validates the .mpp decoders against real sample files when they're present
//! locally (they're git-ignored — see corpus/mpp/README). In CI, where the
//! binaries are absent, this test skips gracefully.

fn corpus(path: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(path)
}

#[test]
fn decodes_real_mpp_task_names_when_present() {
    // (path, first task name, min count, first task's start prefix) for samples.
    let cases = [
        ("corpus/mpp/projectlibre-construction.mpp", "Commercial Construction", 100usize, "2000-01-04"),
        ("corpus/mpp/saswat-part1.mpp", "Project1", 10usize, "2020-01-02"),
    ];
    let mut checked = 0;
    for (path, first, min, first_start) in cases {
        let Ok(bytes) = std::fs::read(corpus(path)) else { continue };
        // container + metadata must parse
        let info = mppread::read_mpp(&bytes).expect("read_mpp");
        assert!(!info.streams.is_empty(), "{path}: no streams");
        // task names decode in order
        let tasks = mppread::mpp::tasks(&bytes);
        assert!(tasks.len() >= min, "{path}: {} tasks (< {min})", tasks.len());
        assert_eq!(tasks[0].name, first, "{path}: first task name");
        // dates decode: the first task's start matches, and every dated task
        // has start ≤ finish (the self-validating invariant the detector uses).
        assert_eq!(
            tasks[0].start.as_deref().map(|s| &s[..10]),
            Some(first_start),
            "{path}: first task start"
        );
        let dated = tasks.iter().filter(|t| t.start.is_some()).count();
        assert!(dated * 5 >= tasks.len() * 4, "{path}: only {dated}/{} dated", tasks.len());
        for t in &tasks {
            if let (Some(s), Some(f)) = (&t.start, &t.finish) {
                assert!(s <= f, "{path}: {} start {s} > finish {f}", t.name);
            }
        }
        checked += 1;
    }
    eprintln!("real .mpp files validated: {checked}");
}
