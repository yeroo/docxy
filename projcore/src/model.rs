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
    StartFinish,  // 2
    StartStart,   // 3
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
    AsLateAsPossible,    // 1
    MustStartOn,         // 2
    MustFinishOn,        // 3
    StartNoEarlierThan,  // 4
    StartNoLaterThan,    // 5
    FinishNoEarlierThan, // 6
    FinishNoLaterThan,   // 7
}

impl ConstraintType {
    pub fn abbrev(self) -> &'static str {
        match self {
            Self::AsSoonAsPossible => "ASAP",
            Self::AsLateAsPossible => "ALAP",
            Self::MustStartOn => "MSO",
            Self::MustFinishOn => "MFO",
            Self::StartNoEarlierThan => "SNET",
            Self::StartNoLaterThan => "SNLT",
            Self::FinishNoEarlierThan => "FNET",
            Self::FinishNoLaterThan => "FNLT",
        }
    }

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

/// How a task keeps duration, work and units consistent (MSPDI `Type`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskType {
    FixedUnits,    // 0
    FixedDuration, // 1
    FixedWork,     // 2
}

impl TaskType {
    pub fn from_code(code: i64) -> Option<TaskType> {
        Some(match code {
            0 => TaskType::FixedUnits,
            1 => TaskType::FixedDuration,
            2 => TaskType::FixedWork,
            _ => return None,
        })
    }

    pub fn code(self) -> i64 {
        match self {
            TaskType::FixedUnits => 0,
            TaskType::FixedDuration => 1,
            TaskType::FixedWork => 2,
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

/// A recorded plan in one MSPDI baseline slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Baseline {
    /// 0 = Baseline; 1..=10 = Baseline1..Baseline10.
    pub number: u8,
    pub start: Option<DateTime>,
    pub finish: Option<DateTime>,
    /// Recorded working minutes; None when Duration was omitted, empty, or invalid.
    pub duration_min: Option<i64>,
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
    /// elsewhere. The editor rewrites them for a manual task whose dates it
    /// edits (start, finish or duration), so a save's Start/Finish agree
    /// with its pinned dates.
    pub stored_start: Option<DateTime>,
    pub stored_finish: Option<DateTime>,
    /// Saved plans, sorted by number with at most one record per slot (0..=10).
    /// Use set_baseline_slot to replace a slot; UI variance uses slot 0.
    pub baselines: Vec<Baseline>,
    /// Manually scheduled (MSPDI `Manual`): the task stays at the dates the
    /// user gave it instead of moving with its links and constraints.
    pub manual: bool,
    /// MSPDI `ManualStart`/`ManualFinish`/`ManualDuration`, kept as read so they
    /// round-trip even on auto tasks, where Project also writes them.
    pub manual_start: Option<DateTime>,
    pub manual_finish: Option<DateTime>,
    pub manual_duration_min: Option<i64>,
    /// A blank row (MSPDI `IsNull`): Project keeps it as an entry with a UID
    /// and ID, but it is not a task. The scheduler and outline ignore it.
    pub is_null: bool,
    // Fields below are kept as read so a save writes them back; `None` means
    // the source had no (valid) value and the save writes none.
    pub guid: Option<String>,
    pub create_date: Option<DateTime>,
    pub wbs: Option<String>,
    pub task_type: Option<TaskType>,
    /// `None` reads as active; see [`Task::is_active`]. The scheduler does not
    /// yet drop inactive tasks.
    pub active: Option<bool>,
    pub effort_driven: Option<bool>,
    /// Duration shown with `?` in Project.
    pub estimated: Option<bool>,
    /// Levelling priority, 0..=1000.
    pub priority: Option<i32>,
    /// Bounds the late finish only, so a missed deadline shows as negative
    /// total slack. It never moves scheduled dates.
    pub deadline: Option<DateTime>,
    pub level_assignments: Option<bool>,
    pub leveling_can_split: Option<bool>,
    /// MSPDI `LevelingDelay`, raw (tenths of a minute), with its display format.
    pub leveling_delay: Option<i64>,
    pub leveling_delay_format: Option<i32>,
    pub ignore_resource_calendar: Option<bool>,
    pub earned_value_method: Option<i32>,
    pub recurring: Option<bool>,
    pub hide_bar: Option<bool>,
    pub rollup: Option<bool>,
    pub external_task: Option<bool>,
    pub is_subproject: Option<bool>,
    pub is_subproject_read_only: Option<bool>,
    /// Stored values docxy does not compute: Work (minutes), Cost and
    /// OverAllocated as Project last calculated them.
    pub work_min: Option<i64>,
    pub cost: Option<Rate>,
    pub over_allocated: Option<bool>,
    // Recorded progress, kept as read so a save writes it back. docxy neither
    // computes nor reconciles it: the scheduler ignores it and edits leave it
    // as read. Durations and work are whole minutes, rounded from the source.
    /// Percents, 0..=100.
    pub percent_complete: Option<u8>,
    pub percent_work_complete: Option<u8>,
    pub physical_percent_complete: Option<u8>,
    pub actual_start: Option<DateTime>,
    pub actual_finish: Option<DateTime>,
    /// Where completed work ends and remaining work picks up.
    pub stop: Option<DateTime>,
    pub resume: Option<DateTime>,
    pub actual_duration_min: Option<i64>,
    pub remaining_duration_min: Option<i64>,
    pub actual_work_min: Option<i64>,
    pub remaining_work_min: Option<i64>,
    pub actual_cost: Option<Rate>,
    pub remaining_cost: Option<Rate>,
    /// Variances as MSPDI stores them, not interpreted: `StartVariance` and
    /// `FinishVariance` are integers, `WorkVariance` a float kept as decimal text.
    pub start_variance: Option<i64>,
    pub finish_variance: Option<i64>,
    pub work_variance: Option<Rate>,
}

impl Task {
    pub fn baseline(&self, number: u8) -> Option<&Baseline> {
        self.baselines.iter().find(|b| b.number == number)
    }

    /// Replace the whole record for a slot, maintaining unique, sorted slots.
    pub fn set_baseline_slot(&mut self, baseline: Baseline) {
        self.baselines.retain(|b| b.number != baseline.number);
        self.baselines.push(baseline);
        self.baselines.sort_by_key(|b| b.number);
    }

    /// The dates a manual task is pinned to: its manual start (else the stored
    /// start) and its manual finish, if any. When the finish is absent the
    /// scheduler derives it from the start and `duration_min`. The stored
    /// finish is never used: it goes stale as soon as the duration is edited.
    /// `None` for auto tasks and for a manual task with no start at all
    /// (Project's "TBD" task), which then schedules like an auto task. Also
    /// `None` for summaries: their dates roll up from their children, and a
    /// manual summary's own dates are not modeled.
    pub fn pinned_dates(&self) -> Option<(DateTime, Option<DateTime>)> {
        if !self.manual || self.summary {
            return None;
        }
        let start = self.manual_start.or(self.stored_start)?;
        Some((start, self.manual_finish))
    }

    pub fn is_active(&self) -> bool {
        self.active.unwrap_or(true)
    }

    pub fn is_milestone(&self) -> bool {
        self.milestone || self.duration_min == 0
    }
}

/// Resource kind. MSPDI encodes Cost using Type 0 plus IsCostResource.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ResourceType {
    Material,
    #[default]
    Work,
    Cost,
}

impl ResourceType {
    /// Read a Type code, accepting the nonstandard code 2 for Cost.
    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            0 => Self::Material,
            1 => Self::Work,
            2 => Self::Cost,
            _ => return None,
        })
    }

    /// Schema Type code. Cost additionally requires IsCostResource = true;
    /// this code alone cannot distinguish Cost from Material.
    pub fn code(self) -> i64 {
        match self {
            Self::Material | Self::Cost => 0,
            Self::Work => 1,
        }
    }
}

