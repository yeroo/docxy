//! Page setup, pagination and PDF output for worksheets.
//!
//! - [`setup`] — the page-setup model ([`setup::PageSetup`]): margins, paper,
//!   orientation, scaling, print options and headers/footers, read from and
//!   patched back into the worksheet part by [`crate::xlsx`].
//! - [`area`] — print areas and print titles over `_xlnm.*` defined names.
//! - [`hf`] — the header/footer code codec (`&[Page]` ⇄ `&P`, sections).
//! - [`paginate`] — the pages a print job prints on, as Excel lays them out.
//! - [`pdf`] — those pages as a PDF.

pub mod area;
pub mod hf;
pub mod paginate;
pub mod pdf;
pub mod setup;
