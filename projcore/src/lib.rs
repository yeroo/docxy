//! `projcore` — a dependency-free project-scheduling engine.
//!
//! The third member of the "open the format, own the model" family alongside
//! `docxcore` (Word) and `gridcore` (Excel). Where those target `.docx`/`.xlsx`,
//! this targets project schedules: the domain of Microsoft Project.
//!
//! Layers:
//! - [`assign`] — assignment dates and costs, refreshed after an edit.
//! - [`editor`] — shared editing, selection, undo/redo and live scheduling.
//! - [`datetime`] — a `std`-only civil wall-clock instant.
//! - [`formats`] — the file formats a host opens and saves (`.yppx`, `.xml`,
//!   and what it registers), and, behind the `plugins` feature, the loader of
//!   format plugins.
//! - [`model`] — the pure domain model (tasks, links, resources, calendars).
//! - [`mspdi`] — reader for MS Project's documented open XML interchange
//!   format, the interop bridge that avoids the undocumented binary `.mpp`.
//! - [`schedule`] — the Critical Path Method engine (forward/backward passes
//!   over working-time calendars).
//! - [`gantt`] — export a scheduled project as a Markdown/Mermaid Gantt chart.
//! - [`formatting`] — the plan's formatting (bar palette, text styles per
//!   category and cell, gridlines, link style), saved in the `.yppx`
//!   `formatting.xml` part.
//! - [`tableview`] — the Project table's saved view state (column widths), in
//!   the `.yppx` `tableview.xml` part.
//! - [`json`] — the small JSON value the plugin ABI's payloads use.
//! - [`plugin_abi`] — the C ABI a format plugin implements (MSPDI XML text
//!   across the boundary), with the optional version-target and UI symbols.
//! - [`uiplugin`] — the JSON shapes of a UI plugin's manifest, state,
//!   commands and overlay, and the host version requirement it targets.
//! - [`report`] — Project's View Reports (Project Overview, Late Tasks, …) as
//!   Markdown.
//! - [`text`] — text conventions shared by the readers (newlines in notes).
//! - [`yppx`] — the native `.yppx` OPC package (ZIP container), the
//!   project-scheduling analog of `.docx`/`.xlsx`.

pub mod assign;
pub mod datetime;
pub mod editor;
pub mod flagpart;
pub mod formats;
pub mod formatting;
pub mod gantt;
pub mod json;
pub mod leveling;
pub mod marked;
pub mod model;
pub mod mpx;
pub mod mspdi;
pub mod plugin_abi;
pub mod report;
pub mod richtext;
pub mod schedule;
pub mod tableview;
pub mod text;
pub mod timeline;
pub mod uiplugin;
pub mod yppx;

pub use datetime::DateTime;
pub use model::{
    Assignment, Baseline, Calendar, CalendarException, ConstraintType, DayWorking, LagFormat,
    LagKind, LagUnit, LinkType, Predecessor, Project, Rate, Resource, Task, TaskType, Week,
    WorkCalendar, WorkWeek, WorkingTime,
};
pub use text::normalize_newlines;
