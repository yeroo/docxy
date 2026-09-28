//! Every COM object the shims implement must opt out of agility.
//!
//! windows-implement 0.60 makes an `#[implement]` object agile by default: its
//! `QueryInterface` hands out the free-threaded marshaler for `IMarshal`. An
//! object that does that tells COM it may be called from any thread, so an
//! out-of-process client's calls can arrive on RPC worker threads instead of
//! the server's single-threaded apartment. The shims keep per-thread state (the
//! document registry) and end the server with `PostQuitMessage`, which posts to
//! the *calling* thread — so under 0.62 the LocalServer32 never exited after
//! `Application.Quit`, and out-of-process `SaveAs` and indexing failed, while
//! every in-process case still passed. 0.58 never exposed the marshaler.
//!
//! The class factory is included: `CreateInstance` builds the Application
//! object, which has to happen on the server's own thread too.
//!
//! This is a text check rather than a runtime one so it runs on every OS in CI,
//! and so a COM object added later cannot quietly be agile.

use std::path::Path;

/// Every `#[implement]` in the COM shim crates, as `crate/src/file:line  attr`,
/// whose attribute does not say `Agile = false`.
fn agile_implements() -> Vec<String> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found = Vec::new();
    for krate in ["comshimcore", "wordcomshim", "xlcomshim"] {
        let src = workspace.join(krate).join("src");
        let mut files: Vec<_> = std::fs::read_dir(&src)
            .unwrap_or_else(|e| panic!("{}: {e}", src.display()))
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "rs"))
            .collect();
        files.sort();
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            for (n, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.starts_with("#[implement(") && !line.contains("Agile = false") {
                    let name = file.file_name().unwrap().to_string_lossy();
                    found.push(format!("{krate}/src/{name}:{}  {line}", n + 1));
                }
            }
        }
    }
    found
}

#[test]
fn every_shim_com_object_opts_out_of_agility() {
    let agile = agile_implements();
    assert!(
        agile.is_empty(),
        "{} #[implement] objects are agile by default and must say `Agile = false`:\n{}",
        agile.len(),
        agile.join("\n")
    );
}

/// The check must actually see the objects: a path or pattern that matched
/// nothing would pass the test above vacuously.
#[test]
fn the_check_finds_the_shims_com_objects() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut count = 0;
    for krate in ["comshimcore", "wordcomshim", "xlcomshim"] {
        let src = workspace.join(krate).join("src");
        for file in std::fs::read_dir(&src).unwrap().flatten() {
            let text = std::fs::read_to_string(file.path()).unwrap_or_default();
            count += text
                .lines()
                .filter(|l| l.trim().starts_with("#[implement("))
                .count();
        }
    }
    assert!(
        count >= 23,
        "only {count} #[implement] objects found; the scan is looking in the wrong place"
    );
}
