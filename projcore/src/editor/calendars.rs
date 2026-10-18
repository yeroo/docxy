//! Calendars one at a time (#1605): what an agent's `cal.add`, `cal.set`,
//! `cal.exception.*`, `cal.workweek.*` and `cal.del` do, and what Project's
//! Change Working Time dialog edits. Each is one undo step; every argument
//! is checked before anything changes, and an edit that leaves a calendar a
//! task schedules on without working time is refused, as the scheduler
//! would refuse to run it ([`crate::schedule::calendar_error`]).
//!
//! Calendar names address calendars elsewhere (`proj.set calendar`,
//! `task.set calendar`, `resource.set base_calendar`), so a name is unique
//! ignoring ASCII case. A base calendar states all seven days; a derived
//! one states only the days it overrides and inherits the rest from its
//! base, so a derived calendar's day may be cleared (inherit again). Working
//! times are minutes from midnight, in order, not overlapping.
use super::*;
use crate::model::{Calendar, CalendarException, DayWorking, WorkWeek, WorkingTime};

/// How a weekday is stated: `Some` states it (an empty `times` is a
/// non-working day), `None` inherits the base calendar's day (a derived
/// calendar only; a base calendar's unstated day is non-working).
pub type DayPatch = Option<DayWorking>;

/// Where a day's working time comes from, for Change Working Time's
/// "Based on" (#1605).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaySource {
    /// An exception of calendar `calendar` (its name when it has one).
    Exception {
        calendar: String,
        name: Option<String>,
    },
    /// A work week of calendar `calendar`.
    WorkWeek {
        calendar: String,
        name: Option<String>,
    },
    /// The default work week of calendar `calendar`.
    Default { calendar: String },
}

/// Project's weekday names, Sunday first as [`DateTime::weekday`] numbers
/// them.
pub const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// A weekday from its name (`Mon`, `monday`), 0 = Sunday.
pub fn weekday_index(name: &str) -> Option<usize> {
    let want = name.trim().to_ascii_lowercase();
    if want.len() < 3 {
        return None;
    }
    WEEKDAYS
        .iter()
        .position(|d| d.to_ascii_lowercase().starts_with(&want))
}

/// Check a day's working times: each from before to, within the day, in
/// order and not overlapping.
pub fn checked_times(times: &[WorkingTime]) -> Result<(), String> {
    let mut last = 0;
    for (n, t) in times.iter().enumerate() {
        if t.from >= t.to {
            return Err(format!(
                "working time {} ends ({}) before it starts ({})",
                n + 1,
                clock(t.to),
                clock(t.from)
            ));
        }
        if t.to > 24 * 60 {
            return Err(format!("working time {} ends past midnight", n + 1));
        }
        if t.from < last {
            return Err(format!(
                "working time {} ({}) overlaps the one before it",
                n + 1,
                clock(t.from)
            ));
        }
        last = t.to;
    }
    Ok(())
}

/// `HH:MM` from minutes since midnight.
pub fn clock(min: u32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

/// Minutes since midnight from `HH:MM` (also `H:MM`), up to `24:00`.
pub fn parse_clock(text: &str) -> Option<u32> {
    let (h, m) = text.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    (h <= 24 && m < 60 && (h < 24 || m == 0)).then_some(h * 60 + m)
}

/// The name a calendar may take: trimmed, not empty, no other calendar's
/// (ignoring ASCII case).
fn checked_calendar_name(proj: &Project, name: &str, own: Option<i32>) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a calendar needs a name".into());
    }
    if let Some(other) = proj
        .calendars
        .iter()
        .find(|c| Some(c.uid) != own && c.name.eq_ignore_ascii_case(name))
    {
        return Err(format!(
            "a calendar named '{}' exists (uid {})",
            other.name, other.uid
        ));
    }
    Ok(name.to_string())
}

/// Whether `base` is reachable from `uid` through base links: rebasing
/// `uid` on it would make a cycle.
fn derives_from(proj: &Project, base: i32, uid: i32) -> bool {
    let mut at = Some(base);
    let mut seen = std::collections::HashSet::new();
    while let Some(c) = at {
        if c == uid {
            return true;
        }
        if !seen.insert(c) {
            return false;
        }
        at = proj.calendar(c).and_then(|cal| cal.base_calendar_uid);
    }
    false
}

