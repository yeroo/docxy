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
            tasks: Vec::new(),
            resources: Vec::new(),
            assignments: Vec::new(),
            calendars: vec![Calendar::standard(1)],
        }
    }
}

impl Project {
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
