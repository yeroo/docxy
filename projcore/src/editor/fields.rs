//! Task fields by Project's own names, each read both as the sheet shows it
//! and as the value underneath. The grid's Entry columns read through the
//! same registry, so a column and a field read of it cannot disagree.
//!
//! `text` uses Project's US spellings (`1 day`, `4 hrs`, `$1,400.00`,
//! `($40,000.00)`, `50%`, `Yes`), except for the Entry columns, which keep
//! the grid's (`2d`, `2026-03-02`). Dates show `YYYY-MM-DD`, `NA` when unset.
//!
//! `value` is [`FieldValue::Null`] only for a date that shows `NA`, for a
//! custom value that does not parse (its text is the stored text), for a
//! stored value the plan does not have (percents, actuals, remaining
//! values, work, cost, fixed cost, baseline values, notes, hyperlink parts),
//! and for Status
//! when the plan has no StatusDate or CurrentDate (or the task no start), so a
//! test can tell it from `0`; such a field's `text` is what Project shows for
//! it (`0%`, `0 days`, `0 hrs`, `$0.00`), empty for Status. Fields with a
//! Project default read that
//! default in both: Active (Yes), Priority (500), Effort Driven and Type (the
//! plan's defaults for new tasks), Fixed Cost Accrual (Prorated, as costs
//! accrue when it is absent or Invalid), Constraint Type (As Soon As
//! Possible), Leveling Delay (0 edays). WBS is the stored code, else the outline
//! number; edits do not renumber a stored code yet. Estimated and Milestone are what the
//! grid shows (a summary is estimated when a leaf below it is; a zero-length
//! leaf is a milestone). A blank row reads only its ID and Unique ID; every
//! other field is empty text and null.
//!
//! Custom fields (Text1-30, Number1-20, Cost1-10, Flag1-20 and
//! Date/Start/Finish/Duration1-10) resolve through the plan's own
//! `<ExtendedAttribute>` definitions: the definition whose `FieldName` is the
//! field's name says which `FieldID` holds the value, so no id is hard-coded.
//! A field with no definition in the plan, or no task value with that id,
//! reads unset (`""`, `0`, `$0.00`, `No`/`false`, `NA`, `0 days` by kind);
//! a stored value that does not parse reads as its raw text and Null.
//!
//! Units: a task's own durations (Actual and Remaining Duration, Duration
//! Variance) show in the unit its Duration was entered in, elapsed included;
//! a Baseline Duration shows in the baseline's own `DurationFormat` when it
//! has one (with `?` when that format is estimated), else in the task's
//! unit; working-time measures (slack, Start and Finish Variance) show in
//! the task unit's working form. Work shows in hours. Values are signed minutes;
//! money is a decimal number of currency units (MSPDI stores hundredths).
//!
//! Variances are computed from the live schedule, not the variances a file
//! stores, which go stale on every edit. They measure the scheduled (CPM,
//! unleveled) dates, which a save writes and Set Baseline records; while
//! leveling is on, the grid's Start and Finish can differ from them. Start
//! and Finish Variance are the working minutes, on the task's calendar, from
//! the Baseline date to the scheduled one (negative when early; 0 without a
//! Baseline date), and Duration, Work and Cost Variance are the current value
//! less the Baseline one, an absent Baseline value counting as 0.
//!
//! Status is Project's own derivation, measured at the plan's StatusDate,
//! else its CurrentDate, else empty: Complete at % Complete = 100; Future
//! Task before the task's Start (minute precision); Late when the status
//! date's calendar day is after the resume point's — a started task's stored
//! resume snapped to the next working start on its calendar, a 0% task's raw
//! Start; On Schedule otherwise.

use super::*;
use crate::model::{AccrueAt, Rate, outline_numbers};
use crate::schedule::working_minutes_between_on;
use std::cell::OnceCell;
use std::collections::HashMap;

