//! Convert between MS Project MSPDI XML and the native `.yppx` package.
//!
//! Direction is inferred from the output extension: `.yppx` packs, `.xml`
//! unpacks back to MSPDI, no extension means `.yppx`, and anything else is
//! refused (see [`yppx::save_target`]). Round-trips through the projcore model.
//!
//! Usage:
//!     cargo run -p projcore --example convert -- corpus/mspdi/10-summary.xml out.yppx
//!     cargo run -p projcore --example convert -- out.yppx roundtrip.xml

use projcore::{mspdi, yppx};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("usage: convert <input> <output.(yppx|xml)>");
        std::process::exit(2);
    };

    let bytes = std::fs::read(input).unwrap_or_else(|e| fail(input, &e.to_string()));
    // Load: a .yppx package, or a bare MSPDI document.
    let proj = if has_ext(Path::new(input), "yppx") {
        yppx::read_yppx(&bytes).unwrap_or_else(|e| fail(input, &e))
    } else {
        let xml = String::from_utf8(bytes).unwrap_or_else(|_| fail(input, "not UTF-8"));
        mspdi::read_mspdi(&xml).unwrap_or_else(|e| fail(input, &e))
    };

    // Save in the format named by the output extension; refuse any other name.
    let target = yppx::save_target(Path::new(output)).unwrap_or_else(|e| fail(output, &e));
    let bytes = if has_ext(&target, "yppx") {
        yppx::write_yppx(&proj)
    } else {
        mspdi::write_mspdi(&proj).into_bytes()
    };
    opccore::fsio::write_atomic(&target, &bytes).unwrap_or_else(|e| fail(output, &e.to_string()));
    eprintln!(
        "{} task(s): {input} -> {}",
        proj.tasks.len(),
        target.display()
    );
}

fn has_ext(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

fn fail(what: &str, msg: &str) -> ! {
    eprintln!("{what}: {msg}");
    std::process::exit(1);
}
