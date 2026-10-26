//! The issue's commands (#309): `yppxy link-fs.mpx --gantt-md out.md` and
//! `--save out.xml` read the MPX file and schedule it as its MSPDI twin
//! does, A 03-02 to 03-03 and B 03-04 to 03-05; an MPX file under an
//! extension no format claims is read by its header.

use std::path::{Path, PathBuf};
use std::process::Command;

/// yppxy with no format plugins, whatever is installed.
fn yppxy() -> Command {
    let no_config = std::env::temp_dir().join("yppxy-tests-no-config");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_yppxy"));
    cmd.env("APPDATA", &no_config)
        .env("XDG_CONFIG_HOME", &no_config)
        .env_remove("YPPX_PLUGIN_DIR");
    cmd
}

fn link_fs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpx/02-link-fs.mpx")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("yppxy-mpx-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(input: &Path, flag: &str, out: &Path) {
    let result = yppxy().arg(input).arg(flag).arg(out).output().unwrap();
    assert!(
        result.status.success(),
        "{} {flag}: {}",
        input.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn gantt_md_of_an_mpx_file_has_the_twins_schedule() {
    let dir = scratch("md");
    let out = dir.join("mpx.md");
    run(&link_fs(), "--gantt-md", &out);
    let md = std::fs::read_to_string(&out).unwrap();
    assert!(
        md.contains("| A | 2026-03-02 08:00:00 | 2026-03-03 17:00:00 |"),
        "{md}"
    );
    assert!(
        md.contains("| B | 2026-03-04 08:00:00 | 2026-03-05 17:00:00 |"),
        "{md}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn save_of_an_mpx_file_writes_the_twins_schedule() {
    let dir = scratch("xml");
    let out = dir.join("mpx.xml");
    run(&link_fs(), "--save", &out);
    let project = projcore::mspdi::read_mspdi(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let schedule = projcore::schedule::schedule(&project);
    let dates: Vec<(String, String, String)> = project
        .tasks
        .iter()
        .map(|t| {
            let r = schedule.get(t.uid).unwrap();
            (
                t.name.clone(),
                r.early_start.to_mspdi(),
                r.early_finish.to_mspdi(),
            )
        })
        .collect();
    let expected = [
        ("A", "2026-03-02T08:00:00", "2026-03-03T17:00:00"),
        ("B", "2026-03-04T08:00:00", "2026-03-05T17:00:00"),
    ]
    .map(|(n, s, f)| (n.to_string(), s.to_string(), f.to_string()));
    assert_eq!(dates, expected);
    // B follows A, as the MPX file's `1` says.
    assert_eq!(project.tasks[1].predecessors[0].uid, project.tasks[0].uid);

    // The import is not written back as .mpx.
    let back = dir.join("back.mpx");
    let result = yppxy()
        .arg(link_fs())
        .arg("--save")
        .arg(&back)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!back.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_mpx_file_under_another_extension_is_read_by_its_header() {
    let dir = scratch("sniff");
    let input = dir.join("link-fs.txt");
    std::fs::copy(link_fs(), &input).unwrap();
    let out = dir.join("mpx.md");
    run(&input, "--gantt-md", &out);
    let md = std::fs::read_to_string(&out).unwrap();
    assert!(
        md.contains("| B | 2026-03-04 08:00:00 | 2026-03-05 17:00:00 |"),
        "{md}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
