//! Validates the .mpp decoders against real sample files when they're present
//! locally (they're git-ignored — see corpus/mpp/README). In CI, where the
//! binaries are absent, this test skips gracefully.

fn corpus(path: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(path)
}

#[test]
fn decodes_real_mpp_task_names_when_present() {
    // (path, expected first task name, minimum task count) for known samples.
    let cases = [
        ("corpus/mpp/projectlibre-construction.mpp", "Commercial Construction", 100usize),
        ("corpus/mpp/saswat-part1.mpp", "Project1", 10usize),
    ];
    let mut checked = 0;
    for (path, first, min) in cases {
        let Ok(bytes) = std::fs::read(corpus(path)) else { continue };
        // container + metadata must parse
        let info = mppread::read_mpp(&bytes).expect("read_mpp");
        assert!(!info.streams.is_empty(), "{path}: no streams");
        // task names decode in order
        let names = mppread::mpp::task_names(&bytes);
        assert!(names.len() >= min, "{path}: {} names (< {min})", names.len());
        assert_eq!(names[0], first, "{path}: first task name");
        checked += 1;
    }
    eprintln!("real .mpp files validated: {checked}");
}
