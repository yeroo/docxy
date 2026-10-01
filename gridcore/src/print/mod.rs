//! Page setup, pagination and PDF output for worksheets.
//!
//! - [`setup`] — the page-setup model ([`setup::PageSetup`]): margins, paper,
//!   orientation, scaling, print options and headers/footers, read from and
//!   patched back into the worksheet part by [`crate::xlsx`].

pub mod setup;