/// When resource costs accrue, including the schema's explicit Invalid value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccrueAt {
    Start,
    End,
    Prorated,
    Invalid,
}

impl AccrueAt {
    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            1 => Self::Start,
            2 => Self::End,
            3 => Self::Prorated,
            4 => Self::Invalid,
            _ => return None,
        })
    }

    pub fn code(self) -> i64 {
        match self {
            Self::Start => 1,
            Self::End => 2,
            Self::Prorated => 3,
            Self::Invalid => 4,
        }
    }
}

/// A resource rate stored as valid `xsd:decimal` text.
///
/// Keeping its source spelling preserves precision and forms such as `+5` or
/// `007` through MSPDI and `.yppx` saves.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rate(String);

impl Rate {
    /// Parse a decimal rate, retaining valid decimal text after trimming XML
    /// Schema whitespace (space, tab, CR, LF); other Unicode spaces are invalid.
    /// Finite float syntax accepted by older readers is converted to decimal.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim_matches([' ', '\t', '\r', '\n']);
        let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
        let mut parts = unsigned.split('.');
        let whole = parts.next()?;
        let fraction = parts.next();
        let decimal = match fraction {
            None => !whole.is_empty() && whole.bytes().all(|byte| byte.is_ascii_digit()),
            Some(fraction) => {
                parts.next().is_none()
                    && (!whole.is_empty() || !fraction.is_empty())
                    && whole.bytes().all(|byte| byte.is_ascii_digit())
                    && fraction.bytes().all(|byte| byte.is_ascii_digit())
            }
        };
        if decimal {
            Some(Self(text.to_owned()))
        } else {
            Self::from_f64(text.parse().ok()?)
        }
    }

    /// Convert a finite float to decimal text; nonfinite values have no rate.
    pub fn from_f64(value: f64) -> Option<Self> {
        value.is_finite().then(|| Self(value.to_string()))
    }

    /// Return the decimal text written to MSPDI.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A resource (person, equipment, material, or cost).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Resource {
    pub uid: i32,
    pub id: i32,
    pub name: String,
    pub kind: ResourceType,
    /// Optional source fields retain the distinction between absent and empty.
    pub initials: Option<String>,
    pub material_label: Option<String>,
    pub code: Option<String>,
    pub group: Option<String>,
    /// Availability, e.g. 1.0 = 100%.
    pub max_units: f64,
    pub accrue_at: Option<AccrueAt>,
    /// Stored rates only; the scheduler does not calculate costs.
    pub standard_rate: Option<Rate>,
    pub overtime_rate: Option<Rate>,
    pub cost_per_use: Option<Rate>,
    pub calendar_uid: Option<i32>,
    // Stored as read so a save writes them back; nothing here schedules,
    // levels or costs with them, and edits do not refresh them.
    /// The unit Project displays each rate in, as the MSPDI code (1 minute,
    /// 2 hour, 3 day, 4 week, 5 month, 7 year; the standard rate also 8, a
    /// material rate). Kept as the code so an unnamed one still round-trips.
    /// The rate text is kept as written, not converted to this unit.
    pub standard_rate_format: Option<u8>,
    pub overtime_rate_format: Option<u8>,
    /// 0 committed, 1 proposed.
    pub booking_type: Option<u8>,
    /// 0 default, 1 none, 2 email, 3 web.
    pub work_group: Option<u8>,
    pub is_generic: Option<bool>,
    pub is_budget: Option<bool>,
    pub is_inactive: Option<bool>,
    pub can_level: Option<bool>,
    pub over_allocated: Option<bool>,
    /// Decimal text, e.g. `1` = 100%.
    pub peak_units: Option<Rate>,
    /// Work in whole minutes, rounded from the source.
    pub work_min: Option<i64>,
    pub regular_work_min: Option<i64>,
    pub remaining_work_min: Option<i64>,
}

