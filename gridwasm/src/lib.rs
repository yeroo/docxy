//! `gridwasm` — WebAssembly bridge for `gridcore` (the Offxy VS Code
//! extension's spreadsheet engine). ABI exports land in a later task; the
//! testable core is [`bridge::Session`].

pub mod bridge;
mod json;
