//! Inspect a compound file (`.mpp`, `.doc`, `.xls`): list its stream paths, or
//! hex-dump one stream. The scaffolding for reverse-engineering a real `.mpp`:
//! navigate its storage tree, then eyeball the bytes of a task/resource block.
//!
//! Usage:
//!     cargo run -p mppread --example inspect -- some.mpp                        # list paths
//!     cargo run -p mppread --example inspect -- some.mpp "   1/TBkndTask/Var2Data"        # hex-dump
//!     cargo run -p mppread --example inspect -- some.mpp "   1/TBkndTask/Var2Data" strings # UTF-16 strings

use mppread::{Cfb, vardata};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: inspect <file> [stream/path] [strings]");
        std::process::exit(2);
    };
    let which = args.next();
    let strings_mode = args.next().as_deref() == Some("strings");

    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    let cfb = Cfb::open(&bytes).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });

    match which {
        None => {
            for p in cfb.paths() {
                let n = cfb.read_path(&p).map(|d| d.len()).unwrap_or(0);
                println!("{p}  ({n} bytes)");
            }
        }
        Some(stream) => match cfb.read_path(&stream) {
            Some(data) if strings_mode => {
                let names = vardata::strings(&data);
                println!(
                    "{stream}: {} UTF-16 strings in {} bytes",
                    names.len(),
                    data.len()
                );
                for s in names {
                    println!("  {s}");
                }
            }
            Some(data) => {
                println!("{stream}: {} bytes", data.len());
                hexdump(&data, 512); // first 512 bytes is plenty to eyeball a header
            }
            None => {
                eprintln!("no stream at path: {stream}");
                std::process::exit(1);
            }
        },
    }
}

/// Classic 16-bytes-per-line hex + ASCII dump, capped at `limit` bytes.
fn hexdump(data: &[u8], limit: usize) {
    for (row, chunk) in data
        .iter()
        .take(limit)
        .collect::<Vec<_>>()
        .chunks(16)
        .enumerate()
    {
        let off = row * 16;
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!("{off:08x}  {:<47}  {ascii}", hex.join(" "));
    }
    if data.len() > limit {
        println!("… {} more bytes", data.len() - limit);
    }
}
