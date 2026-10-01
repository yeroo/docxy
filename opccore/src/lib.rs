//! `opccore` — pure, dependency-free OPC container plumbing.
//!
//! The byte-level layers shared by every Office Open XML format: `.docx`
//! (via `docxcore`) and `.xlsx` (via `gridcore`) are both OPC packages —
//! ZIP containers full of XML parts. This crate is deliberately `std`-only
//! so it stays auditable and trivially testable: the format layers are pure
//! functions over bytes; `fsio` provides atomic filesystem writes.
//!
//! Layers (built bottom-up):
//! - [`inflate`] — DEFLATE (RFC 1951) decompressor.
//! - [`zip`] — read-only ZIP reader (stored + deflate).
//! - [`zipwrite`] — ZIP writer (STORED entries, correct CRC-32).
//! - [`xml`] — minimal pull parser tuned for OOXML.
//! - [`cfb`] — OLE2 Compound File Binary reader (and a small writer), the
//!   container of legacy `.xls`, `.doc` and `.mpp` files.

pub mod cfb;
pub mod fsio;
pub mod inflate;
pub mod xml;
pub mod zip;
pub mod zipwrite;
