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
//! convert; an assignment's Delay, working time on every task, enters it as
//! the wall-clock minutes to where it lands (`assign::delay_in_task_unit`). Unmeasured against Project 2024, which keeps progress and
//! typed durations on an elapsed task.
use super::*;

/// The refusal, on an elapsed task whose wall-clock window holds no working
/// time, of a progress write, or of any write on a task with progress
/// ([`Editor::elapsed_to_working`]).
pub(super) const ELAPSED_NO_WORKING_TIME: &str =
    "This elapsed task spans no working time; durations and progress are edited in working time";

/// What a write of working minutes over an elapsed task does
/// ([`Editor::in_working_time`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Write {
    /// A typed duration (`update_task_with`): it replaces the duration, so
    /// it needs no window unless the task has progress, and it is a change of the task even when the
    /// minutes typed are the ones the conversion gave (`1d` on `1 eday`
    /// from a Monday morning): the duration is now working time.
    Duration,
    /// A manual task's typed finish: it replaces the duration, so it needs
    /// no window unless the task has progress; its own pinned finish again
    /// changes nothing.
    Finish,
    /// A progress setter: the progress is counted in the window's working
    /// time, so a window with none refuses it.
    Progress,
}

impl Editor {
    /// Re-express task `i` in working time when it is elapsed, in the
    /// current undo step (a step of its own outside a batch): its window is
    /// its elapsed duration from its actual start, else its scheduled start
    /// (never the tracked schedule's span, which already counts it as
    /// working minutes), and `working` the working minutes the window covers
    /// on its calendar. The duration becomes `working`; the actual,
    /// remaining and manual durations, and its work resources' actual and
    /// actual overtime work, scale by `working / duration`; a flat work
    /// assignment instead takes the working time its own span covered
    /// ([`Self::own_spans`]) at the same units, its actual work alike, so it
    /// keeps its dates (every task type); the format moves to the same unit in working time. The edit's refresh then
    /// derives cost, remaining work and % work complete.
    ///
    /// A window with no working time (an FS follower over a night or a
    /// weekend), or no start, has nothing to scale by: a `Progress` write,
    /// or any write on a task with progress (an actual start, duration or
    /// work), is refused, changing nothing. A write that replaces the
    /// duration takes the task to zero working minutes, its work by the
    /// rule of a duration going to zero ([`rescale_work`]: the units stay,
    /// a Fixed Work task keeps its work), so the write's own duration then
    /// sets the work from the units, or the units from a Fixed Work task's
    /// work. A working task is left alone.
    pub(super) fn elapsed_to_working(&mut self, i: usize, write: Write) -> Result<(), String> {
        let task = &self.proj.tasks[i];
        if !task.elapsed_duration() {
            return Ok(());
        }
        let elapsed = task.duration_min.max(0);
        let replaces = write != Write::Progress && !has_progress(&self.proj, i);
        let start = task
            .actual_start
            .or_else(|| self.sched.get(task.uid).map(|r| r.early_start));
        let working = match start {
            Some(start) => {
                let calendar = crate::assign::task_calendar(&self.proj, task);
                crate::schedule::working_minutes_on(&calendar, start, start.add_minutes(elapsed))
            }
            None if replaces => 0,
            None => return Err("The task has no start".into()),
        };
        if elapsed > 0 && working <= 0 && !replaces {
            return Err(ELAPSED_NO_WORKING_TIME.into());
        }
        let own = self.own_spans(i, working);
        self.edit_row(i, |proj, _| to_working_time(proj, i, working.max(0), &own))
    }

