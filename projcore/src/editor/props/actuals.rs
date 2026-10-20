//! The Update Tasks fields (#1568): Actual Start, Actual Finish, Actual
//! Duration and Remaining Duration. Each keeps the task's `ActualStart`,
//! `ActualFinish`, `ActualDuration`, `RemainingDuration`, `PercentComplete`
//! and (for the remaining duration) `Duration` consistent with the relation
//! `Task::tracked` schedules from, `duration = actual + remaining`, in one
//! undo step. How each field moves the others is not measured against
//! Project 2024 beyond `corpus/mspdi/22-progress.xml`; the rules not pinned
//! by it are noted on the setters.
use super::*;

/// The progress fields a setter settles together.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Progress {
    start: Option<DateTime>,
    finish: Option<DateTime>,
    actual: Option<i64>,
    remaining: Option<i64>,
    duration: i64,
    percent: u8,
}

impl Progress {
    fn of(task: &Task) -> Progress {
        Progress {
            start: task.actual_start,
            finish: task.actual_finish,
            actual: task.actual_duration_min,
            remaining: task.remaining_duration_min,
            duration: task.duration_min,
            percent: task.percent_complete.unwrap_or(0),
        }
    }

    /// The working minutes done: the recorded actual duration, else the
    /// share the % Complete implies.
    fn done(&self) -> i64 {
        self.actual
            .unwrap_or_else(|| share(self.duration, i64::from(self.percent)))
    }
}

/// `part` of `whole` as a whole percent, rounded.
fn percent_of(part: i64, whole: i64) -> u8 {
    if whole <= 0 {
        return 0;
    }
    u8::try_from(((part * 100 + whole / 2) / whole).clamp(0, 100)).unwrap_or(100)
}

/// `percent` of `whole`, rounded.
fn share(whole: i64, percent: i64) -> i64 {
    (whole * percent + 50) / 100
}

impl Editor {
    /// The leaf task at `uid` whose progress can be set: summaries roll up,
    /// milestones have no duration to progress through.
    fn progress_row(&self, uid: i32) -> Result<usize, String> {
        let i = self.property_row(uid)?;
        let task = &self.proj.tasks[i];
        if task.summary {
            return Err("A summary's progress is computed from its subtasks".into());
        }
        if task.is_milestone() {
            return Err("A milestone has no duration to record progress against".into());
        }
        Ok(i)
    }

    /// Where the task is scheduled to start: the actual start a task with
    /// recorded work takes when only its duration or finish was given.
    fn scheduled_start(&self, i: usize) -> Result<DateTime, String> {
        let task = &self.proj.tasks[i];
        self.sched
            .get(task.uid)
            .map(|r| r.early_start)
            .or(task.stored_start)
            .ok_or_else(|| "The task has no start".into())
    }

    /// Write `new` over task `i`; `Ok(false)` when it changes nothing.
    /// `snapshotted` says the edit's undo step is already open. A changed
    /// actual drops `Stop`/`Resume`, which would otherwise end the completed
    /// part where the old actual did.
    fn store_progress(&mut self, i: usize, new: Progress, snapshotted: bool) -> bool {
        if Progress::of(&self.proj.tasks[i]) == new && !snapshotted {
            return false;
        }
        if !snapshotted {
            self.snapshot();
        }
        let task = &mut self.proj.tasks[i];
        task.actual_start = new.start;
        task.actual_finish = new.finish;
        task.actual_duration_min = new.actual;
        task.remaining_duration_min = new.remaining;
        task.duration_min = new.duration;
        if new.percent != task.percent_complete.unwrap_or(0) {
            task.percent_complete = Some(new.percent);
        }
        task.stop = None;
        task.resume = None;
        task.resume_valid = None;
        self.changed();
        true
    }

    /// Set the task's Actual Start (`None` clears it). Clearing it also
    /// clears the actual finish and duration and sets % Complete to 0: a task
    /// that never started has no actuals. A start after the actual finish is
    /// refused.
    pub fn set_actual_start(&mut self, uid: i32, start: Option<DateTime>) -> Result<bool, String> {
        let i = self.progress_row(uid)?;
        let mut p = Progress::of(&self.proj.tasks[i]);
        match start {
            Some(start) => {
                if p.finish.is_some_and(|finish| finish < start) {
                    return Err("Actual Start is after the actual finish".into());
                }
                p.start = Some(start);
            }
            None => {
                p.start = None;
                p.finish = None;
                p.actual = None;
                p.remaining = None;
                p.percent = 0;
            }
        }
        Ok(self.store_progress(i, p, false))
    }

