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
//!   shipping build, so [`capture`] uses Win32 `PrintWindow` or Linux X11
//!   `GetImage` against the test window. Linux runs on a private Xvfb display
//!   with a window manager; `scripts/ui-linux.py` manages that lifetime.
//!
//! Neither side has to guess at the other's layout.
//!
//! ## Modules
//!
//! - [`image`] — an RGBA buffer, and the arithmetic of cropping one.
//! - [`deflate`] / [`png`] — writing a real PNG with no image crate.
//! - [`capture`] — window from PID, and the window capture itself.
//! - [`driver`] — the control client, `rect`, and `shot`.
//! - [`probe`] — reading a captured region: solid, dashed or absent, and in
//!   what colour.
//! - [`expect`] — the vocabulary a test states that in, and what a failure in
//!   it reads like.
//! - [`script`] — the format a test is written in, and its parser.
//! - [`desktop`] — a Win32 desktop to start the suite on, never switched in,
//!   and to attach a capture thread to.
//! - [`launch`] — starting a sandboxed instance and stopping it again.
//! - [`runner`] — running a parsed script against one.
//! - [`run`] — where a run's evidence is filed.

pub mod capture;
pub mod deflate;
pub mod desktop;
pub mod driver;
pub mod expect;
pub mod image;
pub mod launch;
pub mod png;
pub mod probe;
pub mod run;
pub mod runner;
pub mod script;

pub use driver::{Driver, RegionRect, Shot, control_dir};
pub use expect::{BorderCheck, BorderExpect, ExpectKind, check_border_clipped, parse_border};
pub use image::{Clip, Image, RectPx};
pub use probe::{LineKind, LineProbe, ProbeOpts, Side, probe_edge};
pub use run::Run;
pub use runner::{CaseOutcome, Runner, ScriptOutcome, Status, StepOutcome};
pub use script::{
    Action, Assertion, Case, Script, ScriptError, Step, duplicate_case, parse_script,
};
