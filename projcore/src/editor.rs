//! Shared project editing, selection, history and derived scheduling state.
//!
//! Edits address stable task UIDs. Validation precedes the undo snapshot, so a
//! rejected edit leaves all editor state untouched. Hosts own file I/O and UI
//! messages; [`Editor::mark_saved`] acknowledges a successful save.

use crate::datetime::DateTime;
use crate::model::{
    Assignment, AssignmentBaseline, Baseline, ConstraintType, LagFormat, LagKind, LagUnit,
    LinkType, Predecessor, Project, Resource, ResourceBaseline, ResourceType, Task, TaskType,
    TimephasedValue,
};
use crate::schedule::{Leveled, Schedule, level, schedule};

const UNDO_CAP: usize = 100;

mod cells;
mod effort;
pub use cells::{
    day_finish, format_duration_exact, format_predecessors, format_resource_names, parse_cell_date,
    parse_lag, parse_task_predecessors, split_resource_names,
};
// Re-entering a task's cell goes through `parse_task_predecessors`, which
// keeps links shown in a fallback unit; the plain parser stays internal.
#[cfg(test)]
use cells::parse_predecessors;
use cells::{bracket_units, parse_resource_token, same_shown_units};
mod moving;
mod outline;
use outline::subtree_end;
#[cfg(test)]
mod refresh_tests;

/// A fixed Monday anchor, shared by new schedules and undated imports.
pub fn default_anchor() -> DateTime {
    DateTime::from_ymd_hm(2026, 1, 5, 8, 0)
}

/// A new empty project. Hosts may add a starter task for their own UI.
pub fn untitled_project() -> Project {
    Project {
        name: "Untitled".into(),
        start_date: Some(default_anchor()),
        ..Project::default()
    }
}

/// Fields updated atomically by an agent's `task.set` command.
#[derive(Default)]
pub struct TaskPatch {
    pub name: Option<String>,
    pub duration_min: Option<i64>,
    pub level: Option<u32>,
    /// Manually (`true`) or automatically (`false`) scheduled; see
    /// [`Editor::set_manual`].
    pub manual: Option<bool>,
    /// Whether the duration was typed with Project's `?` (see
    /// [`parse_task_duration`]); only with `duration_min`. `Some(true)` marks
    /// the task estimated, `Some(false)` commits an estimated one (an unset
    /// flag stays unset), even when the minutes are unchanged. `None` keeps
    /// the old rule: a changed duration commits an estimate.
    pub estimated: Option<bool>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AssignOutcome {
    Assigned,
    Cleared,
    AlreadyAssigned,
    NothingToClear,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FindOutcome {
    Inactive,
    Found(usize),
    NotFound,
}

/// A project and its editing session. Project access is read-only so mutations
/// cannot bypass validation, history or rescheduling.
pub struct Editor {
    proj: Project,
    sel: usize,
    /// The task at `sel` when it was last chosen. After an edit, a selection
    /// still on that task is revealed; one left on another task by a clamp
    /// or a shifted index moves out of collapsed summaries instead.
    sel_uid: Option<i32>,
    /// Collapsed summaries by UID: view state, outside the model, history and
    /// saved files (see [`Self::set_collapsed`]).
    collapsed: std::collections::BTreeSet<i32>,
    undo: Vec<Project>,
    redo: Vec<Project>,
    dirty: bool,
    sched: Schedule,
    leveled: bool,
    level: Option<Leveled>,
    last_find: String,
    /// Snapshots ever recorded; unlike `undo.len()` it still moves at `UNDO_CAP`.
    pushes: u64,
    /// The schedule before the current edit, while that edit is being made.
    /// Every reschedule within it refreshes what the edit made stale
    /// ([`crate::assign::refresh`] against the model on top of the undo
    /// stack); undo, redo and a new document clear it, since they restore a
    /// model exactly and must not rewrite it.
    pending: Option<Schedule>,
}

impl Editor {
    pub fn new(proj: Project) -> Self {
        Self::restored(proj, false)
    }

    /// Restore saved session content and its dirty flag, with empty edit history.
    pub fn restored(mut proj: Project, dirty: bool) -> Self {
        recompute_summaries(&mut proj);
        let sched = schedule(&proj);
        let sel_uid = proj.tasks.first().map(|t| t.uid);
        Self {
            proj,
            sel: 0,
            sel_uid,
            collapsed: Default::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            dirty,
            sched,
            leveled: false,
            level: None,
            last_find: String::new(),
            pushes: 0,
            pending: None,
        }
    }

    /// Begin a new document session, retaining find and leveling preferences.
    pub fn replace_project(&mut self, proj: Project) {
        self.proj = proj;
        self.sel = 0;
        self.sel_uid = self.selected_uid();
        self.collapsed.clear();
        self.undo.clear();
        self.redo.clear();
        self.dirty = false;
        self.pending = None;
        self.reschedule();
    }

    pub fn project(&self) -> &Project {
        &self.proj
    }
    pub fn sel(&self) -> usize {
        self.sel
    }
    /// Select a row, expanding any collapsed summary that hides it.
    pub fn select(&mut self, index: usize) {
        self.sel = index.min(self.proj.tasks.len().saturating_sub(1));
        self.reveal(self.sel);
        self.sel_uid = self.selected_uid();
    }
    pub fn selected_uid(&self) -> Option<i32> {
        self.proj.tasks.get(self.sel).map(|t| t.uid)
    }
    pub fn dirty(&self) -> bool {
        self.dirty
    }
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }
    pub fn schedule(&self) -> &Schedule {
        &self.sched
    }
    pub fn leveled(&self) -> bool {
        self.leveled
    }
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }
    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }
    pub fn find_query(&self) -> &str {
        &self.last_find
    }

    pub fn toggle_level(&mut self) {
        self.pending = None;
        self.leveled = !self.leveled;
        self.reschedule();
    }

    pub fn disp_start(&self, uid: i32) -> Option<DateTime> {
        match &self.level {
            Some(lv) => lv.start(uid),
            None => self.sched.get(uid).map(|r| r.early_start),
        }
    }

    pub fn disp_finish(&self, uid: i32) -> Option<DateTime> {
        match &self.level {
            Some(lv) => lv.finish(uid),
            None => self.sched.get(uid).map(|r| r.early_finish),
        }
    }

    /// The earliest displayed date: the project start, or earlier when a task
    /// is shown before it (a manual task pinned there, or an SF-driven one).
    pub fn disp_project_start(&self) -> DateTime {
        self.proj
            .tasks
            .iter()
            .filter_map(|t| self.disp_start(t.uid))
            .fold(self.sched.project_start, DateTime::min)
    }

    /// The latest displayed date: the leveled finish while leveling is on,
    /// otherwise the schedule's, or later when a task is shown after it.
    pub fn disp_project_finish(&self) -> DateTime {
        let finish = match &self.level {
            Some(lv) => lv.project_finish,
            None => self.sched.project_finish,
        };
        self.proj
            .tasks
            .iter()
            .filter_map(|t| self.disp_finish(t.uid))
            .fold(finish, DateTime::max)
    }

    /// A summary's rolled-up span (see [`Schedule::rolled_up`]), leveled when
    /// leveling is on. For a manual summary it differs from its shown dates.
    pub fn disp_rollup(&self, uid: i32) -> Option<(DateTime, DateTime)> {
        match &self.level {
            Some(lv) => lv.rolled_up(uid),
            None => self.sched.rolled_up(uid),
        }
    }

    /// Project's warning ([`crate::schedule::manual_warning`]) on the shown
    /// dates: a manual summary's subtasks finish after it, or a manual task
    /// finishes after its manual parent summary. It answers for manual leaves
    /// too, but hosts show it on summary rows only (projctl's `warning`, the
    /// docxy and yppxy summary bars); leaf rows do not show it yet.
    pub fn summary_warning(&self, uid: i32) -> bool {
        crate::schedule::manual_warning(
            &self.proj,
            uid,
            |u| self.disp_finish(u),
            |u| self.disp_rollup(u),
        )
    }

    /// The duration shown alongside [`Self::disp_start`]/[`Self::disp_finish`]:
    /// a leaf's own duration, or a summary's working time between its displayed
    /// (leveled when leveling is on) dates. `None` for an unknown or
    /// unscheduled task.
    pub fn disp_duration_min(&self, uid: i32) -> Option<i64> {
        let task = self.proj.task(uid)?;
        let start = self.disp_start(uid)?;
        let finish = self.disp_finish(uid)?;
        Some(crate::schedule::summary_or_leaf_min(
            &self.proj, task, start, finish,
        ))
    }

    fn index(&self, uid: i32) -> Result<usize, String> {
        self.proj
            .tasks
            .iter()
            .position(|t| t.uid == uid)
            .ok_or_else(|| format!("no task with uid {uid}"))
    }

    /// Where a new manual task starts: the project start, else the anchor
    /// the schedule actually uses.
    fn new_task_start(&self) -> DateTime {
        self.proj.start_date.unwrap_or(self.sched.project_start)
    }

    /// Row `i` as an edit would make it (see [`materialized`]).
    fn row_as_edited(&self, i: usize) -> Task {
        materialized(&self.proj, i, self.new_task_start(), &self.collapsed)
    }

    /// A structural edit of row `i`. A blank row first becomes a task (see
    /// [`materialized`]); `edit` learns whether it was blank. A manual task
    /// made that way gets its dates pinned, as [`Self::add_task`] does.
    fn edit_row(&mut self, i: usize, edit: impl FnOnce(&mut Project, bool)) -> Result<(), String> {
        let (was_blank, uid) = (self.proj.tasks[i].is_null, self.proj.tasks[i].uid);
        // The edit runs on a copy of this project, so the row becomes the same.
        let made = was_blank.then(|| self.row_as_edited(i));
        self.edit_structure(|proj| {
            if let Some(made) = made {
                proj.tasks[i] = made;
            }
            edit(proj, was_blank);
        })?;
        if was_blank {
            self.stamp_pinned_dates(uid);
        }
        Ok(())
    }

    fn is_blank(&self, uid: i32) -> bool {
        self.proj.task(uid).is_some_and(|t| t.is_null)
    }

    fn snapshot(&mut self) {
        self.push_undo(self.proj.clone());
    }

    /// Record `prev` as the state to undo to: caps history and clears redo.
    /// Every edit comes through here, so it also opens the edit's refresh
    /// (see `pending`).
    fn push_undo(&mut self, prev: Project) {
        self.pending = Some(self.sched.clone());
        self.pushes += 1;
        self.undo.push(prev);
        if self.undo.len() > UNDO_CAP {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn reschedule(&mut self) {
        recompute_summaries(&mut self.proj);
        self.sched = schedule(&self.proj);
        // The refresh changes no scheduling input, so the schedule stands.
        if let (Some(prev_sched), Some(prev)) = (&self.pending, self.undo.last()) {
            crate::assign::refresh(prev, prev_sched, &mut self.proj, &self.sched);
        }
        self.level = self.leveled.then(|| level(&self.proj));
    }

    fn changed(&mut self) {
        self.dirty = true;
        self.prune_collapsed();
        self.sel = self.sel.min(self.proj.tasks.len().saturating_sub(1));
        if self.selected_uid() == self.sel_uid {
            // The edited task itself moved under a collapsed summary.
            self.reveal(self.sel);
        } else if let Some(owner) = self.hidden_owners().get(self.sel).copied().flatten() {
            // A clamped or shifted index: keep the summaries collapsed.
            self.sel = owner;
        }
        self.sel_uid = self.selected_uid();
        self.reschedule();
    }

    /// Validate newly created/exposed leaves before touching history or UI state.
    fn edit_structure(&mut self, edit: impl FnOnce(&mut Project)) -> Result<(), String> {
        let mut next = self.proj.clone();
        edit(&mut next);
        recompute_summaries(&mut next);
        if let Some(error) = crate::schedule::calendar_error(&next) {
            return Err(error);
        }
        // Move the old project into history; `next` is already a full copy.
        let prev = std::mem::replace(&mut self.proj, next);
        self.push_undo(prev);
        self.changed();
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        self.pending = None;
        self.redo.push(std::mem::replace(&mut self.proj, prev));
        self.changed();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.pending = None;
        self.undo.push(std::mem::replace(&mut self.proj, next));
        self.changed();
        true
    }

    /// Find the next matching row, wrapping after the selected row. An empty
    /// query repeats the previous search. Searching never changes history.
    pub fn find(&mut self, query: &str) -> FindOutcome {
        self.find_from(query, self.sel + 1)
    }

    /// [`Self::find`], starting at the first task instead of after the
    /// selected one (as from docxy's entry row).
    pub fn find_from_top(&mut self, query: &str) -> FindOutcome {
        self.find_from(query, 0)
    }

    /// A hidden match expands the summaries that hide it.
    fn find_from(&mut self, query: &str, first: usize) -> FindOutcome {
        let query = query.trim().to_lowercase();
        if !query.is_empty() {
            self.last_find = query;
        }
        let n = self.proj.tasks.len();
        if self.last_find.is_empty() || n == 0 {
            return FindOutcome::Inactive;
        }
        for step in 0..n {
            let i = (first + step) % n;
            if self.proj.tasks[i]
                .name
                .to_lowercase()
                .contains(&self.last_find)
            {
                self.select(i);
                return FindOutcome::Found(i);
            }
        }
        FindOutcome::NotFound
    }

    /// Insert after a UID, or append. As in Microsoft Project, the new task is
    /// the next sibling of the task above it, or that task's first child when
    /// it is a summary; the row it pushes down plays no part, and blank rows
    /// are skipped (see [`level_at`]). Below a collapsed summary's hidden
    /// rows, a task appended or added after a row shown there becomes that
    /// summary's sibling; one added after a hidden row takes its level as if
    /// the summary were expanded. The task takes the plan's stated task type
    /// and effort-driven default, and with the plan's `Autolink` on, a task
    /// inserted into a finish-to-start chain is linked into it (see
    /// `autolink`). The returned row is not automatically selected.
    pub fn add_task(
        &mut self,
        after: Option<i32>,
        name: &str,
        duration_min: i64,
        estimated: bool,
    ) -> Result<usize, String> {
        validate_duration(duration_min)?;
        let at = match after {
            Some(uid) => self.index(uid)? + 1,
            None => self.proj.tasks.len(),
        };
        // The row the task goes after; appending has none.
        let anchor = after.map(|_| at - 1);
        let outline_level = level_at(&self.proj, at, anchor, &self.collapsed);
        let uid = self.next_uid()?;
        // Follow the plan's own default mode; a manual task starts at the
        // project start (or the anchor the schedule actually uses).
        let manual = self.proj.new_tasks_are_manual;
        let manual_start = manual.then(|| self.new_task_start());
        let leveled = self.leveled;
        self.edit_structure(|proj| {
            proj.tasks.insert(
                at,
                Task {
                    uid,
                    id: uid,
                    name: name.into(),
                    outline_level,
                    duration_min,
                    milestone: duration_min == 0,
                    manual,
                    manual_start,
                    manual_duration_min: manual.then_some(duration_min),
                    // Only a `?` (typed, or the plan's estimated default)
                    // marks it: an unset flag saves as absent.
                    estimated: estimated.then_some(true),
                    // The plan's defaults, only where the file states them.
                    task_type: proj.default_task_type,
                    effort_driven: proj.new_tasks_effort_driven,
                    ..Task::default()
                },
            );
            // A manual task linked into a chain starts where that link puts
            // it, as an auto one would be shown, rather than at the project
            // start before its new predecessor finishes. Under leveling that
            // is its leveled start, as `set_manual` pins at the shown dates.
            if anchor.is_some() && autolink(proj, at) && manual {
                let mut probe = proj.clone();
                probe.tasks[at].manual = false;
                probe.tasks[at].manual_start = None;
                probe.tasks[at].manual_duration_min = None;
                let start = if leveled {
                    crate::schedule::level(&probe).start(uid)
                } else {
                    crate::schedule::schedule(&probe)
                        .get(uid)
                        .map(|r| r.early_start)
                };
                if let Some(start) = start {
                    proj.tasks[at].manual_start = Some(start);
                }
            }
        })?;
        self.stamp_pinned_dates(uid);
        Ok(at)
    }

    /// Insert a blank row above a UID, or append, as ONE undo step: Project's
    /// Insert Task › Blank Row. The row stores nothing but its UID and ID, so
    /// it is outside the outline and the schedule until typed into; then it
    /// takes the level of the task above where it sits then (see
    /// [`materialized`]). The returned row is not automatically selected.
    pub fn insert_blank_row(&mut self, before: Option<i32>) -> Result<usize, String> {
        let at = match before {
            Some(uid) => self.index(uid)?,
            None => self.proj.tasks.len(),
        };
        let uid = self.next_uid()?;
        self.edit_structure(|proj| {
            proj.tasks.insert(
                at,
                Task {
                    uid,
                    id: uid,
                    is_null: true,
                    ..Task::default()
                },
            )
        })?;
        Ok(at)
    }

    /// The UID a new row takes: one past the largest in use.
    fn next_uid(&self) -> Result<i32, String> {
        self.proj
            .tasks
            .iter()
            .map(|t| t.uid)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| "No task IDs available".into())
    }

    /// Append a blank row and apply `edit` to it as ONE undo step, as typing
    /// into Project's entry row below the last task does. The row becomes a
    /// task through the ordinary setters (see [`materialized`]). Returns the
    /// new row and `edit`'s value. If `edit` fails or leaves the row blank,
    /// nothing changes: no row, history, dirty flag or selection change.
    pub fn append_row<T>(
        &mut self,
        edit: impl FnOnce(&mut Editor, i32) -> Result<T, String>,
    ) -> Result<Option<(usize, T)>, String> {
        let uid = self.next_uid()?;
        let i = self.proj.tasks.len();
        // Outside history: the scheduler drops blank rows, so the schedule
        // stays valid, and every setter validates before its one snapshot.
        self.proj.tasks.push(Task {
            uid,
            id: uid,
            is_null: true,
            ..Task::default()
        });
        let pushes = self.pushes;
        let result = edit(self, uid);
        let made = self.pushes != pushes && self.proj.tasks.get(i).is_some_and(|t| !t.is_null);
        match result {
            Ok(value) if made => {
                debug_assert_eq!(self.pushes, pushes + 1, "one setter, one snapshot");
                // The snapshot holds the blank row; without it, one Undo
                // removes the whole task.
                let top = self.undo.last_mut().expect("the setter pushed");
                debug_assert!(top.tasks.last().is_some_and(|t| t.uid == uid && t.is_null));
                top.tasks.pop();
                Ok(Some((i, value)))
            }
            result => {
                debug_assert_eq!(
                    self.pushes, pushes,
                    "a no-op or rejected edit pushes nothing"
                );
                debug_assert!(
                    self.proj
                        .tasks
                        .last()
                        .is_some_and(|t| t.uid == uid && t.is_null)
                );
                self.proj.tasks.pop();
                result.map(|_| None)
            }
        }
    }

    /// Rows `uid` owns in the positional outline: itself, then every following
    /// row deeper than it. Uses levels, not the `summary` flag, so a stale flag
    /// cannot cause a partial delete. Blank rows are outside the outline: they
    /// neither end a subtree nor own one. One between two of its descendants
    /// goes with it; one after its last descendant stays.
    fn subtree(&self, uid: i32) -> Result<std::ops::Range<usize>, String> {
        let i = self.index(uid)?;
        Ok(i..subtree_end(&self.proj, i))
    }

    /// How many subtasks (all depths) deleting `uid` would also remove; hosts
    /// confirm before deleting when this is non-zero. Blank rows inside the
    /// subtree go with it but are not subtasks.
    pub fn subtree_len(&self, uid: i32) -> Result<usize, String> {
        let range = self.subtree(uid)?;
        Ok(self.proj.tasks[range]
            .iter()
            .skip(1)
            .filter(|t| !t.is_null)
            .count())
    }

    /// Delete `uid` and its whole subtree as one undo step, returning the
    /// removed UIDs in outline order (the task first).
    pub fn delete_task(&mut self, uid: i32) -> Result<Vec<i32>, String> {
        let range = self.subtree(uid)?;
        let removed: Vec<i32> = self.proj.tasks[range.clone()]
            .iter()
            .map(|t| t.uid)
            .collect();
        self.edit_structure(|proj| {
            proj.tasks.drain(range);
            for t in &mut proj.tasks {
                t.predecessors.retain(|p| !removed.contains(&p.uid));
            }
            // Remove assignments as well, so they cannot attach to a reused UID.
            proj.assignments.retain(|a| !removed.contains(&a.task_uid));
        })?;
        Ok(removed)
    }

    pub fn indent(&mut self, uid: i32, delta: i32) -> Result<(), String> {
        let i = self.index(uid)?;
        self.edit_row(i, |proj, _| {
            let t = &mut proj.tasks[i];
            t.outline_level = (i64::from(t.outline_level) + i64::from(delta)).clamp(1, 20) as u32;
        })
    }

    pub fn rename(&mut self, uid: i32, name: &str) -> Result<(), String> {
        self.update_task(
            uid,
            TaskPatch {
                name: Some(name.into()),
                ..TaskPatch::default()
            },
        )
    }

    /// Set a typed duration; a trailing `?` marks it estimated (see
    /// [`parse_task_duration`]).
    pub fn set_duration(&mut self, uid: i32, text: &str) -> Result<(), String> {
        let (min, estimated) = parse_task_duration(text, &self.proj)
            .ok_or_else(|| format!("Couldn't read duration '{text}' (try 3d, 4h, 2w)"))?;
        self.set_duration_min(uid, min, estimated)
    }

    /// Set a duration as typed, estimated (`1d?`) or not. As in Project,
    /// typing it without `?` commits an estimated duration, even the same one.
    pub fn set_duration_min(&mut self, uid: i32, min: i64, estimated: bool) -> Result<(), String> {
        self.update_task(
            uid,
            TaskPatch {
                duration_min: Some(min),
                estimated: Some(estimated),
                ..TaskPatch::default()
            },
        )
    }

    /// Switch a task between manually and automatically scheduled, as
    /// Project's Task Mode does. A task that becomes manual is pinned where it
    /// is shown now (leveled dates while leveling is on), so the switch moves
    /// nothing; one that becomes automatic drops its pin and the scheduler
    /// places it by its links and constraints again. A blank row becomes a
    /// task with that mode. Setting the mode a task already has is a no-op.
    pub fn set_manual(&mut self, uid: i32, manual: bool) -> Result<(), String> {
        self.update_task(
            uid,
            TaskPatch {
                manual: Some(manual),
                ..TaskPatch::default()
            },
        )
    }

    /// Set the plan's mode for new tasks (MSPDI `NewTasksAreManual`), as one
    /// undo step. Existing tasks keep their own mode.
    pub fn set_new_tasks_manual(&mut self, manual: bool) {
        if self.proj.new_tasks_are_manual == manual {
            return;
        }
        self.snapshot();
        self.proj.new_tasks_are_manual = manual;
        self.changed();
    }

    /// Where [`Self::set_manual`] pins task `i`: its shown start and finish
    /// and its shown duration. A blank row starts where a new manual task
    /// does and keeps the duration it is given; a task the schedule skips (a
    /// calendar without working time) keeps its saved start, else the new
    /// task start, and gets no pinned finish.
    fn pin_at(&self, i: usize) -> (DateTime, Option<DateTime>, Option<i64>) {
        let t = &self.proj.tasks[i];
        if t.is_null {
            return (self.new_task_start(), None, None);
        }
        let start = self
            .disp_start(t.uid)
            .or(t.stored_start)
            .unwrap_or_else(|| self.new_task_start());
        let finish = self.disp_finish(t.uid);
        (start, finish, self.disp_duration_min(t.uid))
    }

    pub fn update_task(&mut self, uid: i32, patch: TaskPatch) -> Result<(), String> {
        let i = self.index(uid)?;
        if patch.name.is_none()
            && patch.duration_min.is_none()
            && patch.level.is_none()
            && patch.manual.is_none()
        {
            return Err(
                "task.set needs at least one of 'name', 'duration', 'level', 'manual'".into(),
            );
        }
        if patch.level.is_some_and(|lv| !(1..=20).contains(&lv)) {
            return Err("'level' must be 1..=20".into());
        }
        if patch.estimated.is_some() && patch.duration_min.is_none() {
            return Err("'estimated' needs a 'duration'".into());
        }
        if let Some(min) = patch.duration_min {
            validate_duration(min)?;
        }
        if patch.duration_min.is_some() {
            self.validate_cell_horizon(uid, patch.duration_min, None)?;
        }
        let t = &self.proj.tasks[i];
        // A manual summary's duration is its own span, not its stored one.
        let manual_summary = t.summary && t.manual && patch.manual.is_none_or(|m| m);
        let (current, milestone) = if manual_summary {
            (self.disp_duration_min(uid).unwrap_or(t.duration_min), None)
        } else {
            (t.duration_min, Some(t.milestone))
        };
        let duration_changed = patch.duration_min.is_some_and(|min| min != current);
        // A blank row always becomes a task: a mode is an edit of it too.
        let mode_changed = patch.manual.is_some_and(|m| m != t.manual || t.is_null);
        // A summary's `?` rolls up from its subtasks; it takes no estimate.
        let estimate_changed = !t.summary
            && patch
                .estimated
                .is_some_and(|e| estimate_after(t.estimated, e) != t.estimated);
        if patch.name.as_ref().is_none_or(|name| *name == t.name)
            && patch
                .duration_min
                .is_none_or(|min| min == current && milestone.is_none_or(|m| (min == 0) == m))
            && patch.level.is_none_or(|lv| lv == t.outline_level)
            && !mode_changed
            && !estimate_changed
        {
            return Ok(());
        }
        // Read before the edit: the pin is where the task is shown now.
        let pin = self.pin_at(i);
        self.edit_row(i, |proj, was_blank| {
            // A blank row's `1 day?` is a default, not a duration the user
            // typed: typing a duration replaces it, committing the estimate
            // unless typed with `?`.
            let t = &mut proj.tasks[i];
            if let Some(name) = patch.name {
                t.name = name;
            }
            let mut rescale = None;
            // The mode before the duration: a new duration then updates the
            // pin as it does for any manual task.
            if let Some(manual) = patch.manual.filter(|&m| m != t.manual) {
                let (start, finish, duration) = pin;
                t.manual = manual;
                t.manual_start = manual.then_some(start);
                t.manual_finish = finish.filter(|_| manual);
                t.manual_duration_min = manual.then(|| duration.unwrap_or(t.duration_min));
            }
            if let Some(min) = patch.duration_min.filter(|_| t.summary && t.manual) {
                // A manual summary's own span: its start stays and its
                // finish follows. Its stored duration, milestone flag and
                // work belong to the rollup and are left alone.
                t.manual_duration_min = Some(min);
                t.manual_finish = None;
            } else if let Some(min) = patch.duration_min {
                // Against the row as materialized: a blank row's default
                // `1 day?` (and a manual plan's ManualDuration) is replaced.
                let changed = was_blank || min != t.duration_min;
                rescale = changed.then_some((t.duration_min, min));
                // A summary's `?` rolls up from its subtasks: its own flag
                // stays as read.
                match patch.estimated {
                    _ if t.summary => {}
                    Some(e) => t.estimated = estimate_after(t.estimated, e),
                    None if changed => commit_estimate(t),
                    None => {}
                }
                t.milestone = min == 0;
                apply_duration(t, min, changed);
            }
            if let Some(lv) = patch.level {
                t.outline_level = lv;
            }
            // After the level, so a summary is judged by the outline the edit makes.
            if let Some((old, new)) = rescale {
                rescale_work(proj, i, old, new);
            }
        })?;
        // Only a date change restamps: a rename or a level change keeps the
        // Finish Project wrote, which our schedule can still differ from
        // (recurring calendar exceptions are not scheduled). A blank row's
        // new dates are stamped by edit_row; a newly pinned task's here.
        if duration_changed || (mode_changed && patch.manual == Some(true)) {
            self.stamp_pinned_dates(uid);
        }
        // A summary made automatic saves the span it now rolls up to, not
        // the manual dates stamped on it.
        if mode_changed && patch.manual == Some(false) && self.proj.tasks[i].summary {
            self.stamp_dates(i);
        }
        Ok(())
    }

    /// After an edit to a manual task's dates, record its pinned start and
    /// scheduled finish as the Start/Finish a save writes. Project does not
    /// reschedule manual tasks on open, so they must agree with
    /// ManualStart/Duration. A manual summary saves its manual dates too, as
    /// Project does. Stored dates do feed the scheduler (the anchor of a plan
    /// without a start date, and the timeline reach), so reschedule after
    /// stamping to keep the schedule in step with the model.
    fn stamp_pinned_dates(&mut self, uid: i32) {
        let Ok(i) = self.index(uid) else {
            return;
        };
        let task = &self.proj.tasks[i];
        if task.pinned_dates().is_none() && task.manual_summary_dates().is_none() {
            return;
        }
        self.stamp_dates(i);
    }

    /// Record row `i`'s scheduled start and finish as its stored dates.
    fn stamp_dates(&mut self, i: usize) {
        let task = &self.proj.tasks[i];
        let start = task
            .pinned_dates()
            .or_else(|| task.manual_summary_dates())
            .map(|(start, _)| start)
            .or_else(|| self.sched.get(task.uid).map(|r| r.early_start));
        let finish = self.sched.get(task.uid).map(|r| r.early_finish);
        let task = &mut self.proj.tasks[i];
        if (task.stored_start, task.stored_finish) == (start, finish) {
            return;
        }
        task.stored_start = start;
        task.stored_finish = finish;
        self.reschedule();
    }

    /// Link `pred` before `uid` with a working-time lag in minutes.
    pub fn add_predecessor(
        &mut self,
        uid: i32,
        pred: i32,
        link: LinkType,
        lag_min: i64,
    ) -> Result<(), String> {
        self.add_link(uid, Predecessor::working(pred, link, lag_min))
    }

    /// Add `link` to `uid`'s predecessors, keeping its lag format.
    pub fn add_link(&mut self, uid: i32, link: Predecessor) -> Result<(), String> {
        let pred = link.uid;
        let i = self.index(uid)?;
        let pred_id = self.proj.task(pred).map_or(pred, |task| task.id);
        if uid == pred || self.index(pred).is_err() || self.is_blank(pred) {
            return Err(format!("No other task with ID {pred_id}"));
        }
        if self.proj.tasks[i]
            .predecessors
            .iter()
            .any(|p| p.uid == pred)
        {
            return Err(format!("Already depends on {pred_id}"));
        }
        let mut predecessors = self.proj.tasks[i].predecessors.clone();
        predecessors.push(link);
        LinkGraph::new(self, i, &predecessors).check_link(pred)?;
        self.validate_cell_horizon(uid, None, Some(&predecessors))?;
        self.edit_row(i, |proj, _| {
            proj.tasks[i].predecessors = predecessors;
        })
    }

    pub fn remove_predecessor(&mut self, uid: i32, pred: i32) -> Result<(), String> {
        let i = self.index(uid)?;
        if !self.proj.tasks[i]
            .predecessors
            .iter()
            .any(|p| p.uid == pred)
        {
            return Err(format!("task {uid} has no predecessor {pred}"));
        }
        self.snapshot();
        self.proj.tasks[i].predecessors.retain(|p| p.uid != pred);
        self.changed();
        Ok(())
    }

    /// Task › Schedule › Unlink Tasks: remove every link touching `uid`, its
    /// predecessors and its successors, as one undo step. A blank row keeps
    /// being blank. Returns how many links were removed; none leaves history
    /// untouched.
    pub fn unlink_task(&mut self, uid: i32) -> Result<usize, String> {
        let i = self.index(uid)?;
        let links = self.proj.tasks[i].predecessors.len()
            + self
                .proj
                .tasks
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, t)| t.predecessors.iter().filter(|p| p.uid == uid).count())
                .sum::<usize>();
        if links == 0 {
            return Ok(0);
        }
        self.edit_structure(|proj| {
            for (j, t) in proj.tasks.iter_mut().enumerate() {
                if j == i {
                    t.predecessors.clear();
                } else {
                    t.predecessors.retain(|p| p.uid != uid);
                }
            }
        })?;
        Ok(links)
    }

    /// Set a task and, for a summary, its descendants active or inactive as
    /// one undoable edit. Blank rows inside the outline are left untouched.
    pub fn set_active(&mut self, uid: i32, active: bool) -> Result<usize, String> {
        let i = self.index(uid)?;
        let task = &self.proj.tasks[i];
        if task.is_null {
            return Err("A blank row cannot be inactivated".into());
        }
        if active && !self.proj.ancestors_active(i) {
            return Err("Its summary task is inactive".into());
        }
        let end = subtree_end(&self.proj, i);
        let changed: Vec<usize> = (i..end)
            .filter(|&j| !self.proj.tasks[j].is_null && self.proj.tasks[j].is_active() != active)
            .collect();
        if changed.is_empty() {
            return Ok(0);
        }
        let count = changed.len();
        self.edit_structure(|proj| {
            for j in changed {
                proj.tasks[j].active = Some(active);
            }
        })?;
        Ok(count)
    }

    pub fn set_constraint(&mut self, uid: i32, text: &str) -> Result<(), String> {
        let (constraint, constraint_date) = parse_constraint(text)?;
        self.set_constraint_typed(uid, constraint, constraint_date)
    }

    pub fn assign_resource(&mut self, uid: i32, name: &str) -> Result<AssignOutcome, String> {
        let i = self.index(uid)?;
        let name = name.trim();
        if name.is_empty() {
            if !self.proj.assignments.iter().any(|a| a.task_uid == uid) {
                return Ok(AssignOutcome::NothingToClear);
            }
            self.snapshot();
            self.proj.assignments.retain(|a| a.task_uid != uid);
            self.changed();
            return Ok(AssignOutcome::Cleared);
        }
        let mut resources = self.proj.resources.clone();
        // A resource literally named like `Crew [A%]` wins over the units suffix.
        let (rid, units) = match resources.iter().find(|r| r.name.eq_ignore_ascii_case(name)) {
            Some(r) => (r.uid, None),
            None => {
                let (base, inner) = parse_resource_token(name)?;
                let rid = find_or_stage_resource(&mut resources, base.trim())?;
                let r = resources.iter().find(|r| r.uid == rid).expect("staged");
                (rid, bracket_units(inner, r, name)?)
            }
        };
        // A blank row is assigned as the task the edit makes it.
        let duration = self.row_as_edited(i).duration_min;
        if let Some(k) = self
            .proj
            .assignments
            .iter()
            .position(|a| a.task_uid == uid && a.resource_uid == rid)
        {
            // Only different explicit units change an existing assignment.
            let kind = resources.iter().find(|r| r.uid == rid).map(|r| r.kind);
            let current = self.proj.assignments[k].units;
            let Some(u) = units.filter(|&u| !same_shown_units(kind, u, current)) else {
                return Ok(AssignOutcome::AlreadyAssigned);
            };
            let u = checked_units(u, name)?;
            let mut assignments = self.proj.assignments.clone();
            assignments[k].set_units(u, assigned_work(kind, duration, u));
            self.commit_assignments(i, resources, assignments)?;
            return Ok(AssignOutcome::Assigned);
        }
        let units = match units {
            Some(u) => checked_units(u, name)?,
            None => default_units(resources.iter().find(|r| r.uid == rid).expect("staged")),
        };
        let mut next_aid = self
            .proj
            .assignments
            .iter()
            .map(|a| a.uid)
            .max()
            .unwrap_or(0);
        let kind = resources.iter().find(|r| r.uid == rid).map(|r| r.kind);
        let mut assignments = self.proj.assignments.clone();
        assignments.push(new_assignment(
            &mut next_aid,
            uid,
            rid,
            kind,
            units,
            duration,
        )?);
        self.commit_assignments(i, resources, assignments)?;
        Ok(AssignOutcome::Assigned)
    }

    /// Project › Schedule › Set Baseline: record the plan as it is scheduled
    /// now in the Baseline (slot 0) of every scheduled task, and of the
    /// assignments and resources (see `baseline_assignments`), as one undo
    /// step. Baseline1..10 stay.
    pub fn set_baseline(&mut self) {
        self.snapshot();
        let baselines: Vec<_> = self
            .proj
            .tasks
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                let r = self.sched.get(t.uid)?;
                Some((
                    i,
                    Baseline {
                        number: 0,
                        start: Some(r.early_start),
                        finish: Some(r.early_finish),
                        duration_min: Some(crate::schedule::summary_or_leaf_min(
                            &self.proj,
                            t,
                            r.early_start,
                            r.early_finish,
                        )),
                        ..Baseline::default()
                    },
                ))
            })
            .collect();
        for (i, baseline) in baselines {
            self.proj.tasks[i].set_baseline_slot(baseline);
        }
        self.baseline_assignments();
        self.changed();
    }

    /// Set Baseline's part for assignments and resources, as Project writes
    /// it: each assignment on a scheduled task records its span as the
    /// schedule now places it and its stored work and cost (the cost priced
    /// when it has none), and each resource with assignments records its
    /// stored work and cost (else their sums), without earned value. Their
    /// timephased Baseline records described the old plan and go, as Clear
    /// Baseline drops them; none is synthesised. Baseline1..10 stay.
    fn baseline_assignments(&mut self) {
        let proj = &self.proj;
        let mut costs: std::collections::HashMap<i32, Option<f64>> = Default::default();
        let recorded: Vec<(usize, AssignmentBaseline)> = proj
            .assignments
            .iter()
            .enumerate()
            .filter_map(|(k, a)| {
                let span = crate::assign::assignment_span(proj, &self.sched, a)?;
                let cost = a
                    .cost
                    .clone()
                    .or_else(|| crate::assign::assignment_cost(proj, a, span));
                Some((
                    k,
                    AssignmentBaseline {
                        number: 0,
                        start: Some(span.0),
                        finish: Some(span.1),
                        work_min: Some(a.work_min),
                        cost,
                    },
                ))
            })
            .collect();
        for (k, b) in &recorded {
            let cost = b.cost.as_ref().map(|c| c.to_f64().unwrap_or(0.0));
            let total = costs
                .entry(proj.assignments[*k].resource_uid)
                .or_insert(Some(0.0));
            *total = total.zip(cost).map(|(t, c)| t + c);
        }
        let resources: Vec<(usize, ResourceBaseline)> = proj
            .resources
            .iter()
            .enumerate()
            .filter(|(_, r)| proj.assignments.iter().any(|a| a.resource_uid == r.uid))
            .map(|(j, r)| {
                let work: i64 = proj
                    .assignments
                    .iter()
                    .filter(|a| a.resource_uid == r.uid)
                    .map(|a| a.work_min)
                    .sum();
                let cost = r.cost.clone().or_else(|| {
                    costs
                        .get(&r.uid)
                        .copied()
                        .flatten()
                        .and_then(crate::assign::money)
                });
                (
                    j,
                    ResourceBaseline {
                        number: 0,
                        work_min: Some(r.work_min.unwrap_or(work)),
                        cost,
                        bcws: None,
                        bcwp: None,
                    },
                )
            })
            .collect();
        for (k, baseline) in recorded {
            let a = &mut self.proj.assignments[k];
            a.set_baseline_slot(baseline);
            a.timephased_data.retain(|t| !t.is_baseline_slot_zero());
        }
        for (j, baseline) in resources {
            let r = &mut self.proj.resources[j];
            r.set_baseline_slot(baseline);
            r.timephased_data.retain(|t| !t.is_baseline_slot_zero());
        }
    }

    /// Project › Schedule › Clear Baseline: remove the Baseline (slot 0) from
    /// every task, resource and assignment, including the resources' and
    /// assignments' timephased Baseline work and cost, as one undo step;
    /// Baseline1..10 stay. `false`, with history untouched, when there is
    /// none to clear.
    pub fn clear_baseline(&mut self) -> Result<bool, String> {
        let proj = &self.proj;
        let has_task = proj.tasks.iter().any(|t| t.baseline(0).is_some());
        let has_resource = proj.resources.iter().any(|r| {
            r.baseline(0).is_some()
                || r.timephased_data
                    .iter()
                    .any(TimephasedValue::is_baseline_slot_zero)
        });
        let has_assignment = proj.assignments.iter().any(|a| {
            a.baseline(0).is_some()
                || a.timephased_data
                    .iter()
                    .any(TimephasedValue::is_baseline_slot_zero)
        });
        if !has_task && !has_resource && !has_assignment {
            return Ok(false);
        }
        self.edit_structure(|proj| {
            for t in &mut proj.tasks {
                t.baselines.retain(|b| b.number != 0);
            }
            for r in &mut proj.resources {
                r.baselines.retain(|b| b.number != 0);
                r.timephased_data.retain(|t| !t.is_baseline_slot_zero());
            }
            for a in &mut proj.assignments {
                a.baselines.retain(|b| b.number != 0);
                a.timephased_data.retain(|t| !t.is_baseline_slot_zero());
            }
        })?;
        Ok(true)
    }

    pub fn toggle_milestone(&mut self, uid: i32) -> Result<(), String> {
        let i = self.index(uid)?;
        // A blank row toggles from the duration it gets as a task.
        let min = if self.row_as_edited(i).duration_min == 0 {
            480
        } else {
            0
        };
        // Not a typed duration: the estimate follows the old rule.
        self.update_task(
            uid,
            TaskPatch {
                duration_min: Some(min),
                ..TaskPatch::default()
            },
        )
    }
}

