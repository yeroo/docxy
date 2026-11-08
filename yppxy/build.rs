// Build the manual in (#1856): `$OUT_DIR/manual.rs` embeds
// `docs/manual/project/`, an empty list when it is missing (a crate built outside
// the checkout, or before a manual is published).
fn main() {
    let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let manuals = manifest.join("../docs/manual");
    if let Err(e) = helpdocs::embed::write_embed(&manuals.join("project"), &out.join("manual.rs")) {
        panic!("embedding the manual: {e}");
    }
    // Cargo scans a directory recursively, so the first published bundle
    // rebuilds too.
    if manuals.is_dir() {
        println!("cargo:rerun-if-changed={}", manuals.display());
    }
    println!("cargo:rerun-if-changed=build.rs");
}
