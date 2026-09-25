// With the `html-export` feature, embed the docxwasm engine that new
// editable-HTML bundles carry (see htmlbundle::engine_build). Without it this
// does nothing, so ordinary builds never need the wasm target.
fn main() {
    if std::env::var_os("CARGO_FEATURE_HTML_EXPORT").is_none() {
        println!("cargo:rerun-if-changed=build.rs");
        return;
    }
    let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest
        .parent()
        .expect("docxy lives in the workspace root");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    if let Err(e) = htmlbundle::engine_build::prepare_engine(root, &out) {
        panic!("html-export: {e}");
    }
}
