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
//! - [`project`] — convert decoded metadata, task dates, outline levels and
//!   predecessor links into a `projcore` project for the terminal and suite hosts.

pub mod cfb;
pub mod mpp;
pub mod oleps;
pub mod project;
pub mod vardata;
pub mod varmeta;

pub use cfb::{Cfb, Node, write_cfb, write_cfb_tree};
pub use mpp::{MppInfo, read_mpp};