    /// Set the task's Actual Finish (`None` clears it). A finish completes the
    /// task: 100 %, actual duration = duration, no remaining duration, and an
    /// actual start from the task's scheduled start when it has none. A
    /// finish before the actual start is refused. Clearing it keeps the actual
    /// start and duration: % Complete is their share (99 when no actual
    /// duration is recorded) and the remaining duration what is left.
    pub fn set_actual_finish(
        &mut self,
        uid: i32,
        finish: Option<DateTime>,
    ) -> Result<bool, String> {
        let i = self.progress_row(uid)?;
        let mut p = Progress::of(&self.proj.tasks[i]);
        match finish {
            Some(finish) => {
                let start = match p.start {
                    Some(start) => start,
                    None => self.scheduled_start(i)?,
                };
                if finish < start {
                    return Err("Actual Finish is before the actual start".into());
                }
                p.start = Some(start);
                p.finish = Some(finish);
                p.actual = Some(p.duration);
                p.remaining = None;
                p.percent = 100;
            }
            None => {
                if p.finish.is_none() {
                    return Ok(false);
                }
                p.finish = None;
                match p.actual {
                    Some(actual) => {
                        p.percent = percent_of(actual, p.duration);
                        p.remaining = Some((p.duration - actual).max(0));
                    }
                    None => {
                        p.percent = 99;
                        p.remaining = Some(p.duration - share(p.duration, 99));
                    }
                }
            }
        }
        Ok(self.store_progress(i, p, false))
    }

    /// Set the task's Actual Duration in working minutes, 0 to its duration.
    /// % Complete becomes its share and the remaining duration what is left;
    /// the duration stays. A task with no actual start takes its scheduled
    /// start (Project marks a task started once work is recorded). Reaching
    /// the whole duration gives 100 % and no remaining duration but invents no
    /// actual finish: only [`Self::set_actual_finish`] types one. A task that
    /// has an actual finish keeps it, so only its whole duration is accepted.
    pub fn set_actual_duration(&mut self, uid: i32, minutes: i64) -> Result<bool, String> {
        let i = self.progress_row(uid)?;
        let mut p = Progress::of(&self.proj.tasks[i]);
        if minutes < 0 {
            return Err("Actual duration cannot be negative".into());
        }
        if minutes > p.duration {
            return Err(
                "Actual duration exceeds the duration; set remaining_duration or duration first"
                    .into(),
            );
        }
        if p.finish.is_some() && minutes != p.duration {
            return Err("The task has an actual finish; clear it first".into());
        }
        if p.start.is_none() {
            p.start = Some(self.scheduled_start(i)?);
        }
        p.actual = Some(minutes);
        p.remaining = Some(p.duration - minutes);
        p.percent = percent_of(minutes, p.duration);
        Ok(self.store_progress(i, p, false))
    }

    /// Set the task's Remaining Duration in working minutes: the duration
    /// becomes the work done plus it (the actual duration, else the share
    /// % Complete implies) and % Complete the done share of that. Zero
    /// completes the task as an actual duration of the whole does; it is
    /// refused when nothing is done (the task would have no duration). A task
    /// that has an actual finish takes only 0. It does not start a task.
    pub fn set_remaining_duration(&mut self, uid: i32, minutes: i64) -> Result<bool, String> {
        let i = self.progress_row(uid)?;
        let mut p = Progress::of(&self.proj.tasks[i]);
        if minutes < 0 {
            return Err("Remaining duration cannot be negative".into());
        }
        if p.finish.is_some() && minutes != 0 {
            return Err("The task has an actual finish; clear it first".into());
        }
        let done = p.done();
        let duration = done + minutes;
        if duration == 0 {
            return Err(
                "Remaining duration 0 with no work done would leave a zero duration".into(),
            );
        }
        let old_duration = p.duration;
        p.duration = duration;
        p.remaining = Some(minutes);
        if p.actual.is_some() || p.start.is_some() {
            p.actual = Some(done);
        }
        p.percent = percent_of(done, duration);
        if p.finish.is_some() {
            p.actual = Some(duration);
            p.remaining = None;
        }
        if duration == old_duration {
            return Ok(self.store_progress(i, p, false));
        }
        // The duration goes through the same edit as a typed one (work
        // rescales, an estimate is committed, a pinned task restamps); its
        // undo step covers the progress fields written after it.
        let patch = TaskPatch {
            duration_min: Some(duration),
            ..TaskPatch::default()
        };
        self.update_task(uid, patch)?;
        let i = self.index(uid)?;
        self.store_progress(i, p, true);
        Ok(true)
    }
}