/// Task creation and updates share the same non-negative duration rule.
fn validate_duration(minutes: i64) -> Result<(), String> {
    if minutes < 0 {
        return Err("Duration must not be negative".into());
    }
    Ok(())
}

/// Both assignment entry points stage resources before taking an undo snapshot.
fn find_or_stage_resource(resources: &mut Vec<Resource>, name: &str) -> Result<i32, String> {
    if let Some(resource) = resources.iter().find(|r| r.name.eq_ignore_ascii_case(name)) {
        return Ok(resource.uid);
    }
    let uid = resources
        .iter()
        .map(|r| r.uid)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or("No resource IDs available")?;
    let id = resources
        .iter()
        .map(|r| r.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or("No resource IDs available")?;
    resources.push(Resource {
        uid,
        id,
        name: name.into(),
        kind: ResourceType::Work,
        max_units: 1.0,
        ..Resource::default()
    });
    Ok(uid)
}

/// A work resource is assigned at its Max. Units, capped at 100% (as in Project);
/// other kinds, and unusable capacities, at 100%.
fn default_units(r: &Resource) -> f64 {
    if r.kind == ResourceType::Work && r.max_units.is_finite() && r.max_units > 0.0 {
        r.max_units.min(1.0)
    } else {
        1.0
    }
}

/// Assignment work: the task duration scaled by the units (saturating).
fn work_for(duration_min: i64, units: f64) -> i64 {
    (duration_min as f64 * units).round() as i64
}

/// The work an assignment is given at `units` on a task of `duration_min`,
/// by its resource's kind: a work resource (or none) works the duration at
/// its units; a material's work is its quantity, the units, in hours (Project
/// 2024, paired corpus 21: 1 unit is `PT1H`); a cost resource has none.
fn assigned_work(kind: Option<ResourceType>, duration_min: i64, units: f64) -> i64 {
    match kind {
        Some(ResourceType::Material) => (units * 60.0).round() as i64,
        Some(ResourceType::Cost) => 0,
        Some(ResourceType::Work) | None => work_for(duration_min, units),
    }
}

/// A duration change on row `i` rescales its assignments' work to duration x
/// units, as Project does for a fixed-units or fixed-duration task. A
/// fixed-work task keeps its work and its units follow instead (see
/// [`effort::fixed_work_units`]); a summary's stored duration is not the one
/// it shows, and material and cost work is not time, so those are left as
/// read. A contoured assignment's units are its peak, so its contour stretches
/// and its work scales with the duration instead. Without a basis to scale
/// from, its work is duration x units once it has none (as after a milestone),
/// and stays as read when a zero duration holds some. What described the old
/// work is dropped as a units edit drops it ([`Assignment::set_work`]);
/// progress is kept. A flat assignment delayed into its task works only from
/// its `Delay` on, so it still finishes with the task.
fn rescale_work(proj: &mut Project, i: usize, old_min: i64, new_min: i64) {
    let t = &proj.tasks[i];
    if proj.is_outline_summary(i) {
        return;
    }
    if t.task_type == Some(TaskType::FixedWork) {
        effort::fixed_work_units(proj, i, new_min);
        return;
    }
    let uid = t.uid;
    let Project {
        assignments,
        resources,
        ..
    } = proj;
    for a in assignments.iter_mut().filter(|a| a.task_uid == uid) {
        let kind = resources.iter().find(|r| r.uid == a.resource_uid);
        if kind.is_some_and(|r| r.kind != ResourceType::Work) {
            continue;
        }
        let work_min = match a.work_contour {
            // A delayed assignment works from its delay to the task finish.
            None | Some(0) => work_for((new_min - a.delay_min()).max(0), a.units),
            Some(_) if old_min > 0 && a.work_min > 0 => {
                (a.work_min as f64 * new_min as f64 / old_min as f64).round() as i64
            }
            Some(_) if a.work_min > 0 => continue,
            Some(_) => work_for(new_min, a.units),
        };
        a.set_work(work_min);
    }
}

/// Explicitly entered units must be positive to create or change an assignment.
fn checked_units(units: f64, token: &str) -> Result<f64, String> {
    if units > 0.0 {
        Ok(units)
    } else {
        Err(format!("Invalid units in '{}'", token.trim()))
    }
}

fn new_assignment(
    next_uid: &mut i32,
    task_uid: i32,
    resource_uid: i32,
    kind: Option<ResourceType>,
    units: f64,
    duration_min: i64,
) -> Result<Assignment, String> {
    *next_uid = next_uid
        .checked_add(1)
        .ok_or("No assignment IDs available")?;
    Ok(Assignment {
        uid: *next_uid,
        task_uid,
        resource_uid,
        units,
        work_min: assigned_work(kind, duration_min, units),
        ..Assignment::default()
    })
}

/// The outline level of a task placed at row `at`, whether inserted there or
/// a blank row there becoming a task. As in Microsoft Project it is the next
/// sibling of the nearest task above, or that task's first child when it is
/// a summary (so the summary keeps its children); the rows below play no
/// part. Blank rows are outside the outline and skipped. At least 1.
///
/// When the nearest task above is hidden by a collapsed summary and the
/// row the task is placed from (`anchor`: the row it goes after, or the
/// blank row itself; none when appending) is shown, the summary is the row
/// the user sees above, so the task becomes its sibling. From a hidden
/// anchor, the level is the one it would be with the summary expanded.
fn level_at(
    proj: &Project,
    at: usize,
    anchor: Option<usize>,
    collapsed: &std::collections::BTreeSet<i32>,
) -> u32 {
    let Some(above) = proj.tasks[..at].iter().rposition(|t| !t.is_null) else {
        return 1;
    };
    let owners = outline::hidden_owners(proj, collapsed);
    // A hidden anchor leaves the rows around it as they are: existing tasks
    // keep their parents (see `outline::hidden_owners`).
    if let Some(owner) = owners[above]
        && anchor.is_none_or(|a| owners[a].is_none())
    {
        return proj.tasks[owner].outline_level.max(1);
    }
    let level = proj.tasks[above].outline_level;
    // A summary's first child is deeper still, so `+ 1` stays within 20.
    if proj.is_outline_summary(above) {
        level + 1
    } else {
        level.max(1)
    }
}

/// Row `i` as it becomes when edited. Typing into a blank row turns it into a
/// task, as in Project: it joins the outline where it sits when it has no
/// level, without a duration it gets Project's new-task default, `1 day?`
/// (`1 day` when the plan's new tasks are not estimated), instead of turning
/// into a milestone, it takes the plan's stated task type and effort-driven
/// default, and in a plan whose new tasks are manual it becomes manual at
/// `start`, as [`Editor::add_task`] makes one.
fn materialized(
    proj: &Project,
    i: usize,
    start: DateTime,
    collapsed: &std::collections::BTreeSet<i32>,
) -> Task {
    let mut t = proj.tasks[i].clone();
    if t.is_null {
        t.is_null = false;
        if t.outline_level == 0 {
            t.outline_level = level_at(proj, i, Some(i), collapsed);
        }
        if t.duration_min == 0 {
            t.duration_min = proj.days_to_minutes(1.0);
            // Estimated when the plan's new tasks are; otherwise unset, so
            // the saved task has no `<Estimated>`.
            t.estimated = proj.new_tasks_estimated().then_some(true);
            t.milestone = false;
        }
        t.task_type = t.task_type.or(proj.default_task_type);
        t.effort_driven = t.effort_driven.or(proj.new_tasks_effort_driven);
        if proj.new_tasks_are_manual && !t.manual {
            t.manual = true;
            t.manual_start = t.manual_start.or(Some(start));
            t.manual_duration_min = t.manual_duration_min.or(Some(t.duration_min));
        }
    }
    t
}

/// The prospective outline and dependency graph for one predecessor edit.
/// Only tasks are copied; `row_as_edited` supplies the row a blank successor
/// would become when the edit commits.
struct LinkGraph {
    tasks: Vec<Task>,
    succ_i: usize,
    successors: std::collections::HashMap<i32, Vec<i32>>,
    ancestors: std::collections::HashMap<i32, Vec<i32>>,
    indexes: std::collections::HashMap<i32, usize>,
}

impl LinkGraph {
    fn new(ed: &Editor, succ_i: usize, proposed: &[Predecessor]) -> Self {
        use std::collections::HashMap;

        let mut tasks = ed.proj.tasks.clone();
        if tasks[succ_i].is_null {
            tasks[succ_i] = ed.row_as_edited(succ_i);
        }
        tasks[succ_i].predecessors = proposed.to_vec();
        let mut successors: HashMap<i32, Vec<i32>> = HashMap::new();
        let mut ancestors = HashMap::new();
        let mut indexes = HashMap::new();
        let mut stack: Vec<(u32, i32)> = Vec::new();
        for (i, task) in tasks.iter().enumerate() {
            if task.is_null {
                continue;
            }
            while stack
                .last()
                .is_some_and(|(level, _)| *level >= task.outline_level)
            {
                stack.pop();
            }
            let mut lineage: Vec<i32> = stack.iter().map(|(_, uid)| *uid).collect();
            lineage.push(task.uid);
            ancestors.insert(task.uid, lineage);
            indexes.insert(task.uid, i);
            stack.push((task.outline_level, task.uid));
            for predecessor in &task.predecessors {
                successors
                    .entry(predecessor.uid)
                    .or_default()
                    .push(task.uid);
            }
        }
        Self {
            tasks,
            succ_i,
            successors,
            ancestors,
            indexes,
        }
    }

    fn check_link(&self, pred_uid: i32) -> Result<(), String> {
        use std::collections::{HashSet, VecDeque};

        let pred_i = self.indexes[&pred_uid]; // caller checked a non-blank predecessor
        let succ_id = self.tasks[self.succ_i].id;
        let pred_id = self.tasks[pred_i].id;
        if (self.succ_i < pred_i && pred_i < outline::subtree_end_in(&self.tasks, self.succ_i))
            || (pred_i < self.succ_i && self.succ_i < outline::subtree_end_in(&self.tasks, pred_i))
        {
            return Err(format!(
                "Tasks {succ_id} and {pred_id} are a summary and its subtask; they cannot be linked"
            ));
        }

        // Summary links connect their leaves. Expand each reachable task once
        // so a link between large summaries does not enqueue a cross product.
        let targets: HashSet<i32> = link_leaves(&self.tasks, pred_i).into_iter().collect();
        let mut visited: HashSet<i32> = HashSet::new();
        let mut queue = VecDeque::new();
        for leaf in link_leaves(&self.tasks, self.succ_i) {
            if visited.insert(leaf) {
                queue.push_back(leaf);
            }
        }
        let mut expanded_ancestors = HashSet::new();
        let mut expanded_successors = HashSet::new();
        while let Some(leaf) = queue.pop_front() {
            if targets.contains(&leaf) {
                return Err(format!(
                    "Linking task {succ_id} to task {pred_id} would create a circular relationship"
                ));
            }
            for ancestor in &self.ancestors[&leaf] {
                if !expanded_ancestors.insert(*ancestor) {
                    continue;
                }
                if let Some(next_tasks) = self.successors.get(ancestor) {
                    for next_uid in next_tasks {
                        if !expanded_successors.insert(*next_uid) {
                            continue;
                        }
                        for next_leaf in link_leaves(&self.tasks, self.indexes[next_uid]) {
                            if visited.insert(next_leaf) {
                                queue.push_back(next_leaf);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Non-blank outline leaves below a row, including the row itself when it
/// has no children.
fn link_leaves(tasks: &[Task], i: usize) -> Vec<i32> {
    let end = outline::subtree_end_in(tasks, i);
    (i..end)
        .filter(|&j| !tasks[j].is_null && !crate::model::is_outline_summary_in(tasks, j))
        .map(|j| tasks[j].uid)
        .collect()
}

/// Link the task just inserted at row `at` into the chain it split, as
/// Project's Autolink does: when the task above it, A, has a finish-to-start
/// link to the next task below it, B, and A, the new task N and B are leaves
/// at one outline level, A->B becomes A->N (no lag) and N->B (with A->B's
/// lag). Otherwise, or with the plan's `Autolink` off, nothing changes.
/// Returns whether it linked the task.
fn autolink(proj: &mut Project, at: usize) -> bool {
    if !proj.autolink() || at == 0 {
        return false;
    }
    let (a, n) = (at - 1, at);
    let Some(b) = (at + 1..proj.tasks.len()).find(|&i| !proj.tasks[i].is_null) else {
        return false;
    };
    let leaf = |i: usize| {
        let t = &proj.tasks[i];
        !t.is_null && !t.summary && !proj.is_outline_summary(i)
    };
    let level = proj.tasks[n].outline_level;
    if !(leaf(a) && leaf(n) && leaf(b))
        || proj.tasks[a].outline_level != level
        || proj.tasks[b].outline_level != level
    {
        return false;
    }
    let (a_uid, n_uid) = (proj.tasks[a].uid, proj.tasks[n].uid);
    let Some(link) = proj.tasks[b]
        .predecessors
        .iter_mut()
        .find(|p| p.uid == a_uid && p.link == LinkType::FinishStart)
    else {
        return false;
    };
    link.uid = n_uid;
    proj.tasks[n].predecessors = vec![Predecessor::fs(a_uid)];
    true
}

/// Give task `t` duration `min`. A manual task keeps its start; its finish
/// follows a new duration instead of staying pinned, while repeating the
/// current duration (`changed` false) keeps a pinned finish.
fn apply_duration(t: &mut Task, min: i64, changed: bool) {
    t.duration_min = min;
    if t.manual && changed {
        t.manual_duration_min = Some(min);
        t.manual_finish = None;
    }
}

/// Typing a duration without `?` commits an estimated one, as in Project.
fn commit_estimate(t: &mut Task) {
    t.estimated = estimate_after(t.estimated, false);
}

/// A task's `Estimated` after a duration typed with (`true`) or without `?`:
/// `?` marks it estimated; without, an estimate is committed and an unset
/// flag stays unset, so an untouched import saves as it was read.
fn estimate_after(current: Option<bool>, typed: bool) -> Option<bool> {
    match (typed, current) {
        (true, _) => Some(true),
        (false, Some(true)) => Some(false),
        (false, other) => other,
    }
}

/// `"?"` after the duration of task `uid` when it is estimated, as Project
/// shows `1 day?`, else `""`. A summary shows it when any task below it is
/// estimated (the rollup is shown, not saved: see #159). Milestones and
/// blank rows show none.
pub fn duration_suffix(proj: &Project, uid: i32) -> &'static str {
    let Some(i) = proj.tasks.iter().position(|t| t.uid == uid) else {
        return "";
    };
    let leaf = |t: &Task| !t.is_null && !t.is_milestone() && t.estimated == Some(true);
    let estimated = if proj.is_outline_summary(i) {
        // A nested summary's own flag is stale too: only leaves count.
        (i + 1..subtree_end(proj, i)).any(|k| leaf(&proj.tasks[k]) && !proj.is_outline_summary(k))
    } else {
        leaf(&proj.tasks[i])
    };
    if estimated { "?" } else { "" }
}

/// A task duration as typed: [`parse_duration`] with Project's optional
/// trailing `?` for an estimate (`3d?`, `4h?`, `2?`). Returns the minutes and
/// whether it was estimated.
pub fn parse_task_duration(text: &str, proj: &Project) -> Option<(i64, bool)> {
    let text = text.trim();
    match text.strip_suffix('?') {
        Some(rest) if !rest.trim_end().ends_with('?') => {
            parse_duration(rest, proj).map(|min| (min, true))
        }
        Some(_) => None,
        None => parse_duration(text, proj).map(|min| (min, false)),
    }
}

fn recompute_summaries(proj: &mut Project) {
    for i in 0..proj.tasks.len() {
        proj.tasks[i].summary = proj.is_outline_summary(i);
    }
}

/// Parse the TUI's duration units using the project's working-day length.
pub fn parse_duration(text: &str, proj: &Project) -> Option<i64> {
    let t = text.trim().to_lowercase();
    let (num, unit) = t
        .strip_suffix(['d', 'h', 'w', 'm'])
        .map(|n| (n, t.chars().last().unwrap()))
        .unwrap_or((t.as_str(), 'd'));
    // Exact minute literals must not lose integer precision through f64.
    if unit == 'm' {
        if let Ok(minutes) = num.trim().parse::<i64>() {
            return Some(minutes);
        }
    }
    let v: f64 = num.trim().parse().ok()?;
    let minutes = match unit {
        'h' => v * 60.0,
        'w' => v * proj.hours_per_week * 60.0,
        'm' => v,
        _ => v * proj.hours_per_day * 60.0,
    }
    .round();
    (minutes.is_finite() && minutes > i64::MIN as f64 && minutes < i64::MAX as f64)
        .then_some(minutes as i64)
}

/// Parse `TYPE [date]`, retaining the TUI's input and rejection conventions.
pub fn parse_constraint(text: &str) -> Result<(ConstraintType, Option<DateTime>), String> {
    let mut it = text.split_whitespace();
    let kind = it.next().unwrap_or("").to_ascii_lowercase();
    let ctype = match kind.as_str() {
        "none" | "asap" => ConstraintType::AsSoonAsPossible,
        "alap" => ConstraintType::AsLateAsPossible,
        "snet" => ConstraintType::StartNoEarlierThan,
        "snlt" => ConstraintType::StartNoLaterThan,
        "fnet" => ConstraintType::FinishNoEarlierThan,
        "fnlt" => ConstraintType::FinishNoLaterThan,
        "mso" => ConstraintType::MustStartOn,
        "mfo" => ConstraintType::MustFinishOn,
        _ => return Err("Constraint: TYPE [date] — SNET/SNLT/FNET/FNLT/MSO/MFO/ALAP/none".into()),
    };
    let needs_date = !matches!(
        ctype,
        ConstraintType::AsSoonAsPossible | ConstraintType::AsLateAsPossible
    );
    let date = it.next().and_then(DateTime::parse_mspdi);
    if needs_date && date.is_none() {
        return Err(format!(
            "{} needs a date, e.g. {kind} 2026-03-05",
            kind.to_uppercase()
        ));
    }
    Ok((ctype, if needs_date { date } else { None }))
}

/// Prompt prefill; ASAP intentionally uses an empty string.
pub fn constraint_hint(t: &Task) -> String {
    if t.constraint == ConstraintType::AsSoonAsPossible {
        return String::new();
    }
    let code = t.constraint.abbrev();
    match t.constraint_date {
        Some(d) => {
            let p = d.parts();
            format!("{code} {:04}-{:02}-{:02}", p.year, p.month, p.day)
        }
        None => code.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::TaskResult;

    #[test]
    fn restored_dirty_state_survives_empty_history() {
        for dirty in [false, true] {
            let mut ed = Editor::restored(untitled_project(), dirty);
            assert_eq!(ed.dirty(), dirty);
            assert_eq!((ed.undo_depth(), ed.redo_depth()), (0, 0));
            assert!(!ed.undo());
            assert!(!ed.redo());
            assert_eq!(ed.dirty(), dirty);
            ed.mark_saved();
            assert!(!ed.dirty());
            assert_schedule(&ed);
        }
    }

    #[test]
    fn untitled_project_has_shared_anchor_and_no_tasks() {
        let p = untitled_project();
        assert!(p.tasks.is_empty());
        assert_eq!(p.name, "Untitled");
        assert_eq!(p.start_date, Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)));
    }

    fn editor() -> Editor {
        Editor::new(Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
            tasks: (1..=2)
                .map(|uid| Task {
                    uid,
                    id: uid,
                    name: format!("Task {uid}"),
                    outline_level: 1,
                    duration_min: 480,
                    ..Task::default()
                })
                .collect(),
            ..Project::default()
        })
    }

    #[test]
    fn set_baseline_captures_durations_and_preserves_other_slots_and_history() {
        let saved = Baseline {
            number: 1,
            duration_min: Some(2400),
            ..Baseline::default()
        };
        let mut proj = editor().project().clone();
        proj.tasks = vec![
            Task {
                uid: 1,
                outline_level: 1,
                duration_min: 99,
                ..Task::default()
            },
            Task {
                uid: 2,
                outline_level: 2,
                duration_min: 960,
                ..Task::default()
            },
            Task {
                uid: 3,
                outline_level: 1,
                duration_min: 0,
                milestone: true,
                ..Task::default()
            },
        ];
        for task in &mut proj.tasks {
            task.set_baseline_slot(saved.clone());
            task.set_baseline_slot(Baseline {
                duration_min: Some(60),
                ..Baseline::default()
            });
        }
        let mut ed = Editor::new(proj);
        let before = ed.project().clone();
        ed.set_baseline();
        assert!(ed.project().tasks[0].summary);
        assert_eq!(ed.project().tasks[0].duration_min, 99);
        for (task, expected) in ed.project().tasks.iter().zip([960, 960, 0]) {
            let baseline = task.baseline(0).unwrap();
            let r = ed.schedule().get(task.uid).unwrap();
            assert_eq!(baseline.duration_min, Some(expected));
            assert_eq!(baseline.start, Some(r.early_start));
            assert_eq!(baseline.finish, Some(r.early_finish));
            assert_eq!(task.baseline(1), Some(&saved));
        }
        let after = ed.project().clone();
        assert!(ed.undo());
        assert_eq!(ed.project(), &before);
        assert!(ed.redo());
        assert_eq!(ed.project(), &after);
        ed.set_duration_min(2, 1440, false).unwrap();
        assert_eq!(
            ed.project().tasks[1].baseline(0).unwrap().duration_min,
            Some(960)
        );
    }

    #[test]
    fn set_baseline_captures_assignments_and_resources() {
        use crate::model::{AssignmentBaseline, Rate, ResourceBaseline};
        let rate = |text: &str| Rate::parse(text);
        let record = |kind| TimephasedValue {
            kind,
            value: Some("PT8H0M0S".into()),
            ..TimephasedValue::default()
        };
        let resource = |uid, cost: Option<Rate>| Resource {
            uid,
            id: uid,
            name: format!("R{uid}"),
            max_units: 1.0,
            standard_rate: rate("50"),
            work_min: Some(7),
            cost,
            ..Resource::default()
        };
        let mut proj = editor().project().clone();
        // R1 stores its totals; R2 stores none, so its cost is the sum.
        proj.resources = vec![resource(1, rate("7")), resource(2, None)];
        proj.resources[1].work_min = None;
        let old_slot = ResourceBaseline {
            number: 1,
            work_min: Some(1),
            ..ResourceBaseline::default()
        };
        for r in &mut proj.resources {
            r.set_baseline_slot(old_slot.clone());
            r.set_baseline_slot(ResourceBaseline {
                number: 0,
                bcws: rate("5"),
                bcwp: rate("5"),
                ..ResourceBaseline::default()
            });
            r.timephased_data = vec![record(7), record(8), record(20)];
        }
        let assignment = |uid, task_uid, resource_uid, cost: Option<Rate>| Assignment {
            uid,
            task_uid,
            resource_uid,
            units: 1.0,
            work_min: 480,
            // Stored dates the schedule would not give it.
            start: Some(DateTime::from_ymd_hm(2026, 1, 1, 8, 0)),
            finish: Some(DateTime::from_ymd_hm(2026, 1, 1, 9, 0)),
            cost,
            baselines: vec![AssignmentBaseline {
                number: 1,
                work_min: Some(1),
                ..AssignmentBaseline::default()
            }],
            timephased_data: vec![record(1), record(4), record(5), record(16)],
            ..Assignment::default()
        };
        proj.assignments = vec![
            assignment(1, 1, 1, rate("1234")),
            assignment(2, 2, 2, None),
            assignment(3, 1, 2, rate("100")),
        ];
        let mut ed = Editor::new(proj);
        let before = ed.project().clone();
        ed.set_baseline();
        let p = ed.project();
        let monday = DateTime::from_ymd_hm(2026, 1, 5, 8, 0);
        // The span as scheduled now (both tasks start Monday), the stored
        // cost, else the priced one.
        for (a, cost) in p.assignments.iter().zip(["1234", "40000", "100"]) {
            let b = a.baseline(0).unwrap();
            assert_eq!(b.start, Some(monday), "{}", a.uid);
            assert_eq!(b.finish, Some(monday.add_minutes(9 * 60)), "{}", a.uid);
            assert_eq!(b.work_min, Some(480));
            assert_eq!(b.cost.as_ref().map(Rate::as_str), Some(cost), "{}", a.uid);
            assert_eq!(a.baseline(1).unwrap().work_min, Some(1));
            let kinds: Vec<u8> = a.timephased_data.iter().map(|t| t.kind).collect();
            assert_eq!(kinds, [1, 16]);
        }
        // Stored totals, else the sums; no earned value.
        let recorded: Vec<_> = p
            .resources
            .iter()
            .map(|r| {
                let b = r.baseline(0).unwrap();
                (
                    b.work_min,
                    b.cost.as_ref().map(Rate::as_str),
                    b.bcws.clone(),
                    b.bcwp.clone(),
                )
            })
            .collect();
        assert_eq!(
            recorded,
            [
                (Some(7), Some("7"), None, None),
                (Some(960), Some("40100"), None, None)
            ]
        );
        for r in &p.resources {
            assert_eq!(r.baseline(1), Some(&old_slot));
            let kinds: Vec<u8> = r.timephased_data.iter().map(|t| t.kind).collect();
            assert_eq!(kinds, [20]);
        }
        // Setting a baseline changes no current value, and is one undo step.
        assert_eq!(p.assignments[1].cost, None);
        assert_eq!(ed.undo_depth(), 1);
        assert!(ed.undo());
        assert_eq!(ed.project(), &before);
    }

    #[test]
    fn stamping_a_manual_task_leaves_the_schedule_in_step_with_the_model() {
        // No start date: the anchor comes from the earliest stored start.
        let march = |day| DateTime::from_ymd_hm(2026, 3, day, 8, 0);
        let proj = Project {
            tasks: vec![
                Task {
                    uid: 1,
                    id: 1,
                    name: "M".into(),
                    outline_level: 1,
                    duration_min: 480,
                    manual: true,
                    manual_start: Some(march(2)),
                    stored_start: Some(march(2)),
                    ..Task::default()
                },
                Task {
                    uid: 2,
                    id: 2,
                    name: "A".into(),
                    outline_level: 1,
                    duration_min: 480,
                    ..Task::default()
                },
            ],
            ..Project::default()
        };
        let mut ed = Editor::new(proj);
        ed.set_start(1, DateTime::from_ymd_hm(2026, 3, 16, 0, 0))
            .unwrap();
        assert_schedule(&ed);
        assert_eq!(
            ed.schedule().get(2).unwrap().early_start,
            march(16),
            "the unlinked auto task follows the moved anchor now, not on the next edit"
        );
    }

    fn results(sched: &Schedule) -> Vec<TaskResult> {
        let mut results: Vec<_> = sched.results().copied().collect();
        results.sort_by_key(|r| r.uid);
        results
    }

    fn assert_schedule(ed: &Editor) {
        let expected = schedule(ed.project());
        assert_eq!(results(ed.schedule()), results(&expected));
        assert_eq!(ed.schedule().project_start, expected.project_start);
        assert_eq!(ed.schedule().project_finish, expected.project_finish);
        if ed.leveled() {
            let expected = level(ed.project());
            for t in &ed.project().tasks {
                assert_eq!(ed.disp_start(t.uid), expected.start(t.uid));
                assert_eq!(ed.disp_finish(t.uid), expected.finish(t.uid));
            }
            assert_eq!(
                ed.level.as_ref().unwrap().project_finish,
                expected.project_finish
            );
        }
    }

    // Compare all state, including actual history contents and derived dates.
    fn assert_unchanged(ed: &mut Editor, action: impl FnOnce(&mut Editor)) {
        let proj = ed.proj.clone();
        let undo = ed.undo.clone();
        let redo = ed.redo.clone();
        let flags = (ed.dirty, ed.sel, ed.leveled, ed.last_find.clone());
        let sched = ed.sched.clone();
        let dates: Vec<_> = ed
            .proj
            .tasks
            .iter()
            .map(|t| (t.uid, ed.disp_start(t.uid), ed.disp_finish(t.uid)))
            .collect();
        let level_finish = ed.level.as_ref().map(|l| l.project_finish);
        action(ed);
        assert_eq!(ed.proj, proj);
        assert_eq!(ed.undo, undo);
        assert_eq!(ed.redo, redo);
        assert_eq!((ed.dirty, ed.sel, ed.leveled, ed.last_find.clone()), flags);
        assert_eq!(results(&ed.sched), results(&sched));
        assert_eq!(ed.sched.project_start, sched.project_start);
        assert_eq!(ed.sched.project_finish, sched.project_finish);
        assert_eq!(ed.level.as_ref().map(|l| l.project_finish), level_finish);
        for (uid, start, finish) in dates {
            assert_eq!(ed.disp_start(uid), start);
            assert_eq!(ed.disp_finish(uid), finish);
        }
    }

    fn project_with_unused_empty_calendar(empty_default: bool) -> Project {
        let mut proj = untitled_project();
        proj.calendars.push(crate::model::Calendar::base(
            3,
            "Closed",
            Default::default(),
        ));
        proj.tasks = vec![
            Task {
                uid: 1,
                name: "Phase".into(),
                outline_level: 1,
                summary: true,
                calendar_uid: Some(3),
                ..Task::default()
            },
            Task {
                uid: 2,
                name: "Build".into(),
                outline_level: 2,
                calendar_uid: Some(1),
                duration_min: 480,
                ..Task::default()
            },
        ];
        if empty_default {
            proj.default_calendar_uid = 3;
        }
        // Exercise the same acceptance path used when opening a file.
        crate::mspdi::read_mspdi(&crate::mspdi::write_mspdi(&proj)).unwrap()
    }

    fn assert_reopens(ed: &Editor) {
        let xml = crate::mspdi::write_mspdi(ed.project());
        assert_eq!(
            crate::mspdi::read_mspdi(&xml).unwrap().tasks,
            ed.project().tasks
        );
        assert_eq!(
            crate::yppx::read_yppx(&crate::yppx::write_yppx(ed.project()))
                .unwrap()
                .tasks,
            ed.project().tasks
        );
    }

    #[test]
    fn structural_calendar_rejections_preserve_all_editor_state() {
        for empty_default in [false, true] {
            let mut ed = Editor::new(project_with_unused_empty_calendar(empty_default));
            ed.toggle_level();
            ed.select(1);
            ed.find("Build");
            for i in 0..=UNDO_CAP {
                ed.rename(2, &format!("Build {i}")).unwrap();
            }
            // Cover rejection with a full undo stack, then with a redo branch.
            for has_redo in [false, true] {
                if has_redo {
                    assert!(ed.undo());
                }
                ed.mark_saved();
                assert_unchanged(&mut ed, |ed| {
                    assert!(ed.delete_task(2).unwrap_err().contains("Closed"));
                    assert!(ed.indent(2, -1).unwrap_err().contains("Closed"));
                    assert!(ed.indent(1, 1).unwrap_err().contains("Closed"));
                    assert!(
                        ed.update_task(
                            2,
                            TaskPatch {
                                name: Some("Must not rename".into()),
                                level: Some(1),
                                ..TaskPatch::default()
                            }
                        )
                        .unwrap_err()
                        .contains("Closed")
                    );
                    if empty_default {
                        // Inserting keeps the parent a summary, so only a
                        // closed default calendar rejects the new task.
                        assert!(
                            ed.add_task(Some(1), "Inserted", 480, false)
                                .unwrap_err()
                                .contains("Closed")
                        );
                        assert!(
                            ed.add_task(None, "Appended", 480, false)
                                .unwrap_err()
                                .contains("Closed")
                        );
                    }
                });
                assert_reopens(&ed);
            }
        }
    }

    #[test]
    fn accepted_edits_with_unused_empty_calendars_reopen() {
        for empty_default in [false, true] {
            let mut ed = Editor::new(project_with_unused_empty_calendar(empty_default));
            assert_reopens(&ed);
            if !empty_default {
                // A first child keeps the empty-calendar summary a summary.
                let mut ed = Editor::new(project_with_unused_empty_calendar(false));
                let at = ed.add_task(Some(1), "Inserted", 480, false).unwrap();
                let tasks = &ed.project().tasks;
                assert_eq!(tasks[at].outline_level, tasks[0].outline_level + 1);
                assert!(tasks[0].summary);
                assert_reopens(&ed);
            }
            ed.rename(2, "Renamed").unwrap();
            assert_reopens(&ed);
            ed.set_duration_min(2, 960, false).unwrap();
            assert_reopens(&ed);
            ed.indent(2, 1).unwrap();
            assert_reopens(&ed);
            ed.set_baseline();
            assert_reopens(&ed);
            assert!(ed.undo());
            assert_reopens(&ed);
            assert!(ed.redo());
            assert_reopens(&ed);
            // Removing the empty-calendar summary removes its subtree with it,
            // and the empty plan still reopens.
            assert_eq!(ed.delete_task(1).unwrap(), vec![1, 2]);
            assert!(ed.project().tasks.is_empty());
            assert_reopens(&ed);
            if !empty_default {
                ed.add_task(None, "New task", 480, false).unwrap();
                assert_reopens(&ed);
            }
        }
    }

    fn assert_edit(ed: &mut Editor, action: impl FnOnce(&mut Editor)) {
        let before = ed.project().clone();
        let depth = ed.undo_depth();
        action(ed);
        let after = ed.project().clone();
        assert_ne!(before, after);
        assert!(ed.dirty());
        assert_eq!(ed.undo_depth(), depth + 1);
        assert_eq!(ed.redo_depth(), 0);
        assert_schedule(ed);
        assert!(ed.undo());
        assert_eq!(ed.project(), &before);
        assert_schedule(ed);
        assert!(ed.redo());
        assert_eq!(ed.project(), &after);
        assert_schedule(ed);
    }

    #[test]
    fn every_operation_is_one_undoable_rescheduled_edit() {
        let mut ed = editor();
        ed.toggle_level();
        assert_edit(&mut ed, |e| {
            e.add_task(Some(1), "Inserted", 960, false).unwrap();
        });
        assert_eq!(ed.project().tasks[1].name, "Inserted");
        assert_edit(&mut ed, |e| {
            e.rename(1, "Design").unwrap();
        });
        assert_edit(&mut ed, |e| {
            e.set_duration(1, "3d").unwrap();
        });
        assert_eq!(ed.project().tasks[0].duration_min, 1440);
        assert_edit(&mut ed, |e| {
            e.indent(3, 1).unwrap();
        });
        assert!(ed.project().tasks[0].summary);
        assert_edit(&mut ed, |e| {
            e.add_predecessor(2, 3, LinkType::FinishStart, 60).unwrap();
        });
        assert!(ed.disp_start(2).unwrap() > ed.disp_finish(3).unwrap());
        assert_edit(&mut ed, |e| {
            e.set_constraint(3, "SNET 2026-01-08").unwrap();
        });
        assert_eq!(ed.schedule().get(3).unwrap().early_start.parts().day, 8);
        assert_edit(&mut ed, |e| {
            assert_eq!(
                e.assign_resource(3, "Alice").unwrap(),
                AssignOutcome::Assigned
            );
        });
        assert_edit(&mut ed, |e| e.set_baseline());
        let baseline = ed.project().tasks[1].baseline(0).unwrap().finish.unwrap();
        assert_edit(&mut ed, |e| {
            e.set_duration_min(3, 4800, false).unwrap();
        });
        assert!(ed.schedule().get(3).unwrap().early_finish > baseline);
        assert_edit(&mut ed, |e| {
            e.toggle_milestone(3).unwrap();
        });
        assert!(ed.project().tasks[1].milestone);
        assert_eq!(ed.project().tasks[1].duration_min, 0);
        assert_edit(&mut ed, |e| {
            e.toggle_milestone(3).unwrap();
        });
        assert!(!ed.project().tasks[1].milestone);
        assert_eq!(ed.project().tasks[1].duration_min, 480);
        assert_edit(&mut ed, |e| {
            e.remove_predecessor(2, 3).unwrap();
        });
        assert!(ed.project().tasks[2].predecessors.is_empty());
        assert_edit(&mut ed, |e| {
            e.update_task(
                2,
                TaskPatch {
                    name: Some("Release".into()),
                    duration_min: Some(0),
                    level: Some(2),
                    manual: None,
                    estimated: None,
                },
            )
            .unwrap();
        });
        let task = &ed.project().tasks[2];
        assert_eq!(
            (
                &*task.name,
                task.duration_min,
                task.outline_level,
                task.milestone
            ),
            ("Release", 0, 2, true)
        );
        assert_edit(&mut ed, |e| {
            e.delete_task(3).unwrap();
        });
        assert_eq!(ed.project().tasks.len(), 2);
    }

    #[test]
    fn rejected_edits_preserve_everything_clean_and_dirty() {
        type Edit = fn(&mut Editor) -> Result<(), String>;
        let bad: &[Edit] = &[
            |e| e.add_task(Some(999), "bad", 480, false).map(|_| ()),
            |e| e.delete_task(999).map(|_| ()),
            |e| e.indent(999, 1),
            |e| e.rename(999, "bad"),
            |e| e.set_duration(999, "1d"),
            |e| e.set_duration(1, "banana"),
            |e| e.set_duration_min(999, 480, false),
            |e| e.add_predecessor(999, 1, LinkType::FinishStart, 0),
            |e| e.add_predecessor(1, 999, LinkType::FinishStart, 0),
            |e| e.add_predecessor(1, 1, LinkType::FinishStart, 0),
            |e| e.add_predecessor(2, 1, LinkType::FinishStart, 0),
            |e| e.remove_predecessor(999, 1),
            |e| e.remove_predecessor(1, 2),
            |e| e.set_constraint(999, "none"),
            |e| e.set_constraint(1, "MSO"),
            |e| e.set_constraint(1, "SNET banana"),
            |e| e.set_constraint(1, "what"),
            |e| e.assign_resource(999, "Alice").map(|_| ()),
            |e| e.toggle_milestone(999),
            |e| {
                e.update_task(
                    999,
                    TaskPatch {
                        name: Some("bad".into()),
                        ..TaskPatch::default()
                    },
                )
            },
            |e| e.update_task(1, TaskPatch::default()),
            |e| {
                e.update_task(
                    1,
                    TaskPatch {
                        name: Some("bad".into()),
                        level: Some(0),
                        ..TaskPatch::default()
                    },
                )
            },
            |e| {
                e.update_task(
                    1,
                    TaskPatch {
                        duration_min: Some(0),
                        level: Some(21),
                        ..TaskPatch::default()
                    },
                )
            },
        ];
        for clean in [false, true] {
            let mut ed = editor();
            ed.add_predecessor(2, 1, LinkType::FinishStart, 0).unwrap();
            ed.assign_resource(1, "Alice").unwrap();
            ed.assign_resource(2, "Alice").unwrap();
            ed.toggle_level();
            ed.find("task 2");
            ed.rename(1, "Changed").unwrap();
            ed.undo();
            if clean {
                ed.mark_saved();
            }
            assert!(ed.redo_depth() > 0);
            for action in bad {
                assert_unchanged(&mut ed, |e| assert!(action(e).is_err()));
            }
        }
    }

    #[test]
    fn append_and_insert_follow_the_row_above_without_selecting() {
        let mut ed = editor();
        ed.indent(1, 1).unwrap();
        ed.indent(2, 2).unwrap();
        ed.select(1);
        let at = ed.add_task(None, "Append", 480, false).unwrap();
        assert_eq!(at, 2);
        assert_eq!(ed.project().tasks[at].outline_level, 3);
        // Task 1 is a summary over Task 2, so the insert is its first child.
        let at = ed.add_task(Some(1), "Insert", 480, false).unwrap();
        assert_eq!(at, 1);
        assert_eq!(ed.project().tasks[at].outline_level, 3);
        assert_eq!(ed.sel(), 1);
        ed.replace_project(Project::default());
        assert_eq!(ed.add_task(None, "First", 0, false).unwrap(), 0);
        assert_eq!(ed.project().tasks[0].outline_level, 1);
        assert!(ed.project().tasks[0].milestone);
    }

    // ---- Insert Task › Blank Row (#158) ----

    #[test]
    fn insert_blank_row_goes_above_as_one_undo_step_and_schedules_nothing() {
        let mut ed = editor();
        ed.toggle_level();
        ed.select(1);
        let sched = results(ed.schedule());
        let finish = ed.level.as_ref().unwrap().project_finish;
        let mut at = None;
        assert_edit(&mut ed, |e| at = Some(e.insert_blank_row(Some(2)).unwrap()));
        assert_eq!(at, Some(1), "above the row it is inserted before");
        assert_eq!(
            ed.project().tasks[1],
            Task {
                uid: 3,
                id: 3,
                is_null: true,
                ..Task::default()
            }
        );
        // The editor leaves the selection index alone, so it lands on the new
        // row; the host selects it explicitly.
        assert_eq!((ed.sel(), ed.selected_uid()), (1, Some(3)));
        // Every task keeps its schedule: the scheduler skips blank rows.
        assert_eq!(results(ed.schedule()), sched);
        assert!(ed.schedule().get(3).is_none());
        assert_eq!(ed.level.as_ref().unwrap().project_finish, finish);
        // `None` appends; the first row of an empty plan is blank too.
        assert_eq!(ed.insert_blank_row(None), Ok(3));
        assert!(ed.project().tasks[3].is_null);
        assert_eq!(ed.project().tasks[3].uid, 4);
        assert_reopens(&ed);
        ed.replace_project(Project::default());
        assert_eq!(ed.insert_blank_row(None), Ok(0));
        assert_eq!(ed.project().tasks[0].uid, 1);
        // An unknown UID changes nothing.
        let mut ed = editor();
        ed.rename(1, "Dirty").unwrap();
        assert_unchanged(&mut ed, |e| {
            assert_eq!(
                e.insert_blank_row(Some(99)),
                Err("no task with uid 99".into())
            );
        });
    }

    #[test]
    fn a_blank_row_inserted_in_a_subtree_stays_outside_the_outline() {
        let mut ed = phase_plan();
        // Between Phase's subtasks, and after its last one (above B).
        ed.insert_blank_row(Some(4)).unwrap();
        ed.insert_blank_row(Some(5)).unwrap();
        let blank = |ed: &Editor, i: usize| ed.project().tasks[i].is_null;
        assert!(blank(&ed, 3) && blank(&ed, 5));
        let tasks: Vec<_> = rows(&ed)
            .into_iter()
            .enumerate()
            .filter(|&(i, _)| !blank(&ed, i))
            .map(|(_, row)| row)
            .collect();
        assert_eq!(
            tasks,
            rows(&phase_plan()),
            "no level or summary flag changes"
        );
        assert_reopens(&ed);
        // Phase still has two subtasks; deleting it takes the blank row
        // between them, not the one after its last subtask.
        assert_eq!(ed.subtree_len(2), Ok(2));
        assert_eq!(ed.delete_task(2).unwrap(), vec![2, 3, 6, 4]);
        assert_eq!(
            ed.project().tasks.iter().map(|t| t.uid).collect::<Vec<_>>(),
            [1, 7, 5]
        );
    }

    #[test]
    fn a_blank_row_next_to_a_collapsed_summary_is_shown_and_keeps_it_collapsed() {
        // Collapse Phase, put the selection on `sel`, insert as the hosts do
        // (then select the row), and type into it.
        let insert = |mut ed: Editor, sel: usize, before: bool| {
            ed.set_collapsed(2, true).unwrap();
            ed.select(sel);
            let at = ed
                .insert_blank_row(before.then(|| ed.selected_uid().unwrap()))
                .unwrap();
            ed.select(at);
            let shown = (at, ed.visible_rows(), ed.is_collapsed(2));
            let uid = ed.project().tasks[at].uid;
            ed.rename(uid, "Typed").unwrap();
            assert!(ed.is_collapsed(2), "typing keeps Phase collapsed");
            (shown, ed.project().tasks[at].outline_level)
        };
        // On B, after Phase's hidden subtasks: the row is outside Phase's
        // subtree, so it shows, and typed it is Phase's sibling.
        assert_eq!(
            insert(phase_plan(), 4, true),
            ((4, vec![0, 1, 4, 5], true), 1)
        );
        // On the collapsed Phase itself: above it.
        assert_eq!(
            insert(phase_plan(), 1, true),
            ((1, vec![0, 1, 2, 5], true), 1)
        );
        // Appended (the suite's entry row) after a collapsed last summary.
        let last = outline(&[(1, "A", 1), (2, "Phase", 1), (3, "P1", 2)]);
        assert_eq!(insert(last, 1, false), ((3, vec![0, 1, 3], true), 1));
    }

    #[test]
    fn typing_into_an_inserted_blank_row_makes_a_task_where_it_sits() {
        // Above Phase's first subtask: the typed row is Phase's first child.
        let mut ed = phase_plan();
        let at = ed.insert_blank_row(Some(3)).unwrap();
        assert_eq!(at, 2);
        let depth = ed.undo_depth();
        ed.rename(6, "Typed").unwrap();
        let t = &ed.project().tasks[at];
        assert_eq!(
            (
                &*t.name,
                t.outline_level,
                t.duration_min,
                t.estimated,
                t.is_null
            ),
            ("Typed", 2, 480, Some(true), false)
        );
        assert!(ed.project().tasks[1].summary);
        assert!(ed.schedule().get(6).is_some());
        assert_eq!(ed.undo_depth(), depth + 1);
        assert!(ed.undo());
        assert!(ed.project().tasks[at].is_null, "undo keeps the blank row");
        // In a manual-default plan the typed row is a pinned manual task.
        let mut proj = phase_plan().project().clone();
        proj.new_tasks_are_manual = true;
        let start = proj.start_date;
        let mut ed = Editor::new(proj);
        ed.insert_blank_row(Some(1)).unwrap();
        ed.rename(6, "Typed").unwrap();
        let t = &ed.project().tasks[0];
        assert!(t.manual && !t.is_null);
        assert_eq!((t.outline_level, t.manual_start), (1, start));
    }

    #[test]
    fn deletion_drops_links_and_clamps_selection() {
        let mut ed = editor();
        ed.add_predecessor(1, 2, LinkType::FinishStart, 0).unwrap();
        ed.select(1);
        assert_edit(&mut ed, |e| {
            e.delete_task(2).unwrap();
        });
        assert_eq!(ed.sel(), 0);
        assert!(ed.project().tasks[0].predecessors.is_empty());
        ed.delete_task(1).unwrap();
        assert_eq!(ed.sel(), 0);
        assert_eq!(ed.selected_uid(), None);
        ed.select(usize::MAX);
        assert_eq!(ed.sel(), 0);
    }

    /// Tasks `(uid, name, level)` scheduled from a fixed start.
    fn outline(rows: &[(i32, &str, u32)]) -> Editor {
        Editor::new(Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
            tasks: rows
                .iter()
                .map(|&(uid, name, outline_level)| Task {
                    uid,
                    id: uid,
                    name: name.into(),
                    outline_level,
                    duration_min: 480,
                    ..Task::default()
                })
                .collect(),
            ..Project::default()
        })
    }

    #[test]
    fn inactivate_summary_skips_blank_rows_and_undo_restores_exact_flags() {
        let base = outline(&[(1, "Summary", 1), (2, "A", 2), (3, "B", 2)]);
        let mut proj = base.project().clone();
        let mut blank = Task {
            uid: 4,
            id: 4,
            is_null: true,
            ..Task::default()
        };
        blank.outline_level = 0;
        proj.tasks.insert(2, blank);
        let mut ed = Editor::new(proj.clone());
        assert_eq!(ed.set_active(1, false).unwrap(), 3);
        assert_eq!(ed.undo_depth(), 1);
        assert_eq!(ed.project().tasks[2].active, None);
        assert!(ed.dirty());
        assert_eq!(ed.set_active(1, false).unwrap(), 0);
        assert_eq!(ed.undo_depth(), 1);
        assert!(ed.undo());
        assert_eq!(ed.project(), &proj);
        assert!(ed.redo());
        assert_eq!(ed.project().tasks[0].active, Some(false));
        assert_eq!(ed.set_active(1, true).unwrap(), 3);
        assert_eq!(ed.project().tasks[2].active, None);
    }

    #[test]
    fn activating_an_unset_flag_is_a_clean_noop() {
        let mut ed = outline(&[(1, "Task", 1)]);
        assert_eq!(ed.project().tasks[0].active, None);
        assert_eq!(ed.set_active(1, true).unwrap(), 0);
        assert_eq!(ed.project().tasks[0].active, None);
        assert_eq!(ed.undo_depth(), 0);
        assert!(!ed.dirty());
    }

    #[test]
    fn inactivate_refuses_blank_and_activation_beneath_inactive_summary() {
        let mut proj = outline(&[(1, "Summary", 1), (2, "Child", 2)])
            .project()
            .clone();
        proj.tasks[0].active = Some(false);
        let mut ed = Editor::new(proj.clone());
        assert!(ed.set_active(2, true).is_err());
        assert_eq!(ed.undo_depth(), 0);
        assert!(!ed.dirty());
        let blank = Task {
            uid: 3,
            id: 3,
            is_null: true,
            ..Task::default()
        };
        proj.tasks.push(blank);
        let mut ed = Editor::new(proj);
        assert!(ed.set_active(3, false).is_err());
        assert_eq!(ed.undo_depth(), 0);
    }

    #[test]
    fn edited_active_flag_round_trips_through_mspdi_and_yppx() {
        let mut ed = outline(&[(1, "Task", 1)]);
        assert_eq!(ed.set_active(1, false).unwrap(), 1);
        let xml = crate::mspdi::write_mspdi(ed.project());
        assert!(xml.contains("<Active>0</Active>"));
        assert_eq!(
            crate::mspdi::read_mspdi(&xml)
                .unwrap()
                .task(1)
                .unwrap()
                .active,
            Some(false)
        );
        let package = crate::yppx::write_yppx(ed.project());
        assert_eq!(
            crate::yppx::read_yppx(&package)
                .unwrap()
                .task(1)
                .unwrap()
                .active,
            Some(false)
        );
        ed.set_active(1, true).unwrap();
        assert!(crate::mspdi::write_mspdi(ed.project()).contains("<Active>1</Active>"));
    }

    fn names(ed: &Editor) -> Vec<(&str, u32)> {
        ed.project()
            .tasks
            .iter()
            .map(|t| (&*t.name, t.outline_level))
            .collect()
    }

    /// `(name, outline_level, summary)` for every row.
    fn rows(ed: &Editor) -> Vec<(&str, u32, bool)> {
        ed.project()
            .tasks
            .iter()
            .map(|t| (&*t.name, t.outline_level, t.summary))
            .collect()
    }

    fn uid_of(ed: &Editor, name: &str) -> i32 {
        ed.project()
            .tasks
            .iter()
            .find(|t| t.name == name)
            .unwrap()
            .uid
    }

    #[test]
    fn a_task_inserted_under_a_summary_is_its_first_child() {
        // The issue's repro, on an untitled project.
        let mut ed = Editor::new(Project::default());
        ed.add_task(None, "S", 480, false).unwrap();
        ed.add_task(None, "c1", 480, false).unwrap();
        ed.indent(uid_of(&ed, "c1"), 1).unwrap();
        let before = ed.project().clone();
        let s = uid_of(&ed, "S");
        assert_eq!(ed.add_task(Some(s), "New", 480, false).unwrap(), 1);
        assert_eq!(
            rows(&ed),
            [("S", 1, true), ("New", 2, false), ("c1", 2, false)]
        );
        // One undo step puts the outline back.
        assert!(ed.undo());
        assert_eq!(ed.project(), &before);
    }

    #[test]
    fn a_task_inserted_after_a_leaf_is_its_sibling() {
        // S{c1, c2}, Y: inserting above Y (below c2) stays inside S.
        let mut ed = outline(&[(1, "S", 1), (2, "c1", 2), (3, "c2", 2), (4, "Y", 1)]);
        assert_eq!(ed.add_task(Some(3), "New", 480, false).unwrap(), 3);
        assert_eq!(
            rows(&ed),
            [
                ("S", 1, true),
                ("c1", 2, false),
                ("c2", 2, false),
                ("New", 2, false),
                ("Y", 1, false),
            ]
        );
        // After a plain task, the new one is a sibling at its level.
        let mut ed = outline(&[(1, "S", 1), (2, "Y", 1)]);
        ed.add_task(Some(1), "New", 480, false).unwrap();
        assert_eq!(names(&ed), [("S", 1), ("New", 1), ("Y", 1)]);
        // Appending copies the last row, which is never a summary.
        let mut ed = outline(&[(1, "S", 1), (2, "c1", 2)]);
        ed.add_task(None, "New", 480, false).unwrap();
        assert_eq!(
            rows(&ed),
            [("S", 1, true), ("c1", 2, false), ("New", 2, false)]
        );
    }

    #[test]
    fn a_task_inserted_under_a_nested_summary_goes_one_level_deeper() {
        let mut ed = outline(&[(1, "A", 1), (2, "S", 2), (3, "c", 3), (4, "B", 1)]);
        ed.add_task(Some(2), "New", 480, false).unwrap();
        assert_eq!(
            rows(&ed),
            [
                ("A", 1, true),
                ("S", 2, true),
                ("New", 3, false),
                ("c", 3, false),
                ("B", 1, false),
            ]
        );
        // An outline that skips a level: the new task is S's child, and the
        // deeper row it pushes down stays inside S.
        let mut ed = outline(&[(1, "S", 1), (2, "c", 3), (3, "Y", 1)]);
        ed.add_task(Some(1), "New", 480, false).unwrap();
        assert_eq!(
            rows(&ed),
            [
                ("S", 1, true),
                ("New", 2, true),
                ("c", 3, false),
                ("Y", 1, false),
            ]
        );
    }

    fn phase_plan() -> Editor {
        outline(&[
            (1, "A", 1),
            (2, "Phase", 1),
            (3, "P1", 2),
            (4, "P2", 2),
            (5, "B", 1),
        ])
    }

    #[test]
    fn deleting_a_summary_deletes_its_subtree_as_one_undo_step() {
        let mut ed = phase_plan();
        assert_eq!(ed.subtree_len(2), Ok(2));
        assert_eq!(ed.subtree_len(3), Ok(0));
        ed.select(1);
        let before = ed.project().clone();
        let depth = ed.undo_depth();

        assert_eq!(ed.delete_task(2).unwrap(), vec![2, 3, 4]);
        assert_eq!(names(&ed), [("A", 1), ("B", 1)]);
        assert!(!ed.project().tasks[0].summary, "A must not adopt P1/P2");
        assert_eq!(
            ed.selected_uid(),
            Some(5),
            "selection moves to the row after"
        );
        assert_eq!(ed.undo_depth(), depth + 1);
        assert_schedule(&ed);

        let after = ed.project().clone();
        assert!(ed.undo());
        assert_eq!(ed.project(), &before);
        assert_schedule(&ed);
        assert!(ed.redo());
        assert_eq!(ed.project(), &after);
    }

    #[test]
    fn deleting_a_nested_summary_stops_at_its_own_level() {
        let mut ed = outline(&[
            (1, "Phase", 1),
            (2, "Sub", 2),
            (3, "S1", 3),
            (4, "S2", 3),
            (5, "P2", 2),
            (6, "B", 1),
        ]);
        assert_eq!(ed.subtree_len(1), Ok(4));
        assert_eq!(ed.subtree_len(2), Ok(2));
        assert_eq!(ed.delete_task(2).unwrap(), vec![2, 3, 4]);
        assert_eq!(names(&ed), [("Phase", 1), ("P2", 2), ("B", 1)]);
        assert!(ed.project().tasks[0].summary);
        // A trailing subtree runs to the end of the plan.
        assert_eq!(ed.delete_task(1).unwrap(), vec![1, 5]);
        assert_eq!(names(&ed), [("B", 1)]);
        assert!(ed.subtree_len(999).is_err());
    }

    #[test]
    fn deleting_a_summary_drops_links_and_assignments_of_its_subtasks() {
        let mut ed = phase_plan();
        ed.add_predecessor(5, 3, LinkType::FinishStart, 0).unwrap();
        ed.add_predecessor(5, 1, LinkType::FinishStart, 0).unwrap();
        ed.assign_resource(4, "Alice").unwrap();
        ed.assign_resource(1, "Alice").unwrap();

        ed.delete_task(2).unwrap();
        let b = ed.project().task(5).unwrap();
        assert_eq!(
            b.predecessors.iter().map(|p| p.uid).collect::<Vec<_>>(),
            [1]
        );
        assert!(ed.project().assignments.iter().all(|a| a.task_uid == 1));
        assert_eq!(ed.project().assignments.len(), 1);
        assert_schedule(&ed);
    }

    #[test]
    fn a_reused_subtask_uid_does_not_inherit_its_assignment() {
        // The trailing summary's child holds the highest UID and a resource.
        let mut ed = outline(&[(1, "A", 1), (3, "Phase", 1), (2, "P1", 2)]);
        ed.assign_resource(2, "Alice").unwrap();
        assert_eq!(ed.delete_task(3).unwrap(), vec![3, 2]);
        assert!(ed.project().assignments.is_empty());

        let at = ed.add_task(None, "Replacement", 480, false).unwrap();
        let uid = ed.project().tasks[at].uid;
        assert_eq!(uid, 2, "the removed subtask's UID is reused");
        assert!(ed.project().assignments.iter().all(|a| a.task_uid != uid));
        assert_schedule(&ed);
    }

    #[test]
    fn indent_clamps_and_recomputes_summaries() {
        let mut ed = editor();
        ed.indent(2, i32::MAX).unwrap();
        assert_eq!(ed.project().tasks[1].outline_level, 20);
        assert!(ed.project().tasks[0].summary);
        ed.indent(2, i32::MIN).unwrap();
        assert_eq!(ed.project().tasks[1].outline_level, 1);
        assert!(!ed.project().tasks[0].summary);
    }

    #[test]
    fn deleting_a_task_removes_assignments_before_its_uid_is_reused() {
        let mut ed = editor();
        ed.assign_resource(1, "Alice").unwrap();
        ed.assign_resource(2, "Alice").unwrap();
        ed.toggle_level();
        let before = ed.project().clone();
        assert!(ed.disp_start(2).unwrap() > ed.disp_start(1).unwrap());

        ed.delete_task(2).unwrap();
        assert!(ed.project().assignments.iter().all(|a| a.task_uid != 2));
        assert_eq!(ed.project().assignments.len(), 1);
        // The resource stays; its totals lose the deleted assignment (#269).
        let alice = &ed.project().resources[0];
        assert_eq!(
            (alice.uid, alice.work_min),
            (before.resources[0].uid, Some(480))
        );
        assert_eq!(before.resources[0].work_min, Some(960));
        assert_schedule(&ed);

        let at = ed.add_task(None, "Replacement", 480, false).unwrap();
        let uid = ed.project().tasks[at].uid;
        assert_eq!(uid, 2, "the highest deleted UID is reused");
        assert!(ed.project().assignments.iter().all(|a| a.task_uid != uid));
        assert_eq!(ed.disp_start(uid), ed.disp_start(1));
        assert_schedule(&ed);

        assert!(ed.undo()); // remove the replacement task
        assert!(ed.undo()); // restore the deleted task and its assignment
        assert_eq!(ed.project(), &before);
        assert!(ed.disp_start(2).unwrap() > ed.disp_start(1).unwrap());
        assert_schedule(&ed);
        assert!(ed.redo());
        assert!(ed.project().assignments.iter().all(|a| a.task_uid != 2));
        assert_schedule(&ed);
    }

    #[test]
    fn assignment_and_clear_refresh_an_enabled_leveling_overlay() {
        let mut ed = editor();
        ed.toggle_level();
        ed.assign_resource(1, " Alice ").unwrap();
        assert_eq!(ed.project().resources[0].name, "Alice");
        let before = ed.disp_start(2).unwrap();
        assert_edit(&mut ed, |e| {
            e.assign_resource(2, "alice").unwrap();
        });
        assert_eq!(ed.project().resources.len(), 1);
        assert!(ed.disp_start(2).unwrap() > before);
        assert_edit(&mut ed, |e| {
            assert_eq!(e.assign_resource(2, "").unwrap(), AssignOutcome::Cleared);
        });
        assert_eq!(ed.disp_start(2).unwrap(), before);
        ed.toggle_level();
        assert_eq!(
            ed.disp_start(2),
            Some(ed.schedule().get(2).unwrap().early_start)
        );
    }

    #[test]
    fn displayed_project_span_reaches_a_manual_task_outside_it() {
        let ed = editor();
        let (start, finish) = (ed.disp_project_start(), ed.disp_project_finish());
        assert_eq!(
            start,
            ed.schedule().project_start,
            "an ordinary plan is unchanged"
        );
        assert_eq!(finish, ed.schedule().project_finish);

        let mut proj = ed.project().clone();
        proj.tasks[0].manual = true;
        proj.tasks[0].manual_start = Some(DateTime::from_ymd_hm(2025, 12, 29, 8, 0));
        proj.tasks[1].manual = true;
        proj.tasks[1].manual_start = Some(DateTime::from_ymd_hm(2026, 2, 2, 8, 0));
        let ed = Editor::new(proj);
        assert_eq!(ed.disp_project_start(), ed.disp_start(1).unwrap());
        assert!(ed.disp_project_start() < ed.schedule().project_start);
        assert_eq!(ed.disp_project_finish(), ed.disp_finish(2).unwrap());
        assert!(ed.disp_project_finish() >= ed.schedule().project_finish);
    }

    #[test]
    fn displayed_project_finish_follows_leveling() {
        let mut ed = editor();
        ed.assign_resource(1, "Alice").unwrap();
        ed.assign_resource(2, "Alice").unwrap();
        let cpm = ed.schedule().project_finish;
        assert_eq!(ed.disp_project_finish(), cpm);
        ed.toggle_level();
        assert!(
            ed.disp_project_finish() > cpm,
            "leveling serialises Alice's tasks"
        );
        assert_eq!(ed.disp_project_finish(), level(ed.project()).project_finish);
        assert_eq!(
            ed.schedule().project_finish,
            cpm,
            "the CPM finish is unchanged"
        );
        ed.toggle_level();
        assert_eq!(ed.disp_project_finish(), cpm);
    }

    #[test]
    fn displayed_summary_duration_follows_leveled_dates() {
        let mut proj = editor().project().clone();
        for t in &mut proj.tasks {
            t.outline_level = 2;
        }
        proj.tasks.insert(
            0,
            Task {
                uid: 3,
                id: 3,
                name: "Phase".into(),
                outline_level: 1,
                summary: true,
                duration_min: 0,
                ..Task::default()
            },
        );
        let mut ed = Editor::new(proj);
        ed.assign_resource(1, "Alice").unwrap();
        ed.assign_resource(2, "Alice").unwrap();
        // Unleveled, the two 1d children run in parallel: the summary spans 1d.
        assert_eq!(ed.disp_duration_min(3), Some(480));
        assert_eq!(ed.disp_duration_min(1), Some(480));

        ed.toggle_level();
        let cpm =
            crate::schedule::task_duration_min(ed.project(), ed.schedule(), &ed.project().tasks[0]);
        assert_eq!(cpm, Some(480));
        // Leveling serialises them on Alice: the displayed span is 2d, matching
        // the displayed start/finish rather than the CPM dates.
        let shown = ed.disp_duration_min(3);
        assert_eq!(
            shown,
            Some(crate::schedule::working_minutes_between(
                ed.project(),
                ed.disp_start(3).unwrap(),
                ed.disp_finish(3).unwrap(),
            ))
        );
        assert_eq!(shown, Some(960));
        assert_ne!(shown, cpm);
        assert_eq!(ed.disp_duration_min(1), Some(480));
        assert_eq!(ed.disp_duration_min(99), None);
    }

    #[test]
    fn assignment_noops_preserve_history_and_dirty_flag() {
        let mut ed = editor();
        ed.assign_resource(1, "Alice").unwrap();
        ed.rename(2, "Redo this").unwrap();
        ed.undo();
        for clean in [false, true] {
            if clean {
                ed.mark_saved();
            }
            assert_unchanged(&mut ed, |e| {
                assert_eq!(
                    e.assign_resource(1, "ALICE").unwrap(),
                    AssignOutcome::AlreadyAssigned
                )
            });
            assert_unchanged(&mut ed, |e| {
                assert_eq!(
                    e.assign_resource(2, " ").unwrap(),
                    AssignOutcome::NothingToClear
                )
            });
        }
    }

    #[test]
    fn structural_edits_keep_the_history_cap_and_clear_redo() {
        let mut ed = editor();
        ed.add_task(None, "Undone", 480, false).unwrap();
        assert!(ed.undo());
        assert_eq!(ed.redo_depth(), 1);
        let before = ed.project().clone();
        ed.add_task(None, "Structural", 480, false).unwrap();
        assert_eq!((ed.undo_depth(), ed.redo_depth()), (1, 0));
        assert!(ed.undo());
        assert_eq!(ed.project(), &before);
        let mut states = vec![before];
        for i in 0..UNDO_CAP + 5 {
            ed.add_task(None, &format!("T{i}"), 480, false).unwrap();
            states.push(ed.project().clone());
        }
        assert_eq!(ed.undo_depth(), UNDO_CAP);
        while ed.undo() {}
        assert_eq!(ed.project(), &states[5]);
    }

    #[test]
    fn summary_durations_and_baselines_use_leaf_calendars_on_an_empty_default() {
        let mut proj = untitled_project();
        proj.calendars = vec![
            crate::model::Calendar::base(1, "Closed", Default::default()),
            crate::model::Calendar::standard(3),
        ];
        proj.tasks = (1..=3)
            .map(|uid| Task {
                uid,
                id: uid,
                name: format!("Task {uid}"),
                outline_level: if uid == 1 { 1 } else { 2 },
                summary: uid == 1,
                calendar_uid: (uid != 1).then_some(3),
                duration_min: if uid == 1 { 0 } else { 480 },
                predecessors: if uid == 3 {
                    vec![crate::model::Predecessor::fs(2)]
                } else {
                    vec![]
                },
                ..Task::default()
            })
            .collect();
        let mut ed = Editor::new(proj);
        assert_eq!(ed.disp_duration_min(1), Some(960));
        ed.toggle_level();
        assert_eq!(ed.disp_duration_min(1), Some(960));
        ed.set_baseline();
        let baseline = ed.project().tasks[0].baseline(0).unwrap();
        assert_eq!(baseline.duration_min, Some(960));
    }

    #[test]
    fn history_cap_redo_branch_and_selection_clamping() {
        let mut ed = editor();
        assert!(!ed.undo());
        assert!(!ed.redo());
        let at = ed.add_task(None, "Third", 480, false).unwrap();
        ed.select(at);
        assert!(ed.undo());
        assert_eq!(ed.sel(), 1);
        ed.rename(1, "Fresh edit").unwrap();
        assert_eq!(ed.redo_depth(), 0);
        ed.replace_project(editor().proj);
        for i in 0..101 {
            ed.rename(1, &format!("Edit {i}")).unwrap();
        }
        assert_eq!(ed.undo_depth(), 100);
        for _ in 0..100 {
            assert!(ed.undo());
        }
        assert!(!ed.undo());
        assert_eq!(ed.project().tasks[0].name, "Edit 0");
        for _ in 0..100 {
            assert!(ed.redo());
        }
        assert!(!ed.redo());
        assert_eq!(ed.undo_depth(), 100);
        assert_eq!(ed.project().tasks[0].name, "Edit 100");
    }

    #[test]
    fn replace_project_resets_document_state_and_keeps_preferences() {
        let mut ed = editor();
        ed.toggle_level();
        ed.find("TASK 2");
        ed.rename(1, "Renamed").unwrap();
        ed.rename(2, "Renamed again").unwrap();
        ed.undo();
        ed.replace_project(Project::default());
        assert!(ed.leveled());
        assert_eq!(ed.find_query(), "task 2");
        assert_eq!((ed.undo_depth(), ed.redo_depth(), ed.sel()), (0, 0, 0));
        assert!(!ed.dirty());
        assert_schedule(&ed);
    }

    #[test]
    fn find_normalizes_repeats_wraps_and_handles_inactive_search() {
        let mut ed = editor();
        assert_eq!(ed.find(""), FindOutcome::Inactive);
        assert_eq!(ed.find(" TASK "), FindOutcome::Found(1));
        assert_eq!(ed.find_query(), "task");
        assert_eq!(ed.find(""), FindOutcome::Found(0));
        assert_eq!(ed.find("missing"), FindOutcome::NotFound);
        assert_eq!(ed.sel(), 0);
        assert!(!ed.dirty());
        assert_eq!(ed.undo_depth(), 0);
        ed.replace_project(Project::default());
        assert_eq!(ed.find("task"), FindOutcome::Inactive);
    }

    #[test]
    fn duration_units_and_constraint_prefills() {
        let proj = Project::default();
        for (text, min) in [
            ("2d", 960),
            ("3", 1440),
            ("4h", 240),
            ("1w", 2400),
            ("30m", 30),
            (" 2D ", 960),
            ("-4h", -240),
        ] {
            assert_eq!(parse_duration(text, &proj), Some(min));
        }
        assert_eq!(parse_duration("nope", &proj), None);
        for code in ["SNET", "SNLT", "FNET", "FNLT", "MSO", "MFO", "ALAP"] {
            let text = if code == "ALAP" {
                code.to_string()
            } else {
                format!("{code} 2026-03-05")
            };
            let (constraint, constraint_date) = parse_constraint(&text).unwrap();
            let task = Task {
                constraint,
                constraint_date,
                ..Task::default()
            };
            assert_eq!(constraint_hint(&task), text);
            assert_eq!(
                parse_constraint(&constraint_hint(&task)).unwrap(),
                (constraint, constraint_date)
            );
        }
        for text in ["none", "asap"] {
            assert_eq!(
                parse_constraint(text).unwrap(),
                (ConstraintType::AsSoonAsPossible, None)
            );
        }
        assert_eq!(constraint_hint(&Task::default()), "");
        assert!(parse_constraint("").is_err());
    }

    // ---- blank rows and estimates (#80) ----

    /// Task 1, a blank row (UID 3) with no level or duration, then Task 2.
    fn blank_row_editor() -> Editor {
        let mut proj = editor().project().clone();
        proj.tasks.insert(
            1,
            Task {
                uid: 3,
                id: 2,
                is_null: true,
                create_date: Some(DateTime::from_ymd_hm(2026, 1, 2, 9, 0)),
                ..Task::default()
            },
        );
        proj.tasks[2].id = 3;
        Editor::new(proj)
    }

    #[test]
    fn editing_a_blank_row_makes_it_a_one_day_estimated_task() {
        let mut ed = blank_row_editor();
        let before = ed.project().clone();
        assert!(ed.schedule().get(3).is_none());
        ed.rename(3, "Typed").unwrap();
        let t = &ed.project().tasks[1];
        assert_eq!(
            t,
            &Task {
                uid: 3,
                id: 2,
                name: "Typed".into(),
                outline_level: 1,
                duration_min: 480,
                estimated: Some(true),
                create_date: before.tasks[1].create_date,
                ..Task::default()
            }
        );
        assert!(ed.schedule().get(3).is_some());
        let xml = crate::mspdi::write_mspdi(ed.project());
        assert!(!xml.contains("<IsNull>1</IsNull>"), "{xml}");
        assert_eq!(
            &crate::mspdi::read_mspdi(&xml).unwrap().tasks,
            &ed.project().tasks
        );
        // One undo step restores the blank row.
        assert_eq!(ed.undo_depth(), 1);
        assert!(ed.undo());
        assert_eq!(ed.project(), &before);
        assert!(ed.schedule().get(3).is_none());
    }

    type Edit = dyn Fn(&mut Editor) -> Result<(), String>;

    #[test]
    fn every_edit_of_a_blank_row_makes_it_a_task() {
        let fs1 = Predecessor::fs(1);
        let day = DateTime::from_ymd_hm(2026, 1, 7, 0, 0);
        let edits: Vec<(&str, Box<Edit>)> = vec![
            ("indent", Box::new(|ed| ed.indent(3, 1))),
            (
                "set_duration",
                Box::new(|ed| ed.set_duration_min(3, 960, false)),
            ),
            ("toggle_milestone", Box::new(|ed| ed.toggle_milestone(3))),
            (
                "set_constraint",
                Box::new(|ed| ed.set_constraint(3, "SNET 2026-01-07")),
            ),
            ("set_start", Box::new(move |ed| ed.set_start(3, day))),
            ("set_finish", Box::new(move |ed| ed.set_finish(3, day))),
            (
                "add_predecessor",
                Box::new(|ed| ed.add_predecessor(3, 1, LinkType::FinishStart, 0)),
            ),
            (
                "set_predecessors",
                Box::new(move |ed| ed.set_predecessors(3, vec![fs1])),
            ),
            (
                "assign_resource",
                Box::new(|ed| ed.assign_resource(3, "Alice").map(|_| ())),
            ),
            (
                "set_resources",
                Box::new(|ed| ed.set_resources(3, &["Alice".into()])),
            ),
        ];
        for (name, edit) in edits {
            let mut ed = blank_row_editor();
            let before = ed.project().clone();
            edit(&mut ed).unwrap_or_else(|e| panic!("{name}: {e}"));
            let t = &ed.project().tasks[1];
            assert!(!t.is_null, "{name}");
            assert!(t.outline_level >= 1, "{name}");
            assert!(ed.schedule().get(3).is_some(), "{name}");
            assert_schedule(&ed);
            assert_eq!(ed.undo_depth(), 1, "{name}");
            assert!(ed.undo());
            assert_eq!(ed.project(), &before, "{name}");
        }
        // A typed duration commits the new task's estimate; a milestone has none.
        let mut ed = blank_row_editor();
        ed.set_duration_min(3, 960, false).unwrap();
        let t = &ed.project().tasks[1];
        assert_eq!(
            (t.duration_min, t.estimated, t.milestone),
            (960, Some(false), false)
        );
        // Typing exactly the default one day commits it too.
        let mut ed = blank_row_editor();
        ed.set_duration_min(3, 480, false).unwrap();
        let t = &ed.project().tasks[1];
        assert_eq!(
            (t.duration_min, t.estimated, t.is_null),
            (480, Some(false), false)
        );
        // So does a manual blank row's typed finish that lands on one day.
        let mut proj = blank_row_editor().project().clone();
        proj.tasks[1].manual = true;
        proj.tasks[1].manual_start = Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0));
        let mut ed = Editor::new(proj);
        ed.set_finish(3, DateTime::from_ymd_hm(2026, 1, 5, 0, 0))
            .unwrap();
        let t = &ed.project().tasks[1];
        assert_eq!(
            (t.duration_min, t.estimated, t.is_null),
            (480, Some(false), false)
        );
        let mut ed = blank_row_editor();
        ed.toggle_milestone(3).unwrap();
        let t = &ed.project().tasks[1];
        assert_eq!((t.duration_min, t.milestone), (0, true));
    }

    #[test]
    fn a_blank_row_is_not_a_predecessor() {
        let mut ed = blank_row_editor();
        let before = ed.project().clone();
        assert_eq!(
            ed.add_predecessor(2, 3, LinkType::FinishStart, 0)
                .unwrap_err(),
            "No other task with ID 2"
        );
        let blank = Predecessor::fs(3);
        assert_eq!(
            ed.set_predecessors(2, vec![blank]).unwrap_err(),
            "No task with ID 2"
        );
        assert_eq!(ed.project(), &before);
        assert_eq!(ed.undo_depth(), 0);
        assert!(!ed.dirty());
    }

    #[test]
    fn a_link_to_a_blank_row_the_task_already_has_survives_a_cell_edit() {
        let proj = crate::mspdi::read_mspdi(include_str!("../../corpus/mspdi/20-task-fields.xml"))
            .unwrap();
        let mut ed = Editor::new(proj);
        // Pour (UID 4) links from Excavate (2) and from the blank row (3).
        let links = ed.project().task(4).unwrap().predecessors.clone();
        assert_eq!(links.iter().map(|p| p.uid).collect::<Vec<_>>(), [2, 3]);
        let mut edited = links.clone();
        edited[0].lag = 480;
        ed.set_predecessors(4, edited.clone()).unwrap();
        assert_eq!(ed.project().task(4).unwrap().predecessors, edited);
        // The blank row still does not drive Pour: only the new lag does.
        assert_eq!(
            ed.schedule().get(4).unwrap().early_start,
            DateTime::from_ymd_hm(2026, 3, 5, 8, 0)
        );
        let back = crate::mspdi::read_mspdi(&crate::mspdi::write_mspdi(ed.project())).unwrap();
        assert_eq!(back.tasks, ed.project().tasks);
        // A new link to the blank row is still refused.
        let mut added = ed.project().task(2).unwrap().predecessors.clone();
        added.push(links[1]);
        assert_eq!(
            ed.set_predecessors(2, added).unwrap_err(),
            "No task with ID 3"
        );
    }

    #[test]
    fn a_blank_row_under_a_summary_becomes_its_first_subtask() {
        let task = |uid, outline_level, is_null| Task {
            uid,
            id: uid,
            name: if is_null {
                String::new()
            } else {
                format!("T{uid}")
            },
            outline_level,
            duration_min: if is_null { 0 } else { 480 },
            is_null,
            ..Task::default()
        };
        let mut phase = task(1, 1, false);
        phase.summary = true;
        let mut ed = Editor::new(Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
            tasks: vec![
                phase,
                task(3, 0, true),
                task(2, 2, false),
                task(4, 1, false),
            ],
            ..Project::default()
        });
        ed.rename(3, "Typed").unwrap();
        let levels: Vec<_> = ed.project().tasks.iter().map(|t| t.outline_level).collect();
        assert_eq!(levels, [1, 2, 2, 1]);
        let summaries: Vec<_> = ed.project().tasks.iter().map(|t| t.summary).collect();
        assert_eq!(summaries, [true, false, false, false]);
        // Phase rolls up the typed row: a 2-day subtask sets its finish.
        ed.set_duration_min(3, 960, false).unwrap();
        assert!(ed.project().tasks[0].summary);
        assert_eq!(
            ed.schedule().get(1).unwrap().early_finish,
            DateTime::from_ymd_hm(2026, 1, 6, 17, 0)
        );
        // Below a task that is not a summary, the row takes that task's level.
        let mut ed = blank_row_editor();
        ed.indent(1, 1).unwrap();
        ed.rename(3, "Typed").unwrap();
        assert_eq!(ed.project().tasks[1].outline_level, 2);
    }

    #[test]
    fn a_blank_row_in_a_manual_plan_becomes_a_pinned_manual_task() {
        let mut proj = blank_row_editor().project().clone();
        proj.new_tasks_are_manual = true;
        let start = proj.start_date.unwrap();
        let mut ed = Editor::new(proj);
        ed.rename(3, "Typed").unwrap();
        let t = ed.project().tasks[1].clone();
        assert!(t.manual && !t.is_null);
        assert_eq!(
            (t.manual_start, t.manual_duration_min, t.manual_finish),
            (Some(start), Some(480), None)
        );
        let r = *ed.schedule().get(3).unwrap();
        assert_eq!(r.early_start, start);
        // Its Start/Finish are stamped as add_task stamps a new manual task.
        assert_eq!(
            (t.stored_start, t.stored_finish),
            (Some(start), Some(r.early_finish))
        );
        // A typed start or finish is the manual task's own date, not an
        // SNET/FNET constraint, and the schedule puts it on the typed day
        // (Wednesday; the project starts on Monday the 5th).
        let manual_plan = || {
            let mut proj = blank_row_editor().project().clone();
            proj.new_tasks_are_manual = true;
            Editor::new(proj)
        };
        let wednesday = DateTime::from_ymd_hm(2026, 1, 7, 0, 0);
        let mut ed = manual_plan();
        ed.set_start(3, wednesday).unwrap();
        let t = ed.project().tasks[1].clone();
        let typed = DateTime::from_ymd_hm(2026, 1, 7, 8, 0);
        assert!(t.manual && !t.is_null);
        assert_eq!((t.manual_start, t.manual_finish), (Some(typed), None));
        assert_eq!(t.constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(ed.schedule().get(3).unwrap().early_start, typed);
        assert_eq!(t.stored_start, Some(typed));
        let mut ed = manual_plan();
        ed.set_finish(3, wednesday).unwrap();
        let t = ed.project().tasks[1].clone();
        let end = DateTime::from_ymd_hm(2026, 1, 7, 17, 0);
        assert!(t.manual && !t.is_null);
        assert_eq!((t.manual_start, t.manual_finish), (Some(start), Some(end)));
        assert_eq!((t.duration_min, t.manual_duration_min), (1440, Some(1440)));
        assert_eq!(t.constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(ed.schedule().get(3).unwrap().early_finish, end);
        // Typing the project start itself still makes the row a task.
        let mut ed = manual_plan();
        ed.set_start(3, DateTime::from_ymd_hm(2026, 1, 5, 0, 0))
            .unwrap();
        assert!(!ed.project().tasks[1].is_null);
        assert_eq!(ed.undo_depth(), 1);
        // A zero duration or milestone toggle replaces the default ManualDuration.
        for edit in [
            |ed: &mut Editor| ed.toggle_milestone(3),
            |ed: &mut Editor| ed.set_duration_min(3, 0, false),
        ] {
            let mut ed = manual_plan();
            edit(&mut ed).unwrap();
            let t = &ed.project().tasks[1];
            assert_eq!(
                (t.duration_min, t.manual_duration_min, t.milestone),
                (0, Some(0), true)
            );
            assert_eq!(t.stored_finish, Some(start));
        }
        // Every materializing edit pins it, not only the date edits. An
        // explicit constraint is recorded, but a manual task stays at its
        // pinned start (the project start), as in Project.
        for edit in [
            |ed: &mut Editor| ed.set_constraint(3, "SNET 2026-01-07"),
            |ed: &mut Editor| ed.add_predecessor(3, 1, LinkType::FinishStart, 0),
            |ed: &mut Editor| ed.indent(3, 1),
        ] {
            let mut proj = blank_row_editor().project().clone();
            proj.new_tasks_are_manual = true;
            let mut ed = Editor::new(proj);
            edit(&mut ed).unwrap();
            let t = &ed.project().tasks[1];
            assert!(t.manual);
            assert_eq!(t.stored_start, Some(start));
        }
        // A plan whose new tasks are automatic keeps the row automatic.
        let mut ed = blank_row_editor();
        ed.rename(3, "Typed").unwrap();
        assert!(!ed.project().tasks[1].manual);
        assert_eq!(ed.project().tasks[1].manual_start, None);
    }

    /// Phase (summary), a level-0 blank row, then Phase's child at level 2,
    /// and a top-level task after it.
    fn summary_blank_child_editor() -> Editor {
        let task = |uid, outline_level, is_null| Task {
            uid,
            id: uid,
            name: if is_null {
                String::new()
            } else {
                format!("T{uid}")
            },
            outline_level,
            duration_min: if is_null { 0 } else { 480 },
            is_null,
            ..Task::default()
        };
        let mut phase = task(1, 1, false);
        phase.summary = true;
        Editor::new(Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
            tasks: vec![
                phase,
                task(3, 0, true),
                task(2, 2, false),
                task(4, 1, false),
            ],
            ..Project::default()
        })
    }

    #[test]
    fn a_task_inserted_after_a_blank_row_under_a_summary_is_its_first_child() {
        let mut ed = summary_blank_child_editor();
        let at = ed.add_task(Some(3), "Inserted", 480, false).unwrap();
        assert_eq!(at, 2);
        let levels: Vec<_> = ed.project().tasks.iter().map(|t| t.outline_level).collect();
        assert_eq!(levels, [1, 0, 2, 2, 1]);
        let summaries: Vec<_> = ed.project().tasks.iter().map(|t| t.summary).collect();
        assert_eq!(summaries, [true, false, false, false, false]);
    }

    #[test]
    fn a_summary_delete_runs_past_a_blank_row_between_its_children() {
        let mut ed = summary_blank_child_editor();
        // Phase, a blank row and a child, then another child after the blank.
        let mut proj = ed.project().clone();
        proj.tasks.insert(
            1,
            Task {
                uid: 5,
                id: 5,
                name: "T5".into(),
                outline_level: 2,
                duration_min: 480,
                ..Task::default()
            },
        );
        // A blank row after the subtree's last child stays.
        proj.tasks.insert(
            4,
            Task {
                uid: 6,
                id: 6,
                is_null: true,
                ..Task::default()
            },
        );
        ed.replace_project(proj);
        let uids = |ed: &Editor| ed.project().tasks.iter().map(|t| t.uid).collect::<Vec<_>>();
        assert_eq!(uids(&ed), [1, 5, 3, 2, 6, 4]);
        // The blank row goes with the subtree but is not a subtask.
        assert_eq!(ed.subtree_len(1), Ok(2));
        assert_eq!(ed.delete_task(1), Ok(vec![1, 5, 3, 2]));
        assert_eq!(uids(&ed), [6, 4]);
        assert_eq!(ed.undo_depth(), 1);
        assert!(ed.undo());
        // A blank row owns no subtree: deleting it deletes only itself, even
        // at level 0 above deeper rows.
        assert_eq!(ed.subtree_len(3), Ok(0));
        assert_eq!(ed.delete_task(3), Ok(vec![3]));
        assert_eq!(uids(&ed), [1, 5, 2, 6, 4]);
    }

    #[test]
    fn assigning_a_blank_row_assigns_the_task_it_becomes() {
        let mut proj = blank_row_editor().project().clone();
        proj.resources.push(Resource {
            uid: 1,
            id: 1,
            name: "Half".into(),
            kind: ResourceType::Work,
            max_units: 0.5,
            ..Resource::default()
        });
        for set in [false, true] {
            let mut ed = Editor::new(proj.clone());
            if set {
                ed.set_resources(3, &["Half".into()]).unwrap();
            } else {
                ed.assign_resource(3, "Half").unwrap();
            }
            let t = &ed.project().tasks[1];
            assert!(!t.is_null && t.duration_min == 480);
            // Max. Units (#95) apply, and the work is the new task's one day.
            let a = ed
                .project()
                .assignments
                .iter()
                .find(|a| a.task_uid == 3)
                .unwrap();
            assert_eq!((a.units, a.work_min), (0.5, 240));
            assert_eq!(ed.undo_depth(), 1);
            assert!(ed.undo());
            assert!(ed.project().tasks[1].is_null);
            assert!(ed.project().assignments.is_empty());
        }
    }

    #[test]
    fn a_task_added_below_a_blank_row_takes_the_level_of_the_task_above() {
        let mut ed = blank_row_editor();
        ed.indent(1, 1).unwrap(); // Task 1 at level 2
        let at = ed.add_task(Some(3), "After blank", 480, false).unwrap();
        assert_eq!(at, 2);
        assert_eq!(ed.project().tasks[at].outline_level, 2);
        // With no task above, the level is 1.
        let mut ed = blank_row_editor();
        ed.delete_task(1).unwrap();
        let at = ed.add_task(Some(3), "First", 480, false).unwrap();
        assert_eq!(ed.project().tasks[at].outline_level, 1);
        assert!(!ed.project().tasks[at - 1].summary);
    }

    #[test]
    fn new_tasks_carry_no_imported_task_fields() {
        let mut ed = editor();
        let at = ed.add_task(None, "New", 480, false).unwrap();
        assert_eq!(
            ed.project().tasks[at],
            Task {
                uid: 3,
                id: 3,
                name: "New".into(),
                outline_level: 1,
                duration_min: 480,
                ..Task::default()
            }
        );
    }

    #[test]
    fn a_new_duration_commits_an_estimate() {
        let mut proj = editor().project().clone();
        proj.tasks[0].estimated = Some(true);
        let mut ed = Editor::new(proj);
        // Retyping the same duration commits the estimate, as in Project (#159).
        ed.set_duration_min(1, 480, false).unwrap();
        assert_eq!(ed.project().tasks[0].estimated, Some(false));
        assert_eq!(ed.undo_depth(), 1);
        assert!(ed.dirty());
        ed.set_duration_min(1, 960, false).unwrap();
        assert_eq!(ed.project().tasks[0].estimated, Some(false));
        // An unset flag stays unset.
        ed.set_duration_min(2, 960, false).unwrap();
        assert_eq!(ed.project().tasks[1].estimated, None);
        // A manual task's typed finish changes its duration and commits it.
        let mut proj = editor().project().clone();
        proj.tasks[0].estimated = Some(true);
        proj.tasks[0].manual = true;
        proj.tasks[0].manual_start = Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0));
        let mut ed = Editor::new(proj);
        ed.set_finish(1, DateTime::from_ymd_hm(2026, 1, 6, 0, 0))
            .unwrap();
        let t = &ed.project().tasks[0];
        assert_eq!((t.duration_min, t.estimated), (960, Some(false)));
    }

    #[test]
    fn a_duration_typed_with_a_question_mark_is_estimated() {
        let mut ed = editor();
        ed.set_duration(1, "2d?").unwrap();
        let t = &ed.project().tasks[0];
        assert_eq!((t.duration_min, t.estimated), (960, Some(true)));
        assert_eq!(duration_suffix(ed.project(), 1), "?");
        assert_eq!(ed.undo_depth(), 1);
        // The same estimate again changes nothing.
        ed.mark_saved();
        ed.set_duration(1, " 2d? ").unwrap();
        assert_eq!((ed.undo_depth(), ed.dirty()), (1, false));
        // `?` on the minutes a task already has marks it in one step.
        ed.set_duration(2, "1d?").unwrap();
        let t = &ed.project().tasks[1];
        assert_eq!((t.duration_min, t.estimated), (480, Some(true)));
        assert_eq!((ed.undo_depth(), ed.dirty()), (2, true));
        assert!(ed.undo());
        assert_eq!(ed.project().tasks[1].estimated, None);
        // Retyping it without `?` commits it; an unset flag is not set.
        ed.set_duration(1, "2d").unwrap();
        assert_eq!(ed.project().tasks[0].estimated, Some(false));
        assert_eq!(duration_suffix(ed.project(), 1), "");
        ed.mark_saved();
        let depth = ed.undo_depth();
        ed.set_duration(2, "1d").unwrap();
        assert_eq!(ed.project().tasks[1].estimated, None);
        assert_eq!((ed.undo_depth(), ed.dirty()), (depth, false));
        // An estimate needs a duration.
        assert!(
            ed.update_task(
                1,
                TaskPatch {
                    estimated: Some(true),
                    name: Some("x".into()),
                    ..TaskPatch::default()
                },
            )
            .is_err()
        );
    }

    #[test]
    fn task_durations_parse_an_optional_question_mark() {
        let proj = Project::default();
        for (text, parsed) in [
            ("3d?", Some((1440, true))),
            ("4h?", Some((240, true))),
            ("2?", Some((960, true))),
            (" 2d ? ", Some((960, true))),
            ("2d", Some((960, false))),
            ("2d??", None),
            ("?", None),
            ("nope?", None),
        ] {
            assert_eq!(parse_task_duration(text, &proj), parsed, "{text}");
        }
        // Lags and other callers keep the plain parser.
        assert_eq!(parse_duration("3d?", &proj), None);
    }

    #[test]
    fn a_blank_row_typed_with_an_estimate_becomes_an_estimated_task() {
        let mut ed = blank_row_editor();
        ed.set_duration(3, "2d?").unwrap();
        let t = &ed.project().tasks[1];
        assert_eq!(
            (t.is_null, t.duration_min, t.estimated),
            (false, 960, Some(true))
        );
        let mut ed = blank_row_editor();
        ed.set_duration(3, "1d").unwrap();
        let t = &ed.project().tasks[1];
        assert_eq!((t.duration_min, t.estimated), (480, Some(false)));
    }

    #[test]
    fn added_tasks_carry_a_typed_estimate() {
        let mut ed = editor();
        let at = ed.add_task(None, "Maybe", 960, true).unwrap();
        assert_eq!(ed.project().tasks[at].estimated, Some(true));
        assert_eq!(ed.undo_depth(), 1);
        let at = ed.add_task(None, "Sure", 960, false).unwrap();
        assert_eq!(ed.project().tasks[at].estimated, None);
    }

    #[test]
    fn an_auto_summary_keeps_its_stored_estimate_through_a_duration_patch() {
        for (stored, typed) in [(None, true), (Some(true), false), (Some(false), true)] {
            let mut proj = outline(&[(1, "S", 1), (2, "a", 2)]).project().clone();
            proj.tasks[0].estimated = stored;
            let mut ed = Editor::new(proj);
            for patch in [
                // Minutes that differ from its stale stored duration.
                TaskPatch {
                    duration_min: Some(1440),
                    estimated: Some(typed),
                    ..TaskPatch::default()
                },
                // A rename carrying the duration it has.
                TaskPatch {
                    name: Some("Renamed".into()),
                    duration_min: Some(480),
                    estimated: Some(typed),
                    ..TaskPatch::default()
                },
                // The old rule without an estimate.
                TaskPatch {
                    duration_min: Some(960),
                    ..TaskPatch::default()
                },
            ] {
                ed.update_task(1, patch).unwrap();
                assert_eq!(
                    ed.project().tasks[0].estimated,
                    stored,
                    "{stored:?} {typed}"
                );
            }
        }
    }

    #[test]
    fn a_summary_shows_the_estimate_of_the_tasks_below_it() {
        let mut ed = outline(&[(1, "S", 1), (2, "a", 2), (3, "b", 2), (4, "T", 1)]);
        assert_eq!(duration_suffix(ed.project(), 1), "");
        ed.set_duration(3, "2d?").unwrap();
        assert_eq!(duration_suffix(ed.project(), 1), "?");
        assert_eq!(duration_suffix(ed.project(), 3), "?");
        assert_eq!(duration_suffix(ed.project(), 4), "");
        // Not written into the summary's own flag.
        assert_eq!(ed.project().tasks[0].estimated, None);
        // Committing the last estimated subtask clears it; a stale stored
        // flag on the summary does not show.
        ed.set_duration(3, "2d").unwrap();
        assert_eq!(duration_suffix(ed.project(), 1), "");
        let mut proj = ed.project().clone();
        proj.tasks[0].estimated = Some(true);
        assert_eq!(duration_suffix(&proj, 1), "");
        // A summary takes no estimate of its own.
        let mut proj = ed.project().clone();
        proj.tasks[0].manual = true;
        proj.tasks[0].manual_start = Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0));
        let mut ed = Editor::new(proj);
        ed.set_duration(1, "5d?").unwrap();
        assert_eq!(ed.project().tasks[0].estimated, None);
        // A milestone shows none.
        ed.set_duration(4, "0?").unwrap();
        assert_eq!(duration_suffix(ed.project(), 4), "");
    }

    // ---- the plan's new-task defaults and autolink (#187) ----

    #[test]
    fn a_new_task_takes_the_plans_stated_task_type_and_effort_driven() {
        let mut proj = editor().project().clone();
        proj.default_task_type = Some(TaskType::FixedWork);
        proj.new_tasks_effort_driven = Some(true);
        let mut ed = Editor::new(proj);
        let at = ed.add_task(Some(1), "New", 480, false).unwrap();
        let t = &ed.project().tasks[at];
        assert_eq!(
            (t.task_type, t.effort_driven),
            (Some(TaskType::FixedWork), Some(true))
        );
        let xml = crate::mspdi::write_mspdi(ed.project());
        assert!(xml.contains("<Type>2</Type>"), "{xml}");
        assert!(xml.contains("<EffortDriven>1</EffortDriven>"), "{xml}");
        // Unstated, the task keeps both absent, as before.
        let mut ed = editor();
        let at = ed.add_task(Some(1), "New", 480, false).unwrap();
        let t = &ed.project().tasks[at];
        assert_eq!((t.task_type, t.effort_driven), (None, None));
        let xml = crate::mspdi::write_mspdi(ed.project());
        assert!(
            !xml.contains("<Type>") && !xml.contains("<EffortDriven>"),
            "{xml}"
        );
    }

    #[test]
    fn a_blank_row_follows_the_plans_new_task_defaults() {
        let mut proj = blank_row_editor().project().clone();
        proj.new_tasks_estimated = Some(false);
        proj.default_task_type = Some(TaskType::FixedDuration);
        proj.new_tasks_effort_driven = Some(false);
        let mut ed = Editor::new(proj);
        ed.rename(3, "Typed").unwrap();
        let t = &ed.project().tasks[1];
        assert_eq!(
            (t.duration_min, t.estimated, t.task_type, t.effort_driven),
            (480, None, Some(TaskType::FixedDuration), Some(false))
        );
        assert_eq!(duration_suffix(ed.project(), 3), "");
        let xml = crate::mspdi::write_mspdi(ed.project());
        assert!(!xml.contains("<Estimated>"), "{xml}");
        // Stated on, it is `1 day?` as with the option absent.
        let mut proj = blank_row_editor().project().clone();
        proj.new_tasks_estimated = Some(true);
        let mut ed = Editor::new(proj);
        ed.rename(3, "Typed").unwrap();
        assert_eq!(ed.project().tasks[1].estimated, Some(true));
        assert_eq!(duration_suffix(ed.project(), 3), "?");
    }

    /// Tasks 1 (A) and 2 (B), B depending on A by `link`, with `autolink`.
    fn chain(link: LinkType, lag: i64, autolink: Option<bool>) -> Editor {
        let mut proj = editor().project().clone();
        proj.tasks[1].predecessors = vec![Predecessor::working(1, link, lag)];
        proj.autolink = autolink;
        Editor::new(proj)
    }

    fn preds(ed: &Editor, uid: i32) -> Vec<(i32, LinkType, i64)> {
        ed.project()
            .task(uid)
            .unwrap()
            .predecessors
            .iter()
            .map(|p| (p.uid, p.link, p.lag))
            .collect()
    }

    #[test]
    fn a_task_inserted_into_a_linked_chain_is_linked_into_it() {
        for autolink in [None, Some(true)] {
            let mut ed = chain(LinkType::FinishStart, 960, autolink);
            let at = ed.add_task(Some(1), "N", 480, false).unwrap();
            let n = ed.project().tasks[at].uid;
            assert_eq!(preds(&ed, n), [(1, LinkType::FinishStart, 0)]);
            // N->B keeps A->B's lag.
            assert_eq!(preds(&ed, 2), [(n, LinkType::FinishStart, 960)]);
            assert_schedule(&ed);
            // One undo step restores A->B.
            assert_eq!(ed.undo_depth(), 1);
            assert!(ed.undo());
            assert_eq!(preds(&ed, 2), [(1, LinkType::FinishStart, 960)]);
            assert_eq!(ed.project().tasks.len(), 2);
        }
    }

    #[test]
    fn autolink_leaves_other_inserts_alone() {
        // Autolink off.
        let mut ed = chain(LinkType::FinishStart, 0, Some(false));
        let at = ed.add_task(Some(1), "N", 480, false).unwrap();
        assert!(ed.project().tasks[at].predecessors.is_empty());
        assert_eq!(preds(&ed, 2), [(1, LinkType::FinishStart, 0)]);
        // A link other than finish-to-start.
        let mut ed = chain(LinkType::StartStart, 0, None);
        let at = ed.add_task(Some(1), "N", 480, false).unwrap();
        assert!(ed.project().tasks[at].predecessors.is_empty());
        assert_eq!(preds(&ed, 2), [(1, LinkType::StartStart, 0)]);
        // A is not linked to B.
        let mut ed = editor();
        let at = ed.add_task(Some(1), "N", 480, false).unwrap();
        assert!(ed.project().tasks[at].predecessors.is_empty());
        assert!(preds(&ed, 2).is_empty());
        // Appending below B, and inserting after B: nothing below to split.
        for after in [None, Some(2)] {
            let mut ed = chain(LinkType::FinishStart, 0, None);
            let at = ed.add_task(after, "N", 480, false).unwrap();
            assert!(ed.project().tasks[at].predecessors.is_empty());
            assert_eq!(preds(&ed, 2), [(1, LinkType::FinishStart, 0)]);
        }
        // B at another level: A is the last subtask of a summary.
        let mut ed = chain(LinkType::FinishStart, 0, None);
        let mut proj = ed.project().clone();
        proj.tasks.insert(
            0,
            Task {
                uid: 3,
                id: 3,
                name: "Phase".into(),
                outline_level: 1,
                ..Task::default()
            },
        );
        proj.tasks[1].outline_level = 2;
        ed.replace_project(proj);
        let at = ed.add_task(Some(1), "N", 480, false).unwrap();
        assert_eq!(ed.project().tasks[at].outline_level, 2);
        assert!(ed.project().tasks[at].predecessors.is_empty());
        assert_eq!(preds(&ed, 2), [(1, LinkType::FinishStart, 0)]);
    }

    #[test]
    fn a_manual_task_linked_into_a_chain_starts_after_its_predecessor() {
        // A (10d) -> B in a plan whose new tasks are manual.
        let manual_chain = |autolink| {
            let mut ed = chain(LinkType::FinishStart, 480, autolink);
            let mut proj = ed.project().clone();
            proj.tasks[0].duration_min = 10 * 480;
            proj.new_tasks_are_manual = true;
            ed.replace_project(proj);
            ed
        };
        let mut ed = manual_chain(None);
        let (a_finish, b_start) = {
            let s = ed.schedule();
            (
                s.get(1).unwrap().early_finish,
                s.get(2).unwrap().early_start,
            )
        };
        let at = ed.add_task(Some(1), "N", 480, false).unwrap();
        let n = ed.project().tasks[at].clone();
        assert!(n.manual);
        assert_eq!(preds(&ed, n.uid), [(1, LinkType::FinishStart, 0)]);
        assert_eq!(preds(&ed, 2), [(n.uid, LinkType::FinishStart, 480)]);
        let s = ed.schedule();
        let (nr, br) = (s.get(n.uid).unwrap(), s.get(2).unwrap());
        // N starts where an auto task after A would; B moves past N by
        // N's day, keeping its one-day lag.
        assert!(nr.early_start > a_finish, "{nr:?}");
        assert_eq!(n.manual_start, Some(nr.early_start));
        assert!(br.early_start > nr.early_finish && br.early_start > b_start);
        for t in &ed.project().tasks {
            let r = s.get(t.uid).unwrap();
            assert!(r.total_slack_min >= 0, "task {}: {r:?}", t.uid);
        }
        assert_schedule(&ed);
        // With Autolink off, the manual task starts at the project start as
        // before, unlinked, and B stays put.
        let mut ed = manual_chain(Some(false));
        let at = ed.add_task(Some(1), "N", 480, false).unwrap();
        let n = ed.project().tasks[at].clone();
        assert_eq!(n.manual_start, ed.project().start_date);
        assert!(n.predecessors.is_empty());
        assert_eq!(ed.schedule().get(2).unwrap().early_start, b_start);
    }

    #[test]
    fn under_leveling_an_autolinked_manual_task_starts_after_the_leveled_predecessor() {
        // X and A share Alice, so leveling delays A; B follows A.
        let mut proj = chain(LinkType::FinishStart, 0, None).project().clone();
        proj.tasks.insert(
            0,
            Task {
                uid: 3,
                id: 3,
                ..proj.tasks[0].clone()
            },
        );
        proj.tasks[0].name = "X".into();
        proj.new_tasks_are_manual = true;
        let mut ed = Editor::new(proj);
        ed.assign_resource(3, "Alice").unwrap();
        ed.assign_resource(1, "Alice").unwrap();
        ed.toggle_level();
        let a_finish = ed.disp_finish(1).unwrap();
        assert!(
            a_finish > ed.schedule().get(1).unwrap().early_finish,
            "A leveled later"
        );
        let at = ed.add_task(Some(1), "N", 480, false).unwrap();
        let n = ed.project().tasks[at].clone();
        assert!(n.manual);
        assert_eq!(preds(&ed, 2), [(n.uid, LinkType::FinishStart, 0)]);
        let (n_start, n_finish) = (
            ed.disp_start(n.uid).unwrap(),
            ed.disp_finish(n.uid).unwrap(),
        );
        assert_eq!(n.manual_start, Some(n_start));
        assert!(
            n_start > a_finish,
            "N {n_start:?} before A's leveled finish {a_finish:?}"
        );
        assert!(ed.disp_start(2).unwrap() > n_finish);
        // The saved start is the leveled one.
        assert_eq!(n.stored_start, Some(n_start));
    }

    #[test]
    fn autolink_skips_blank_rows_to_find_the_task_below() {
        let mut proj = chain(LinkType::FinishStart, 0, None).project().clone();
        proj.tasks.insert(
            1,
            Task {
                uid: 3,
                id: 3,
                is_null: true,
                ..Task::default()
            },
        );
        let mut ed = Editor::new(proj);
        let at = ed.add_task(Some(1), "N", 480, false).unwrap();
        let n = ed.project().tasks[at].uid;
        assert_eq!(preds(&ed, n), [(1, LinkType::FinishStart, 0)]);
        assert_eq!(preds(&ed, 2), [(n, LinkType::FinishStart, 0)]);
    }

    // ---- the entry row below the last task (#145) ----

    #[test]
    fn append_row_makes_one_estimated_task_as_one_undo_step() {
        let mut ed = editor();
        ed.indent(2, 1).unwrap();
        ed.mark_saved();
        let before = ed.project().clone();
        let depth = ed.undo_depth();
        let (row, ()) = ed
            .append_row(|ed, uid| ed.rename(uid, "Typed"))
            .unwrap()
            .unwrap();
        assert_eq!(row, 2);
        let t = &ed.project().tasks[2];
        assert_eq!(
            t,
            &Task {
                uid: 3,
                id: 3,
                name: "Typed".into(),
                // The level of the task above, as Project's typed blank row.
                outline_level: 2,
                duration_min: 480,
                estimated: Some(true),
                ..Task::default()
            }
        );
        assert!(ed.schedule().get(3).is_some());
        assert!(ed.dirty());
        assert_eq!((ed.undo_depth(), ed.redo_depth()), (depth + 1, 0));
        assert_eq!(ed.sel(), 0, "the host selects the new row");
        let after = ed.project().clone();
        assert!(ed.undo());
        assert_eq!(ed.project(), &before, "one Undo removes the whole task");
        assert!(ed.redo());
        assert_eq!(ed.project(), &after);
        assert_schedule(&ed);
    }

    #[test]
    fn append_row_passes_the_setters_value_and_uses_every_setter() {
        let day = DateTime::from_ymd_hm(2026, 1, 7, 0, 0);
        type RowEdit = dyn Fn(&mut Editor, i32) -> Result<(), String>;
        let edits: Vec<(&str, Box<RowEdit>)> = vec![
            (
                "duration",
                Box::new(|ed, uid| ed.set_duration_min(uid, 960, false)),
            ),
            (
                "milestone",
                Box::new(|ed, uid| ed.set_duration_min(uid, 0, false)),
            ),
            ("start", Box::new(move |ed, uid| ed.set_start(uid, day))),
            ("finish", Box::new(move |ed, uid| ed.set_finish(uid, day))),
            (
                "predecessors",
                Box::new(|ed, uid| {
                    let preds = parse_predecessors("1", ed.project())?;
                    ed.set_predecessors(uid, preds)
                }),
            ),
            (
                "resources",
                Box::new(|ed, uid| ed.set_resources(uid, &["Bob".to_string()])),
            ),
        ];
        for (name, edit) in edits {
            let mut ed = editor();
            let before = ed.project().clone();
            let appended = ed.append_row(|ed, uid| edit(ed, uid)).unwrap();
            assert_eq!(appended, Some((2, ())), "{name}");
            let t = &ed.project().tasks[2];
            assert!(!t.is_null && t.uid == 3, "{name}");
            assert_eq!(ed.undo_depth(), 1, "{name}");
            assert!(ed.undo());
            assert_eq!(ed.project(), &before, "{name}");
        }
        let mut ed = editor();
        let got = ed
            .append_row(|ed, uid| {
                ed.rename(uid, "Valued")?;
                Ok(uid * 10)
            })
            .unwrap();
        assert_eq!(got, Some((2, 30)));
    }

    #[test]
    fn a_rejected_or_blank_append_changes_nothing() {
        let mut ed = editor();
        ed.rename(1, "Renamed").unwrap();
        ed.rename(2, "Renamed too").unwrap();
        assert!(ed.undo());
        ed.select(1);
        for dirty in [false, true] {
            if !dirty {
                ed.mark_saved();
            }
            assert_unchanged(&mut ed, |ed| {
                let e = ed.append_row(|ed, uid| ed.set_duration_min(uid, -1, false));
                assert!(e.unwrap_err().contains("negative"));
                let e = ed.append_row(|ed, uid| {
                    let preds = parse_predecessors("99", ed.project())?;
                    ed.set_predecessors(uid, preds)
                });
                assert!(e.is_err());
                // An empty name leaves the row blank: nothing to append.
                assert_eq!(ed.append_row(|ed, uid| ed.rename(uid, "")).unwrap(), None);
                assert_eq!(ed.append_row(|_, _| Ok(())).unwrap(), None);
            });
            assert_eq!(ed.redo_depth(), 1);
            // Dirty, still with a redo branch, for the second pass.
            ed.rename(1, "Dirty").unwrap();
            assert!(ed.undo());
        }
    }

    #[test]
    fn append_row_is_exact_with_a_full_history() {
        let mut ed = editor();
        for i in 0..UNDO_CAP + 3 {
            ed.rename(1, &format!("R{i}")).unwrap();
        }
        assert_eq!(ed.undo_depth(), UNDO_CAP);
        let oldest = ed.undo[0].clone();
        assert_unchanged(&mut ed, |ed| {
            assert!(
                ed.append_row(|ed, uid| ed.set_duration_min(uid, -1, false))
                    .is_err()
            );
        });
        assert_eq!(
            ed.undo[0], oldest,
            "a rejected append keeps the oldest step"
        );
        let before = ed.project().clone();
        ed.append_row(|ed, uid| ed.rename(uid, "Typed"))
            .unwrap()
            .unwrap();
        assert_eq!(ed.undo_depth(), UNDO_CAP);
        assert_eq!(
            ed.undo[0],
            ed_undo_after_trim(&oldest),
            "trimmed exactly once"
        );
        assert!(ed.undo());
        assert_eq!(ed.project(), &before, "one Undo removes only the new task");
    }

    /// The oldest entry after one more push trims `oldest`: task 1 renamed once more.
    fn ed_undo_after_trim(oldest: &Project) -> Project {
        let mut next = oldest.clone();
        let n: usize = next.tasks[0].name[1..].parse().unwrap();
        next.tasks[0].name = format!("R{}", n + 1);
        next
    }

    #[test]
    fn append_row_in_a_manual_plan_stamps_a_pinned_manual_task() {
        let mut proj = editor().project().clone();
        proj.new_tasks_are_manual = true;
        let start = proj.start_date.unwrap();
        let mut ed = Editor::new(proj);
        let before = ed.project().clone();
        ed.append_row(|ed, uid| ed.rename(uid, "Typed"))
            .unwrap()
            .unwrap();
        let t = ed.project().tasks[2].clone();
        assert!(t.manual && !t.is_null);
        assert_eq!(
            (t.manual_start, t.manual_duration_min),
            (Some(start), Some(480))
        );
        let r = *ed.schedule().get(3).unwrap();
        assert_eq!(
            (t.stored_start, t.stored_finish),
            (Some(start), Some(r.early_finish))
        );
        assert!(ed.undo());
        assert_eq!(ed.project(), &before);
    }

    #[test]
    fn append_row_on_an_empty_plan_makes_the_first_task() {
        let mut ed = Editor::new(untitled_project());
        let (row, ()) = ed
            .append_row(|ed, uid| ed.rename(uid, "Design"))
            .unwrap()
            .unwrap();
        assert_eq!(row, 0);
        let t = &ed.project().tasks[0];
        assert_eq!(
            (t.uid, t.id, t.name.as_str(), t.outline_level),
            (1, 1, "Design", 1)
        );
        assert_eq!(ed.undo_depth(), 1);
        assert!(ed.undo());
        assert!(ed.project().tasks.is_empty());
    }

    /// Task 2 follows task 1 (FS): an auto task placed by its link.
    fn linked_editor() -> Editor {
        let mut ed = editor();
        ed.add_predecessor(2, 1, LinkType::FinishStart, 0).unwrap();
        ed.mark_saved();
        ed
    }

    type Mode = (bool, Option<DateTime>, Option<DateTime>, Option<i64>);

    fn mode(ed: &Editor, uid: i32) -> Mode {
        let t = ed.project().task(uid).unwrap();
        (
            t.manual,
            t.manual_start,
            t.manual_finish,
            t.manual_duration_min,
        )
    }

    fn shown(ed: &Editor, uid: i32) -> (Option<DateTime>, Option<DateTime>) {
        (ed.disp_start(uid), ed.disp_finish(uid))
    }

    fn saved(ed: &Editor, uid: i32) -> (Option<DateTime>, Option<DateTime>) {
        let t = ed.project().task(uid).unwrap();
        (t.stored_start, t.stored_finish)
    }

    #[test]
    fn switching_to_manual_pins_the_task_where_it_is_shown() {
        let mut ed = linked_editor();
        let (start, finish) = (ed.disp_start(2).unwrap(), ed.disp_finish(2).unwrap());
        assert_eq!(start, DateTime::from_ymd_hm(2026, 1, 6, 8, 0));
        assert_edit(&mut ed, |e| e.set_manual(2, true).unwrap());
        assert_eq!(mode(&ed, 2), (true, Some(start), Some(finish), Some(480)));
        assert_eq!(shown(&ed, 2), (Some(start), Some(finish)));
        assert_eq!(saved(&ed, 2), (Some(start), Some(finish)));
        // Pinned: its predecessor growing no longer moves it.
        ed.set_duration(1, "3d").unwrap();
        assert_eq!(shown(&ed, 2), (Some(start), Some(finish)));
    }

    #[test]
    fn switching_to_auto_releases_the_pin_to_the_links() {
        // Pinned before its predecessor ends, and well after it.
        for day in [5, 14] {
            let mut ed = linked_editor();
            let linked = ed.disp_start(2);
            ed.set_manual(2, true).unwrap();
            ed.set_start(2, DateTime::from_ymd_hm(2026, 1, day, 0, 0))
                .unwrap();
            assert_eq!(
                ed.disp_start(2),
                Some(DateTime::from_ymd_hm(2026, 1, day, 8, 0))
            );
            assert_edit(&mut ed, |e| e.set_manual(2, false).unwrap());
            assert_eq!(mode(&ed, 2), (false, None, None, None));
            assert_eq!(ed.disp_start(2), linked, "placed by its link again");
        }
    }

    #[test]
    fn setting_the_mode_a_task_has_changes_nothing() {
        let mut ed = linked_editor();
        assert_unchanged(&mut ed, |e| e.set_manual(2, false).unwrap());
        ed.set_manual(2, true).unwrap();
        ed.mark_saved();
        assert_unchanged(&mut ed, |e| e.set_manual(2, true).unwrap());
        assert_unchanged(&mut ed, |e| {
            assert_eq!(e.set_manual(99, true).unwrap_err(), "no task with uid 99")
        });
    }

    #[test]
    fn switching_under_leveling_pins_the_leveled_dates() {
        let mut ed = editor();
        ed.assign_resource(1, "Alice").unwrap();
        ed.assign_resource(2, "Alice").unwrap();
        ed.toggle_level();
        let (start, finish) = (ed.disp_start(2).unwrap(), ed.disp_finish(2).unwrap());
        assert!(
            start > ed.schedule().get(2).unwrap().early_start,
            "leveled later"
        );
        assert_edit(&mut ed, |e| e.set_manual(2, true).unwrap());
        assert_eq!(mode(&ed, 2), (true, Some(start), Some(finish), Some(480)));
        assert_eq!(shown(&ed, 2), (Some(start), Some(finish)));
        assert_eq!(saved(&ed, 2), (Some(start), Some(finish)));
        // With leveling off, the pinned task stays where leveling put it.
        ed.toggle_level();
        assert_eq!(shown(&ed, 2), (Some(start), Some(finish)));
    }

    #[test]
    fn a_blank_row_becomes_a_task_with_the_mode() {
        let start = blank_row_editor().project().start_date.unwrap();
        let mut ed = blank_row_editor();
        ed.set_manual(3, true).unwrap();
        assert_eq!(ed.undo_depth(), 1);
        assert!(!ed.project().tasks[1].is_null);
        assert_eq!(mode(&ed, 3), (true, Some(start), None, Some(480)));
        let r = *ed.schedule().get(3).unwrap();
        assert_eq!(r.early_start, start);
        assert_eq!(saved(&ed, 3), (Some(start), Some(r.early_finish)));
        // Auto in an auto plan still makes the row a task.
        let mut ed = blank_row_editor();
        ed.set_manual(3, false).unwrap();
        assert!(!ed.project().tasks[1].is_null);
        assert_eq!(mode(&ed, 3), (false, None, None, None));
        assert_eq!(ed.undo_depth(), 1);
        // Auto in a manual plan overrides the plan's default.
        let mut proj = blank_row_editor().project().clone();
        proj.new_tasks_are_manual = true;
        let mut ed = Editor::new(proj);
        ed.set_manual(3, false).unwrap();
        assert!(!ed.project().tasks[1].is_null);
        assert_eq!(mode(&ed, 3), (false, None, None, None));
        assert_eq!(ed.undo_depth(), 1);
    }

    #[test]
    fn a_summary_switched_to_manual_keeps_its_own_dates() {
        let mut ed = outline(&[(1, "Phase", 1), (2, "A", 2), (3, "B", 2)]);
        ed.add_predecessor(3, 2, LinkType::FinishStart, 0).unwrap();
        let dates = |ed: &Editor| -> Vec<_> {
            ed.project()
                .tasks
                .iter()
                .map(|t| shown(ed, t.uid))
                .collect()
        };
        let before = dates(&ed);
        let (start, finish) = before[0];
        let span = ed.disp_duration_min(1);
        assert_edit(&mut ed, |e| e.set_manual(1, true).unwrap());
        assert!(ed.project().tasks[0].summary);
        assert_eq!(mode(&ed, 1), (true, start, finish, span));
        assert_eq!(dates(&ed), before);
        assert_eq!(saved(&ed, 1), (start, finish));
        assert!(!ed.summary_warning(1));
        let reread = crate::mspdi::read_mspdi(&crate::mspdi::write_mspdi(ed.project())).unwrap();
        assert!(reread.tasks[0].manual);
        // Its subtasks no longer move it: they run past it, and it warns.
        ed.set_duration(3, "3d").unwrap();
        assert_eq!(shown(&ed, 1), (start, finish));
        let rolled = (start.unwrap(), DateTime::from_ymd_hm(2026, 1, 8, 17, 0));
        assert_eq!(ed.disp_rollup(1), Some(rolled));
        assert!(ed.summary_warning(1));
        // Made automatic again, it rolls up and saves that span.
        assert_edit(&mut ed, |e| e.set_manual(1, false).unwrap());
        assert_eq!(mode(&ed, 1), (false, None, None, None));
        assert_eq!(shown(&ed, 1), (Some(rolled.0), Some(rolled.1)));
        assert_eq!(saved(&ed, 1), (Some(rolled.0), Some(rolled.1)));
        assert!(!ed.summary_warning(1));
    }

    #[test]
    fn the_new_task_mode_is_one_undo_step_and_new_tasks_follow_it() {
        let mut ed = editor();
        assert_unchanged(&mut ed, |e| e.set_new_tasks_manual(false));
        assert_edit(&mut ed, |e| e.set_new_tasks_manual(true));
        assert!(ed.project().new_tasks_are_manual);
        let at = ed.add_task(None, "Pinned", 480, false).unwrap();
        assert!(ed.project().tasks[at].manual);
        ed.set_new_tasks_manual(false);
        let at = ed.add_task(None, "Auto", 480, false).unwrap();
        assert!(!ed.project().tasks[at].manual);
        // A blank row follows the plan's mode as it is when it is typed into.
        let mut ed = blank_row_editor();
        ed.set_new_tasks_manual(true);
        ed.rename(3, "Typed").unwrap();
        assert!(ed.project().tasks[1].manual);
    }

    #[test]
    fn a_switched_task_and_the_new_task_mode_survive_a_save() {
        let reopen = |ed: &Editor| {
            crate::mspdi::read_mspdi(&crate::mspdi::write_mspdi(ed.project())).unwrap()
        };
        let mut ed = linked_editor();
        ed.set_manual(2, true).unwrap();
        ed.set_new_tasks_manual(true);
        let reread = reopen(&ed);
        assert!(reread.new_tasks_are_manual);
        let b = reread.task(2).unwrap();
        assert_eq!(
            (
                b.manual,
                b.manual_start,
                b.manual_finish,
                b.manual_duration_min
            ),
            mode(&ed, 2)
        );
        assert_eq!((b.stored_start, b.stored_finish), saved(&ed, 2));
        ed.set_manual(2, false).unwrap();
        ed.set_new_tasks_manual(false);
        let reread = reopen(&ed);
        assert!(!reread.new_tasks_are_manual);
        let b = reread.task(2).unwrap();
        assert_eq!(
            (
                b.manual,
                b.manual_start,
                b.manual_finish,
                b.manual_duration_min
            ),
            (false, None, None, None)
        );
    }

    #[test]
    fn a_patch_switches_the_mode_with_other_fields_in_one_step() {
        let mut ed = linked_editor();
        let start = ed.disp_start(2).unwrap();
        assert_edit(&mut ed, |e| {
            e.update_task(
                2,
                TaskPatch {
                    name: Some("Pinned".into()),
                    duration_min: Some(960),
                    manual: Some(true),
                    ..TaskPatch::default()
                },
            )
            .unwrap()
        });
        // Pinned at its start; the new duration moves the finish, as it does
        // for any manual task.
        assert_eq!(mode(&ed, 2), (true, Some(start), None, Some(960)));
        assert_eq!(ed.project().task(2).unwrap().name, "Pinned");
        let finish = Some(DateTime::from_ymd_hm(2026, 1, 7, 17, 0));
        assert_eq!(shown(&ed, 2), (Some(start), finish));
        assert_eq!(saved(&ed, 2), (Some(start), finish));
    }

    fn state(ed: &Editor) -> (Project, usize, usize, bool) {
        (
            ed.project().clone(),
            ed.undo_depth(),
            ed.redo_depth(),
            ed.dirty(),
        )
    }

    fn links(ed: &Editor) -> Vec<(i32, Vec<i32>)> {
        ed.project()
            .tasks
            .iter()
            .map(|t| (t.uid, t.predecessors.iter().map(|p| p.uid).collect()))
            .collect()
    }

    fn rejected_link(ed: &mut Editor, succ: i32, pred: i32, message: &str) {
        let before = state(ed);
        let err = ed.add_link(succ, Predecessor::fs(pred)).unwrap_err();
        assert!(err.contains(message), "{err}");
        assert_eq!(state(ed), before);
    }

    #[test]
    fn summary_and_descendant_links_are_refused_in_both_directions() {
        let proj =
            crate::mspdi::read_mspdi(include_str!("../../corpus/mspdi/10-summary.xml")).unwrap();
        let mut ed = Editor::new(proj);
        rejected_link(&mut ed, 2, 1, "summary and its subtask");
        rejected_link(&mut ed, 1, 3, "summary and its subtask");

        let mut ed = outline(&[(1, "S", 1), (2, "Nested", 2), (3, "A", 3), (4, "X", 1)]);
        rejected_link(&mut ed, 1, 3, "summary and its subtask");
        rejected_link(&mut ed, 3, 1, "summary and its subtask");
    }

    #[test]
    fn direct_and_transitive_cycles_are_refused_for_all_link_types() {
        let types = [
            LinkType::FinishStart,
            LinkType::StartStart,
            LinkType::FinishFinish,
            LinkType::StartFinish,
        ];
        for link_type in types {
            let mut ed = outline(&[(1, "A", 1), (2, "B", 1), (3, "C", 1)]);
            ed.add_link(2, Predecessor::working(1, link_type, 0))
                .unwrap();
            let before = state(&ed);
            assert!(
                ed.add_link(1, Predecessor::working(2, link_type, 0))
                    .unwrap_err()
                    .contains("circular relationship")
            );
            assert_eq!(state(&ed), before);
            ed.add_link(3, Predecessor::working(2, link_type, 0))
                .unwrap();
            let before = state(&ed);
            assert!(
                ed.add_link(1, Predecessor::working(3, link_type, 0))
                    .unwrap_err()
                    .contains("circular relationship")
            );
            assert_eq!(state(&ed), before);
        }
    }

    #[test]
    fn cycles_through_summary_leaves_are_refused() {
        let rows = [(1, "S", 1), (2, "A", 2), (3, "B", 2), (4, "X", 1)];
        let mut ed = outline(&rows);
        ed.add_link(4, Predecessor::fs(2)).unwrap(); // A -> X
        rejected_link(&mut ed, 1, 4, "circular relationship"); // X -> S -> A

        let mut ed = outline(&rows);
        ed.add_link(1, Predecessor::fs(4)).unwrap(); // X -> S -> A
        rejected_link(&mut ed, 4, 2, "circular relationship"); // A -> X
    }

    #[test]
    fn links_between_separate_outline_branches_are_allowed() {
        let rows = [
            (1, "S1", 1),
            (2, "A", 2),
            (3, "B", 2),
            (4, "S2", 1),
            (5, "C", 2),
            (6, "D", 2),
            (7, "X", 1),
        ];
        let mut ed = outline(&rows);
        ed.add_link(3, Predecessor::fs(2)).unwrap(); // siblings
        ed.add_link(4, Predecessor::fs(1)).unwrap(); // separate summaries
        ed.add_link(7, Predecessor::fs(1)).unwrap(); // summary -> outside

        let mut ed = outline(&rows);
        ed.add_link(1, Predecessor::fs(7)).unwrap(); // outside -> summary
        assert_eq!(
            ed.project().task(1).unwrap().predecessors,
            [Predecessor::fs(7)]
        );
    }

    #[test]
    fn a_blank_successor_is_checked_in_its_materialized_outline_position() {
        for blank_after_last_child in [false, true] {
            let mut proj = outline(&[(1, "S", 1), (2, "A", 2), (3, "X", 1)])
                .project()
                .clone();
            let at = if blank_after_last_child { 2 } else { 1 };
            proj.tasks.insert(
                at,
                Task {
                    uid: 4,
                    id: 4,
                    is_null: true,
                    ..Task::default()
                },
            );
            let mut ed = Editor::new(proj);
            rejected_link(&mut ed, 4, 1, "summary and its subtask");
            let before = state(&ed);
            assert!(
                ed.set_predecessors(4, vec![Predecessor::fs(1)])
                    .unwrap_err()
                    .contains("summary and its subtask")
            );
            assert_eq!(state(&ed), before);
        }
    }

    #[test]
    fn new_link_errors_name_visible_task_ids() {
        let mut proj = outline(&[(10, "S", 1), (20, "A", 2)]).project().clone();
        proj.tasks[0].id = 1;
        proj.tasks[1].id = 2;
        let mut ed = Editor::new(proj);
        let err = ed.add_link(20, Predecessor::fs(10)).unwrap_err();
        assert!(err.contains("2") && err.contains("1"), "{err}");
        assert!(!err.contains("10") && !err.contains("20"), "{err}");
        assert_eq!(
            ed.add_link(20, Predecessor::fs(20)).unwrap_err(),
            "No other task with ID 2"
        );

        let mut proj = outline(&[(10, "A", 1), (20, "B", 1)]).project().clone();
        proj.tasks[0].id = 1;
        proj.tasks[1].id = 2;
        let mut ed = Editor::new(proj);
        ed.add_link(20, Predecessor::fs(10)).unwrap();
        assert_eq!(
            ed.add_link(20, Predecessor::fs(10)).unwrap_err(),
            "Already depends on 1"
        );
    }

    #[test]
    fn cycles_through_large_linked_summaries_are_refused() {
        let mut rows = vec![(1, "P", 1)];
        for uid in 2..=251 {
            rows.push((uid, "P leaf", 2));
        }
        rows.push((252, "S", 1));
        for uid in 253..=502 {
            rows.push((uid, "S leaf", 2));
        }
        rows.push((503, "X", 1));
        let mut ed = outline(&rows);
        ed.add_link(252, Predecessor::fs(1)).unwrap();
        rejected_link(&mut ed, 1, 252, "circular relationship");
        ed.add_link(503, Predecessor::fs(252)).unwrap();
        rejected_link(&mut ed, 1, 503, "circular relationship");
    }

    #[test]
    fn set_predecessors_checks_new_links_and_keeps_loaded_bad_links() {
        let mut proj = outline(&[(1, "S", 1), (2, "A", 2), (3, "X", 1)])
            .project()
            .clone();
        proj.tasks[1].predecessors = vec![Predecessor::fs(1)]; // loaded bad link
        let mut ed = Editor::new(proj);
        let mut retained = Predecessor::fs(1);
        retained.lag = 60;
        ed.set_predecessors(2, vec![retained]).unwrap();
        assert_eq!(ed.project().task(2).unwrap().predecessors, [retained]);
        let before = state(&ed);
        assert!(
            ed.set_predecessors(1, vec![Predecessor::fs(2)])
                .unwrap_err()
                .contains("summary and its subtask")
        );
        assert_eq!(state(&ed), before);

        let mut ed = outline(&[(1, "A", 1), (2, "B", 1), (3, "C", 1)]);
        ed.add_link(2, Predecessor::fs(1)).unwrap();
        ed.add_link(3, Predecessor::fs(2)).unwrap();
        let before = state(&ed);
        assert!(
            ed.set_predecessors(1, vec![Predecessor::fs(3)])
                .unwrap_err()
                .contains("circular relationship")
        );
        assert_eq!(state(&ed), before);
        // A replacement that preserves an acyclic graph still works.
        ed.set_predecessors(3, vec![Predecessor::fs(1)]).unwrap();
    }

    #[test]
    fn unlink_removes_predecessors_and_successors_in_one_step() {
        let mut ed = editor();
        for uid in 3..=4 {
            ed.add_task(None, &format!("Task {uid}"), 480, false)
                .unwrap();
        }
        // 1 -> 2 -> 3, and 2 -> 4 with 1 -> 4: unlinking 2 leaves only 1 -> 4.
        ed.add_predecessor(2, 1, LinkType::FinishStart, 0).unwrap();
        ed.add_predecessor(3, 2, LinkType::StartStart, 0).unwrap();
        ed.add_predecessor(4, 2, LinkType::FinishStart, 60).unwrap();
        ed.add_predecessor(4, 1, LinkType::FinishStart, 0).unwrap();
        let before = links(&ed);
        let depth = ed.undo_depth();
        assert_eq!(ed.unlink_task(2), Ok(3));
        assert_eq!(
            links(&ed),
            [(1, vec![]), (2, vec![]), (3, vec![]), (4, vec![1])]
        );
        assert_eq!(ed.undo_depth(), depth + 1);
        assert_schedule(&ed);
        assert!(ed.undo());
        assert_eq!(links(&ed), before, "one undo restores every link");
    }

    #[test]
    fn unlinking_a_task_without_links_changes_nothing() {
        let mut ed = editor();
        ed.rename(1, "x").unwrap();
        ed.undo();
        ed.mark_saved();
        let before = state(&ed);
        assert_eq!(ed.unlink_task(1), Ok(0));
        assert_eq!(state(&ed), before);
        assert!(ed.unlink_task(99).is_err());
        assert_eq!(state(&ed), before);
    }

    #[test]
    fn unlinking_a_blank_row_keeps_it_blank() {
        let mut ed = editor();
        let mut proj = ed.project().clone();
        proj.tasks[1].is_null = true;
        proj.tasks[1].predecessors.push(Predecessor::fs(1));
        proj.tasks[0].predecessors.push(Predecessor::fs(2));
        ed = Editor::new(proj);
        let blank = ed.project().tasks[1].clone();
        assert_eq!(ed.unlink_task(2), Ok(2));
        let t = &ed.project().tasks[1];
        assert!(t.is_null, "not materialized");
        assert_eq!(
            (t.outline_level, t.duration_min, t.manual),
            (blank.outline_level, blank.duration_min, blank.manual)
        );
        assert_eq!(links(&ed), [(1, vec![]), (2, vec![])]);
    }

    fn baselined() -> Project {
        let mut proj = editor().project().clone();
        let slot = |number| Baseline {
            number,
            start: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
            finish: Some(DateTime::from_ymd_hm(2026, 1, 5, 17, 0)),
            duration_min: Some(480),
            ..Baseline::default()
        };
        proj.tasks[0].set_baseline_slot(slot(0));
        proj.tasks[0].set_baseline_slot(slot(1));
        proj.tasks[1].set_baseline_slot(slot(0));
        let mut crew = Resource {
            uid: 1,
            id: 1,
            name: "Crew".into(),
            ..Resource::default()
        };
        for number in [0, 1] {
            crew.set_baseline_slot(crate::model::ResourceBaseline {
                number,
                work_min: Some(480),
                ..Default::default()
            });
        }
        proj.resources.push(crew);
        let mut a = Assignment {
            uid: 1,
            task_uid: 2,
            resource_uid: 1,
            units: 1.0,
            work_min: 480,
            ..Assignment::default()
        };
        for number in [0, 3] {
            a.set_baseline_slot(crate::model::AssignmentBaseline {
                number,
                work_min: Some(480),
                ..Default::default()
            });
        }
        // Planned work, Baseline work and cost, and Baseline1 work by day.
        a.timephased_data = [1, 4, 5, 16]
            .map(|kind| TimephasedValue {
                kind,
                uid: Some(1),
                value: Some("PT8H0M0S".into()),
                ..TimephasedValue::default()
            })
            .into();
        proj.assignments.push(a);
        proj
    }

    /// Resource baseline slots and assignment timephased kinds.
    fn resource_slots_and_timephased(proj: &Project) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        (
            proj.resources
                .iter()
                .map(|r| r.baselines.iter().map(|b| b.number).collect())
                .collect(),
            proj.assignments
                .iter()
                .map(|a| a.timephased_data.iter().map(|t| t.kind).collect())
                .collect(),
        )
    }

    fn slots(proj: &Project) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        (
            proj.tasks
                .iter()
                .map(|t| t.baselines.iter().map(|b| b.number).collect())
                .collect(),
            proj.assignments
                .iter()
                .map(|a| a.baselines.iter().map(|b| b.number).collect())
                .collect(),
        )
    }

    #[test]
    fn clear_baseline_removes_slot_zero_from_tasks_and_assignments() {
        let mut ed = Editor::new(baselined());
        let before = ed.project().clone();
        assert_eq!(ed.clear_baseline(), Ok(true));
        let cleared = (vec![vec![1], vec![]], vec![vec![3]]);
        // The resource keeps Baseline1; the assignment its planned work and
        // Baseline1 curve, not the Baseline work and cost (4, 5).
        let cleared_rest = (vec![vec![1]], vec![vec![1, 16]]);
        assert_eq!(slots(ed.project()), cleared);
        assert_eq!(resource_slots_and_timephased(ed.project()), cleared_rest);
        assert_eq!(ed.undo_depth(), 1);
        assert!(ed.dirty());
        // A save and reopen keeps Baseline1..10 and not the cleared slot.
        let xml = crate::mspdi::write_mspdi(ed.project());
        let back = crate::mspdi::read_mspdi(&xml).unwrap();
        assert_eq!(slots(&back), cleared);
        assert_eq!(resource_slots_and_timephased(&back), cleared_rest);
        assert!(ed.undo());
        assert_eq!(ed.project(), &before);
        // Only an assignment's slot 0 left: still cleared.
        let mut proj = baselined();
        for t in &mut proj.tasks {
            t.baselines.retain(|b| b.number != 0);
        }
        let mut ed = Editor::new(proj);
        assert_eq!(ed.clear_baseline(), Ok(true));
        assert_eq!(slots(ed.project()), cleared);
        // Only a resource's slot 0, or only an assignment's timephased
        // Baseline, left: still cleared.
        for keep_resource in [true, false] {
            let mut proj = baselined();
            for t in &mut proj.tasks {
                t.baselines.retain(|b| b.number != 0);
            }
            proj.assignments[0].baselines.retain(|b| b.number != 0);
            if keep_resource {
                proj.assignments[0]
                    .timephased_data
                    .retain(|t| !t.is_baseline_slot_zero());
            } else {
                proj.resources[0].baselines.retain(|b| b.number != 0);
            }
            let mut ed = Editor::new(proj);
            assert_eq!(ed.clear_baseline(), Ok(true), "resource: {keep_resource}");
            assert_eq!(
                resource_slots_and_timephased(ed.project()),
                cleared_rest,
                "resource: {keep_resource}"
            );
        }
    }

    #[test]
    fn clearing_without_a_baseline_changes_nothing() {
        let mut proj = baselined();
        proj.tasks[1].baselines.clear();
        proj.tasks[0].baselines.retain(|b| b.number != 0);
        proj.assignments[0].baselines.retain(|b| b.number != 0);
        proj.resources[0].baselines.retain(|b| b.number != 0);
        proj.assignments[0]
            .timephased_data
            .retain(|t| !t.is_baseline_slot_zero());
        let mut ed = Editor::new(proj);
        let before = state(&ed);
        assert_eq!(ed.clear_baseline(), Ok(false));
        assert_eq!(state(&ed), before);
    }

    /// #267: a resource's timephased Baseline (7 work, 8 cost) is its slot 0
    /// too; Baseline1's (20) stays. It alone is enough to clear.
    #[test]
    fn clear_baseline_removes_resource_timephased_slot_zero() {
        let mut proj = editor().project().clone();
        proj.resources.push(Resource {
            uid: 1,
            id: 1,
            name: "Crew".into(),
            timephased_data: [7, 8, 20]
                .map(|kind| TimephasedValue {
                    kind,
                    uid: Some(1),
                    value: Some("PT8H0M0S".into()),
                    ..TimephasedValue::default()
                })
                .into(),
            ..Resource::default()
        });
        let kinds = |proj: &Project| -> Vec<u8> {
            proj.resources[0]
                .timephased_data
                .iter()
                .map(|t| t.kind)
                .collect()
        };
        let mut ed = Editor::new(proj);
        assert_eq!(ed.clear_baseline(), Ok(true));
        assert_eq!(kinds(ed.project()), [20]);
        let xml = crate::mspdi::write_mspdi(ed.project());
        assert_eq!(kinds(&crate::mspdi::read_mspdi(&xml).unwrap()), [20]);
        assert_eq!(ed.clear_baseline(), Ok(false));
    }
}