/// An assignment of a resource to a task.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Assignment {
    pub uid: i32,
    pub task_uid: i32,
    pub resource_uid: i32,
    pub units: f64,
    /// Work in **minutes**.
    pub work_min: i64,
    // Recorded progress, kept as read so a save writes it back; see the same
    // block on [`Task`]. Work is whole minutes, rounded from the source.
    /// 0..=100.
    pub percent_work_complete: Option<u8>,
    pub actual_start: Option<DateTime>,
    pub actual_finish: Option<DateTime>,
    pub stop: Option<DateTime>,
    pub resume: Option<DateTime>,
    pub actual_work_min: Option<i64>,
    pub remaining_work_min: Option<i64>,
    pub actual_cost: Option<Rate>,
    pub remaining_cost: Option<Rate>,
    /// Variances as MSPDI stores them, not interpreted (integers for dates,
    /// decimal text for work and cost).
    pub start_variance: Option<i64>,
    pub finish_variance: Option<i64>,
    pub work_variance: Option<Rate>,
    pub cost_variance: Option<Rate>,
    // Stored as read, like the progress above; the scheduler neither uses
    // nor refreshes them.
    /// How the work is spread over time: 0 flat .. 8 contoured.
    pub work_contour: Option<u8>,
    pub fixed_material: Option<bool>,
    pub has_fixed_rate_units: Option<bool>,
    /// The assignment's own dates, which differ from its task's when it is
    /// delayed or contoured.
    pub start: Option<DateTime>,
    pub finish: Option<DateTime>,
    /// Work less overtime, in whole minutes. An edit that changes `work_min`
    /// clears it, since keeping it would assert overtime nobody entered.
    pub regular_work_min: Option<i64>,
    /// Saved plans, sorted by number with at most one record per slot (0..=10).
    pub baselines: Vec<AssignmentBaseline>,
}

impl Assignment {
    pub fn baseline(&self, number: u8) -> Option<&AssignmentBaseline> {
        self.baselines.iter().find(|b| b.number == number)
    }

    /// Replace the whole record for a slot, maintaining unique, sorted slots.
    pub fn set_baseline_slot(&mut self, baseline: AssignmentBaseline) {
        self.baselines.retain(|b| b.number != baseline.number);
        self.baselines.push(baseline);
        self.baselines.sort_by_key(|b| b.number);
    }
}

/// A recorded plan in one MSPDI baseline slot of an assignment.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct AssignmentBaseline {
    /// 0 = Baseline; 1..=10 = Baseline1..Baseline10.
    pub number: u8,
    pub start: Option<DateTime>,
    pub finish: Option<DateTime>,
    /// Recorded work in whole minutes; None when omitted or invalid.
    pub work_min: Option<i64>,
    pub cost: Option<Rate>,
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

/// A week of working patterns, indexed by weekday, Sunday=0..Saturday=6
/// (matching [`DateTime::weekday`]), with every day resolved.
pub type Week = [DayWorking; 7];

/// A working-time calendar. `week[d]` is indexed by weekday, Sunday=0..Saturday=6
/// (matching [`DateTime::weekday`]). A derived calendar states only the days it
/// overrides; resolve its working time with [`Project::resolved_calendar`]
/// (by date, with exceptions) or [`Project::resolved_week`] (the weekly
/// pattern alone) rather than reading `week` directly.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Calendar {
    pub uid: i32,
    pub name: String,
    /// The calendar this one derives from (MSPDI `BaseCalendarUID`); `None`
    /// for a base calendar.
    pub base_calendar_uid: Option<i32>,
    /// MSPDI `IsBaselineCalendar`, kept as read.
    pub is_baseline_calendar: bool,
    /// The weekdays this calendar states itself. `None` inherits the base
    /// calendar's day; a base calendar has no base, so there an unstated day is
    /// non-working.
    pub week: [Option<DayWorking>; 7],
    /// MSPDI `Exceptions`, in file order: holidays, one-off working days and
    /// changed hours. Only [`CalendarException::scheduled`] ones change working
    /// time; the rest are kept so a save writes them back.
    pub exceptions: Vec<CalendarException>,
}

