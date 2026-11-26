//! `docxy-pdf <file> [<ms>]`: the PDF child terminal docxy spawns (#2259);
//! see `pdfpages::child` for its protocol.

use std::time::Duration;

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!(
            "usage: docxy-pdf <file.pdf> [<timeout ms>]\n\
             Serves a PDF's pages and text to terminal docxy on stdin/stdout."
        );
        std::process::exit(pdfpages::child::EXIT_USAGE);
    };
    let timeout = args
        .get(1)
        .and_then(|a| a.to_str())
        .and_then(|s| s.parse::<u64>().ok())
        .map_or(pdfpages::child::TIMEOUT, Duration::from_millis);
    std::process::exit(pdfpages::child::run(std::path::Path::new(path), timeout));
}
