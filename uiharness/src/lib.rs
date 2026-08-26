//! `uiharness` — the driver side of the suite's scripted UI test harness.
//!
//! The suite starts a [`ctlcore`] control server when launched with `--harness`
//! and an isolated `DOCXY_CONFIG_DIR` (see `suite/docxy/src/harness.rs`). This
//! crate is what sits on the other end of that socket: it sends the verbs a
//! test is written in, asks the app where a named region ended up, photographs
//! the window, and cuts the region out of the picture.
//!
//! ## Who knows what
//!
//! The split is deliberate and is what keeps a region assertion from drifting
//! when a panel moves:
//!
//! - **The app supplies the geometry.** Only the layout knows where the grid
//!   starts, where `A1` is at this scroll position, or how big a chart card
//!   became. It answers the `rect` verb in physical desktop pixels.
//! - **The harness supplies the pixels.** gpui has no window readback in a
//!   shipping build, so [`capture`] uses Win32 `PrintWindow` against the test
//!   window's HWND — which captures that window alone even when it is partly
//!   occluded, so a test does not have to own the desktop.
//!
//! Neither side has to guess at the other's layout.
//!
//! ## Modules
//!
//! - [`image`] — an RGBA buffer, and the arithmetic of cropping one.
//! - [`deflate`] / [`png`] — writing a real PNG with no image crate.
//! - [`capture`] — HWND from PID, and the window capture itself.
//! - [`driver`] — the control client, `rect`, and `shot`.
//! - [`probe`] — reading a captured region: solid, dashed or absent, and in
//!   what colour.
//! - [`expect`] — the vocabulary a test states that in, and what a failure in
//!   it reads like.
//! - [`script`] — the format a test is written in, and its parser.
//! - [`launch`] — starting a sandboxed instance and stopping it again.
//! - [`runner`] — running a parsed script against one.
//! - [`run`] — where a run's evidence is filed.

pub mod capture;
pub mod deflate;
pub mod driver;
pub mod expect;
pub mod image;
pub mod launch;
pub mod png;
pub mod probe;
pub mod run;
pub mod runner;
pub mod script;

pub use driver::{Driver, RegionRect, control_dir};
pub use expect::{BorderCheck, BorderExpect, ExpectKind, check_border, parse_border};
pub use image::{Image, RectPx};
pub use probe::{LineKind, LineProbe, ProbeOpts, Side, probe_edge};
pub use run::Run;
pub use runner::{CaseOutcome, Runner, ScriptOutcome, Status, StepOutcome};
pub use script::{Action, Assertion, Case, Script, ScriptError, Step, parse_script};
