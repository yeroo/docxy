//! `gridcore` — pure, dependency-free XLSX (SpreadsheetML) engine.
//!
//! The spreadsheet sibling of `docxcore`, built on the same shared `opccore`
//! container layers and the same philosophy (see `SPREADSHEET.md`):
//!
//! - **Headless-first.** Everything here is a pure function over bytes and
//!   models — no terminal, no filesystem assumptions. The `xlsxy` binary
//!   (TUI) is one frontend; `--recalc` batch jobs are another.
//! - **Lossless by design.** Save regenerates only the cell data we model
//!   and splices it into the original worksheet XML; every other part is
//!   preserved byte-for-byte.
//! - **Calculation fidelity is measured, not claimed.** Formulas the engine
//!   cannot evaluate keep Excel's cached values untouched.
//!
//! Layers:
//! - [`sheet`] — the workbook model: sheets, sparse cells, values, styles.
//! - [`formula`] — lexer/parser/AST/serializer + evaluator for the formula
//!   language.
//! - [`engine`] — dependency-graph recalculation over a workbook.
//! - [`xlsx`] — `.xlsx` bytes ⇄ [`sheet::Workbook`] with part preservation.

pub mod engine;
pub mod formula;
pub mod sheet;
pub mod xlsx;
