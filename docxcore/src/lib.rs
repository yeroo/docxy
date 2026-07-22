//! `docxcore` — pure, dependency-free DOCX (OOXML) I/O.
//!
//! This crate is deliberately `std`-only so it stays auditable and trivially
//! testable: every layer is a pure function over bytes, with no terminal, no
//! filesystem assumptions, and no third-party crates. The `docxy` binary
//! (TUI) is built on top of it.
//!
//! Layers (built bottom-up):
//! - [`inflate`] — DEFLATE (RFC 1951) decompressor (from `opccore`).
//! - [`zip`] — read-only ZIP reader (stored + deflate, from `opccore`).
//! - [`xml`] — minimal pull parser tuned for OOXML (from `opccore`).
//!
//! Higher layers:
//! - [`model`] — the editable document tree.
//! - [`load`] — `.docx` bytes -> [`model::Document`].
//!
//! Save and PDF export are added in later phases per `ARCHITECTURE.md`.

// The container plumbing lives in the shared `opccore` crate (it is OPC-level,
// not Word-specific — `gridcore` builds `.xlsx` support on the same layers).
// Re-exported here so `docxcore::zip::ZipArchive` etc. keep working.
pub use opccore::{inflate, xml, zip, zipwrite};

pub mod agent;
pub mod chart;
pub mod comments;
pub mod editor;
pub mod equation;
pub mod export;
pub mod field;
pub mod latex;
pub mod load;
pub mod markdown;
pub mod mathbox;
pub mod mermaid;
pub mod mermaid_seq;
pub mod model;
pub mod notes;
pub mod numbering;
pub mod omath;
pub mod package;
pub mod render;
pub mod serialize;
pub mod styles;
