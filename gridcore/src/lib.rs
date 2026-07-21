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
//! - [`edit`] — structural edits (insert/delete rows & columns, renames)
//!   with workbook-wide reference rewriting.
//! - [`numfmt`] — the number-format runtime: real rendering of format codes
//!   (powers `TEXT()` and cell display).
//! - [`format`] — `cell.format` patch parsing/application and its `Xf`
//!   read-back mapping, shared by every host's agent-facing format verb.
//! - [`xlsx`] — `.xlsx` bytes ⇄ [`sheet::Workbook`] with part preservation.

pub mod cf;
pub mod comments;
pub mod drawing;
pub mod edit;
pub mod engine;
pub mod format;
pub mod formula;
pub mod frame;
pub mod model;
pub mod numfmt;
pub mod pivot;
pub mod pivotcalc;
pub mod sheet;
pub mod stats;
pub mod xlsx;
