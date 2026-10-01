//! Page setup, pagination and PDF output for worksheets.
//!
//! - [`setup`] — the page-setup model ([`setup::PageSetup`]): margins, paper,
//!   orientation, scaling, print options and headers/footers, read from and
//!   patched back into the worksheet part by [`crate::xlsx`].
//! - [`area`] — print areas and print titles over `_xlnm.*` defined names.
//! - [`hf`] — the header/footer code codec (`&[Page]` ⇄ `&P`, sections).

pub mod area;
pub mod hf;
pub mod setup;
