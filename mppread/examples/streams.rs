//! List the streams inside a compound file (`.mpp`, `.doc`, `.xls`, …).
//!
//! Usage:
//!     cargo run -p mppread --example streams -- some.mpp
//!
//! Handy for peeking at a real `.mpp` you have on hand — the stream names
//! (`Props`, `Var2Data`, `Fixed2Data`, calendar/task/resource blocks, …) are
//! the map for the eventual `.mpp` → projcore decoder.

use mppread::{read_mpp, Cfb};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: streams <file.(mpp|doc|xls)>");
        std::process::exit(2);
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };

    // Documented metadata (works on any compound file).
    if let Ok(info) = read_mpp(&bytes) {
        let field = |label: &str, v: &str| {
            if !v.is_empty() {
                println!("  {label:<12} {v}");
            }
        };
        println!("metadata:");
        field("title", &info.title);
        field("author", &info.author);
        field("last author", &info.last_author);
        field("company", &info.company);
        field("manager", &info.manager);
        field("revision", &info.revision);
        if let Some(c) = &info.created {
            println!("  {:<12} {c}", "created");
        }
        if let Some(s) = &info.saved {
            println!("  {:<12} {s}", "saved");
        }
    }

    let cfb = match Cfb::open(&bytes) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    println!("{path}: {} directory entries", cfb.entries().len());
    for e in cfb.entries() {
        let kind = match e.kind {
            5 => "root",
            1 => "storage",
            2 => "stream",
            _ => "?",
        };
        let size = cfb.read_stream(&e.name).map(|d| d.len()).unwrap_or(0);
        if e.is_stream() {
            println!("  {kind:<8} {:<32} {size} bytes", e.name);
        } else {
            println!("  {kind:<8} {}", e.name);
        }
    }
}
