//! The OLE2 Compound File Binary container, now shared with gridcore's
//! legacy `.xls` reader: it lives in [`opccore::cfb`] and is re-exported here
//! so `mppread::cfb` keeps its API.

pub use opccore::cfb::*;
