//! The File "backstage" (menu + folder browser + preview + Save As) and the
//! shared `Start` welcome dialog now live in the shared `backstagecore` crate
//! — this module is a thin re-export shim so existing call sites in
//! `main.rs` (`backstage::Item`, `backstage::Backstage`, …) keep compiling.
pub use backstagecore::*;