/// One readable task field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Id,
    TaskMode,
    Name,
    Duration,
    Start,
    Finish,
    Predecessors,
    ResourceNames,
    PercentComplete,
    PercentWorkComplete,
    PhysicalPercentComplete,
    ActualStart,
    ActualFinish,
    ActualDuration,
    RemainingDuration,
    Work,
    ActualWork,
    RemainingWork,
    Cost,
    ActualCost,
    RemainingCost,
    FixedCost,
    FixedCostAccrual,
    /// A Baseline slot: 0 is Baseline, 1..=10 are Baseline1..Baseline10.
    Baseline(u8, BaselinePart),
    /// A custom field slot: Text1-30, Number1-20, Cost1-10, Flag1-20 and
    /// Date/Start/Finish/Duration1-10 (`n` is 1-based), resolved through the
    /// plan's own `<ExtendedAttribute>` definitions.
    Custom(CustomKind, u8),
    StartVariance,
    FinishVariance,
    DurationVariance,
    WorkVariance,
    CostVariance,
    TotalSlack,
    FreeSlack,
    StartSlack,
    FinishSlack,
    EarlyStart,
    EarlyFinish,
    LateStart,
    LateFinish,
    Critical,
    ConstraintType,
    ConstraintDate,
    Deadline,
    Active,
    OutlineNumber,
    OutlineLevel,
    Wbs,
    LevelingDelay,
    Type,
    EffortDriven,
    Priority,
    Notes,
    Hyperlink,
    HyperlinkAddress,
    HyperlinkSubAddress,
    Milestone,
    Summary,
    Estimated,
    Status,
    UniqueId,
}

/// The part of a Baseline slot a field reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BaselinePart {
    Start,
    Finish,
    Duration,
    Work,
    Cost,
}

/// The family of a custom (user-defined) task field (`#577`). Project
/// numbers each family separately: Text1-30, Number1-20, Cost1-10, Flag1-20
/// and Date/Start/Finish/Duration1-10.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CustomKind {
    Text,
    Number,
    Cost,
    Flag,
    Date,
    Start,
    Finish,
    Duration,
}

/// Every custom family: its kind, name prefix and highest number.
const CUSTOM_KINDS: [(CustomKind, &str, u8); 8] = [
    (CustomKind::Text, "Text", 30),
    (CustomKind::Number, "Number", 20),
    (CustomKind::Cost, "Cost", 10),
    (CustomKind::Flag, "Flag", 20),
    (CustomKind::Date, "Date", 10),
    (CustomKind::Start, "Start", 10),
    (CustomKind::Finish, "Finish", 10),
    (CustomKind::Duration, "Duration", 10),
];

const BASELINE_PARTS: [(BaselinePart, &str); 5] = [
    (BaselinePart::Start, "Start"),
    (BaselinePart::Finish, "Finish"),
    (BaselinePart::Duration, "Duration"),
    (BaselinePart::Work, "Work"),
    (BaselinePart::Cost, "Cost"),
];

/// The grid's Entry columns, left to right.
pub const ENTRY_FIELDS: [Field; 8] = [
    Field::Id,
    Field::TaskMode,
    Field::Name,
    Field::Duration,
    Field::Start,
    Field::Finish,
    Field::Predecessors,
    Field::ResourceNames,
];

/// The fields before the Baseline family, in registry order.
const HEAD: &[(Field, &str)] = &[
    (Field::Id, "ID"),
    (Field::TaskMode, "Task Mode"),
    (Field::Name, "Name"),
    (Field::Duration, "Duration"),
    (Field::Start, "Start"),
    (Field::Finish, "Finish"),
    (Field::Predecessors, "Predecessors"),
    (Field::ResourceNames, "Resource Names"),
    (Field::PercentComplete, "% Complete"),
    (Field::PercentWorkComplete, "% Work Complete"),
    (Field::PhysicalPercentComplete, "Physical % Complete"),
    (Field::ActualStart, "Actual Start"),
    (Field::ActualFinish, "Actual Finish"),
    (Field::ActualDuration, "Actual Duration"),
    (Field::RemainingDuration, "Remaining Duration"),
    (Field::Work, "Work"),
    (Field::ActualWork, "Actual Work"),
    (Field::RemainingWork, "Remaining Work"),
    (Field::Cost, "Cost"),
    (Field::ActualCost, "Actual Cost"),
    (Field::RemainingCost, "Remaining Cost"),
    (Field::FixedCost, "Fixed Cost"),
    (Field::FixedCostAccrual, "Fixed Cost Accrual"),
];

