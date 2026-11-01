//! An elapsed task's move to working time (#484). A leaf whose
//! `DurationFormat` is elapsed holds wall-clock minutes in `duration_min`;
//! every duration the editor computes or reads from a cell is working time
//! (no cell parses an elapsed unit). So an edit that writes working minutes
//! over an elapsed task first re-expresses the whole task in working time
//! ([`Editor::elapsed_to_working`]), then runs exactly as on a working task:
//! a typed duration (`update_task_with`), a manual task's typed finish
//! (`set_finish_at`) and every progress setter. The conversion and the edit
//! are one undo step. An effort recalculation (work / units, the work
//! counted over the elapsed minutes) is elapsed minutes and does not
//! convert. Unmeasured against Project 2024, which keeps progress and
//! typed durations on an elapsed task.
use super::*;

/// The refusal of a working-time edit on an elapsed task whose wall-clock
/// window holds no working time ([`Editor::elapsed_to_working`]).
pub(super) const ELAPSED_NO_WORKING_TIME: &str =
    "This elapsed task spans no working time; progress is counted in working time";

impl Editor {
    /// Re-express task `i` in working time when it is elapsed, in the
    /// current undo step (a step of its own outside a batch): its window is
    /// its elapsed duration from its actual start, else its scheduled start
    /// (never the tracked schedule's span, which already counts it as
    /// working minutes), and `working` the working minutes the window covers
    /// on its calendar. The duration becomes `working`; the actual,
    /// remaining and manual durations, and its work resources' actual and
    /// actual overtime work, scale by `working / duration`; its assignments'
    /// work follows at the same units (`scale_work`, every task type); the
    /// format moves to the same unit in working time. The edit's refresh then
    /// derives cost, remaining work and % work complete. Refused, changing
    /// nothing, with no start or a window with no working time (a weekend):
    /// the task would become a milestone. A working task is left alone.
    pub(super) fn elapsed_to_working(&mut self, i: usize) -> Result<(), String> {
        let task = &self.proj.tasks[i];
        if !task.elapsed_duration() {
            return Ok(());
        }
        let start = task
            .actual_start
            .or_else(|| self.sched.get(task.uid).map(|r| r.early_start))
            .ok_or("The task has no start")?;
        let calendar = crate::assign::task_calendar(&self.proj, task);
        let elapsed = task.duration_min.max(0);
        let working =
            crate::schedule::working_minutes_on(&calendar, start, start.add_minutes(elapsed));
        if elapsed > 0 && working <= 0 {
            return Err(ELAPSED_NO_WORKING_TIME.into());
        }
        self.edit_row(i, |proj, _| to_working_time(proj, i, working))
    }

    /// Run `edit` on elapsed task `i` re-expressed in working time first
    /// ([`Self::elapsed_to_working`]), as one undo step. An `edit` that
    /// fails, or changes nothing itself, leaves the task as it was (a batch
    /// this one runs inside keeps the conversion of an `edit` that changed
    /// nothing, and rolls a failure back with the rest). The callers
    /// re-enter themselves in `edit`; the task is no longer elapsed there.
    pub(super) fn in_working_time<T>(
        &mut self,
        i: usize,
        edit: impl FnOnce(&mut Editor) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut kept = None;
        let result = self.batch(|ed| {
            ed.elapsed_to_working(i)?;
            let pushes = ed.pushes;
            let value = edit(ed)?;
            if ed.pushes == pushes {
                // Unchanged: roll the conversion back with the batch.
                kept = Some(value);
                return Err(String::new());
            }
            Ok(value)
        });
        match kept {
            Some(value) => Ok(value),
            None => result,
        }
    }
}

/// [`Editor::elapsed_to_working`]'s rewrite of task `i` over `working`
/// minutes.
fn to_working_time(proj: &mut Project, i: usize, working: i64) {
    let t = &mut proj.tasks[i];
    let elapsed = t.duration_min;
    let scale = move |m: i64| {
        if elapsed <= 0 {
            return m;
        }
        i64::try_from(i128::from(m) * i128::from(working) / i128::from(elapsed)).unwrap_or(m)
    };
    if elapsed > 0 {
        t.duration_min = working;
    }
    t.actual_duration_min = t.actual_duration_min.map(scale);
    t.remaining_duration_min = t.remaining_duration_min.map(scale);
    t.manual_duration_min = t.manual_duration_min.map(scale);
    to_working_format(t);
    if elapsed <= 0 {
        return;
    }
    let uid = t.uid;
    scale_work(proj, i, elapsed, working);
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
        a.actual_work_min = a.actual_work_min.map(scale);
        a.actual_overtime_work_min = a.actual_overtime_work_min.map(scale);
    }
}

/// The same unit in working time: elapsed codes are the working ones plus
/// one, and elapsed days (8) become days, the default, which is no format.
/// The estimated bit stays.
fn to_working_format(t: &mut Task) {
    if t.elapsed_duration() {
        t.duration_format = t
            .duration_format
            .map(|code| code - 1)
            .filter(|&code| code != 7);
    }
}

#[cfg(test)]
mod tests;
