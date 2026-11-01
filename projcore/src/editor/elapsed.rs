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

/// The refusal of a write on an elapsed task (whose window does hold working
/// time) when one assignment's own span holds none and it has actual work,
/// which the conversion would otherwise lose ([`Editor::elapsed_to_working`]).
pub(super) const ELAPSED_ASSIGNMENT_NO_WORKING_TIME: &str = "An assignment's actual work on this elapsed task lies outside working time; durations and progress are edited in working time";

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
    /// assignment of an untracked task instead by its own span's factor
    /// ([`Self::own_spans`]), so it keeps its dates; each assignment's work,
    /// scheduled overtime and actual work by one factor, the units kept
    /// (every task type); the format moves to the same unit in working time. The edit's refresh then
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
    /// work. Over a window that does hold working time, one assignment whose
    /// own span holds none keeps its work, unless it has actual work there:
    /// the write is then refused (`ELAPSED_ASSIGNMENT_NO_WORKING_TIME`). A
    /// working task is left alone.
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
        let own = self.own_spans(i, working)?;
        self.edit_row(i, |proj, _| to_working_time(proj, i, working.max(0), &own))
    }

    /// Each flat work assignment's own factor on task `i` on the 24x7 clock
    /// (#484 r10, r11): the working minutes, on the task's calendar, of the
    /// span `assignment_span` dates it over, against that span's wall-clock
    /// minutes, by its index in the plan's assignments. The span is its
    /// actual start, else its working Delay from the task start, to its
    /// actual finish, else work ÷ units later on the clock. Read before the
    /// conversion, so the assignment keeps the working time it covered,
    /// whatever the rest of the window holds (a weekend, a shorter or
    /// delayed assignment); with an actual finish its work is its recorded
    /// span's density, which its dates, read as they are, keep. None on a
    /// task off the clock (a tracked one is scheduled in working time
    /// already), a window with no working time, or a span of no length. A
    /// span with no working time (an evening after an FS link) has nothing
    /// to re-express by: the assignment keeps its work (a factor of 1), as
    /// a progress write keeps a working task's, and its work then runs from
    /// the next working minute; with actual work, which would have to sit
    /// outside working time, the conversion is refused
    /// (`ELAPSED_ASSIGNMENT_NO_WORKING_TIME`).
    fn own_spans(&self, i: usize, working: i64) -> Result<Vec<(usize, Factor)>, String> {
        let task = &self.proj.tasks[i];
        if working <= 0 || !crate::schedule::on_the_clock(task) {
            return Ok(Vec::new());
        }
        let calendar = crate::assign::task_calendar(&self.proj, task);
        self.proj
            .assignments
            .iter()
            .enumerate()
            .filter(|(_, a)| a.task_uid == task.uid)
            .filter(|(_, a)| crate::assign::is_flat_work(&self.proj, a))
            .filter_map(|(k, a)| {
                let (start, finish) = crate::assign::assignment_span(&self.proj, &self.sched, a)?;
                let wall = finish.minutes() - start.minutes();
                let working = crate::schedule::working_minutes_on(&calendar, start, finish);
                let actual = |m: Option<i64>| m.is_some_and(|m| m > 0);
                match (wall > 0, working > 0) {
                    (true, true) => Some(Ok((k, Factor { working, wall }))),
                    (true, false)
                        if actual(a.actual_work_min) || actual(a.actual_overtime_work_min) =>
                    {
                        Some(Err(ELAPSED_ASSIGNMENT_NO_WORKING_TIME.to_string()))
                    }
                    (true, false) => Some(Ok((
                        k,
                        Factor {
                            working: 1,
                            wall: 1,
                        },
                    ))),
                    _ => None,
                }
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

/// Working minutes over the wall-clock minutes of a span: what a minute of
/// it becomes in working time.
#[derive(Clone, Copy, Debug)]
pub(super) struct Factor {
    working: i64,
    wall: i64,
}

impl Factor {
    /// `m` re-expressed (`m` itself over a span of no length).
    fn of(self, m: i64) -> i64 {
        if self.wall <= 0 {
            return m;
        }
        i64::try_from(i128::from(m) * i128::from(self.working) / i128::from(self.wall)).unwrap_or(m)
    }
}

/// [`Editor::elapsed_to_working`]'s rewrite of task `i` over `working`
/// minutes; `own` holds the flat assignments' own factors
/// ([`Editor::own_spans`]).
fn to_working_time(proj: &mut Project, i: usize, working: i64, own: &[(usize, Factor)]) {
    let t = &mut proj.tasks[i];
    let elapsed = t.duration_min;
    let window = Factor {
        working,
        wall: elapsed,
    };
    let scale = |m: i64| window.of(m);
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
    // Each work assignment's work figures by one factor: a flat assignment
    // of a task on the clock its own span's, so it keeps the working time it
    // covered and its dates, every other (contoured work, a tracked task's)
    // the window's (`rescale_work` would recompute flat work as the whole
    // duration at its units). Its scheduled overtime is put back after
    // `set_work`, within the work, for the refresh to price; what `set_work`
    // clears otherwise (regular work, costs, remaining overtime) the refresh
    // derives again, and the planned spread, which described the old work,
    // is dropped. Dates, delays, units,
    // baselines, earned value, budgets and Project's protected actuals (a
    // snapshot no edit maintains) stay as they are.
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
        let factor = own
            .iter()
            .find(|&&(at, _)| at == k)
            .map_or(window, |&(_, f)| f);
        // Only what the factor changes is rewritten: `set_work` clears what
        // the refresh derives, and an assignment whose inputs did not move
        // (a factor of 1: hours from a morning, a 24-hour calendar) is not
        // refreshed.
        let work = factor.of(a.work_min);
        let overtime = a.overtime_work_min.map(|o| factor.of(o).min(work));
        if work != a.work_min {
            a.set_work(work);
        }
        if overtime != a.overtime_work_min {
            a.overtime_work_min = overtime;
        }
        a.actual_work_min = a.actual_work_min.map(|m| factor.of(m));
        a.actual_overtime_work_min = a.actual_overtime_work_min.map(|m| factor.of(m));
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