/// The fields after the Baseline family, in registry order.
const TAIL: &[(Field, &str)] = &[
    (Field::StartVariance, "Start Variance"),
    (Field::FinishVariance, "Finish Variance"),
    (Field::DurationVariance, "Duration Variance"),
    (Field::WorkVariance, "Work Variance"),
    (Field::CostVariance, "Cost Variance"),
    (Field::TotalSlack, "Total Slack"),
    (Field::FreeSlack, "Free Slack"),
    (Field::StartSlack, "Start Slack"),
    (Field::FinishSlack, "Finish Slack"),
    (Field::EarlyStart, "Early Start"),
    (Field::EarlyFinish, "Early Finish"),
    (Field::LateStart, "Late Start"),
    (Field::LateFinish, "Late Finish"),
    (Field::Critical, "Critical"),
    (Field::ConstraintType, "Constraint Type"),
    (Field::ConstraintDate, "Constraint Date"),
    (Field::Deadline, "Deadline"),
    (Field::Active, "Active"),
    (Field::OutlineNumber, "Outline Number"),
    (Field::OutlineLevel, "Outline Level"),
    (Field::Wbs, "WBS"),
    (Field::LevelingDelay, "Leveling Delay"),
    (Field::Type, "Type"),
    (Field::EffortDriven, "Effort Driven"),
    (Field::Priority, "Priority"),
    (Field::Notes, "Notes"),
    (Field::Hyperlink, "Hyperlink"),
    (Field::HyperlinkAddress, "Hyperlink Address"),
    (Field::HyperlinkSubAddress, "Hyperlink SubAddress"),
    (Field::Milestone, "Milestone"),
    (Field::Summary, "Summary"),
    (Field::Estimated, "Estimated"),
    (Field::Status, "Status"),
    (Field::UniqueId, "Unique ID"),
];

impl Field {
    /// Every readable field, in registry order.
    pub fn all() -> Vec<Field> {
        let baselines = (0..=10u8).flat_map(|n| {
            BASELINE_PARTS
                .iter()
                .map(move |&(part, _)| Field::Baseline(n, part))
        });
        let customs = CUSTOM_KINDS
            .iter()
            .flat_map(|&(kind, _, max)| (1..=max).map(move |n| Field::Custom(kind, n)));
        HEAD.iter()
            .map(|&(f, _)| f)
            .chain(baselines)
            .chain(TAIL.iter().map(|&(f, _)| f))
            .chain(customs)
            .collect()
    }

    /// Project's name for the field (`% Complete`, `Baseline3 Finish`).
    pub fn name(self) -> String {
        if let Field::Baseline(n, part) = self {
            let part = BASELINE_PARTS
                .iter()
                .find(|(p, _)| *p == part)
                .map_or("", |(_, name)| name);
            return if n == 0 {
                format!("Baseline {part}")
            } else {
                format!("Baseline{n} {part}")
            };
        }
        if let Field::Custom(kind, n) = self {
            let prefix = CUSTOM_KINDS
                .iter()
                .find(|(k, _, _)| *k == kind)
                .map_or("", |&(_, prefix, _)| prefix);
            return format!("{prefix}{n}");
        }
        HEAD.iter()
            .chain(TAIL)
            .find(|(f, _)| *f == self)
            .map_or_else(String::new, |(_, name)| (*name).to_string())
    }

    /// The field with this name, trimmed and matched without regard to ASCII
    /// case; the error names it as given.
    pub fn parse(name: &str) -> Result<Field, String> {
        let wanted = name.trim();
        Field::all()
            .into_iter()
            .find(|f| f.name().eq_ignore_ascii_case(wanted))
            .ok_or_else(|| format!("unknown task field '{name}'"))
    }
}

/// Every readable field name, in registry order.
pub fn field_names() -> Vec<String> {
    Field::all().into_iter().map(Field::name).collect()
}

/// The value under a field.
#[derive(Clone, PartialEq, Debug)]
pub enum FieldValue {
    Null,
    Bool(bool),
    Int(i64),
    /// Signed minutes of duration, work or slack.
    Minutes(i64),
    /// Currency units (not hundredths).
    Money(f64),
    /// A plain number: assignment units (1.0 = 100%) or a material's quantity.
    Number(f64),
    Date(DateTime),
    Text(String),
}