/// One MSPDI calendar exception. Every `Option` field is kept as read (`None`
/// when the file omitted it) so a save writes it back unchanged. `DayWorking`
/// is always written, from whether `day` has working times, as for a weekday.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CalendarException {
    pub name: Option<String>,
    /// `TimePeriod/FromDate` and `TimePeriod/ToDate`.
    pub from: Option<DateTime>,
    pub to: Option<DateTime>,
    /// MSPDI `Type`: 1 is daily, 2-8 are recurring patterns.
    pub kind: Option<i32>,
    pub occurrences: Option<i32>,
    pub entered_by_occurrences: Option<bool>,
    pub period: Option<i32>,
    pub days_of_week: Option<i32>,
    pub month_item: Option<i32>,
    pub month_position: Option<i32>,
    pub month: Option<i32>,
    pub month_day: Option<i32>,
    /// The day's working time; empty `times` ⇒ non-working.
    pub day: DayWorking,
}

impl CalendarException {
    /// A date-range exception filled in as Project writes one (daily, one
    /// occurrence, not entered by occurrences, no name). Project's legacy
    /// `WeekDay` `DayType 0` entries read as these, so a save and a reread
    /// give the same value.
    pub fn date_range(from: DateTime, to: DateTime, day: DayWorking) -> CalendarException {
        CalendarException {
            from: Some(from),
            to: Some(to),
            kind: Some(1),
            occurrences: Some(1),
            entered_by_occurrences: Some(false),
            day,
            ..CalendarException::default()
        }
    }

    /// The inclusive range of day numbers the scheduler honours this exception
    /// on: a daily (`Type 1`) exception, from `FromDate`'s date to `ToDate`'s
    /// date. A recurrence (`Type` 2-8, or `Period` > 1) is kept but not
    /// scheduled, and yields `None`.
    pub fn scheduled(&self) -> Option<(i64, i64)> {
        if self.kind != Some(1) || self.period.is_some_and(|p| p > 1) {
            return None;
        }
        let first = self.from?.day_number();
        Some((first, self.to?.day_number().max(first)))
    }
}

impl Calendar {
    /// The MS Project "Standard" base calendar: Mon–Fri, 08:00–12:00 &
    /// 13:00–17:00 (8 working hours), weekends off.
    pub fn standard(uid: i32) -> Calendar {
        Calendar::base(uid, "Standard", Calendar::standard_week())
    }

    /// The working week of [`Calendar::standard`].
    pub fn standard_week() -> Week {
        let shift = vec![
            WorkingTime {
                from: 8 * 60,
                to: 12 * 60,
            },
            WorkingTime {
                from: 13 * 60,
                to: 17 * 60,
            },
        ];
        let off = DayWorking::default();
        let on = DayWorking { times: shift };
        // Sun, Mon, Tue, Wed, Thu, Fri, Sat
        [
            off.clone(),
            on.clone(),
            on.clone(),
            on.clone(),
            on.clone(),
            on,
            off,
        ]
    }

    /// A base calendar with the given weekly pattern.
    pub fn base(uid: i32, name: &str, week: Week) -> Calendar {
        Calendar {
            uid,
            name: name.into(),
            base_calendar_uid: None,
            is_baseline_calendar: false,
            week: week.map(Some),
            exceptions: Vec::new(),
        }
    }

    /// This calendar's working week, each day it does not state taken from its
    /// base chain (`lookup` finds a calendar by UID). A missing base or a cycle
    /// ends the chain; a day still unresolved is non-working.
    pub fn resolve_week<'a>(&'a self, lookup: impl Fn(i32) -> Option<&'a Calendar>) -> Week {
        let mut week: [Option<&DayWorking>; 7] = [None; 7];
        let mut seen = std::collections::HashSet::new();
        let mut next = Some(self);
        while let Some(cal) = next {
            if !seen.insert(cal.uid) {
                break;
            }
            for (day, own) in week.iter_mut().zip(&cal.week) {
                if day.is_none() {
                    *day = own.as_ref();
                }
            }
            next = cal.base_calendar_uid.and_then(&lookup);
        }
        week.map(|day| day.cloned().unwrap_or_default())
    }

    /// This calendar's working time by date, exceptions included, through the
    /// base chain [`Calendar::resolve_week`] walks.
    pub fn resolve<'a>(&'a self, lookup: impl Fn(i32) -> Option<&'a Calendar>) -> WorkCalendar {
        let mut levels = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut next = Some(self);
        while let Some(cal) = next {
            if !seen.insert(cal.uid) {
                break;
            }
            levels.push(Level::new(cal));
            next = cal.base_calendar_uid.and_then(&lookup);
        }
        WorkCalendar(Kind::Chain(levels))
    }
}

/// Working time resolved by date: a calendar with its base chain and
/// exceptions, a plain weekly pattern, or the union of several calendars (the
/// summary calendar's fallback).
///
/// A day resolves to the first scheduled exception covering that date, looking
/// down the base chain from the calendar itself; failing that, to the first
/// calendar in the chain that states that weekday. A day nothing states is
/// non-working. So a derived calendar keeps its base's holidays even on a
/// weekday it states itself, and only its own exception overrides one. This
/// is Microsoft Project 2024's rule (checked on a resource calendar, #126);
/// MPXJ instead lets a derived calendar's own weekday win.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkCalendar(Kind);

