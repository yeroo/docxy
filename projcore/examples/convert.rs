//! Convert between MS Project MSPDI XML and the native `.yppx` package.
//!
//! Direction is inferred from the output extension: `.yppx` packs, anything
//! else (`.xml`) unpacks back to MSPDI. Round-trips through the projcore model.
//!
//! Usage:
//!     cargo run -p projcore --example convert -- corpus/mspdi/10-summary.xml out.yppx
//!     cargo run -p projcore --example convert -- out.yppx roundtrip.xml

use projcore::{mspdi, yppx};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("usage: convert <input> <output.(yppx|xml)>");
        std::process::exit(2);
    };

    let bytes = std::fs::read(input).unwrap_or_else(|e| fail(input, &e.to_string()));
    // Load: a .yppx package, or a bare MSPDI document.
    let proj = if input.ends_with(".yppx") {
        yppx::read_yppx(&bytes).unwrap_or_else(|e| fail(input, &e))
    } else {
        let xml = String::from_utf8(bytes).unwrap_or_else(|_| fail(input, "not UTF-8"));
        mspdi::read_mspdi(&xml).unwrap_or_else(|e| fail(input, &e))
    };

    // Save in the format named by the output extension.
    if output.ends_with(".yppx") {
        std::fs::write(output, yppx::write_yppx(&proj)).unwrap_or_else(|e| fail(output, &e.to_string()));
    } else {
        std::fs::write(output, mspdi::write_mspdi(&proj)).unwrap_or_else(|e| fail(output, &e.to_string()));
    }
    eprintln!("{} task(s): {input} -> {output}", proj.tasks.len());
}

fn fail(what: &str, msg: &str) -> ! {
    eprintln!("{what}: {msg}");
    std::process::exit(1);
}
