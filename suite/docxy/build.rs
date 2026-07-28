//! Build script: on Windows, compile `docxy.rc` so the app icon
//! (`assets/docxy.ico`) is embedded into `suite.exe` — giving it a proper icon
//! in Explorer, the taskbar, and Alt-Tab.
fn main() {
    println!("cargo:rerun-if-changed=docxy.rc");
    println!("cargo:rerun-if-changed=assets/docxy.ico");

    #[cfg(windows)]
    {
        // v3 API: (resource_file, macro_definitions). Surface a real error if the
        // resource compiler is missing or the .rc/.ico can't be found, rather
        // than silently shipping an icon-less exe.
        if let Err(e) = embed_resource::compile("docxy.rc", embed_resource::NONE).manifest_required() {
            println!("cargo:warning=failed to embed the app icon: {e:?}");
        }
    }
}