#[derive(Clone, PartialEq, Eq, Debug)]
enum Kind {
    Chain(Vec<Level>),
    Union(Vec<WorkCalendar>),
}

/// One calendar of a base chain.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Level {
    /// Scheduled exceptions as disjoint `(first_day, last_day, working)`
    /// ranges sorted by day. Where exceptions overlap, the first in file order
    /// wins.
    exceptions: Vec<(i64, i64, DayWorking)>,
    week: [Option<DayWorking>; 7],
}

impl Level {
    fn new(cal: &Calendar) -> Level {
        let mut ranges: Vec<(i64, i64, DayWorking)> = Vec::new();
        for exception in &cal.exceptions {
            let Some(range) = exception.scheduled() else {
                continue;
            };
            // Keep only the days no earlier exception claimed.
            let mut pieces = vec![range];
            for &(taken_first, taken_last, _) in &ranges {
                pieces = pieces
                    .into_iter()
                    .flat_map(|(first, last)| {
                        if taken_last < first || taken_first > last {
                            vec![(first, last)]
                        } else {
                            [(first, taken_first - 1), (taken_last + 1, last)]
                                .into_iter()
                                .filter(|(first, last)| first <= last)
                                .collect()
                        }
                    })
                    .collect();
            }
            ranges.extend(
                pieces
                    .into_iter()
                    .map(|(first, last)| (first, last, exception.day.clone())),
            );
        }
        ranges.sort_by_key(|&(first, _, _)| first);
        Level {
            exceptions: ranges,
            week: cal.week.clone(),
        }
    }

    /// This calendar's own exception covering day number `day`.
    fn exception(&self, day: i64) -> Option<&DayWorking> {
        let after = self
            .exceptions
            .partition_point(|&(first, _, _)| first <= day);
        let (_, last, working) = &self.exceptions[after.checked_sub(1)?];
        (day <= *last).then_some(working)
    }

    /// The weekday of day number `day`, if this calendar states it.
    fn weekday(&self, day: i64) -> Option<&DayWorking> {
        self.week[(day + 4).rem_euclid(7) as usize].as_ref()
    }
}

impl WorkCalendar {
    /// A calendar that is the same every week.
    pub fn weekly(week: Week) -> WorkCalendar {
        WorkCalendar(Kind::Chain(vec![Level {
            exceptions: Vec::new(),
            week: week.map(Some),
        }]))
    }

    /// Working time wherever any of `calendars` works.
    pub fn union(calendars: Vec<WorkCalendar>) -> WorkCalendar {
        WorkCalendar(Kind::Union(calendars))
    }

    /// The working time on day number `day` (as [`DateTime::day_number`]):
    /// sorted, positive-length slots, overlaps merged.
    pub fn day(&self, day: i64) -> Vec<WorkingTime> {
        let mut out = Vec::new();
        self.day_into(day, &mut out);
        out
    }

    /// [`WorkCalendar::day`] into a reused buffer, which is cleared first.
    pub(crate) fn day_into(&self, day: i64, out: &mut Vec<WorkingTime>) {
        out.clear();
        match &self.0 {
            Kind::Chain(levels) => {
                let working = levels
                    .iter()
                    .find_map(|level| level.exception(day))
                    .or_else(|| levels.iter().find_map(|level| level.weekday(day)));
                if let Some(working) = working {
                    out.extend(merge(working.times.iter().copied()));
                }
            }
            Kind::Union(calendars) => {
                let mut all = Vec::new();
                let mut one = Vec::new();
                for cal in calendars {
                    cal.day_into(day, &mut one);
                    all.append(&mut one);
                }
                out.extend(merge(all.into_iter()));
            }
        }
    }

    /// The weekly pattern alone, exceptions ignored. Whether a calendar can
    /// schedule at all is decided on this, so an exception never makes an
    /// otherwise empty calendar schedulable.
    pub fn week(&self) -> Week {
        match &self.0 {
            Kind::Chain(levels) => std::array::from_fn(|dow| {
                levels
                    .iter()
                    .find_map(|level| level.week[dow].clone())
                    .unwrap_or_default()
            }),
            Kind::Union(calendars) => {
                let weeks: Vec<Week> = calendars.iter().map(WorkCalendar::week).collect();
                std::array::from_fn(|dow| DayWorking {
                    times: merge(
                        weeks
                            .iter()
                            .flat_map(|week| week[dow].times.iter().copied()),
                    ),
                })
            }
        }
    }

    /// Whether the weekly pattern has any working time (see
    /// [`WorkCalendar::week`]).
    pub fn has_working_time(&self) -> bool {
        self.week()
            .iter()
            .flat_map(|day| &day.times)
            .any(|t| t.from < t.to)
    }
}

/// Sorted, positive-length slots with overlapping ones merged.
fn merge(times: impl Iterator<Item = WorkingTime>) -> Vec<WorkingTime> {
    let mut times: Vec<WorkingTime> = times.filter(|t| t.from < t.to).collect();
    times.sort_by_key(|t| t.from);
    let mut merged: Vec<WorkingTime> = Vec::with_capacity(times.len());
    for t in times {
        match merged.last_mut() {
            Some(last) if t.from <= last.to => last.to = last.to.max(t.to),
            _ => merged.push(t),
        }
    }
    merged
}