/// A field as the sheet shows it and the value underneath.
#[derive(Clone, PartialEq, Debug)]
pub struct FieldRead {
    pub text: String,
    pub value: FieldValue,
}

impl FieldRead {
    fn new(text: impl Into<String>, value: FieldValue) -> FieldRead {
        FieldRead {
            text: text.into(),
            value,
        }
    }
}

/// Reads fields of one editor's tasks, computing outline numbers once, on
/// the first read that needs them.
pub struct FieldReader<'a> {
    ed: &'a Editor,
    outline: OnceCell<HashMap<i32, String>>,
}

impl<'a> FieldReader<'a> {
    pub fn new(ed: &'a Editor) -> FieldReader<'a> {
        FieldReader {
            ed,
            outline: OnceCell::new(),
        }
    }

    /// A task's outline number (`1.2`), `0` for the project summary row;
    /// `None` for a blank row or an unknown task.
    pub fn outline_number(&self, uid: i32) -> Option<&str> {
        self.outline
            .get_or_init(|| {
                let tasks = &self.ed.project().tasks;
                tasks
                    .iter()
                    .zip(outline_numbers(tasks))
                    .filter_map(|(t, n)| Some((t.uid, n?)))
                    .collect()
            })
            .get(&uid)
            .map(String::as_str)
    }

    pub fn read(&self, task: &Task, field: Field) -> FieldRead {
        if task.is_null && !matches!(field, Field::Id | Field::UniqueId) {
            return FieldRead::new("", FieldValue::Null);
        }
        let ed = self.ed;
        let proj = ed.project();
        let own = DurationUnit::of_task(task);
        let working = own.working();
        let result = ed.schedule().get(task.uid);
        let baseline = task.baseline(0);
        match field {
            Field::Id => int(i64::from(task.id)),
            Field::TaskMode => text(task_mode_name(task.manual)),
            Field::Name => text(&task.name),
            Field::Duration => entry_duration(ed, task),
            Field::Start => date(ed.disp_start(task.uid)),
            Field::Finish => date(ed.disp_finish(task.uid)),
            Field::Predecessors => text(format_predecessors(task, proj)),
            Field::ResourceNames => text(format_resource_names(proj, task.uid)),
            Field::PercentComplete => percent(task.percent_complete),
            Field::PercentWorkComplete => percent(task.percent_work_complete),
            Field::PhysicalPercentComplete => percent(task.physical_percent_complete),
            Field::ActualStart => date(task.actual_start),
            Field::ActualFinish => date(task.actual_finish),
            Field::ActualDuration => duration(proj, task.actual_duration_min, own),
            Field::RemainingDuration => duration(proj, task.remaining_duration_min, own),
            Field::Work => work(task.work_min),
            Field::ActualWork => work(task.actual_work_min),
            Field::RemainingWork => work(task.remaining_work_min),
            Field::Cost => money(task.cost.as_ref()),
            Field::ActualCost => money(task.actual_cost.as_ref()),
            Field::RemainingCost => money(task.remaining_cost.as_ref()),
            Field::FixedCost => money(task.fixed_cost.as_ref()),
            Field::FixedCostAccrual => text(match task.fixed_cost_accrual {
                Some(AccrueAt::Start) => "Start",
                Some(AccrueAt::End) => "End",
                Some(AccrueAt::Prorated | AccrueAt::Invalid) | None => "Prorated",
            }),
            Field::Baseline(n, part) => {
                let b = task.baseline(n);
                match part {
                    BaselinePart::Start => date(b.and_then(|b| b.start)),
                    BaselinePart::Finish => date(b.and_then(|b| b.finish)),
                    BaselinePart::Duration => {
                        let min = b.and_then(|b| b.duration_min);
                        match b.and_then(|b| b.duration_format) {
                            Some(code) => {
                                let unit = DurationUnit::of_code(code).unwrap_or(own);
                                let estimated = LagFormat::from_code(i64::from(code))
                                    .is_some_and(LagFormat::estimated);
                                FieldRead::new(
                                    format_duration_field(proj, min.unwrap_or(0), unit, estimated),
                                    minutes(min),
                                )
                            }
                            None => duration(proj, min, own),
                        }
                    }
                    BaselinePart::Work => work(b.and_then(|b| b.work_min)),
                    BaselinePart::Cost => money(b.and_then(|b| b.cost.as_ref())),
                }
            }
            // Variances measure the schedule a save writes (and Set Baseline
            // records), not the leveled view; a task without a schedule
            // result falls back to what the grid shows.
            Field::StartVariance => date_variance(
                proj,
                task,
                result
                    .map(|r| r.early_start)
                    .or_else(|| ed.disp_start(task.uid)),
                baseline.and_then(|b| b.start),
                working,
            ),
            Field::FinishVariance => date_variance(
                proj,
                task,
                result
                    .map(|r| r.early_finish)
                    .or_else(|| ed.disp_finish(task.uid)),
                baseline.and_then(|b| b.finish),
                working,
            ),
            Field::DurationVariance => {
                let current = crate::schedule::task_duration_min(proj, ed.schedule(), task)
                    .or_else(|| ed.disp_duration_min(task.uid))
                    .unwrap_or(task.duration_min);
                let min = current - baseline.and_then(|b| b.duration_min).unwrap_or(0);
                duration(proj, Some(min), own)
            }
            Field::WorkVariance => work(Some(
                task.work_min.unwrap_or(0) - baseline.and_then(|b| b.work_min).unwrap_or(0),
            )),
            Field::CostVariance => {
                let current = hundredths(task.cost.as_ref());
                let base = hundredths(baseline.and_then(|b| b.cost.as_ref()));
                money_value(Some(current - base))
            }
            Field::TotalSlack => duration(proj, result.map(|r| r.total_slack_min), working),
            Field::FreeSlack => duration(proj, result.map(|r| r.free_slack_min), working),
            Field::StartSlack => duration(proj, result.map(|r| r.start_slack_min), working),
            Field::FinishSlack => duration(proj, result.map(|r| r.finish_slack_min), working),
            Field::EarlyStart => date(result.map(|r| r.early_start)),
            Field::EarlyFinish => date(result.map(|r| r.early_finish)),
            Field::LateStart => date(result.map(|r| r.late_start)),
            Field::LateFinish => date(result.map(|r| r.late_finish)),
            Field::Critical => flag(result.is_some_and(|r| r.critical)),
            Field::ConstraintType => text(constraint_name(task.constraint)),
            Field::ConstraintDate => date(task.constraint_date),
            Field::Deadline => date(task.deadline),
            Field::Active => flag(task.is_active()),
            Field::OutlineNumber => optional_text(self.outline_number(task.uid)),
            Field::OutlineLevel => int(i64::from(task.outline_level)),
            // The stored code (a file's, or an MPP override), else the outline
            // number. Structural edits do not renumber a stored code yet, and
            // a save writes the same one, so the field shows what the plan
            // holds.
            Field::Wbs => optional_text(task.wbs.as_deref().or(self.outline_number(task.uid))),
            Field::LevelingDelay => {
                // Stored in tenths of a minute; Project shows it in elapsed
                // days unless the file gives another format.
                let min = task.leveling_delay.unwrap_or(0) / 10;
                let unit = task
                    .leveling_delay_format
                    .and_then(|code| u8::try_from(code).ok())
                    .and_then(DurationUnit::of_code)
                    .unwrap_or(DurationUnit::ELAPSED_DAYS);
                FieldRead::new(
                    format_duration_field(proj, min, unit, false),
                    FieldValue::Minutes(min),
                )
            }
            Field::Type => text(task_type_name(
                task.task_type.unwrap_or(proj.default_task_type()),
            )),
            Field::EffortDriven => {
                flag(task.effort_driven.unwrap_or(proj.new_tasks_effort_driven()))
            }
            Field::Priority => int(i64::from(task.priority.unwrap_or(500))),
            Field::Notes => optional_text(task.notes.as_deref()),
            Field::Hyperlink => optional_text(task.hyperlink.as_deref()),
            Field::HyperlinkAddress => optional_text(task.hyperlink_address.as_deref()),
            Field::HyperlinkSubAddress => optional_text(task.hyperlink_sub_address.as_deref()),
            // A summary's stored duration is stale and may be 0, so only a
            // leaf is a milestone by length, as the grid's Duration reads it.
            Field::Milestone => flag(task.milestone || (!task.summary && task.duration_min == 0)),
            Field::Summary => flag(task.summary),
            Field::Estimated => flag(!duration_suffix(proj, task.uid).is_empty()),
            Field::Status => status(ed, task),
            Field::UniqueId => int(i64::from(task.uid)),
            Field::Custom(kind, n) => custom::read(proj, task, kind, n),
        }
    }
}

/// A task's mode as Project's Task Mode column shows it.
pub fn task_mode_name(manual: bool) -> &'static str {
    if manual {
        "Manually Scheduled"
    } else {
        "Auto Scheduled"
    }
}

