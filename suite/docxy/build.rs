//! Build script: on Windows, compile `docxy.rc` so the app icon
//! (`assets/docxy.ico`) is embedded into `suite.exe` — giving it a proper icon
//! in Explorer, the taskbar, and Alt-Tab. With the `html-export` feature it
//! also builds the docxwasm engine that editable-HTML export embeds.
fn main() {
    println!("cargo:rerun-if-changed=docxy.rc");
    println!("cargo:rerun-if-changed=assets/docxy.ico");

    // With `html-export`, embed the docxwasm engine new editable-HTML bundles
    // carry (htmlbundle::engine_build; the root workspace is two levels up).
    if std::env::var_os("CARGO_FEATURE_HTML_EXPORT").is_some() {
        let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let root = manifest.join("..").join("..");
        let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
        if let Err(e) = htmlbundle::engine_build::prepare_engine(&root, &out) {
            panic!("html-export: {e}");
        }
    }

    #[cfg(windows)]
    {
        // GPUI's debug render path can exceed the PE default 1 MiB main-thread
        // stack during an ordinary selection redraw. Release builds use less
        // stack, which hid the failure from the live harness. Reserve 8 MiB for
        // suite.exe; Windows still commits stack pages only as they are needed.
        println!("cargo:rustc-link-arg-bin=suite=/STACK:8388608");

        // v3 API: (resource_file, macro_definitions). Surface a real error if the
        // resource compiler is missing or the .rc/.ico can't be found, rather
        // than silently shipping an icon-less exe.
        if let Err(e) =
            embed_resource::compile("docxy.rc", embed_resource::NONE).manifest_required()
        {
            println!("cargo:warning=failed to embed the app icon: {e:?}");
        }
    }
}
