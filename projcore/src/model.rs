//! The project domain model — pure input data, no scheduling results.
//!
//! A [`Project`] is a set of [`Task`]s linked by dependencies, optionally
//! staffed by [`Resource`]s via [`Assignment`]s, all interpreted against
//! working-time [`Calendar`]s. The scheduler ([`crate::schedule`]) consumes
//! this and produces start/finish dates; it never mutates the model. MSPDI's
//! own computed `Start`/`Finish` are captured here as `stored_*` so they can
//! serve as an oracle for our scheduler.

use crate::datetime::DateTime;

/// Dependency kind between two tasks. The `code` is MSPDI's integer encoding,
/// which is *not* in the intuitive order — memorized here once so nowhere else
/// has to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LinkType {
    FinishFinish, // 0
    #[default]
    FinishStart, // 1
    StartFinish, // 2
    StartStart,  // 3
}

impl LinkType {
    pub fn from_code(code: i64) -> Option<LinkType> {
        Some(match code {
            0 => LinkType::FinishFinish,
            1 => LinkType::FinishStart,
            2 => LinkType::StartFinish,
            3 => LinkType::StartStart,
            _ => return None,
        })
    }

    pub fn code(self) -> i64 {
        match self {
            LinkType::FinishFinish => 0,
            LinkType::FinishStart => 1,
            LinkType::StartFinish => 2,
            LinkType::StartStart => 3,
        }
    }
}

/// Scheduling constraint on a task. `code` is MSPDI's encoding (0..=7).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ConstraintType {
    #[default]
    AsSoonAsPossible, // 0
    AsLateAsPossible,  // 1
    MustStartOn,       // 2
    MustFinishOn,      // 3
    StartNoEarlierThan, // 4
    StartNoLaterThan,   // 5
    FinishNoEarlierThan, // 6
    FinishNoLaterThan,   // 7
}

impl ConstraintType {
    pub fn from_code(code: i64) -> Option<ConstraintType> {
        use ConstraintType::*;
        Some(match code {
            0 => AsSoonAsPossible,
            1 => AsLateAsPossible,
            2 => MustStartOn,
            3 => MustFinishOn,
            4 => StartNoEarlierThan,
            5 => StartNoLaterThan,
            6 => FinishNoEarlierThan,
            7 => FinishNoLaterThan,
            _ => return None,
        })
    }

    pub fn code(self) -> i64 {
        use ConstraintType::*;
        match self {
            AsSoonAsPossible => 0,
            AsLateAsPossible => 1,
            MustStartOn => 2,
            MustFinishOn => 3,
            StartNoEarlierThan => 4,
            StartNoLaterThan => 5,
            FinishNoEarlierThan => 6,
            FinishNoLaterThan => 7,
        }
    }
}

/// One predecessor link on a task.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Predecessor {
    /// UID of the predecessor task.
    pub uid: i32,
    pub link: LinkType,
    /// Lag in **minutes**, already converted from MSPDI's tenths-of-a-minute.
    /// Negative means lead (overlap).
    pub lag_min: i64,
}

/// A schedulable task (or a summary/milestone).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Task {
    pub uid: i32,
    pub id: i32,
    pub name: String,
    /// 1-based outline depth; summary tasks own the following deeper rows.
    pub outline_level: u32,
    pub summary: bool,
    pub milestone: bool,
    /// Duration as **working minutes** (span of working time, not wall-clock).
    pub duration_min: i64,
    pub predecessors: Vec<Predecessor>,
    pub constraint: ConstraintType,
    pub constraint_date: Option<DateTime>,
    /// Task-specific calendar UID; falls back to the project calendar.
    pub calendar_uid: Option<i32>,
    /// Start/Finish as stored in the source file (Project's own computed
    /// values). Used as an oracle; the scheduler writes its own results
    /// elsewhere.
    pub stored_start: Option<DateTime>,
    pub stored_finish: Option<DateTime>,
    /// Baseline (the saved plan) start/finish, for planned-vs-current variance.
    /// Set by "Set Baseline"; round-trips through MSPDI's `<Baseline>` element.
    pub baseline_start: Option<DateTime>,
    pub baseline_finish: Option<DateTime>,
}

impl Task {
    pub fn is_milestone(&self) -> bool {
        self.milestone || self.duration_min == 0
    }
}

/// A resource (person, equipment, or material).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Resource {
    pub uid: i32,
    pub id: i32,
    pub name: String,
    /// MSPDI Type: 1 = Work (people/equipment), 0 = Material. We keep the flag.
    pub is_work: bool,
    /// Availability, e.g. 1.0 = 100%.
    pub max_units: f64,
    pub calendar_uid: Option<i32>,
}

