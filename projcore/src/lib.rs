//! `projcore` — a dependency-free project-scheduling engine.
//!
//! The third member of the "open the format, own the model" family alongside
//! `docxcore` (Word) and `gridcore` (Excel). Where those target `.docx`/`.xlsx`,
//! this targets project schedules: the domain of Microsoft Project.
//!
//! Layers:
//! - [`datetime`] — a `std`-only civil wall-clock instant.
//! - [`model`] — the pure domain model (tasks, links, resources, calendars).
//! - [`mspdi`] — reader for MS Project's documented open XML interchange
//!   format, the interop bridge that avoids the undocumented binary `.mpp`.
//!
//! The scheduler (Critical Path Method) and the native `.yppx` package land in
//! subsequent layers.

pub mod datetime;
pub mod model;
pub mod mspdi;

pub use datetime::DateTime;
pub use model::{
    Assignment, Calendar, ConstraintType, DayWorking, LinkType, Predecessor, Project, Resource,
    Task, WorkingTime,
};
