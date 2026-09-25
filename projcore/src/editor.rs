//! Shared project editing, selection, history and derived scheduling state.
//!
//! Edits address stable task UIDs. Validation precedes the undo snapshot, so a
//! rejected edit leaves all editor state untouched. Hosts own file I/O and UI
//! messages; [`Editor::mark_saved`] acknowledges a successful save.

use crate::datetime::DateTime;
use crate::model::{
    Assignment, Baseline, ConstraintType, LinkType, Predecessor, Project, Resource, ResourceType,
    Task,
};
use crate::schedule::{Leveled, Schedule, level, schedule};

const UNDO_CAP: usize = 100;

mod cells;
pub use cells::{
    day_finish, format_duration_exact, format_predecessors, format_resource_names, parse_cell_date,
    parse_predecessors,
};
use cells::{format_units, parse_resource_token};

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
    undo: Vec<Project>,
    redo: Vec<Project>,
    dirty: bool,
    sched: Schedule,
    leveled: bool,
    level: Option<Leveled>,
    last_find: String,
}

impl Editor {
    pub fn new(proj: Project) -> Self {
        Self::restored(proj, false)
    }

    /// Restore saved session content and its dirty flag, with empty edit history.
    pub fn restored(mut proj: Project, dirty: bool) -> Self {
        recompute_summaries(&mut proj);
        let sched = schedule(&proj);
        Self {
            proj,
            sel: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            dirty,
            sched,
            leveled: false,
            level: None,
            last_find: String::new(),
        }
    }

    /// Begin a new document session, retaining find and leveling preferences.
    pub fn replace_project(&mut self, proj: Project) {
        self.proj = proj;
        self.sel = 0;
        self.undo.clear();
        self.redo.clear();
        self.dirty = false;
        self.reschedule();
    }

    pub fn project(&self) -> &Project {
        &self.proj
    }
    pub fn sel(&self) -> usize {
        self.sel
    }
    pub fn select(&mut self, index: usize) {
        self.sel = index.min(self.proj.tasks.len().saturating_sub(1));
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
        materialized(&self.proj, i, self.new_task_start())
    }

    /// A structural edit of row `i`. A blank row first becomes a task (see
    /// [`materialized`]); `edit` learns whether it was blank. A manual task
    /// made that way gets its dates pinned, as [`Self::add_task`] does.
    fn edit_row(&mut self, i: usize, edit: impl FnOnce(&mut Project, bool)) -> Result<(), String> {
        let (was_blank, uid) = (self.proj.tasks[i].is_null, self.proj.tasks[i].uid);
        let start = self.new_task_start();
        self.edit_structure(|proj| {
            if was_blank {
                proj.tasks[i] = materialized(proj, i, start);
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
    fn push_undo(&mut self, prev: Project) {
        self.undo.push(prev);
        if self.undo.len() > UNDO_CAP {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn reschedule(&mut self) {
        recompute_summaries(&mut self.proj);
        self.sched = schedule(&self.proj);
        self.level = self.leveled.then(|| level(&self.proj));
    }

    fn changed(&mut self) {
        self.dirty = true;
        self.select(self.sel);
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
        self.redo.push(std::mem::replace(&mut self.proj, prev));
        self.changed();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.proj, next));
        self.changed();
        true
    }

    /// Find the next matching row, wrapping after the selected row. An empty
    /// query repeats the previous search. Searching never changes history.
    pub fn find(&mut self, query: &str) -> FindOutcome {
        let query = query.trim().to_lowercase();
        if !query.is_empty() {
            self.last_find = query;
        }
        let n = self.proj.tasks.len();
        if self.last_find.is_empty() || n == 0 {
            return FindOutcome::Inactive;
        }
        for step in 1..=n {
            let i = (self.sel + step) % n;
            if self.proj.tasks[i]
                .name
                .to_lowercase()
                .contains(&self.last_find)
            {
                self.sel = i;
                return FindOutcome::Found(i);
            }
        }
        FindOutcome::NotFound
    }

    /// Insert after a UID, or append. As in Microsoft Project, the new task is
    /// the next sibling of the task above it, or that task's first child when
    /// it is a summary; the row it pushes down plays no part, and blank rows
    /// are skipped (see [`level_at`]). The returned row is not automatically
    /// selected.
    pub fn add_task(
        &mut self,
        after: Option<i32>,
        name: &str,
        duration_min: i64,
    ) -> Result<usize, String> {
        validate_duration(duration_min)?;
        let at = match after {
            Some(uid) => self.index(uid)? + 1,
            None => self.proj.tasks.len(),
        };
        let outline_level = level_at(&self.proj, at);
        let uid = self
            .proj
            .tasks
            .iter()
            .map(|t| t.uid)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("No task IDs available")?;
        // Follow the plan's own default mode; a manual task starts at the
        // project start (or the anchor the schedule actually uses).
        let manual = self.proj.new_tasks_are_manual;
        let manual_start = manual.then(|| self.new_task_start());
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
                    ..Task::default()
                },
            )
        })?;
        self.stamp_pinned_dates(uid);
        Ok(at)
    }