/// An assignment of a resource to a task.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Assignment {
    pub uid: i32,
    pub task_uid: i32,
    pub resource_uid: i32,
    pub units: f64,
    /// Work in **minutes**.
    pub work_min: i64,
}

/// A working-time slot within a day, in minutes-of-day (`from` inclusive,
/// `to` exclusive). E.g. 08:00–12:00 is `{ from: 480, to: 720 }`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WorkingTime {
    pub from: u32,
    pub to: u32,
}

/// A weekday's working pattern. Empty `times` ⇒ a non-working day.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DayWorking {
    pub times: Vec<WorkingTime>,
}

impl DayWorking {
    pub fn working(&self) -> bool {
        !self.times.is_empty()
    }

    /// Total working minutes in this day.
    pub fn minutes(&self) -> i64 {
        self.times.iter().map(|t| (t.to - t.from) as i64).sum()
    }
}

/// A working-time calendar. `week[d]` is indexed by weekday, Sunday=0..Saturday=6
/// (matching [`DateTime::weekday`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Calendar {
    pub uid: i32,
    pub name: String,
    pub week: [DayWorking; 7],
}

impl Calendar {
    /// The MS Project "Standard" base calendar: Mon–Fri, 08:00–12:00 &
    /// 13:00–17:00 (8 working hours), weekends off.
    pub fn standard(uid: i32) -> Calendar {
        let shift = vec![
            WorkingTime { from: 8 * 60, to: 12 * 60 },
            WorkingTime { from: 13 * 60, to: 17 * 60 },
        ];
        let off = DayWorking::default();
        let on = DayWorking { times: shift };
        Calendar {
            uid,
            name: "Standard".into(),
            // Sun, Mon, Tue, Wed, Thu, Fri, Sat
            week: [
                off.clone(),
                on.clone(),
                on.clone(),
                on.clone(),
                on.clone(),
                on,
                off,
            ],
        }
    }
}

/// A whole project: tasks, staffing, and the calendars they schedule against.
#[derive(Clone, PartialEq, Debug)]
pub struct Project {
    pub name: String,
    pub title: String,
    pub start_date: Option<DateTime>,
    /// Conversion factor for rendering durations (MSPDI `HoursPerDay`).
    pub hours_per_day: f64,
    pub hours_per_week: f64,
    /// UID of the project's default calendar.
    pub default_calendar_uid: i32,
    pub tasks: Vec<Task>,
    pub resources: Vec<Resource>,
    pub assignments: Vec<Assignment>,
    pub calendars: Vec<Calendar>,
}

impl Default for Project {
    fn default() -> Project {
        Project {
            name: String::new(),
            title: String::new(),
            start_date: None,
            hours_per_day: 8.0,
            hours_per_week: 40.0,
            default_calendar_uid: 1,
            tasks: Vec::new(),
            resources: Vec::new(),
            assignments: Vec::new(),
            calendars: vec![Calendar::standard(1)],
        }
    }
}

impl Project {
    /// Working minutes → days, using the project's `hours_per_day` (how MS
    /// Project renders a duration column).
    pub fn minutes_to_days(&self, min: i64) -> f64 {
        min as f64 / (self.hours_per_day * 60.0)
    }

    /// Days → working minutes.
    pub fn days_to_minutes(&self, days: f64) -> i64 {
        (days * self.hours_per_day * 60.0).round() as i64
    }

    pub fn task(&self, uid: i32) -> Option<&Task> {
        self.tasks.iter().find(|t| t.uid == uid)
    }

    /// The calendar a task schedules against: its own, else the project default,
    /// else the first calendar, else a synthesized Standard.
    pub fn calendar_for(&self, task: &Task) -> Calendar {
        let want = task.calendar_uid.unwrap_or(self.default_calendar_uid);
        self.calendars
            .iter()
            .find(|c| c.uid == want)
            .or_else(|| self.calendars.first())
            .cloned()
            .unwrap_or_else(|| Calendar::standard(want))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_constraint_codes_round_trip() {
        for c in 0..=3 {
            assert_eq!(LinkType::from_code(c).unwrap().code(), c);
        }
        for c in 0..=7 {
            assert_eq!(ConstraintType::from_code(c).unwrap().code(), c);
        }
        assert!(LinkType::from_code(4).is_none());
        assert!(ConstraintType::from_code(8).is_none());
    }

    #[test]
    fn standard_calendar_is_8h_weekdays() {
        let cal = Calendar::standard(1);
        assert_eq!(cal.week[1].minutes(), 480); // Monday
        assert!(!cal.week[0].working()); // Sunday off
        assert!(!cal.week[6].working()); // Saturday off
    }

    #[test]
    fn duration_conversion() {
        let p = Project::default();
        assert_eq!(p.minutes_to_days(960), 2.0); // 16h @ 8h/day
        assert_eq!(p.days_to_minutes(2.0), 960);
    }
}
