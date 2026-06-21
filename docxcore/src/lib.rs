//! `docxcore` — pure, dependency-free DOCX (OOXML) I/O.
//!
//! This crate is deliberately `std`-only so it stays auditable and trivially
//! testable: every layer is a pure function over bytes, with no terminal, no
//! filesystem assumptions, and no third-party crates. The `docxy` binary
//! (TUI) is built on top of it.
//!
//! Layers (built bottom-up):
//! - [`inflate`] — DEFLATE (RFC 1951) decompressor.
//! - [`zip`] — read-only ZIP reader (stored + deflate).
//! - [`xml`] — minimal pull parser tuned for OOXML.
//!
//! Higher layers:
//! - [`model`] — the editable document tree.
//! - [`load`] — `.docx` bytes -> [`model::Document`].
//!
//! Save and PDF export are added in later phases per `ARCHITECTURE.md`.

pub mod chart;
pub mod comments;
pub mod editor;
pub mod equation;
pub mod export;
pub mod inflate;
pub mod load;
pub mod model;
pub mod numbering;
pub mod package;
pub mod render;
pub mod serialize;
pub mod styles;
pub mod xml;
pub mod zip;
pub mod zipwrite;
