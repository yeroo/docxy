//! `mppread` — read MS Project `.mpp` metadata and validated task tables.
//!
//! A `.mpp` file (like `.doc`/`.xls`) is a **compound file**: a
//! filesystem-in-a-file of storages and streams. This crate reads that
//! container — [`cfb::Cfb`] opens the bytes and exposes the streams by name —
//! and decodes metadata and task tables:
//!
//! - [`cfb`] — the OLE2 Compound File Binary container (MS-CFB).
//! - [`oleps`] — OLE property sets (MS-OLEPS), the typed key/value streams.
//! - [`mpp`] — [`mpp::read_mpp`] reads metadata; [`mpp::decode_tasks`] reads
//!   task tables only after their counted indexes and fields validate.
//!
//! - [`project`] — convert decoded metadata, task dates, outline levels and
//!   predecessor links into a `projcore` project for the terminal and suite hosts.

pub mod cfb;
mod fixedmeta;
pub mod mpp;
pub mod oleps;
pub mod project;
mod taskdecode;
pub mod vardata;
pub mod varmeta;

pub use cfb::{Cfb, Node, write_cfb, write_cfb_tree};
pub use mpp::{MppInfo, read_mpp};
