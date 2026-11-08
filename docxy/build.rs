// Build the Word manual in (#1856): `$OUT_DIR/manual.rs` embeds
// `docs/manual/word/`, an empty list when it is missing (a crate built outside
// the checkout, or before a manual is published).
//
// With the `html-export` feature, also embed the docxwasm engine that new
// editable-HTML bundles carry (see htmlbundle::engine_build). Without it that
// does nothing, so ordinary builds never need the wasm target.
fn main() {
    let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let manuals = manifest.join("../docs/manual");
    if let Err(e) = helpdocs::embed::write_embed(&manuals.join("word"), &out.join("manual.rs")) {
        panic!("embedding the manual: {e}");
    }
    // Cargo scans a directory recursively, so the first published bundle
    // rebuilds too.
    if manuals.is_dir() {
        println!("cargo:rerun-if-changed={}", manuals.display());
    }
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_HTML_EXPORT").is_none() {
        return;
    }
    let root = manifest
        .parent()
        .expect("docxy lives in the workspace root");
    if let Err(e) = htmlbundle::engine_build::prepare_engine(root, &out) {
        panic!("html-export: {e}");
    }
}
