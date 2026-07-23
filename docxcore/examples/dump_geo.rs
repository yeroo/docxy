// Dev tool: print the mermaid geometry JSON for a source file (Phase-2 visual harness).
// Usage: cargo run --example dump_geo -- path/to/diagram.mmd
use std::io::Read;

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump_geo <file>");
    let mut src = String::new();
    std::fs::File::open(&path)
        .expect("open")
        .read_to_string(&mut src)
        .expect("read");
    let (w, h, json) = docxcore::mermaid::geometry_box(&src);
    eprintln!(
        "canvas {w} x {h} EMU  ({:.1} x {:.1} in)",
        w as f64 / 914400.0,
        h as f64 / 914400.0
    );
    println!("{json}");
}
