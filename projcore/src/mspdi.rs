//! Reader for MS Project's **MSPDI** interchange format (`.xml`).
//!
//! MSPDI is Microsoft's documented open schema — the format Project produces
//! via *Save As → XML*. It is the interop bridge for `projcore`: we never need
//! to touch the undocumented binary `.mpp` to exchange schedules with anyone
//! who owns Project. This module reads the subset the scheduler needs; unknown
//! elements are skipped whole, so a full Project export is tolerated even
//! though we only pull the fields we model.
//!
//! Units, the way MSPDI encodes them (each a classic trap):
//! - Durations/Work are ISO-8601 (`PT16H0M0S`) — converted to **minutes**.
//! - `LinkLag` is **tenths of a minute**, regardless of `LagFormat`.
//! - `MinutesPerDay` sets the days↔minutes display factor.

use crate::datetime::DateTime;
use crate::model::*;
use crate::schedule::TaskResult;
use opccore::xml::{Event, XmlParser};

/// Parse an MSPDI document into a [`Project`].
pub fn read_mspdi(xml: &str) -> Result<Project, String> {
    let mut p = XmlParser::new(xml);
    // Check the document kind before interpreting fields. This is deliberately
    // a root check, not full XML well-formedness validation.
    loop {
        match p.next() {
            Event::Start if p.name() == "Project" => break,
            Event::Start => {
                return Err(format!(
                    "not an MSPDI document: root element is <{}>, expected <Project>",
                    p.name()
                ));
            }
            Event::Eof => return Err("not an MSPDI document: expected <Project> root".into()),
            _ => {}
        }
    }
    let mut proj = Project {
        calendars: Vec::new(),
        ..Project::default()
    };
    let mut task_uids = Vec::new();
    let mut minutes_per_day: Option<f64> = None;
    let mut minutes_per_week: Option<f64> = None;

    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    // Root: descend into it rather than skipping.
                    "Project" => {}
                    "Name" => proj.name = text_of(&mut p),
                    "Title" => proj.title = text_of(&mut p),
                    "StartDate" => proj.start_date = DateTime::parse_mspdi(&text_of(&mut p)),
                    "HonorConstraints" => proj.honor_constraints = bool_of(&mut p),
                    "NewTasksAreManual" => proj.new_tasks_are_manual = bool_of(&mut p),
                    "MinutesPerDay" => minutes_per_day = text_of(&mut p).trim().parse().ok(),
                    "MinutesPerWeek" => minutes_per_week = text_of(&mut p).trim().parse().ok(),
                    // Some emitters use HoursPerDay directly; honor it too.
                    "HoursPerDay" => {
                        if let Ok(h) = text_of(&mut p).trim().parse::<f64>() {
                            minutes_per_day = Some(h * 60.0);
                        }
                    }
                    "CalendarUID" => {
                        if let Ok(u) = text_of(&mut p).trim().parse() {
                            proj.default_calendar_uid = u;
                        }
                    }
                    "Tasks" => parse_tasks(&mut p, &mut proj.tasks, &mut task_uids),
                    "Resources" => parse_resources(&mut p, &mut proj.resources),
                    "Assignments" => parse_assignments(&mut p, &mut proj.assignments),
                    "Calendars" => parse_calendars(&mut p, &mut proj.calendars),
                    // Any other leaf is a project option docxy does not model:
                    // keep its text so a save writes it back. A block with
                    // child elements (OutlineCodes, ExtendedAttributes, ...) is
                    // consumed whole so its children can't be mistaken for
                    // header fields. So is a prefixed element or one carrying
                    // attributes: an option stores only a name and text, so
                    // writing it back would lose its namespace binding (an
                    // unbound `x:` prefix, or a foreign `xmlns` moved into
                    // MSPDI's) or attributes such as `xsi:nil`.
                    _ if name.contains(':') || !p.attrs().is_empty() => p.skip_element(),
                    _ => {
                        if let Some(text) = leaf_text_of(&mut p) {
                            set_option(&mut proj.options, name, text);
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    normalize_task_uids(&mut proj.tasks, &task_uids)?;

    if let Some(m) = minutes_per_day {
        proj.hours_per_day = m / 60.0;
    }
    proj.hours_per_week = minutes_per_week
        .map(|m| m / 60.0)
        .unwrap_or(proj.hours_per_day * 5.0);
    if proj.calendars.is_empty() {
        proj.calendars
            .push(Calendar::standard(proj.default_calendar_uid));
    }
    if let Some(error) = crate::schedule::calendar_error(&proj) {
        return Err(error);
    }
    Ok(proj)
}

/// Read the text content of the element whose `Start` was just consumed,
/// decoding XML entities. Any nested element is skipped whole. Consumes the
/// element's closing `End`.
fn text_of(p: &mut XmlParser) -> String {
    element_text(p).0
}

/// Like [`text_of`], but `None` when the element has a child element, so a
/// block is never flattened into the text of its leaves.
fn leaf_text_of(p: &mut XmlParser) -> Option<String> {
    let (text, leaf) = element_text(p);
    leaf.then_some(text)
}

/// The element's decoded text, and whether it had no child elements.
fn element_text(p: &mut XmlParser) -> (String, bool) {
    let mut s = String::new();
    let mut leaf = true;
    loop {
        match p.next() {
            Event::Text => XmlParser::append_decoded(p.text(), &mut s),
            Event::Start => {
                leaf = false;
                p.skip_element();
            }
            Event::End | Event::Eof => break,
        }
    }
    (s, leaf)
}

/// Store an unmodeled project option. A repeated name keeps its first
/// position and takes the later value, as a reader of the last value would.
fn set_option(options: &mut Vec<(String, String)>, name: String, text: String) {
    match options.iter_mut().find(|(n, _)| *n == name) {
        Some(slot) => slot.1 = text,
        None => options.push((name, text)),
    }
}

/// Reject ambiguous explicit IDs before allocating missing ones in file order.
/// Only task UIDs change: IDs, predecessor links and assignments remain intact.
fn normalize_task_uids(tasks: &mut [Task], uids: &[Option<i32>]) -> Result<(), String> {
    let mut explicit = std::collections::HashMap::new();
    let mut max_uid = uids.iter().flatten().copied().max().unwrap_or(0);
    for (index, uid) in uids.iter().enumerate() {
        if let Some(uid) = uid {
            if let Some(previous) = explicit.insert(*uid, index) {
                return Err(format!(
                    "duplicate task UID {uid}: {:?} and {:?}",
                    tasks[previous].name, tasks[index].name
                ));
            }
        }
    }
    for (task, uid) in tasks.iter_mut().zip(uids) {
        task.uid = match uid {
            Some(uid) => *uid,
            None => {
                max_uid = max_uid.checked_add(1).ok_or_else(|| {
                    format!(
                        "cannot allocate task UID for {:?}: i32 UID space exhausted",
                        task.name
                    )
                })?;
                max_uid
            }
        };
    }
    Ok(())
}

fn parse_tasks(p: &mut XmlParser, out: &mut Vec<Task>, uids: &mut Vec<Option<i32>>) {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "Task" {
                    let (task, uid) = parse_task(p);
                    out.push(task);
                    uids.push(uid);
                } else {
                    p.skip_element();
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
}

fn parse_task(p: &mut XmlParser) -> (Task, Option<i32>) {
    let mut t = Task::default();
    let mut uid = None;
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "UID" => uid = text_of(p).trim().parse::<i32>().ok(),
                    "ID" => t.id = int_of(p) as i32,
                    "Name" => t.name = text_of(p),
                    "OutlineLevel" => t.outline_level = int_of(p) as u32,
                    "Summary" => t.summary = bool_of(p),
                    "Milestone" => t.milestone = bool_of(p),
                    "Duration" => t.duration_min = iso8601_to_minutes(&text_of(p)),
                    "Start" => t.stored_start = DateTime::parse_mspdi(&text_of(p)),
                    "Finish" => t.stored_finish = DateTime::parse_mspdi(&text_of(p)),
                    "Manual" => t.manual = bool_of(p),
                    "ManualStart" => t.manual_start = DateTime::parse_mspdi(&text_of(p)),
                    "ManualFinish" => t.manual_finish = DateTime::parse_mspdi(&text_of(p)),
                    "ManualDuration" => t.manual_duration_min = try_iso8601_to_minutes(&text_of(p)),
                    "ConstraintType" => {
                        t.constraint = ConstraintType::from_code(int_of(p)).unwrap_or_default();
                    }
                    "ConstraintDate" => t.constraint_date = DateTime::parse_mspdi(&text_of(p)),
                    "CalendarUID" => t.calendar_uid = Some(int_of(p) as i32),
                    "PredecessorLink" => {
                        if let Some(pred) = parse_predecessor(p) {
                            t.predecessors.push(pred);
                        }
                    }
                    "Baseline" => parse_baseline(p, &mut t),
                    "IsNull" => t.is_null = bool_of(p),
                    "GUID" => t.guid = Some(text_of(p)).filter(|g| !g.trim().is_empty()),
                    "CreateDate" => t.create_date = DateTime::parse_mspdi(&text_of(p)),
                    "WBS" => t.wbs = Some(text_of(p)),
                    "Type" => t.task_type = opt_int_of(p).and_then(TaskType::from_code),
                    "Active" => t.active = opt_bool_of(p),
                    "EffortDriven" => t.effort_driven = opt_bool_of(p),
                    "Estimated" => t.estimated = opt_bool_of(p),
                    "Priority" => {
                        t.priority = opt_int_of(p)
                            .filter(|n| (0..=1000).contains(n))
                            .map(|n| n as i32);
                    }
                    "Deadline" => t.deadline = DateTime::parse_mspdi(&text_of(p)),
                    "LevelAssignments" => t.level_assignments = opt_bool_of(p),
                    "LevelingCanSplit" => t.leveling_can_split = opt_bool_of(p),
                    "LevelingDelay" => t.leveling_delay = opt_int_of(p),
                    "LevelingDelayFormat" => t.leveling_delay_format = opt_i32_of(p),
                    "IgnoreResourceCalendar" => t.ignore_resource_calendar = opt_bool_of(p),
                    "EarnedValueMethod" => t.earned_value_method = opt_i32_of(p),
                    "Recurring" => t.recurring = opt_bool_of(p),
                    "HideBar" => t.hide_bar = opt_bool_of(p),
                    "Rollup" => t.rollup = opt_bool_of(p),
                    "ExternalTask" => t.external_task = opt_bool_of(p),
                    "IsSubproject" => t.is_subproject = opt_bool_of(p),
                    "IsSubprojectReadOnly" => t.is_subproject_read_only = opt_bool_of(p),
                    "Work" => t.work_min = try_iso8601_to_minutes(&text_of(p)),
                    "Cost" => t.cost = rate_of(p),
                    "OverAllocated" => t.over_allocated = opt_bool_of(p),
                    "PercentComplete" => t.percent_complete = percent_of(p),
                    "PercentWorkComplete" => t.percent_work_complete = percent_of(p),
                    "PhysicalPercentComplete" => t.physical_percent_complete = percent_of(p),
                    "ActualStart" => t.actual_start = DateTime::parse_mspdi(&text_of(p)),
                    "ActualFinish" => t.actual_finish = DateTime::parse_mspdi(&text_of(p)),
                    "Stop" => t.stop = DateTime::parse_mspdi(&text_of(p)),
                    "Resume" => t.resume = DateTime::parse_mspdi(&text_of(p)),
                    "ActualDuration" => t.actual_duration_min = try_iso8601_to_minutes(&text_of(p)),
                    "RemainingDuration" => {
                        t.remaining_duration_min = try_iso8601_to_minutes(&text_of(p));
                    }
                    "ActualWork" => t.actual_work_min = try_iso8601_to_minutes(&text_of(p)),
                    "RemainingWork" => t.remaining_work_min = try_iso8601_to_minutes(&text_of(p)),
                    "ActualCost" => t.actual_cost = rate_of(p),
                    "RemainingCost" => t.remaining_cost = rate_of(p),
                    "StartVariance" => t.start_variance = opt_int_of(p),
                    "FinishVariance" => t.finish_variance = opt_int_of(p),
                    "WorkVariance" => t.work_variance = rate_of(p),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    (t, uid)
}

/// Parse a task's recorded plan without borrowing values from its current plan.
fn parse_baseline(p: &mut XmlParser, t: &mut Task) {
    let mut baseline = Baseline::default();
    let mut number = Some(0);
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "Number" => {
                        number = text_of(p).trim().parse::<u8>().ok().filter(|n| *n <= 10);
                    }
                    "Start" => baseline.start = DateTime::parse_mspdi(&text_of(p)),
                    "Finish" => baseline.finish = DateTime::parse_mspdi(&text_of(p)),
                    "Duration" => baseline.duration_min = try_iso8601_to_minutes(&text_of(p)),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    if let Some(number) = number {
        if baseline.start.is_some() || baseline.finish.is_some() || baseline.duration_min.is_some()
        {
            baseline.number = number;
            t.set_baseline_slot(baseline);
        }
    }
}

fn parse_predecessor(p: &mut XmlParser) -> Option<Predecessor> {
    let mut uid: Option<i32> = None;
    let mut link = LinkType::FinishStart;
    let mut lag_tenths: i64 = 0;
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "PredecessorUID" => uid = Some(int_of(p) as i32),
                    "Type" => {
                        link = LinkType::from_code(int_of(p)).unwrap_or(LinkType::FinishStart)
                    }
                    "LinkLag" => lag_tenths = int_of(p),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    // LinkLag is tenths of a minute; round to whole minutes.
    let lag_min = (lag_tenths as f64 / 10.0).round() as i64;
    uid.map(|uid| Predecessor { uid, link, lag_min })
}

fn parse_resources(p: &mut XmlParser, out: &mut Vec<Resource>) {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "Resource" {
                    out.push(parse_resource(p));
                } else {
                    p.skip_element();
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
}

fn parse_resource(p: &mut XmlParser) -> Resource {
    let mut r = Resource {
        max_units: 1.0,
        ..Resource::default()
    };
    let mut type_code = None;
    let mut is_cost = false;
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "UID" => r.uid = int_of(p) as i32,
                    "ID" => r.id = int_of(p) as i32,
                    "Name" => r.name = text_of(p),
                    "Type" => type_code = text_of(p).trim().parse::<i64>().ok(),
                    "IsCostResource" => is_cost = bool_of(p),
                    "Initials" => r.initials = Some(text_of(p)),
                    "MaterialLabel" => r.material_label = Some(text_of(p)),
                    "Code" => r.code = Some(text_of(p)),
                    "Group" => r.group = Some(text_of(p)),
                    "MaxUnits" => r.max_units = float_of(p),
                    "AccrueAt" => r.accrue_at = AccrueAt::from_code(int_of(p)),
                    "StandardRate" => r.standard_rate = rate_of(p),
                    "OvertimeRate" => r.overtime_rate = rate_of(p),
                    "CostPerUse" => r.cost_per_use = rate_of(p),
                    "CalendarUID" => r.calendar_uid = Some(int_of(p) as i32),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    // Resolve after all children so the flag can precede or follow Type.
    // Missing/unknown Type defaults to Work; Type 2 is a lenient Cost import.
    r.kind = if type_code == Some(0) && is_cost {
        ResourceType::Cost
    } else {
        type_code
            .and_then(ResourceType::from_code)
            .unwrap_or(ResourceType::Work)
    };
    r
}

fn parse_assignments(p: &mut XmlParser, out: &mut Vec<Assignment>) {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "Assignment" {
                    out.push(parse_assignment(p));
                } else {
                    p.skip_element();
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
}

fn parse_assignment(p: &mut XmlParser) -> Assignment {
    // Absent Units means 100%, not the zero `Default` would give.
    let mut a = Assignment {
        units: 1.0,
        ..Assignment::default()
    };
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "UID" => a.uid = int_of(p) as i32,
                    "TaskUID" => a.task_uid = int_of(p) as i32,
                    "ResourceUID" => a.resource_uid = int_of(p) as i32,
                    "Units" => a.units = float_of(p),
                    "Work" => a.work_min = iso8601_to_minutes(&text_of(p)),
                    "PercentWorkComplete" => a.percent_work_complete = percent_of(p),
                    "ActualStart" => a.actual_start = DateTime::parse_mspdi(&text_of(p)),
                    "ActualFinish" => a.actual_finish = DateTime::parse_mspdi(&text_of(p)),
                    "Stop" => a.stop = DateTime::parse_mspdi(&text_of(p)),
                    "Resume" => a.resume = DateTime::parse_mspdi(&text_of(p)),
                    "ActualWork" => a.actual_work_min = try_iso8601_to_minutes(&text_of(p)),
                    "RemainingWork" => a.remaining_work_min = try_iso8601_to_minutes(&text_of(p)),
                    "ActualCost" => a.actual_cost = rate_of(p),
                    "RemainingCost" => a.remaining_cost = rate_of(p),
                    "StartVariance" => a.start_variance = opt_int_of(p),
                    "FinishVariance" => a.finish_variance = opt_int_of(p),
                    "WorkVariance" => a.work_variance = rate_of(p),
                    "CostVariance" => a.cost_variance = rate_of(p),
                    // Its Start/Finish/Work/Cost are the recorded plan's, not the assignment's.
                    "Baseline" => parse_assignment_baseline(p, &mut a),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    a
}

/// Parse an assignment's recorded plan; the same slot rules as [`parse_baseline`].
fn parse_assignment_baseline(p: &mut XmlParser, a: &mut Assignment) {
    let mut baseline = AssignmentBaseline::default();
    let mut number = Some(0);
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "Number" => {
                        number = text_of(p).trim().parse::<u8>().ok().filter(|n| *n <= 10);
                    }
                    "Start" => baseline.start = DateTime::parse_mspdi(&text_of(p)),
                    "Finish" => baseline.finish = DateTime::parse_mspdi(&text_of(p)),
                    "Work" => baseline.work_min = try_iso8601_to_minutes(&text_of(p)),
                    "Cost" => baseline.cost = rate_of(p),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    if let Some(number) = number {
        if baseline != AssignmentBaseline::default() {
            baseline.number = number;
            a.set_baseline_slot(baseline);
        }
    }
}

fn parse_calendars(p: &mut XmlParser, out: &mut Vec<Calendar>) {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "Calendar" {
                    out.push(parse_calendar(p));
                } else {
                    p.skip_element();
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
}

fn parse_calendar(p: &mut XmlParser) -> Calendar {
    let mut cal = Calendar {
        uid: 0,
        name: String::new(),
        base_calendar_uid: None,
        is_baseline_calendar: false,
        week: Default::default(),
    };
    let mut is_base = false;
    let mut base_uid: Option<i32> = None;
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "UID" => cal.uid = int_of(p) as i32,
                    "Name" => cal.name = text_of(p),
                    "IsBaseCalendar" => is_base = bool_of(p),
                    "IsBaselineCalendar" => cal.is_baseline_calendar = bool_of(p),
                    "BaseCalendarUID" => base_uid = opt_i32_of(p),
                    "WeekDays" => parse_weekdays(p, &mut cal.week),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    // A derived calendar states only the days it overrides and inherits the
    // rest; a base calendar's unstated days are non-working.
    if is_base {
        base_uid = None;
    }
    cal.base_calendar_uid = base_uid.filter(|&uid| uid != -1);
    if cal.base_calendar_uid.is_none() {
        for day in &mut cal.week {
            day.get_or_insert_with(DayWorking::default);
        }
    }
    cal
}

fn parse_weekdays(p: &mut XmlParser, week: &mut [Option<DayWorking>; 7]) {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "WeekDay" {
                    parse_weekday(p, week);
                } else {
                    p.skip_element();
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
}

fn parse_weekday(p: &mut XmlParser, week: &mut [Option<DayWorking>; 7]) {
    // MSPDI DayType: 1=Sunday .. 7=Saturday. Our week[] is Sunday=0..Saturday=6.
    // DayType 0 is a legacy exception entry, not a weekday (exceptions: #126).
    let mut day_type: Option<usize> = None;
    let mut working = false;
    let mut times: Vec<WorkingTime> = Vec::new();
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "DayType" => {
                        day_type = match int_of(p) {
                            d @ 1..=7 => Some(d as usize - 1),
                            _ => None,
                        }
                    }
                    "DayWorking" => working = bool_of(p),
                    "WorkingTimes" => parse_working_times(p, &mut times),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    if let Some(d) = day_type {
        // A non-working day yields empty times even if some were present.
        week[d] = Some(DayWorking {
            times: if working { times } else { Vec::new() },
        });
    }
}

fn parse_working_times(p: &mut XmlParser, out: &mut Vec<WorkingTime>) {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "WorkingTime" {
                    if let Some(wt) = parse_working_time(p) {
                        out.push(wt);
                    }
                } else {
                    p.skip_element();
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
}

fn parse_working_time(p: &mut XmlParser) -> Option<WorkingTime> {
    let mut from: Option<u32> = None;
    let mut to: Option<u32> = None;
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "FromTime" => from = time_to_min(&text_of(p)),
                    "ToTime" => to = time_to_min(&text_of(p)),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    let (from, mut to) = (from?, to?);
    // Project encodes end-of-day as 00:00, including a full midnight-to-midnight day.
    if to == 0 {
        to = 1440;
    }
    (to > from).then_some(WorkingTime { from, to })
}

// ---- small scalar readers ---------------------------------------------------

fn int_of(p: &mut XmlParser) -> i64 {
    text_of(p).trim().parse().unwrap_or(0)
}

fn float_of(p: &mut XmlParser) -> f64 {
    text_of(p).trim().parse().unwrap_or(0.0)
}

/// Invalid optional rates stay absent; nonfinite floats are not XML decimals.
fn rate_of(p: &mut XmlParser) -> Option<Rate> {
    Rate::parse(&text_of(p))
}

fn bool_of(p: &mut XmlParser) -> bool {
    matches!(text_of(p).trim(), "1" | "true" | "True")
}

/// An optional flag: anything but an `xsd:boolean` spelling stays absent.
fn opt_bool_of(p: &mut XmlParser) -> Option<bool> {
    match text_of(p).trim() {
        "1" | "true" | "True" => Some(true),
        "0" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// An optional integer: unparseable text stays absent instead of reading as 0.
fn opt_int_of(p: &mut XmlParser) -> Option<i64> {
    text_of(p).trim().parse().ok()
}

/// A percentage, 0..=100; anything else stays absent.
fn percent_of(p: &mut XmlParser) -> Option<u8> {
    text_of(p).trim().parse().ok().filter(|n| *n <= 100)
}

fn opt_i32_of(p: &mut XmlParser) -> Option<i32> {
    text_of(p).trim().parse().ok()
}

/// `HH:MM[:SS]` → minute of day, ignoring seconds.
fn time_to_min(s: &str) -> Option<u32> {
    let mut it = s.trim().split(':');
    let h: u32 = it.next()?.trim().parse().ok()?;
    let m: u32 = it.next()?.trim().parse().ok()?;
    (h <= 24 && m < 60).then_some(h * 60 + m)
}

/// ISO-8601 duration (`P[nD]T[nH][nM][nS]`) → whole minutes. MSPDI task
/// durations are `PT…` form; days and seconds are handled for robustness.
pub fn iso8601_to_minutes(s: &str) -> i64 {
    let s = s.trim();
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && bytes[i] == b'P' {
        i += 1;
    }
    let mut minutes = 0i64;
    let mut in_time = false;
    let mut num = String::new();
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == 'T' {
            in_time = true;
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || c == '-' || c == '.' {
            num.push(c);
            i += 1;
            continue;
        }
        let val: f64 = num.parse().unwrap_or(0.0);
        num.clear();
        match c {
            'D' => minutes += (val * 1440.0).round() as i64,
            'H' if in_time => minutes += (val * 60.0).round() as i64,
            'M' if in_time => minutes += val.round() as i64,
            'S' if in_time => minutes += (val / 60.0).round() as i64,
            _ => {}
        }
        i += 1;
    }
    minutes
}

/// Parse the supported duration components without treating invalid input as zero.
/// Baselines need to distinguish a recorded zero from an unavailable duration;
/// task and assignment imports retain the permissive parser above.
fn try_iso8601_to_minutes(s: &str) -> Option<i64> {
    let body = s.trim().strip_prefix('P')?;
    let mut minutes = 0i64;
    let mut in_time = false;
    let mut previous_unit = 0;
    let mut num = String::new();
    for c in body.chars() {
        if c == 'T' && !in_time && num.is_empty() {
            in_time = true;
            continue;
        }
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            continue;
        }
        let (unit, factor) = match c {
            'D' if !in_time => (1, 1440.0),
            'H' if in_time => (2, 60.0),
            'M' if in_time => (3, 1.0),
            'S' if in_time => (4, 1.0 / 60.0),
            _ => return None,
        };
        if unit <= previous_unit {
            return None;
        }
        let value = (num.parse::<f64>().ok()? * factor).round();
        // Reject overflow and nonfinite values instead of saturating to invented data.
        if !(i64::MIN as f64..-(i64::MIN as f64)).contains(&value) {
            return None;
        }
        minutes = minutes.checked_add(value as i64)?;
        previous_unit = unit;
        num.clear();
    }
    (num.is_empty() && previous_unit > 0 && (!in_time || previous_unit > 1)).then_some(minutes)
}

// ---- writer -----------------------------------------------------------------

/// Serialize a [`Project`] back to MSPDI XML.
///
/// Emits the fields projcore models — enough for MS Project to open the file
/// and for our own reader to round-trip — plus every project-level option the
/// reader kept verbatim in [`Project::options`]: the schema's in
/// [`PROJECT_HEADER`] order, then any others in read order. Other elements outside the model (custom fields,
/// views, extended attributes, outline codes) are not preserved: this is a
/// model-faithful writer, not a byte-faithful one. Each task's stored
/// `Start`/`Finish` are written when present (e.g. after scheduling and
/// stamping them back), so a scheduled project exports with dates Project can
/// display without recalculating.
///
/// The computed task fields (`OutlineNumber`, early/late dates, the four
/// slacks, `Critical`) come from [`crate::schedule::schedule`], never from
/// values read from a file. Known limit: that schedule ignores calendar
/// exceptions (#126), so on a plan with holidays these fields can differ from
/// Project's and from the stored `Start`/`Finish`.
pub fn write_mspdi(proj: &Project) -> String {
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    s.push_str("<Project xmlns=\"http://schemas.microsoft.com/project\">\n");
    for &name in PROJECT_HEADER {
        if let Some(text) = header_text(proj, name) {
            tag(&mut s, 1, name, &text);
        }
    }
    // Options the schema does not name (a newer Project's, or another
    // emitter's) still survive, after the ones it does.
    for (name, text) in &proj.options {
        if !PROJECT_HEADER.contains(&name.as_str()) {
            tag(&mut s, 1, name, text);
        }
    }

    s.push_str("  <Tasks>\n");
    let sched = crate::schedule::schedule(proj);
    let numbers = outline_numbers(&proj.tasks);
    for (t, number) in proj.tasks.iter().zip(&numbers) {
        let computed = Computed {
            outline_number: number.as_deref(),
            result: sched.get(t.uid).filter(|_| !t.is_null),
        };
        write_task(&mut s, t, &computed);
    }
    s.push_str("  </Tasks>\n");

    if !proj.resources.is_empty() {
        s.push_str("  <Resources>\n");
        for r in &proj.resources {
            write_resource(&mut s, r);
        }
        s.push_str("  </Resources>\n");
    }
    if !proj.assignments.is_empty() {
        s.push_str("  <Assignments>\n");
        for a in &proj.assignments {
            write_assignment(&mut s, a);
        }
        s.push_str("  </Assignments>\n");
    }

    s.push_str("  <Calendars>\n");
    for c in &proj.calendars {
        write_calendar(&mut s, c);
    }
    s.push_str("  </Calendars>\n");

    s.push_str("</Project>\n");
    s
}

/// The scalar children of MSPDI's `<Project>`, in the schema's sequence order
/// (the order Project itself writes them). The writer walks it to place the
/// modeled fields and the stored options; it never filters what the reader
/// keeps.
const PROJECT_HEADER: &[&str] = &[
    "SaveVersion",
    "BuildNumber",
    "Name",
    "GUID",
    "Title",
    "Subject",
    "Category",
    "Company",
    "Manager",
    "Author",
    "CreationDate",
    "Revision",
    "LastSaved",
    "ScheduleFromStart",
    "StartDate",
    "FinishDate",
    "FYStartDate",
    "CriticalSlackLimit",
    "CurrencyDigits",
    "CurrencySymbol",
    "CurrencyCode",
    "CurrencySymbolPosition",
    "CalendarUID",
    "DefaultStartTime",
    "DefaultFinishTime",
    "MinutesPerDay",
    "MinutesPerWeek",
    "DaysPerMonth",
    "DefaultTaskType",
    "DefaultFixedCostAccrual",
    "DefaultStandardRate",
    "DefaultOvertimeRate",
    "DurationFormat",
    "WorkFormat",
    "EditableActualCosts",
    "HonorConstraints",
    "EarnedValueMethod",
    "InsertedProjectsLikeSummary",
    "MultipleCriticalPaths",
    "NewTasksEffortDriven",
    "NewTasksEstimated",
    "SplitsInProgressTasks",
    "SpreadActualCost",
    "SpreadPercentComplete",
    "TaskUpdatesResource",
    "FiscalYearStart",
    "WeekStartDay",
    "MoveCompletedEndsBack",
    "MoveRemainingStartsBack",
    "MoveRemainingStartsForward",
    "MoveCompletedEndsForward",
    "BaselineForEarnedValue",
    "AutoAddNewResourcesAndTasks",
    "StatusDate",
    "CurrentDate",
    "MicrosoftProjectServerURL",
    "Autolink",
    "NewTaskStartDate",
    "NewTasksAreManual",
    "DefaultTaskEVMethod",
    "ProjectExternallyEdited",
    "ExtendedCreationDate",
    "ActualsInSync",
    "RemoveFileProperties",
    "AdminProject",
    "UpdateManuallyScheduledTasksWhenEditingLinks",
    "KeepTaskOnNearestWorkingTimeWhenMadeAutoScheduled",
];

/// The text a save writes for one header element: the modeled value (never
/// `None` for the fields always written), else the option stored verbatim.
fn header_text(proj: &Project, name: &str) -> Option<String> {
    let flag = |on: bool| Some(if on { "1" } else { "0" }.to_string());
    match name {
        "Name" => Some(proj.name.clone()),
        "Title" => (!proj.title.is_empty()).then(|| proj.title.clone()),
        "StartDate" => proj.start_date.map(|d| d.to_mspdi()),
        "CalendarUID" => Some(proj.default_calendar_uid.to_string()),
        "MinutesPerDay" => Some(((proj.hours_per_day * 60.0).round() as i64).to_string()),
        "MinutesPerWeek" => Some(((proj.hours_per_week * 60.0).round() as i64).to_string()),
        "HonorConstraints" => flag(proj.honor_constraints),
        "NewTasksAreManual" => flag(proj.new_tasks_are_manual),
        _ => proj.option(name).map(str::to_string),
    }
}

/// The computed values a save writes for one task, from docxy's own schedule.
struct Computed<'a> {
    outline_number: Option<&'a str>,
    result: Option<&'a TaskResult>,
}

/// Outline numbers (`1`, `1.1`, `2`) in row order; blank rows get none, and
/// level 0 (the project summary) is `0`. The number has one component per
/// ancestor, not per level: a row more than one level deeper than the row
/// above it is numbered one level deeper, as Project would, and a later row
/// that returns to that slot (at the same level, or at any level between the
/// parent's and its own) is the next sibling there, so 1, 3, 3 and 1, 3, 2
/// both number 1, 1.1, 1.2.
fn outline_numbers(tasks: &[Task]) -> Vec<Option<String>> {
    // One (outline level, counter) per component of the current number.
    let mut path: Vec<(u32, u32)> = Vec::new();
    tasks
        .iter()
        .map(|t| {
            if t.is_null {
                return None;
            }
            let level = t.outline_level;
            if level == 0 {
                return Some("0".into());
            }
            // Leave every slot at this level or deeper; the shallowest one left
            // is the slot this row takes, as that slot's next sibling.
            let mut previous = 0;
            while path.last().is_some_and(|&(l, _)| l >= level) {
                previous = path.pop().expect("checked non-empty").1;
            }
            path.push((level, previous + 1));
            Some(
                path.iter()
                    .map(|(_, n)| n.to_string())
                    .collect::<Vec<_>>()
                    .join("."),
            )
        })
        .collect()
}

fn flag(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

fn opt_flag(s: &mut String, name: &str, value: Option<bool>) {
    if let Some(value) = value {
        tag(s, 3, name, flag(value));
    }
}

fn opt_date(s: &mut String, name: &str, value: Option<DateTime>) {
    if let Some(value) = value {
        tag(s, 3, name, &value.to_mspdi());
    }
}

fn opt_text(s: &mut String, name: &str, value: Option<impl ToString>) {
    if let Some(value) = value {
        tag(s, 3, name, &value.to_string());
    }
}

/// Write one task's children in the MSPDI `Task` sequence (the order Project
/// 2024 writes them). A blank row writes only what it stores: no computed
/// fields and none of the elements every task otherwise states.
fn write_task(s: &mut String, t: &Task, computed: &Computed) {
    let task = !t.is_null;
    s.push_str("    <Task>\n");
    tag(s, 3, "UID", &t.uid.to_string());
    opt_text(s, "GUID", t.guid.as_ref());
    tag(s, 3, "ID", &t.id.to_string());
    if task || !t.name.is_empty() {
        tag(s, 3, "Name", &t.name);
    }
    opt_flag(s, "Active", t.active);
    // A blank row states only what it carries: flags when set, a constraint
    // when not the default. It is never a summary (the outline skips it).
    if task || t.manual {
        tag(s, 3, "Manual", flag(t.manual));
    }
    opt_text(s, "Type", t.task_type.map(TaskType::code));
    // Every row states it, as Project writes it.
    tag(s, 3, "IsNull", flag(t.is_null));
    opt_date(s, "CreateDate", t.create_date);
    opt_text(s, "WBS", t.wbs.as_ref());
    opt_text(s, "OutlineNumber", computed.outline_number);
    tag(s, 3, "OutlineLevel", &t.outline_level.to_string());
    opt_text(s, "Priority", t.priority);
    opt_date(s, "Start", t.stored_start);
    opt_date(s, "Finish", t.stored_finish);
    if task || t.duration_min != 0 {
        tag(s, 3, "Duration", &min_to_iso(t.duration_min));
    }
    opt_date(s, "ManualStart", t.manual_start);
    opt_date(s, "ManualFinish", t.manual_finish);
    opt_text(s, "ManualDuration", t.manual_duration_min.map(min_to_iso));
    if task {
        tag(s, 3, "DurationFormat", "7");
    }
    opt_text(s, "Work", t.work_min.map(min_to_iso));
    opt_date(s, "Stop", t.stop);
    opt_date(s, "Resume", t.resume);
    opt_flag(s, "EffortDriven", t.effort_driven);
    opt_flag(s, "Recurring", t.recurring);
    opt_flag(s, "OverAllocated", t.over_allocated);
    opt_flag(s, "Estimated", t.estimated);
    if task || t.milestone {
        tag(s, 3, "Milestone", flag(t.milestone));
    }
    if task {
        tag(s, 3, "Summary", flag(t.summary));
    }
    opt_flag(s, "Critical", computed.result.map(|r| r.critical));
    opt_flag(s, "IsSubproject", t.is_subproject);
    opt_flag(s, "IsSubprojectReadOnly", t.is_subproject_read_only);
    opt_flag(s, "ExternalTask", t.external_task);
    if let Some(r) = computed.result {
        tag(s, 3, "EarlyStart", &r.early_start.to_mspdi());
        tag(s, 3, "EarlyFinish", &r.early_finish.to_mspdi());
        tag(s, 3, "LateStart", &r.late_start.to_mspdi());
        tag(s, 3, "LateFinish", &r.late_finish.to_mspdi());
    }
    opt_text(s, "StartVariance", t.start_variance);
    opt_text(s, "FinishVariance", t.finish_variance);
    opt_text(
        s,
        "WorkVariance",
        t.work_variance.as_ref().map(Rate::as_str),
    );
    if let Some(r) = computed.result {
        // Slack is working minutes in the model, tenths of a minute in MSPDI.
        for (name, min) in [
            ("FreeSlack", r.free_slack_min),
            ("TotalSlack", r.total_slack_min),
            ("StartSlack", r.start_slack_min),
            ("FinishSlack", r.finish_slack_min),
        ] {
            tag(s, 3, name, &(min * 10).to_string());
        }
    }
    opt_text(s, "PercentComplete", t.percent_complete);
    opt_text(s, "PercentWorkComplete", t.percent_work_complete);
    opt_text(s, "Cost", t.cost.as_ref().map(Rate::as_str));
    opt_date(s, "ActualStart", t.actual_start);
    opt_date(s, "ActualFinish", t.actual_finish);
    opt_text(s, "ActualDuration", t.actual_duration_min.map(min_to_iso));
    opt_text(s, "ActualCost", t.actual_cost.as_ref().map(Rate::as_str));
    opt_text(s, "ActualWork", t.actual_work_min.map(min_to_iso));
    opt_text(
        s,
        "RemainingDuration",
        t.remaining_duration_min.map(min_to_iso),
    );
    opt_text(
        s,
        "RemainingCost",
        t.remaining_cost.as_ref().map(Rate::as_str),
    );
    opt_text(s, "RemainingWork", t.remaining_work_min.map(min_to_iso));
    if task || t.constraint != ConstraintType::AsSoonAsPossible {
        tag(s, 3, "ConstraintType", &t.constraint.code().to_string());
    }
    opt_text(s, "CalendarUID", t.calendar_uid);
    opt_date(s, "ConstraintDate", t.constraint_date);
    opt_date(s, "Deadline", t.deadline);
    opt_flag(s, "LevelAssignments", t.level_assignments);
    opt_flag(s, "LevelingCanSplit", t.leveling_can_split);
    opt_text(s, "LevelingDelay", t.leveling_delay);
    opt_text(s, "LevelingDelayFormat", t.leveling_delay_format);
    opt_flag(s, "IgnoreResourceCalendar", t.ignore_resource_calendar);
    opt_flag(s, "HideBar", t.hide_bar);
    opt_flag(s, "Rollup", t.rollup);
    opt_text(s, "PhysicalPercentComplete", t.physical_percent_complete);
    opt_text(s, "EarnedValueMethod", t.earned_value_method);
    for p in &t.predecessors {
        s.push_str("      <PredecessorLink>\n");
        tag(s, 4, "PredecessorUID", &p.uid.to_string());
        tag(s, 4, "Type", &p.link.code().to_string());
        // model lag is minutes; MSPDI LinkLag is tenths of a minute.
        tag(s, 4, "LinkLag", &(p.lag_min * 10).to_string());
        tag(s, 4, "LagFormat", "7");
        s.push_str("      </PredecessorLink>\n");
    }
    let mut baselines: Vec<_> = t.baselines.iter().collect();
    baselines.sort_by_key(|b| b.number);
    for baseline in baselines {
        s.push_str("      <Baseline>\n");
        tag(s, 4, "Number", &baseline.number.to_string());
        if let Some(start) = baseline.start {
            tag(s, 4, "Start", &start.to_mspdi());
        }
        if let Some(finish) = baseline.finish {
            tag(s, 4, "Finish", &finish.to_mspdi());
        }
        if let Some(duration) = baseline.duration_min {
            tag(s, 4, "Duration", &min_to_iso(duration));
        }
        s.push_str("      </Baseline>\n");
    }
    s.push_str("    </Task>\n");
}

fn write_resource(s: &mut String, r: &Resource) {
    s.push_str("    <Resource>\n");
    tag(s, 3, "UID", &r.uid.to_string());
    tag(s, 3, "ID", &r.id.to_string());
    tag(s, 3, "Name", &r.name);
    tag(s, 3, "Type", &r.kind.code().to_string());
    // Keep the relative sequence from Microsoft's Resource XSD, including
    // IsCostResource after CalendarUID (Type itself only permits 0 and 1).
    for (name, value) in [
        ("Initials", &r.initials),
        ("MaterialLabel", &r.material_label),
        ("Code", &r.code),
        ("Group", &r.group),
    ] {
        if let Some(value) = value {
            tag(s, 3, name, value);
        }
    }
    tag(s, 3, "MaxUnits", &fmt_f(r.max_units));
    if let Some(accrue_at) = r.accrue_at {
        tag(s, 3, "AccrueAt", &accrue_at.code().to_string());
    }
    for (name, value) in [
        ("StandardRate", &r.standard_rate),
        ("OvertimeRate", &r.overtime_rate),
        ("CostPerUse", &r.cost_per_use),
    ] {
        if let Some(value) = value {
            tag(s, 3, name, value.as_str());
        }
    }
    if let Some(c) = r.calendar_uid {
        tag(s, 3, "CalendarUID", &c.to_string());
    }
    if r.kind == ResourceType::Cost {
        tag(s, 3, "IsCostResource", "1");
    }
    s.push_str("    </Resource>\n");
}

fn write_assignment(s: &mut String, a: &Assignment) {
    s.push_str("    <Assignment>\n");
    tag(s, 3, "UID", &a.uid.to_string());
    tag(s, 3, "TaskUID", &a.task_uid.to_string());
    tag(s, 3, "ResourceUID", &a.resource_uid.to_string());
    // Microsoft's Assignment sequence, as Project 2024 writes it.
    opt_text(s, "PercentWorkComplete", a.percent_work_complete);
    opt_text(s, "ActualCost", a.actual_cost.as_ref().map(Rate::as_str));
    opt_date(s, "ActualFinish", a.actual_finish);
    opt_date(s, "ActualStart", a.actual_start);
    opt_text(s, "ActualWork", a.actual_work_min.map(min_to_iso));
    opt_text(
        s,
        "CostVariance",
        a.cost_variance.as_ref().map(Rate::as_str),
    );
    opt_text(s, "FinishVariance", a.finish_variance);
    opt_text(
        s,
        "WorkVariance",
        a.work_variance.as_ref().map(Rate::as_str),
    );
    opt_text(
        s,
        "RemainingCost",
        a.remaining_cost.as_ref().map(Rate::as_str),
    );
    opt_text(s, "RemainingWork", a.remaining_work_min.map(min_to_iso));
    opt_date(s, "Stop", a.stop);
    opt_date(s, "Resume", a.resume);
    opt_text(s, "StartVariance", a.start_variance);
    tag(s, 3, "Units", &fmt_f(a.units));
    tag(s, 3, "Work", &min_to_iso(a.work_min));
    for baseline in &a.baselines {
        s.push_str("      <Baseline>\n");
        tag(s, 4, "Number", &baseline.number.to_string());
        if let Some(start) = baseline.start {
            tag(s, 4, "Start", &start.to_mspdi());
        }
        if let Some(finish) = baseline.finish {
            tag(s, 4, "Finish", &finish.to_mspdi());
        }
        if let Some(work) = baseline.work_min {
            tag(s, 4, "Work", &min_to_iso(work));
        }
        if let Some(cost) = &baseline.cost {
            tag(s, 4, "Cost", cost.as_str());
        }
        s.push_str("      </Baseline>\n");
    }
    s.push_str("    </Assignment>\n");
}

fn write_calendar(s: &mut String, c: &Calendar) {
    s.push_str("    <Calendar>\n");
    tag(s, 3, "UID", &c.uid.to_string());
    tag(s, 3, "Name", &c.name);
    tag(s, 3, "IsBaseCalendar", flag(c.base_calendar_uid.is_none()));
    tag(s, 3, "IsBaselineCalendar", flag(c.is_baseline_calendar));
    tag(
        s,
        3,
        "BaseCalendarUID",
        &c.base_calendar_uid.unwrap_or(-1).to_string(),
    );
    // A base calendar states all seven days, an unstated one as non-working. A
    // derived calendar states only the days it overrides, so Project keeps
    // inheriting the rest from its base.
    let non_working = DayWorking::default();
    let days: Vec<(usize, &DayWorking)> = c
        .week
        .iter()
        .enumerate()
        .filter_map(|(idx, day)| match (day, c.base_calendar_uid) {
            (Some(day), _) => Some((idx, day)),
            (None, None) => Some((idx, &non_working)),
            (None, Some(_)) => None,
        })
        .collect();
    if days.is_empty() {
        s.push_str("    </Calendar>\n");
        return;
    }
    s.push_str("      <WeekDays>\n");
    for (idx, day) in days {
        // model week[] is Sun=0..Sat=6; MSPDI DayType is 1=Sun..7=Sat.
        s.push_str("        <WeekDay>\n");
        tag(s, 5, "DayType", &(idx + 1).to_string());
        tag(s, 5, "DayWorking", if day.working() { "1" } else { "0" });
        if day.working() {
            s.push_str("          <WorkingTimes>\n");
            for w in &day.times {
                s.push_str("            <WorkingTime>");
                s.push_str(&format!(
                    "<FromTime>{}</FromTime><ToTime>{}</ToTime>",
                    min_to_clock(w.from),
                    min_to_clock(w.to)
                ));
                s.push_str("</WorkingTime>\n");
            }
            s.push_str("          </WorkingTimes>\n");
        }
        s.push_str("        </WeekDay>\n");
    }
    s.push_str("      </WeekDays>\n");
    s.push_str("    </Calendar>\n");
}

/// Write `<Name>text</Name>` at the given indent depth (2 spaces each), with the
/// text XML-escaped.
fn tag(s: &mut String, depth: usize, name: &str, text: &str) {
    for _ in 0..depth {
        s.push_str("  ");
    }
    s.push('<');
    s.push_str(name);
    s.push('>');
    esc_into(text, s);
    s.push_str("</");
    s.push_str(name);
    s.push_str(">\n");
}

fn esc_into(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

fn min_to_iso(min: i64) -> String {
    let (h, m) = (min / 60, min % 60);
    format!("PT{h}H{m}M0S")
}

/// Minute-of-day → `HH:MM:SS`. End-of-day (1440) is written as `00:00:00`
/// (Project's midnight convention), which the reader maps back to 1440.
fn min_to_clock(min: u32) -> String {
    let m = if min >= 1440 { 0 } else { min };
    format!("{:02}:{:02}:00", m / 60, m % 60)
}

/// Format a float without a trailing `.0` (so `1.0` → `1`, `0.5` → `0.5`).
fn fmt_f(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uid_xml(tasks: &str) -> String {
        format!("<Project><Tasks>{tasks}</Tasks></Project>")
    }

    #[test]
    fn missing_uids_are_fresh_and_explicit_references_are_unchanged() {
        let xml = r#"<Project><Tasks>
          <Task><ID>1</ID><Name>Missing</Name></Task>
          <Task><UID>invalid</UID><ID>2</ID><Name>Invalid</Name></Task>
          <Task><UID>2147483648</UID><ID>3</ID><Name>Out of range</Name></Task>
          <Task><UID>0</UID><ID>4</ID><Name>Summary</Name></Task>
          <Task><UID>40</UID><ID>5</ID><Name>Explicit</Name>
            <PredecessorLink><PredecessorUID>0</PredecessorUID><Type>1</Type></PredecessorLink>
          </Task>
        </Tasks><Assignments><Assignment><UID>7</UID><TaskUID>40</TaskUID><ResourceUID>3</ResourceUID></Assignment></Assignments></Project>"#;
        let project = read_mspdi(xml).unwrap();
        assert_eq!(
            project.tasks.iter().map(|t| t.uid).collect::<Vec<_>>(),
            [41, 42, 43, 0, 40]
        );
        assert_eq!(
            project.tasks.iter().map(|t| t.id).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        assert_eq!(project.tasks[4].predecessors[0].uid, 0);
        assert_eq!(project.assignments[0].task_uid, 40);
        assert_eq!(read_mspdi(&write_mspdi(&project)).unwrap(), project);
        let package = opccore::zipwrite::write_zip(&[(
            crate::yppx::MAIN_PART.into(),
            xml.as_bytes().to_vec(),
        )]);
        assert_eq!(crate::yppx::read_yppx(&package).unwrap(), project);
        let missing_only = read_mspdi(&uid_xml("<Task/><Task/>")).unwrap();
        assert_eq!(
            missing_only.tasks.iter().map(|t| t.uid).collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[test]
    fn duplicate_explicit_uids_name_both_tasks_including_zero() {
        for uid in [0, 7] {
            let xml = uid_xml(&format!(
                "<Task><UID>{uid}</UID><Name>A</Name></Task><Task><UID>{uid}</UID><Name>B</Name></Task>"
            ));
            let error = format!("duplicate task UID {uid}: \"A\" and \"B\"");
            assert_eq!(read_mspdi(&xml).unwrap_err(), error);
            let package =
                opccore::zipwrite::write_zip(&[(crate::yppx::MAIN_PART.into(), xml.into_bytes())]);
            assert_eq!(crate::yppx::read_yppx(&package).unwrap_err(), error);
        }
    }

    #[test]
    fn uid_allocation_checks_exhaustion_only_when_needed() {
        let negative = read_mspdi(&uid_xml("<Task><UID>-4</UID></Task><Task/>")).unwrap();
        assert_eq!(negative.tasks[1].uid, -3);
        let max = "<Task><UID>2147483647</UID></Task>";
        assert_eq!(read_mspdi(&uid_xml(max)).unwrap().tasks[0].uid, i32::MAX);
        let error =
            read_mspdi(&uid_xml(&format!("{max}<Task><Name>Missing</Name></Task>"))).unwrap_err();
        assert!(error.contains("UID space exhausted") && error.contains("Missing"));
        let near = "<Task><UID>2147483646</UID></Task>";
        assert_eq!(
            read_mspdi(&uid_xml(&format!("{near}<Task/>")))
                .unwrap()
                .tasks[1]
                .uid,
            i32::MAX
        );
        assert!(
            read_mspdi(&uid_xml(&format!("{near}<Task/><Task/>")))
                .unwrap_err()
                .contains("UID space exhausted")
        );
    }

    #[test]
    fn honor_constraints_reads_boolean_forms_and_defaults_to_true() {
        for (value, expected) in [("0", false), ("1", true), ("false", false), ("true", true)] {
            let xml = format!("<Project><HonorConstraints>{value}</HonorConstraints></Project>");
            assert_eq!(read_mspdi(&xml).unwrap().honor_constraints, expected);
        }
        assert!(read_mspdi("<Project/>").unwrap().honor_constraints);
    }

    #[test]
    fn honor_constraints_survives_mspdi_and_native_package_round_trips() {
        for honor_constraints in [true, false] {
            let proj = Project {
                honor_constraints,
                ..Project::default()
            };
            let xml = write_mspdi(&proj);
            assert!(xml.contains(if honor_constraints {
                "<HonorConstraints>1</HonorConstraints>"
            } else {
                "<HonorConstraints>0</HonorConstraints>"
            }));
            assert_eq!(
                read_mspdi(&xml).unwrap().honor_constraints,
                honor_constraints
            );
            let package = crate::yppx::write_yppx(&proj);
            assert_eq!(
                crate::yppx::read_yppx(&package).unwrap().honor_constraints,
                honor_constraints
            );
        }
    }

    const MANUAL_XML: &str = r#"<Project><NewTasksAreManual>1</NewTasksAreManual><Tasks>
      <Task><UID>1</UID><Name>Manual</Name><Manual>1</Manual><Duration>PT8H0M0S</Duration>
        <Start>2026-03-02T08:00:00</Start><Finish>2026-03-02T17:00:00</Finish>
        <ManualStart>2026-03-02T08:00:00</ManualStart><ManualDuration>PT8H0M0S</ManualDuration></Task>
      <Task><UID>2</UID><Name>Auto</Name><Manual>0</Manual><Duration>PT16H0M0S</Duration>
        <ManualStart>2026-03-03T08:00:00</ManualStart><ManualFinish>2026-03-04T17:00:00</ManualFinish>
        <ManualDuration>PT16H0M0S</ManualDuration></Task>
    </Tasks></Project>"#;

    #[test]
    fn task_mode_and_manual_fields_are_read() {
        let proj = read_mspdi(MANUAL_XML).unwrap();
        assert!(proj.new_tasks_are_manual);
        let (manual, auto) = (&proj.tasks[0], &proj.tasks[1]);
        assert!(manual.manual);
        assert_eq!(
            manual.manual_start.unwrap().to_mspdi(),
            "2026-03-02T08:00:00"
        );
        assert_eq!(manual.manual_finish, None);
        assert_eq!(manual.manual_duration_min, Some(480));
        // Project writes the manual fields on auto tasks too; keep them as read.
        assert!(!auto.manual);
        assert_eq!(
            auto.manual_finish.unwrap().to_mspdi(),
            "2026-03-04T17:00:00"
        );
        assert_eq!(auto.manual_duration_min, Some(960));
        assert!(!read_mspdi("<Project/>").unwrap().new_tasks_are_manual);
    }

    #[test]
    fn task_mode_and_manual_fields_survive_mspdi_and_native_package_round_trips() {
        let proj = read_mspdi(MANUAL_XML).unwrap();
        let xml = write_mspdi(&proj);
        assert!(xml.contains("<NewTasksAreManual>1</NewTasksAreManual>"));
        assert!(xml.contains("<Manual>1</Manual>"));
        assert!(xml.contains("<ManualStart>2026-03-02T08:00:00</ManualStart>"));
        assert!(xml.contains("<ManualDuration>PT8H0M0S</ManualDuration>"));
        assert_eq!(read_mspdi(&xml).unwrap().tasks, proj.tasks);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj)).unwrap();
        assert_eq!(package.tasks, proj.tasks);
        assert!(package.new_tasks_are_manual);
    }

    #[test]
    fn saved_file_states_every_task_mode_and_the_project_default() {
        let proj = Project {
            tasks: vec![Task {
                uid: 1,
                ..Task::default()
            }],
            ..Project::default()
        };
        let xml = write_mspdi(&proj);
        assert!(xml.contains("<Manual>0</Manual>"));
        assert!(xml.contains("<NewTasksAreManual>0</NewTasksAreManual>"));
        // Absent manual fields stay absent rather than being invented.
        assert!(!xml.contains("<ManualStart>"));
        assert!(!xml.contains("<ManualDuration>"));
    }

    #[test]
    fn invalid_manual_duration_stays_absent() {
        let xml = "<Project><Tasks><Task><UID>1</UID><Manual>1</Manual>\
                   <ManualDuration>banana</ManualDuration></Task></Tasks></Project>";
        assert_eq!(read_mspdi(xml).unwrap().tasks[0].manual_duration_min, None);
    }

    fn resource_project(resources: &str) -> Project {
        read_mspdi(&format!(
            "<Project><Resources>{resources}</Resources></Project>"
        ))
        .unwrap()
    }

    #[test]
    fn resource_fields_and_legacy_cost_type_survive_save() {
        let proj = resource_project(
            "<Resource><UID>1</UID><ID>1</ID><Name>Alice</Name><Type>1</Type><MaxUnits>1</MaxUnits>
              <Initials>A</Initials><Group>Eng</Group><Code>C7</Code>
              <StandardRate>50</StandardRate><OvertimeRate>75</OvertimeRate>
              <CostPerUse>10</CostPerUse><AccrueAt>3</AccrueAt></Resource>
             <Resource><UID>2</UID><ID>2</ID><Name>Licence</Name><Type>2</Type><MaxUnits>1</MaxUnits></Resource>
             <Resource><UID>3</UID><ID>3</ID><Name>Concrete</Name><Type>0</Type>
              <MaterialLabel>tonnes</MaterialLabel><MaxUnits>1</MaxUnits></Resource>",
        );
        assert_eq!(
            proj.resources[0],
            Resource {
                uid: 1,
                id: 1,
                name: "Alice".into(),
                kind: ResourceType::Work,
                initials: Some("A".into()),
                group: Some("Eng".into()),
                code: Some("C7".into()),
                standard_rate: Rate::parse("50"),
                overtime_rate: Rate::parse("75"),
                cost_per_use: Rate::parse("10"),
                accrue_at: Some(AccrueAt::Prorated),
                max_units: 1.0,
                ..Resource::default()
            }
        );
        assert_eq!(proj.resources[1].kind, ResourceType::Cost);
        assert_eq!(proj.resources[2].kind, ResourceType::Material);
        assert_eq!(proj.resources[2].material_label.as_deref(), Some("tonnes"));
        let xml = write_mspdi(&proj);
        for element in [
            "<Initials>A</Initials>",
            "<Group>Eng</Group>",
            "<Code>C7</Code>",
            "<StandardRate>50</StandardRate>",
            "<OvertimeRate>75</OvertimeRate>",
            "<CostPerUse>10</CostPerUse>",
            "<AccrueAt>3</AccrueAt>",
            "<MaterialLabel>tonnes</MaterialLabel>",
            "<IsCostResource>1</IsCostResource>",
        ] {
            assert!(xml.contains(element), "missing {element}");
        }
        assert!(!xml.contains("<Type>2</Type>"));
        assert_eq!(read_mspdi(&xml).unwrap().resources, proj.resources);
    }

    #[test]
    fn resource_type_and_cost_flag_are_encoded_together() {
        use ResourceType::*;
        for (children, expected) in [
            ("<Type>0</Type>", Material),
            ("<Type>1</Type>", Work),
            ("<Type>2</Type>", Cost),
            ("<Type>0</Type><IsCostResource>1</IsCostResource>", Cost),
            ("<IsCostResource>true</IsCostResource><Type>0</Type>", Cost),
            (
                "<Type>0</Type><IsCostResource>false</IsCostResource>",
                Material,
            ),
            ("<Type>0</Type><IsCostResource>0</IsCostResource>", Material),
            ("<Type>1</Type><IsCostResource>1</IsCostResource>", Work),
            ("", Work),
            ("<IsCostResource>1</IsCostResource>", Work),
            ("<Type>7</Type>", Work),
            ("<Type/>", Work),
            ("<Type></Type>", Work),
            ("<Type>1.0</Type>", Work),
            ("<Type>abc</Type>", Work),
            ("<Type/><IsCostResource>1</IsCostResource>", Work),
            (
                "<Type>abc</Type><IsCostResource>true</IsCostResource>",
                Work,
            ),
        ] {
            let proj = resource_project(&format!("<Resource>{children}</Resource>"));
            assert_eq!(proj.resources[0].kind, expected, "{children}");
            let mut xml = String::new();
            write_resource(&mut xml, &proj.resources[0]);
            let type_code = if expected == Work { 1 } else { 0 };
            assert!(xml.contains(&format!("<Type>{type_code}</Type>")), "{xml}");
            assert_eq!(
                xml.contains("<IsCostResource>1</IsCostResource>"),
                expected == Cost
            );
            assert_eq!(resource_project(&xml).resources, proj.resources);
        }
    }

    #[test]
    fn invalid_resource_rates_stay_absent_on_save() {
        for name in ["StandardRate", "OvertimeRate", "CostPerUse"] {
            let elements = std::iter::once(format!("<{name}/>")).chain(
                [
                    "",
                    "abc",
                    "NaN",
                    "inf",
                    "-infinity",
                    "1e999",
                    "&#xA0;5&#xA0;",
                ]
                .map(|value| format!("<{name}>{value}</{name}>")),
            );
            for element in elements {
                let proj = resource_project(&format!("<Resource>{element}</Resource>"));
                let r = &proj.resources[0];
                assert_eq!(
                    (&r.standard_rate, &r.overtime_rate, &r.cost_per_use),
                    (&None, &None, &None),
                    "{element}"
                );
                let xml = write_mspdi(&proj);
                assert!(!xml.contains(&format!("<{name}>")), "{xml}");
                assert_eq!(read_mspdi(&xml).unwrap().resources, proj.resources);
            }
        }
    }

    #[test]
    fn resource_rates_round_trip_as_text() {
        let huge = format!("1{}", "0".repeat(400));
        let values = [
            "9007199254740993",
            "0.12345678901234567890123456789",
            &huge,
            "+5",
            "-0.50",
            ".5",
            "5.",
            "007",
        ];
        for name in ["StandardRate", "OvertimeRate", "CostPerUse"] {
            for value in values {
                let proj =
                    resource_project(&format!("<Resource><{name}> {value} </{name}></Resource>"));
                let rate = match name {
                    "StandardRate" => proj.resources[0].standard_rate.as_ref(),
                    "OvertimeRate" => proj.resources[0].overtime_rate.as_ref(),
                    _ => proj.resources[0].cost_per_use.as_ref(),
                };
                assert_eq!(rate.map(Rate::as_str), Some(value), "{name}");
                let xml = write_mspdi(&proj);
                assert!(xml.contains(&format!("<{name}>{value}</{name}>")), "{xml}");
                assert_eq!(read_mspdi(&xml).unwrap().resources, proj.resources);
            }
            for (source, expected) in [("1e3", "1000"), ("1.5E2", "150")] {
                let proj =
                    resource_project(&format!("<Resource><{name}>{source}</{name}></Resource>"));
                let xml = write_mspdi(&proj);
                assert!(
                    xml.contains(&format!("<{name}>{expected}</{name}>")),
                    "{xml}"
                );
                assert_eq!(read_mspdi(&xml).unwrap().resources, proj.resources);
            }
        }
    }

    #[test]
    fn optional_resource_fields_preserve_empty_zero_and_escaped_values() {
        let proj = resource_project(
            "<Resource><Initials></Initials><MaterialLabel/>
             <Code>R&amp;D &lt;x&gt;</Code><Group>R&amp;D &lt;x&gt;</Group>
             <StandardRate>0</StandardRate><OvertimeRate>12.5</OvertimeRate>
             <CostPerUse>100000000000000000000</CostPerUse></Resource>",
        );
        let r = &proj.resources[0];
        assert_eq!(r.initials.as_deref(), Some(""));
        assert_eq!(r.material_label.as_deref(), Some(""));
        assert_eq!(r.code.as_deref(), Some("R&D <x>"));
        assert_eq!(r.group.as_deref(), Some("R&D <x>"));
        assert_eq!(r.standard_rate.as_ref().map(Rate::as_str), Some("0"));
        assert_eq!(r.overtime_rate.as_ref().map(Rate::as_str), Some("12.5"));
        assert_eq!(
            r.cost_per_use.as_ref().map(Rate::as_str),
            Some("100000000000000000000")
        );
        let xml = write_mspdi(&proj);
        for element in [
            "<Initials></Initials>",
            "<MaterialLabel></MaterialLabel>",
            "<Code>R&amp;D &lt;x&gt;</Code>",
            "<Group>R&amp;D &lt;x&gt;</Group>",
            "<StandardRate>0</StandardRate>",
            "<OvertimeRate>12.5</OvertimeRate>",
            "<CostPerUse>100000000000000000000</CostPerUse>",
        ] {
            assert!(xml.contains(element), "missing {element}");
        }
        assert_eq!(read_mspdi(&xml).unwrap().resources, proj.resources);
    }

    #[test]
    fn resource_accrual_preserves_every_schema_value() {
        for (code, expected) in [
            (1, AccrueAt::Start),
            (2, AccrueAt::End),
            (3, AccrueAt::Prorated),
            (4, AccrueAt::Invalid),
        ] {
            let proj =
                resource_project(&format!("<Resource><AccrueAt>{code}</AccrueAt></Resource>"));
            assert_eq!(proj.resources[0].accrue_at, Some(expected));
            let xml = write_mspdi(&proj);
            assert!(xml.contains(&format!("<AccrueAt>{code}</AccrueAt>")));
            assert_eq!(read_mspdi(&xml).unwrap().resources, proj.resources);
        }
        for code in [0, 9] {
            let proj =
                resource_project(&format!("<Resource><AccrueAt>{code}</AccrueAt></Resource>"));
            assert_eq!(proj.resources[0].accrue_at, None);
            assert!(!write_mspdi(&proj).contains("<AccrueAt>"));
        }
    }

    #[test]
    fn absent_resource_fields_stay_absent() {
        let proj = resource_project(
            "<Resource><UID>1</UID><ID>1</ID><Name>Alice</Name><Type>1</Type><MaxUnits>1</MaxUnits></Resource>",
        );
        let mut xml = String::new();
        write_resource(&mut xml, &proj.resources[0]);
        assert_eq!(
            xml,
            concat!(
                "    <Resource>\n",
                "      <UID>1</UID>\n",
                "      <ID>1</ID>\n",
                "      <Name>Alice</Name>\n",
                "      <Type>1</Type>\n",
                "      <MaxUnits>1</MaxUnits>\n",
                "    </Resource>\n",
            )
        );
    }

    #[test]
    fn resource_children_follow_schema_sequence() {
        // Microsoft: XML Schema for the Resources Element (Project 2016).
        // https://learn.microsoft.com/en-us/office-project/xml-data-interchange/xml-schema-for-the-resources-element
        let r = Resource {
            kind: ResourceType::Cost,
            initials: Some("A".into()),
            material_label: Some("unit".into()),
            code: Some("C7".into()),
            group: Some("Eng".into()),
            accrue_at: Some(AccrueAt::End),
            standard_rate: Rate::parse("50"),
            overtime_rate: Rate::parse("75"),
            cost_per_use: Rate::parse("10"),
            calendar_uid: Some(1),
            ..Resource::default()
        };
        let mut xml = String::new();
        write_resource(&mut xml, &r);
        let mut parser = XmlParser::new(&xml);
        let mut names = Vec::new();
        loop {
            match parser.next() {
                Event::Start => names.push(parser.name().to_string()),
                Event::Eof => break,
                _ => {}
            }
        }
        assert_eq!(
            names,
            [
                "Resource",
                "UID",
                "ID",
                "Name",
                "Type",
                "Initials",
                "MaterialLabel",
                "Code",
                "Group",
                "MaxUnits",
                "AccrueAt",
                "StandardRate",
                "OvertimeRate",
                "CostPerUse",
                "CalendarUID",
                "IsCostResource"
            ]
        );
        assert_eq!(resource_project(&xml).resources, vec![r]);
    }

    #[test]
    fn requires_project_root_but_allows_empty_projects() {
        for xml in ["", "hello", "<foo/>", "<foo><Project/></foo>"] {
            assert!(read_mspdi(xml).is_err(), "{xml}");
        }
        for xml in ["<Project/>", "<?xml version=\"1.0\"?><Project/>"] {
            assert!(read_mspdi(xml).unwrap().tasks.is_empty());
        }
    }

    #[test]
    fn iso_durations() {
        assert_eq!(iso8601_to_minutes("PT16H0M0S"), 960);
        assert_eq!(iso8601_to_minutes("PT0H0M0S"), 0);
        assert_eq!(iso8601_to_minutes("PT8H30M0S"), 510);
        assert_eq!(iso8601_to_minutes("PT1H"), 60);
        assert_eq!(iso8601_to_minutes("P1DT0H0M0S"), 1440);
    }

    #[test]
    fn time_parsing() {
        assert_eq!(time_to_min("08:00:00"), Some(480));
        assert_eq!(time_to_min("13:30"), Some(810));
        assert_eq!(time_to_min("garbage"), None);
    }

    const MINIMAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Project xmlns="http://schemas.microsoft.com/project">
  <Name>demo</Name>
  <MinutesPerDay>480</MinutesPerDay>
  <CalendarUID>1</CalendarUID>
  <Tasks>
    <Task><UID>1</UID><ID>1</ID><Name>A &amp; B</Name>
      <OutlineLevel>1</OutlineLevel>
      <Duration>PT16H0M0S</Duration><DurationFormat>7</DurationFormat>
      <Start>2026-03-02T08:00:00</Start><Finish>2026-03-03T17:00:00</Finish></Task>
    <Task><UID>2</UID><ID>2</ID><Name>Second</Name>
      <OutlineLevel>1</OutlineLevel>
      <Duration>PT24H0M0S</Duration><DurationFormat>7</DurationFormat>
      <PredecessorLink>
        <PredecessorUID>1</PredecessorUID><Type>1</Type>
        <LinkLag>4800</LinkLag><LagFormat>7</LagFormat>
      </PredecessorLink></Task>
  </Tasks>
</Project>"#;

    #[test]
    fn reads_minimal_project() {
        let proj = read_mspdi(MINIMAL).unwrap();
        assert_eq!(proj.name, "demo");
        assert_eq!(proj.hours_per_day, 8.0);
        assert_eq!(proj.tasks.len(), 2);

        let a = &proj.tasks[0];
        assert_eq!(a.name, "A & B"); // entity decoded
        assert_eq!(a.duration_min, 960);
        assert_eq!(a.stored_start.unwrap().to_mspdi(), "2026-03-02T08:00:00");

        let b = &proj.tasks[1];
        assert_eq!(b.predecessors.len(), 1);
        let pred = b.predecessors[0];
        assert_eq!(pred.uid, 1);
        assert_eq!(pred.link, LinkType::FinishStart);
        assert_eq!(pred.lag_min, 480); // 4800 tenths-of-min = 2 days = 8h/day
    }

    #[test]
    fn reads_calendar() {
        let xml = r#"<Project><Calendars><Calendar>
          <UID>1</UID><Name>Std</Name>
          <WeekDays>
            <WeekDay><DayType>2</DayType><DayWorking>1</DayWorking>
              <WorkingTimes>
                <WorkingTime><FromTime>08:00:00</FromTime><ToTime>12:00:00</ToTime></WorkingTime>
                <WorkingTime><FromTime>13:00:00</FromTime><ToTime>17:00:00</ToTime></WorkingTime>
              </WorkingTimes></WeekDay>
            <WeekDay><DayType>1</DayType><DayWorking>0</DayWorking></WeekDay>
          </WeekDays></Calendar></Calendars></Project>"#;
        let proj = read_mspdi(xml).unwrap();
        assert_eq!(proj.calendars.len(), 1);
        let cal = &proj.calendars[0];
        assert_eq!(cal.name, "Std");
        assert_eq!(cal.week[1].as_ref().unwrap().minutes(), 480); // Monday (DayType 2) = 8h
        assert!(!cal.week[0].as_ref().unwrap().working()); // Sunday (DayType 1) off
    }

    /// `Standard` (UID 1) as Project writes a base calendar, plus `calendars`,
    /// and two tasks on calendars `a` and `b`.
    fn calendars_xml(calendars: &str, a: i32, b: i32) -> String {
        let day = |d: u32, on: bool| {
            if on {
                format!(
                    "<WeekDay><DayType>{d}</DayType><DayWorking>1</DayWorking><WorkingTimes>\
                     <WorkingTime><FromTime>08:00:00</FromTime><ToTime>12:00:00</ToTime></WorkingTime>\
                     <WorkingTime><FromTime>13:00:00</FromTime><ToTime>17:00:00</ToTime></WorkingTime>\
                     </WorkingTimes></WeekDay>"
                )
            } else {
                format!("<WeekDay><DayType>{d}</DayType><DayWorking>0</DayWorking></WeekDay>")
            }
        };
        let standard: String = (1..=7).map(|d| day(d, (2..=6).contains(&d))).collect();
        format!(
            r#"<Project><CalendarUID>1</CalendarUID><Calendars>
            <Calendar><UID>1</UID><Name>Standard</Name><IsBaseCalendar>1</IsBaseCalendar>
              <IsBaselineCalendar>0</IsBaselineCalendar><BaseCalendarUID>-1</BaseCalendarUID>
              <WeekDays>{standard}</WeekDays></Calendar>
            {calendars}
            </Calendars><Tasks>
            <Task><UID>1</UID><ID>1</ID><Name>A</Name><OutlineLevel>1</OutlineLevel>
              <Duration>PT8H0M0S</Duration><CalendarUID>{a}</CalendarUID></Task>
            <Task><UID>2</UID><ID>2</ID><Name>B</Name><OutlineLevel>1</OutlineLevel>
              <Duration>PT8H0M0S</Duration><CalendarUID>{b}</CalendarUID></Task>
            </Tasks></Project>"#
        )
    }

    /// A resource-style calendar derived from Standard with Friday off, and one
    /// that states no weekdays at all (the usual shape in Project's files).
    const DERIVED: &str = r#"
        <Calendar><UID>2</UID><Name>Night crew</Name><IsBaseCalendar>0</IsBaseCalendar>
          <IsBaselineCalendar>1</IsBaselineCalendar><BaseCalendarUID>1</BaseCalendarUID>
          <WeekDays><WeekDay><DayType>6</DayType><DayWorking>0</DayWorking></WeekDay></WeekDays>
        </Calendar>
        <Calendar><UID>3</UID><Name>Plain</Name><IsBaseCalendar>0</IsBaseCalendar>
          <IsBaselineCalendar>0</IsBaselineCalendar><BaseCalendarUID>1</BaseCalendarUID>
        </Calendar>"#;

    /// The `<Calendar>` element with this UID in written MSPDI.
    fn calendar_block(xml: &str, uid: i32) -> &str {
        let start = xml
            .find(&format!("<Calendar>\n      <UID>{uid}</UID>"))
            .unwrap();
        let end = start + xml[start..].find("</Calendar>").unwrap();
        &xml[start..end]
    }

    #[test]
    fn derived_calendars_keep_their_base_through_mspdi_and_yppx() {
        // Task A is on the calendar with no weekdays of its own: before #83 it
        // read as seven non-working days and the file was rejected.
        let proj = read_mspdi(&calendars_xml(DERIVED, 3, 2)).unwrap();
        let night = proj.calendar(2).unwrap();
        assert_eq!(night.base_calendar_uid, Some(1));
        assert!(night.is_baseline_calendar);
        let mut friday_off: [Option<DayWorking>; 7] = Default::default();
        friday_off[5] = Some(DayWorking::default());
        assert_eq!(night.week, friday_off);
        let plain = proj.calendar(3).unwrap();
        assert_eq!(plain.base_calendar_uid, Some(1));
        assert!(!plain.is_baseline_calendar);
        assert_eq!(plain.week, <[Option<DayWorking>; 7]>::default());
        let standard = proj.calendar(1).unwrap();
        assert_eq!(standard.base_calendar_uid, None);

        let xml = write_mspdi(&proj);
        assert!(calendar_block(&xml, 1).contains(
            "<Name>Standard</Name>\n      <IsBaseCalendar>1</IsBaseCalendar>\n      \
             <IsBaselineCalendar>0</IsBaselineCalendar>\n      \
             <BaseCalendarUID>-1</BaseCalendarUID>\n      <WeekDays>"
        ));
        assert!(calendar_block(&xml, 2).contains(
            "<Name>Night crew</Name>\n      <IsBaseCalendar>0</IsBaseCalendar>\n      \
             <IsBaselineCalendar>1</IsBaselineCalendar>\n      \
             <BaseCalendarUID>1</BaseCalendarUID>\n      <WeekDays>"
        ));
        let back = read_mspdi(&xml).unwrap();
        assert_eq!(back.calendars, proj.calendars);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj)).unwrap();
        assert_eq!(package.calendars, proj.calendars);
    }

    #[test]
    fn derived_calendar_writes_only_its_own_weekdays_and_a_base_writes_all_seven() {
        let mut proj = read_mspdi(&calendars_xml(DERIVED, 1, 1)).unwrap();
        proj.calendars
            .push(Calendar::base(4, "Closed", Default::default()));
        // A base calendar whose days are unstated in memory.
        proj.calendars.push(Calendar {
            week: Default::default(),
            ..Calendar::base(5, "Unstated", Default::default())
        });
        let xml = write_mspdi(&proj);
        let night = calendar_block(&xml, 2);
        assert_eq!(night.matches("<WeekDay>").count(), 1);
        assert!(night.contains("<DayType>6</DayType>\n          <DayWorking>0</DayWorking>"));
        let plain = calendar_block(&xml, 3);
        assert_eq!(plain.matches("<WeekDay>").count(), 0);
        assert!(!plain.contains("<WeekDays>"));
        assert_eq!(calendar_block(&xml, 1).matches("<WeekDay>").count(), 7);
        for uid in [4, 5] {
            let block = calendar_block(&xml, uid);
            assert_eq!(block.matches("<WeekDay>").count(), 7, "calendar {uid}");
            assert_eq!(
                block.matches("<DayWorking>0</DayWorking>").count(),
                7,
                "calendar {uid}"
            );
        }
        let back = read_mspdi(&xml).unwrap();
        assert_eq!(
            back.calendar(5).unwrap().week,
            Calendar::base(5, "", Default::default()).week
        );
    }

    #[test]
    fn reader_decides_base_or_derived_from_both_flags() {
        let cases = [
            // IsBaseCalendar wins over a stray BaseCalendarUID.
            (
                "<IsBaseCalendar>1</IsBaseCalendar><BaseCalendarUID>1</BaseCalendarUID>",
                None,
            ),
            // MSPDI's default for IsBaseCalendar is false.
            ("<BaseCalendarUID>1</BaseCalendarUID>", Some(1)),
            (
                "<IsBaseCalendar>0</IsBaseCalendar><BaseCalendarUID>-1</BaseCalendarUID>",
                None,
            ),
            ("<IsBaseCalendar>0</IsBaseCalendar>", None),
            (
                "<IsBaseCalendar>0</IsBaseCalendar><BaseCalendarUID>x</BaseCalendarUID>",
                None,
            ),
        ];
        for (flags, base) in cases {
            let cal = format!(
                "<Calendar><UID>2</UID><Name>C</Name>{flags}<WeekDays>\
                 <WeekDay><DayType>2</DayType><DayWorking>1</DayWorking><WorkingTimes>\
                 <WorkingTime><FromTime>08:00:00</FromTime><ToTime>12:00:00</ToTime></WorkingTime>\
                 </WorkingTimes></WeekDay></WeekDays></Calendar>"
            );
            let proj = read_mspdi(&calendars_xml(&cal, 1, 1)).unwrap();
            let cal = proj.calendar(2).unwrap();
            assert_eq!(cal.base_calendar_uid, base, "{flags}");
            // A base calendar's unstated days are non-working; a derived one's
            // are inherited.
            let unstated = cal.week[3].clone();
            match base {
                None => assert_eq!(unstated, Some(DayWorking::default()), "{flags}"),
                Some(_) => assert_eq!(unstated, None, "{flags}"),
            }
        }
    }

    #[test]
    fn task_on_a_derived_calendar_with_a_broken_chain_and_no_own_time_is_rejected() {
        for base in [99, 2] {
            let cal = format!(
                "<Calendar><UID>2</UID><Name>Orphan</Name><IsBaseCalendar>0</IsBaseCalendar>\
                 <BaseCalendarUID>{base}</BaseCalendarUID></Calendar>"
            );
            let error = read_mspdi(&calendars_xml(&cal, 1, 2)).unwrap_err();
            assert!(
                error.contains("calendar \"Orphan\" (UID 2) has no working time"),
                "{error}"
            );
        }
    }

    #[test]
    fn day_type_zero_does_not_overwrite_sunday() {
        let exception = "<WeekDay><DayType>0</DayType><DayWorking>1</DayWorking>\
             <TimePeriod><FromDate>2026-03-01T00:00:00</FromDate>\
             <ToDate>2026-03-01T23:59:00</ToDate></TimePeriod><WorkingTimes>\
             <WorkingTime><FromTime>08:00:00</FromTime><ToTime>12:00:00</ToTime></WorkingTime>\
             </WorkingTimes></WeekDay>";
        let cals = format!(
            "<Calendar><UID>2</UID><Name>Base</Name><IsBaseCalendar>1</IsBaseCalendar>\
             <WeekDays><WeekDay><DayType>1</DayType><DayWorking>0</DayWorking></WeekDay>\
             {exception}</WeekDays></Calendar>\
             <Calendar><UID>3</UID><Name>Derived</Name><IsBaseCalendar>0</IsBaseCalendar>\
             <BaseCalendarUID>1</BaseCalendarUID><WeekDays>{exception}</WeekDays></Calendar>"
        );
        let proj = read_mspdi(&calendars_xml(&cals, 1, 1)).unwrap();
        assert_eq!(
            proj.calendar(2).unwrap().week[0],
            Some(DayWorking::default())
        );
        assert_eq!(
            proj.calendar(3).unwrap().week,
            <[Option<DayWorking>; 7]>::default()
        );
    }

    fn empty_calendar_project() -> Project {
        let mut proj = read_mspdi(MINIMAL).unwrap();
        proj.calendars
            .push(Calendar::base(3, "Closed", Default::default()));
        proj
    }

    #[test]
    fn used_empty_calendar_is_rejected_including_default_fallback() {
        for calendar_uid in [Some(3), None, Some(999)] {
            let mut proj = empty_calendar_project();
            proj.default_calendar_uid = 3;
            proj.tasks[0].calendar_uid = calendar_uid;
            let error = read_mspdi(&write_mspdi(&proj)).unwrap_err();
            assert_eq!(
                error,
                "calendar \"Closed\" (UID 3) has no working time; task \"A & B\" (UID 1) cannot be scheduled"
            );
        }
        let mut proj = empty_calendar_project();
        proj.tasks[0].calendar_uid = Some(3);
        assert!(
            read_mspdi(&write_mspdi(&proj))
                .unwrap_err()
                .contains("Closed")
        );
    }

    #[test]
    fn empty_calendars_unused_by_leaves_are_accepted() {
        fn assert_editor_reopens(proj: &Project) {
            let loaded = read_mspdi(&write_mspdi(proj)).unwrap();
            let editor = crate::editor::Editor::new(loaded);
            assert!(read_mspdi(&write_mspdi(editor.project())).is_ok());
            assert!(crate::yppx::read_yppx(&crate::yppx::write_yppx(editor.project())).is_ok());
        }
        let mut proj = empty_calendar_project();
        assert_editor_reopens(&proj);
        proj.tasks[0].summary = true;
        proj.tasks[0].calendar_uid = Some(3);
        proj.tasks[1].outline_level = proj.tasks[0].outline_level + 1;
        assert_editor_reopens(&proj);
        proj.tasks.clear();
        proj.default_calendar_uid = 3;
        assert_editor_reopens(&proj);
    }

    #[test]
    fn empty_calendar_rejection_covers_stored_and_outline_leaves() {
        let mut proj = empty_calendar_project();
        proj.tasks[0].summary = true;
        proj.tasks[0].calendar_uid = Some(3);
        // A sibling at the same level leaves the stored summary childless.
        assert!(
            read_mspdi(&write_mspdi(&proj))
                .unwrap_err()
                .contains("Closed")
        );
        // The last row cannot have children either.
        proj.tasks.truncate(1);
        assert!(
            read_mspdi(&write_mspdi(&proj))
                .unwrap_err()
                .contains("Closed")
        );

        let mut proj = empty_calendar_project();
        proj.tasks[0].calendar_uid = Some(3);
        proj.tasks[1].outline_level = proj.tasks[0].outline_level + 1;
        // Outline children alone are insufficient: raw schedule() still treats
        // the stored Summary=0 parent as a leaf.
        assert!(!proj.tasks[0].summary);
        assert!(
            read_mspdi(&write_mspdi(&proj))
                .unwrap_err()
                .contains("Closed")
        );
    }

    #[test]
    fn working_time_midnight_and_inverted_shifts() {
        for (from, to, expected) in [
            (
                "00:00:00",
                "00:00:00",
                Some(WorkingTime { from: 0, to: 1440 }),
            ),
            (
                "16:00:00",
                "00:00:00",
                Some(WorkingTime {
                    from: 960,
                    to: 1440,
                }),
            ),
            (
                "08:00:00",
                "17:00:00",
                Some(WorkingTime {
                    from: 480,
                    to: 1020,
                }),
            ),
            ("10:00:00", "09:00:00", None),
        ] {
            let xml = format!(
                "<WorkingTime><FromTime>{from}</FromTime><ToTime>{to}</ToTime></WorkingTime>"
            );
            let mut parser = XmlParser::new(&xml);
            assert_eq!(parser.next(), Event::Start);
            assert_eq!(parse_working_time(&mut parser), expected, "{from} -> {to}");
        }
    }

    #[test]
    fn twenty_four_hour_calendar_round_trips() {
        let mut proj = Project::default();
        for day in proj.calendars[0].week.iter_mut().flatten() {
            day.times = vec![WorkingTime { from: 0, to: 1440 }];
        }
        let xml = write_mspdi(&proj);
        assert!(xml.contains("<FromTime>00:00:00</FromTime>"));
        assert!(xml.contains("<ToTime>00:00:00</ToTime>"));
        assert_eq!(read_mspdi(&xml).unwrap().calendars, proj.calendars);
        assert_eq!(
            crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj))
                .unwrap()
                .calendars,
            proj.calendars
        );
    }

    #[test]
    fn write_then_read_round_trips() {
        let orig = read_mspdi(MINIMAL).unwrap();
        let xml = write_mspdi(&orig);
        let back = read_mspdi(&xml).unwrap();

        assert_eq!(back.name, orig.name);
        assert_eq!(back.hours_per_day, orig.hours_per_day);
        assert_eq!(back.tasks.len(), orig.tasks.len());
        for (a, b) in orig.tasks.iter().zip(&back.tasks) {
            assert_eq!(a.uid, b.uid);
            assert_eq!(a.name, b.name); // '&' survives escape round-trip
            assert_eq!(a.duration_min, b.duration_min);
            assert_eq!(a.stored_start, b.stored_start);
            assert_eq!(a.predecessors, b.predecessors); // link type + lag preserved
        }
    }

    #[test]
    fn baseline_round_trips() {
        let mut proj = Project {
            calendars: vec![Calendar::standard(1)],
            ..Project::default()
        };
        proj.tasks.push(Task {
            uid: 1,
            id: 1,
            name: "A".into(),
            outline_level: 1,
            duration_min: 960,
            baselines: vec![Baseline {
                start: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
                finish: Some(DateTime::from_ymd_hm(2026, 3, 3, 17, 0)),
                ..Baseline::default()
            }],
            ..Task::default()
        });
        let back = read_mspdi(&write_mspdi(&proj)).unwrap();
        assert_eq!(back.tasks[0].baselines, proj.tasks[0].baselines);
    }

    fn project_with_baselines(baselines: &str) -> Project {
        read_mspdi(&format!("<Project><Tasks><Task><UID>1</UID><Duration>PT16H0M0S</Duration>{baselines}</Task></Tasks></Project>")).unwrap()
    }

    #[test]
    fn baseline_slot_and_duration_survive_round_trip() {
        let proj = project_with_baselines(
            "<Baseline><Number>1</Number><Start>2026-03-09T08:00:00</Start><Finish>2026-03-13T17:00:00</Finish><Duration>PT40H0M0S</Duration></Baseline>",
        );
        assert_eq!(proj.tasks[0].duration_min, 960);
        let expected = Baseline {
            number: 1,
            start: Some(DateTime::from_ymd_hm(2026, 3, 9, 8, 0)),
            finish: Some(DateTime::from_ymd_hm(2026, 3, 13, 17, 0)),
            duration_min: Some(2400),
        };
        assert_eq!(proj.tasks[0].baselines, vec![expected]);
        let xml = write_mspdi(&proj);
        let baseline = xml
            .split("<Baseline>")
            .nth(1)
            .unwrap()
            .split("</Baseline>")
            .next()
            .unwrap();
        assert_eq!(
            baseline.trim(),
            "<Number>1</Number>\n        <Start>2026-03-09T08:00:00</Start>\n        <Finish>2026-03-13T17:00:00</Finish>\n        <Duration>PT40H0M0S</Duration>"
        );
        assert_eq!(read_mspdi(&xml).unwrap().tasks[0].baselines, vec![expected]);
    }

    #[test]
    fn multiple_baseline_slots_round_trip() {
        let mut proj = project_with_baselines(
            "<Baseline><Number>1</Number><Start>2026-03-09T08:00:00</Start><Finish>2026-03-13T17:00:00</Finish><Duration>PT40H0M0S</Duration></Baseline><Baseline><Number>0</Number><Start>2026-03-02T08:00:00</Start><Finish>2026-03-04T17:00:00</Finish><Duration>PT24H0M0S</Duration></Baseline>",
        );
        assert_eq!(
            proj.tasks[0]
                .baselines
                .iter()
                .map(|b| b.number)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        let expected = proj.tasks[0].baselines.clone();
        // Public model callers can provide unsorted slots; lookup and output still work.
        proj.tasks[0].baselines.reverse();
        assert_eq!(proj.tasks[0].baseline(0), Some(&expected[0]));
        let xml = write_mspdi(&proj);
        assert!(xml.find("<Number>0").unwrap() < xml.find("<Number>1").unwrap());
        assert_eq!(read_mspdi(&xml).unwrap().tasks[0].baselines, expected);
        let back = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj)).unwrap();
        assert_eq!(back.tasks[0].baselines, expected);
    }

    #[test]
    fn partial_baselines_emit_only_recorded_fields() {
        for field in [
            "<Start>2026-03-09T08:00:00</Start>",
            "<Finish>2026-03-13T17:00:00</Finish>",
            "<Duration>PT0H0M0S</Duration>",
        ] {
            let proj =
                project_with_baselines(&format!("<Baseline><Number>1</Number>{field}</Baseline>"));
            let xml = write_mspdi(&proj);
            let baseline = xml
                .split("<Baseline>")
                .nth(1)
                .unwrap()
                .split("</Baseline>")
                .next()
                .unwrap();
            assert_eq!(
                baseline.trim(),
                format!("<Number>1</Number>\n        {field}")
            );
            assert_eq!(
                read_mspdi(&xml).unwrap().tasks[0].baselines,
                proj.tasks[0].baselines
            );
        }
    }

    #[test]
    fn fallible_baseline_duration_keeps_valid_values() {
        for (source, expected) in [
            ("PT16H0M0S", 960),
            ("PT0H0M0S", 0),
            (" PT8H30M0S ", 510),
            ("PT1.5H", 90),
            ("P1DT1H", 1500),
            ("P1D", 1440),
            ("PT30S", 1),
        ] {
            assert_eq!(try_iso8601_to_minutes(source), Some(expected));
            let proj = project_with_baselines(&format!(
                "<Baseline><Number>1</Number><Duration>{source}</Duration></Baseline>"
            ));
            assert_eq!(
                proj.tasks[0].baseline(1).unwrap().duration_min,
                Some(expected)
            );
        }
        for source in [
            "",
            "P",
            "1H",
            "P1DT",
            "PT1M1H",
            "PT1H1H",
            "PT9999999999999999999999H",
            "PT-0H",
            "P-1D",
            "PT1H-30M",
            "PT+1H",
        ] {
            assert_eq!(try_iso8601_to_minutes(source), None, "{source}");
        }
        // The existing task parser deliberately remains permissive.
        assert_eq!(iso8601_to_minutes("1D"), 1440);
        assert_eq!(iso8601_to_minutes("PT1Hgarbage"), 60);
    }

    #[test]
    fn invalid_baseline_duration_never_fabricates_zero() {
        for duration in [
            "<Duration/>",
            "<Duration>   </Duration>",
            "<Duration>garbage</Duration>",
            "<Duration>PT</Duration>",
            "<Duration>PT1.2.3H</Duration>",
            "<Duration>PT1Hgarbage</Duration>",
            "<Duration>PT1H2</Duration>",
            "<Duration>PT-0H</Duration>",
        ] {
            for start in ["", "<Start>2026-03-09T08:00:00</Start>"] {
                let proj = project_with_baselines(&format!(
                    "<Baseline><Number>1</Number>{start}{duration}</Baseline>"
                ));
                if start.is_empty() {
                    assert!(proj.tasks[0].baselines.is_empty(), "{duration}");
                } else {
                    assert_eq!(
                        proj.tasks[0].baselines,
                        vec![Baseline {
                            number: 1,
                            start: Some(DateTime::from_ymd_hm(2026, 3, 9, 8, 0)),
                            ..Baseline::default()
                        }],
                        "{duration}"
                    );
                }
                let xml = write_mspdi(&proj);
                // Ignore the task's current Duration when checking baseline output.
                let baseline = xml.split("<Baseline>").nth(1);
                assert_eq!(baseline.is_some(), !start.is_empty());
                if let Some(baseline) = baseline {
                    assert!(
                        !baseline
                            .split("</Baseline>")
                            .next()
                            .unwrap()
                            .contains("<Duration")
                    );
                }
                assert_eq!(
                    read_mspdi(&xml).unwrap().tasks[0].baselines,
                    proj.tasks[0].baselines
                );
            }
        }
    }

    #[test]
    fn missing_and_empty_baselines_write_no_element() {
        for source in ["", "<Baseline/>", "<Baseline><Number>1</Number></Baseline>"] {
            let proj = project_with_baselines(source);
            assert!(proj.tasks[0].baselines.is_empty());
            assert!(!write_mspdi(&proj).contains("<Baseline>"));
        }
    }

    #[test]
    fn baseline_numbers_default_only_when_absent() {
        let original = "<Baseline><Duration>PT8H0M0S</Duration></Baseline>";
        for number in ["-1", "11", "256", "x", ""] {
            let proj = project_with_baselines(&format!(
                "{original}<Baseline><Number>{number}</Number><Duration>PT40H0M0S</Duration></Baseline>"
            ));
            assert_eq!(
                proj.tasks[0].baselines,
                vec![Baseline {
                    duration_min: Some(480),
                    ..Baseline::default()
                }]
            );
        }
        let proj = project_with_baselines(
            "<Baseline><Number> 10 </Number><Duration>PT8H0M0S</Duration></Baseline>",
        );
        assert_eq!(proj.tasks[0].baseline(10).unwrap().duration_min, Some(480));
        assert_eq!(
            read_mspdi(&write_mspdi(&proj)).unwrap().tasks[0].baselines,
            proj.tasks[0].baselines
        );
    }

    #[test]
    fn duplicate_baseline_replaces_entire_record() {
        let proj = project_with_baselines(
            "<Baseline><Number>1</Number><Start>2026-03-09T08:00:00</Start><Duration>PT40H0M0S</Duration></Baseline><Baseline><Number>1</Number><Finish>2026-03-13T17:00:00</Finish></Baseline>",
        );
        assert_eq!(
            proj.tasks[0].baselines,
            vec![Baseline {
                number: 1,
                finish: Some(DateTime::from_ymd_hm(2026, 3, 13, 17, 0)),
                ..Baseline::default()
            }]
        );
    }

    #[test]
    fn calendar_write_round_trips_working_times() {
        let proj = Project::default(); // one Standard calendar
        let xml = write_mspdi(&proj);
        let back = read_mspdi(&xml).unwrap();
        assert_eq!(back.calendars.len(), 1);
        let cal = &back.calendars[0];
        assert_eq!(cal.week[1].as_ref().unwrap().minutes(), 480); // Monday still 8h
        assert_eq!(cal.week[1].as_ref().unwrap().times.len(), 2); // two shifts preserved
        assert!(
            !cal.week[0].as_ref().unwrap().working() && !cal.week[6].as_ref().unwrap().working()
        ); // weekend off
    }

    // ---- task fields (#80) ----

    fn task_project(tasks: &str) -> Project {
        read_mspdi(&format!(
            "<Project><StartDate>2026-03-02T08:00:00</StartDate><Tasks>{tasks}</Tasks></Project>"
        ))
        .unwrap()
    }

    /// Every stored task field #80 keeps, with Project's non-default values.
    const TASK_FIELDS: &str = "<Task><UID>1</UID>\
        <GUID>651A2669-EF7E-F111-A0F9-34C93D776CA2</GUID><ID>1</ID><Name>Pour</Name>\
        <Active>0</Active><Manual>0</Manual><Type>1</Type><IsNull>0</IsNull>\
        <CreateDate>2026-07-13T23:14:00</CreateDate><WBS>1.2</WBS>\
        <OutlineNumber>9.9</OutlineNumber><OutlineLevel>1</OutlineLevel><Priority>900</Priority>\
        <Duration>PT8H0M0S</Duration><Work>PT16H30M0S</Work><EffortDriven>1</EffortDriven>\
        <Recurring>1</Recurring><OverAllocated>1</OverAllocated><Estimated>1</Estimated>\
        <IsSubproject>1</IsSubproject><IsSubprojectReadOnly>1</IsSubprojectReadOnly>\
        <ExternalTask>1</ExternalTask><Cost>1250.50</Cost>\
        <Deadline>2026-03-20T17:00:00</Deadline><LevelAssignments>0</LevelAssignments>\
        <LevelingCanSplit>0</LevelingCanSplit><LevelingDelay>4800</LevelingDelay>\
        <LevelingDelayFormat>7</LevelingDelayFormat><IgnoreResourceCalendar>1</IgnoreResourceCalendar>\
        <HideBar>1</HideBar><Rollup>1</Rollup><EarnedValueMethod>1</EarnedValueMethod></Task>";

    #[test]
    fn task_fields_are_read() {
        let t = &task_project(TASK_FIELDS).tasks[0];
        assert_eq!(
            t,
            &Task {
                uid: 1,
                id: 1,
                name: "Pour".into(),
                outline_level: 1,
                duration_min: 480,
                guid: Some("651A2669-EF7E-F111-A0F9-34C93D776CA2".into()),
                create_date: Some(DateTime::from_ymd_hm(2026, 7, 13, 23, 14)),
                wbs: Some("1.2".into()),
                task_type: Some(TaskType::FixedDuration),
                active: Some(false),
                effort_driven: Some(true),
                estimated: Some(true),
                priority: Some(900),
                deadline: Some(DateTime::from_ymd_hm(2026, 3, 20, 17, 0)),
                level_assignments: Some(false),
                leveling_can_split: Some(false),
                leveling_delay: Some(4800),
                leveling_delay_format: Some(7),
                ignore_resource_calendar: Some(true),
                earned_value_method: Some(1),
                recurring: Some(true),
                hide_bar: Some(true),
                rollup: Some(true),
                external_task: Some(true),
                is_subproject: Some(true),
                is_subproject_read_only: Some(true),
                work_min: Some(990),
                cost: Rate::parse("1250.50"),
                over_allocated: Some(true),
                ..Task::default()
            }
        );
        assert!(!t.is_active());
    }

    #[test]
    fn task_fields_survive_mspdi_and_native_package_round_trips() {
        let proj = task_project(TASK_FIELDS);
        let xml = write_mspdi(&proj);
        for element in [
            "<GUID>651A2669-EF7E-F111-A0F9-34C93D776CA2</GUID>",
            "<Active>0</Active>",
            "<Type>1</Type>",
            "<CreateDate>2026-07-13T23:14:00</CreateDate>",
            "<WBS>1.2</WBS>",
            "<Priority>900</Priority>",
            "<Work>PT16H30M0S</Work>",
            "<EffortDriven>1</EffortDriven>",
            "<Recurring>1</Recurring>",
            "<OverAllocated>1</OverAllocated>",
            "<Estimated>1</Estimated>",
            "<IsSubproject>1</IsSubproject>",
            "<IsSubprojectReadOnly>1</IsSubprojectReadOnly>",
            "<ExternalTask>1</ExternalTask>",
            "<Cost>1250.50</Cost>",
            "<Deadline>2026-03-20T17:00:00</Deadline>",
            "<LevelAssignments>0</LevelAssignments>",
            "<LevelingCanSplit>0</LevelingCanSplit>",
            "<LevelingDelay>4800</LevelingDelay>",
            "<LevelingDelayFormat>7</LevelingDelayFormat>",
            "<IgnoreResourceCalendar>1</IgnoreResourceCalendar>",
            "<HideBar>1</HideBar>",
            "<Rollup>1</Rollup>",
            "<EarnedValueMethod>1</EarnedValueMethod>",
        ] {
            assert!(xml.contains(element), "missing {element}");
        }
        // A task is not a blank row; the stored OutlineNumber is recomputed.
        assert!(xml.contains("<IsNull>0</IsNull>"));
        assert!(xml.contains("<OutlineNumber>1</OutlineNumber>"));
        assert_eq!(read_mspdi(&xml).unwrap().tasks, proj.tasks);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj)).unwrap();
        assert_eq!(package.tasks, proj.tasks);
    }

    #[test]
    fn every_task_type_and_flag_value_round_trips() {
        for code in 0..=2 {
            let proj = task_project(&format!("<Task><UID>1</UID><Type>{code}</Type></Task>"));
            assert_eq!(proj.tasks[0].task_type.map(TaskType::code), Some(code));
            let xml = write_mspdi(&proj);
            assert!(xml.contains(&format!("<Type>{code}</Type>")));
            assert_eq!(read_mspdi(&xml).unwrap().tasks, proj.tasks);
        }
        for (text, value) in [("1", true), ("true", true), ("0", false), ("false", false)] {
            let proj = task_project(&format!(
                "<Task><UID>1</UID><Active>{text}</Active><Estimated>{text}</Estimated></Task>"
            ));
            assert_eq!(proj.tasks[0].active, Some(value));
            assert_eq!(proj.tasks[0].estimated, Some(value));
            assert_eq!(read_mspdi(&write_mspdi(&proj)).unwrap().tasks, proj.tasks);
        }
    }

    /// Optional task elements #80 keeps. IsNull is not among them: every row
    /// states it.
    const NEW_TASK_ELEMENTS: [&str; 24] = [
        "GUID",
        "Active",
        "Type",
        "CreateDate",
        "WBS",
        "Priority",
        "Work",
        "EffortDriven",
        "Recurring",
        "OverAllocated",
        "Estimated",
        "IsSubproject",
        "IsSubprojectReadOnly",
        "ExternalTask",
        "Cost",
        "Deadline",
        "LevelAssignments",
        "LevelingCanSplit",
        "LevelingDelay",
        "LevelingDelayFormat",
        "IgnoreResourceCalendar",
        "HideBar",
        "Rollup",
        "EarnedValueMethod",
    ];

    /// The `<Tasks>` section of a written file.
    fn task_xml(xml: &str) -> &str {
        &xml[xml.find("<Tasks>").unwrap()..xml.find("</Tasks>").unwrap()]
    }

    #[test]
    fn absent_task_fields_stay_absent() {
        let proj = task_project(
            "<Task><UID>1</UID><ID>1</ID><Name>A</Name><Duration>PT8H0M0S</Duration></Task>",
        );
        assert_eq!(
            proj.tasks[0],
            Task {
                uid: 1,
                id: 1,
                name: "A".into(),
                duration_min: 480,
                ..Task::default()
            }
        );
        let xml = write_mspdi(&proj);
        for name in NEW_TASK_ELEMENTS {
            assert!(
                !task_xml(&xml).contains(&format!("<{name}>")),
                "{name}: {xml}"
            );
        }
        assert!(xml.contains("<IsNull>0</IsNull>"));
        assert_eq!(read_mspdi(&xml).unwrap().tasks, proj.tasks);
    }

    #[test]
    fn invalid_task_fields_stay_absent() {
        for element in [
            "<Type>x</Type>",
            "<Type>3</Type>",
            "<Type>-1</Type>",
            "<Type/>",
            "<Priority>abc</Priority>",
            "<Priority>1001</Priority>",
            "<Priority>-1</Priority>",
            "<Active>maybe</Active>",
            "<Active/>",
            "<Estimated>2</Estimated>",
            "<EffortDriven>yes</EffortDriven>",
            "<Deadline>soon</Deadline>",
            "<CreateDate>NA</CreateDate>",
            "<Work>banana</Work>",
            "<Cost>NaN</Cost>",
            "<LevelingDelay>1.5</LevelingDelay>",
            "<LevelingDelayFormat>x</LevelingDelayFormat>",
            "<EarnedValueMethod>x</EarnedValueMethod>",
            "<GUID></GUID>",
        ] {
            let proj = task_project(&format!("<Task><UID>1</UID>{element}</Task>"));
            assert_eq!(
                proj.tasks[0],
                Task {
                    uid: 1,
                    ..Task::default()
                },
                "{element}"
            );
            let xml = write_mspdi(&proj);
            for name in NEW_TASK_ELEMENTS {
                assert!(
                    !task_xml(&xml).contains(&format!("<{name}>")),
                    "{element}: {xml}"
                );
            }
        }
        // The Priority range is inclusive.
        for n in [0, 1000] {
            let proj = task_project(&format!(
                "<Task><UID>1</UID><Priority>{n}</Priority></Task>"
            ));
            assert_eq!(proj.tasks[0].priority, Some(n));
        }
    }

    fn element_names(xml: &str) -> Vec<String> {
        let mut parser = XmlParser::new(xml);
        let mut names = Vec::new();
        loop {
            match parser.next() {
                Event::Start => names.push(parser.name().to_string()),
                Event::Eof => break,
                _ => {}
            }
        }
        names
    }

    #[test]
    fn task_children_follow_schema_sequence() {
        // The MSPDI Task sequence, as Project 2024 writes it (checked over the
        // 1569 tasks of a private Project 2024 corpus) and as Microsoft's XML
        // Schema for the Tasks Element lists it. That corpus has no task-level
        // ActualCost or ActualWork; their places are the schema's.
        let mut proj = task_project(TASK_FIELDS);
        let t = &mut proj.tasks[0];
        t.manual = true;
        t.stored_start = Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0));
        t.stored_finish = Some(DateTime::from_ymd_hm(2026, 3, 2, 17, 0));
        t.manual_start = t.stored_start;
        t.manual_finish = t.stored_finish;
        t.manual_duration_min = Some(480);
        t.calendar_uid = Some(1);
        t.constraint = ConstraintType::StartNoEarlierThan;
        t.constraint_date = t.stored_start;
        t.set_baseline_slot(Baseline {
            number: 0,
            duration_min: Some(480),
            ..Baseline::default()
        });
        let progress = task_project(PROGRESS).tasks.remove(0);
        let t = &mut proj.tasks[0];
        t.percent_complete = progress.percent_complete;
        t.percent_work_complete = progress.percent_work_complete;
        t.physical_percent_complete = progress.physical_percent_complete;
        t.actual_start = progress.actual_start;
        t.actual_finish = progress.actual_finish;
        t.stop = progress.stop;
        t.resume = progress.resume;
        t.actual_duration_min = progress.actual_duration_min;
        t.remaining_duration_min = progress.remaining_duration_min;
        t.actual_work_min = progress.actual_work_min;
        t.remaining_work_min = progress.remaining_work_min;
        t.actual_cost = progress.actual_cost.clone();
        t.remaining_cost = progress.remaining_cost.clone();
        t.start_variance = progress.start_variance;
        t.finish_variance = progress.finish_variance;
        t.work_variance = progress.work_variance.clone();
        proj.tasks.insert(
            0,
            Task {
                uid: 2,
                outline_level: 1,
                duration_min: 480,
                ..Task::default()
            },
        );
        proj.tasks[1].predecessors.push(Predecessor {
            uid: 2,
            link: LinkType::FinishStart,
            lag_min: 0,
        });
        let mut xml = String::new();
        let sched = crate::schedule::schedule(&proj);
        write_task(
            &mut xml,
            &proj.tasks[1],
            &Computed {
                outline_number: Some("2"),
                result: sched.get(1),
            },
        );
        assert_eq!(
            element_names(&xml),
            [
                "Task",
                "UID",
                "GUID",
                "ID",
                "Name",
                "Active",
                "Manual",
                "Type",
                "IsNull",
                "CreateDate",
                "WBS",
                "OutlineNumber",
                "OutlineLevel",
                "Priority",
                "Start",
                "Finish",
                "Duration",
                "ManualStart",
                "ManualFinish",
                "ManualDuration",
                "DurationFormat",
                "Work",
                "Stop",
                "Resume",
                "EffortDriven",
                "Recurring",
                "OverAllocated",
                "Estimated",
                "Milestone",
                "Summary",
                "Critical",
                "IsSubproject",
                "IsSubprojectReadOnly",
                "ExternalTask",
                "EarlyStart",
                "EarlyFinish",
                "LateStart",
                "LateFinish",
                "StartVariance",
                "FinishVariance",
                "WorkVariance",
                "FreeSlack",
                "TotalSlack",
                "StartSlack",
                "FinishSlack",
                "PercentComplete",
                "PercentWorkComplete",
                "Cost",
                "ActualStart",
                "ActualFinish",
                "ActualDuration",
                "ActualCost",
                "ActualWork",
                "RemainingDuration",
                "RemainingCost",
                "RemainingWork",
                "ConstraintType",
                "CalendarUID",
                "ConstraintDate",
                "Deadline",
                "LevelAssignments",
                "LevelingCanSplit",
                "LevelingDelay",
                "LevelingDelayFormat",
                "IgnoreResourceCalendar",
                "HideBar",
                "Rollup",
                "PhysicalPercentComplete",
                "EarnedValueMethod",
                "PredecessorLink",
                "PredecessorUID",
                "Type",
                "LinkLag",
                "LagFormat",
                "Baseline",
                "Number",
                "Duration",
            ]
        );
        // IsNull sits between Type and CreateDate on a blank row.
        let blank = Task {
            uid: 3,
            is_null: true,
            task_type: Some(TaskType::FixedUnits),
            create_date: t_date(),
            ..Task::default()
        };
        let mut xml = String::new();
        write_task(
            &mut xml,
            &blank,
            &Computed {
                outline_number: None,
                result: None,
            },
        );
        assert_eq!(
            element_names(&xml),
            [
                "Task",
                "UID",
                "ID",
                "Type",
                "IsNull",
                "CreateDate",
                "OutlineLevel"
            ]
        );
    }

    fn t_date() -> Option<DateTime> {
        Some(DateTime::from_ymd_hm(2026, 7, 13, 23, 14))
    }

    /// The text of the first `<name>` in the task with this UID.
    fn written(xml: &str, uid: i32, name: &str) -> Option<String> {
        let open = format!("<UID>{uid}</UID>");
        let task = xml.split("<Task>").find(|t| t.contains(&open))?;
        let task = &task[..task.find("</Task>")?];
        let start = task.find(&format!("<{name}>"))? + name.len() + 2;
        Some(task[start..start + task[start..].find('<')?].to_string())
    }

    #[test]
    fn computed_task_fields_come_from_the_schedule_not_the_file() {
        // B has 16h of free and total slack behind A; the file claims otherwise.
        let proj = task_project(
            "<Task><UID>1</UID><ID>1</ID><Name>A</Name><OutlineLevel>1</OutlineLevel>
               <Duration>PT24H0M0S</Duration></Task>
             <Task><UID>2</UID><ID>2</ID><Name>B</Name><OutlineLevel>1</OutlineLevel>
               <Duration>PT8H0M0S</Duration><Critical>1</Critical>
               <EarlyStart>1999-01-01T08:00:00</EarlyStart><TotalSlack>-999</TotalSlack>
               <StartSlack>7</StartSlack><OutlineNumber>7.7</OutlineNumber></Task>",
        );
        let xml = write_mspdi(&proj);
        let get =
            |uid, name| written(&xml, uid, name).unwrap_or_else(|| panic!("{uid} {name}: {xml}"));
        assert_eq!(get(2, "OutlineNumber"), "2");
        assert_eq!(get(2, "Critical"), "0");
        assert_eq!(get(2, "EarlyStart"), "2026-03-02T08:00:00");
        assert_eq!(get(2, "EarlyFinish"), "2026-03-02T17:00:00");
        assert_eq!(get(2, "LateStart"), "2026-03-04T08:00:00");
        assert_eq!(get(2, "LateFinish"), "2026-03-04T17:00:00");
        // 16 working hours, in tenths of a minute.
        for name in ["TotalSlack", "FreeSlack", "StartSlack", "FinishSlack"] {
            assert_eq!(get(2, name), "9600", "{name}");
        }
        assert_eq!(get(1, "Critical"), "1");
        assert_eq!(get(1, "TotalSlack"), "0");
        assert_eq!(get(1, "OutlineNumber"), "1");
    }

    #[test]
    fn outline_numbers_treat_a_jumped_level_as_one_slot() {
        let rows = |levels: &[u32]| -> Vec<Option<String>> {
            let tasks: Vec<Task> = levels
                .iter()
                .map(|&outline_level| Task {
                    outline_level,
                    ..Task::default()
                })
                .collect();
            outline_numbers(&tasks)
        };
        let numbers = |ns: &[&str]| -> Vec<Option<String>> {
            ns.iter().map(|n| Some(n.to_string())).collect()
        };
        // A repeated jumped row is the jumped row's sibling.
        assert_eq!(rows(&[1, 3, 3]), numbers(&["1", "1.1", "1.2"]));
        // A shallower row after a jump, still under the same parent, takes
        // the jumped row's slot: its next sibling, not a second "1.1".
        assert_eq!(rows(&[1, 3, 2]), numbers(&["1", "1.1", "1.2"]));
        assert_eq!(rows(&[1, 3, 2, 3]), numbers(&["1", "1.1", "1.2", "1.2.1"]));
        assert_eq!(rows(&[1, 2, 1]), numbers(&["1", "1.1", "2"]));
        assert_eq!(rows(&[2, 2, 1]), numbers(&["1", "2", "3"]));
    }

    #[test]
    fn outline_numbers_follow_levels_and_skip_blank_rows() {
        let row = |outline_level, is_null| Task {
            outline_level,
            is_null,
            ..Task::default()
        };
        let tasks = [
            row(0, false),
            row(1, false),
            row(2, false),
            row(0, true),
            row(2, false),
            row(4, false),
            row(1, false),
            row(3, false),
            row(1, true),
            row(2, false),
        ];
        assert_eq!(
            outline_numbers(&tasks),
            [
                Some("0"),
                Some("1"),
                Some("1.1"),
                None,
                Some("1.2"),
                Some("1.2.1"),
                Some("2"),
                Some("2.1"),
                None,
                Some("2.2"),
            ]
            .map(|n| n.map(String::from))
        );
    }

    const BLANK_ROW: &str = "<Task><UID>1</UID><ID>1</ID><Name>Phase</Name><OutlineLevel>1</OutlineLevel>
          <Summary>1</Summary></Task>
        <Task><UID>2</UID><ID>2</ID><Name>A</Name><OutlineLevel>2</OutlineLevel>
          <Duration>PT16H0M0S</Duration></Task>
        <Task><UID>3</UID><ID>3</ID><IsNull>1</IsNull><CreateDate>2026-07-13T23:14:00</CreateDate></Task>
        <Task><UID>4</UID><ID>4</ID><Name>B</Name><OutlineLevel>2</OutlineLevel>
          <Duration>PT8H0M0S</Duration>
          <PredecessorLink><PredecessorUID>2</PredecessorUID><Type>1</Type></PredecessorLink></Task>";

    #[test]
    fn a_blank_row_keeps_its_mode_constraint_and_milestone_flag() {
        let proj = task_project(
            "<Task><UID>1</UID><ID>1</ID><IsNull>1</IsNull><Manual>1</Manual>
               <Milestone>1</Milestone><Duration>PT8H0M0S</Duration>
               <ConstraintType>4</ConstraintType><ConstraintDate>2026-03-04T08:00:00</ConstraintDate>
               <Summary>0</Summary></Task>",
        );
        let t = &proj.tasks[0];
        assert!(t.is_null && t.manual && t.milestone);
        assert_eq!(t.constraint, ConstraintType::StartNoEarlierThan);
        let xml = write_mspdi(&proj);
        assert_eq!(
            element_names(task_xml(&xml))
                .into_iter()
                .skip(2)
                .collect::<Vec<_>>(),
            [
                "UID",
                "ID",
                "Manual",
                "IsNull",
                "OutlineLevel",
                "Duration",
                "Milestone",
                "ConstraintType",
                "ConstraintDate"
            ]
        );
        assert_eq!(read_mspdi(&xml).unwrap().tasks, proj.tasks);
        // Defaults stay unstated on a blank row.
        let plain = task_project("<Task><UID>1</UID><IsNull>1</IsNull></Task>");
        let xml = write_mspdi(&plain);
        for name in ["Manual", "Milestone", "Summary", "ConstraintType"] {
            assert!(!task_xml(&xml).contains(&format!("<{name}>")), "{name}");
        }
    }

    #[test]
    fn blank_row_round_trips_without_computed_fields() {
        let proj = task_project(BLANK_ROW);
        let blank = &proj.tasks[2];
        assert!(blank.is_null);
        assert_eq!((blank.uid, blank.id, blank.outline_level), (3, 3, 0));
        let xml = write_mspdi(&proj);
        let blank_xml = xml.split("<Task>").nth(3).unwrap();
        assert_eq!(
            blank_xml
                .split("</Task>")
                .next()
                .unwrap()
                .split_whitespace()
                .collect::<String>(),
            "<UID>3</UID><ID>3</ID><IsNull>1</IsNull>\
             <CreateDate>2026-07-13T23:14:00</CreateDate><OutlineLevel>0</OutlineLevel>"
        );
        // The rows around it keep their outline numbers and results.
        assert_eq!(written(&xml, 4, "OutlineNumber").as_deref(), Some("1.2"));
        assert_eq!(
            written(&xml, 4, "EarlyStart").as_deref(),
            Some("2026-03-04T08:00:00")
        );
        assert_eq!(read_mspdi(&xml).unwrap().tasks, proj.tasks);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj)).unwrap();
        assert_eq!(package.tasks, proj.tasks);
    }

    /// The `<Project>` header of a written file: each child up to the first
    /// collection, as (name, decoded text); a child with children gets "<block>".
    fn header_of(xml: &str) -> Vec<(String, String)> {
        let mut p = XmlParser::new(xml);
        while !(p.next() == Event::Start && p.name() == "Project") {}
        let mut header = Vec::new();
        loop {
            match p.next() {
                Event::Start if p.name() == "Tasks" => break,
                Event::Start => {
                    let name = p.name().to_string();
                    let text = leaf_text_of(&mut p).unwrap_or_else(|| "<block>".into());
                    header.push((name, text));
                }
                Event::End | Event::Eof => break,
                Event::Text => {}
            }
        }
        header
    }

    /// Issue #82's list of the options a save lost, typed out independently of
    /// `PROJECT_HEADER` so a gap in that table cannot hide here.
    const ISSUE_82_OPTIONS: &[&str] = &[
        "ScheduleFromStart",
        "HonorConstraints",
        "NewTasksAreManual",
        "NewTasksEffortDriven",
        "NewTasksEstimated",
        "DefaultTaskType",
        "Autolink",
        "CriticalSlackLimit",
        "MultipleCriticalPaths",
        "DefaultStartTime",
        "DefaultFinishTime",
        "DaysPerMonth",
        "WeekStartDay",
        "FYStartDate",
        "FiscalYearStart",
        "SplitsInProgressTasks",
        "MoveCompletedEndsBack",
        "MoveCompletedEndsForward",
        "MoveRemainingStartsBack",
        "MoveRemainingStartsForward",
        "SpreadPercentComplete",
        "SpreadActualCost",
        "TaskUpdatesResource",
        "ActualsInSync",
        "EditableActualCosts",
        "EarnedValueMethod",
        "DefaultTaskEVMethod",
        "BaselineForEarnedValue",
        "DefaultFixedCostAccrual",
        "CurrencySymbol",
        "CurrencyCode",
        "CurrencyDigits",
        "CurrencySymbolPosition",
        "DurationFormat",
        "WorkFormat",
        "CurrentDate",
        "FinishDate",
        "Author",
        "GUID",
        "CreationDate",
        "LastSaved",
        "Revision",
        "SaveVersion",
    ];

    /// A header carrying every schema option, in reverse schema order so the
    /// writer's order cannot come from the input's. Modeled fields get values
    /// they format back identically; the rest get a value unique to them.
    fn full_header() -> Vec<(String, String)> {
        PROJECT_HEADER
            .iter()
            .map(|&name| {
                let text = match name {
                    "Name" => "Plan".to_string(),
                    "Title" => "Backward plan".to_string(),
                    "StartDate" => "2026-03-02T08:00:00".to_string(),
                    "CalendarUID" => "1".to_string(),
                    "MinutesPerDay" => "420".to_string(),
                    "MinutesPerWeek" => "2100".to_string(),
                    "HonorConstraints" => "0".to_string(),
                    "NewTasksAreManual" => "1".to_string(),
                    "ScheduleFromStart" => "0".to_string(),
                    _ => format!("{name}-value"),
                };
                (name.to_string(), text)
            })
            .collect()
    }

    fn header_xml(header: &[(String, String)], extra: &str) -> String {
        let mut xml = String::from("<Project>");
        for (name, text) in header.iter().rev() {
            xml.push_str(&format!("<{name}>{text}</{name}>"));
        }
        xml.push_str(extra);
        xml.push_str("<Tasks/></Project>");
        xml
    }

    #[test]
    fn every_project_option_survives_mspdi_and_yppx_saves_in_schema_order() {
        let expected = full_header();
        for name in ISSUE_82_OPTIONS {
            assert!(expected.iter().any(|(n, _)| n == name), "{name} untested");
        }
        let proj = read_mspdi(&header_xml(&expected, "")).unwrap();
        assert_eq!(proj.option("ScheduleFromStart"), Some("0"));
        let xml = write_mspdi(&proj);
        // Every option once, with its text, in schema order.
        assert_eq!(header_of(&xml), expected);
        assert!(xml.contains("<ScheduleFromStart>0</ScheduleFromStart>"));
        // A save stores the options in schema order; the saved file reads back
        // to the same header.
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj)).unwrap();
        assert_eq!(header_of(&write_mspdi(&package)), expected);
    }

    #[test]
    fn project_options_keep_xml_specials_and_empty_values() {
        let proj = read_mspdi(
            "<Project><Author>A &amp; B</Author><Subject/>\
             <CurrencySymbol>&lt;€&gt;</CurrencySymbol></Project>",
        )
        .unwrap();
        assert_eq!(proj.option("Author"), Some("A & B"));
        assert_eq!(proj.option("Subject"), Some(""));
        let xml = write_mspdi(&proj);
        assert!(xml.contains("<Author>A &amp; B</Author>"));
        assert!(xml.contains("<CurrencySymbol>&lt;€&gt;</CurrencySymbol>"));
        assert!(xml.contains("<Subject></Subject>"));
        let back = read_mspdi(&xml).unwrap();
        for name in ["Author", "Subject", "CurrencySymbol"] {
            assert_eq!(back.option(name), proj.option(name), "{name}");
        }
    }

    #[test]
    fn unknown_leaf_options_survive_after_the_schema_ones_and_blocks_do_not() {
        let proj = read_mspdi(
            "<Project><ZzFutureOption>7</ZzFutureOption><Author>Me</Author>\
             <ZzBlock><A>1</A></ZzBlock><ZzRepeat>a</ZzRepeat>\
             <ExtendedAttributes><ExtendedAttribute><FieldID>1</FieldID>\
             </ExtendedAttribute></ExtendedAttributes>\
             <ZzOther>x</ZzOther><ZzRepeat>b</ZzRepeat><Tasks/></Project>",
        )
        .unwrap();
        let xml = write_mspdi(&proj);
        let header = header_of(&xml);
        let names: Vec<&str> = header.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "Name",
                "Author",
                "CalendarUID",
                "MinutesPerDay",
                "MinutesPerWeek",
                "HonorConstraints",
                "NewTasksAreManual",
                "ZzFutureOption",
                "ZzRepeat",
                "ZzOther",
            ]
        );
        // A repeat keeps its first place and takes the later value.
        assert_eq!(
            header[7..],
            [
                ("ZzFutureOption".to_string(), "7".to_string()),
                ("ZzRepeat".to_string(), "b".to_string()),
                ("ZzOther".to_string(), "x".to_string()),
            ]
        );
        assert!(!xml.contains("ZzBlock") && !xml.contains("<A>") && !xml.contains("FieldID"));
    }

    #[test]
    fn namespaced_or_attributed_header_leaves_are_not_stored() {
        let proj = read_mspdi(
            r#"<Project xmlns="http://schemas.microsoft.com/project" xmlns:x="urn:x">
                 <x:Ext>1</x:Ext>
                 <Other xmlns="urn:other">2</Other>
                 <Nil xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:nil="true"/>
                 <Author>Me</Author>
                 <Tasks/>
               </Project>"#,
        )
        .unwrap();
        // Only the plain leaf is an option; a save could not rebind the rest.
        assert_eq!(proj.options, [("Author".to_string(), "Me".to_string())]);
        let xml = write_mspdi(&proj);
        assert!(!xml.contains("x:") && !xml.contains("urn:other"));
        assert!(!xml.contains("<Other>") && !xml.contains("<Nil>"));
    }

    #[test]
    fn absent_project_options_stay_absent() {
        let proj = read_mspdi("<Project><HoursPerDay>7</HoursPerDay></Project>").unwrap();
        // HoursPerDay is modeled (written back as MinutesPerDay), not stored.
        assert!(proj.options.is_empty());
        let header = header_of(&write_mspdi(&proj));
        assert_eq!(
            header,
            [
                ("Name", ""),
                ("CalendarUID", "1"),
                ("MinutesPerDay", "420"),
                ("MinutesPerWeek", "2100"),
                ("HonorConstraints", "1"),
                ("NewTasksAreManual", "0"),
            ]
            .map(|(n, t)| (n.to_string(), t.to_string()))
        );
    }

    /// A tracked task carrying every progress field #81 keeps, in schema order.
    const PROGRESS: &str = "<Task><UID>1</UID><ID>1</ID><Name>Pour</Name>\
        <OutlineLevel>1</OutlineLevel><Duration>PT32H0M0S</Duration>\
        <Stop>2026-03-03T17:00:00</Stop><Resume>2026-03-04T08:00:00</Resume>\
        <StartVariance>4800</StartVariance><FinishVariance>-4800</FinishVariance>\
        <WorkVariance>960000.0</WorkVariance>\
        <PercentComplete>50</PercentComplete><PercentWorkComplete>40</PercentWorkComplete>\
        <ActualStart>2026-03-02T08:00:00</ActualStart><ActualFinish>2026-03-05T17:00:00</ActualFinish>\
        <ActualDuration>PT16H0M0S</ActualDuration><ActualCost>800.25</ActualCost>\
        <ActualWork>PT16H30M0S</ActualWork><RemainingDuration>PT16H0M0S</RemainingDuration>\
        <RemainingCost>799.75</RemainingCost><RemainingWork>PT15H30M0S</RemainingWork>\
        <PhysicalPercentComplete>30</PhysicalPercentComplete></Task>";

    /// Task elements #81 keeps.
    const PROGRESS_TASK_ELEMENTS: [&str; 16] = [
        "PercentComplete",
        "PercentWorkComplete",
        "PhysicalPercentComplete",
        "ActualStart",
        "ActualFinish",
        "ActualDuration",
        "ActualWork",
        "ActualCost",
        "Stop",
        "Resume",
        "RemainingDuration",
        "RemainingWork",
        "RemainingCost",
        "StartVariance",
        "FinishVariance",
        "WorkVariance",
    ];

    /// A tracked assignment carrying every progress field #81 keeps, and two baselines.
    const PROGRESS_ASSIGNMENT: &str = "<Assignment><UID>1</UID><TaskUID>1</TaskUID>\
        <ResourceUID>1</ResourceUID><PercentWorkComplete>50</PercentWorkComplete>\
        <ActualCost>400</ActualCost><ActualFinish>2026-03-05T17:00:00</ActualFinish>\
        <ActualStart>2026-03-02T08:00:00</ActualStart><ActualWork>PT16H0M0S</ActualWork>\
        <CostVariance>-12.5</CostVariance><FinishVariance>4800</FinishVariance>\
        <WorkVariance>480000.0</WorkVariance><RemainingCost>400.00</RemainingCost>\
        <RemainingWork>PT16H0M0S</RemainingWork><Stop>2026-03-03T17:00:00</Stop>\
        <Resume>2026-03-04T08:00:00</Resume><StartVariance>0</StartVariance>\
        <Units>1</Units><Work>PT32H0M0S</Work>\
        <Baseline><Number>0</Number><Start>2026-03-02T08:00:00</Start>\
        <Finish>2026-03-05T17:00:00</Finish><Work>PT32H0M0S</Work><Cost>800</Cost></Baseline>\
        <Baseline><Number>3</Number><Work>PT4H0M0S</Work></Baseline></Assignment>";

    /// Assignment elements #81 keeps.
    const PROGRESS_ASSIGNMENT_ELEMENTS: [&str; 14] = [
        "PercentWorkComplete",
        "ActualCost",
        "ActualFinish",
        "ActualStart",
        "ActualWork",
        "CostVariance",
        "FinishVariance",
        "WorkVariance",
        "RemainingCost",
        "RemainingWork",
        "Stop",
        "Resume",
        "StartVariance",
        "Baseline",
    ];

    fn assignment_project(assignments: &str) -> Project {
        read_mspdi(&format!(
            "<Project><StartDate>2026-03-02T08:00:00</StartDate><Tasks>\
             <Task><UID>1</UID><ID>1</ID><OutlineLevel>1</OutlineLevel>\
             <Duration>PT32H0M0S</Duration></Task></Tasks><Resources>\
             <Resource><UID>1</UID><ID>1</ID><Name>Crew</Name></Resource></Resources>\
             <Assignments>{assignments}</Assignments></Project>"
        ))
        .unwrap()
    }

    /// The `<Assignments>` section of a written file.
    fn assignment_xml(xml: &str) -> &str {
        &xml[xml.find("<Assignments>").unwrap()..xml.find("</Assignments>").unwrap()]
    }

    fn d(day: u32, hour: u32) -> Option<DateTime> {
        Some(DateTime::from_ymd_hm(2026, 3, day, hour, 0))
    }

    #[test]
    fn progress_fields_are_read() {
        assert_eq!(
            task_project(PROGRESS).tasks[0],
            Task {
                uid: 1,
                id: 1,
                name: "Pour".into(),
                outline_level: 1,
                duration_min: 1920,
                percent_complete: Some(50),
                percent_work_complete: Some(40),
                physical_percent_complete: Some(30),
                actual_start: d(2, 8),
                actual_finish: d(5, 17),
                stop: d(3, 17),
                resume: d(4, 8),
                actual_duration_min: Some(960),
                remaining_duration_min: Some(960),
                actual_work_min: Some(990),
                remaining_work_min: Some(930),
                actual_cost: Rate::parse("800.25"),
                remaining_cost: Rate::parse("799.75"),
                start_variance: Some(4800),
                finish_variance: Some(-4800),
                work_variance: Rate::parse("960000.0"),
                ..Task::default()
            }
        );
        assert_eq!(
            assignment_project(PROGRESS_ASSIGNMENT).assignments,
            [Assignment {
                uid: 1,
                task_uid: 1,
                resource_uid: 1,
                units: 1.0,
                work_min: 1920,
                percent_work_complete: Some(50),
                actual_start: d(2, 8),
                actual_finish: d(5, 17),
                stop: d(3, 17),
                resume: d(4, 8),
                actual_work_min: Some(960),
                remaining_work_min: Some(960),
                actual_cost: Rate::parse("400"),
                remaining_cost: Rate::parse("400.00"),
                start_variance: Some(0),
                finish_variance: Some(4800),
                work_variance: Rate::parse("480000.0"),
                cost_variance: Rate::parse("-12.5"),
                baselines: vec![
                    AssignmentBaseline {
                        number: 0,
                        start: d(2, 8),
                        finish: d(5, 17),
                        work_min: Some(1920),
                        cost: Rate::parse("800"),
                    },
                    AssignmentBaseline {
                        number: 3,
                        work_min: Some(240),
                        ..AssignmentBaseline::default()
                    },
                ],
            }]
        );
    }

    #[test]
    fn progress_fields_survive_mspdi_and_native_package_round_trips() {
        let mut proj = assignment_project(PROGRESS_ASSIGNMENT);
        proj.tasks = task_project(PROGRESS).tasks;
        let xml = write_mspdi(&proj);
        for element in [
            "<PercentComplete>50</PercentComplete>",
            "<PercentWorkComplete>40</PercentWorkComplete>",
            "<PhysicalPercentComplete>30</PhysicalPercentComplete>",
            "<ActualStart>2026-03-02T08:00:00</ActualStart>",
            "<ActualFinish>2026-03-05T17:00:00</ActualFinish>",
            "<Stop>2026-03-03T17:00:00</Stop>",
            "<Resume>2026-03-04T08:00:00</Resume>",
            "<ActualDuration>PT16H0M0S</ActualDuration>",
            "<ActualWork>PT16H30M0S</ActualWork>",
            "<ActualCost>800.25</ActualCost>",
            "<RemainingDuration>PT16H0M0S</RemainingDuration>",
            "<RemainingWork>PT15H30M0S</RemainingWork>",
            "<RemainingCost>799.75</RemainingCost>",
            "<StartVariance>4800</StartVariance>",
            "<FinishVariance>-4800</FinishVariance>",
            "<WorkVariance>960000.0</WorkVariance>",
        ] {
            assert!(task_xml(&xml).contains(element), "missing {element}");
        }
        for element in [
            "<PercentWorkComplete>50</PercentWorkComplete>",
            "<ActualCost>400</ActualCost>",
            "<ActualWork>PT16H0M0S</ActualWork>",
            "<CostVariance>-12.5</CostVariance>",
            "<WorkVariance>480000.0</WorkVariance>",
            "<RemainingCost>400.00</RemainingCost>",
            "<StartVariance>0</StartVariance>",
            "<Cost>800</Cost>",
        ] {
            assert!(assignment_xml(&xml).contains(element), "missing {element}");
        }
        let back = read_mspdi(&xml).unwrap();
        assert_eq!(back.tasks, proj.tasks);
        assert_eq!(back.assignments, proj.assignments);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj)).unwrap();
        assert_eq!(package.tasks, proj.tasks);
        assert_eq!(package.assignments, proj.assignments);
    }

    #[test]
    fn progress_durations_round_to_minutes() {
        // As Project 2024 wrote them in a tracked plan: whole minutes are the
        // model's unit, so the seconds do not survive a save.
        let mut proj = assignment_project(
            "<Assignment><UID>1</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID>\
             <ActualWork>PT32H9M36S</ActualWork><RemainingWork>PT15H50M24S</RemainingWork>\
             </Assignment>",
        );
        proj.tasks = task_project(
            "<Task><UID>1</UID><ActualWork>PT32H9M36S</ActualWork>\
             <ActualDuration>PT0H0M30S</ActualDuration></Task>",
        )
        .tasks;
        assert_eq!(proj.tasks[0].actual_work_min, Some(1930));
        assert_eq!(proj.tasks[0].actual_duration_min, Some(1));
        let a = &proj.assignments[0];
        assert_eq!(a.actual_work_min, Some(1930));
        assert_eq!(a.remaining_work_min, Some(950));
        let xml = write_mspdi(&proj);
        assert!(task_xml(&xml).contains("<ActualWork>PT32H10M0S</ActualWork>"));
        assert!(task_xml(&xml).contains("<ActualDuration>PT0H1M0S</ActualDuration>"));
        assert!(assignment_xml(&xml).contains("<ActualWork>PT32H10M0S</ActualWork>"));
        assert!(assignment_xml(&xml).contains("<RemainingWork>PT15H50M0S</RemainingWork>"));
    }

    #[test]
    fn absent_progress_fields_stay_absent() {
        let proj = assignment_project(
            "<Assignment><UID>1</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID>\
             <Units>1</Units><Work>PT32H0M0S</Work></Assignment>",
        );
        assert_eq!(
            proj.assignments,
            [Assignment {
                uid: 1,
                task_uid: 1,
                resource_uid: 1,
                units: 1.0,
                work_min: 1920,
                ..Assignment::default()
            }]
        );
        assert_eq!(
            proj.tasks[0],
            Task {
                uid: 1,
                id: 1,
                outline_level: 1,
                duration_min: 1920,
                ..Task::default()
            }
        );
        let xml = write_mspdi(&proj);
        for name in PROGRESS_TASK_ELEMENTS {
            assert!(!task_xml(&xml).contains(&format!("<{name}>")), "{name}");
        }
        for name in PROGRESS_ASSIGNMENT_ELEMENTS {
            assert!(
                !assignment_xml(&xml).contains(&format!("<{name}>")),
                "{name}"
            );
        }
        let back = read_mspdi(&xml).unwrap();
        assert_eq!(back.tasks, proj.tasks);
        assert_eq!(back.assignments, proj.assignments);
    }

    #[test]
    fn invalid_progress_fields_stay_absent() {
        for element in [
            "<PercentComplete>101</PercentComplete>",
            "<PercentComplete>-1</PercentComplete>",
            "<PercentComplete>abc</PercentComplete>",
            "<PercentComplete/>",
            "<PercentWorkComplete>50.5</PercentWorkComplete>",
            "<PhysicalPercentComplete>256</PhysicalPercentComplete>",
            "<ActualStart>soon</ActualStart>",
            "<ActualFinish/>",
            "<Stop>x</Stop>",
            "<Resume>NA</Resume>",
            "<ActualDuration>banana</ActualDuration>",
            "<ActualDuration/>",
            "<RemainingDuration>PT</RemainingDuration>",
            "<ActualWork>8h</ActualWork>",
            "<RemainingWork>-PT8H0M0S</RemainingWork>",
            "<ActualCost>NaN</ActualCost>",
            "<RemainingCost>x</RemainingCost>",
            "<StartVariance>1.5</StartVariance>",
            "<FinishVariance>x</FinishVariance>",
            "<WorkVariance>x</WorkVariance>",
            "<WorkVariance>inf</WorkVariance>",
        ] {
            let proj = task_project(&format!("<Task><UID>1</UID>{element}</Task>"));
            assert_eq!(
                proj.tasks[0],
                Task {
                    uid: 1,
                    ..Task::default()
                },
                "{element}"
            );
            let xml = write_mspdi(&proj);
            for name in PROGRESS_TASK_ELEMENTS {
                assert!(!task_xml(&xml).contains(&format!("<{name}>")), "{element}");
            }
        }
        for element in [
            "<PercentWorkComplete>101</PercentWorkComplete>",
            "<PercentWorkComplete>x</PercentWorkComplete>",
            "<ActualStart>soon</ActualStart>",
            "<ActualFinish>x</ActualFinish>",
            "<Stop/>",
            "<Resume>x</Resume>",
            "<ActualWork>banana</ActualWork>",
            "<RemainingWork>PT8X</RemainingWork>",
            "<ActualCost>x</ActualCost>",
            "<RemainingCost>NaN</RemainingCost>",
            "<StartVariance>x</StartVariance>",
            "<FinishVariance>2.5</FinishVariance>",
            "<WorkVariance>x</WorkVariance>",
            "<CostVariance>x</CostVariance>",
            "<Baseline><Number>0</Number><Work>x</Work><Cost>x</Cost></Baseline>",
        ] {
            let proj = assignment_project(&format!(
                "<Assignment><UID>1</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID>\
                 {element}</Assignment>"
            ));
            assert_eq!(
                proj.assignments,
                [Assignment {
                    uid: 1,
                    task_uid: 1,
                    resource_uid: 1,
                    units: 1.0,
                    ..Assignment::default()
                }],
                "{element}"
            );
            let xml = write_mspdi(&proj);
            for name in PROGRESS_ASSIGNMENT_ELEMENTS {
                assert!(
                    !assignment_xml(&xml).contains(&format!("<{name}>")),
                    "{element}"
                );
            }
        }
        // The percent range is inclusive.
        for n in [0, 100] {
            let proj = task_project(&format!(
                "<Task><UID>1</UID><PercentComplete>{n}</PercentComplete></Task>"
            ));
            assert_eq!(proj.tasks[0].percent_complete, Some(n));
        }
    }

    #[test]
    fn assignment_baselines_round_trip() {
        let baselines = |xml: &str| {
            assignment_project(&format!(
                "<Assignment><UID>1</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID>\
                 {xml}</Assignment>"
            ))
            .assignments
            .remove(0)
            .baselines
        };
        let work = |number, min| AssignmentBaseline {
            number,
            work_min: Some(min),
            ..AssignmentBaseline::default()
        };
        // Slots are sorted; a missing Number is slot 0.
        assert_eq!(
            baselines(
                "<Baseline><Number>10</Number><Work>PT1H0M0S</Work></Baseline>\
                 <Baseline><Work>PT2H0M0S</Work></Baseline>"
            ),
            [work(0, 120), work(10, 60)]
        );
        // A duplicate slot replaces the whole record, cost included.
        assert_eq!(
            baselines(
                "<Baseline><Number>1</Number><Work>PT1H0M0S</Work><Cost>5</Cost></Baseline>\
                 <Baseline><Number>1</Number><Work>PT2H0M0S</Work></Baseline>"
            ),
            [work(1, 120)]
        );
        // An empty record, and an invalid or out-of-range Number, are dropped.
        assert_eq!(
            baselines(
                "<Baseline><Number>2</Number></Baseline>\
                 <Baseline><Number>11</Number><Work>PT1H0M0S</Work></Baseline>\
                 <Baseline><Number>x</Number><Work>PT1H0M0S</Work></Baseline>"
            ),
            []
        );
        // A recorded zero is kept, not confused with an absent value.
        assert_eq!(
            baselines("<Baseline><Number>4</Number><Work>PT0H0M0S</Work></Baseline>"),
            [work(4, 0)]
        );
        let proj = assignment_project(PROGRESS_ASSIGNMENT);
        let xml = write_mspdi(&proj);
        assert_eq!(read_mspdi(&xml).unwrap().assignments, proj.assignments);
    }

    #[test]
    fn assignment_children_follow_schema_sequence() {
        // The MSPDI Assignment sequence, as Project 2024 writes it (merged over
        // the assignments of a private Project 2024 corpus) and as Microsoft's
        // XML Schema lists it. That corpus has no assignment ActualCost or
        // Baseline; their places are the schema's.
        let proj = assignment_project(PROGRESS_ASSIGNMENT);
        let mut xml = String::new();
        write_assignment(&mut xml, &proj.assignments[0]);
        assert_eq!(
            element_names(&xml),
            [
                "Assignment",
                "UID",
                "TaskUID",
                "ResourceUID",
                "PercentWorkComplete",
                "ActualCost",
                "ActualFinish",
                "ActualStart",
                "ActualWork",
                "CostVariance",
                "FinishVariance",
                "WorkVariance",
                "RemainingCost",
                "RemainingWork",
                "Stop",
                "Resume",
                "StartVariance",
                "Units",
                "Work",
                "Baseline",
                "Number",
                "Start",
                "Finish",
                "Work",
                "Cost",
                "Baseline",
                "Number",
                "Work",
            ]
        );
    }
}