    /// Rows `uid` owns in the positional outline: itself, then every following
    /// row deeper than it. Uses levels, not the `summary` flag, so a stale flag
    /// cannot cause a partial delete. Blank rows are outside the outline: they
    /// neither end a subtree nor own one. One between two of its descendants
    /// goes with it; one after its last descendant stays.
    fn subtree(&self, uid: i32) -> Result<std::ops::Range<usize>, String> {
        let i = self.index(uid)?;
        let task = &self.proj.tasks[i];
        if task.is_null {
            return Ok(i..i + 1);
        }
        let mut end = i + 1;
        for (k, row) in self.proj.tasks.iter().enumerate().skip(i + 1) {
            if row.is_null {
                continue;
            }
            if row.outline_level <= task.outline_level {
                break;
            }
            end = k + 1;
        }
        Ok(i..end)
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

    pub fn set_duration(&mut self, uid: i32, text: &str) -> Result<(), String> {
        let min = parse_duration(text, &self.proj)
            .ok_or_else(|| format!("Couldn't read duration '{text}' (try 3d, 4h, 2w)"))?;
        self.set_duration_min(uid, min)
    }

    pub fn set_duration_min(&mut self, uid: i32, min: i64) -> Result<(), String> {
        self.update_task(
            uid,
            TaskPatch {
                duration_min: Some(min),
                ..TaskPatch::default()
            },
        )
    }

    pub fn update_task(&mut self, uid: i32, patch: TaskPatch) -> Result<(), String> {
        let i = self.index(uid)?;
        if patch.name.is_none() && patch.duration_min.is_none() && patch.level.is_none() {
            return Err("task.set needs at least one of 'name', 'duration', 'level'".into());
        }
        if patch.level.is_some_and(|lv| !(1..=20).contains(&lv)) {
            return Err("'level' must be 1..=20".into());
        }
        if let Some(min) = patch.duration_min {
            validate_duration(min)?;
        }
        if patch.duration_min.is_some() {
            self.validate_cell_horizon(uid, patch.duration_min, None)?;
        }
        let t = &self.proj.tasks[i];
        let duration_changed = patch.duration_min.is_some_and(|min| min != t.duration_min);
        if patch.name.as_ref().is_none_or(|name| *name == t.name)
            && patch
                .duration_min
                .is_none_or(|min| min == t.duration_min && (min == 0) == t.milestone)
            && patch.level.is_none_or(|lv| lv == t.outline_level)
        {
            return Ok(());
        }
        self.edit_row(i, |proj, was_blank| {
            // A blank row's `1 day?` is a default, not a duration the user
            // typed: typing any duration into it commits it.
            let t = &mut proj.tasks[i];
            if let Some(name) = patch.name {
                t.name = name;
            }
            if let Some(min) = patch.duration_min {
                // Against the row as materialized: a blank row's default
                // `1 day?` (and a manual plan's ManualDuration) is replaced.
                let changed = was_blank || min != t.duration_min;
                if changed {
                    commit_estimate(t);
                }
                t.duration_min = min;
                t.milestone = min == 0;
                // A manual task keeps its start; its finish follows a new
                // duration instead of staying pinned. Repeating the current
                // duration keeps a pinned finish.
                if t.manual && changed {
                    t.manual_duration_min = Some(min);
                    t.manual_finish = None;
                }
            }
            if let Some(lv) = patch.level {
                t.outline_level = lv;
            }
        })?;
        // Only a date change restamps: projcore ignores calendar exceptions,
        // so our finish can differ from the one Project wrote. A blank row's
        // new dates are stamped by edit_row.
        if duration_changed {
            self.stamp_pinned_dates(uid);
        }
        Ok(())
    }

    /// After an edit to a manual task's dates, record its pinned start and
    /// scheduled finish as the Start/Finish a save writes. Project does not
    /// reschedule manual tasks on open, so they must agree with
    /// ManualStart/Duration. Stored dates do feed the scheduler (the anchor
    /// of a plan without a start date, and the timeline reach), so reschedule
    /// after stamping to keep the schedule in step with the model.
    fn stamp_pinned_dates(&mut self, uid: i32) {
        let Ok(i) = self.index(uid) else {
            return;
        };
        let Some((start, _)) = self.proj.tasks[i].pinned_dates() else {
            return;
        };
        let finish = self.sched.get(uid).map(|r| r.early_finish);
        let task = &mut self.proj.tasks[i];
        if (task.stored_start, task.stored_finish) == (Some(start), finish) {
            return;
        }
        task.stored_start = Some(start);
        task.stored_finish = finish;
        self.reschedule();
    }

    pub fn add_predecessor(
        &mut self,
        uid: i32,
        pred: i32,
        link: LinkType,
        lag_min: i64,
    ) -> Result<(), String> {
        let i = self.index(uid)?;
        if uid == pred || self.index(pred).is_err() || self.is_blank(pred) {
            return Err(format!("No other task with ID {pred}"));
        }
        if self.proj.tasks[i]
            .predecessors
            .iter()
            .any(|p| p.uid == pred)
        {
            return Err(format!("Already depends on {pred}"));
        }
        self.edit_row(i, |proj, _| {
            proj.tasks[i].predecessors.push(Predecessor {
                uid: pred,
                link,
                lag_min,
            });
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
                let (base, units) = parse_resource_token(name)?;
                (find_or_stage_resource(&mut resources, base.trim())?, units)
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
            let Some(u) =
                units.filter(|&u| format_units(u) != format_units(self.proj.assignments[k].units))
            else {
                return Ok(AssignOutcome::AlreadyAssigned);
            };
            let u = checked_units(u, name)?;
            self.edit_row(i, |proj, _| {
                let a = &mut proj.assignments[k];
                a.units = u;
                a.work_min = work_for(duration, u);
            })?;
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
        let assignment = new_assignment(&mut next_aid, uid, rid, units, duration)?;
        self.edit_row(i, |proj, _| {
            proj.resources = resources;
            proj.assignments.push(assignment);
        })?;
        Ok(AssignOutcome::Assigned)
    }

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
                    },
                ))
            })
            .collect();
        for (i, baseline) in baselines {
            self.proj.tasks[i].set_baseline_slot(baseline);
        }
        self.changed();
    }

    pub fn toggle_milestone(&mut self, uid: i32) -> Result<(), String> {
        let i = self.index(uid)?;
        // A blank row toggles from the duration it gets as a task.
        let min = if self.row_as_edited(i).duration_min == 0 {
            480
        } else {
            0
        };
        self.set_duration_min(uid, min)
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
        work_min: work_for(duration_min, units),
        ..Assignment::default()
    })
}

