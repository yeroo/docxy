//! The public docxy suite: the suite (`docxy_suite`, rooted at src/main.rs)
//! with no extensions.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    docxy_suite::run(docxy_suite::ext::Extensions::default());
}
