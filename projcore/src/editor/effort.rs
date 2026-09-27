//! Task types and effort-driven scheduling (#159): what an edit of a task's
//! duration, of an assignment's units, or of the resources assigned to it
//! does to the other two of duration, work and units, as in Project.
//!
//! An absent `Type` reads as Fixed Units and an absent `EffortDriven` as not
//! effort-driven, Project's own defaults. Only work resources take part:
//! material and cost work is not time. A task with a contoured assignment,
//! a summary and a milestone keep the plain rule (work = duration x units).
use super::*;
use crate::assign::{contoured, is_work};

impl Editor {
    /// Commit staged `resources` and `assignments` for row `i` as one undo
    /// step, first recalculating the task by its type (see [`recalculate`]).
    /// A new duration is checked against the scheduling horizon before
    /// anything changes. A manual task keeps its start and its finish
    /// follows; the estimate and the milestone flag are left alone.
    pub(super) fn commit_assignments(
        &mut self,
        i: usize,
        resources: Vec<Resource>,
        mut assignments: Vec<Assignment>,
    ) -> Result<(), String> {
        let task = self.row_as_edited(i);
        let summary = self.proj.is_outline_summary(i);
        let duration = recalculate(&self.proj, &task, summary, &resources, &mut assignments);
        if duration.is_some() {
            self.validate_cell_horizon(task.uid, duration, None)?;
        }
        let changed = duration.is_some_and(|d| d != task.duration_min);
        self.edit_row(i, |proj, _| {
            proj.resources = resources;
            proj.assignments = assignments;
            if let Some(d) = duration {
                apply_duration(&mut proj.tasks[i], d, changed);
            }
        })?;
        if changed {
            self.stamp_pinned_dates(task.uid);
        }
        Ok(())
    }
}

/// Project's default: a task without a `Type` is Fixed Units.
fn task_type(t: &Task) -> TaskType {
    t.task_type.unwrap_or(TaskType::FixedUnits)
}

/// A Fixed Work task is always effort-driven.
fn effort_driven(t: &Task) -> bool {
    t.effort_driven == Some(true) || task_type(t) == TaskType::FixedWork
}

/// Recalculate `task`'s staged work `assignments` after a resource edit, from
/// the task's assignments in `proj` before it. Returns the task's new
/// duration, if the edit changes it.
///
/// - When the set of work resources changed and the task is effort-driven
///   and had work, the work it had is kept and split by units: a Fixed Units
///   or Fixed Work task takes the duration that work needs, a Fixed Duration
///   one keeps its duration and scales the units instead.
/// - Otherwise, when units changed on an assignment with work, a Fixed Units
///   or Fixed Work task keeps that work and its duration becomes the longest
///   its assignments need. The others keep their work, and so finish early
///   in Project; here every assignment spans its task, so a later duration
///   edit rescales them all to duration x units again. A Fixed Duration task
///   keeps its duration and the work follows the units, as staged.
///
/// Anything else is left as staged: work = duration x units.
fn recalculate(
    proj: &Project,
    task: &Task,
    summary: bool,
    resources: &[Resource],
    assignments: &mut [Assignment],
) -> Option<i64> {
    let uid = task.uid;
    if summary || task.duration_min <= 0 {
        return None;
    }
    let before: Vec<&Assignment> = proj
        .assignments
        .iter()
        .filter(|a| a.task_uid == uid && is_work(&proj.resources, a))
        .collect();
    let after: Vec<usize> = (0..assignments.len())
        .filter(|&k| assignments[k].task_uid == uid && is_work(resources, &assignments[k]))
        .collect();
    if before.iter().any(|a| contoured(a)) || after.iter().any(|&k| contoured(&assignments[k])) {
        return None;
    }
    let set = |rids: &mut dyn Iterator<Item = i32>| rids.collect::<std::collections::BTreeSet<_>>();
    let set_changed = set(&mut before.iter().map(|a| a.resource_uid))
        != set(&mut after.iter().map(|&k| assignments[k].resource_uid));
    if set_changed {
        redistribute(task, &before, assignments, &after)
    } else {
        units_changed(task, &before, assignments, &after)
    }
}

/// Effort-driven: keep the work `before` had across the assignments `after`.
fn redistribute(
    task: &Task,
    before: &[&Assignment],
    assignments: &mut [Assignment],
    after: &[usize],
) -> Option<i64> {
    let work: i64 = before.iter().map(|a| a.work_min.max(0)).sum();
    let units: f64 = after.iter().map(|&k| assignments[k].units).sum();
    let delayed = before.iter().any(|a| a.delay_min() > 0)
        || after.iter().any(|&k| assignments[k].delay_min() > 0);
    if !effort_driven(task)
        || work <= 0
        || after.is_empty()
        || !units.is_finite()
        || units <= 0.0
        || delayed
    {
        return None;
    }
    let share = |u: f64| (work as f64 * u / units).round() as i64;
    match task_type(task) {
        TaskType::FixedDuration => {
            let scale = work as f64 / (units * task.duration_min as f64);
            for &k in after {
                let a = &mut assignments[k];
                let u = a.units * scale;
                a.set_units(u, work_for(task.duration_min, u));
            }
            None
        }
        TaskType::FixedUnits | TaskType::FixedWork => {
            let duration = (work as f64 / units).round() as i64;
            if duration <= 0 {
                return None;
            }
            for &k in after {
                let w = share(assignments[k].units);
                assignments[k].set_work(w);
            }
            Some(duration)
        }
    }
}

/// Units changed on the same resources: a Fixed Units or Fixed Work task
/// keeps each changed assignment's work and stretches to the longest.
fn units_changed(
    task: &Task,
    before: &[&Assignment],
    assignments: &mut [Assignment],
    after: &[usize],
) -> Option<i64> {
    if task_type(task) == TaskType::FixedDuration {
        return None;
    }
    let mut changed = false;
    for &k in after {
        let a = &mut assignments[k];
        let Some(old) = before.iter().find(|b| b.uid == a.uid) else {
            continue;
        };
        if old.units != a.units && old.work_min > 0 {
            a.set_work(old.work_min);
            changed = true;
        }
    }
    if !changed {
        return None;
    }
    after
        .iter()
        .map(|&k| &assignments[k])
        .filter(|a| a.units > 0.0 && a.work_min > 0)
        .map(|a| (a.work_min as f64 / a.units).round() as i64 + a.delay_min())
        .max()
        .filter(|&d| d > 0)
}

/// A duration edit of a Fixed Work task: each flat work assignment keeps its
/// work and works it over the new span (from its delay to the task finish),
/// so its units change instead. Without work, or without a span left, its
/// units stay. A contoured one's units are its peak and stay too.
pub(super) fn fixed_work_units(proj: &mut Project, i: usize, new_min: i64) {
    let uid = proj.tasks[i].uid;
    let Project {
        assignments,
        resources,
        ..
    } = proj;
    for a in assignments.iter_mut().filter(|a| a.task_uid == uid) {
        if !is_work(resources, a) || contoured(a) || a.work_min <= 0 {
            continue;
        }
        let span = new_min - a.delay_min();
        if span > 0 {
            a.units = a.work_min as f64 / span as f64;
            a.peak_units = None;
        }
    }
}

#[cfg(test)]
mod tests;
