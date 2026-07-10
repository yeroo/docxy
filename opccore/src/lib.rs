//! `opccore` — pure, dependency-free OPC container plumbing.
//!
//! The byte-level layers shared by every Office Open XML format: `.docx`
//! (via `docxcore`) and `.xlsx` (via `gridcore`) are both OPC packages —
//! ZIP containers full of XML parts. This crate is deliberately `std`-only
//! so it stays auditable and trivially testable: every layer is a pure
//! function over bytes.
//!
//! Layers (built bottom-up):
//! - [`inflate`] — DEFLATE (RFC 1951) decompressor.
//! - [`zip`] — read-only ZIP reader (stored + deflate).
//! - [`zipwrite`] — ZIP writer (STORED entries, correct CRC-32).
//! - [`xml`] — minimal pull parser tuned for OOXML.

pub mod inflate;
pub mod xml;
pub mod zip;
pub mod zipwrite;