fn constraint_name(c: ConstraintType) -> &'static str {
    match c {
        ConstraintType::AsSoonAsPossible => "As Soon As Possible",
        ConstraintType::AsLateAsPossible => "As Late As Possible",
        ConstraintType::MustStartOn => "Must Start On",
        ConstraintType::MustFinishOn => "Must Finish On",
        ConstraintType::StartNoEarlierThan => "Start No Earlier Than",
        ConstraintType::StartNoLaterThan => "Start No Later Than",
        ConstraintType::FinishNoEarlierThan => "Finish No Earlier Than",
        ConstraintType::FinishNoLaterThan => "Finish No Later Than",
    }
}

fn task_type_name(t: TaskType) -> &'static str {
    match t {
        TaskType::FixedUnits => "Fixed Units",
        TaskType::FixedDuration => "Fixed Duration",
        TaskType::FixedWork => "Fixed Work",
    }
}

fn int(n: i64) -> FieldRead {
    FieldRead::new(n.to_string(), FieldValue::Int(n))
}

fn text(s: impl Into<String>) -> FieldRead {
    let s = s.into();
    FieldRead::new(s.clone(), FieldValue::Text(s))
}

/// Status as Project computes it, at the plan's status date (see
/// [`Project::status_date`]); `("", Null)` when the plan has no StatusDate or
/// CurrentDate, or the task has no start. The resume point is a started
/// task's stored `resume` (else `stop`, else start) snapped to the next
/// working start on its calendar, or a 0% task's raw start.
fn status(ed: &Editor, task: &Task) -> FieldRead {
    let proj = ed.project();
    let Some(status_date) = proj.status_date() else {
        return FieldRead::new("", FieldValue::Null);
    };
    let Some(start) = ed.disp_start(task.uid).or(task.stored_start) else {
        return FieldRead::new("", FieldValue::Null);
    };
    let resume_point = match task.percent_complete {
        Some(0) | None => start,
        Some(_) => {
            let r = task.resume.or(task.stop).unwrap_or(start);
            crate::assign::advance(&crate::assign::task_calendar(proj, task), r, 0, false)
                .unwrap_or(r)
        }
    };
    text(task_status(
        task.percent_complete,
        start,
        resume_point,
        status_date,
    ))
}

