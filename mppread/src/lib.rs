//! `mppread` — read the OLE2 Compound File Binary container of legacy binary
//! Office files, the first layer of MS Project `.mpp` import.
//!
//! A `.mpp` file (like `.doc`/`.xls`) is a **compound file**: a
//! filesystem-in-a-file of storages and streams. This crate reads that
//! container — [`cfb::Cfb`] opens the bytes and exposes the streams by name.
//! Interpreting the (undocumented) contents of those streams into a
//! `projcore` project is a later layer; this one is the exact, documented
//! foundation it stands on.

pub mod cfb;

pub use cfb::{write_cfb, Cfb};