/// A whole project: tasks, staffing, and the calendars they schedule against.
#[derive(Clone, PartialEq, Debug)]
pub struct Project {
    pub name: String,
    pub title: String,
    pub start_date: Option<DateTime>,
    /// Let date constraints override conflicting links (MSPDI HonorConstraints).
    pub honor_constraints: bool,
    /// Whether tasks added to this plan start out manually scheduled (MSPDI
    /// `NewTasksAreManual`).
    pub new_tasks_are_manual: bool,
    /// Conversion factor for rendering durations (MSPDI `HoursPerDay`).
    pub hours_per_day: f64,
    pub hours_per_week: f64,
    /// UID of the project's default calendar.
    pub default_calendar_uid: i32,
    /// Project-level MSPDI options docxy stores but does not model
    /// (`ScheduleFromStart`, currency, task defaults, ...), as (element name,
    /// text) in read order. A save writes each back verbatim, so an unsupported
    /// setting survives rather than resetting to Project's default.
    pub options: Vec<(String, String)>,
    /// Tasks in outline order. UIDs are unique: the readers reject duplicates,
    /// and the scheduler, links and assignments all look tasks up by UID.
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
            honor_constraints: true,
            new_tasks_are_manual: false,
            hours_per_day: 8.0,
            hours_per_week: 40.0,
            default_calendar_uid: 1,
            options: Vec::new(),
            tasks: Vec::new(),
            resources: Vec::new(),
            assignments: Vec::new(),
            calendars: vec![Calendar::standard(1)],
        }
    }
}

