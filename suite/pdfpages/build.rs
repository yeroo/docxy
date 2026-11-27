//! The wasm module's linear memory is capped (#2259): a page that asks for
//! more traps in its own instance, which the webview replaces, instead of
//! growing until the browser takes the whole webview down.

/// 1 GiB of linear memory: four times the largest raster the viewer asks
/// for (16 MP of RGBA, premultiplied and straight) with the document's own
/// copies beside it.
const WASM_MAX_MEMORY: u64 = 1 << 30;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        println!("cargo:rustc-link-arg-cdylib=--max-memory={WASM_MAX_MEMORY}");
    }
}