    /// The working work of each flat work assignment of task `i` on the
    /// 24x7 clock, by its own span (#484 r10): the working minutes, on the
    /// task's calendar, of the span `assignment_span` dates it over (its
    /// working Delay, then work ÷ units on the clock), at its units, by its
    /// index in the plan's assignments. Read before the conversion, so the
    /// assignment keeps the working time it covered, whatever the rest of
    /// the window holds (a weekend, a shorter or delayed assignment). None
    /// on a task off the clock (a tracked one is scheduled in working time
    /// already) or a window with no working time.
    fn own_spans(&self, i: usize, working: i64) -> Vec<(usize, i64)> {
        let task = &self.proj.tasks[i];
        if working <= 0 || !crate::schedule::on_the_clock(task) {
            return Vec::new();
        }
        let calendar = crate::assign::task_calendar(&self.proj, task);
        self.proj
            .assignments
            .iter()
            .enumerate()
            .filter(|(_, a)| a.task_uid == task.uid && a.actual_start.is_none())
            .filter(|(_, a)| a.actual_finish.is_none())
            .filter(|(_, a)| crate::assign::is_flat_work(&self.proj, a))
            .filter_map(|(k, a)| {
                let (start, finish) = crate::assign::assignment_span(&self.proj, &self.sched, a)?;
                let minutes = crate::schedule::working_minutes_on(&calendar, start, finish);
                Some((k, (minutes as f64 * a.units).round() as i64))
            })
            .collect()
    }

    /// Run `edit` on elapsed task `i` re-expressed in working time first
    /// ([`Self::elapsed_to_working`]), as one undo step. An `edit` that
    /// fails, or (except a typed duration, [`Write::Duration`]) makes no
    /// change of its own, leaves the editor as it was, also inside an outer
    /// batch, which the conversion then never reaches. The callers re-enter
    /// themselves in `edit`; the task is no longer elapsed there.
    pub(super) fn in_working_time<T>(
        &mut self,
        i: usize,
        write: Write,
        edit: impl FnOnce(&mut Editor) -> Result<T, String>,
    ) -> Result<T, String> {
        self.batch(|ed| {
            let point = ed.savepoint();
            let mut converted = None;
            let result = ed.elapsed_to_working(i, write).and_then(|()| {
                converted = Some(ed.pushes);
                edit(ed)
            });
            let unchanged = converted == Some(ed.pushes) && write != Write::Duration;
            if result.is_err() || unchanged {
                ed.rollback_to(point);
            }
            result
        })
    }
}

/// Whether task `i` has progress the conversion has to re-express: an
/// actual start, finish or duration, or actual work on it.
fn has_progress(proj: &Project, i: usize) -> bool {
    let t = &proj.tasks[i];
    let some = |m: Option<i64>| m.is_some_and(|m| m > 0);
    t.actual_start.is_some()
        || t.actual_finish.is_some()
        || some(t.actual_duration_min)
        || proj
            .assignments
            .iter()
            .filter(|a| a.task_uid == t.uid)
            .any(|a| some(a.actual_work_min) || some(a.actual_overtime_work_min))
}

/// [`Editor::elapsed_to_working`]'s rewrite of task `i` over `working`
/// minutes; `own` holds the flat assignments' work by their own spans
/// ([`Editor::own_spans`]).
fn to_working_time(proj: &mut Project, i: usize, working: i64, own: &[(usize, i64)]) {
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
    if working == 0 {
        // Nothing to re-express the work by: a duration going to zero.
        rescale_work(proj, i, elapsed, 0);
        return;
    }
    // A flat assignment of a task on the clock takes the working time its
    // own span covered, and its actual work the same factor, so its % work
    // complete stays; every other work figure (contoured work, a tracked
    // task's) goes by the window's factor (`rescale_work` would recompute
    // flat work as the whole duration at its units).
    let Project {
        assignments,
        resources,
        ..
    } = proj;
    for (k, a) in assignments.iter_mut().enumerate() {
        let kind = resources.iter().find(|r| r.uid == a.resource_uid);
        if a.task_uid != uid || kind.is_some_and(|r| r.kind != ResourceType::Work) {
            continue;
        }
        let old = a.work_min;
        let work = own
            .iter()
            .find(|&&(at, _)| at == k)
            .map_or_else(|| scale(old), |&(_, work)| work);
        let by_own = move |m: i64| match own.iter().any(|&(at, _)| at == k) {
            true if old > 0 => {
                i64::try_from(i128::from(m) * i128::from(work) / i128::from(old)).unwrap_or(m)
            }
            _ => scale(m),
        };
        a.set_work(work);
        a.actual_work_min = a.actual_work_min.map(by_own);
        a.actual_overtime_work_min = a.actual_overtime_work_min.map(by_own);
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