/// Project's Status rule, from the x-status oracle
/// (`corpus/mpp/snapshots/x-status-sweep.json`, key `transitions`): Complete
/// at % Complete = 100, whatever the status date; Future Task before the
/// task's Start (minute precision); Late when the status date's calendar day
/// is after the resume point's; On Schedule otherwise. Days compare as
/// calendar days, not working time.
fn task_status(
    percent: Option<u8>,
    start: DateTime,
    resume_point: DateTime,
    status_date: DateTime,
) -> &'static str {
    if percent == Some(100) {
        return "Complete";
    }
    if status_date < start {
        return "Future Task";
    }
    if status_date.day_number() > resume_point.day_number() {
        return "Late";
    }
    "On Schedule"
}

fn optional_text(s: Option<&str>) -> FieldRead {
    match s {
        Some(s) => text(s),
        None => FieldRead::new("", FieldValue::Null),
    }
}

fn flag(b: bool) -> FieldRead {
    FieldRead::new(if b { "Yes" } else { "No" }, FieldValue::Bool(b))
}

fn percent(p: Option<u8>) -> FieldRead {
    FieldRead::new(
        format!("{}%", p.unwrap_or(0)),
        p.map_or(FieldValue::Null, |p| FieldValue::Int(i64::from(p))),
    )
}

