//! `mppread` — read the OLE2 Compound File Binary container of legacy binary
//! Office files, the first layer of MS Project `.mpp` import.
//!
//! A `.mpp` file (like `.doc`/`.xls`) is a **compound file**: a
//! filesystem-in-a-file of storages and streams. This crate reads that
//! container — [`cfb::Cfb`] opens the bytes and exposes the streams by name —
//! and decodes the parts that are *documented*:
//!
//! - [`cfb`] — the OLE2 Compound File Binary container (MS-CFB).
//! - [`oleps`] — OLE property sets (MS-OLEPS), the typed key/value streams.
//! - [`mpp`] — [`mpp::read_mpp`] pulls a `.mpp`'s metadata (title, author,
//!   company, dates) plus its stream directory.
//!
//! Interpreting the *undocumented*, version-specific task/resource var-data
//! blocks into a `projcore` project is a later layer; this crate is the exact,
//! documented foundation it will stand on.

pub mod cfb;
pub mod mpp;
pub mod oleps;

pub use cfb::{write_cfb, Cfb};
pub use mpp::{read_mpp, MppInfo};
