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
//! - [`mod@format`] — `cell.format` patch parsing/application and its `Xf`
//!   read-back mapping, shared by every host's agent-facing format verb.
//! - [`textio`] — delimited/fixed-width text in and out (CSV open, the Text
//!   Import Wizard, Text to Columns, Save As text types).
//! - [`xlsx`] — `.xlsx` bytes ⇄ [`sheet::Workbook`] with part preservation.
//! - [`legacy`] — import of `.xls`, `.xlsb` and `.ods` workbooks.
//! - [`docprops`] — document properties (`docProps/` core, app, custom).
//! - [`options`] — the per-user Editing options (File › Options › Advanced).
//! - [`autocorrect`] — AutoCorrect for typed entries: replace list,
//!   capitalisation rules, exceptions, hyperlinks.
//! - [`flashfill`] — Flash Fill: a column's pattern from typed examples.
//! - [`fcomplete`] — Formula AutoComplete: the names offered while a
//!   formula is typed.
//! - [`print`] — page setup, print areas and breaks, pagination, sheet PDF.

pub mod autocorrect;
pub mod cf;
pub mod clock;
pub mod comments;
pub mod docprops;
pub mod drawing;
pub mod edit;
pub mod engine;
pub mod entry;
pub mod fcomplete;
pub mod filter;
pub mod flashfill;
pub mod format;
pub mod formula;
pub mod frame;
pub mod legacy;
pub mod model;
pub mod names;
pub mod numfmt;
pub mod options;
pub mod outline;
pub mod pivot;
pub mod pivotcalc;
pub mod print;
pub mod sheet;
pub mod stats;
pub mod textio;
pub mod xlsx;