/// The outline level of a task placed at row `at`, whether inserted there or
/// a blank row there becoming a task. As in Microsoft Project it is the next
/// sibling of the nearest task above, or that task's first child when it is
/// a summary (so the summary keeps its children); the rows below play no
/// part. Blank rows are outside the outline and skipped. At least 1.
fn level_at(proj: &Project, at: usize) -> u32 {
    let Some(above) = proj.tasks[..at].iter().rposition(|t| !t.is_null) else {
        return 1;
    };
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
/// level, without a duration it gets Project's new-task default, `1 day?`,
/// instead of turning into a milestone, and in a plan whose new tasks are
/// manual it becomes manual at `start`, as [`Editor::add_task`] makes one.
fn materialized(proj: &Project, i: usize, start: DateTime) -> Task {
    let mut t = proj.tasks[i].clone();
    if t.is_null {
        t.is_null = false;
        if t.outline_level == 0 {
            t.outline_level = level_at(proj, i);
        }
        if t.duration_min == 0 {
            t.duration_min = proj.days_to_minutes(1.0);
            t.estimated = Some(true);
            t.milestone = false;
        }
        if proj.new_tasks_are_manual && !t.manual {
            t.manual = true;
            t.manual_start = t.manual_start.or(Some(start));
            t.manual_duration_min = t.manual_duration_min.or(Some(t.duration_min));
        }
    }
    t
}

/// Typing a duration without `?` commits an estimated one, as in Project.
fn commit_estimate(t: &mut Task) {
    if t.estimated == Some(true) {
        t.estimated = Some(false);
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
            task.set_baseline_slot(saved);
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
        ed.set_duration_min(2, 1440).unwrap();
        assert_eq!(
            ed.project().tasks[1].baseline(0).unwrap().duration_min,
            Some(960)
        );
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
        proj.calendars.push(crate::model::Calendar {
            uid: 3,
            name: "Closed".into(),
            week: Default::default(),
        });
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
                            ed.add_task(Some(1), "Inserted", 480)
                                .unwrap_err()
                                .contains("Closed")
                        );
                        assert!(
                            ed.add_task(None, "Appended", 480)
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
                let at = ed.add_task(Some(1), "Inserted", 480).unwrap();
                let tasks = &ed.project().tasks;
                assert_eq!(tasks[at].outline_level, tasks[0].outline_level + 1);
                assert!(tasks[0].summary);
                assert_reopens(&ed);
            }
            ed.rename(2, "Renamed").unwrap();
            assert_reopens(&ed);
            ed.set_duration_min(2, 960).unwrap();
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
                ed.add_task(None, "New task", 480).unwrap();
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
            e.add_task(Some(1), "Inserted", 960).unwrap();
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
            e.set_duration_min(3, 4800).unwrap();
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
            |e| e.add_task(Some(999), "bad", 480).map(|_| ()),
            |e| e.delete_task(999).map(|_| ()),
            |e| e.indent(999, 1),
            |e| e.rename(999, "bad"),
            |e| e.set_duration(999, "1d"),
            |e| e.set_duration(1, "banana"),
            |e| e.set_duration_min(999, 480),
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
        let at = ed.add_task(None, "Append", 480).unwrap();
        assert_eq!(at, 2);
        assert_eq!(ed.project().tasks[at].outline_level, 3);
        // Task 1 is a summary over Task 2, so the insert is its first child.
        let at = ed.add_task(Some(1), "Insert", 480).unwrap();
        assert_eq!(at, 1);
        assert_eq!(ed.project().tasks[at].outline_level, 3);
        assert_eq!(ed.sel(), 1);
        ed.replace_project(Project::default());
        assert_eq!(ed.add_task(None, "First", 0).unwrap(), 0);
        assert_eq!(ed.project().tasks[0].outline_level, 1);
        assert!(ed.project().tasks[0].milestone);
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
        ed.add_task(None, "S", 480).unwrap();
        ed.add_task(None, "c1", 480).unwrap();
        ed.indent(uid_of(&ed, "c1"), 1).unwrap();
        let before = ed.project().clone();
        let s = uid_of(&ed, "S");
        assert_eq!(ed.add_task(Some(s), "New", 480).unwrap(), 1);
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
        assert_eq!(ed.add_task(Some(3), "New", 480).unwrap(), 3);
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
        ed.add_task(Some(1), "New", 480).unwrap();
        assert_eq!(names(&ed), [("S", 1), ("New", 1), ("Y", 1)]);
        // Appending copies the last row, which is never a summary.
        let mut ed = outline(&[(1, "S", 1), (2, "c1", 2)]);
        ed.add_task(None, "New", 480).unwrap();
        assert_eq!(
            rows(&ed),
            [("S", 1, true), ("c1", 2, false), ("New", 2, false)]
        );
    }

    #[test]
    fn a_task_inserted_under_a_nested_summary_goes_one_level_deeper() {
        let mut ed = outline(&[(1, "A", 1), (2, "S", 2), (3, "c", 3), (4, "B", 1)]);
        ed.add_task(Some(2), "New", 480).unwrap();
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
        ed.add_task(Some(1), "New", 480).unwrap();
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

        let at = ed.add_task(None, "Replacement", 480).unwrap();
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
        assert_eq!(ed.project().resources, before.resources);
        assert_schedule(&ed);

        let at = ed.add_task(None, "Replacement", 480).unwrap();
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
        ed.add_task(None, "Undone", 480).unwrap();
        assert!(ed.undo());
        assert_eq!(ed.redo_depth(), 1);
        let before = ed.project().clone();
        ed.add_task(None, "Structural", 480).unwrap();
        assert_eq!((ed.undo_depth(), ed.redo_depth()), (1, 0));
        assert!(ed.undo());
        assert_eq!(ed.project(), &before);
        let mut states = vec![before];
        for i in 0..UNDO_CAP + 5 {
            ed.add_task(None, &format!("T{i}"), 480).unwrap();
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
            crate::model::Calendar {
                uid: 1,
                name: "Closed".into(),
                week: Default::default(),
            },
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
                    vec![crate::model::Predecessor {
                        uid: 2,
                        link: LinkType::FinishStart,
                        lag_min: 0,
                    }]
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
        let at = ed.add_task(None, "Third", 480).unwrap();
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
        let fs1 = Predecessor {
            uid: 1,
            link: LinkType::FinishStart,
            lag_min: 0,
        };
        let day = DateTime::from_ymd_hm(2026, 1, 7, 0, 0);
        let edits: Vec<(&str, Box<Edit>)> = vec![
            ("indent", Box::new(|ed| ed.indent(3, 1))),
            ("set_duration", Box::new(|ed| ed.set_duration_min(3, 960))),
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
        ed.set_duration_min(3, 960).unwrap();
        let t = &ed.project().tasks[1];
        assert_eq!(
            (t.duration_min, t.estimated, t.milestone),
            (960, Some(false), false)
        );
        // Typing exactly the default one day commits it too.
        let mut ed = blank_row_editor();
        ed.set_duration_min(3, 480).unwrap();
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
            "No other task with ID 3"
        );
        let blank = Predecessor {
            uid: 3,
            link: LinkType::FinishStart,
            lag_min: 0,
        };
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
        edited[0].lag_min = 480;
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
        ed.set_duration_min(3, 960).unwrap();
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
            |ed: &mut Editor| ed.set_duration_min(3, 0),
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
        let at = ed.add_task(Some(3), "Inserted", 480).unwrap();
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
        let at = ed.add_task(Some(3), "After blank", 480).unwrap();
        assert_eq!(at, 2);
        assert_eq!(ed.project().tasks[at].outline_level, 2);
        // With no task above, the level is 1.
        let mut ed = blank_row_editor();
        ed.delete_task(1).unwrap();
        let at = ed.add_task(Some(3), "First", 480).unwrap();
        assert_eq!(ed.project().tasks[at].outline_level, 1);
        assert!(!ed.project().tasks[at - 1].summary);
    }

    #[test]
    fn new_tasks_carry_no_imported_task_fields() {
        let mut ed = editor();
        let at = ed.add_task(None, "New", 480).unwrap();
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
        // Retyping the same duration is a no-op (see the follow-up on #80).
        ed.set_duration_min(1, 480).unwrap();
        assert_eq!(ed.project().tasks[0].estimated, Some(true));
        ed.set_duration_min(1, 960).unwrap();
        assert_eq!(ed.project().tasks[0].estimated, Some(false));
        // An unset flag stays unset.
        ed.set_duration_min(2, 960).unwrap();
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
}
