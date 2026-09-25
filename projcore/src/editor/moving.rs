//! Task › Tasks › Move: moving a task in time by working days, as Project's
//! Move Task Forward/Backward does.
use super::*;
use crate::model::WorkCalendar;
use crate::schedule::HORIZON_DAYS;

impl Editor {
    /// Move a task's start by `text` (see [`parse_move`]) working days on its
    /// calendar, keeping the time of day. An auto task gets a
    /// Start-No-Earlier-Than constraint there and a manual task's pinned
    /// start moves (see [`Self::set_start_at`]); one undo step. The base is
    /// the unleveled start: leveling is recomputed on every edit, so a base
    /// that included its delay would drift on each move. Returns the new
    /// start.
    pub fn move_task(&mut self, uid: i32, text: &str) -> Result<DateTime, String> {
        let task = &self.proj.tasks[self.index(uid)?];
        if task.is_null {
            return Err("The row has no task to move".into());
        }
        if task.summary {
            return Err("Move a subtask, not a summary".into());
        }
        let days = parse_move(text, &self.proj)?;
        let base = task
            .pinned_dates()
            .map(|(start, _)| start)
            .or_else(|| self.sched.get(uid).map(|r| r.early_start))
            .ok_or("The row has no task to move")?;
        let calendar = cells::task_calendar(&self.proj, task);
        let start = shift_working_days(&calendar, base, days)?;
        self.validate_pinned_day(start)?;
        self.set_start_at(uid, start)?;
        Ok(start)
    }
}

/// Parse a Move amount: a signed whole number of days (`d`) or weeks (`w`),
/// as working days. A week is the plan's working days per week, from its
/// hours per week and per day (MSPDI has no days-per-week field).
pub fn parse_move(text: &str, proj: &Project) -> Result<i64, String> {
    let t = text.trim().to_ascii_lowercase();
    let err = || format!("Couldn't read '{}' (try 1d, 1w, 4w, -1d)", text.trim());
    let (num, per) = if let Some(n) = t.strip_suffix('d') {
        (n, 1)
    } else if let Some(n) = t.strip_suffix('w') {
        (n, week_days(proj))
    } else {
        return Err(err());
    };
    let n: i64 = num.trim().parse().map_err(|_| err())?;
    if n == 0 {
        return Err("Move by at least one day".into());
    }
    n.checked_mul(per)
        .filter(|days| days.unsigned_abs() <= HORIZON_DAYS.unsigned_abs())
        .ok_or_else(|| "Move is outside the scheduling range".into())
}

/// Working days in a week: 5 on a default plan (40h / 8h).
fn week_days(proj: &Project) -> i64 {
    let days = (proj.hours_per_week / proj.hours_per_day).round();
    if days.is_finite() && (1.0..=7.0).contains(&days) {
        days as i64
    } else {
        5
    }
}

/// `from` moved by `days` working days of `calendar` (backward when
/// negative), at the same time of day; non-working days are skipped. Refused
/// when the walk leaves the scheduling horizon: as out of range when the
/// calendar works but not enough, else as a calendar without working days.
fn shift_working_days(
    calendar: &WorkCalendar,
    from: DateTime,
    days: i64,
) -> Result<DateTime, String> {
    let step = days.signum();
    let mut left = days.unsigned_abs();
    let mut offset = 0i64;
    let mut slots = Vec::new();
    while left > 0 {
        if offset.unsigned_abs() >= HORIZON_DAYS.unsigned_abs() {
            return Err(if left < days.unsigned_abs() {
                "Move is outside the scheduling range"
            } else {
                "No working day in reach on the task's calendar"
            }
            .into());
        }
        offset += step;
        calendar.day_into(from.day_number() + offset, &mut slots);
        if !slots.is_empty() {
            left -= 1;
        }
    }
    Ok(from.add_days(offset))
}

#[cfg(test)]
mod tests;
