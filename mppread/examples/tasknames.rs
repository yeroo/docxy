//! Decode and list the tasks (name + start/finish) from a `.mpp` file.
//!
//! Names come from VarMeta/Var2Data; start/finish are auto-detected from the
//! per-task FixedData records (left blank when no layout fits).
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
    let tasks = mppread::mpp::tasks(&bytes);
    let dated = tasks.iter().filter(|t| t.start.is_some()).count();
    let leveled = tasks.iter().filter(|t| t.outline_level.is_some()).count();
    let links: usize = tasks.iter().map(|t| t.predecessors.len()).sum();
    println!("{file}: {} tasks ({dated} dated, {leveled} outlined, {links} links)", tasks.len());
    for (i, t) in tasks.iter().enumerate() {
        let start = t.start.as_deref().unwrap_or("");
        let finish = t.finish.as_deref().unwrap_or("");
        let indent = "  ".repeat(t.outline_level.unwrap_or(1).saturating_sub(1) as usize);
        let preds: Vec<String> = t.predecessors.iter().map(|p| (p.pred + 1).to_string()).collect();
        let dep = if preds.is_empty() { String::new() } else { format!("  ← {}", preds.join(",")) };
        println!("  {:>4}  {:<19}  {:<19}  {indent}{}{dep}", i + 1, start, finish, t.name);
    }
}