fn minutes(min: Option<i64>) -> FieldValue {
    min.map_or(FieldValue::Null, FieldValue::Minutes)
}

fn date(dt: Option<DateTime>) -> FieldRead {
    FieldRead::new(
        format_date_field(dt),
        dt.map_or(FieldValue::Null, FieldValue::Date),
    )
}

fn duration(proj: &Project, min: Option<i64>, unit: DurationUnit) -> FieldRead {
    FieldRead::new(
        format_duration_field(proj, min.unwrap_or(0), unit, false),
        minutes(min),
    )
}

fn work(min: Option<i64>) -> FieldRead {
    FieldRead::new(format_work(min.unwrap_or(0)), minutes(min))
}

fn hundredths(rate: Option<&Rate>) -> f64 {
    rate.and_then(Rate::to_f64).unwrap_or(0.0)
}

fn money(rate: Option<&Rate>) -> FieldRead {
    money_value(rate.map(|r| hundredths(Some(r))))
}

/// Money from hundredths, as MSPDI stores every task cost.
fn money_value(hundredths: Option<f64>) -> FieldRead {
    let units = hundredths.map(|h| h / 100.0);
    FieldRead::new(
        format_money(units.unwrap_or(0.0)),
        units.map_or(FieldValue::Null, FieldValue::Money),
    )
}

/// The Entry table's Duration: a summary's rolled-up span in days (`?` when
/// it has no schedule), else the task's own duration in the unit it was
/// entered in, a milestone's zero included. The estimate's `?` comes from
/// `duration_suffix`, which shows none for a milestone.
fn entry_duration(ed: &Editor, task: &Task) -> FieldRead {
    let proj = ed.project();
    // Summaries first: their stored duration is stale (and may be 0, which
    // `is_milestone` would misread), so derive it from the shown dates.
    if task.summary {
        return match ed.disp_duration_min(task.uid) {
            Some(min) => FieldRead::new(
                grid_days(proj, min) + duration_suffix(proj, task.uid),
                FieldValue::Minutes(min),
            ),
            None => FieldRead::new("?", FieldValue::Null),
        };
    }
    let min = FieldValue::Minutes(task.duration_min);
    let shown = task
        .duration_unit()
        .and_then(|unit| proj.format_in_unit(task.duration_min, unit, 1))
        .unwrap_or_else(|| grid_days(proj, task.duration_min));
    FieldRead::new(shown + duration_suffix(proj, task.uid), min)
}

/// Days as the grid's Duration column shows them: `2d`, `1.5d`.
fn grid_days(proj: &Project, min: i64) -> String {
    let d = proj.minutes_to_days(min);
    if (d.round() - d).abs() < 1e-9 {
        format!("{d:.0}d")
    } else {
        format!("{d:.1}d")
    }
}

/// Start or Finish Variance: working minutes from the Baseline date to the
/// scheduled one on the task's calendar, negative when the scheduled date is
/// earlier.
fn date_variance(
    proj: &Project,
    task: &Task,
    scheduled: Option<DateTime>,
    baseline: Option<DateTime>,
    unit: DurationUnit,
) -> FieldRead {
    let min = match (scheduled, baseline) {
        (_, None) => Some(0),
        (None, Some(_)) => None,
        (Some(scheduled), Some(base)) => Some(if scheduled >= base {
            working_minutes_between_on(proj, task.calendar_uid, base, scheduled)
        } else {
            -working_minutes_between_on(proj, task.calendar_uid, scheduled, base)
        }),
    };
    duration(proj, min, unit)
}

/// The unit a duration shows in: a working unit, or an elapsed one (`edays`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct DurationUnit {
    unit: LagUnit,
    elapsed: bool,
}

impl DurationUnit {
    const DAYS: DurationUnit = DurationUnit {
        unit: LagUnit::Day,
        elapsed: false,
    };
    const ELAPSED_DAYS: DurationUnit = DurationUnit {
        unit: LagUnit::Day,
        elapsed: true,
    };

