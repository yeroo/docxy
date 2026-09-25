//! `projcore` — a dependency-free project-scheduling engine.
//!
//! The third member of the "open the format, own the model" family alongside
//! `docxcore` (Word) and `gridcore` (Excel). Where those target `.docx`/`.xlsx`,
//! this targets project schedules: the domain of Microsoft Project.
//!
//! Layers:
//! - [`editor`] — shared editing, selection, undo/redo and live scheduling.
//! - [`datetime`] — a `std`-only civil wall-clock instant.
//! - [`model`] — the pure domain model (tasks, links, resources, calendars).
//! - [`mspdi`] — reader for MS Project's documented open XML interchange
//!   format, the interop bridge that avoids the undocumented binary `.mpp`.
//! - [`schedule`] — the Critical Path Method engine (forward/backward passes
//!   over working-time calendars).
//! - [`gantt`] — export a scheduled project as a Markdown/Mermaid Gantt chart.
//! - [`yppx`] — the native `.yppx` OPC package (ZIP container), the
//!   project-scheduling analog of `.docx`/`.xlsx`.

pub mod datetime;
pub mod editor;
pub mod gantt;
pub mod model;
pub mod mspdi;
pub mod schedule;
pub mod yppx;

pub use datetime::DateTime;
pub use model::{
    Assignment, Baseline, Calendar, ConstraintType, DayWorking, LinkType, Predecessor, Project,
    Rate, Resource, Task, TaskType, Week, WorkingTime,
};