/// Exceptions and work weeks keep Project's instants: the range's first
/// day at 00:00 and its last day at 23:59.
fn range_instants(from: DateTime, to: DateTime) -> Result<(DateTime, DateTime), String> {
    let (a, b) = (from.start_of_day(), to.start_of_day());
    if b < a {
        return Err("the range ends before it starts".into());
    }
    Ok((a, DateTime::from_minutes(b.minutes() + 23 * 60 + 59)))
}

impl Editor {
    /// The calendar `key` names: a uid, else a name ignoring ASCII case.
    pub fn calendar_by(&self, key: &str) -> Result<i32, String> {
        let key = key.trim();
        if let Ok(uid) = key.parse::<i32>()
            && self.proj.calendar(uid).is_some()
        {
            return Ok(uid);
        }
        self.proj
            .calendars
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(key))
            .map(|c| c.uid)
            .ok_or_else(|| format!("no calendar named '{key}'"))
    }

    /// Apply `edit` to a copy of the plan as one undo step, unless it changes
    /// nothing (`Ok(false)`) or leaves a scheduled calendar without working
    /// time (the error).
    fn edit_calendars(
        &mut self,
        edit: impl FnOnce(&mut Project) -> Result<(), String>,
    ) -> Result<bool, String> {
        let mut next = self.proj.clone();
        edit(&mut next)?;
        if next == self.proj {
            return Ok(false);
        }
        if let Some(error) = crate::schedule::calendar_error(&next) {
            return Err(error);
        }
        let prev = std::mem::replace(&mut self.proj, next);
        self.push_undo(prev);
        self.changed();
        Ok(true)
    }

    /// Add a calendar named `name`: derived from `base` when given, else a
    /// base calendar with Project's standard week, or a copy of `copy`'s
    /// week, exceptions and work weeks. Returns its uid.
    pub fn add_calendar(
        &mut self,
        name: &str,
        base: Option<i32>,
        copy: Option<i32>,
    ) -> Result<i32, String> {
        let name = checked_calendar_name(&self.proj, name, None)?;
        if let Some(b) = base
            && self.proj.calendar(b).is_none()
        {
            return Err(format!("no calendar with uid {b} to derive from"));
        }
        let copied = match copy {
            Some(c) => Some(
                self.proj
                    .calendar(c)
                    .cloned()
                    .ok_or_else(|| format!("no calendar with uid {c} to copy"))?,
            ),
            None => None,
        };
        let uid = self
            .proj
            .calendars
            .iter()
            .map(|c| c.uid)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("No calendar UIDs available")?;
        let cal = match (base, copied) {
            (Some(b), _) => Calendar {
                uid,
                name,
                base_calendar_uid: Some(b),
                is_baseline_calendar: false,
                week: [None, None, None, None, None, None, None],
                exceptions: Vec::new(),
                work_weeks: Vec::new(),
            },
            (None, Some(from)) => Calendar {
                uid,
                name,
                is_baseline_calendar: false,
                ..from
            },
            (None, None) => Calendar::base(uid, &name, Calendar::standard_week()),
        };
        self.edit_calendars(|proj| {
            proj.calendars.push(cal);
            Ok(())
        })?;
        Ok(uid)
    }

    /// Rename calendar `uid`, rebase a derived one and set the days of its
    /// default week (`week`: weekday index, pattern); `Ok(false)` when
    /// nothing differs.
    pub fn set_calendar(
        &mut self,
        uid: i32,
        name: Option<&str>,
        base: Option<i32>,
        week: &[(usize, DayPatch)],
    ) -> Result<bool, String> {
        let cal = self
            .proj
            .calendar(uid)
            .ok_or_else(|| format!("no calendar with uid {uid}"))?;
        let name = match name {
            Some(n) => Some(checked_calendar_name(&self.proj, n, Some(uid))?),
            None => None,
        };
        if let Some(b) = base {
            if cal.base_calendar_uid.is_none() {
                return Err(format!(
                    "'{}' is a base calendar; only a derived calendar has a base",
                    cal.name
                ));
            }
            if self.proj.calendar(b).is_none() {
                return Err(format!("no calendar with uid {b} to derive from"));
            }
            if b == uid || derives_from(&self.proj, b, uid) {
                return Err(format!("'{}' cannot derive from itself", cal.name));
            }
        }
        for (day, pattern) in week {
            if *day > 6 {
                return Err(format!("no weekday {day}"));
            }
            match pattern {
                Some(d) => checked_times(&d.times)?,
                None if cal.base_calendar_uid.is_none() => {
                    return Err(format!(
                        "'{}' is a base calendar: its {} is working or not, it inherits nothing",
                        cal.name, WEEKDAYS[*day]
                    ));
                }
                None => {}
            }
        }
        self.edit_calendars(|proj| {
            let cal = proj
                .calendars
                .iter_mut()
                .find(|c| c.uid == uid)
                .ok_or("the calendar is gone")?;
            if let Some(n) = name {
                cal.name = n;
            }
            if let Some(b) = base {
                cal.base_calendar_uid = Some(b);
            }
            for (day, pattern) in week {
                cal.week[*day] = pattern.clone();
            }
            Ok(())
        })
    }

    /// Add (no `index`) or change an exception of calendar `uid`: a
    /// date-range exception as Project writes one, `from` to `to` (dates),
    /// non-working when `day.times` is empty. Returns its index.
    pub fn set_calendar_exception(
        &mut self,
        uid: i32,
        index: Option<usize>,
        name: Option<&str>,
        from: DateTime,
        to: DateTime,
        day: DayWorking,
    ) -> Result<usize, String> {
        let cal = self
            .proj
            .calendar(uid)
            .ok_or_else(|| format!("no calendar with uid {uid}"))?;
        checked_times(&day.times)?;
        let (from, to) = range_instants(from, to)?;
        let at = match index {
            Some(i) if i >= cal.exceptions.len() => {
                return Err(format!(
                    "'{}' has {} exceptions; no exception {i}",
                    cal.name,
                    cal.exceptions.len()
                ));
            }
            Some(i) => i,
            None => cal.exceptions.len(),
        };
        let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
        self.edit_calendars(|proj| {
            let cal = proj
                .calendars
                .iter_mut()
                .find(|c| c.uid == uid)
                .ok_or("the calendar is gone")?;
            let mut e = CalendarException::date_range(from, to, day);
            e.name = name;
            if at == cal.exceptions.len() {
                cal.exceptions.push(e);
            } else {
                // A recurring exception edited becomes a date range, as the
                // dialog's Details would write it.
                cal.exceptions[at] = e;
            }
            Ok(())
        })?;
        Ok(at)
    }

    /// Remove exception `index` of calendar `uid`.
    pub fn delete_calendar_exception(&mut self, uid: i32, index: usize) -> Result<(), String> {
        let cal = self
            .proj
            .calendar(uid)
            .ok_or_else(|| format!("no calendar with uid {uid}"))?;
        if index >= cal.exceptions.len() {
            return Err(format!(
                "'{}' has {} exceptions; no exception {index}",
                cal.name,
                cal.exceptions.len()
            ));
        }
        self.edit_calendars(|proj| {
            let cal = proj
                .calendars
                .iter_mut()
                .find(|c| c.uid == uid)
                .ok_or("the calendar is gone")?;
            cal.exceptions.remove(index);
            Ok(())
        })?;
        Ok(())
    }

    /// Add (no `index`) or change a work week of calendar `uid`: `from` to
    /// `to` (dates) with the days it states. Returns its index.
    pub fn set_calendar_work_week(
        &mut self,
        uid: i32,
        index: Option<usize>,
        name: Option<&str>,
        from: DateTime,
        to: DateTime,
        week: [DayPatch; 7],
    ) -> Result<usize, String> {
        let cal = self
            .proj
            .calendar(uid)
            .ok_or_else(|| format!("no calendar with uid {uid}"))?;
        for day in week.iter().flatten() {
            checked_times(&day.times)?;
        }
        let (from, to) = range_instants(from, to)?;
        let at = match index {
            Some(i) if i >= cal.work_weeks.len() => {
                return Err(format!(
                    "'{}' has {} work weeks; no work week {i}",
                    cal.name,
                    cal.work_weeks.len()
                ));
            }
            Some(i) => i,
            None => cal.work_weeks.len(),
        };
        let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
        self.edit_calendars(|proj| {
            let cal = proj
                .calendars
                .iter_mut()
                .find(|c| c.uid == uid)
                .ok_or("the calendar is gone")?;
            let w = WorkWeek {
                name,
                from: Some(from),
                to: Some(to),
                week,
            };
            if at == cal.work_weeks.len() {
                cal.work_weeks.push(w);
            } else {
                cal.work_weeks[at] = w;
            }
            Ok(())
        })?;
        Ok(at)
    }

    /// Remove work week `index` of calendar `uid`.
    pub fn delete_calendar_work_week(&mut self, uid: i32, index: usize) -> Result<(), String> {
        let cal = self
            .proj
            .calendar(uid)
            .ok_or_else(|| format!("no calendar with uid {uid}"))?;
        if index >= cal.work_weeks.len() {
            return Err(format!(
                "'{}' has {} work weeks; no work week {index}",
                cal.name,
                cal.work_weeks.len()
            ));
        }
        self.edit_calendars(|proj| {
            let cal = proj
                .calendars
                .iter_mut()
                .find(|c| c.uid == uid)
                .ok_or("the calendar is gone")?;
            cal.work_weeks.remove(index);
            Ok(())
        })?;
        Ok(())
    }

    /// Delete calendar `uid`: refused for the plan's default calendar, one a
    /// task or resource uses, or one another calendar derives from.
    pub fn delete_calendar(&mut self, uid: i32) -> Result<(), String> {
        let proj = &self.proj;
        let cal = proj
            .calendar(uid)
            .ok_or_else(|| format!("no calendar with uid {uid}"))?;
        if proj.default_calendar_uid == uid {
            return Err(format!(
                "'{}' is the project calendar; pick another first (Project Information)",
                cal.name
            ));
        }
        if let Some(t) = proj.tasks.iter().find(|t| t.calendar_uid == Some(uid)) {
            return Err(format!("'{}' is task '{}' calendar", cal.name, t.name));
        }
        if let Some(r) = proj.resources.iter().find(|r| r.calendar_uid == Some(uid)) {
            return Err(format!("'{}' is resource '{}' calendar", cal.name, r.name));
        }
        if let Some(d) = proj
            .calendars
            .iter()
            .find(|c| c.base_calendar_uid == Some(uid))
        {
            return Err(format!("'{}' derives from '{}'", d.name, cal.name));
        }
        self.edit_calendars(|proj| {
            proj.calendars.retain(|c| c.uid != uid);
            Ok(())
        })?;
        Ok(())
    }

    /// Calendar `uid`'s working times on `date`, and where they come from:
    /// Change Working Time's "Working times for <date>" and "Based on".
    pub fn calendar_day(
        &self,
        uid: i32,
        date: DateTime,
    ) -> Result<(Vec<WorkingTime>, DaySource), String> {
        let proj = &self.proj;
        let cal = proj
            .calendar(uid)
            .ok_or_else(|| format!("no calendar with uid {uid}"))?;
        let day = date.day_number();
        let weekday = date.weekday() as usize;
        let times = proj.resolved_calendar(cal).day(day);
        // The source: the first calendar up the chain with an exception on
        // the day, else a work week, else the first stating the weekday.
        let mut at = Some(cal);
        let mut seen = std::collections::HashSet::new();
        let mut default: Option<String> = None;
        while let Some(c) = at {
            if !seen.insert(c.uid) {
                break;
            }
            if let Some(e) = c
                .exceptions
                .iter()
                .find(|e| e.scheduled().is_some_and(|(a, b)| a <= day && day <= b))
            {
                return Ok((
                    times,
                    DaySource::Exception {
                        calendar: c.name.clone(),
                        name: e.name.clone(),
                    },
                ));
            }
            if let Some(w) = c
                .work_weeks
                .iter()
                .find(|w| w.scheduled().is_some_and(|(a, b)| a <= day && day <= b))
                && w.week[weekday].is_some()
            {
                return Ok((
                    times,
                    DaySource::WorkWeek {
                        calendar: c.name.clone(),
                        name: w.name.clone(),
                    },
                ));
            }
            if default.is_none() && c.week[weekday].is_some() {
                default = Some(c.name.clone());
            }
            at = c.base_calendar_uid.and_then(|b| proj.calendar(b));
        }
        Ok((
            times,
            DaySource::Default {
                calendar: default.unwrap_or_else(|| cal.name.clone()),
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> Editor {
        let mut proj = untitled_project();
        proj.tasks.push(Task {
            uid: 1,
            id: 1,
            name: "A".into(),
            outline_level: 1,
            duration_min: 480,
            ..Task::default()
        });
        Editor::new(proj)
    }

    fn times(pairs: &[(u32, u32)]) -> DayWorking {
        DayWorking {
            times: pairs
                .iter()
                .map(|(from, to)| WorkingTime {
                    from: *from,
                    to: *to,
                })
                .collect(),
        }
    }

    #[test]
    fn calendars_are_added_renamed_rebased_and_deleted_as_undo_steps() {
        let mut ed = editor();
        let standard = ed.project().default_calendar_uid;
        assert_eq!(ed.calendar_by("Standard"), Ok(standard));
        assert_eq!(ed.calendar_by(&standard.to_string()), Ok(standard));
        assert!(ed.calendar_by("Moon").is_err());
        assert!(ed.add_calendar("standard", None, None).is_err());
        assert!(ed.add_calendar(" ", None, None).is_err());
        let night = ed.add_calendar("Night Shift", None, None).unwrap();
        assert_eq!(ed.undo_depth(), 1);
        let ann = ed.add_calendar("Ann", Some(standard), None).unwrap();
        assert_eq!(
            ed.project().calendar(ann).unwrap().base_calendar_uid,
            Some(standard)
        );
        assert!(
            ed.project()
                .calendar(ann)
                .unwrap()
                .week
                .iter()
                .all(Option::is_none)
        );
        let copy = ed.add_calendar("Night copy", None, Some(night)).unwrap();
        assert_eq!(
            ed.project().calendar(copy).unwrap().week,
            ed.project().calendar(night).unwrap().week
        );
        // Rename; rebase a derived calendar, never on itself or a cycle, never a base.
        assert_eq!(ed.set_calendar(night, Some("Nights"), None, &[]), Ok(true));
        assert_eq!(ed.set_calendar(night, Some("Nights"), None, &[]), Ok(false));
        assert!(ed.set_calendar(night, Some("Ann"), None, &[]).is_err());
        assert_eq!(ed.set_calendar(ann, None, Some(night), &[]), Ok(true));
        assert!(ed.set_calendar(ann, None, Some(ann), &[]).is_err());
        assert!(ed.set_calendar(night, None, Some(standard), &[]).is_err());
        // The default week: a base calendar states every day, a derived one may inherit.
        assert_eq!(
            ed.set_calendar(night, None, None, &[(6, Some(times(&[(8 * 60, 12 * 60)])))]),
            Ok(true)
        );
        assert!(
            ed.project().calendar(night).unwrap().week[6]
                .as_ref()
                .unwrap()
                .working()
        );
        assert!(ed.set_calendar(night, None, None, &[(6, None)]).is_err());
        assert!(
            ed.set_calendar(night, None, None, &[(1, Some(times(&[(12 * 60, 8 * 60)])))])
                .is_err()
        );
        assert!(
            ed.set_calendar(
                night,
                None,
                None,
                &[(1, Some(times(&[(8 * 60, 12 * 60), (11 * 60, 17 * 60)])))]
            )
            .is_err()
        );
        assert_eq!(
            ed.set_calendar(ann, None, None, &[(1, Some(times(&[])))]),
            Ok(true)
        );
        assert_eq!(ed.set_calendar(ann, None, None, &[(1, None)]), Ok(true));
        // A calendar a task schedules on cannot lose all its working time.
        ed.set_task_calendar(1, Some(night)).unwrap();
        let none: Vec<(usize, DayPatch)> = (0..7).map(|d| (d, Some(times(&[])))).collect();
        assert!(ed.set_calendar(night, None, None, &none).is_err());
        // Delete: refused while used, as the default, or as a base.
        assert!(ed.delete_calendar(night).is_err());
        assert!(ed.delete_calendar(standard).is_err());
        ed.set_task_calendar(1, None).unwrap();
        assert!(ed.delete_calendar(night).is_err(), "Ann derives from it");
        ed.delete_calendar(ann).unwrap();
        ed.delete_calendar(night).unwrap();
        assert!(ed.project().calendar(night).is_none());
        assert!(ed.undo());
        assert!(ed.project().calendar(night).is_some());
    }

    #[test]
    fn exceptions_and_work_weeks_shape_the_days_and_say_where_they_come_from() {
        let mut ed = editor();
        let standard = ed.project().default_calendar_uid;
        let day = |d: u32| DateTime::from_ymd_hm(2026, 3, d, 10, 0);
        // A holiday on Tue 3/3 and a half day on Wed 3/4.
        assert_eq!(
            ed.set_calendar_exception(standard, None, Some("Holiday"), day(3), day(3), times(&[])),
            Ok(0)
        );
        assert_eq!(
            ed.set_calendar_exception(
                standard,
                None,
                None,
                day(4),
                day(4),
                times(&[(8 * 60, 12 * 60)])
            ),
            Ok(1)
        );
        assert!(
            ed.set_calendar_exception(standard, Some(5), None, day(4), day(4), times(&[]))
                .is_err()
        );
        assert!(
            ed.set_calendar_exception(standard, None, None, day(6), day(5), times(&[]))
                .is_err()
        );
        let (t, src) = ed.calendar_day(standard, day(3)).unwrap();
        assert!(t.is_empty());
        assert_eq!(
            src,
            DaySource::Exception {
                calendar: "Standard".into(),
                name: Some("Holiday".into())
            }
        );
        let (t, _) = ed.calendar_day(standard, day(4)).unwrap();
        assert_eq!(t.len(), 1);
        let (t, src) = ed.calendar_day(standard, day(5)).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(
            src,
            DaySource::Default {
                calendar: "Standard".into()
            }
        );
        // A work week over 3/9..3/13 with Fridays off.
        let mut week: [DayPatch; 7] = Default::default();
        week[5] = Some(times(&[]));
        assert_eq!(
            ed.set_calendar_work_week(standard, None, Some("Short"), day(9), day(13), week.clone()),
            Ok(0)
        );
        let (t, src) = ed.calendar_day(standard, day(13)).unwrap();
        assert!(t.is_empty());
        assert_eq!(
            src,
            DaySource::WorkWeek {
                calendar: "Standard".into(),
                name: Some("Short".into())
            }
        );
        // Monday of that week is not stated by the work week: the default.
        let (t, src) = ed.calendar_day(standard, day(9)).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(
            src,
            DaySource::Default {
                calendar: "Standard".into()
            }
        );
        // A derived calendar inherits the exception and names the base.
        let ann = ed.add_calendar("Ann", Some(standard), None).unwrap();
        let (_, src) = ed.calendar_day(ann, day(3)).unwrap();
        assert_eq!(
            src,
            DaySource::Exception {
                calendar: "Standard".into(),
                name: Some("Holiday".into())
            }
        );
        // Edits and deletes by index; a bad index is refused.
        assert_eq!(
            ed.set_calendar_exception(
                standard,
                Some(1),
                Some("Half"),
                day(4),
                day(4),
                times(&[(8 * 60, 12 * 60)])
            ),
            Ok(1)
        );
        assert_eq!(
            ed.project().calendar(standard).unwrap().exceptions[1]
                .name
                .as_deref(),
            Some("Half")
        );
        ed.delete_calendar_exception(standard, 0).unwrap();
        assert_eq!(ed.project().calendar(standard).unwrap().exceptions.len(), 1);
        assert!(ed.delete_calendar_exception(standard, 1).is_err());
        ed.delete_calendar_work_week(standard, 0).unwrap();
        assert!(ed.delete_calendar_work_week(standard, 0).is_err());
        // Every edit was a step.
        assert_eq!(ed.undo_depth(), 7);
        assert_eq!(weekday_index("mon"), Some(1));
        assert_eq!(weekday_index("Saturday"), Some(6));
        assert_eq!(weekday_index("x"), None);
        assert_eq!(parse_clock("8:00"), Some(480));
        assert_eq!(parse_clock("24:00"), Some(1440));
        assert_eq!(parse_clock("24:30"), None);
        assert_eq!(clock(1020), "17:00");
    }
}