    /// The unit of an MSPDI duration format code; `None` for a percent, null
    /// or unknown code.
    fn of_code(code: u8) -> Option<DurationUnit> {
        let format = LagFormat::from_code(i64::from(code))?;
        match format.kind() {
            LagKind::Working => Some(DurationUnit {
                unit: format.unit(),
                elapsed: false,
            }),
            LagKind::Elapsed => Some(DurationUnit {
                unit: format.unit(),
                elapsed: true,
            }),
            LagKind::Percent => None,
        }
    }

    /// The unit a task's Duration was entered in, days by default.
    fn of_task(task: &Task) -> DurationUnit {
        task.duration_format
            .and_then(DurationUnit::of_code)
            .unwrap_or(DurationUnit::DAYS)
    }

    /// The same unit counting working time.
    fn working(self) -> DurationUnit {
        DurationUnit {
            elapsed: false,
            ..self
        }
    }

    fn minutes(self, proj: &Project) -> f64 {
        if self.elapsed {
            super::cells::elapsed_unit_min(self.unit).unwrap_or(1440.0)
        } else {
            proj.working_unit_min(self.unit)
                .or_else(|| proj.working_unit_min(LagUnit::Day))
                .unwrap_or(480.0)
        }
    }

    fn words(self) -> (&'static str, &'static str) {
        match (self.unit, self.elapsed) {
            (LagUnit::Minute, false) => ("min", "mins"),
            (LagUnit::Minute, true) => ("emin", "emins"),
            (LagUnit::Hour, false) => ("hr", "hrs"),
            (LagUnit::Hour, true) => ("ehr", "ehrs"),
            (LagUnit::Week, false) => ("wk", "wks"),
            (LagUnit::Week, true) => ("ewk", "ewks"),
            (LagUnit::Month, false) => ("mon", "mons"),
            (LagUnit::Month, true) => ("emon", "emons"),
            (LagUnit::Day | LagUnit::Percent, false) => ("day", "days"),
            (LagUnit::Day | LagUnit::Percent, true) => ("eday", "edays"),
        }
    }
}

/// A number with up to two decimals, trailing zeros dropped, and whether it
/// is exactly one either way (`1 day`, `-1 day`).
fn two_decimals(value: f64) -> (String, bool) {
    let rounded = (value * 100.0).round() / 100.0 + 0.0;
    let mut s = format!("{rounded:.2}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    (s, rounded.abs() == 1.0)
}

/// A duration as Project shows it: `0 days`, `1 day`, `-1 day`, `1.25 days`,
/// `12 days?`, `4 hrs`, `2 wks`, `2 edays`.
fn format_duration_field(proj: &Project, min: i64, unit: DurationUnit, estimated: bool) -> String {
    let (number, one) = two_decimals(min as f64 / unit.minutes(proj));
    let (singular, plural) = unit.words();
    let word = if one { singular } else { plural };
    let mark = if estimated { "?" } else { "" };
    format!("{number} {word}{mark}")
}

/// Work in hours, as Project shows it: `0 hrs`, `1 hr`, `1.5 hrs`.
fn format_work(min: i64) -> String {
    let (number, one) = two_decimals(min as f64 / 60.0);
    format!("{number} {}", if one { "hr" } else { "hrs" })
}

/// Money as Project shows it in US currency: `$1,400.00`, `($40,000.00)`,
/// `$0.00`.
fn format_money(units: f64) -> String {
    let cents = (units * 100.0).round() as i128;
    let abs = cents.unsigned_abs();
    let whole = (abs / 100).to_string();
    let mut grouped = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let shown = format!("${grouped}.{:02}", abs % 100);
    if cents < 0 {
        format!("({shown})")
    } else {
        shown
    }
}

/// A date as the grid shows it, `YYYY-MM-DD`, or `NA` when unset.
pub fn format_date_field(dt: Option<DateTime>) -> String {
    dt.map_or_else(
        || "NA".into(),
        |d| {
            let p = d.parts();
            format!("{:04}-{:02}-{:02}", p.year, p.month, p.day)
        },
    )
}

pub(super) mod assignment;
mod custom;

#[cfg(test)]
mod tests;
