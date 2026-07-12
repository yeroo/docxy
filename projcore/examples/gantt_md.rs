//! Read an MSPDI (MS Project XML) file, schedule it, and print a Markdown Gantt
//! chart to stdout.
//!
//! Usage:
//!     cargo run -p projcore --example gantt_md -- path/to/project.xml
//!     cargo run -p projcore --example gantt_md -- corpus/mspdi/10-summary.xml

use projcore::{gantt, mspdi, schedule};

fn main() {
    let path = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            eprintln!("usage: gantt_md <file.xml>");
            std::process::exit(2);
        }
    };
    let xml = match std::fs::read_to_string(&path) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    let proj = match mspdi::read_mspdi(&xml) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{path}: parse error: {e}");
            std::process::exit(1);
        }
    };
    let sched = schedule::schedule(&proj);
    print!("{}", gantt::to_markdown(&proj, &sched));
}