impl Project {
    /// The stored text of an unmodeled project option (see [`Project::options`]).
    pub fn option(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Whether this row has outline children, independently of its stored flag.
    /// Blank rows are outside the outline: one is never a summary, and the
    /// next non-blank row decides whether the row above it is.
    pub(crate) fn is_outline_summary(&self, index: usize) -> bool {
        self.tasks.get(index).is_some_and(|task| {
            !task.is_null
                && self.tasks[index + 1..]
                    .iter()
                    .find(|next| !next.is_null)
                    .is_some_and(|next| next.outline_level > task.outline_level)
        })
    }

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

    /// The calendar with this UID. When several share it, the last wins, as in
    /// the scheduler.
    pub fn calendar(&self, uid: i32) -> Option<&Calendar> {
        self.calendars.iter().rev().find(|c| c.uid == uid)
    }

    /// `cal`'s working week, resolved through its base chain in this project.
    /// Exceptions are ignored; see [`Project::resolved_calendar`].
    pub fn resolved_week(&self, cal: &Calendar) -> Week {
        cal.resolve_week(|uid| self.calendar(uid))
    }

    /// `cal`'s working time by date, exceptions included, resolved through its
    /// base chain in this project.
    pub fn resolved_calendar(&self, cal: &Calendar) -> WorkCalendar {
        cal.resolve(|uid| self.calendar(uid))
    }

    /// A task's working week, resolved through its base chain: the task's
    /// calendar, or the project default when the task names none; if that UID
    /// is missing, the first calendar; with no calendars, Standard. This differs
    /// from the scheduler, which falls back to the project default (#131).
    /// This is the weekly pattern alone: calendar exceptions are ignored.
    pub fn calendar_for(&self, task: &Task) -> Week {
        let want = task.calendar_uid.unwrap_or(self.default_calendar_uid);
        match self.calendar(want).or_else(|| self.calendars.first()) {
            Some(cal) => self.resolved_week(cal),
            None => Calendar::standard_week(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_keeps_decimal_text_and_normalizes_finite_float_syntax() {
        for text in [
            "9007199254740993",
            "0.12345678901234567890123456789",
            "+5",
            "-0.50",
            ".5",
            "5.",
            "007",
        ] {
            assert_eq!(Rate::parse(&format!(" {text} ")).unwrap().as_str(), text);
        }
        let huge = format!("1{}", "0".repeat(400));
        assert_eq!(Rate::parse(&huge).unwrap().as_str(), huge);
        for (source, expected) in [("1e3", "1000"), ("1.5E2", "150")] {
            assert_eq!(Rate::parse(source).unwrap().as_str(), expected);
        }
        assert_eq!(Rate::parse("\t\r\n5 \n").unwrap().as_str(), "5");
        for text in [
            "",
            ".",
            "+",
            "abc",
            "NaN",
            "inf",
            "-infinity",
            "1e999",
            "\u{a0}5\u{a0}",
            "5\u{2003}",
            "\u{3000}1e3",
        ] {
            assert_eq!(Rate::parse(text), None, "{text}");
        }
    }

    #[test]
    fn rate_from_f64_accepts_only_finite_values() {
        assert_eq!(
            Rate::from_f64(1e20).unwrap().as_str(),
            "100000000000000000000"
        );
        assert_eq!(Rate::from_f64(-0.0).unwrap().as_str(), "-0");
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(Rate::from_f64(value), None);
        }
    }

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
    fn task_type_codes_round_trip() {
        for c in 0..=2 {
            assert_eq!(TaskType::from_code(c).unwrap().code(), c);
        }
        assert!(TaskType::from_code(3).is_none());
        assert!(TaskType::from_code(-1).is_none());
    }

    #[test]
    fn blank_rows_are_outside_the_outline() {
        let row = |outline_level, is_null| Task {
            outline_level,
            is_null,
            ..Task::default()
        };
        let proj = Project {
            // Phase, a blank row, its child, a deeper blank row, a sibling.
            tasks: vec![
                row(1, false),
                row(0, true),
                row(2, false),
                row(3, true),
                row(2, false),
            ],
            ..Project::default()
        };
        let summaries: Vec<bool> = (0..5).map(|i| proj.is_outline_summary(i)).collect();
        assert_eq!(summaries, [true, false, false, false, false]);
    }

    #[test]
    fn standard_calendar_is_8h_weekdays() {
        let cal = Calendar::standard(1);
        let week = Project::default().resolved_week(&cal);
        assert_eq!(week[1].minutes(), 480); // Monday
        assert!(!week[0].working()); // Sunday off
        assert!(!week[6].working()); // Saturday off
    }

    fn derived(uid: i32, base: i32, week: [Option<DayWorking>; 7]) -> Calendar {
        Calendar {
            uid,
            name: format!("Derived {uid}"),
            base_calendar_uid: Some(base),
            is_baseline_calendar: false,
            week,
            exceptions: Vec::new(),
        }
    }

    fn hours(from: u32, to: u32) -> DayWorking {
        DayWorking {
            times: vec![WorkingTime {
                from: from * 60,
                to: to * 60,
            }],
        }
    }

    #[test]
    fn derived_week_resolves_transitively_through_its_base_chain() {
        // Standard <- 2 (Friday off) <- 3 (Monday 07-15).
        let mut friday_off: [Option<DayWorking>; 7] = Default::default();
        friday_off[5] = Some(DayWorking::default());
        let mut short_monday: [Option<DayWorking>; 7] = Default::default();
        short_monday[1] = Some(hours(7, 15));
        let proj = Project {
            calendars: vec![
                Calendar::standard(1),
                derived(2, 1, friday_off),
                derived(3, 2, short_monday),
            ],
            ..Project::default()
        };
        let week = proj.resolved_week(&proj.calendars[2]);
        let standard = Calendar::standard_week();
        assert_eq!(week[1], hours(7, 15));
        assert_eq!(week[2], standard[2]);
        assert_eq!(week[4], standard[4]);
        assert!(!week[5].working());
        assert!(!week[0].working() && !week[6].working());
    }

    #[test]
    fn broken_base_chains_end_and_leave_unresolved_days_non_working() {
        let mut monday: [Option<DayWorking>; 7] = Default::default();
        monday[1] = Some(hours(8, 12));
        for calendars in [
            // Missing base.
            vec![derived(2, 99, monday.clone())],
            // Self-reference.
            vec![derived(2, 2, monday.clone())],
            // A longer cycle.
            vec![
                derived(2, 3, monday.clone()),
                derived(3, 2, Default::default()),
            ],
        ] {
            let proj = Project {
                calendars,
                ..Project::default()
            };
            let week = proj.resolved_week(&proj.calendars[0]);
            assert_eq!(week[1], hours(8, 12));
            for day in [0, 2, 3, 4, 5, 6] {
                assert!(!week[day].working(), "day {day}");
            }
        }
    }

    #[test]
    fn calendar_lookup_takes_the_last_calendar_with_a_uid() {
        let mut long = Calendar::standard(1);
        long.week[1] = Some(hours(6, 20));
        let proj = Project {
            calendars: vec![Calendar::standard(1), long.clone()],
            ..Project::default()
        };
        assert_eq!(proj.calendar(1), Some(&long));
        assert_eq!(proj.calendar_for(&Task::default())[1], hours(6, 20));
    }

    #[test]
    fn duration_conversion() {
        let p = Project::default();
        assert_eq!(p.minutes_to_days(960), 2.0); // 16h @ 8h/day
        assert_eq!(p.days_to_minutes(2.0), 960);
    }

    fn march(day: u32) -> i64 {
        DateTime::from_ymd_hm(2026, 3, day, 0, 0).day_number()
    }

    /// A date-range exception over 2026-03-`first`..=`last`.
    fn exception(first: u32, last: u32, day: DayWorking) -> CalendarException {
        CalendarException::date_range(
            DateTime::from_ymd_hm(2026, 3, first, 0, 0),
            DateTime::from_ymd_hm(2026, 3, last, 23, 59),
            day,
        )
    }

    #[test]
    fn derived_calendar_resolves_exceptions_down_the_chain_before_weekdays() {
        // Project 2024's rule, from the #126 probe: Standard takes Wed 4th and
        // Wed 11th off <- Crew states Wednesday 07-15, works 09-11 on the 11th
        // and takes Fri 6th off <- a calendar deriving from Crew states nothing.
        let mut base = Calendar::standard(1);
        base.exceptions.push(exception(4, 4, DayWorking::default()));
        base.exceptions
            .push(exception(11, 11, DayWorking::default()));
        let mut wednesday: [Option<DayWorking>; 7] = Default::default();
        wednesday[3] = Some(hours(7, 15));
        let mut crew = derived(2, 1, wednesday);
        crew.exceptions.push(exception(11, 11, hours(9, 11)));
        crew.exceptions.push(exception(6, 6, DayWorking::default()));
        let proj = Project {
            calendars: vec![base, crew, derived(3, 2, Default::default())],
            ..Project::default()
        };
        for uid in [2, 3] {
            let cal = proj.resolved_calendar(proj.calendar(uid).unwrap());
            // The base's holiday beats Crew's own Wednesday.
            assert!(cal.day(march(4)).is_empty(), "{uid}");
            assert_eq!(cal.day(march(5)), Calendar::standard_week()[4].times);
            assert!(cal.day(march(6)).is_empty(), "{uid}");
            // Crew's own exception beats the base's holiday.
            assert_eq!(cal.day(march(11)), hours(9, 11).times);
            assert_eq!(cal.day(march(18)), hours(7, 15).times);
            assert_eq!(cal.week(), proj.resolved_week(proj.calendar(uid).unwrap()));
        }
        let base = proj.resolved_calendar(&proj.calendars[0]);
        assert!(base.day(march(11)).is_empty());
        assert_eq!(base.day(march(6)), Calendar::standard_week()[5].times);
        assert_eq!(base.day(march(18)), Calendar::standard_week()[3].times);
    }

    #[test]
    fn overlapping_exceptions_resolve_to_the_first_in_file_order() {
        let off = DayWorking::default;
        let mut cal = Calendar::standard(1);
        cal.exceptions = vec![exception(3, 5, off()), exception(4, 6, hours(9, 10))];
        let work = Project::default().resolved_calendar(&cal);
        for day in [3, 4, 5] {
            assert!(work.day(march(day)).is_empty(), "{day}");
        }
        assert_eq!(work.day(march(6)), hours(9, 10).times);
        // A later exception around an earlier one keeps only its outer days.
        cal.exceptions = vec![exception(4, 5, hours(9, 10)), exception(3, 7, off())];
        let work = Project::default().resolved_calendar(&cal);
        assert_eq!(work.day(march(2)), Calendar::standard_week()[1].times);
        assert!(work.day(march(3)).is_empty());
        assert_eq!(work.day(march(4)), hours(9, 10).times);
        assert_eq!(work.day(march(5)), hours(9, 10).times);
        assert!(work.day(march(6)).is_empty());
    }

    #[test]
    fn only_daily_exceptions_schedule_over_whole_dates() {
        let at =
            |day: u32, hour: u32, minute: u32| DateTime::from_ymd_hm(2026, 3, day, hour, minute);
        let one = CalendarException::date_range(at(4, 0, 0), at(4, 0, 0), DayWorking::default());
        // A ToDate at midnight of the FromDate's date is still that one day.
        assert_eq!(one.scheduled(), Some((march(4), march(4))));
        let two = CalendarException {
            to: Some(at(5, 0, 0)),
            ..one.clone()
        };
        assert_eq!(two.scheduled(), Some((march(4), march(5))));
        let backwards = CalendarException {
            to: Some(at(3, 12, 0)),
            ..one.clone()
        };
        assert_eq!(backwards.scheduled(), Some((march(4), march(4))));
        let every = CalendarException {
            period: Some(1),
            ..one.clone()
        };
        assert_eq!(every.scheduled(), Some((march(4), march(4))));
        // Recurrences and incomplete ones are kept but never scheduled.
        let unscheduled = [
            CalendarException {
                kind: Some(2),
                ..one.clone()
            },
            CalendarException {
                kind: Some(6),
                days_of_week: Some(8),
                ..one.clone()
            },
            CalendarException {
                period: Some(2),
                ..one.clone()
            },
            CalendarException {
                kind: None,
                ..one.clone()
            },
            CalendarException {
                from: None,
                ..one.clone()
            },
        ];
        let mut cal = Calendar::standard(1);
        for e in unscheduled {
            assert_eq!(e.scheduled(), None, "{e:?}");
            cal.exceptions.push(e);
        }
        let work = Project::default().resolved_calendar(&cal);
        assert_eq!(work.day(march(4)), Calendar::standard_week()[3].times);
    }

    #[test]
    fn union_merges_each_date_and_weekly_pattern_ignores_exceptions() {
        let mut short = Calendar::standard(1);
        short.exceptions.push(exception(4, 4, hours(9, 10)));
        let mut late = Calendar::standard(2);
        late.exceptions.push(exception(4, 4, hours(9, 15)));
        let proj = Project::default();
        let union = WorkCalendar::union(vec![
            proj.resolved_calendar(&short),
            proj.resolved_calendar(&late),
        ]);
        assert_eq!(union.day(march(4)), hours(9, 15).times);
        assert_eq!(union.day(march(3)), Calendar::standard_week()[2].times);
        assert_eq!(union.week(), Calendar::standard_week());
        assert!(union.has_working_time());
        // A working exception on an otherwise empty calendar is still no
        // weekly working time.
        let mut closed = Calendar::base(3, "Closed", Default::default());
        closed.exceptions.push(exception(4, 4, hours(8, 17)));
        let closed = proj.resolved_calendar(&closed);
        assert_eq!(closed.day(march(4)), hours(8, 17).times);
        assert!(!closed.has_working_time());
    }
}
