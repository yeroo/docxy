//! Decode and list the task names from a `.mpp` file (via VarMeta/Var2Data).
//!
//! Usage:
//!     cargo run -p mppread --example tasknames -- some.mpp

fn main() {
    let Some(file) = std::env::args().nth(1) else {
        eprintln!("usage: tasknames <file.mpp>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&file).unwrap_or_else(|e| {
        eprintln!("{file}: {e}");
        std::process::exit(1);
    });
    let names = mppread::mpp::task_names(&bytes);
    println!("{file}: {} task names", names.len());
    for (i, name) in names.iter().enumerate() {
        println!("  {:>4}  {name}", i + 1);
    }
}
