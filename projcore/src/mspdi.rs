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
//! - `LinkLag` is **tenths of a minute** of working or elapsed time, except
//!   for a percentage `LagFormat` (19, estimated 51), where it is the
//!   percentage itself (#104). An unsupported `LagFormat` fails the read.
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
                    // Modeled options: a value that parses fills the field; one
                    // that does not is kept verbatim. Of repeated leaves, the
                    // later wins, as `set_option` has it; a block or an
                    // attributed or prefixed repeat is skipped (see
                    // `read_option`).
                    "NewTasksEffortDriven" => {
                        read_option(&mut p, &mut proj, name, parse_bool, |proj| {
                            &mut proj.new_tasks_effort_driven
                        })
                    }
                    "NewTasksEstimated" => {
                        read_option(&mut p, &mut proj, name, parse_bool, |proj| {
                            &mut proj.new_tasks_estimated
                        })
                    }
                    "DefaultTaskType" => read_option(
                        &mut p,
                        &mut proj,
                        name,
                        |text| text.parse().ok().and_then(TaskType::from_code),
                        |proj| &mut proj.default_task_type,
                    ),
                    "Autolink" => read_option(&mut p, &mut proj, name, parse_bool, |proj| {
                        &mut proj.autolink
                    }),
                    "CriticalSlackLimit" => read_option(
                        &mut p,
                        &mut proj,
                        name,
                        |text| text.parse().ok(),
                        |proj| &mut proj.critical_slack_limit_days,
                    ),
                    "MultipleCriticalPaths" => {
                        read_option(&mut p, &mut proj, name, parse_bool, |proj| {
                            &mut proj.multiple_critical_paths
                        })
                    }
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
                    // docxy's durations are the schedule's input, so its saves
                    // always declare them authoritative (see `header_text`);
                    // a source's value is never kept.
                    "ProjectExternallyEdited" => p.skip_element(),
                    "Tasks" => parse_tasks(&mut p, &mut proj.tasks, &mut task_uids)?,
                    "Resources" => parse_resources(&mut p, &mut proj.resources),
                    "Assignments" => parse_assignments(&mut p, &mut proj.assignments),
                    "Calendars" => parse_calendars(&mut p, &mut proj.calendars),
                    "OutlineCodes" if kept_as_element(&p) => {
                        parse_definitions(&mut p, "OutlineCode", &mut proj.outline_code_definitions)
                    }
                    "WBSMasks" if kept_as_element(&p) => {
                        let block = parse_element(&mut p, 0);
                        if !block.children.is_empty() {
                            proj.wbs_masks = Some(block);
                        }
                    }
                    "ExtendedAttributes" => parse_definitions(
                        &mut p,
                        "ExtendedAttribute",
                        &mut proj.extended_attribute_definitions,
                    ),
                    // A prefixed or attributed element (leaf or block) is
                    // consumed here, including OutlineCodes/WBSMasks wrappers:
                    // an option stores only name and text, so writing it back
                    // would lose namespace bindings or attributes like xsi:nil.
                    // The final arm keeps plain unknown leaves as options.
                    // Plain unknown blocks such as Views reach that arm, but
                    // leaf_text_of returns None and drops them whole.
                    _ if !kept_as_element(&p) => p.skip_element(),
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

/// A task, resource or assignment `Notes` value: [`text_of`] with its line
/// breaks normalised to `\n`. The reader keeps raw CR bytes and decodes
/// `&#13;` to `\r`, so without this a note would depend on how the file was
/// saved or checked out (#531).
fn note_text(p: &mut XmlParser) -> String {
    crate::normalize_newlines(&text_of(p))
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

/// Read a modeled project option into its field. Text that parses sets the
/// field and drops any stored text of an earlier repeat; text that does not
/// clears the field and is stored verbatim. A block, a prefixed element or one
/// with attributes is skipped, as an unmodeled option's is, and leaves an
/// earlier repeat's value in place.
fn read_option<T>(
    p: &mut XmlParser,
    proj: &mut Project,
    name: String,
    parse: impl Fn(&str) -> Option<T>,
    field: impl Fn(&mut Project) -> &mut Option<T>,
) {
    if !kept_as_element(p) {
        p.skip_element();
        return;
    }
    let Some(text) = leaf_text_of(p) else {
        return;
    };
    match parse(text.trim()) {
        Some(value) => {
            *field(proj) = Some(value);
            proj.options.retain(|(n, _)| *n != name);
        }
        None => {
            *field(proj) = None;
            set_option(&mut proj.options, name, text);
        }
    }
}

/// An `xsd:boolean` spelling, as [`opt_bool_of`] reads it.
fn parse_bool(text: &str) -> Option<bool> {
    match text {
        "1" | "true" | "True" => Some(true),
        "0" | "false" | "False" => Some(false),
        _ => None,
    }
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

fn parse_tasks(
    p: &mut XmlParser,
    out: &mut Vec<Task>,
    uids: &mut Vec<Option<i32>>,
) -> Result<(), String> {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "Task" {
                    let (task, uid) = parse_task(p)?;
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
    Ok(())
}

fn parse_task(p: &mut XmlParser) -> Result<(Task, Option<i32>), String> {
    let mut t = Task::default();
    let mut uid = None;
    // A link whose lag format docxy cannot schedule fails the read, never
    // turning into minutes; reported once the task's UID is known.
    let mut bad_lag_format = None;
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
                    // Days, the default, is no format.
                    "DurationFormat" => t.duration_format = opt_u8_of(p).filter(|&f| f != 7),
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
                    "PredecessorLink" => match parse_predecessor(p) {
                        Ok(Some(pred)) => t.predecessors.push(pred),
                        Ok(None) => {}
                        Err(code) => bad_lag_format = bad_lag_format.or(Some(code)),
                    },
                    "Baseline" => parse_baseline(p, &mut t),
                    "ExtendedAttribute" => {
                        t.extended_attributes.extend(parse_extended_attribute(p));
                    }
                    "OutlineCode" => t.outline_codes.extend(parse_outline_code(p)),
                    "IsNull" => t.is_null = bool_of(p),
                    "GUID" => t.guid = guid_of(p),
                    "CreateDate" => t.create_date = DateTime::parse_mspdi(&text_of(p)),
                    "Contact" => t.contact = Some(text_of(p)),
                    "WBS" => t.wbs = Some(text_of(p)),
                    "WBSLevel" => t.wbs_level = Some(text_of(p)),
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
                    "PreLeveledStart" => t.pre_leveled_start = DateTime::parse_mspdi(&text_of(p)),
                    "PreLeveledFinish" => t.pre_leveled_finish = DateTime::parse_mspdi(&text_of(p)),
                    "Hyperlink" => t.hyperlink = Some(text_of(p)),
                    "HyperlinkAddress" => t.hyperlink_address = Some(text_of(p)),
                    "HyperlinkSubAddress" => t.hyperlink_sub_address = Some(text_of(p)),
                    "IgnoreResourceCalendar" => t.ignore_resource_calendar = opt_bool_of(p),
                    "Notes" => t.notes = Some(note_text(p)),
                    "EarnedValueMethod" => t.earned_value_method = opt_i32_of(p),
                    "Recurring" => t.recurring = opt_bool_of(p),
                    "HideBar" => t.hide_bar = opt_bool_of(p),
                    "Rollup" => t.rollup = opt_bool_of(p),
                    "ExternalTask" => t.external_task = opt_bool_of(p),
                    "ExternalTaskProject" => t.external_task_project = Some(text_of(p)),
                    "IsSubproject" => t.is_subproject = opt_bool_of(p),
                    "IsSubprojectReadOnly" => t.is_subproject_read_only = opt_bool_of(p),
                    "SubprojectName" => t.subproject_name = Some(text_of(p)),
                    "DisplayAsSummary" => t.display_as_summary = opt_bool_of(p),
                    "IsPublished" => t.is_published = opt_bool_of(p),
                    "StatusManager" => t.status_manager = Some(text_of(p)),
                    "CommitmentStart" => t.commitment_start = DateTime::parse_mspdi(&text_of(p)),
                    "CommitmentFinish" => t.commitment_finish = DateTime::parse_mspdi(&text_of(p)),
                    "CommitmentType" => {
                        t.commitment_type = opt_int_of(p)
                            .filter(|n| (0..=2).contains(n))
                            .map(|n| n as i32);
                    }
                    "Work" => t.work_min = try_iso8601_to_minutes(&text_of(p)),
                    "Cost" => t.cost = rate_of(p),
                    "FixedCost" => t.fixed_cost = rate_of(p),
                    "FixedCostAccrual" => {
                        t.fixed_cost_accrual = opt_int_of(p).and_then(AccrueAt::from_code);
                    }
                    "OverAllocated" => t.over_allocated = opt_bool_of(p),
                    "PercentComplete" => t.percent_complete = percent_of(p),
                    "PercentWorkComplete" => t.percent_work_complete = percent_of(p),
                    "PhysicalPercentComplete" => t.physical_percent_complete = percent_of(p),
                    "ActualStart" => t.actual_start = DateTime::parse_mspdi(&text_of(p)),
                    "ActualFinish" => t.actual_finish = DateTime::parse_mspdi(&text_of(p)),
                    "Stop" => t.stop = DateTime::parse_mspdi(&text_of(p)),
                    "Resume" => t.resume = DateTime::parse_mspdi(&text_of(p)),
                    "ResumeValid" => t.resume_valid = opt_bool_of(p),
                    "ActualDuration" => t.actual_duration_min = try_iso8601_to_minutes(&text_of(p)),
                    "RemainingDuration" => {
                        t.remaining_duration_min = try_iso8601_to_minutes(&text_of(p));
                    }
                    "ActualWork" => t.actual_work_min = try_iso8601_to_minutes(&text_of(p)),
                    "RemainingWork" => t.remaining_work_min = try_iso8601_to_minutes(&text_of(p)),
                    "ActualCost" => t.actual_cost = rate_of(p),
                    "RemainingCost" => t.remaining_cost = rate_of(p),
                    "OvertimeCost" => t.overtime_cost = rate_of(p),
                    "OvertimeWork" => t.overtime_work_min = try_iso8601_to_minutes(&text_of(p)),
                    "ActualOvertimeCost" => t.actual_overtime_cost = rate_of(p),
                    "ActualOvertimeWork" => {
                        t.actual_overtime_work_min = try_iso8601_to_minutes(&text_of(p));
                    }
                    "RegularWork" => t.regular_work_min = try_iso8601_to_minutes(&text_of(p)),
                    "RemainingOvertimeCost" => t.remaining_overtime_cost = rate_of(p),
                    "RemainingOvertimeWork" => {
                        t.remaining_overtime_work_min = try_iso8601_to_minutes(&text_of(p));
                    }
                    "ACWP" => t.acwp = rate_of(p),
                    "CV" => t.cv = rate_of(p),
                    "BCWS" => t.bcws = rate_of(p),
                    "BCWP" => t.bcwp = rate_of(p),
                    "ActualWorkProtected" => {
                        t.actual_work_protected_min = try_iso8601_to_minutes(&text_of(p));
                    }
                    "ActualOvertimeWorkProtected" => {
                        t.actual_overtime_work_protected_min = try_iso8601_to_minutes(&text_of(p));
                    }
                    "TimephasedData" => t.timephased_data.extend(parse_timephased_data(p)),
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
    if let Some(code) = bad_lag_format {
        let uid = uid.map_or_else(|| "?".to_string(), |u| u.to_string());
        return Err(format!(
            "unsupported LagFormat {code} on a predecessor link of task UID {uid}"
        ));
    }
    Ok((t, uid))
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
                    "TimephasedData" => baseline.timephased_data.extend(parse_timephased_data(p)),
                    "Number" => {
                        number = text_of(p).trim().parse::<u8>().ok().filter(|n| *n <= 10);
                    }
                    "Start" => baseline.start = DateTime::parse_mspdi(&text_of(p)),
                    "Finish" => baseline.finish = DateTime::parse_mspdi(&text_of(p)),
                    "Duration" => baseline.duration_min = try_iso8601_to_minutes(&text_of(p)),
                    "DurationFormat" => baseline.duration_format = opt_u8_of(p),
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
        if baseline != Baseline::default() {
            baseline.number = number;
            t.set_baseline_slot(baseline);
        }
    }
}

/// One `<PredecessorLink>`; `Err` carries an unsupported `LagFormat` code.
fn parse_predecessor(p: &mut XmlParser) -> Result<Option<Predecessor>, i64> {
    let mut uid: Option<i32> = None;
    let mut link = LinkType::FinishStart;
    let mut link_lag: i64 = 0;
    let mut cross_project = None;
    let mut cross_project_name = None;
    // An absent LagFormat is days, as docxy has always read it.
    let mut format_code = LagFormat::DAYS.code();
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "PredecessorUID" => uid = Some(int_of(p) as i32),
                    "Type" => {
                        link = LinkType::from_code(int_of(p)).unwrap_or(LinkType::FinishStart)
                    }
                    "LinkLag" => link_lag = int_of(p),
                    "CrossProject" => cross_project = opt_bool_of(p),
                    "CrossProjectName" => cross_project_name = Some(text_of(p)),
                    "LagFormat" => format_code = int_of(p),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    let lag_format = LagFormat::from_code(format_code).ok_or(format_code)?;
    let lag = lag_from_link_lag(link_lag, lag_format);
    Ok(uid.map(|uid| Predecessor {
        uid,
        link,
        lag,
        lag_format,
        cross_project,
        cross_project_name,
    }))
}

/// A `LinkLag` as the model holds it: a percentage as is, time from tenths of
/// a minute to whole minutes. `.mpp` link records use the same encoding.
pub fn lag_from_link_lag(link_lag: i64, format: LagFormat) -> i64 {
    match format.kind() {
        LagKind::Percent => link_lag,
        LagKind::Working | LagKind::Elapsed => (link_lag as f64 / 10.0).round() as i64,
    }
}

/// [`lag_from_link_lag`] inverted, for writing.
fn link_lag_of(p: &Predecessor) -> i64 {
    match p.lag_format.kind() {
        LagKind::Percent => p.lag,
        LagKind::Working | LagKind::Elapsed => p.lag.saturating_mul(10),
    }
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
                    "StandardRateFormat" => r.standard_rate_format = opt_u8_of(p),
                    "OvertimeRateFormat" => r.overtime_rate_format = opt_u8_of(p),
                    "BookingType" => r.booking_type = opt_u8_of(p),
                    "WorkGroup" => r.work_group = opt_u8_of(p),
                    "IsGeneric" => r.is_generic = opt_bool_of(p),
                    "IsBudget" => r.is_budget = opt_bool_of(p),
                    "IsInactive" => r.is_inactive = opt_bool_of(p),
                    "CanLevel" => r.can_level = opt_bool_of(p),
                    "OverAllocated" => r.over_allocated = opt_bool_of(p),
                    "PeakUnits" => r.peak_units = rate_of(p),
                    "Work" => r.work_min = try_iso8601_to_minutes(&text_of(p)),
                    "RegularWork" => r.regular_work_min = try_iso8601_to_minutes(&text_of(p)),
                    "RemainingWork" => r.remaining_work_min = try_iso8601_to_minutes(&text_of(p)),
                    "OvertimeWork" => r.overtime_work_min = try_iso8601_to_minutes(&text_of(p)),
                    "Cost" => r.cost = rate_of(p),
                    "EmailAddress" => r.email_address = Some(text_of(p)),
                    "Notes" => r.notes = Some(note_text(p)),
                    "AvailableFrom" => r.available_from = DateTime::parse_mspdi(&text_of(p)),
                    "AvailableTo" => r.available_to = DateTime::parse_mspdi(&text_of(p)),
                    "ExtendedAttribute" => {
                        r.extended_attributes.extend(parse_extended_attribute(p));
                    }
                    // Its Work/Cost are the recorded plan's, not the resource's.
                    "Baseline" => parse_resource_baseline(p, &mut r),
                    "AvailabilityPeriods" => parse_availability_periods(p, &mut r),
                    "Rates" => parse_rates(p, &mut r),
                    "GUID" => r.guid = guid_of(p),
                    "IsNull" => r.is_null = opt_bool_of(p),
                    "Phonetics" => r.phonetics = Some(text_of(p)),
                    "NTAccount" => r.nt_account = Some(text_of(p)),
                    "Hyperlink" => r.hyperlink = Some(text_of(p)),
                    "HyperlinkAddress" => r.hyperlink_address = Some(text_of(p)),
                    "HyperlinkSubAddress" => r.hyperlink_sub_address = Some(text_of(p)),
                    "Start" => r.start = DateTime::parse_mspdi(&text_of(p)),
                    "Finish" => r.finish = DateTime::parse_mspdi(&text_of(p)),
                    "ActualWork" => r.actual_work_min = try_iso8601_to_minutes(&text_of(p)),
                    "ActualOvertimeWork" => {
                        r.actual_overtime_work_min = try_iso8601_to_minutes(&text_of(p));
                    }
                    "RemainingOvertimeWork" => {
                        r.remaining_overtime_work_min = try_iso8601_to_minutes(&text_of(p));
                    }
                    "PercentWorkComplete" => r.percent_work_complete = percent_of(p),
                    "OvertimeCost" => r.overtime_cost = rate_of(p),
                    "ActualCost" => r.actual_cost = rate_of(p),
                    "ActualOvertimeCost" => r.actual_overtime_cost = rate_of(p),
                    "RemainingCost" => r.remaining_cost = rate_of(p),
                    "RemainingOvertimeCost" => r.remaining_overtime_cost = rate_of(p),
                    "WorkVariance" => r.work_variance = rate_of(p),
                    "CostVariance" => r.cost_variance = rate_of(p),
                    "SV" => r.sv = rate_of(p),
                    "CV" => r.cv = rate_of(p),
                    "ACWP" => r.acwp = rate_of(p),
                    "BCWS" => r.bcws = rate_of(p),
                    "BCWP" => r.bcwp = rate_of(p),
                    "IsEnterprise" => r.is_enterprise = opt_bool_of(p),
                    "ActualWorkProtected" => {
                        r.actual_work_protected_min = try_iso8601_to_minutes(&text_of(p));
                    }
                    "ActualOvertimeWorkProtected" => {
                        r.actual_overtime_work_protected_min = try_iso8601_to_minutes(&text_of(p));
                    }
                    "ActiveDirectoryGUID" => r.active_directory_guid = Some(text_of(p)),
                    "CreationDate" => r.creation_date = DateTime::parse_mspdi(&text_of(p)),
                    "CostCenter" => r.cost_center = Some(text_of(p)),
                    "AssnOwner" => r.assn_owner = Some(text_of(p)),
                    "AssnOwnerGuid" => r.assn_owner_guid = guid_of(p),
                    "OutlineCode" => r.outline_codes.extend(parse_outline_code(p)),
                    "TimephasedData" => r.timephased_data.extend(parse_timephased_data(p)),
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
                    "WorkContour" => a.work_contour = opt_u8_of(p),
                    "FixedMaterial" => a.fixed_material = opt_bool_of(p),
                    "HasFixedRateUnits" => a.has_fixed_rate_units = opt_bool_of(p),
                    "Start" => a.start = DateTime::parse_mspdi(&text_of(p)),
                    "Finish" => a.finish = DateTime::parse_mspdi(&text_of(p)),
                    "RegularWork" => a.regular_work_min = try_iso8601_to_minutes(&text_of(p)),
                    "OvertimeWork" => a.overtime_work_min = try_iso8601_to_minutes(&text_of(p)),
                    "Cost" => a.cost = rate_of(p),
                    "CostRateTable" => a.cost_rate_table = opt_u8_of(p),
                    "Delay" => a.delay = opt_int_of(p),
                    "LevelingDelay" => a.leveling_delay = opt_int_of(p),
                    "LevelingDelayFormat" => a.leveling_delay_format = opt_u8_of(p),
                    "Notes" => a.notes = Some(note_text(p)),
                    "ExtendedAttribute" => {
                        a.extended_attributes.extend(parse_extended_attribute(p));
                    }
                    // Its Start/Finish/Work/Cost are the recorded plan's, not the assignment's.
                    "Baseline" => parse_assignment_baseline(p, &mut a),
                    "TimephasedData" => a.timephased_data.extend(parse_timephased_data(p)),
                    "GUID" => a.guid = guid_of(p),
                    "ActualOvertimeCost" => a.actual_overtime_cost = rate_of(p),
                    "ActualOvertimeWork" => {
                        a.actual_overtime_work_min = try_iso8601_to_minutes(&text_of(p));
                    }
                    "ACWP" => a.acwp = rate_of(p),
                    "Confirmed" => a.confirmed = opt_bool_of(p),
                    "RateScale" => a.rate_scale = opt_u8_of(p),
                    "CV" => a.cv = rate_of(p),
                    "Hyperlink" => a.hyperlink = Some(text_of(p)),
                    "HyperlinkAddress" => a.hyperlink_address = Some(text_of(p)),
                    "HyperlinkSubAddress" => a.hyperlink_sub_address = Some(text_of(p)),
                    "LinkedFields" => a.linked_fields = opt_bool_of(p),
                    "Milestone" => a.milestone = opt_bool_of(p),
                    "Overallocated" => a.overallocated = opt_bool_of(p),
                    "OvertimeCost" => a.overtime_cost = rate_of(p),
                    "PeakUnits" => a.peak_units = rate_of(p),
                    "RemainingOvertimeCost" => a.remaining_overtime_cost = rate_of(p),
                    "RemainingOvertimeWork" => {
                        a.remaining_overtime_work_min = try_iso8601_to_minutes(&text_of(p));
                    }
                    "ResponsePending" => a.response_pending = opt_bool_of(p),
                    "Summary" => a.summary = opt_bool_of(p),
                    "SV" => a.sv = rate_of(p),
                    "UpdateNeeded" => a.update_needed = opt_bool_of(p),
                    "VAC" => a.vac = rate_of(p),
                    "BCWS" => a.bcws = rate_of(p),
                    "BCWP" => a.bcwp = rate_of(p),
                    "BookingType" => a.booking_type = opt_u8_of(p),
                    "ActualWorkProtected" => {
                        a.actual_work_protected_min = try_iso8601_to_minutes(&text_of(p));
                    }
                    "ActualOvertimeWorkProtected" => {
                        a.actual_overtime_work_protected_min = try_iso8601_to_minutes(&text_of(p));
                    }
                    "CreationDate" => a.creation_date = DateTime::parse_mspdi(&text_of(p)),
                    "AssnOwner" => a.assn_owner = Some(text_of(p)),
                    "AssnOwnerGuid" => a.assn_owner_guid = guid_of(p),
                    "BudgetCost" => a.budget_cost = rate_of(p),
                    "BudgetWork" => a.budget_work_min = try_iso8601_to_minutes(&text_of(p)),
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
                    "TimephasedData" => baseline.timephased_data.extend(parse_timephased_data(p)),
                    "Number" => {
                        number = text_of(p).trim().parse::<u8>().ok().filter(|n| *n <= 10);
                    }
                    "Start" => baseline.start = DateTime::parse_mspdi(&text_of(p)),
                    "Finish" => baseline.finish = DateTime::parse_mspdi(&text_of(p)),
                    "Work" => baseline.work_min = try_iso8601_to_minutes(&text_of(p)),
                    "Cost" => baseline.cost = rate_of(p),
                    "BCWS" => baseline.bcws = rate_of(p),
                    "BCWP" => baseline.bcwp = rate_of(p),
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

/// Parse a resource's recorded plan; the same slot rules as [`parse_baseline`].
fn parse_resource_baseline(p: &mut XmlParser, r: &mut Resource) {
    let mut baseline = ResourceBaseline::default();
    let mut number = Some(0);
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "TimephasedData" => baseline.timephased_data.extend(parse_timephased_data(p)),
                    "Number" => {
                        number = text_of(p).trim().parse::<u8>().ok().filter(|n| *n <= 10);
                    }
                    "Work" => baseline.work_min = try_iso8601_to_minutes(&text_of(p)),
                    "Cost" => baseline.cost = rate_of(p),
                    "BCWS" => baseline.bcws = rate_of(p),
                    "BCWP" => baseline.bcwp = rate_of(p),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    if let Some(number) = number {
        if baseline != ResourceBaseline::default() {
            baseline.number = number;
            r.set_baseline_slot(baseline);
        }
    }
}

/// Parse `AvailabilityPeriods`; a period with no valid child is dropped.
fn parse_availability_periods(p: &mut XmlParser, r: &mut Resource) {
    loop {
        match p.next() {
            Event::Start if p.name() == "AvailabilityPeriod" => {
                let mut period = AvailabilityPeriod::default();
                loop {
                    match p.next() {
                        Event::Start => {
                            let name = p.name().to_string();
                            match name.as_str() {
                                "AvailableFrom" => {
                                    period.available_from = DateTime::parse_mspdi(&text_of(p));
                                }
                                "AvailableTo" => {
                                    period.available_to = DateTime::parse_mspdi(&text_of(p));
                                }
                                "AvailableUnits" => period.available_units = rate_of(p),
                                _ => p.skip_element(),
                            }
                        }
                        Event::End | Event::Eof => break,
                        _ => {}
                    }
                }
                if period != AvailabilityPeriod::default() {
                    r.availability_periods.push(period);
                }
            }
            Event::Start => p.skip_element(),
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
}

/// Parse `Rates`; an entry with no valid child is dropped.
fn parse_rates(p: &mut XmlParser, r: &mut Resource) {
    loop {
        match p.next() {
            Event::Start if p.name() == "Rate" => {
                let mut rate = RateEntry::default();
                loop {
                    match p.next() {
                        Event::Start => {
                            let name = p.name().to_string();
                            match name.as_str() {
                                "RatesFrom" => rate.rates_from = DateTime::parse_mspdi(&text_of(p)),
                                "RatesTo" => rate.rates_to = DateTime::parse_mspdi(&text_of(p)),
                                "RateTable" => rate.rate_table = opt_u8_of(p),
                                "StandardRate" => rate.standard_rate = rate_of(p),
                                "StandardRateFormat" => rate.standard_rate_format = opt_u8_of(p),
                                "OvertimeRate" => rate.overtime_rate = rate_of(p),
                                "OvertimeRateFormat" => rate.overtime_rate_format = opt_u8_of(p),
                                "CostPerUse" => rate.cost_per_use = rate_of(p),
                                _ => p.skip_element(),
                            }
                        }
                        Event::End | Event::Eof => break,
                        _ => {}
                    }
                }
                if rate != RateEntry::default() {
                    r.rates.push(rate);
                }
            }
            Event::Start => p.skip_element(),
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
}

/// Parse one custom field value; `None` without a `FieldID`, which names it.
fn parse_extended_attribute(p: &mut XmlParser) -> Option<ExtendedAttributeValue> {
    let mut field_id = None;
    let mut attribute = ExtendedAttributeValue::default();
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "FieldID" => field_id = leaf_text_of(p),
                    "Value" => attribute.value = leaf_text_of(p),
                    "ValueGUID" => attribute.value_guid = leaf_text_of(p),
                    "DurationFormat" => attribute.duration_format = opt_u8_of(p),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    attribute.field_id = field_id?;
    Some(attribute)
}

/// Parse one task or resource outline code value; `None` without a `FieldID`, which
/// names it.
fn parse_outline_code(p: &mut XmlParser) -> Option<OutlineCodeValue> {
    let mut field_id = None;
    let mut code = OutlineCodeValue::default();
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "FieldID" => field_id = leaf_text_of(p),
                    "ValueID" => code.value_id = leaf_text_of(p),
                    "ValueGUID" => code.value_guid = leaf_text_of(p),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    code.field_id = field_id?;
    Some(code)
}

/// Keep matching definitions in an `<OutlineCodes>` or
/// `<ExtendedAttributes>` block whole; skip other children.
fn parse_definitions(p: &mut XmlParser, child_name: &str, out: &mut Vec<XmlElement>) {
    loop {
        match p.next() {
            Event::Start if p.name() == child_name && kept_as_element(p) => {
                out.push(parse_element(p, 1));
            }
            Event::Start => p.skip_element(),
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
}

/// Whether an element can be kept as an [`XmlElement`]: one that is neither
/// prefixed nor carries attributes, since the tree keeps only names and text
/// (the rule the header options follow).
fn kept_as_element(p: &XmlParser) -> bool {
    !p.name().contains(':') && p.attrs().is_empty()
}

/// How deep a kept definition goes, counting its root as 1.
/// Project's deepest is 4 (`ExtendedAttribute/ValueList/Value/ID`); the bound
/// keeps a crafted file from overflowing the stack of the recursive reader,
/// writer, and the tree's derived `Clone`/`PartialEq`/`Drop`.
const MAX_DEFINITION_DEPTH: usize = 32;

/// Read the element whose `Start` was just consumed, at `depth`, and its
/// children, as an [`XmlElement`]. A leaf keeps its decoded text verbatim; the
/// text between an element's children is dropped (even when every child
/// was), and so is a child [`kept_as_element`] refuses, or one past
/// [`MAX_DEFINITION_DEPTH`], with its whole subtree.
fn parse_element(p: &mut XmlParser, depth: usize) -> XmlElement {
    let mut element = XmlElement {
        name: p.name().to_string(),
        ..XmlElement::default()
    };
    let mut leaf = true;
    loop {
        match p.next() {
            Event::Text => XmlParser::append_decoded(p.text(), &mut element.text),
            Event::Start => {
                leaf = false;
                if depth < MAX_DEFINITION_DEPTH && kept_as_element(p) {
                    element.children.push(parse_element(p, depth + 1));
                } else {
                    p.skip_element();
                }
            }
            Event::End | Event::Eof => break,
        }
    }
    if !leaf {
        element.text.clear();
    }
    element
}

/// Parse one timephased record; `None` without a valid `Type`, which says
/// what its value measures.
fn parse_timephased_data(p: &mut XmlParser) -> Option<TimephasedValue> {
    let mut kind = None;
    let mut record = TimephasedValue::default();
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "Type" => kind = opt_u8_of(p),
                    "UID" => record.uid = opt_i32_of(p),
                    "Start" => record.start = DateTime::parse_mspdi(&text_of(p)),
                    "Finish" => record.finish = DateTime::parse_mspdi(&text_of(p)),
                    "Unit" => record.unit = opt_u8_of(p),
                    "Value" => record.value = leaf_text_of(p),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    record.kind = kind?;
    Some(record)
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
        exceptions: Vec::new(),
        work_weeks: Vec::new(),
    };
    let mut is_base = false;
    let mut base_uid: Option<i32> = None;
    let mut legacy = Vec::new();
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
                    "WeekDays" => parse_weekdays(p, &mut cal.week, &mut legacy),
                    "Exceptions" => parse_exceptions(p, &mut cal.exceptions),
                    "WorkWeeks" => parse_work_weeks(p, &mut cal.work_weeks),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    // Project writes each date-range exception twice: as a legacy `DayType 0`
    // weekday and in `Exceptions`. `Exceptions` is the full form and wins; a
    // file with only the legacy form (an older writer's) reads as the same
    // date-range exceptions.
    if cal.exceptions.is_empty() {
        cal.exceptions = legacy;
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

fn parse_weekdays(
    p: &mut XmlParser,
    week: &mut [Option<DayWorking>; 7],
    legacy: &mut Vec<CalendarException>,
) {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "WeekDay" {
                    parse_weekday(p, week, legacy);
                } else {
                    p.skip_element();
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
}

fn parse_weekday(
    p: &mut XmlParser,
    week: &mut [Option<DayWorking>; 7],
    legacy: &mut Vec<CalendarException>,
) {
    // MSPDI DayType: 1=Sunday .. 7=Saturday. Our week[] is Sunday=0..Saturday=6.
    // DayType 0 is Project's legacy form of a date-range exception.
    let mut day_type: Option<i64> = None;
    let mut working = false;
    let mut times: Vec<WorkingTime> = Vec::new();
    let mut period = (None, None);
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "DayType" => day_type = opt_int_of(p),
                    "DayWorking" => working = bool_of(p),
                    "WorkingTimes" => parse_working_times(p, &mut times),
                    "TimePeriod" => period = parse_time_period(p),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    // A non-working day yields empty times even if some were present.
    let day = DayWorking {
        times: if working { times } else { Vec::new() },
    };
    match day_type {
        Some(d @ 1..=7) => week[d as usize - 1] = Some(day),
        Some(0) => {
            if let (Some(from), Some(to)) = period {
                legacy.push(CalendarException::date_range(from, to, day));
            }
        }
        _ => {}
    }
}

/// `TimePeriod`: its `FromDate` and `ToDate`.
fn parse_time_period(p: &mut XmlParser) -> (Option<DateTime>, Option<DateTime>) {
    let (mut from, mut to) = (None, None);
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "FromDate" => from = DateTime::parse_mspdi(&text_of(p)),
                    "ToDate" => to = DateTime::parse_mspdi(&text_of(p)),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    (from, to)
}

fn parse_work_weeks(p: &mut XmlParser, out: &mut Vec<WorkWeek>) {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "WorkWeek" {
                    out.push(parse_work_week(p));
                } else {
                    p.skip_element();
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
}

fn parse_work_week(p: &mut XmlParser) -> WorkWeek {
    let mut work_week = WorkWeek::default();
    let mut legacy = Vec::new();
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "TimePeriod" => {
                        (work_week.from, work_week.to) = parse_time_period(p);
                    }
                    "Name" => work_week.name = Some(text_of(p)),
                    "WeekDays" => parse_weekdays(p, &mut work_week.week, &mut legacy),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    work_week
}

fn parse_exceptions(p: &mut XmlParser, out: &mut Vec<CalendarException>) {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "Exception" {
                    out.push(parse_exception(p));
                } else {
                    p.skip_element();
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
}

fn parse_exception(p: &mut XmlParser) -> CalendarException {
    let mut exception = CalendarException::default();
    let mut working = false;
    let mut times: Vec<WorkingTime> = Vec::new();
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "EnteredByOccurrences" => exception.entered_by_occurrences = opt_bool_of(p),
                    "TimePeriod" => (exception.from, exception.to) = parse_time_period(p),
                    "Occurrences" => exception.occurrences = opt_i32_of(p),
                    "Name" => exception.name = Some(text_of(p)),
                    "Type" => exception.kind = opt_i32_of(p),
                    "Period" => exception.period = opt_i32_of(p),
                    "DaysOfWeek" => exception.days_of_week = opt_i32_of(p),
                    "MonthItem" => exception.month_item = opt_i32_of(p),
                    "MonthPosition" => exception.month_position = opt_i32_of(p),
                    "Month" => exception.month = opt_i32_of(p),
                    "MonthDay" => exception.month_day = opt_i32_of(p),
                    "DayWorking" => working = bool_of(p),
                    "WorkingTimes" => parse_working_times(p, &mut times),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    if working {
        exception.day.times = times;
    }
    exception
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

/// A GUID's text; an empty one is no GUID.
fn guid_of(p: &mut XmlParser) -> Option<String> {
    Some(text_of(p)).filter(|g| !g.trim().is_empty())
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
    parse_bool(text_of(p).trim())
}

/// An optional integer: unparseable text stays absent instead of reading as 0.
fn opt_int_of(p: &mut XmlParser) -> Option<i64> {
    text_of(p).trim().parse().ok()
}

/// A small code such as a rate unit or contour: outside `u8` stays absent.
fn opt_u8_of(p: &mut XmlParser) -> Option<u8> {
    opt_int_of(p).and_then(|n| u8::try_from(n).ok())
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
/// Optional durations (baselines, progress, stored work, resource work) need to
/// distinguish a recorded zero from an unavailable one; only the required task
/// `Duration` and assignment `Work` keep the permissive parser above.
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
/// [`PROJECT_HEADER`] order, then any others in read order. Outline code
/// definitions, WBS masks and extended attribute definitions follow in schema
/// order. Other elements outside the model (views, etc.) are not preserved:
/// this is a model-faithful writer, not a byte-faithful one.
///
/// An auto-scheduled task's stored `Start`/`Finish` are written as the dates
/// docxy schedules it to (its `EarlyStart`/`EarlyFinish`), not the ones it was
/// read with (one it was read without stays absent), so a scheduled project
/// exports with dates Project can display without recalculating. The stored
/// dates are kept, written when present, for manual tasks, blank rows,
/// external placeholders, tasks the schedule skips and auto summaries with
/// nothing scheduled below them, and for every task of a plan with an input
/// Project schedules by and docxy's schedule ignores: `ScheduleFromStart` 0,
/// a non-zero task or assignment `LevelingDelay` or assignment `Delay`, an
/// elapsed task `DurationFormat`, or a work resource whose calendar differs
/// from its task's. See `scheduled_dates` and `schedule_reproduces`.
///
/// The header always says `<ProjectExternallyEdited>0</ProjectExternallyEdited>`,
/// whatever the source said: the saved `<Duration>`s are docxy's own and
/// authoritative, and without it Project recomputes them from Start/Finish.
///
/// The computed task fields (`OutlineNumber`, early/late dates, the four
/// slacks, `Critical`) come from [`crate::schedule::schedule`], never from
/// values read from a file. That schedule honours each calendar's daily
/// (`Type 1`) exceptions. Known limits, which no cheap check detects, so a
/// plan with one still saves the schedule's dates: recurring exceptions
/// (`Type` 2-8, or a `Period` above 1) are written back but not scheduled, and
/// an untracked split (a gap in a task's work that is not a recorded
/// Stop/Resume) is not modelled. On such a plan these fields and an auto
/// task's `Start`/`Finish` can differ from Project's, and the difference
/// spreads through links, rollups and late dates to other tasks.
///
/// Assignment and resource values are written as the model holds them, never
/// recomputed here: Project trusts them, and an unedited file keeps its own.
/// The editor refreshes the ones an edit made stale (assignment dates, cost
/// and remaining work, task and resource totals) when it makes the edit; see
/// `assign::refresh`.
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
    // Definition blocks follow header leaves and precede Calendars and Tasks.
    if !proj.outline_code_definitions.is_empty() {
        s.push_str("  <OutlineCodes>\n");
        for definition in &proj.outline_code_definitions {
            write_element(&mut s, 2, definition);
        }
        s.push_str("  </OutlineCodes>\n");
    }
    if let Some(block) = &proj.wbs_masks {
        write_element(&mut s, 1, block);
    }
    if !proj.extended_attribute_definitions.is_empty() {
        s.push_str("  <ExtendedAttributes>\n");
        for definition in &proj.extended_attribute_definitions {
            write_element(&mut s, 2, definition);
        }
        s.push_str("  </ExtendedAttributes>\n");
    }

    s.push_str("  <Tasks>\n");
    let sched = crate::schedule::schedule(proj);
    let numbers = outline_numbers(&proj.tasks);
    let dates = scheduled_dates(proj, &sched);
    for ((t, number), &dates) in proj.tasks.iter().zip(&numbers).zip(&dates) {
        let computed = Computed {
            outline_number: number.as_deref(),
            result: sched.get(t.uid).filter(|_| !t.is_null),
            dates,
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
    let stored = || proj.option(name).map(str::to_string);
    match name {
        "Name" => Some(proj.name.clone()),
        "Title" => (!proj.title.is_empty()).then(|| proj.title.clone()),
        "StartDate" => proj.start_date.map(|d| d.to_mspdi()),
        "CalendarUID" => Some(proj.default_calendar_uid.to_string()),
        "MinutesPerDay" => Some(((proj.hours_per_day * 60.0).round() as i64).to_string()),
        "MinutesPerWeek" => Some(((proj.hours_per_week * 60.0).round() as i64).to_string()),
        "HonorConstraints" => flag(proj.honor_constraints),
        "NewTasksAreManual" => flag(proj.new_tasks_are_manual),
        // The options below are written only when the file stated them (or
        // an edit set them); unparseable text falls back to the stored option.
        "NewTasksEffortDriven" => proj.new_tasks_effort_driven.and_then(flag).or_else(stored),
        "NewTasksEstimated" => proj.new_tasks_estimated.and_then(flag).or_else(stored),
        "DefaultTaskType" => proj
            .default_task_type
            .map(|t| t.code().to_string())
            .or_else(stored),
        "Autolink" => proj.autolink.and_then(flag).or_else(stored),
        "CriticalSlackLimit" => proj
            .critical_slack_limit_days
            .map(|d| d.to_string())
            .or_else(stored),
        "MultipleCriticalPaths" => proj.multiple_critical_paths.and_then(flag).or_else(stored),
        // Without `0`, Project treats the file as edited outside Project,
        // ignores each `<Duration>` and recomputes it from Start/Finish (#111).
        "ProjectExternallyEdited" => Some("0".into()),
        _ => stored(),
    }
}

/// The computed values a save writes for one task, from docxy's own schedule.
struct Computed<'a> {
    outline_number: Option<&'a str>,
    result: Option<&'a TaskResult>,
    /// The `Start`/`Finish` the schedule places the task at, written in place
    /// of its stored dates; `None` keeps the stored ones.
    dates: Option<(DateTime, DateTime)>,
}

/// Where the schedule puts each task, which a save writes as its
/// `Start`/`Finish` (by row): Project trusts those, and the ones an auto task
/// was read with are stale once anything it depends on moved (#343). `None`
/// keeps the stored dates:
///
/// - a blank row's and an external placeholder's are not ours;
/// - a manual task's are what it is pinned to (and a TBD one's absent Start is
///   what keeps it TBD on reload);
/// - a task the schedule skips has none;
/// - an auto summary with nothing scheduled below it spans its own stored
///   dates, which the schedule only clamps;
/// - every task of a plan the schedule is known not to reproduce, where
///   Project's dates are the better ones; see [`schedule_reproduces`].
fn scheduled_dates(
    proj: &Project,
    sched: &crate::schedule::Schedule,
) -> Vec<Option<(DateTime, DateTime)>> {
    if !schedule_reproduces(proj) {
        return vec![None; proj.tasks.len()];
    }
    proj.tasks
        .iter()
        .map(|t| {
            if t.is_null || t.is_external_leaf() || t.manual {
                return None;
            }
            if t.summary && sched.rolled_up(t.uid).is_none() {
                return None;
            }
            sched.get(t.uid).map(|r| (r.early_start, r.early_finish))
        })
        .collect()
}

/// Whether nothing in the plan is an input Project schedules by and docxy's
/// schedule ignores. Each such input moves its task, and through links,
/// rollups and shared resources others, so a plan with one keeps every task's
/// stored dates. The inputs checked, each cheap and objective:
///
/// - `ScheduleFromStart` 0: docxy schedules forward;
/// - a non-zero task or assignment `LevelingDelay`: Project's leveling result,
///   which the unleveled schedule ignores;
/// - a non-zero assignment `Delay`, which only the leveling pass books;
/// - an elapsed `DurationFormat` on a task: the schedule counts every duration
///   in working time;
/// - a work assignment whose resource's calendar has other working time than
///   its task's calendar (resource calendars are not scheduled), unless the
///   task ignores resource calendars. A resource calendar that states nothing
///   of its own and derives from the task's calendar, as Project makes one for
///   each resource, has the same working time.
///
/// Known limits, not checked: recurring calendar exceptions and untracked
/// splits, see [`write_mspdi`].
fn schedule_reproduces(proj: &Project) -> bool {
    let nonzero = |value: Option<i64>| value.is_some_and(|v| v != 0);
    if proj.option("ScheduleFromStart").and_then(parse_bool) == Some(false) {
        return false;
    }
    let elapsed = |t: &Task| {
        t.duration_format
            .and_then(|code| LagFormat::from_code(i64::from(code)))
            .is_some_and(|f| f.kind() == LagKind::Elapsed)
    };
    if proj
        .tasks
        .iter()
        .any(|t| nonzero(t.leveling_delay) || (!t.is_null && !t.summary && elapsed(t)))
    {
        return false;
    }
    !proj.assignments.iter().any(|a| {
        nonzero(a.leveling_delay) || nonzero(a.delay) || resource_calendar_differs(proj, a)
    })
}

/// Whether `a`'s work resource works on a calendar other than its task's.
fn resource_calendar_differs(proj: &Project, a: &Assignment) -> bool {
    let Some(task) = proj.task(a.task_uid) else {
        return false;
    };
    let Some(resource) = proj.resources.iter().find(|r| r.uid == a.resource_uid) else {
        return false;
    };
    if resource.kind != ResourceType::Work || task.ignore_resource_calendar == Some(true) {
        return false;
    }
    // No resource calendar (none, or a UID such as -1 that names none) has
    // nothing to differ by: nothing else in docxy resolves one either.
    let Some(mut uid) = resource.calendar_uid else {
        return false;
    };
    // The task's calendar as the scheduler resolves it: a UID naming no
    // calendar (Project writes -1 for "none") is the project's.
    let task_uid = task
        .calendar_uid
        .filter(|&uid| proj.calendar(uid).is_some())
        .unwrap_or(proj.default_calendar_uid);
    // Follow calendars that state nothing of their own to the one they take
    // their time from.
    for _ in 0..proj.calendars.len() {
        let Some(cal) = proj.calendar(uid) else {
            return false;
        };
        if uid == task_uid {
            return false;
        }
        let own = cal.week.iter().any(Option::is_some)
            || !cal.exceptions.is_empty()
            || !cal.work_weeks.is_empty();
        match cal.base_calendar_uid {
            Some(base) if !own => uid = base,
            _ => return true,
        }
    }
    true
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

fn opt_rate(s: &mut String, name: &str, value: Option<&Rate>) {
    opt_text(s, name, value.map(Rate::as_str));
}

fn opt_work(s: &mut String, name: &str, value: Option<i64>) {
    opt_text(s, name, value.map(min_to_iso));
}

/// Write one task's children in the MSPDI `Task` sequence (the order Project
/// 2024 writes them). A blank row writes only what it stores: no computed
/// fields and none of the elements every task otherwise states.
fn write_task(s: &mut String, t: &Task, computed: &Computed) {
    let task = !t.is_null;
    // Display dates for an external placeholder are not local calculations.
    // Keep its stored Start/Finish; do not synthesize Critical or slack.
    let result = if t.is_external_leaf() {
        None
    } else {
        computed.result
    };
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
    opt_text(s, "Contact", t.contact.as_deref());
    opt_text(s, "WBS", t.wbs.as_ref());
    opt_text(s, "WBSLevel", t.wbs_level.as_deref());
    opt_text(s, "OutlineNumber", computed.outline_number);
    tag(s, 3, "OutlineLevel", &t.outline_level.to_string());
    opt_text(s, "Priority", t.priority);
    // A scheduled date replaces a stored one; an absent one stays absent.
    let (start, finish) = match computed.dates {
        Some((start, finish)) => (
            t.stored_start.and(Some(start)),
            t.stored_finish.and(Some(finish)),
        ),
        None => (t.stored_start, t.stored_finish),
    };
    opt_date(s, "Start", start);
    opt_date(s, "Finish", finish);
    if task || t.duration_min != 0 {
        tag(s, 3, "Duration", &min_to_iso(t.duration_min));
    }
    opt_date(s, "ManualStart", t.manual_start);
    opt_date(s, "ManualFinish", t.manual_finish);
    opt_text(s, "ManualDuration", t.manual_duration_min.map(min_to_iso));
    if task {
        // No format is days, as docxy always wrote.
        tag(
            s,
            3,
            "DurationFormat",
            &t.duration_format.unwrap_or(7).to_string(),
        );
    }
    opt_text(s, "Work", t.work_min.map(min_to_iso));
    opt_date(s, "Stop", t.stop);
    opt_date(s, "Resume", t.resume);
    opt_flag(s, "ResumeValid", t.resume_valid);
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
    opt_flag(s, "DisplayAsSummary", t.display_as_summary);
    opt_flag(s, "Critical", result.map(|r| r.critical));
    opt_flag(s, "IsSubproject", t.is_subproject);
    opt_flag(s, "IsSubprojectReadOnly", t.is_subproject_read_only);
    opt_text(s, "SubprojectName", t.subproject_name.as_deref());
    opt_flag(s, "ExternalTask", t.external_task);
    opt_text(s, "ExternalTaskProject", t.external_task_project.as_ref());
    if let Some(r) = result {
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
    if let Some(r) = result {
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
    opt_text(s, "FixedCost", t.fixed_cost.as_ref().map(Rate::as_str));
    opt_text(
        s,
        "FixedCostAccrual",
        t.fixed_cost_accrual.map(AccrueAt::code),
    );
    opt_text(s, "PercentComplete", t.percent_complete);
    opt_text(s, "PercentWorkComplete", t.percent_work_complete);
    opt_text(s, "Cost", t.cost.as_ref().map(Rate::as_str));
    opt_text(
        s,
        "OvertimeCost",
        t.overtime_cost.as_ref().map(Rate::as_str),
    );
    opt_text(s, "OvertimeWork", t.overtime_work_min.map(min_to_iso));
    opt_date(s, "ActualStart", t.actual_start);
    opt_date(s, "ActualFinish", t.actual_finish);
    opt_text(s, "ActualDuration", t.actual_duration_min.map(min_to_iso));
    opt_text(s, "ActualCost", t.actual_cost.as_ref().map(Rate::as_str));
    opt_text(
        s,
        "ActualOvertimeCost",
        t.actual_overtime_cost.as_ref().map(Rate::as_str),
    );
    opt_text(s, "ActualWork", t.actual_work_min.map(min_to_iso));
    opt_text(
        s,
        "ActualOvertimeWork",
        t.actual_overtime_work_min.map(min_to_iso),
    );
    opt_text(s, "RegularWork", t.regular_work_min.map(min_to_iso));
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
    opt_text(
        s,
        "RemainingOvertimeCost",
        t.remaining_overtime_cost.as_ref().map(Rate::as_str),
    );
    opt_text(
        s,
        "RemainingOvertimeWork",
        t.remaining_overtime_work_min.map(min_to_iso),
    );
    opt_text(s, "ACWP", t.acwp.as_ref().map(Rate::as_str));
    opt_text(s, "CV", t.cv.as_ref().map(Rate::as_str));
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
    opt_date(s, "PreLeveledStart", t.pre_leveled_start);
    opt_date(s, "PreLeveledFinish", t.pre_leveled_finish);
    opt_text(s, "Hyperlink", t.hyperlink.as_deref());
    opt_text(s, "HyperlinkAddress", t.hyperlink_address.as_deref());
    opt_text(s, "HyperlinkSubAddress", t.hyperlink_sub_address.as_deref());
    opt_flag(s, "IgnoreResourceCalendar", t.ignore_resource_calendar);
    opt_text(s, "Notes", t.notes.as_deref());
    opt_flag(s, "HideBar", t.hide_bar);
    opt_flag(s, "Rollup", t.rollup);
    opt_text(s, "BCWS", t.bcws.as_ref().map(Rate::as_str));
    opt_text(s, "BCWP", t.bcwp.as_ref().map(Rate::as_str));
    opt_text(s, "PhysicalPercentComplete", t.physical_percent_complete);
    opt_text(s, "EarnedValueMethod", t.earned_value_method);
    for p in &t.predecessors {
        s.push_str("      <PredecessorLink>\n");
        tag(s, 4, "PredecessorUID", &p.uid.to_string());
        tag(s, 4, "Type", &p.link.code().to_string());
        if let Some(value) = p.cross_project {
            tag(s, 4, "CrossProject", flag(value));
        }
        if let Some(name) = &p.cross_project_name {
            tag(s, 4, "CrossProjectName", name);
        }
        tag(s, 4, "LinkLag", &link_lag_of(p).to_string());
        tag(s, 4, "LagFormat", &p.lag_format.code().to_string());
        s.push_str("      </PredecessorLink>\n");
    }
    opt_text(
        s,
        "ActualWorkProtected",
        t.actual_work_protected_min.map(min_to_iso),
    );
    opt_text(
        s,
        "ActualOvertimeWorkProtected",
        t.actual_overtime_work_protected_min.map(min_to_iso),
    );
    write_extended_attributes(s, &t.extended_attributes);
    let mut baselines: Vec<_> = t.baselines.iter().collect();
    baselines.sort_by_key(|b| b.number);
    for baseline in baselines {
        s.push_str("      <Baseline>\n");
        write_timephased_data(s, &baseline.timephased_data, 4);
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
        if let Some(format) = baseline.duration_format {
            tag(s, 4, "DurationFormat", &format.to_string());
        }
        if let Some(work) = baseline.work_min {
            tag(s, 4, "Work", &min_to_iso(work));
        }
        if let Some(cost) = baseline.cost.as_ref() {
            tag(s, 4, "Cost", cost.as_str());
        }
        s.push_str("      </Baseline>\n");
    }
    write_outline_codes(s, &t.outline_codes);
    opt_flag(s, "IsPublished", t.is_published);
    opt_text(s, "StatusManager", t.status_manager.as_deref());
    opt_date(s, "CommitmentStart", t.commitment_start);
    opt_date(s, "CommitmentFinish", t.commitment_finish);
    opt_text(s, "CommitmentType", t.commitment_type);
    write_timephased_data(s, &t.timephased_data, 3);
    s.push_str("    </Task>\n");
}

fn write_resource(s: &mut String, r: &Resource) {
    s.push_str("    <Resource>\n");
    // Microsoft's Resource sequence as Project 2010+ writes it (Project 2024
    // exports agree), including IsCostResource after CostCenter (Type itself
    // only permits 0 and 1).
    tag(s, 3, "UID", &r.uid.to_string());
    opt_text(s, "GUID", r.guid.as_ref());
    tag(s, 3, "ID", &r.id.to_string());
    tag(s, 3, "Name", &r.name);
    tag(s, 3, "Type", &r.kind.code().to_string());
    opt_flag(s, "IsNull", r.is_null);
    for (name, value) in [
        ("Initials", &r.initials),
        ("Phonetics", &r.phonetics),
        ("NTAccount", &r.nt_account),
        ("MaterialLabel", &r.material_label),
        ("Code", &r.code),
        ("Group", &r.group),
    ] {
        if let Some(value) = value {
            tag(s, 3, name, value);
        }
    }
    opt_text(s, "WorkGroup", r.work_group);
    opt_text(s, "EmailAddress", r.email_address.as_deref());
    opt_text(s, "Hyperlink", r.hyperlink.as_deref());
    opt_text(s, "HyperlinkAddress", r.hyperlink_address.as_deref());
    opt_text(s, "HyperlinkSubAddress", r.hyperlink_sub_address.as_deref());
    tag(s, 3, "MaxUnits", &fmt_f(r.max_units));
    opt_rate(s, "PeakUnits", r.peak_units.as_ref());
    opt_flag(s, "OverAllocated", r.over_allocated);
    opt_date(s, "AvailableFrom", r.available_from);
    opt_date(s, "AvailableTo", r.available_to);
    opt_date(s, "Start", r.start);
    opt_date(s, "Finish", r.finish);
    opt_flag(s, "CanLevel", r.can_level);
    if let Some(accrue_at) = r.accrue_at {
        tag(s, 3, "AccrueAt", &accrue_at.code().to_string());
    }
    opt_work(s, "Work", r.work_min);
    opt_work(s, "RegularWork", r.regular_work_min);
    opt_work(s, "OvertimeWork", r.overtime_work_min);
    opt_work(s, "ActualWork", r.actual_work_min);
    opt_work(s, "RemainingWork", r.remaining_work_min);
    opt_work(s, "ActualOvertimeWork", r.actual_overtime_work_min);
    opt_work(s, "RemainingOvertimeWork", r.remaining_overtime_work_min);
    opt_text(s, "PercentWorkComplete", r.percent_work_complete);
    opt_rate(s, "StandardRate", r.standard_rate.as_ref());
    opt_text(s, "StandardRateFormat", r.standard_rate_format);
    opt_rate(s, "Cost", r.cost.as_ref());
    opt_rate(s, "OvertimeRate", r.overtime_rate.as_ref());
    opt_text(s, "OvertimeRateFormat", r.overtime_rate_format);
    opt_rate(s, "OvertimeCost", r.overtime_cost.as_ref());
    opt_rate(s, "CostPerUse", r.cost_per_use.as_ref());
    opt_rate(s, "ActualCost", r.actual_cost.as_ref());
    opt_rate(s, "ActualOvertimeCost", r.actual_overtime_cost.as_ref());
    opt_rate(s, "RemainingCost", r.remaining_cost.as_ref());
    opt_rate(
        s,
        "RemainingOvertimeCost",
        r.remaining_overtime_cost.as_ref(),
    );
    opt_rate(s, "WorkVariance", r.work_variance.as_ref());
    opt_rate(s, "CostVariance", r.cost_variance.as_ref());
    opt_rate(s, "SV", r.sv.as_ref());
    opt_rate(s, "CV", r.cv.as_ref());
    opt_rate(s, "ACWP", r.acwp.as_ref());
    if let Some(c) = r.calendar_uid {
        tag(s, 3, "CalendarUID", &c.to_string());
    }
    opt_text(s, "Notes", r.notes.as_deref());
    opt_rate(s, "BCWS", r.bcws.as_ref());
    opt_rate(s, "BCWP", r.bcwp.as_ref());
    opt_flag(s, "IsGeneric", r.is_generic);
    opt_flag(s, "IsInactive", r.is_inactive);
    opt_flag(s, "IsEnterprise", r.is_enterprise);
    opt_text(s, "BookingType", r.booking_type);
    opt_work(s, "ActualWorkProtected", r.actual_work_protected_min);
    opt_work(
        s,
        "ActualOvertimeWorkProtected",
        r.actual_overtime_work_protected_min,
    );
    opt_text(s, "ActiveDirectoryGUID", r.active_directory_guid.as_deref());
    opt_date(s, "CreationDate", r.creation_date);
    opt_text(s, "CostCenter", r.cost_center.as_deref());
    if r.kind == ResourceType::Cost {
        tag(s, 3, "IsCostResource", "1");
    }
    opt_text(s, "AssnOwner", r.assn_owner.as_deref());
    opt_text(s, "AssnOwnerGuid", r.assn_owner_guid.as_deref());
    opt_flag(s, "IsBudget", r.is_budget);
    write_extended_attributes(s, &r.extended_attributes);
    for baseline in &r.baselines {
        s.push_str("      <Baseline>\n");
        write_timephased_data(s, &baseline.timephased_data, 4);
        tag(s, 4, "Number", &baseline.number.to_string());
        if let Some(work) = baseline.work_min {
            tag(s, 4, "Work", &min_to_iso(work));
        }
        for (name, value) in [
            ("Cost", &baseline.cost),
            ("BCWS", &baseline.bcws),
            ("BCWP", &baseline.bcwp),
        ] {
            if let Some(value) = value {
                tag(s, 4, name, value.as_str());
            }
        }
        s.push_str("      </Baseline>\n");
    }
    write_outline_codes(s, &r.outline_codes);
    if !r.availability_periods.is_empty() {
        s.push_str("      <AvailabilityPeriods>\n");
        for period in &r.availability_periods {
            s.push_str("        <AvailabilityPeriod>\n");
            if let Some(from) = period.available_from {
                tag(s, 5, "AvailableFrom", &from.to_mspdi());
            }
            if let Some(to) = period.available_to {
                tag(s, 5, "AvailableTo", &to.to_mspdi());
            }
            if let Some(units) = &period.available_units {
                tag(s, 5, "AvailableUnits", units.as_str());
            }
            s.push_str("        </AvailabilityPeriod>\n");
        }
        s.push_str("      </AvailabilityPeriods>\n");
    }
    if !r.rates.is_empty() {
        s.push_str("      <Rates>\n");
        for rate in &r.rates {
            s.push_str("        <Rate>\n");
            if let Some(from) = rate.rates_from {
                tag(s, 5, "RatesFrom", &from.to_mspdi());
            }
            if let Some(to) = rate.rates_to {
                tag(s, 5, "RatesTo", &to.to_mspdi());
            }
            let codes = |code: Option<u8>| code.map(|c| c.to_string());
            for (name, value) in [
                ("RateTable", codes(rate.rate_table)),
                (
                    "StandardRate",
                    rate.standard_rate.as_ref().map(|r| r.as_str().to_owned()),
                ),
                ("StandardRateFormat", codes(rate.standard_rate_format)),
                (
                    "OvertimeRate",
                    rate.overtime_rate.as_ref().map(|r| r.as_str().to_owned()),
                ),
                ("OvertimeRateFormat", codes(rate.overtime_rate_format)),
                (
                    "CostPerUse",
                    rate.cost_per_use.as_ref().map(|r| r.as_str().to_owned()),
                ),
            ] {
                if let Some(value) = value {
                    tag(s, 5, name, &value);
                }
            }
            s.push_str("        </Rate>\n");
        }
        s.push_str("      </Rates>\n");
    }
    write_timephased_data(s, &r.timephased_data, 3);
    s.push_str("    </Resource>\n");
}

fn write_outline_codes(s: &mut String, codes: &[OutlineCodeValue]) {
    for code in codes {
        s.push_str("      <OutlineCode>\n");
        tag(s, 4, "FieldID", &code.field_id);
        if let Some(value_id) = &code.value_id {
            tag(s, 4, "ValueID", value_id);
        }
        if let Some(guid) = &code.value_guid {
            tag(s, 4, "ValueGUID", guid);
        }
        s.push_str("      </OutlineCode>\n");
    }
}

/// Custom field values, in the schema's child order.
fn write_extended_attributes(s: &mut String, attributes: &[ExtendedAttributeValue]) {
    for attribute in attributes {
        s.push_str("      <ExtendedAttribute>\n");
        tag(s, 4, "FieldID", &attribute.field_id);
        if let Some(value) = &attribute.value {
            tag(s, 4, "Value", value);
        }
        if let Some(guid) = &attribute.value_guid {
            tag(s, 4, "ValueGUID", guid);
        }
        if let Some(format) = attribute.duration_format {
            tag(s, 4, "DurationFormat", &format.to_string());
        }
        s.push_str("      </ExtendedAttribute>\n");
    }
}

fn write_assignment(s: &mut String, a: &Assignment) {
    s.push_str("    <Assignment>\n");
    // Microsoft's Assignment sequence as Project 2010+ writes it (Project
    // 2024 exports agree).
    tag(s, 3, "UID", &a.uid.to_string());
    opt_text(s, "GUID", a.guid.as_ref());
    tag(s, 3, "TaskUID", &a.task_uid.to_string());
    tag(s, 3, "ResourceUID", &a.resource_uid.to_string());
    opt_text(s, "PercentWorkComplete", a.percent_work_complete);
    opt_rate(s, "ActualCost", a.actual_cost.as_ref());
    opt_date(s, "ActualFinish", a.actual_finish);
    opt_rate(s, "ActualOvertimeCost", a.actual_overtime_cost.as_ref());
    opt_work(s, "ActualOvertimeWork", a.actual_overtime_work_min);
    opt_date(s, "ActualStart", a.actual_start);
    opt_work(s, "ActualWork", a.actual_work_min);
    opt_rate(s, "ACWP", a.acwp.as_ref());
    opt_flag(s, "Confirmed", a.confirmed);
    opt_rate(s, "Cost", a.cost.as_ref());
    opt_text(s, "CostRateTable", a.cost_rate_table);
    opt_text(s, "RateScale", a.rate_scale);
    opt_rate(s, "CostVariance", a.cost_variance.as_ref());
    opt_rate(s, "CV", a.cv.as_ref());
    opt_text(s, "Delay", a.delay);
    opt_date(s, "Finish", a.finish);
    opt_text(s, "FinishVariance", a.finish_variance);
    opt_text(s, "Hyperlink", a.hyperlink.as_deref());
    opt_text(s, "HyperlinkAddress", a.hyperlink_address.as_deref());
    opt_text(s, "HyperlinkSubAddress", a.hyperlink_sub_address.as_deref());
    opt_rate(s, "WorkVariance", a.work_variance.as_ref());
    opt_flag(s, "HasFixedRateUnits", a.has_fixed_rate_units);
    opt_flag(s, "FixedMaterial", a.fixed_material);
    opt_text(s, "LevelingDelay", a.leveling_delay);
    opt_text(s, "LevelingDelayFormat", a.leveling_delay_format);
    opt_flag(s, "LinkedFields", a.linked_fields);
    opt_flag(s, "Milestone", a.milestone);
    opt_text(s, "Notes", a.notes.as_deref());
    opt_flag(s, "Overallocated", a.overallocated);
    opt_rate(s, "OvertimeCost", a.overtime_cost.as_ref());
    opt_work(s, "OvertimeWork", a.overtime_work_min);
    opt_rate(s, "PeakUnits", a.peak_units.as_ref());
    opt_work(s, "RegularWork", a.regular_work_min);
    opt_rate(s, "RemainingCost", a.remaining_cost.as_ref());
    opt_rate(
        s,
        "RemainingOvertimeCost",
        a.remaining_overtime_cost.as_ref(),
    );
    opt_work(s, "RemainingOvertimeWork", a.remaining_overtime_work_min);
    opt_work(s, "RemainingWork", a.remaining_work_min);
    opt_flag(s, "ResponsePending", a.response_pending);
    opt_date(s, "Start", a.start);
    opt_date(s, "Stop", a.stop);
    opt_date(s, "Resume", a.resume);
    opt_text(s, "StartVariance", a.start_variance);
    opt_flag(s, "Summary", a.summary);
    opt_rate(s, "SV", a.sv.as_ref());
    tag(s, 3, "Units", &fmt_f(a.units));
    opt_flag(s, "UpdateNeeded", a.update_needed);
    opt_rate(s, "VAC", a.vac.as_ref());
    tag(s, 3, "Work", &min_to_iso(a.work_min));
    opt_text(s, "WorkContour", a.work_contour);
    opt_rate(s, "BCWS", a.bcws.as_ref());
    opt_rate(s, "BCWP", a.bcwp.as_ref());
    opt_text(s, "BookingType", a.booking_type);
    opt_work(s, "ActualWorkProtected", a.actual_work_protected_min);
    opt_work(
        s,
        "ActualOvertimeWorkProtected",
        a.actual_overtime_work_protected_min,
    );
    opt_date(s, "CreationDate", a.creation_date);
    opt_text(s, "AssnOwner", a.assn_owner.as_deref());
    opt_text(s, "AssnOwnerGuid", a.assn_owner_guid.as_deref());
    opt_rate(s, "BudgetCost", a.budget_cost.as_ref());
    opt_work(s, "BudgetWork", a.budget_work_min);
    write_extended_attributes(s, &a.extended_attributes);
    for baseline in &a.baselines {
        s.push_str("      <Baseline>\n");
        write_timephased_data(s, &baseline.timephased_data, 4);
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
        for (name, value) in [
            ("Cost", &baseline.cost),
            ("BCWS", &baseline.bcws),
            ("BCWP", &baseline.bcwp),
        ] {
            if let Some(value) = value {
                tag(s, 4, name, value.as_str());
            }
        }
        s.push_str("      </Baseline>\n");
    }
    write_timephased_data(s, &a.timephased_data, 3);
    s.push_str("    </Assignment>\n");
}

/// Timephased records, in the schema's child order.
fn write_timephased_data(s: &mut String, records: &[TimephasedValue], depth: usize) {
    for record in records {
        s.push_str(&"  ".repeat(depth));
        s.push_str("<TimephasedData>\n");
        tag(s, depth + 1, "Type", &record.kind.to_string());
        if let Some(uid) = record.uid {
            tag(s, depth + 1, "UID", &uid.to_string());
        }
        if let Some(start) = record.start {
            tag(s, depth + 1, "Start", &start.to_mspdi());
        }
        if let Some(finish) = record.finish {
            tag(s, depth + 1, "Finish", &finish.to_mspdi());
        }
        if let Some(unit) = record.unit {
            tag(s, depth + 1, "Unit", &unit.to_string());
        }
        if let Some(value) = &record.value {
            tag(s, depth + 1, "Value", value);
        }
        s.push_str(&"  ".repeat(depth));
        s.push_str("</TimephasedData>\n");
    }
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
    // Like Project, each exception the scheduler honours is also written in
    // the legacy form, a `DayType 0` weekday over its dates.
    let legacy: Vec<(&CalendarException, DateTime, DateTime)> = c
        .exceptions
        .iter()
        .filter(|e| e.scheduled().is_some())
        .filter_map(|e| Some((e, e.from?, e.to?)))
        .collect();
    if !days.is_empty() || !legacy.is_empty() {
        s.push_str("      <WeekDays>\n");
        for (idx, day) in days {
            // model week[] is Sun=0..Sat=6; MSPDI DayType is 1=Sun..7=Sat.
            s.push_str("        <WeekDay>\n");
            tag(s, 5, "DayType", &(idx + 1).to_string());
            write_day_working(s, day, 5);
            s.push_str("        </WeekDay>\n");
        }
        for (e, from, to) in legacy {
            s.push_str("        <WeekDay>\n");
            tag(s, 5, "DayType", "0");
            tag(s, 5, "DayWorking", if e.day.working() { "1" } else { "0" });
            write_time_period(s, from, to);
            write_working_times(s, &e.day, 5);
            s.push_str("        </WeekDay>\n");
        }
        s.push_str("      </WeekDays>\n");
    }
    if !c.exceptions.is_empty() {
        s.push_str("      <Exceptions>\n");
        for e in &c.exceptions {
            write_exception(s, e);
        }
        s.push_str("      </Exceptions>\n");
    }
    if !c.work_weeks.is_empty() {
        write_work_weeks(s, &c.work_weeks);
    }
    s.push_str("    </Calendar>\n");
}

fn write_work_weeks(s: &mut String, weeks: &[WorkWeek]) {
    s.push_str("      <WorkWeeks>\n");
    for work_week in weeks {
        s.push_str("        <WorkWeek>\n");
        write_optional_time_period(s, work_week.from, work_week.to, 5);
        if let Some(name) = &work_week.name {
            tag(s, 5, "Name", name);
        }
        if work_week.week.iter().any(Option::is_some) {
            s.push_str("          <WeekDays>\n");
            for (idx, day) in work_week.week.iter().enumerate() {
                let Some(day) = day else { continue };
                s.push_str("            <WeekDay>\n");
                tag(s, 7, "DayType", &(idx + 1).to_string());
                write_day_working(s, day, 7);
                s.push_str("            </WeekDay>\n");
            }
            s.push_str("          </WeekDays>\n");
        }
        s.push_str("        </WorkWeek>\n");
    }
    s.push_str("      </WorkWeeks>\n");
}

/// One `Exception`, its elements in schema order. An optional field the
/// reader found absent stays absent; `DayWorking` is always written, from
/// whether the day has working times, as for a weekday.
fn write_exception(s: &mut String, e: &CalendarException) {
    let int = |s: &mut String, name: &str, value: Option<i32>| {
        if let Some(value) = value {
            tag(s, 5, name, &value.to_string());
        }
    };
    s.push_str("        <Exception>\n");
    if let Some(entered) = e.entered_by_occurrences {
        tag(s, 5, "EnteredByOccurrences", flag(entered));
    }
    write_optional_time_period(s, e.from, e.to, 5);
    int(s, "Occurrences", e.occurrences);
    if let Some(name) = &e.name {
        tag(s, 5, "Name", name);
    }
    int(s, "Type", e.kind);
    int(s, "Period", e.period);
    int(s, "DaysOfWeek", e.days_of_week);
    int(s, "MonthItem", e.month_item);
    int(s, "MonthPosition", e.month_position);
    int(s, "Month", e.month);
    int(s, "MonthDay", e.month_day);
    write_day_working(s, &e.day, 5);
    s.push_str("        </Exception>\n");
}

/// A legacy weekday's `TimePeriod`.
fn write_time_period(s: &mut String, from: DateTime, to: DateTime) {
    write_optional_time_period(s, Some(from), Some(to), 5);
}

fn write_optional_time_period(
    s: &mut String,
    from: Option<DateTime>,
    to: Option<DateTime>,
    depth: usize,
) {
    if from.is_none() && to.is_none() {
        return;
    }
    let indent = "  ".repeat(depth);
    s.push_str(&format!("{indent}<TimePeriod>\n"));
    if let Some(from) = from {
        tag(s, depth + 1, "FromDate", &from.to_mspdi());
    }
    if let Some(to) = to {
        tag(s, depth + 1, "ToDate", &to.to_mspdi());
    }
    s.push_str(&format!("{indent}</TimePeriod>\n"));
}

/// `DayWorking`, then `WorkingTimes` when the day works.
fn write_day_working(s: &mut String, day: &DayWorking, depth: usize) {
    tag(
        s,
        depth,
        "DayWorking",
        if day.working() { "1" } else { "0" },
    );
    write_working_times(s, day, depth);
}

fn write_working_times(s: &mut String, day: &DayWorking, depth: usize) {
    if !day.working() {
        return;
    }
    let indent = "  ".repeat(depth);
    let child_indent = "  ".repeat(depth + 1);
    s.push_str(&format!("{indent}<WorkingTimes>\n"));
    for w in &day.times {
        s.push_str(&format!("{child_indent}<WorkingTime>"));
        s.push_str(&format!(
            "<FromTime>{}</FromTime><ToTime>{}</ToTime>",
            min_to_clock(w.from),
            min_to_clock(w.to)
        ));
        s.push_str("</WorkingTime>\n");
    }
    s.push_str(&format!("{indent}</WorkingTimes>\n"));
}

/// Write an element kept as read: a leaf as `<Name>text</Name>`, else its
/// children one level deeper.
fn write_element(s: &mut String, depth: usize, element: &XmlElement) {
    if element.children.is_empty() {
        tag(s, depth, &element.name, &element.text);
        return;
    }
    let indent = "  ".repeat(depth);
    s.push_str(&format!("{indent}<{}>\n", element.name));
    for child in &element.children {
        write_element(s, depth + 1, child);
    }
    s.push_str(&format!("{indent}</{}>\n", element.name));
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
            // A conformant reader turns a raw CR (or CR LF) into LF.
            '\r' => out.push_str("&#13;"),
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

/// Issue #385's plan: two tasks entered in weeks (`DurationFormat` 9), one in
/// hours (5), the rest in days (7), on the default Standard calendar.
#[cfg(test)]
pub(crate) const DURATION_FORMATS_PLAN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Project xmlns="http://schemas.microsoft.com/project">
  <Name>formats</Name>
  <MinutesPerDay>480</MinutesPerDay>
  <MinutesPerWeek>2400</MinutesPerWeek>
  <StartDate>2026-03-02T08:00:00</StartDate>
  <Tasks>
    <Task><UID>1</UID><ID>1</ID><Name>S</Name><OutlineLevel>1</OutlineLevel>
      <Duration>PT8H0M0S</Duration><DurationFormat>7</DurationFormat></Task>
    <Task><UID>2</UID><ID>2</ID><Name>Half</Name><OutlineLevel>1</OutlineLevel>
      <Duration>PT4H0M0S</Duration><DurationFormat>5</DurationFormat>
      <PredecessorLink><PredecessorUID>1</PredecessorUID><Type>1</Type></PredecessorLink></Task>
    <Task><UID>3</UID><ID>3</ID><Name>Full</Name><OutlineLevel>1</OutlineLevel>
      <Duration>PT8H0M0S</Duration><DurationFormat>7</DurationFormat>
      <PredecessorLink><PredecessorUID>1</PredecessorUID><Type>1</Type></PredecessorLink></Task>
    <Task><UID>4</UID><ID>4</ID><Name>Merge</Name><OutlineLevel>1</OutlineLevel>
      <Duration>PT8H0M0S</Duration><DurationFormat>7</DurationFormat>
      <PredecessorLink><PredecessorUID>2</PredecessorUID><Type>1</Type></PredecessorLink>
      <PredecessorLink><PredecessorUID>3</PredecessorUID><Type>1</Type></PredecessorLink></Task>
    <Task><UID>5</UID><ID>5</ID><Name>WkShort</Name><OutlineLevel>1</OutlineLevel>
      <Duration>PT40H0M0S</Duration><DurationFormat>9</DurationFormat>
      <PredecessorLink><PredecessorUID>4</PredecessorUID><Type>1</Type></PredecessorLink></Task>
    <Task><UID>6</UID><ID>6</ID><Name>WkLong</Name><OutlineLevel>1</OutlineLevel>
      <Duration>PT60H0M0S</Duration><DurationFormat>9</DurationFormat>
      <PredecessorLink><PredecessorUID>4</PredecessorUID><Type>1</Type></PredecessorLink></Task>
    <Task><UID>7</UID><ID>7</ID><Name>End</Name><OutlineLevel>1</OutlineLevel>
      <Duration>PT8H0M0S</Duration><DurationFormat>7</DurationFormat>
      <PredecessorLink><PredecessorUID>5</PredecessorUID><Type>1</Type></PredecessorLink>
      <PredecessorLink><PredecessorUID>6</PredecessorUID><Type>1</Type></PredecessorLink></Task>
  </Tasks>
</Project>"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn uid_xml(tasks: &str) -> String {
        format!("<Project><Tasks>{tasks}</Tasks></Project>")
    }

    #[test]
    fn nested_baseline_timephased_records_round_trip_for_all_owners() {
        let record = "<TimephasedData><Type>4</Type><UID>7</UID><Start>2026-03-02T08:00:00</Start><Finish>2026-03-02T17:00:00</Finish><Unit>2</Unit><Value>PT8H0M0S</Value></TimephasedData>";
        let xml = format!(
            "<Project><Tasks><Task><UID>1</UID><ID>1</ID><Name>T</Name><Baseline>{record}<Number>1</Number></Baseline></Task></Tasks><Resources><Resource><UID>2</UID><ID>1</ID><Name>R</Name><Baseline>{record}<Number>1</Number></Baseline></Resource></Resources><Assignments><Assignment><UID>3</UID><TaskUID>1</TaskUID><ResourceUID>2</ResourceUID><Baseline>{record}<Number>1</Number></Baseline><TimephasedData><Type>1</Type><Value>PT1H0M0S</Value></TimephasedData></Assignment></Assignments></Project>"
        );
        let project = read_mspdi(&xml).unwrap();
        let task = project.tasks[0].baseline(1).unwrap();
        let resource = project.resources[0].baseline(1).unwrap();
        let assignment = project.assignments[0].baseline(1).unwrap();
        assert_eq!(task.timephased_data, resource.timephased_data);
        assert_eq!(task.timephased_data, assignment.timephased_data);
        assert_eq!(assignment.timephased_data.len(), 1);
        assert_eq!(
            assignment.timephased_data[0],
            TimephasedValue {
                kind: 4,
                uid: Some(7),
                start: DateTime::parse_mspdi("2026-03-02T08:00:00"),
                finish: DateTime::parse_mspdi("2026-03-02T17:00:00"),
                unit: Some(2),
                value: Some("PT8H0M0S".into()),
            }
        );
        assert_eq!(project.assignments[0].timephased_data.len(), 1);
        assert_eq!(project.assignments[0].timephased_data[0].kind, 1);

        let written = write_mspdi(&project);
        let baseline_blocks: Vec<_> = written.split("<Baseline>").skip(1).collect();
        assert_eq!(baseline_blocks.len(), 3);
        for block in baseline_blocks {
            let block = block.split("</Baseline>").next().unwrap();
            assert!(block.contains("        <TimephasedData>\n"));
            assert!(block.find("<TimephasedData>").unwrap() < block.find("<Number>").unwrap());
        }
        assert_eq!(read_mspdi(&written).unwrap(), project);
        assert_eq!(
            crate::yppx::read_yppx(&crate::yppx::write_yppx(&project).unwrap()).unwrap(),
            project
        );
    }

    #[test]
    fn nested_baseline_records_follow_slot_and_type_validation() {
        let typed = "<TimephasedData><Type>4</Type><Value>PT1H0M0S</Value></TimephasedData>";
        let untyped = "<TimephasedData><Value>PT2H0M0S</Value></TimephasedData>";
        let a = assignment_project(&format!(
            "<Assignment><UID>1</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID><Baseline>{typed}<Number>1</Number>{untyped}</Baseline><Baseline>{typed}<Number>11</Number></Baseline></Assignment>"
        ));
        assert_eq!(a.assignments[0].baselines.len(), 1);
        assert_eq!(
            a.assignments[0].baseline(1).unwrap().timephased_data.len(),
            1
        );
        assert_eq!(
            a.assignments[0].baseline(1).unwrap().timephased_data[0].kind,
            4
        );
        assert_eq!(read_mspdi(&write_mspdi(&a)).unwrap(), a);

        let later = "<TimephasedData><Type>5</Type><Value>2.5</Value></TimephasedData>";
        let duplicate = assignment_project(&format!(
            "<Assignment><UID>1</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID><Baseline>{typed}<Number>1</Number></Baseline><Baseline>{later}<Number>1</Number><Work>PT2H0M0S</Work></Baseline></Assignment>"
        ));
        let slot = duplicate.assignments[0].baseline(1).unwrap();
        assert_eq!(slot.timephased_data.len(), 1);
        assert_eq!(slot.timephased_data[0].kind, 5);
        assert_eq!(slot.timephased_data[0].value.as_deref(), Some("2.5"));
        assert_eq!(slot.work_min, Some(120));
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
            let package = crate::yppx::write_yppx(&proj).unwrap();
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
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
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
        let full = |kind| Resource {
            kind,
            initials: Some("A".into()),
            material_label: Some("unit".into()),
            code: Some("C7".into()),
            group: Some("Eng".into()),
            work_group: Some(1),
            peak_units: Rate::parse("1"),
            over_allocated: Some(false),
            can_level: Some(true),
            accrue_at: Some(AccrueAt::End),
            work_min: Some(480),
            regular_work_min: Some(480),
            remaining_work_min: Some(240),
            standard_rate: Rate::parse("50"),
            standard_rate_format: Some(3),
            overtime_rate: Rate::parse("75"),
            overtime_rate_format: Some(2),
            cost_per_use: Rate::parse("10"),
            calendar_uid: Some(1),
            is_generic: Some(false),
            is_inactive: Some(false),
            booking_type: Some(0),
            is_budget: Some(true),
            email_address: Some("a@example.com".into()),
            available_from: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            available_to: Some(DateTime::from_ymd_hm(2026, 12, 31, 17, 0)),
            overtime_work_min: Some(60),
            cost: Rate::parse("400"),
            notes: Some("note".into()),
            extended_attributes: vec![ExtendedAttributeValue {
                field_id: "205520904".into(),
                value: Some("x".into()),
                value_guid: Some("{0}".into()),
                duration_format: Some(7),
            }],
            baselines: vec![ResourceBaseline {
                number: 0,
                timephased_data: vec![],
                work_min: Some(480),
                cost: Rate::parse("400"),
                bcws: Rate::parse("1"),
                bcwp: Rate::parse("2"),
            }],
            availability_periods: vec![AvailabilityPeriod {
                available_from: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
                available_to: Some(DateTime::from_ymd_hm(2026, 12, 31, 17, 0)),
                available_units: Rate::parse("0.5"),
            }],
            rates: vec![RateEntry {
                rates_from: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
                rates_to: Some(DateTime::from_ymd_hm(2026, 12, 31, 17, 0)),
                rate_table: Some(1),
                standard_rate: Rate::parse("60"),
                standard_rate_format: Some(2),
                overtime_rate: Rate::parse("90"),
                overtime_rate_format: Some(2),
                cost_per_use: Rate::parse("5"),
            }],
            ..Resource::default()
        };
        for r in [full(ResourceType::Cost), full(ResourceType::Work)] {
            let mut xml = String::new();
            write_resource(&mut xml, &r);
            let mut expected = vec![
                "Resource",
                "UID",
                "ID",
                "Name",
                "Type",
                "Initials",
                "MaterialLabel",
                "Code",
                "Group",
                "WorkGroup",
                "EmailAddress",
                "MaxUnits",
                "PeakUnits",
                "OverAllocated",
                "AvailableFrom",
                "AvailableTo",
                "CanLevel",
                "AccrueAt",
                "Work",
                "RegularWork",
                "OvertimeWork",
                "RemainingWork",
                "StandardRate",
                "StandardRateFormat",
                "Cost",
                "OvertimeRate",
                "OvertimeRateFormat",
                "CostPerUse",
                "CalendarUID",
                "Notes",
                "IsGeneric",
                "IsInactive",
                "BookingType",
                "IsCostResource",
                "IsBudget",
                "ExtendedAttribute",
                "FieldID",
                "Value",
                "ValueGUID",
                "DurationFormat",
                "Baseline",
                "Number",
                "Work",
                "Cost",
                "BCWS",
                "BCWP",
                "AvailabilityPeriods",
                "AvailabilityPeriod",
                "AvailableFrom",
                "AvailableTo",
                "AvailableUnits",
                "Rates",
                "Rate",
                "RatesFrom",
                "RatesTo",
                "RateTable",
                "StandardRate",
                "StandardRateFormat",
                "OvertimeRate",
                "OvertimeRateFormat",
                "CostPerUse",
            ];
            if r.kind != ResourceType::Cost {
                expected.retain(|name| *name != "IsCostResource");
            }
            assert_eq!(element_names(&xml), expected, "{:?}", r.kind);
            assert_eq!(resource_project(&xml).resources, vec![r]);
        }
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
        let pred = &b.predecessors[0];
        assert_eq!(pred.uid, 1);
        assert_eq!(pred.link, LinkType::FinishStart);
        assert_eq!(pred.lag, 480); // 4800 tenths-of-min = 2 days = 8h/day
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
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
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
    fn work_weeks_read_write_in_project_order_and_keep_unstated_days() {
        let xml = include_str!("../../corpus/mspdi/28-work-weeks.xml");
        let mut project = read_mspdi(xml).unwrap();
        assert_eq!(project.calendar(1).unwrap().work_weeks.len(), 1);
        let summer = &project.calendar(1).unwrap().work_weeks[0];
        assert_eq!(summer.name.as_deref(), Some("Summer"));
        assert_eq!(summer.from.unwrap().to_mspdi(), "2026-03-09T00:00:00");
        assert_eq!(summer.to.unwrap().to_mspdi(), "2026-03-20T23:59:00");
        assert_eq!(
            summer.week.iter().map(Option::is_some).collect::<Vec<_>>(),
            [false, true, false, false, false, true, true]
        );
        assert_eq!(summer.week[1].as_ref().unwrap().minutes(), 600);
        assert!(!summer.week[5].as_ref().unwrap().working());
        assert_eq!(summer.week[6].as_ref().unwrap().minutes(), 240);
        assert!(project.calendar(2).unwrap().work_weeks.is_empty());
        assert_eq!(
            project.calendar(3).unwrap().work_weeks[0]
                .week
                .iter()
                .map(Option::is_some)
                .collect::<Vec<_>>(),
            [false, false, false, true, true, false, false]
        );

        // An incomplete work week is kept exactly even though it cannot
        // schedule until both dates exist.
        project.calendars[0].work_weeks.push(WorkWeek {
            from: Some(DateTime::from_ymd_hm(2026, 4, 1, 0, 0)),
            week: std::array::from_fn(|_| None),
            ..WorkWeek::default()
        });
        let written = write_mspdi(&project);
        let standard = calendar_block(&written, 1);
        assert!(standard.find("<Exceptions>").unwrap() < standard.find("<WorkWeeks>").unwrap());
        let first = standard
            .split("<WorkWeek>")
            .nth(1)
            .unwrap()
            .split("</WorkWeek>")
            .next()
            .unwrap();
        assert!(first.find("<TimePeriod>").unwrap() < first.find("<Name>").unwrap());
        assert!(first.find("<Name>").unwrap() < first.find("<WeekDays>").unwrap());
        let days: Vec<&str> = first
            .split("<DayType>")
            .skip(1)
            .map(|part| part.split("</DayType>").next().unwrap())
            .collect();
        assert_eq!(days, ["2", "6", "7"]);
        let second = standard
            .split("<WorkWeek>")
            .nth(2)
            .unwrap()
            .split("</WorkWeek>")
            .next()
            .unwrap();
        assert!(second.contains("<FromDate>2026-04-01T00:00:00</FromDate>"));
        assert!(
            !second.contains("<ToDate>")
                && !second.contains("<Name>")
                && !second.contains("<WeekDays>")
        );
        assert!(!calendar_block(&written, 2).contains("<WorkWeeks>"));
        assert_eq!(read_mspdi(&written).unwrap().calendars, project.calendars);
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
    fn external_leaf_on_an_empty_calendar_can_be_read() {
        let mut proj = empty_calendar_project();
        proj.tasks[0].external_task = Some(true);
        proj.tasks[0].calendar_uid = Some(3);
        let back = read_mspdi(&write_mspdi(&proj)).unwrap();
        assert_eq!(back.tasks[0].external_task, Some(true));
        assert!(
            !crate::schedule::schedule(&back)
                .get(back.tasks[0].uid)
                .unwrap()
                .critical
        );
        let mut summary = back;
        summary.tasks[0].summary = true;
        assert!(read_mspdi(&write_mspdi(&summary)).is_ok());
    }

    #[test]
    fn empty_calendars_unused_by_leaves_are_accepted() {
        fn assert_editor_reopens(proj: &Project) {
            let loaded = read_mspdi(&write_mspdi(proj)).unwrap();
            let editor = crate::editor::Editor::new(loaded);
            assert!(read_mspdi(&write_mspdi(editor.project())).is_ok());
            assert!(
                crate::yppx::read_yppx(&crate::yppx::write_yppx(editor.project()).unwrap()).is_ok()
            );
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
            crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap())
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
            ..Baseline::default()
        };
        assert_eq!(proj.tasks[0].baselines, vec![expected.clone()]);
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
    fn task_baseline_work_and_cost_survive_issue_round_trip() {
        let source = include_str!("../../corpus/mspdi/02-link-fs.xml").replacen(
            "    </Task>",
            "      <Baseline><Number>0</Number><Start>2026-03-02T08:00:00</Start><Finish>2026-03-03T17:00:00</Finish><Duration>PT8H0M0S</Duration><Work>PT8H0M0S</Work><Cost>100000</Cost></Baseline>\n    </Task>",
            1,
        );
        let proj = read_mspdi(&source).unwrap();
        let expected = Baseline {
            start: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            finish: Some(DateTime::from_ymd_hm(2026, 3, 3, 17, 0)),
            duration_min: Some(480),
            work_min: Some(480),
            cost: Rate::parse("100000"),
            ..Baseline::default()
        };
        assert_eq!(proj.tasks[0].baseline(0), Some(&expected));
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
            "<Number>0</Number>\n        <Start>2026-03-02T08:00:00</Start>\n        <Finish>2026-03-03T17:00:00</Finish>\n        <Duration>PT8H0M0S</Duration>\n        <Work>PT8H0M0S</Work>\n        <Cost>100000</Cost>"
        );
        assert_eq!(
            read_mspdi(&xml).unwrap().tasks[0].baseline(0),
            Some(&expected)
        );
    }

    #[test]
    fn task_baseline_new_fields_keep_their_slots_and_yppx_values() {
        let proj = project_with_baselines(
            "<Baseline><Number>0</Number><DurationFormat>7</DurationFormat><Work>PT0H0M0S</Work><Cost>0</Cost></Baseline>\
             <Baseline><Number>3</Number><Start>2026-03-02T08:00:00</Start><Finish>2026-03-03T17:00:00</Finish><Duration>PT8H0M0S</Duration><DurationFormat>8</DurationFormat><Work>PT8H0M0S</Work><Cost>+001000.50</Cost></Baseline>",
        );
        assert_eq!(proj.tasks[0].baseline(0).unwrap().duration_format, Some(7));
        assert_eq!(proj.tasks[0].baseline(0).unwrap().work_min, Some(0));
        assert_eq!(
            proj.tasks[0]
                .baseline(0)
                .unwrap()
                .cost
                .as_ref()
                .map(Rate::as_str),
            Some("0")
        );
        assert_eq!(proj.tasks[0].baseline(3).unwrap().duration_format, Some(8));
        assert_eq!(proj.tasks[0].baseline(3).unwrap().work_min, Some(480));
        assert_eq!(
            proj.tasks[0]
                .baseline(3)
                .unwrap()
                .cost
                .as_ref()
                .map(Rate::as_str),
            Some("+001000.50")
        );
        let xml = write_mspdi(&proj);
        let slot_three = xml
            .split("<Baseline>")
            .nth(2)
            .unwrap()
            .split("</Baseline>")
            .next()
            .unwrap();
        assert_eq!(
            slot_three.trim(),
            "<Number>3</Number>\n        <Start>2026-03-02T08:00:00</Start>\n        <Finish>2026-03-03T17:00:00</Finish>\n        <Duration>PT8H0M0S</Duration>\n        <DurationFormat>8</DurationFormat>\n        <Work>PT8H0M0S</Work>\n        <Cost>+001000.50</Cost>"
        );
        assert_eq!(
            read_mspdi(&xml).unwrap().tasks[0].baselines,
            proj.tasks[0].baselines
        );
        let back = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
        assert_eq!(back.tasks[0].baselines, proj.tasks[0].baselines);
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
        let back = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
        assert_eq!(back.tasks[0].baselines, expected);
    }

    #[test]
    fn partial_baselines_emit_only_recorded_fields() {
        for field in [
            "<Start>2026-03-09T08:00:00</Start>",
            "<Finish>2026-03-13T17:00:00</Finish>",
            "<Duration>PT0H0M0S</Duration>",
            "<DurationFormat>0</DurationFormat>",
            "<Work>PT0H0M0S</Work>",
            "<Cost>0</Cost>",
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
    fn invalid_task_baseline_fields_do_not_create_a_slot() {
        for field in [
            "<DurationFormat/>",
            "<DurationFormat>256</DurationFormat>",
            "<Work/>",
            "<Work>invalid</Work>",
            "<Cost/>",
            "<Cost>invalid</Cost>",
        ] {
            let proj =
                project_with_baselines(&format!("<Baseline><Number>3</Number>{field}</Baseline>"));
            assert!(proj.tasks[0].baselines.is_empty(), "{field}");
            let proj = project_with_baselines(&format!(
                "<Baseline><Number>3</Number><Start>2026-03-02T08:00:00</Start>{field}</Baseline>"
            ));
            assert_eq!(
                proj.tasks[0].baseline(3).unwrap().start,
                Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0))
            );
            assert_eq!(proj.tasks[0].baseline(3).unwrap().duration_format, None);
            assert_eq!(proj.tasks[0].baseline(3).unwrap().work_min, None);
            assert_eq!(proj.tasks[0].baseline(3).unwrap().cost, None);
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
        <CreateDate>2026-07-13T23:14:00</CreateDate><Contact>Site lead</Contact>\
        <WBS>1.2</WBS><WBSLevel>Level 2</WBSLevel>\
        <OutlineNumber>9.9</OutlineNumber><OutlineLevel>1</OutlineLevel><Priority>900</Priority>\
        <Duration>PT8H0M0S</Duration><Work>PT16H30M0S</Work><EffortDriven>1</EffortDriven>\
        <Recurring>1</Recurring><OverAllocated>1</OverAllocated><Estimated>1</Estimated>\
        <IsSubproject>1</IsSubproject><IsSubprojectReadOnly>1</IsSubprojectReadOnly>\
        <SubprojectName>Concrete</SubprojectName><DisplayAsSummary>1</DisplayAsSummary>\
        <ExternalTask>1</ExternalTask><Cost>1250.50</Cost>\
        <Deadline>2026-03-20T17:00:00</Deadline><LevelAssignments>0</LevelAssignments>\
        <LevelingCanSplit>0</LevelingCanSplit><LevelingDelay>4800</LevelingDelay>\
        <LevelingDelayFormat>7</LevelingDelayFormat>\
        <PreLeveledStart>2026-03-18T08:00:00</PreLeveledStart>\
        <PreLeveledFinish>2026-03-18T17:00:00</PreLeveledFinish>\
        <Hyperlink>Survey plan</Hyperlink>\
        <HyperlinkAddress>https://example.com/a?x=1&amp;y=2</HyperlinkAddress>\
        <HyperlinkSubAddress>Gantt Chart!1</HyperlinkSubAddress>\
        <IgnoreResourceCalendar>1</IgnoreResourceCalendar><Notes>Check forms</Notes>\
        <HideBar>1</HideBar><Rollup>1</Rollup><EarnedValueMethod>1</EarnedValueMethod>\
        <IsPublished>0</IsPublished><StatusManager>Alice</StatusManager>\
        <CommitmentStart>2026-03-19T08:00:00</CommitmentStart>\
        <CommitmentFinish>2026-03-19T17:00:00</CommitmentFinish>\
        <CommitmentType>2</CommitmentType></Task>";

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
                contact: Some("Site lead".into()),
                wbs: Some("1.2".into()),
                wbs_level: Some("Level 2".into()),
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
                pre_leveled_start: Some(DateTime::from_ymd_hm(2026, 3, 18, 8, 0)),
                pre_leveled_finish: Some(DateTime::from_ymd_hm(2026, 3, 18, 17, 0)),
                hyperlink: Some("Survey plan".into()),
                hyperlink_address: Some("https://example.com/a?x=1&y=2".into()),
                hyperlink_sub_address: Some("Gantt Chart!1".into()),
                ignore_resource_calendar: Some(true),
                notes: Some("Check forms".into()),
                earned_value_method: Some(1),
                recurring: Some(true),
                hide_bar: Some(true),
                rollup: Some(true),
                external_task: Some(true),
                is_subproject: Some(true),
                is_subproject_read_only: Some(true),
                subproject_name: Some("Concrete".into()),
                display_as_summary: Some(true),
                is_published: Some(false),
                status_manager: Some("Alice".into()),
                commitment_start: Some(DateTime::from_ymd_hm(2026, 3, 19, 8, 0)),
                commitment_finish: Some(DateTime::from_ymd_hm(2026, 3, 19, 17, 0)),
                commitment_type: Some(2),
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
            "<Contact>Site lead</Contact>",
            "<WBS>1.2</WBS>",
            "<WBSLevel>Level 2</WBSLevel>",
            "<Priority>900</Priority>",
            "<Work>PT16H30M0S</Work>",
            "<EffortDriven>1</EffortDriven>",
            "<Recurring>1</Recurring>",
            "<OverAllocated>1</OverAllocated>",
            "<Estimated>1</Estimated>",
            "<IsSubproject>1</IsSubproject>",
            "<IsSubprojectReadOnly>1</IsSubprojectReadOnly>",
            "<SubprojectName>Concrete</SubprojectName>",
            "<DisplayAsSummary>1</DisplayAsSummary>",
            "<ExternalTask>1</ExternalTask>",
            "<Cost>1250.50</Cost>",
            "<Deadline>2026-03-20T17:00:00</Deadline>",
            "<LevelAssignments>0</LevelAssignments>",
            "<LevelingCanSplit>0</LevelingCanSplit>",
            "<LevelingDelay>4800</LevelingDelay>",
            "<LevelingDelayFormat>7</LevelingDelayFormat>",
            "<PreLeveledStart>2026-03-18T08:00:00</PreLeveledStart>",
            "<PreLeveledFinish>2026-03-18T17:00:00</PreLeveledFinish>",
            "<Hyperlink>Survey plan</Hyperlink>",
            "<HyperlinkAddress>https://example.com/a?x=1&amp;y=2</HyperlinkAddress>",
            "<HyperlinkSubAddress>Gantt Chart!1</HyperlinkSubAddress>",
            "<IgnoreResourceCalendar>1</IgnoreResourceCalendar>",
            "<Notes>Check forms</Notes>",
            "<HideBar>1</HideBar>",
            "<Rollup>1</Rollup>",
            "<EarnedValueMethod>1</EarnedValueMethod>",
            "<IsPublished>0</IsPublished>",
            "<StatusManager>Alice</StatusManager>",
            "<CommitmentStart>2026-03-19T08:00:00</CommitmentStart>",
            "<CommitmentFinish>2026-03-19T17:00:00</CommitmentFinish>",
            "<CommitmentType>2</CommitmentType>",
        ] {
            assert!(xml.contains(element), "missing {element}");
        }
        // A task is not a blank row; the stored OutlineNumber is recomputed.
        assert!(xml.contains("<IsNull>0</IsNull>"));
        assert!(xml.contains("<OutlineNumber>1</OutlineNumber>"));
        assert_eq!(read_mspdi(&xml).unwrap().tasks, proj.tasks);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
        assert_eq!(package.tasks, proj.tasks);
    }

    #[test]
    fn empty_task_hyperlink_elements_are_kept() {
        let proj = task_project(
            "<Task><UID>1</UID><Hyperlink/><HyperlinkAddress></HyperlinkAddress><HyperlinkSubAddress/></Task>",
        );
        let task = &proj.tasks[0];
        assert_eq!(task.hyperlink.as_deref(), Some(""));
        assert_eq!(task.hyperlink_address.as_deref(), Some(""));
        assert_eq!(task.hyperlink_sub_address.as_deref(), Some(""));

        let xml = write_mspdi(&proj);
        for name in ["Hyperlink", "HyperlinkAddress", "HyperlinkSubAddress"] {
            assert!(xml.contains(&format!("<{name}></{name}>")));
        }
        assert_eq!(read_mspdi(&xml).unwrap().tasks, proj.tasks);
    }

    #[test]
    fn empty_task_text_elements_are_kept() {
        let names = [
            "Contact",
            "WBSLevel",
            "SubprojectName",
            "Notes",
            "StatusManager",
        ];
        let source = format!(
            "<Task><UID>1</UID>{}</Task>",
            names
                .iter()
                .map(|name| format!("<{name}/>"))
                .collect::<String>()
        );
        let proj = task_project(&source);
        let task = &proj.tasks[0];
        for value in [
            &task.contact,
            &task.wbs_level,
            &task.subproject_name,
            &task.notes,
            &task.status_manager,
        ] {
            assert_eq!(value.as_deref(), Some(""));
        }
        let xml = write_mspdi(&proj);
        for name in names {
            assert!(xml.contains(&format!("<{name}></{name}>")), "{name}: {xml}");
        }
        assert_eq!(read_mspdi(&xml).unwrap().tasks, proj.tasks);
    }

    #[test]
    fn task_notes_survive_save() {
        let proj = task_project(
            "<Task><UID>1</UID><Notes>Line 1&#13;\nLine 2 &amp; &lt;x&gt;</Notes></Task>",
        );
        let expected = "Line 1\nLine 2 & <x>";
        assert_eq!(proj.tasks[0].notes.as_deref(), Some(expected));
        let xml = write_mspdi(&proj);
        assert!(xml.contains("<Notes>Line 1\nLine 2 &amp; &lt;x&gt;</Notes>"));
        assert_eq!(read_mspdi(&xml).unwrap().tasks, proj.tasks);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
        assert_eq!(package.tasks, proj.tasks);
    }

    /// Issue #531: a note reads with `\n` line breaks whether the file holds
    /// raw CR LF or CR bytes or `&#13;` references, for every owner of Notes.
    #[test]
    fn notes_line_breaks_read_as_lf_for_tasks_resources_and_assignments() {
        for (notes, expected) in [
            ("a\r\nb\rc", "a\nb\nc"),
            ("a&#13;&#10;b", "a\nb"),
            ("a&#13;\nb", "a\nb"),
            ("a&#xD;&#xA;b", "a\nb"),
            ("a&#xd;&#xa;b", "a\nb"),
            ("&#13;&#10;", "\n"),
            ("a\nb", "a\nb"),
        ] {
            let task = task_project(&format!("<Task><UID>1</UID><Notes>{notes}</Notes></Task>"));
            assert_eq!(
                task.tasks[0].notes.as_deref(),
                Some(expected),
                "task {notes:?}"
            );
            let resource = resource_project(&format!(
                "<Resource><UID>1</UID><ID>1</ID><Name>R</Name><Notes>{notes}</Notes></Resource>"
            ));
            let resource = resource.resources[0].notes.as_deref();
            assert_eq!(resource, Some(expected), "resource {notes:?}");
            let assignment = assignment_project(&format!(
                "<Assignment><UID>1</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID>\
                 <Notes>{notes}</Notes></Assignment>"
            ));
            let assignment = assignment.assignments[0].notes.as_deref();
            assert_eq!(assignment, Some(expected), "assignment {notes:?}");
        }
    }

    /// The writer still escapes a CR it is given (a conformant reader would
    /// otherwise drop it), and reading that back gives the model's `\n`.
    #[test]
    fn written_cr_in_a_note_is_escaped_and_reads_back_as_lf() {
        let mut proj = task_project("<Task><UID>1</UID><Notes>x</Notes></Task>");
        proj.tasks[0].notes = Some("Line 1\r\nLine 2\rLine 3".into());
        let xml = write_mspdi(&proj);
        assert!(
            xml.contains("<Notes>Line 1&#13;\nLine 2&#13;Line 3</Notes>"),
            "{xml}"
        );
        let back = read_mspdi(&xml).unwrap();
        assert_eq!(
            back.tasks[0].notes.as_deref(),
            Some("Line 1\nLine 2\nLine 3")
        );
    }

    /// Issue #386: the single task replaced by a summary with nothing under it.
    fn childless_summary_file() -> String {
        let source = include_str!("../../corpus/mspdi/01-single-task.xml");
        let xml = source
            .replace("<Name>Dig foundation</Name>", "<Name>Phase</Name>")
            .replace("<Summary>0</Summary>", "<Summary>1</Summary>")
            .replace(
                "<Duration>PT16H0M0S</Duration>",
                "<Duration>PT0H0M0S</Duration>",
            )
            .replace(
                "<Finish>2026-03-03T17:00:00</Finish>",
                "<Finish>2026-03-02T08:00:00</Finish>",
            );
        assert_eq!(xml.matches("Phase").count(), 1);
        assert!(xml.contains("<Summary>1</Summary>"));
        assert!(xml.contains("<Duration>PT0H0M0S</Duration>"));
        xml
    }

    #[test]
    fn childless_summary_is_scheduled_listed_and_saved_with_its_slack() {
        let proj = read_mspdi(&childless_summary_file()).unwrap();
        let task = &proj.tasks[0];
        assert!(task.summary);
        let sched = crate::schedule::schedule(&proj);
        let at = DateTime::from_ymd_hm(2026, 3, 2, 8, 0);
        let r = sched.get(1).unwrap();
        assert_eq!(
            (r.early_start, r.early_finish, r.late_start, r.late_finish),
            (at, at, at, at)
        );
        assert_eq!(
            crate::schedule::task_duration_min(&proj, &sched, task),
            Some(0)
        );

        let md = crate::gantt::to_markdown(&proj, &sched);
        assert!(
            md.contains(
                "| **Phase** | 2026-03-02 08:00:00 | 2026-03-02 08:00:00 |  | 0d | 0d | 0d | ✓ |"
            ),
            "{md}"
        );

        let saved = write_mspdi(&proj);
        let task_xml = saved.split("<Task>").nth(1).unwrap();
        for element in [
            "<Start>2026-03-02T08:00:00</Start>",
            "<Finish>2026-03-02T08:00:00</Finish>",
            "<Duration>PT0H0M0S</Duration>",
            "<Critical>1</Critical>",
            "<EarlyStart>2026-03-02T08:00:00</EarlyStart>",
            "<EarlyFinish>2026-03-02T08:00:00</EarlyFinish>",
            "<LateStart>2026-03-02T08:00:00</LateStart>",
            "<LateFinish>2026-03-02T08:00:00</LateFinish>",
            "<FreeSlack>0</FreeSlack>",
            "<TotalSlack>0</TotalSlack>",
            "<StartSlack>0</StartSlack>",
            "<FinishSlack>0</FinishSlack>",
        ] {
            assert!(task_xml.contains(element), "missing {element}:\n{task_xml}");
        }
    }

    #[test]
    fn task_hyperlink_survives_save() {
        let source = include_str!("../../corpus/mspdi/01-single-task.xml");
        let xml = source.replace(
            "<Name>Dig foundation</Name>",
            "<Name>Dig foundation</Name>\n      <Hyperlink>Site survey</Hyperlink>\n      <HyperlinkAddress>https://example.com/survey.pdf</HyperlinkAddress>\n      <HyperlinkSubAddress>Gantt Chart!1</HyperlinkSubAddress>",
        );
        assert_ne!(xml, source);
        let proj = read_mspdi(&xml).unwrap();
        let task = &proj.tasks[0];
        assert_eq!(task.hyperlink.as_deref(), Some("Site survey"));
        assert_eq!(
            task.hyperlink_address.as_deref(),
            Some("https://example.com/survey.pdf")
        );
        assert_eq!(task.hyperlink_sub_address.as_deref(), Some("Gantt Chart!1"));

        let saved = write_mspdi(&proj);
        for element in [
            "<Hyperlink>Site survey</Hyperlink>",
            "<HyperlinkAddress>https://example.com/survey.pdf</HyperlinkAddress>",
            "<HyperlinkSubAddress>Gantt Chart!1</HyperlinkSubAddress>",
        ] {
            assert!(saved.contains(element), "missing {element}");
        }
        let back = read_mspdi(&saved).unwrap();
        assert_eq!(back.tasks, proj.tasks);
        let result = crate::schedule::schedule(&back);
        let task = result.get(1).unwrap();
        assert_eq!(task.early_start, DateTime::from_ymd_hm(2026, 3, 2, 8, 0));
        assert_eq!(task.early_finish, DateTime::from_ymd_hm(2026, 3, 3, 17, 0));
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
    const NEW_TASK_ELEMENTS: [&str; 41] = [
        "GUID",
        "Active",
        "Type",
        "CreateDate",
        "Contact",
        "WBS",
        "WBSLevel",
        "Priority",
        "Work",
        "EffortDriven",
        "Recurring",
        "OverAllocated",
        "Estimated",
        "IsSubproject",
        "IsSubprojectReadOnly",
        "SubprojectName",
        "DisplayAsSummary",
        "ExternalTask",
        "FixedCost",
        "FixedCostAccrual",
        "Cost",
        "Deadline",
        "LevelAssignments",
        "LevelingCanSplit",
        "LevelingDelay",
        "LevelingDelayFormat",
        "PreLeveledStart",
        "PreLeveledFinish",
        "Hyperlink",
        "HyperlinkAddress",
        "HyperlinkSubAddress",
        "IgnoreResourceCalendar",
        "Notes",
        "HideBar",
        "Rollup",
        "EarnedValueMethod",
        "IsPublished",
        "StatusManager",
        "CommitmentStart",
        "CommitmentFinish",
        "CommitmentType",
    ];

    /// The `<Tasks>` section of a written file.
    fn task_xml(xml: &str) -> &str {
        &xml[xml.find("<Tasks>").unwrap()..xml.find("</Tasks>").unwrap()]
    }

    /// The written `<Task>` element of task `uid`.
    fn one_task_xml(xml: &str, uid: i32) -> &str {
        let at = xml.find(&format!("<UID>{uid}</UID>")).unwrap();
        &xml[at..at + xml[at..].find("</Task>").unwrap()]
    }

    /// Issue #343: an auto task read with stale Start/Finish saves the dates
    /// it is scheduled to, the same as its EarlyStart/EarlyFinish.
    #[test]
    fn auto_task_saves_its_scheduled_dates_not_the_stale_stored_ones() {
        let source = include_str!("../../corpus/mspdi/02-link-fs.xml").replacen(
            "<Start>2026-03-04T08:00:00</Start><Finish>2026-03-05T17:00:00</Finish>",
            "<Start>2026-03-11T08:00:00</Start><Finish>2026-03-12T17:00:00</Finish>",
            1,
        );
        let proj = read_mspdi(&source).unwrap();
        assert_eq!(
            proj.task(2).unwrap().stored_start,
            Some(DateTime::from_ymd_hm(2026, 3, 11, 8, 0))
        );
        let xml = write_mspdi(&proj);
        let b = one_task_xml(&xml, 2);
        assert!(b.contains("<Manual>0</Manual>"), "{b}");
        for tag in [
            "<Start>2026-03-04T08:00:00</Start>",
            "<Finish>2026-03-05T17:00:00</Finish>",
            "<EarlyStart>2026-03-04T08:00:00</EarlyStart>",
            "<EarlyFinish>2026-03-05T17:00:00</EarlyFinish>",
        ] {
            assert!(b.contains(tag), "{tag} in {b}");
        }
    }

    /// A manual task saves the dates it stores even where the schedule puts
    /// it elsewhere: Project does not reschedule it.
    #[test]
    fn manual_task_keeps_its_stored_dates() {
        let mut proj = task_project("<Task><UID>1</UID><Duration>PT8H0M0S</Duration></Task>");
        let t = &mut proj.tasks[0];
        t.manual = true;
        t.manual_start = Some(DateTime::from_ymd_hm(2026, 3, 11, 8, 0));
        t.stored_start = Some(DateTime::from_ymd_hm(2026, 3, 9, 8, 0));
        t.stored_finish = Some(DateTime::from_ymd_hm(2026, 3, 9, 17, 0));
        let early = crate::schedule::schedule(&proj).get(1).unwrap().early_start;
        assert_eq!(early, DateTime::from_ymd_hm(2026, 3, 11, 8, 0));
        let xml = write_mspdi(&proj);
        let t = one_task_xml(&xml, 1);
        assert!(t.contains("<Start>2026-03-09T08:00:00</Start>"), "{t}");
        assert!(t.contains("<Finish>2026-03-09T17:00:00</Finish>"), "{t}");
    }

    /// A manual task with no start (TBD) gains no Start on save, which would
    /// pin it there on reload.
    #[test]
    fn tbd_manual_task_stays_tbd_through_a_save() {
        let mut proj = task_project("<Task><UID>1</UID><Duration>PT8H0M0S</Duration></Task>");
        proj.tasks[0].manual = true;
        assert!(proj.tasks[0].pinned_dates().is_none());
        assert!(crate::schedule::schedule(&proj).get(1).is_some());
        let xml = write_mspdi(&proj);
        assert!(!one_task_xml(&xml, 1).contains("<Start>"), "{xml}");
        let back = read_mspdi(&xml).unwrap();
        assert!(back.tasks[0].manual && back.tasks[0].pinned_dates().is_none());
    }

    /// An auto summary with nothing scheduled below it spans its own stored
    /// dates, which the schedule clamps (a Finish before the Start becomes
    /// the Start): it saves them as read. A blank row keeps what it stores.
    #[test]
    fn empty_auto_summary_and_blank_row_keep_their_stored_dates() {
        let mut proj = task_project(
            "<Task><UID>1</UID><OutlineLevel>1</OutlineLevel><Summary>1</Summary>             <Start>2026-03-10T08:00:00</Start><Finish>2026-03-05T17:00:00</Finish></Task>             <Task><UID>2</UID><OutlineLevel>2</OutlineLevel><IsNull>1</IsNull></Task>",
        );
        proj.tasks[1].stored_start = Some(DateTime::from_ymd_hm(2026, 3, 11, 8, 0));
        let sched = crate::schedule::schedule(&proj);
        assert!(proj.tasks[0].summary && proj.tasks[1].is_null);
        assert!(sched.rolled_up(1).is_none());
        let r = sched.get(1).unwrap();
        assert_eq!(r.early_finish, DateTime::from_ymd_hm(2026, 3, 10, 8, 0));
        let xml = write_mspdi(&proj);
        let summary = one_task_xml(&xml, 1);
        assert!(
            summary.contains("<Start>2026-03-10T08:00:00</Start>"),
            "{summary}"
        );
        assert!(
            summary.contains("<Finish>2026-03-05T17:00:00</Finish>"),
            "{summary}"
        );
        assert!(
            one_task_xml(&xml, 2).contains("<Start>2026-03-11T08:00:00</Start>"),
            "{xml}"
        );
    }

    /// A resource calendar with Mondays off, unlike Standard.
    const ALICE_OFF_MONDAYS: &str =
        "<WeekDays><WeekDay><DayType>2</DayType><DayWorking>0</DayWorking></WeekDay></WeekDays>";

    /// An input Project schedules by and docxy's schedule ignores moves its
    /// task and, through links, others: a plan with one saves every task's
    /// dates as Project wrote them. A runs 3/3 and B, FS after it, 3/4, where
    /// the schedule has them at 3/2 and 3/3. Without such an input the same
    /// plan saves the schedule's dates.
    #[test]
    fn a_plan_with_an_input_the_schedule_ignores_keeps_its_stored_dates() {
        // (each task's extra elements, B's assignment's extra elements, Alice's
        // calendar's own elements, whether the plan keeps its dates)
        let cases = [
            ("", "", "", false),
            ("<LevelingDelay>4800</LevelingDelay>", "", "", true),
            ("", "<LevelingDelay>4800</LevelingDelay>", "", true),
            ("", "<Delay>4800</Delay>", "", true),
            (
                "<LevelingDelay>0</LevelingDelay>",
                "<Delay>0</Delay>",
                "",
                false,
            ),
            ("<DurationFormat>8</DurationFormat>", "", "", true),
            ("<DurationFormat>7</DurationFormat>", "", "", false),
            ("<DurationFormat>39</DurationFormat>", "", "", false),
            ("<DurationFormat>40</DurationFormat>", "", "", true),
            ("", "", ALICE_OFF_MONDAYS, true),
            (
                "<IgnoreResourceCalendar>1</IgnoreResourceCalendar>",
                "",
                ALICE_OFF_MONDAYS,
                false,
            ),
            // Project writes -1 for a task with no calendar of its own: like a
            // UID naming no calendar, it is the project's, which Alice's
            // derives from unchanged.
            ("<CalendarUID>-1</CalendarUID>", "", "", false),
            ("<CalendarUID>99</CalendarUID>", "", "", false),
            ("<CalendarUID>-1</CalendarUID>", "", ALICE_OFF_MONDAYS, true),
        ];
        let standard = include_str!("../../corpus/mspdi/02-link-fs.xml");
        let standard = &standard[standard.find("<Calendar>").unwrap()
            ..standard.find("</Calendar>").unwrap() + "</Calendar>".len()];
        // A resource CalendarUID naming no calendar (-1, or a missing UID)
        // gives it none to differ by, whatever Alice's calendar states.
        let cases = cases
            .into_iter()
            .map(|case| (case, "2"))
            .chain(["-1", "99"].map(|alice_uid| (("", "", ALICE_OFF_MONDAYS, false), alice_uid)));
        for ((task_extra, assignment_extra, alice_calendar, kept), alice_uid) in cases {
            let xml = format!(
                "<Project><StartDate>2026-03-02T08:00:00</StartDate><CalendarUID>1</CalendarUID>
                <Tasks>
                <Task><UID>1</UID><Name>A</Name><OutlineLevel>1</OutlineLevel>
                  <Duration>PT8H0M0S</Duration>{task_extra}
                  <Start>2026-03-03T08:00:00</Start><Finish>2026-03-03T17:00:00</Finish></Task>
                <Task><UID>2</UID><Name>B</Name><OutlineLevel>1</OutlineLevel>
                  <Duration>PT8H0M0S</Duration>{task_extra}
                  <Start>2026-03-04T08:00:00</Start><Finish>2026-03-04T17:00:00</Finish>
                  <PredecessorLink><PredecessorUID>1</PredecessorUID><Type>1</Type>
                  </PredecessorLink></Task>
                </Tasks>
                <Resources><Resource><UID>1</UID><Name>Alice</Name><Type>1</Type>
                  <CalendarUID>{alice_uid}</CalendarUID></Resource></Resources>
                <Assignments>
                <Assignment><UID>1</UID><TaskUID>2</TaskUID>
                  <ResourceUID>1</ResourceUID>{assignment_extra}</Assignment>
                <Assignment><UID>2</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID>
                </Assignment></Assignments>
                <Calendars>{standard}
                <Calendar><UID>2</UID><Name>Alice</Name><IsBaseCalendar>0</IsBaseCalendar>
                  <BaseCalendarUID>1</BaseCalendarUID>{alice_calendar}</Calendar></Calendars>
                </Project>"
            );
            let proj = read_mspdi(&xml).unwrap();
            let sched = crate::schedule::schedule(&proj);
            let early = |uid| sched.get(uid).unwrap().early_start.to_mspdi();
            assert_eq!(
                (early(1), early(2)),
                ("2026-03-02T08:00:00".into(), "2026-03-03T08:00:00".into()),
                "{task_extra} {assignment_extra} {alice_calendar} {alice_uid}"
            );
            let saved = write_mspdi(&proj);
            let start = |uid| {
                let task = one_task_xml(&saved, uid);
                let at = task.find("<Start>").unwrap() + "<Start>".len();
                task[at..at + 10].to_string()
            };
            let expected = if kept {
                ("2026-03-03", "2026-03-04")
            } else {
                ("2026-03-02", "2026-03-03")
            };
            assert_eq!(
                (start(1).as_str(), start(2).as_str()),
                expected,
                "{task_extra} {assignment_extra} {alice_calendar} {alice_uid}"
            );
        }
    }

    /// docxy schedules forward even when a plan is scheduled from its
    /// finish, so such a plan saves every task's dates as Project wrote them.
    #[test]
    fn a_plan_scheduled_from_its_finish_keeps_its_stored_dates() {
        let source = include_str!("../../corpus/mspdi/02-link-fs.xml")
            .replacen(
                "<Start>2026-03-04T08:00:00</Start><Finish>2026-03-05T17:00:00</Finish>",
                "<Start>2026-03-11T08:00:00</Start><Finish>2026-03-12T17:00:00</Finish>",
                1,
            )
            .replacen(
                "<Name>link-fs</Name>",
                "<Name>link-fs</Name><ScheduleFromStart>0</ScheduleFromStart>",
                1,
            );
        let proj = read_mspdi(&source).unwrap();
        assert_eq!(proj.option("ScheduleFromStart"), Some("0"));
        let xml = write_mspdi(&proj);
        let b = one_task_xml(&xml, 2);
        assert!(b.contains("<Start>2026-03-11T08:00:00</Start>"), "{b}");
        assert!(
            b.contains("<EarlyStart>2026-03-04T08:00:00</EarlyStart>"),
            "{b}"
        );
    }

    #[test]
    fn task_duration_format_is_read_and_written_back() {
        let proj = read_mspdi(DURATION_FORMATS_PLAN).unwrap();
        let formats = |proj: &Project| {
            proj.tasks
                .iter()
                .map(|t| (t.name.clone(), t.duration_format))
                .collect::<Vec<_>>()
        };
        // Days, the default, is no format.
        let expected = [
            ("S", None),
            ("Half", Some(5)),
            ("Full", None),
            ("Merge", None),
            ("WkShort", Some(9)),
            ("WkLong", Some(9)),
            ("End", None),
        ]
        .map(|(name, format)| (name.to_string(), format));
        assert_eq!(formats(&proj), expected);
        let xml = write_mspdi(&proj);
        // Each task's own format, in task order.
        let written: Vec<&str> = task_xml(&xml)
            .split("<Task>")
            .skip(1)
            .map(|task| {
                let from = task.find("<DurationFormat>").unwrap() + "<DurationFormat>".len();
                &task[from..from + task[from..].find('<').unwrap()]
            })
            .collect();
        assert_eq!(written, ["7", "5", "7", "7", "9", "9", "7"]);
        assert_eq!(formats(&read_mspdi(&xml).unwrap()), expected);
    }

    #[test]
    fn task_duration_format_keeps_codes_and_drops_invalid_ones() {
        for (text, read, written) in [
            ("9", Some(9), "9"),
            ("41", Some(41), "41"),
            ("39", Some(39), "39"),
            ("8", Some(8), "8"),
            ("53", Some(53), "53"),
            ("7", None, "7"),
            ("", None, "7"),
            ("x", None, "7"),
            ("256", None, "7"),
            ("-1", None, "7"),
        ] {
            let proj = task_project(&format!(
                "<Task><UID>1</UID><ID>1</ID><Name>A</Name><Duration>PT8H0M0S</Duration>\
                 <DurationFormat>{text}</DurationFormat></Task>"
            ));
            assert_eq!(proj.tasks[0].duration_format, read, "{text:?}");
            assert!(
                task_xml(&write_mspdi(&proj))
                    .contains(&format!("<DurationFormat>{written}</DurationFormat>")),
                "{text:?}"
            );
        }
        // A task without one still saves days; a blank row saves none.
        let proj = task_project(
            "<Task><UID>1</UID><ID>1</ID><Name>A</Name><Duration>PT8H0M0S</Duration></Task>\
             <Task><UID>2</UID><ID>2</ID><IsNull>1</IsNull></Task>",
        );
        let xml = write_mspdi(&proj);
        assert_eq!(
            task_xml(&xml)
                .matches("<DurationFormat>7</DurationFormat>")
                .count(),
            1
        );
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
    fn fixed_cost_and_accrual_round_trip_for_tasks_and_summaries() {
        let proj = task_project(
            "<Task><UID>0</UID><ID>0</ID><OutlineLevel>0</OutlineLevel><Summary>1</Summary><FixedCost>1200</FixedCost><FixedCostAccrual>1</FixedCostAccrual><Cost>51200</Cost><RemainingCost>51200</RemainingCost></Task>\
             <Task><UID>2</UID><ID>1</ID><OutlineLevel>1</OutlineLevel><Summary>1</Summary><FixedCost>300</FixedCost><FixedCostAccrual>2</FixedCostAccrual><Cost>50300</Cost><RemainingCost>50300</RemainingCost></Task>\
             <Task><UID>1</UID><ID>2</ID><OutlineLevel>2</OutlineLevel><FixedCost>50000</FixedCost><FixedCostAccrual>3</FixedCostAccrual><Cost>50000</Cost><RemainingCost>50000</RemainingCost></Task>",
        );
        for (task, cost, accrual, total) in [
            (&proj.tasks[0], "1200", AccrueAt::Start, "51200"),
            (&proj.tasks[1], "300", AccrueAt::End, "50300"),
            (&proj.tasks[2], "50000", AccrueAt::Prorated, "50000"),
        ] {
            assert_eq!(task.fixed_cost.as_ref().map(Rate::as_str), Some(cost));
            assert_eq!(task.fixed_cost_accrual, Some(accrual));
            assert_eq!(task.cost.as_ref().map(Rate::as_str), Some(total));
            assert_eq!(task.remaining_cost.as_ref().map(Rate::as_str), Some(total));
        }
        let xml = write_mspdi(&proj);
        let back = read_mspdi(&xml).unwrap();
        for (before, after) in proj.tasks.iter().zip(&back.tasks) {
            assert_eq!(after.fixed_cost, before.fixed_cost);
            assert_eq!(after.fixed_cost_accrual, before.fixed_cost_accrual);
            assert_eq!(after.cost, before.cost);
            assert_eq!(after.remaining_cost, before.remaining_cost);
        }
        assert_eq!(xml.matches("<FixedCost>").count(), 3);
        assert_eq!(xml.matches("<FixedCostAccrual>").count(), 3);
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
            "<DisplayAsSummary>maybe</DisplayAsSummary>",
            "<IsPublished>2</IsPublished>",
            "<PreLeveledStart>soon</PreLeveledStart>",
            "<PreLeveledFinish>soon</PreLeveledFinish>",
            "<CommitmentStart>soon</CommitmentStart>",
            "<CommitmentFinish>soon</CommitmentFinish>",
            "<CommitmentType>3</CommitmentType>",
            "<CommitmentType>x</CommitmentType>",
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
        t.external_task = Some(false);
        t.stored_start = Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0));
        t.stored_finish = Some(DateTime::from_ymd_hm(2026, 3, 2, 17, 0));
        t.manual_start = t.stored_start;
        t.manual_finish = t.stored_finish;
        t.manual_duration_min = Some(480);
        t.fixed_cost = Rate::parse("50000");
        t.fixed_cost_accrual = Some(AccrueAt::Prorated);
        t.calendar_uid = Some(1);
        t.constraint = ConstraintType::StartNoEarlierThan;
        t.constraint_date = t.stored_start;
        t.set_baseline_slot(Baseline {
            number: 0,
            duration_min: Some(480),
            ..Baseline::default()
        });
        t.extended_attributes = vec![ExtendedAttributeValue {
            field_id: "188743731".into(),
            value: Some("A".into()),
            value_guid: Some("C8A6D07D-4E0D-4F63-9A8B-0D1E2F3A4B5C".into()),
            duration_format: Some(7),
        }];
        t.outline_codes = vec![OutlineCodeValue {
            field_id: "188744105".into(),
            value_id: Some("1".into()),
            ..OutlineCodeValue::default()
        }];
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
        t.resume_valid = Some(false);
        t.overtime_cost = Rate::parse("12.25");
        t.overtime_work_min = Some(0);
        t.actual_overtime_cost = Rate::parse("2.5");
        t.actual_overtime_work_min = Some(30);
        t.regular_work_min = Some(450);
        t.remaining_overtime_cost = Rate::parse("9.75");
        t.remaining_overtime_work_min = Some(15);
        t.acwp = Rate::parse("101.25");
        t.cv = Rate::parse("-3.5");
        t.bcws = Rate::parse("105");
        t.bcwp = Rate::parse("97.75");
        t.actual_work_protected_min = Some(60);
        t.actual_overtime_work_protected_min = Some(0);
        t.timephased_data.push(TimephasedValue {
            kind: 2,
            uid: Some(1),
            value: Some("PT1H0M0S".into()),
            ..TimephasedValue::default()
        });
        proj.tasks.insert(
            0,
            Task {
                uid: 2,
                outline_level: 1,
                duration_min: 480,
                ..Task::default()
            },
        );
        proj.tasks[1].predecessors.push(Predecessor::fs(2));
        proj.tasks[1].predecessors[0].cross_project = Some(true);
        proj.tasks[1].predecessors[0].cross_project_name = Some(r"C:\plans\other.mpp\7".into());
        let mut xml = String::new();
        let sched = crate::schedule::schedule(&proj);
        write_task(
            &mut xml,
            &proj.tasks[1],
            &Computed {
                outline_number: Some("2"),
                result: sched.get(1),
                dates: None,
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
                "Contact",
                "WBS",
                "WBSLevel",
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
                "ResumeValid",
                "EffortDriven",
                "Recurring",
                "OverAllocated",
                "Estimated",
                "Milestone",
                "Summary",
                "DisplayAsSummary",
                "Critical",
                "IsSubproject",
                "IsSubprojectReadOnly",
                "SubprojectName",
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
                "FixedCost",
                "FixedCostAccrual",
                "PercentComplete",
                "PercentWorkComplete",
                "Cost",
                "OvertimeCost",
                "OvertimeWork",
                "ActualStart",
                "ActualFinish",
                "ActualDuration",
                "ActualCost",
                "ActualOvertimeCost",
                "ActualWork",
                "ActualOvertimeWork",
                "RegularWork",
                "RemainingDuration",
                "RemainingCost",
                "RemainingWork",
                "RemainingOvertimeCost",
                "RemainingOvertimeWork",
                "ACWP",
                "CV",
                "ConstraintType",
                "CalendarUID",
                "ConstraintDate",
                "Deadline",
                "LevelAssignments",
                "LevelingCanSplit",
                "LevelingDelay",
                "LevelingDelayFormat",
                "PreLeveledStart",
                "PreLeveledFinish",
                "Hyperlink",
                "HyperlinkAddress",
                "HyperlinkSubAddress",
                "IgnoreResourceCalendar",
                "Notes",
                "HideBar",
                "Rollup",
                "BCWS",
                "BCWP",
                "PhysicalPercentComplete",
                "EarnedValueMethod",
                "PredecessorLink",
                "PredecessorUID",
                "Type",
                "CrossProject",
                "CrossProjectName",
                "LinkLag",
                "LagFormat",
                "ActualWorkProtected",
                "ActualOvertimeWorkProtected",
                "ExtendedAttribute",
                "FieldID",
                "Value",
                "ValueGUID",
                "DurationFormat",
                "Baseline",
                "Number",
                "Duration",
                "OutlineCode",
                "FieldID",
                "ValueID",
                "IsPublished",
                "StatusManager",
                "CommitmentStart",
                "CommitmentFinish",
                "CommitmentType",
                "TimephasedData",
                "Type",
                "UID",
                "Value",
            ]
        );
        proj.tasks[1].external_task = Some(true);
        proj.tasks[1].external_task_project = Some(r"C:\plans\other.mpp".into());
        let mut external_xml = String::new();
        write_task(
            &mut external_xml,
            &proj.tasks[1],
            &Computed {
                outline_number: Some("2"),
                result: sched.get(1),
                dates: None,
            },
        );
        let external_flag = external_xml.find("<ExternalTask>1</ExternalTask>").unwrap();
        let external_path = external_xml.find("<ExternalTaskProject>").unwrap();
        let early = external_xml.find("<StartVariance>").unwrap();
        assert!(external_flag < external_path && external_path < early);
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
                dates: None,
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

    #[test]
    fn task_tracking_fields_keep_values_and_timephased_order() {
        let source = "<Task><UID>1</UID><ID>1</ID><Name>Tracked</Name>\
            <ResumeValid>0</ResumeValid><OvertimeCost>001.250</OvertimeCost>\
            <OvertimeWork>PT0H0M0S</OvertimeWork>\
            <ActualOvertimeCost>-2.50</ActualOvertimeCost>\
            <ActualOvertimeWork>PT1H0M0S</ActualOvertimeWork>\
            <RegularWork>PT7H0M0S</RegularWork>\
            <RemainingOvertimeCost>0</RemainingOvertimeCost>\
            <RemainingOvertimeWork>PT0H30M0S</RemainingOvertimeWork>\
            <ACWP>3.125</ACWP><CV>-0.5</CV><BCWS>007</BCWS><BCWP>8.</BCWP>\
            <ActualWorkProtected>PT1H0M0S</ActualWorkProtected>\
            <ActualOvertimeWorkProtected>PT0H0M0S</ActualOvertimeWorkProtected>\
            <TimephasedData><Type>2</Type><UID>1</UID><Value>PT1H0M0S</Value></TimephasedData>\
            <TimephasedData><Type>99</Type><UID>1</UID><Value>4.25</Value></TimephasedData>\
            </Task>";
        let proj = task_project(source);
        let task = &proj.tasks[0];
        assert_eq!(task.resume_valid, Some(false));
        assert_eq!(task.overtime_work_min, Some(0));
        assert_eq!(task.actual_overtime_work_min, Some(60));
        assert_eq!(task.regular_work_min, Some(420));
        assert_eq!(task.remaining_overtime_work_min, Some(30));
        assert_eq!(task.actual_work_protected_min, Some(60));
        assert_eq!(task.actual_overtime_work_protected_min, Some(0));
        for (value, expected) in [
            (&task.overtime_cost, "001.250"),
            (&task.actual_overtime_cost, "-2.50"),
            (&task.remaining_overtime_cost, "0"),
            (&task.acwp, "3.125"),
            (&task.cv, "-0.5"),
            (&task.bcws, "007"),
            (&task.bcwp, "8."),
        ] {
            assert_eq!(value.as_ref().map(Rate::as_str), Some(expected));
        }
        assert_eq!(
            task.timephased_data
                .iter()
                .map(|v| v.kind)
                .collect::<Vec<_>>(),
            [2, 99]
        );
        let saved = write_mspdi(&proj);
        assert_eq!(read_mspdi(&saved).unwrap().tasks[0], *task);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
        assert_eq!(package.tasks[0], *task);

        let absent = task_project(
            "<Task><UID>1</UID><ID>1</ID><Name>Invalid</Name>\
             <ResumeValid>maybe</ResumeValid><OvertimeWork>8h</OvertimeWork>\
             <ActualOvertimeCost>free</ActualOvertimeCost></Task>",
        );
        let saved = write_mspdi(&absent);
        for name in [
            "ResumeValid",
            "OvertimeWork",
            "ActualOvertimeCost",
            "BCWS",
            "TimephasedData",
        ] {
            assert!(!task_xml(&saved).contains(&format!("<{name}>")), "{name}");
        }
    }

    #[test]
    fn task_overtime_work_rounds_to_whole_minutes() {
        let proj = task_project(
            "<Task><UID>1</UID><ID>1</ID><Name>A</Name>\
             <ActualOvertimeWork>PT1M30S</ActualOvertimeWork></Task>",
        );
        assert_eq!(proj.tasks[0].actual_overtime_work_min, Some(2));
        assert!(
            task_xml(&write_mspdi(&proj))
                .contains("<ActualOvertimeWork>PT0H2M0S</ActualOvertimeWork>")
        );
    }

    #[test]
    fn resource_and_assignment_tracking_fields_still_survive_save() {
        let xml = "<Project><Tasks><Task><UID>1</UID><ID>1</ID><Name>T</Name>\
            <Duration>PT8H0M0S</Duration></Task></Tasks><Resources>\
            <Resource><UID>1</UID><ID>1</ID><Name>R</Name><Type>1</Type>\
            <ActualOvertimeWork>PT1H0M0S</ActualOvertimeWork>\
            <ActualOvertimeCost>2.25</ActualOvertimeCost>\
            <RemainingOvertimeWork>PT2H0M0S</RemainingOvertimeWork>\
            <RemainingOvertimeCost>3.25</RemainingOvertimeCost>\
            <ACWP>4.25</ACWP><BCWS>5.25</BCWS><BCWP>6.25</BCWP><CV>7.25</CV>\
            <TimephasedData><Type>2</Type><UID>1</UID><Value>PT1H0M0S</Value></TimephasedData>\
            </Resource></Resources><Assignments>\
            <Assignment><UID>1</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID>\
            <ActualOvertimeWork>PT3H0M0S</ActualOvertimeWork>\
            <ActualOvertimeCost>12.25</ActualOvertimeCost>\
            <RemainingOvertimeWork>PT4H0M0S</RemainingOvertimeWork>\
            <RemainingOvertimeCost>13.25</RemainingOvertimeCost>\
            <ACWP>14.25</ACWP><BCWS>15.25</BCWS><BCWP>16.25</BCWP><CV>17.25</CV>\
            <TimephasedData><Type>2</Type><UID>1</UID><Value>PT3H0M0S</Value></TimephasedData>\
            </Assignment></Assignments></Project>";
        let project = read_mspdi(xml).unwrap();
        let saved = read_mspdi(&write_mspdi(&project)).unwrap();
        assert_eq!(saved.resources, project.resources);
        assert_eq!(saved.assignments, project.assignments);
        assert_eq!(saved.resources[0].timephased_data[0].kind, 2);
        assert_eq!(saved.assignments[0].timephased_data[0].kind, 2);
        assert_eq!(
            saved.resources[0]
                .actual_overtime_cost
                .as_ref()
                .map(Rate::as_str),
            Some("2.25")
        );
        assert_eq!(
            saved.assignments[0].bcwp.as_ref().map(Rate::as_str),
            Some("16.25")
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
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
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
                    "ProjectExternallyEdited" => "0".to_string(),
                    // Modeled options (#187), each off its default.
                    "NewTasksEffortDriven" => "1".to_string(),
                    "NewTasksEstimated" => "0".to_string(),
                    "DefaultTaskType" => "2".to_string(),
                    "Autolink" => "0".to_string(),
                    "CriticalSlackLimit" => "3".to_string(),
                    "MultipleCriticalPaths" => "1".to_string(),
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
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
        assert_eq!(header_of(&write_mspdi(&package)), expected);
    }

    /// The six options #187 models, each with a non-default value.
    const MODELED_OPTIONS: &str = "<NewTasksEffortDriven>1</NewTasksEffortDriven>        <NewTasksEstimated>0</NewTasksEstimated><DefaultTaskType>2</DefaultTaskType>        <Autolink>0</Autolink><CriticalSlackLimit>3</CriticalSlackLimit>        <MultipleCriticalPaths>1</MultipleCriticalPaths>";

    fn modeled_options(proj: &Project) -> ModeledOptions {
        (
            proj.new_tasks_effort_driven,
            proj.new_tasks_estimated,
            proj.default_task_type,
            proj.autolink,
            proj.critical_slack_limit_days,
            proj.multiple_critical_paths,
        )
    }

    type ModeledOptions = (
        Option<bool>,
        Option<bool>,
        Option<TaskType>,
        Option<bool>,
        Option<i64>,
        Option<bool>,
    );

    #[test]
    fn task_default_and_critical_path_options_read_into_typed_fields() {
        let proj = read_mspdi(&format!("<Project>{MODELED_OPTIONS}</Project>")).unwrap();
        let expected = (
            Some(true),
            Some(false),
            Some(TaskType::FixedWork),
            Some(false),
            Some(3),
            Some(true),
        );
        assert_eq!(modeled_options(&proj), expected);
        // A parsed option is kept in its field only.
        assert!(proj.options.is_empty(), "{:?}", proj.options);
        let xml = write_mspdi(&proj);
        assert_eq!(modeled_options(&read_mspdi(&xml).unwrap()), expected);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
        assert_eq!(modeled_options(&package), expected);
        // The effective values follow the file.
        assert!(proj.new_tasks_effort_driven() && !proj.new_tasks_estimated());
        assert_eq!(proj.default_task_type(), TaskType::FixedWork);
        assert!(!proj.autolink() && proj.multiple_critical_paths());
        assert_eq!(proj.critical_slack_limit_min(), 3 * 480);
    }

    #[test]
    fn absent_task_default_options_stay_absent_and_take_projects_defaults() {
        let proj = read_mspdi("<Project><Tasks/></Project>").unwrap();
        assert_eq!(modeled_options(&proj), (None, None, None, None, None, None));
        assert!(!proj.new_tasks_effort_driven() && proj.new_tasks_estimated());
        assert_eq!(proj.default_task_type(), TaskType::FixedUnits);
        assert!(proj.autolink() && !proj.multiple_critical_paths());
        assert_eq!(proj.critical_slack_limit_min(), 0);
        let xml = write_mspdi(&proj);
        for name in [
            "NewTasksEffortDriven",
            "NewTasksEstimated",
            "DefaultTaskType",
            "Autolink",
            "CriticalSlackLimit",
            "MultipleCriticalPaths",
        ] {
            assert!(!xml.contains(name), "{name} written");
        }
    }

    #[test]
    fn a_modeled_option_saves_in_canonical_form_in_schema_position() {
        let proj = read_mspdi("<Project><Autolink>true</Autolink><Tasks/></Project>").unwrap();
        assert_eq!(proj.autolink, Some(true));
        let names: Vec<String> = header_of(&write_mspdi(&proj))
            .into_iter()
            .map(|(n, t)| format!("{n}={t}"))
            .collect();
        // After StatusDate/CurrentDate's slot, before NewTasksAreManual.
        let at = |entry: &str| names.iter().position(|n| n == entry).unwrap();
        assert!(at("Autolink=1") < at("NewTasksAreManual=0"), "{names:?}");
        assert!(at("HonorConstraints=1") < at("Autolink=1"), "{names:?}");
    }

    #[test]
    fn an_unparseable_modeled_option_is_kept_verbatim() {
        let names = [
            "NewTasksEffortDriven",
            "NewTasksEstimated",
            "DefaultTaskType",
            "Autolink",
            "CriticalSlackLimit",
            "MultipleCriticalPaths",
        ];
        let header: String = names
            .iter()
            .map(|n| format!("<{n}>{n}-value</{n}>"))
            .collect();
        let proj = read_mspdi(&format!("<Project>{header}<Tasks/></Project>")).unwrap();
        assert_eq!(modeled_options(&proj), (None, None, None, None, None, None));
        let xml = write_mspdi(&proj);
        for n in names {
            assert!(xml.contains(&format!("<{n}>{n}-value</{n}>")), "{n}: {xml}");
        }
        // Out of range is unparseable too.
        let proj = read_mspdi("<Project><DefaultTaskType>7</DefaultTaskType></Project>").unwrap();
        assert_eq!(proj.default_task_type, None);
        assert!(write_mspdi(&proj).contains("<DefaultTaskType>7</DefaultTaskType>"));
    }

    #[test]
    fn the_later_of_a_repeated_modeled_option_wins() {
        let proj = read_mspdi("<Project><Autolink>1</Autolink><Autolink>junk</Autolink></Project>")
            .unwrap();
        assert_eq!(proj.autolink, None);
        let xml = write_mspdi(&proj);
        assert!(xml.contains("<Autolink>junk</Autolink>") && !xml.contains("<Autolink>1"));
        let proj = read_mspdi("<Project><Autolink>junk</Autolink><Autolink>1</Autolink></Project>")
            .unwrap();
        assert_eq!(proj.autolink, Some(true));
        assert_eq!(proj.option("Autolink"), None);
        let xml = write_mspdi(&proj);
        assert!(xml.contains("<Autolink>1</Autolink>") && !xml.contains("junk"));
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
                "ProjectExternallyEdited",
                "ZzFutureOption",
                "ZzRepeat",
                "ZzOther",
                "ExtendedAttributes",
            ]
        );
        // A repeat keeps its first place and takes the later value.
        assert_eq!(
            header[8..11],
            [
                ("ZzFutureOption".to_string(), "7".to_string()),
                ("ZzRepeat".to_string(), "b".to_string()),
                ("ZzOther".to_string(), "x".to_string()),
            ]
        );
        assert!(!xml.contains("ZzBlock") && !xml.contains("<A>"));
        // The definitions block is kept as definitions, never as an option.
        assert!(proj.option("ExtendedAttributes").is_none() && proj.option("FieldID").is_none());
        assert_eq!(proj.extended_attribute_definitions.len(), 1);
    }

    fn leaf(name: &str, text: &str) -> XmlElement {
        XmlElement {
            name: name.into(),
            text: text.into(),
            children: Vec::new(),
        }
    }

    fn node(name: &str, children: Vec<XmlElement>) -> XmlElement {
        XmlElement {
            name: name.into(),
            text: String::new(),
            children,
        }
    }

    #[test]
    fn task_extended_attributes_round_trip() {
        let proj = task_project(
            "<Task><UID>1</UID><Name>Pour</Name><OutlineLevel>1</OutlineLevel>\
             <Duration>PT8H0M0S</Duration>\
             <ExtendedAttribute><FieldID>188743731</FieldID><Value>R&amp;D &lt;1&gt;</Value>\
             </ExtendedAttribute>\
             <ExtendedAttribute><Value>orphan</Value></ExtendedAttribute>\
             <ExtendedAttribute><FieldID>188743783</FieldID><Value>2</Value>\
             <ValueGUID>C8A6D07D-4E0D-4F63-9A8B-0D1E2F3A4B5C</ValueGUID></ExtendedAttribute>\
             <ExtendedAttribute><FieldID>188743783</FieldID><Value>PT16H0M0S</Value>\
             <DurationFormat>7</DurationFormat></ExtendedAttribute></Task>",
        );
        let expected = vec![
            ExtendedAttributeValue {
                field_id: "188743731".into(),
                value: Some("R&D <1>".into()),
                ..ExtendedAttributeValue::default()
            },
            // One without a FieldID names no field and is dropped.
            ExtendedAttributeValue {
                field_id: "188743783".into(),
                value: Some("2".into()),
                value_guid: Some("C8A6D07D-4E0D-4F63-9A8B-0D1E2F3A4B5C".into()),
                duration_format: None,
            },
            ExtendedAttributeValue {
                field_id: "188743783".into(),
                value: Some("PT16H0M0S".into()),
                value_guid: None,
                duration_format: Some(7),
            },
        ];
        assert_eq!(proj.tasks[0].extended_attributes, expected);
        let xml = write_mspdi(&proj);
        assert!(xml.contains("<Value>R&amp;D &lt;1&gt;</Value>"), "{xml}");
        assert_eq!(
            read_mspdi(&xml).unwrap().tasks[0].extended_attributes,
            expected
        );
        let back = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
        assert_eq!(back.tasks[0].extended_attributes, expected);
    }

    #[test]
    fn extended_attribute_definitions_round_trip() {
        let proj = read_mspdi(
            "<Project><Author>Me</Author>\
             <ExtendedAttributes>\
               <ExtendedAttribute><FieldID>188743731</FieldID><FieldName>Text1</FieldName>\
                 <Alias>Trade &amp; crew</Alias><Ltuid>8A3C</Ltuid>\
                 <ValueList>\
                   <Value><ID>1</ID><Value>A</Value><Description/></Value>\
                   <Value><ID>2</ID><Value>&lt;B&gt;</Value><Description> two </Description></Value>\
                 </ValueList>\
               </ExtendedAttribute>\
               <Other>skipped</Other>\
             </ExtendedAttributes>\
             <ExtendedAttributes>\
               <ExtendedAttribute><FieldID>188743783</FieldID>\
                 <Formula>[Duration]*2</Formula></ExtendedAttribute>\
             </ExtendedAttributes><Tasks/></Project>",
        )
        .unwrap();
        let expected = vec![
            node(
                "ExtendedAttribute",
                vec![
                    leaf("FieldID", "188743731"),
                    leaf("FieldName", "Text1"),
                    leaf("Alias", "Trade & crew"),
                    leaf("Ltuid", "8A3C"),
                    node(
                        "ValueList",
                        vec![
                            node(
                                "Value",
                                vec![leaf("ID", "1"), leaf("Value", "A"), leaf("Description", "")],
                            ),
                            node(
                                "Value",
                                vec![
                                    leaf("ID", "2"),
                                    leaf("Value", "<B>"),
                                    leaf("Description", " two "),
                                ],
                            ),
                        ],
                    ),
                ],
            ),
            node(
                "ExtendedAttribute",
                vec![
                    leaf("FieldID", "188743783"),
                    leaf("Formula", "[Duration]*2"),
                ],
            ),
        ];
        assert_eq!(proj.extended_attribute_definitions, expected);
        assert_eq!(proj.options, [("Author".to_string(), "Me".to_string())]);

        let xml = write_mspdi(&proj);
        // One block, after the header and before the tasks.
        assert_eq!(xml.matches("<ExtendedAttributes>").count(), 1);
        let block = xml.find("<ExtendedAttributes>").unwrap();
        assert!(xml.find("<Author>").unwrap() < block && block < xml.find("<Tasks>").unwrap());
        assert!(xml.contains("<Alias>Trade &amp; crew</Alias>"), "{xml}");
        assert!(!xml.contains("<Other>"));
        let back = read_mspdi(&xml).unwrap();
        assert_eq!(back.extended_attribute_definitions, expected);
        assert_eq!(back.options, proj.options);
        let back = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
        assert_eq!(back.extended_attribute_definitions, expected);
    }

    #[test]
    fn no_definitions_writes_no_block() {
        let plain = task_project("<Task><UID>1</UID></Task>");
        assert!(plain.extended_attribute_definitions.is_empty());
        assert!(!write_mspdi(&plain).contains("ExtendedAttributes"));
        // An empty block is consumed: no definitions, and not an option.
        let empty = read_mspdi("<Project><ExtendedAttributes/><Tasks/></Project>").unwrap();
        assert!(empty.extended_attribute_definitions.is_empty());
        assert!(empty.options.is_empty());
        assert!(!write_mspdi(&empty).contains("ExtendedAttributes"));
    }

    #[test]
    fn outline_definitions_wbs_masks_and_task_values_round_trip_in_schema_order() {
        let proj = read_mspdi(
            "<Project><Author>Me</Author><OutlineCodes>\
             <OutlineCode><FieldID>1</FieldID><FieldName>Region</FieldName>\
             <Masks><Mask><Level>1</Level><Separator>.</Separator></Mask></Masks>\
             <Values><Value><ValueID>1</ValueID><Description>A&amp;B</Description>\
             <Children><Value><ValueID>2</ValueID><Description>&lt;West&gt;</Description>\
             </Value></Children></Value></Values></OutlineCode></OutlineCodes>\
             <OutlineCodes><OutlineCode><FieldID>2</FieldID><Alias>Team</Alias>\
             </OutlineCode></OutlineCodes>\
             <WBSMasks><VerifyUniqueCodes>1</VerifyUniqueCodes><GenerateCodes>0</GenerateCodes>\
             <Prefix>PRJ&amp;</Prefix><WBSMask><Level>1</Level><Type>0</Type>\
             <Length>2</Length><Separator>.</Separator></WBSMask>\
             <WBSMask><Level>2</Level><Type>1</Type><Length>0</Length>\
             <Separator>-</Separator></WBSMask></WBSMasks>\
             <ExtendedAttributes><ExtendedAttribute><FieldID>3</FieldID>\
             </ExtendedAttribute></ExtendedAttributes>\
             <Tasks><Task><UID>1</UID><Name>Work</Name><OutlineLevel>1</OutlineLevel>\
             <Baseline><Number>0</Number><Start>2026-03-02T08:00:00</Start></Baseline>\
             <OutlineCode><FieldID>1</FieldID><ValueID>1</ValueID></OutlineCode>\
             <OutlineCode><FieldID>2</FieldID><ValueGUID>abc</ValueGUID></OutlineCode>\
             <TimephasedData><Type>1</Type><UID>1</UID><Start>2026-03-02T08:00:00</Start>\
             <Finish>2026-03-02T17:00:00</Finish><Unit>0</Unit><Value>PT8H0M0S</Value>\
             </TimephasedData></Task></Tasks></Project>",
        )
        .unwrap();
        assert_eq!(proj.outline_code_definitions.len(), 2);
        assert_eq!(
            proj.outline_code_definitions[0].children[3].children[0].children[2].children[0]
                .children[1]
                .text,
            "<West>"
        );
        assert_eq!(
            proj.wbs_masks.as_ref().unwrap().children,
            vec![
                leaf("VerifyUniqueCodes", "1"),
                leaf("GenerateCodes", "0"),
                leaf("Prefix", "PRJ&"),
                node(
                    "WBSMask",
                    vec![
                        leaf("Level", "1"),
                        leaf("Type", "0"),
                        leaf("Length", "2"),
                        leaf("Separator", "."),
                    ],
                ),
                node(
                    "WBSMask",
                    vec![
                        leaf("Level", "2"),
                        leaf("Type", "1"),
                        leaf("Length", "0"),
                        leaf("Separator", "-"),
                    ],
                ),
            ]
        );
        assert_eq!(proj.tasks[0].outline_codes.len(), 2);
        let xml = write_mspdi(&proj);
        assert_eq!(xml.matches("<OutlineCodes>").count(), 1);
        assert_eq!(xml.matches("<WBSMasks>").count(), 1);
        assert!(xml.contains("<Description>A&amp;B</Description>"));
        let positions = [
            "<Author>",
            "<OutlineCodes>",
            "<WBSMasks>",
            "<ExtendedAttributes>",
            "<Tasks>",
        ]
        .map(|tag| xml.find(tag).unwrap());
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        let task = task_xml(&xml);
        let positions =
            ["<Baseline>", "<OutlineCode>", "<TimephasedData>"].map(|tag| task.find(tag).unwrap());
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        for back in [
            read_mspdi(&xml).unwrap(),
            crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap(),
        ] {
            assert_eq!(back.outline_code_definitions, proj.outline_code_definitions);
            assert_eq!(back.wbs_masks, proj.wbs_masks);
            assert_eq!(back.tasks[0].outline_codes, proj.tasks[0].outline_codes);
        }
    }

    #[test]
    fn empty_and_repeated_header_blocks_follow_defined_rules() {
        let plain = task_project("<Task><UID>1</UID></Task>");
        let saved = write_mspdi(&plain);
        assert!(!saved.contains("OutlineCodes"));
        assert!(!saved.contains("WBSMasks"));
        assert!(!saved.contains("<OutlineCode>"));
        let proj = read_mspdi(
            "<Project><OutlineCodes/><WBSMasks/>\
             <WBSMasks><Prefix>A</Prefix></WBSMasks>\
             <WBSMasks><Prefix>B</Prefix></WBSMasks><WBSMasks/>\
             <WBSMasks>
               </WBSMasks>\
             <Tasks/></Project>",
        )
        .unwrap();
        assert!(proj.outline_code_definitions.is_empty());
        assert_eq!(
            proj.wbs_masks,
            Some(node("WBSMasks", vec![leaf("Prefix", "B")]))
        );
        let saved = write_mspdi(&proj);
        assert!(!saved.contains("OutlineCodes"));
        assert_eq!(saved.matches("<WBSMasks>").count(), 1);
        let empty = read_mspdi("<Project><OutlineCodes/><WBSMasks/><Tasks/></Project>").unwrap();
        assert!(empty.wbs_masks.is_none());
        assert!(!write_mspdi(&empty).contains("WBSMasks"));
        let text_only = read_mspdi("<Project><WBSMasks>junk</WBSMasks><Tasks/></Project>").unwrap();
        assert!(text_only.wbs_masks.is_none());
        assert!(!write_mspdi(&text_only).contains("WBSMasks"));
    }

    #[test]
    fn unsafe_children_and_wrappers_in_header_blocks_are_skipped() {
        let proj = read_mspdi(
            r#"<Project xmlns:x="urn:x">
               <OutlineCodes><OutlineCode><FieldID>1</FieldID>
                 <x:Extra><Inner>bad</Inner></x:Extra>
                 <Alias x:flag="yes"><Inner>bad</Inner></Alias>
                 <Name xmlns:y="urn:y"><Inner>bad</Inner></Name>
                 <Value>safe</Value>
               </OutlineCode></OutlineCodes>
               <OutlineCodes x:flag="yes"><OutlineCode><FieldID>bad</FieldID></OutlineCode></OutlineCodes>
               <x:OutlineCodes><OutlineCode><FieldID>bad</FieldID></OutlineCode></x:OutlineCodes>
               <WBSMasks><Prefix>safe</Prefix>
                 <x:Other><Inner>bad</Inner></x:Other>
                 <WBSMask x:flag="yes"><Inner>bad</Inner></WBSMask>
                 <Wrapper xmlns:y="urn:y"><Inner>bad</Inner></Wrapper>
               </WBSMasks>
               <WBSMasks x:flag="yes"><Prefix>bad</Prefix></WBSMasks>
               <x:WBSMasks><Prefix>bad</Prefix></x:WBSMasks>
               <Tasks/></Project>"#,
        )
        .unwrap();
        assert_eq!(
            proj.outline_code_definitions,
            vec![node(
                "OutlineCode",
                vec![leaf("FieldID", "1"), leaf("Value", "safe")]
            )]
        );
        assert_eq!(
            proj.wbs_masks,
            Some(node("WBSMasks", vec![leaf("Prefix", "safe")]))
        );
        let xml = write_mspdi(&proj);
        assert!(!xml.contains("bad") && !xml.contains("x:") && !xml.contains("xmlns:y"));
        assert_eq!(read_mspdi(&xml).unwrap().wbs_masks, proj.wbs_masks);
    }

    #[test]
    fn wbs_mask_descendants_stop_at_definition_depth_limit() {
        let deep = 10_000;
        let xml = format!(
            "<Project><WBSMasks><Prefix>P</Prefix><WBSMask>{}x{}</WBSMask>\
             </WBSMasks><Tasks/></Project>",
            "<a>".repeat(deep),
            "</a>".repeat(deep)
        );
        let proj = read_mspdi(&xml).unwrap();
        let mask = proj.wbs_masks.as_ref().unwrap();
        let mut depth = 1;
        let mut element = &mask.children[1];
        while let Some(child) = element.children.first() {
            element = child;
            depth += 1;
        }
        assert_eq!((depth, element.text.as_str()), (MAX_DEFINITION_DEPTH, ""));
        assert_eq!(
            read_mspdi(&write_mspdi(&proj)).unwrap().wbs_masks,
            proj.wbs_masks
        );
    }

    #[test]
    fn definitions_past_the_depth_bound_are_dropped_not_overflowed() {
        let deep = 10_000;
        let xml = format!(
            "<Project><ExtendedAttributes><ExtendedAttribute><FieldID>1</FieldID>\
             {}x{}</ExtendedAttribute></ExtendedAttributes><Tasks/></Project>",
            "<a>".repeat(deep),
            "</a>".repeat(deep)
        );
        let proj = read_mspdi(&xml).unwrap();
        let definition = &proj.extended_attribute_definitions[0];
        assert_eq!(definition.children[0], leaf("FieldID", "1"));
        // The chain of <a> stops at the bound (the ExtendedAttribute is depth
        // 1, its first <a> depth 2). The last kept one had a child, so it is
        // not a leaf and keeps no text.
        let mut depth = 2;
        let mut element = &definition.children[1];
        while let Some(child) = element.children.first() {
            element = child;
            depth += 1;
        }
        assert_eq!((depth, element.text.as_str()), (MAX_DEFINITION_DEPTH, ""));
        let back = read_mspdi(&write_mspdi(&proj)).unwrap();
        assert_eq!(
            back.extended_attribute_definitions,
            proj.extended_attribute_definitions
        );
    }

    #[test]
    fn prefixed_or_attributed_definition_children_are_dropped() {
        let proj = read_mspdi(
            r#"<Project xmlns="http://schemas.microsoft.com/project" xmlns:x="urn:x">
                 <ExtendedAttributes>
                   <ExtendedAttribute>
                     <FieldID>188743731</FieldID>
                     <x:Ext><Inner>1</Inner></x:Ext>
                     <Alias xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:nil="true"/>
                     <Wrapper> <Other xmlns="urn:other">2</Other> </Wrapper>
                   </ExtendedAttribute>
                   <ExtendedAttribute kind="foreign"><FieldID>1</FieldID></ExtendedAttribute>
                 </ExtendedAttributes>
                 <Tasks/>
               </Project>"#,
        )
        .unwrap();
        // A child whose children were all dropped is still not a leaf: its
        // text (the whitespace around them) is not kept.
        assert_eq!(
            proj.extended_attribute_definitions,
            [node(
                "ExtendedAttribute",
                vec![leaf("FieldID", "188743731"), leaf("Wrapper", "")]
            )]
        );
        let xml = write_mspdi(&proj);
        assert!(!xml.contains("x:") && !xml.contains("urn:other") && !xml.contains("Inner"));
        assert!(!xml.contains("<Alias") && !xml.contains("<FieldID>1<"));
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
                ("ProjectExternallyEdited", "0"),
            ]
            .map(|(n, t)| (n.to_string(), t.to_string()))
        );
    }

    /// The `<ProjectExternallyEdited>` leaves of a saved header.
    fn externally_edited(xml: &str) -> Vec<String> {
        header_of(xml)
            .into_iter()
            .filter(|(n, _)| n == "ProjectExternallyEdited")
            .map(|(_, t)| t)
            .collect()
    }

    /// #111: without `ProjectExternallyEdited` = 0 Project recomputes every
    /// duration from Start/Finish, so a project built in docxy must say it.
    #[test]
    fn save_declares_durations_authoritative() {
        for proj in [Project::default(), crate::editor::untitled_project()] {
            let xml = write_mspdi(&proj);
            assert_eq!(externally_edited(&xml), ["0"]);
            let names: Vec<String> = header_of(&xml).into_iter().map(|(n, _)| n).collect();
            let at = |name: &str| names.iter().position(|n| n == name).unwrap();
            assert!(at("NewTasksAreManual") < at("ProjectExternallyEdited"));
            let tag = xml.find("<ProjectExternallyEdited>").unwrap();
            assert!(tag < xml.find("<Tasks>").unwrap());
        }
    }

    /// #111: a source that says `1` (or anything) saves as `0`, once, and the
    /// reader keeps no stale option that a later save could repeat.
    #[test]
    fn externally_edited_source_saves_as_zero() {
        let proj = read_mspdi(
            "<Project><ProjectExternallyEdited>1</ProjectExternallyEdited><Tasks/></Project>",
        )
        .unwrap();
        assert_eq!(proj.option("ProjectExternallyEdited"), None);
        let xml = write_mspdi(&proj);
        assert_eq!(externally_edited(&xml), ["0"]);
        let again = read_mspdi(&xml).unwrap();
        assert_eq!(again.option("ProjectExternallyEdited"), None);
        assert_eq!(externally_edited(&write_mspdi(&again)), ["0"]);
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
        <Finish>2026-03-05T17:00:00</Finish><Work>PT32H0M0S</Work><Cost>800</Cost>\
        <BCWS>8.</BCWS><BCWP>7.25</BCWP></Baseline>\
        <Baseline><Number>3</Number><BCWS>5</BCWS></Baseline></Assignment>";

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
                // The baseline's Start/Finish are not the assignment's own.
                work_contour: None,
                fixed_material: None,
                has_fixed_rate_units: None,
                start: None,
                finish: None,
                regular_work_min: None,
                // Nor is the baseline's Cost.
                overtime_work_min: None,
                cost: None,
                cost_rate_table: None,
                delay: None,
                leveling_delay: None,
                leveling_delay_format: None,
                notes: None,
                extended_attributes: vec![],
                timephased_data: vec![],
                baselines: vec![
                    AssignmentBaseline {
                        number: 0,
                        timephased_data: vec![],
                        start: d(2, 8),
                        finish: d(5, 17),
                        work_min: Some(1920),
                        cost: Rate::parse("800"),
                        bcws: Rate::parse("8."),
                        bcwp: Rate::parse("7.25"),
                    },
                    AssignmentBaseline {
                        number: 3,
                        bcws: Rate::parse("5"),
                        ..AssignmentBaseline::default()
                    },
                ],
                // The #267 fields stay unset: this fixture's Baselines carry
                // none of their names.
                ..Assignment::default()
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
        let assignment = assignment_xml(&xml);
        let slot_zero = assignment
            .split("<Baseline>")
            .nth(1)
            .unwrap()
            .split("</Baseline>")
            .next()
            .unwrap();
        assert!(
            slot_zero
                .contains("<Cost>800</Cost>\n        <BCWS>8.</BCWS>\n        <BCWP>7.25</BCWP>")
        );
        let slot_three = assignment
            .split("<Baseline>")
            .nth(2)
            .unwrap()
            .split("</Baseline>")
            .next()
            .unwrap();
        assert!(slot_three.contains("<Number>3</Number>"));
        assert!(slot_three.contains("<BCWS>5</BCWS>"));
        assert!(!slot_three.contains("<BCWP>"));
        let back = read_mspdi(&xml).unwrap();
        assert_eq!(back.tasks, proj.tasks);
        assert_eq!(back.assignments, proj.assignments);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
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
        let mut proj = assignment_project(PROGRESS_ASSIGNMENT);
        let a = &mut proj.assignments[0];
        a.finish = d(5, 17);
        a.has_fixed_rate_units = Some(true);
        a.fixed_material = Some(false);
        a.regular_work_min = Some(1920);
        a.start = d(2, 8);
        a.work_contour = Some(1);
        a.cost = Rate::parse("800");
        a.cost_rate_table = Some(2);
        a.delay = Some(4800);
        a.leveling_delay = Some(9600);
        a.leveling_delay_format = Some(7);
        a.notes = Some("note".into());
        a.overtime_work_min = Some(60);
        a.extended_attributes = vec![ExtendedAttributeValue {
            field_id: "255852547".into(),
            value: Some("x".into()),
            value_guid: Some("{0}".into()),
            duration_format: Some(7),
        }];
        a.timephased_data = vec![TimephasedValue {
            kind: 1,
            uid: Some(1),
            start: d(2, 8),
            finish: d(3, 8),
            unit: Some(2),
            value: Some("PT8H0M0S".into()),
        }];
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
                "Cost",
                "CostRateTable",
                "CostVariance",
                "Delay",
                "Finish",
                "FinishVariance",
                "WorkVariance",
                "HasFixedRateUnits",
                "FixedMaterial",
                "LevelingDelay",
                "LevelingDelayFormat",
                "Notes",
                "OvertimeWork",
                "RegularWork",
                "RemainingCost",
                "RemainingWork",
                "Start",
                "Stop",
                "Resume",
                "StartVariance",
                "Units",
                "Work",
                "WorkContour",
                "ExtendedAttribute",
                "FieldID",
                "Value",
                "ValueGUID",
                "DurationFormat",
                "Baseline",
                "Number",
                "Start",
                "Finish",
                "Work",
                "Cost",
                "BCWS",
                "BCWP",
                "Baseline",
                "Number",
                "BCWS",
                "TimephasedData",
                "Type",
                "UID",
                "Start",
                "Finish",
                "Unit",
                "Value",
            ]
        );
        assert_eq!(
            assignment_project(&xml).assignments,
            proj.assignments,
            "the written children read back"
        );
    }

    /// A work resource carrying every field #84 keeps, with a standard rate
    /// shown per day and an overtime rate shown per week.
    const RESOURCE_FIELDS: &str = "<Resource><UID>1</UID><ID>1</ID><Name>Alice</Name>\
        <Type>1</Type><WorkGroup>2</WorkGroup><MaxUnits>1</MaxUnits><PeakUnits>1.5</PeakUnits>\
        <OverAllocated>1</OverAllocated><CanLevel>0</CanLevel><Work>PT40H0M0S</Work>\
        <RegularWork>PT32H0M0S</RegularWork><RemainingWork>PT24H0M0S</RemainingWork>\
        <StandardRate>800</StandardRate><StandardRateFormat>3</StandardRateFormat>\
        <OvertimeRate>6000</OvertimeRate><OvertimeRateFormat>4</OvertimeRateFormat>\
        <IsGeneric>1</IsGeneric><IsInactive>0</IsInactive><BookingType>1</BookingType>\
        <IsBudget>0</IsBudget></Resource>";

    /// Resource elements #84 keeps.
    const RESOURCE_FIELD_ELEMENTS: [&str; 13] = [
        "WorkGroup",
        "PeakUnits",
        "OverAllocated",
        "CanLevel",
        "Work",
        "RegularWork",
        "RemainingWork",
        "StandardRateFormat",
        "OvertimeRateFormat",
        "IsGeneric",
        "IsInactive",
        "BookingType",
        "IsBudget",
    ];

    /// An assignment carrying every field #84 keeps, besides #81's progress.
    const CONTOUR_ASSIGNMENT: &str = "<Assignment><UID>1</UID><TaskUID>1</TaskUID>\
        <ResourceUID>1</ResourceUID><PercentWorkComplete>25</PercentWorkComplete>\
        <Finish>2026-03-06T12:00:00</Finish><HasFixedRateUnits>1</HasFixedRateUnits>\
        <FixedMaterial>0</FixedMaterial><RegularWork>PT32H0M0S</RegularWork>\
        <RemainingWork>PT24H0M0S</RemainingWork><Start>2026-03-03T08:00:00</Start>\
        <Units>1</Units><Work>PT32H0M0S</Work><WorkContour>3</WorkContour></Assignment>";

    /// Assignment elements #84 keeps.
    const CONTOUR_ASSIGNMENT_ELEMENTS: [&str; 6] = [
        "Finish",
        "HasFixedRateUnits",
        "FixedMaterial",
        "RegularWork",
        "Start",
        "WorkContour",
    ];

    #[test]
    fn resource_rate_units_and_flags_are_read() {
        assert_eq!(
            resource_project(RESOURCE_FIELDS).resources,
            [Resource {
                uid: 1,
                id: 1,
                name: "Alice".into(),
                kind: ResourceType::Work,
                max_units: 1.0,
                standard_rate: Rate::parse("800"),
                overtime_rate: Rate::parse("6000"),
                standard_rate_format: Some(3),
                overtime_rate_format: Some(4),
                booking_type: Some(1),
                work_group: Some(2),
                is_generic: Some(true),
                is_budget: Some(false),
                is_inactive: Some(false),
                can_level: Some(false),
                over_allocated: Some(true),
                peak_units: Rate::parse("1.5"),
                work_min: Some(2400),
                regular_work_min: Some(1920),
                remaining_work_min: Some(1440),
                ..Resource::default()
            }]
        );
    }

    #[test]
    fn resource_rate_units_and_flags_survive_mspdi_and_native_package_round_trips() {
        let proj = resource_project(RESOURCE_FIELDS);
        let xml = write_mspdi(&proj);
        for element in [
            "<WorkGroup>2</WorkGroup>",
            "<PeakUnits>1.5</PeakUnits>",
            "<OverAllocated>1</OverAllocated>",
            "<CanLevel>0</CanLevel>",
            "<Work>PT40H0M0S</Work>",
            "<RegularWork>PT32H0M0S</RegularWork>",
            "<RemainingWork>PT24H0M0S</RemainingWork>",
            "<StandardRate>800</StandardRate>",
            "<StandardRateFormat>3</StandardRateFormat>",
            "<OvertimeRate>6000</OvertimeRate>",
            "<OvertimeRateFormat>4</OvertimeRateFormat>",
            "<IsGeneric>1</IsGeneric>",
            "<IsInactive>0</IsInactive>",
            "<BookingType>1</BookingType>",
            "<IsBudget>0</IsBudget>",
        ] {
            assert!(xml.contains(element), "missing {element}");
        }
        assert_eq!(read_mspdi(&xml).unwrap().resources, proj.resources);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
        assert_eq!(package.resources, proj.resources);
    }

    #[test]
    fn assignment_contour_flags_and_dates_are_read() {
        assert_eq!(
            assignment_project(CONTOUR_ASSIGNMENT).assignments,
            [Assignment {
                uid: 1,
                task_uid: 1,
                resource_uid: 1,
                units: 1.0,
                work_min: 1920,
                percent_work_complete: Some(25),
                remaining_work_min: Some(1440),
                work_contour: Some(3),
                fixed_material: Some(false),
                has_fixed_rate_units: Some(true),
                start: d(3, 8),
                finish: d(6, 12),
                regular_work_min: Some(1920),
                ..Assignment::default()
            }]
        );
    }

    #[test]
    fn assignment_contour_flags_and_dates_survive_mspdi_and_native_package_round_trips() {
        let proj = assignment_project(CONTOUR_ASSIGNMENT);
        let xml = write_mspdi(&proj);
        for element in [
            "<PercentWorkComplete>25</PercentWorkComplete>",
            "<Finish>2026-03-06T12:00:00</Finish>",
            "<HasFixedRateUnits>1</HasFixedRateUnits>",
            "<FixedMaterial>0</FixedMaterial>",
            "<RegularWork>PT32H0M0S</RegularWork>",
            "<RemainingWork>PT24H0M0S</RemainingWork>",
            "<Start>2026-03-03T08:00:00</Start>",
            "<WorkContour>3</WorkContour>",
        ] {
            assert!(assignment_xml(&xml).contains(element), "missing {element}");
        }
        assert_eq!(read_mspdi(&xml).unwrap().assignments, proj.assignments);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
        assert_eq!(package.assignments, proj.assignments);
    }

    #[test]
    fn absent_resource_and_assignment_fields_stay_absent() {
        let proj = assignment_project(
            "<Assignment><UID>1</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID>\
             <Units>1</Units><Work>PT32H0M0S</Work></Assignment>",
        );
        assert_eq!(
            proj.resources,
            [Resource {
                uid: 1,
                id: 1,
                name: "Crew".into(),
                max_units: 1.0,
                ..Resource::default()
            }]
        );
        let xml = write_mspdi(&proj);
        let resources = &xml[xml.find("<Resources>").unwrap()..xml.find("</Resources>").unwrap()];
        for name in RESOURCE_FIELD_ELEMENTS {
            assert!(!resources.contains(&format!("<{name}>")), "{name}");
        }
        for name in CONTOUR_ASSIGNMENT_ELEMENTS {
            assert!(
                !assignment_xml(&xml).contains(&format!("<{name}>")),
                "{name}"
            );
        }
    }

    #[test]
    fn invalid_resource_fields_stay_absent() {
        for element in [
            "<StandardRateFormat>h</StandardRateFormat>",
            "<OvertimeRateFormat>256</OvertimeRateFormat>",
            "<BookingType>300</BookingType>",
            "<WorkGroup>-1</WorkGroup>",
            "<IsGeneric>yes</IsGeneric>",
            "<IsBudget>2</IsBudget>",
            "<IsInactive/>",
            "<CanLevel>on</CanLevel>",
            "<OverAllocated>x</OverAllocated>",
            "<PeakUnits>lots</PeakUnits>",
            "<Work>8h</Work>",
            "<RegularWork/>",
            "<RemainingWork>PT</RemainingWork>",
        ] {
            let proj = resource_project(&format!(
                "<Resource><UID>1</UID><ID>1</ID><Name>A</Name>{element}</Resource>"
            ));
            assert_eq!(
                proj.resources,
                [Resource {
                    uid: 1,
                    id: 1,
                    name: "A".into(),
                    max_units: 1.0,
                    ..Resource::default()
                }],
                "{element}"
            );
        }
    }

    #[test]
    fn invalid_assignment_fields_stay_absent() {
        for element in [
            "<WorkContour>-1</WorkContour>",
            "<WorkContour>flat</WorkContour>",
            "<FixedMaterial>maybe</FixedMaterial>",
            "<HasFixedRateUnits/>",
            "<Start>soon</Start>",
            "<Finish/>",
            "<RegularWork>8h</RegularWork>",
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
        }
    }

    /// A work resource carrying every field #199 keeps: two rate tables, two
    /// availability periods, two baselines and two custom field values, with
    /// XML specials and a CR LF in its notes, which reads as LF (#531).
    const RESOURCE_RATES: &str = "<Resource><UID>1</UID><ID>1</ID><Name>Alice</Name>\
        <Type>1</Type><EmailAddress>a&amp;b@example.com</EmailAddress><MaxUnits>1</MaxUnits>\
        <AvailableFrom>2026-03-02T08:00:00</AvailableFrom><AvailableTo>2049-12-31T23:59:00</AvailableTo>\
        <Work>PT40H0M0S</Work><OvertimeWork>PT8H0M0S</OvertimeWork><StandardRate>50</StandardRate>\
        <Cost>2000.50</Cost><Notes>Keys &lt;desk&gt;&#13;&#10;Badge</Notes>\
        <ExtendedAttribute><FieldID>205520904</FieldID><Value>Ops</Value></ExtendedAttribute>\
        <ExtendedAttribute><FieldID>205521121</FieldID><Value>PT8H0M0S</Value>\
          <ValueGUID>{8C2A3B1E-0000-4000-8000-000000000001}</ValueGUID><DurationFormat>7</DurationFormat>\
        </ExtendedAttribute>\
        <Baseline><Number>0</Number><Work>PT40H0M0S</Work><Cost>2000</Cost><BCWS>1000</BCWS><BCWP>500</BCWP></Baseline>\
        <Baseline><Number>2</Number><Cost>0</Cost></Baseline>\
        <AvailabilityPeriods>\
          <AvailabilityPeriod><AvailableFrom>2026-03-02T08:00:00</AvailableFrom>\
            <AvailableTo>2026-03-31T17:00:00</AvailableTo><AvailableUnits>1</AvailableUnits></AvailabilityPeriod>\
          <AvailabilityPeriod><AvailableFrom>2026-04-01T08:00:00</AvailableFrom>\
            <AvailableTo>2049-12-31T23:59:00</AvailableTo><AvailableUnits>0.5</AvailableUnits></AvailabilityPeriod>\
        </AvailabilityPeriods>\
        <Rates>\
          <Rate><RatesFrom>1984-01-01T00:00:00</RatesFrom><RatesTo>2049-12-31T23:59:00</RatesTo>\
            <RateTable>0</RateTable><StandardRate>50</StandardRate><StandardRateFormat>2</StandardRateFormat>\
            <OvertimeRate>75</OvertimeRate><OvertimeRateFormat>2</OvertimeRateFormat><CostPerUse>0</CostPerUse></Rate>\
          <Rate><RatesFrom>1984-01-01T00:00:00</RatesFrom><RatesTo>2049-12-31T23:59:00</RatesTo>\
            <RateTable>1</RateTable><StandardRate>80.25</StandardRate><StandardRateFormat>3</StandardRateFormat>\
            <OvertimeRate>0</OvertimeRate><OvertimeRateFormat>2</OvertimeRateFormat><CostPerUse>15</CostPerUse></Rate>\
        </Rates></Resource>";

    /// Resource elements #199 keeps, besides those inside its blocks.
    const RESOURCE_RATE_ELEMENTS: [&str; 10] = [
        "EmailAddress",
        "AvailableFrom",
        "AvailableTo",
        "OvertimeWork",
        "Cost",
        "Notes",
        "ExtendedAttribute",
        "Baseline",
        "AvailabilityPeriods",
        "Rates",
    ];

    /// An assignment carrying every field #199 keeps: a delay, a leveling
    /// delay, table C, two custom field values and two timephased records.
    const DELAY_ASSIGNMENT: &str = "<Assignment><UID>1</UID><TaskUID>1</TaskUID>\
        <ResourceUID>1</ResourceUID><Cost>1650.5</Cost><CostRateTable>2</CostRateTable>\
        <Delay>4800</Delay><LevelingDelay>9600</LevelingDelay><LevelingDelayFormat>7</LevelingDelayFormat>\
        <Notes>Night &amp; weekend</Notes><OvertimeWork>PT4H0M0S</OvertimeWork>\
        <Units>1</Units><Work>PT32H0M0S</Work>\
        <ExtendedAttribute><FieldID>255852547</FieldID><Value>12.5</Value></ExtendedAttribute>\
        <ExtendedAttribute><FieldID>255852548</FieldID><Value></Value></ExtendedAttribute>\
        <TimephasedData><Type>1</Type><UID>1</UID><Start>2026-03-02T08:00:00</Start>\
          <Finish>2026-03-03T08:00:00</Finish><Unit>2</Unit><Value>PT8H0M0S</Value></TimephasedData>\
        <TimephasedData><Type>1</Type><UID>1</UID><Start>2026-03-03T08:00:00</Start>\
          <Finish>2026-03-06T17:00:00</Finish><Unit>2</Unit><Value>PT24H0M0S</Value></TimephasedData>\
        </Assignment>";

    /// Assignment elements #199 keeps.
    const DELAY_ASSIGNMENT_ELEMENTS: [&str; 9] = [
        "Cost",
        "CostRateTable",
        "Delay",
        "LevelingDelay",
        "LevelingDelayFormat",
        "Notes",
        "OvertimeWork",
        "ExtendedAttribute",
        "TimephasedData",
    ];

    fn at(y: i64, mo: u32, day: u32, h: u32, mi: u32) -> Option<DateTime> {
        Some(DateTime::from_ymd_hm(y, mo, day, h, mi))
    }

    #[test]
    fn resource_rates_availability_and_notes_are_read() {
        let rate = |table, standard, format, overtime, per_use| RateEntry {
            rates_from: at(1984, 1, 1, 0, 0),
            rates_to: at(2049, 12, 31, 23, 59),
            rate_table: Some(table),
            standard_rate: Rate::parse(standard),
            standard_rate_format: Some(format),
            overtime_rate: Rate::parse(overtime),
            overtime_rate_format: Some(2),
            cost_per_use: Rate::parse(per_use),
        };
        assert_eq!(
            resource_project(RESOURCE_RATES).resources,
            [Resource {
                uid: 1,
                id: 1,
                name: "Alice".into(),
                kind: ResourceType::Work,
                max_units: 1.0,
                standard_rate: Rate::parse("50"),
                work_min: Some(2400),
                overtime_work_min: Some(480),
                cost: Rate::parse("2000.50"),
                email_address: Some("a&b@example.com".into()),
                notes: Some("Keys <desk>\nBadge".into()),
                available_from: at(2026, 3, 2, 8, 0),
                available_to: at(2049, 12, 31, 23, 59),
                extended_attributes: vec![
                    ExtendedAttributeValue {
                        field_id: "205520904".into(),
                        value: Some("Ops".into()),
                        ..ExtendedAttributeValue::default()
                    },
                    ExtendedAttributeValue {
                        field_id: "205521121".into(),
                        value: Some("PT8H0M0S".into()),
                        value_guid: Some("{8C2A3B1E-0000-4000-8000-000000000001}".into()),
                        duration_format: Some(7),
                    },
                ],
                baselines: vec![
                    ResourceBaseline {
                        number: 0,
                        timephased_data: vec![],
                        work_min: Some(2400),
                        cost: Rate::parse("2000"),
                        bcws: Rate::parse("1000"),
                        bcwp: Rate::parse("500"),
                    },
                    ResourceBaseline {
                        number: 2,
                        cost: Rate::parse("0"),
                        ..ResourceBaseline::default()
                    },
                ],
                availability_periods: vec![
                    AvailabilityPeriod {
                        available_from: at(2026, 3, 2, 8, 0),
                        available_to: at(2026, 3, 31, 17, 0),
                        available_units: Rate::parse("1"),
                    },
                    AvailabilityPeriod {
                        available_from: at(2026, 4, 1, 8, 0),
                        available_to: at(2049, 12, 31, 23, 59),
                        available_units: Rate::parse("0.5"),
                    },
                ],
                rates: vec![rate(0, "50", 2, "75", "0"), rate(1, "80.25", 3, "0", "15"),],
                ..Resource::default()
            }]
        );
    }

    #[test]
    fn resource_rates_availability_and_notes_survive_mspdi_and_native_package_round_trips() {
        let proj = resource_project(RESOURCE_RATES);
        let xml = write_mspdi(&proj);
        for element in [
            "<EmailAddress>a&amp;b@example.com</EmailAddress>",
            "<AvailableFrom>2026-03-02T08:00:00</AvailableFrom>",
            "<AvailableTo>2049-12-31T23:59:00</AvailableTo>",
            "<OvertimeWork>PT8H0M0S</OvertimeWork>",
            "<Cost>2000.50</Cost>",
            "<Notes>Keys &lt;desk&gt;\nBadge</Notes>",
            "<FieldID>205520904</FieldID>",
            "<Value>Ops</Value>",
            "<ValueGUID>{8C2A3B1E-0000-4000-8000-000000000001}</ValueGUID>",
            "<DurationFormat>7</DurationFormat>",
            "<BCWS>1000</BCWS>",
            "<BCWP>500</BCWP>",
            "<Number>2</Number>",
            "<AvailableUnits>0.5</AvailableUnits>",
            "<AvailableTo>2026-03-31T17:00:00</AvailableTo>",
            "<RatesFrom>1984-01-01T00:00:00</RatesFrom>",
            "<RateTable>1</RateTable>",
            "<StandardRate>80.25</StandardRate>",
            "<CostPerUse>15</CostPerUse>",
        ] {
            assert!(xml.contains(element), "missing {element}");
        }
        let read = read_mspdi(&xml).unwrap().resources;
        assert_eq!(read, proj.resources);
        assert_eq!(read[0].rates.len(), 2);
        assert_eq!(read[0].availability_periods.len(), 2);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
        assert_eq!(package.resources, proj.resources);
    }

    #[test]
    fn assignment_delay_overtime_cost_and_timephased_data_are_read() {
        let record = |start, finish, value: &str| TimephasedValue {
            kind: 1,
            uid: Some(1),
            start,
            finish,
            unit: Some(2),
            value: Some(value.into()),
        };
        assert_eq!(
            assignment_project(DELAY_ASSIGNMENT).assignments,
            [Assignment {
                uid: 1,
                task_uid: 1,
                resource_uid: 1,
                units: 1.0,
                work_min: 1920,
                cost: Rate::parse("1650.5"),
                cost_rate_table: Some(2),
                delay: Some(4800),
                leveling_delay: Some(9600),
                leveling_delay_format: Some(7),
                notes: Some("Night & weekend".into()),
                overtime_work_min: Some(240),
                extended_attributes: vec![
                    ExtendedAttributeValue {
                        field_id: "255852547".into(),
                        value: Some("12.5".into()),
                        ..ExtendedAttributeValue::default()
                    },
                    // An empty value is kept, not confused with an absent one.
                    ExtendedAttributeValue {
                        field_id: "255852548".into(),
                        value: Some(String::new()),
                        ..ExtendedAttributeValue::default()
                    },
                ],
                timephased_data: vec![
                    record(d(2, 8), d(3, 8), "PT8H0M0S"),
                    record(d(3, 8), d(6, 17), "PT24H0M0S"),
                ],
                ..Assignment::default()
            }]
        );
    }

    #[test]
    fn assignment_delay_overtime_cost_and_timephased_data_survive_mspdi_and_native_package_round_trips()
     {
        let proj = assignment_project(DELAY_ASSIGNMENT);
        let xml = write_mspdi(&proj);
        for element in [
            "<Cost>1650.5</Cost>",
            "<CostRateTable>2</CostRateTable>",
            "<Delay>4800</Delay>",
            "<LevelingDelay>9600</LevelingDelay>",
            "<LevelingDelayFormat>7</LevelingDelayFormat>",
            "<Notes>Night &amp; weekend</Notes>",
            "<OvertimeWork>PT4H0M0S</OvertimeWork>",
            "<FieldID>255852547</FieldID>",
            "<Value>12.5</Value>",
            "<Value></Value>",
            "<Type>1</Type>",
            "<Start>2026-03-03T08:00:00</Start>",
            "<Finish>2026-03-06T17:00:00</Finish>",
            "<Unit>2</Unit>",
            "<Value>PT24H0M0S</Value>",
        ] {
            assert!(assignment_xml(&xml).contains(element), "missing {element}");
        }
        let read = read_mspdi(&xml).unwrap().assignments;
        assert_eq!(read, proj.assignments);
        assert_eq!(read[0].timephased_data.len(), 2);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
        assert_eq!(package.assignments, proj.assignments);
    }

    #[test]
    fn empty_resource_and_assignment_text_stays_distinct_from_absent() {
        let resource = &resource_project(
            "<Resource><UID>1</UID><ID>1</ID><Name>A</Name>\
             <EmailAddress></EmailAddress><Notes/></Resource>",
        )
        .resources[0];
        assert_eq!(
            (resource.email_address.as_deref(), resource.notes.as_deref()),
            (Some(""), Some(""))
        );
        let proj = assignment_project(
            "<Assignment><UID>1</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID>\
             <Notes></Notes></Assignment>",
        );
        assert_eq!(proj.assignments[0].notes.as_deref(), Some(""));
        let xml = write_mspdi(&proj);
        assert!(assignment_xml(&xml).contains("<Notes></Notes>"));
        assert_eq!(read_mspdi(&xml).unwrap().assignments, proj.assignments);
    }

    #[test]
    fn absent_rate_availability_delay_and_timephased_fields_stay_absent() {
        let proj = assignment_project(
            "<Assignment><UID>1</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID>\
             <Units>1</Units><Work>PT32H0M0S</Work></Assignment>",
        );
        let xml = write_mspdi(&proj);
        let resources = &xml[xml.find("<Resources>").unwrap()..xml.find("</Resources>").unwrap()];
        for name in RESOURCE_RATE_ELEMENTS {
            assert!(!resources.contains(&format!("<{name}>")), "{name}");
        }
        for name in DELAY_ASSIGNMENT_ELEMENTS {
            assert!(
                !assignment_xml(&xml).contains(&format!("<{name}>")),
                "{name}"
            );
        }
    }

    #[test]
    fn invalid_rate_availability_and_delay_fields_stay_absent() {
        for element in [
            "<OvertimeWork>8h</OvertimeWork>",
            "<Cost>lots</Cost>",
            "<AvailableFrom>soon</AvailableFrom>",
            "<AvailableTo/>",
            // Blocks whose every child is invalid, or that name no field, are dropped.
            "<ExtendedAttribute><Value>x</Value></ExtendedAttribute>",
            "<ExtendedAttribute><FieldID><x/></FieldID><Value>x</Value></ExtendedAttribute>",
            "<Baseline><Number>11</Number><Cost>1</Cost></Baseline>",
            "<Baseline><Number>x</Number><Cost>1</Cost></Baseline>",
            "<Baseline><Number>1</Number><Cost>free</Cost></Baseline>",
            "<AvailabilityPeriods/>",
            "<AvailabilityPeriods><AvailabilityPeriod><AvailableUnits>half</AvailableUnits>\
             <AvailableFrom>x</AvailableFrom></AvailabilityPeriod></AvailabilityPeriods>",
            "<Rates/>",
            "<Rates><Rate><RateTable>256</RateTable><StandardRate>x</StandardRate>\
             <RatesTo>never</RatesTo></Rate></Rates>",
        ] {
            let proj = resource_project(&format!(
                "<Resource><UID>1</UID><ID>1</ID><Name>A</Name>{element}</Resource>"
            ));
            assert_eq!(
                proj.resources,
                [Resource {
                    uid: 1,
                    id: 1,
                    name: "A".into(),
                    max_units: 1.0,
                    ..Resource::default()
                }],
                "{element}"
            );
        }
        for element in [
            "<Cost>x</Cost>",
            "<CostRateTable>-1</CostRateTable>",
            "<Delay>soon</Delay>",
            "<LevelingDelay>1.5</LevelingDelay>",
            "<LevelingDelayFormat>256</LevelingDelayFormat>",
            "<OvertimeWork>PT</OvertimeWork>",
            "<ExtendedAttribute><Value>x</Value></ExtendedAttribute>",
            "<TimephasedData><UID>1</UID><Value>PT8H0M0S</Value></TimephasedData>",
            "<TimephasedData><Type>x</Type><Value>PT8H0M0S</Value></TimephasedData>",
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
        }
    }

    #[test]
    fn rate_and_availability_entries_keep_their_valid_children() {
        // One valid child keeps the entry; the invalid ones beside it stay absent.
        let r = &resource_project(
            "<Resource><UID>1</UID><ID>1</ID><Name>A</Name>\
             <AvailabilityPeriods><AvailabilityPeriod><AvailableFrom>x</AvailableFrom>\
             <AvailableUnits>0</AvailableUnits></AvailabilityPeriod></AvailabilityPeriods>\
             <Rates><Rate><RateTable>4</RateTable><StandardRate>x</StandardRate></Rate></Rates>\
             <Baseline><Work>PT1H0M0S</Work></Baseline></Resource>",
        )
        .resources[0];
        assert_eq!(
            r.availability_periods,
            [AvailabilityPeriod {
                available_units: Rate::parse("0"),
                ..AvailabilityPeriod::default()
            }]
        );
        assert_eq!(
            r.rates,
            [RateEntry {
                rate_table: Some(4),
                ..RateEntry::default()
            }]
        );
        // A missing Number is slot 0, as on an assignment.
        assert_eq!(
            r.baselines,
            [ResourceBaseline {
                work_min: Some(60),
                ..ResourceBaseline::default()
            }]
        );
    }

    #[test]
    fn resource_baseline_slots_are_sorted_and_a_duplicate_replaces_the_record() {
        let r = &resource_project(
            "<Resource><UID>1</UID><ID>1</ID><Name>A</Name>\
             <Baseline><Number>10</Number><Cost>1</Cost></Baseline>\
             <Baseline><Number>3</Number><Cost>2</Cost><BCWS>5</BCWS></Baseline>\
             <Baseline><Number>3</Number><Cost>3</Cost></Baseline></Resource>",
        )
        .resources[0];
        let cost = |number, cost| ResourceBaseline {
            number,
            cost: Rate::parse(cost),
            ..ResourceBaseline::default()
        };
        assert_eq!(r.baselines, [cost(3, "3"), cost(10, "1")]);
        assert_eq!(r.baseline(10), Some(&cost(10, "1")));
    }

    /// A plan whose Standard calendar carries `standard_extra` (legacy weekday
    /// entries) and `standard_exceptions`, plus a calendar "Crew" derived from
    /// it with `crew`, and one 3-day task.
    fn exceptions_xml(standard_extra: &str, standard_exceptions: &str, crew: &str) -> String {
        let day = |d: u32| {
            if (2..=6).contains(&d) {
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
        let week: String = (1..=7).map(day).collect();
        format!(
            r#"<Project><StartDate>2026-03-02T08:00:00</StartDate><CalendarUID>1</CalendarUID><Calendars>
            <Calendar><UID>1</UID><Name>Standard</Name><IsBaseCalendar>1</IsBaseCalendar>
              <BaseCalendarUID>-1</BaseCalendarUID>
              <WeekDays>{week}{standard_extra}</WeekDays>{standard_exceptions}</Calendar>
            <Calendar><UID>2</UID><Name>Crew</Name><IsBaseCalendar>0</IsBaseCalendar>
              <BaseCalendarUID>1</BaseCalendarUID>{crew}</Calendar>
            </Calendars><Tasks>
            <Task><UID>1</UID><ID>1</ID><Name>A</Name><OutlineLevel>1</OutlineLevel>
              <Duration>PT24H0M0S</Duration></Task>
            </Tasks></Project>"#
        )
    }

    const LEGACY_HOLIDAY: &str = "<WeekDay><DayType>0</DayType><DayWorking>0</DayWorking>\
        <TimePeriod><FromDate>2026-03-04T00:00:00</FromDate><ToDate>2026-03-04T23:59:00</ToDate></TimePeriod>\
        </WeekDay>";
    const LEGACY_SATURDAY: &str = "<WeekDay><DayType>0</DayType><DayWorking>1</DayWorking>\
        <TimePeriod><FromDate>2026-03-07T00:00:00</FromDate><ToDate>2026-03-07T23:59:00</ToDate></TimePeriod>\
        <WorkingTimes><WorkingTime><FromTime>08:00:00</FromTime><ToTime>12:00:00</ToTime></WorkingTime></WorkingTimes>\
        </WeekDay>";
    /// Project's own shape: a named holiday, a working Saturday, and a
    /// yearly recurrence with every recurrence field.
    const EXCEPTIONS: &str = "<Exceptions>\
        <Exception><EnteredByOccurrences>0</EnteredByOccurrences>\
          <TimePeriod><FromDate>2026-03-04T00:00:00</FromDate><ToDate>2026-03-04T23:59:00</ToDate></TimePeriod>\
          <Occurrences>1</Occurrences><Name>Founders day</Name><Type>1</Type><DayWorking>0</DayWorking></Exception>\
        <Exception><EnteredByOccurrences>0</EnteredByOccurrences>\
          <TimePeriod><FromDate>2026-03-07T00:00:00</FromDate><ToDate>2026-03-07T23:59:00</ToDate></TimePeriod>\
          <Occurrences>1</Occurrences><Name>Stocktake</Name><Type>1</Type><DayWorking>1</DayWorking>\
          <WorkingTimes><WorkingTime><FromTime>08:00:00</FromTime><ToTime>12:00:00</ToTime></WorkingTime></WorkingTimes>\
        </Exception>\
        <Exception><EnteredByOccurrences>1</EnteredByOccurrences>\
          <TimePeriod><FromDate>2026-12-25T00:00:00</FromDate><ToDate>2030-12-25T23:59:00</ToDate></TimePeriod>\
          <Occurrences>5</Occurrences><Name>Christmas &amp; co</Name><Type>2</Type><Period>1</Period>\
          <DaysOfWeek>0</DaysOfWeek><MonthItem>0</MonthItem><MonthPosition>0</MonthPosition>\
          <Month>11</Month><MonthDay>25</MonthDay><DayWorking>0</DayWorking></Exception>\
        </Exceptions>";
    const CREW_MONDAY_OFF: &str = "<Exceptions><Exception>\
        <TimePeriod><FromDate>2026-03-09T00:00:00</FromDate><ToDate>2026-03-09T23:59:00</ToDate></TimePeriod>\
        <Type>1</Type><DayWorking>0</DayWorking></Exception></Exceptions>";

    fn march(day: u32) -> DateTime {
        DateTime::from_ymd_hm(2026, 3, day, 0, 0)
    }

    fn march_end(day: u32) -> DateTime {
        DateTime::from_ymd_hm(2026, 3, day, 23, 59)
    }

    fn morning() -> DayWorking {
        DayWorking {
            times: vec![WorkingTime {
                from: 8 * 60,
                to: 12 * 60,
            }],
        }
    }

    #[test]
    fn calendar_exceptions_keep_every_field_through_mspdi_and_yppx() {
        let xml = exceptions_xml(
            &format!("{LEGACY_HOLIDAY}{LEGACY_SATURDAY}"),
            EXCEPTIONS,
            CREW_MONDAY_OFF,
        );
        let proj = read_mspdi(&xml).unwrap();
        let standard = proj.calendar(1).unwrap();
        // The legacy entries repeat the first two exceptions and are not added.
        assert_eq!(
            standard.exceptions,
            [
                CalendarException {
                    name: Some("Founders day".into()),
                    ..CalendarException::date_range(march(4), march_end(4), DayWorking::default())
                },
                CalendarException {
                    name: Some("Stocktake".into()),
                    ..CalendarException::date_range(march(7), march_end(7), morning())
                },
                CalendarException {
                    name: Some("Christmas & co".into()),
                    from: Some(DateTime::from_ymd_hm(2026, 12, 25, 0, 0)),
                    to: Some(DateTime::from_ymd_hm(2030, 12, 25, 23, 59)),
                    kind: Some(2),
                    occurrences: Some(5),
                    entered_by_occurrences: Some(true),
                    period: Some(1),
                    days_of_week: Some(0),
                    month_item: Some(0),
                    month_position: Some(0),
                    month: Some(11),
                    month_day: Some(25),
                    day: DayWorking::default(),
                },
            ]
        );
        let crew = proj.calendar(2).unwrap();
        assert_eq!(
            crew.exceptions,
            [CalendarException {
                kind: Some(1),
                from: Some(march(9)),
                to: Some(march_end(9)),
                ..CalendarException::default()
            }]
        );
        assert_eq!(crew.week, <[Option<DayWorking>; 7]>::default());

        let written = write_mspdi(&proj);
        assert_eq!(read_mspdi(&written).unwrap().calendars, proj.calendars);
        let package = crate::yppx::read_yppx(&crate::yppx::write_yppx(&proj).unwrap()).unwrap();
        assert_eq!(package.calendars, proj.calendars);
        // Each scheduled exception is also written in the legacy form, the
        // yearly one only in `Exceptions`.
        let block = calendar_block(&written, 1);
        assert_eq!(block.matches("<DayType>0</DayType>").count(), 2);
        assert_eq!(block.matches("<Exception>").count(), 3);
        assert!(block.find("</WeekDays>").unwrap() < block.find("<Exceptions>").unwrap());
        // A derived calendar with no weekdays of its own still writes its
        // exceptions, and the legacy form of the scheduled one.
        let block = calendar_block(&written, 2);
        assert_eq!(block.matches("<WeekDay>").count(), 1);
        assert_eq!(block.matches("<DayType>0</DayType>").count(), 1);
        assert_eq!(block.matches("<Exception>").count(), 1);
        // Only the calendar has a name; the exception had none to write.
        assert_eq!(block.matches("<Name>").count(), 1);
        assert!(!block.contains("<Occurrences>"));

        // The holiday moves the task: Mon, Tue, then Thu. The working Saturday
        // comes after it.
        let sched = crate::schedule::schedule(&proj);
        assert_eq!(
            sched.get(1).unwrap().early_finish.to_mspdi(),
            "2026-03-05T17:00:00"
        );
    }

    #[test]
    fn exception_elements_are_written_in_schema_order() {
        let proj = read_mspdi(&exceptions_xml("", EXCEPTIONS, "")).unwrap();
        let written = write_mspdi(&proj);
        let block = calendar_block(&written, 1);
        let yearly = &block[block.find("<Name>Christmas").unwrap()..];
        let start = block[..block.len() - yearly.len()]
            .rfind("<Exception>")
            .unwrap();
        let end = start + block[start..].find("</Exception>").unwrap();
        let names: Vec<&str> = block[start + "<Exception>".len()..end]
            .split('<')
            .filter(|t| !t.trim().is_empty() && !t.starts_with('/'))
            .map(|t| t.split('>').next().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "EnteredByOccurrences",
                "TimePeriod",
                "FromDate",
                "ToDate",
                "Occurrences",
                "Name",
                "Type",
                "Period",
                "DaysOfWeek",
                "MonthItem",
                "MonthPosition",
                "Month",
                "MonthDay",
                "DayWorking",
            ]
        );
        assert!(written.contains("<Name>Christmas &amp; co</Name>"));
    }

    #[test]
    fn legacy_exception_entries_alone_read_as_date_range_exceptions() {
        let xml = exceptions_xml(&format!("{LEGACY_HOLIDAY}{LEGACY_SATURDAY}"), "", "");
        let proj = read_mspdi(&xml).unwrap();
        assert_eq!(
            proj.calendar(1).unwrap().exceptions,
            [
                CalendarException::date_range(march(4), march_end(4), DayWorking::default()),
                CalendarException::date_range(march(7), march_end(7), morning()),
            ]
        );
        // Written in both forms and read back from `Exceptions`: a fixed point.
        let written = write_mspdi(&proj);
        let block = calendar_block(&written, 1);
        assert_eq!(block.matches("<DayType>0</DayType>").count(), 2);
        assert_eq!(block.matches("<Exception>").count(), 2);
        assert_eq!(block.matches("<Occurrences>1</Occurrences>").count(), 2);
        assert_eq!(block.matches("<Name>").count(), 1);
        let again = read_mspdi(&written).unwrap();
        assert_eq!(again.calendars, proj.calendars);
        assert_eq!(write_mspdi(&again), written);
    }

    #[test]
    fn exceptions_win_over_legacy_entries_that_disagree() {
        // Only the `Exceptions` form counts when both are present.
        let only_saturday = "<Exceptions><Exception>\
            <TimePeriod><FromDate>2026-03-07T00:00:00</FromDate><ToDate>2026-03-07T23:59:00</ToDate></TimePeriod>\
            <Type>1</Type><DayWorking>1</DayWorking>\
            <WorkingTimes><WorkingTime><FromTime>08:00:00</FromTime><ToTime>12:00:00</ToTime></WorkingTime></WorkingTimes>\
            </Exception></Exceptions>";
        let proj = read_mspdi(&exceptions_xml(LEGACY_HOLIDAY, only_saturday, "")).unwrap();
        let exceptions = &proj.calendar(1).unwrap().exceptions;
        assert_eq!(exceptions.len(), 1);
        assert_eq!(exceptions[0].from, Some(march(7)));
        assert_eq!(exceptions[0].day, morning());
    }

    /// Tasks 1 and 2, with a link from 1 to 2 carrying this LinkLag and
    /// LagFormat (the format element omitted when `None`).
    fn lag_xml(link_lag: i64, format: Option<i64>) -> String {
        let format = format.map_or(String::new(), |f| format!("<LagFormat>{f}</LagFormat>"));
        uid_xml(&format!(
            "<Task><UID>1</UID><ID>1</ID><Duration>PT32H0M0S</Duration></Task>             <Task><UID>2</UID><ID>2</ID><Duration>PT8H0M0S</Duration><PredecessorLink>             <PredecessorUID>1</PredecessorUID><Type>1</Type>             <LinkLag>{link_lag}</LinkLag>{format}</PredecessorLink></Task>"
        ))
    }

    fn supported_lag_formats() -> impl Iterator<Item = LagFormat> {
        (0..=64).filter_map(LagFormat::from_code)
    }

    #[test]
    fn link_lag_is_read_in_its_formats_kind() {
        // #104: a percentage is the LinkLag itself; time is tenths of a minute,
        // of working or elapsed time. An absent LagFormat is days.
        for (link_lag, format, lag) in [
            (50, Some(19), 50),
            (-25, Some(19), -25),
            (-25, Some(51), -25),
            (28800, Some(8), 2880),
            (-14400, Some(8), -1440),
            (100800, Some(42), 10080),
            (1800, Some(5), 180),
            (4800, None, 480),
        ] {
            let proj = read_mspdi(&lag_xml(link_lag, format)).unwrap();
            let pred = &proj.tasks[1].predecessors[0];
            assert_eq!(pred.lag, lag, "{link_lag} {format:?}");
            assert_eq!(pred.lag_format.code(), format.unwrap_or(7));
        }
    }

    #[test]
    fn every_supported_lag_format_survives_a_save() {
        for format in supported_lag_formats() {
            for link_lag in [0, 10, -10, 50, -25, 28800, -28800, 1234560] {
                let proj = read_mspdi(&lag_xml(link_lag, Some(format.code()))).unwrap();
                let pred = &proj.tasks[1].predecessors[0];
                assert_eq!(pred.lag_format, format);
                let saved = write_mspdi(&proj);
                assert!(
                    saved.contains(&format!("<LagFormat>{}</LagFormat>", format.code())),
                    "{format:?}"
                );
                let back = read_mspdi(&saved).unwrap();
                assert_eq!(
                    back.tasks[1].predecessors, proj.tasks[1].predecessors,
                    "{format:?}"
                );
                // Whole minutes and percentages write back the LinkLag read.
                if format.kind() == LagKind::Percent || link_lag % 10 == 0 {
                    assert!(
                        saved.contains(&format!("<LinkLag>{link_lag}</LinkLag>")),
                        "{format:?} {link_lag}"
                    );
                }
            }
        }
    }

    #[test]
    fn unsupported_lag_formats_fail_the_read() {
        // Never turned into minutes: elapsed percent (20, 52), the null
        // formats (21, 53), and anything unknown.
        for code in [0, 1, 2, 13, 20, 21, 52, 53, 99, -7] {
            let err = read_mspdi(&lag_xml(50, Some(code))).unwrap_err();
            assert_eq!(
                err,
                format!("unsupported LagFormat {code} on a predecessor link of task UID 2")
            );
        }
    }

    /// Fixture 13 carries a value for every Resource and Assignment child.
    const FIXTURE_13: &str = include_str!("../../corpus/mspdi/13-resource-fields.xml");

    /// Microsoft's Resource children in the Project 2010+ sequence, as Project
    /// 2024 writes it. Not the 2007 pj12 XSD, which has no GUID, CostCenter or
    /// RateScale and puts ExtendedAttribute..OutlineCode before IsCostResource.
    #[rustfmt::skip]
    const RESOURCE_SEQUENCE: &[&str] = &[
        "UID", "GUID", "ID", "Name", "Type", "IsNull", "Initials", "Phonetics",
        "NTAccount", "MaterialLabel", "Code", "Group", "WorkGroup", "EmailAddress",
        "Hyperlink", "HyperlinkAddress", "HyperlinkSubAddress", "MaxUnits", "PeakUnits",
        "OverAllocated", "AvailableFrom", "AvailableTo", "Start", "Finish", "CanLevel",
        "AccrueAt", "Work", "RegularWork", "OvertimeWork", "ActualWork", "RemainingWork",
        "ActualOvertimeWork", "RemainingOvertimeWork", "PercentWorkComplete",
        "StandardRate", "StandardRateFormat", "Cost", "OvertimeRate", "OvertimeRateFormat",
        "OvertimeCost", "CostPerUse", "ActualCost", "ActualOvertimeCost", "RemainingCost",
        "RemainingOvertimeCost", "WorkVariance", "CostVariance", "SV", "CV", "ACWP",
        "CalendarUID", "Notes", "BCWS", "BCWP", "IsGeneric", "IsInactive", "IsEnterprise",
        "BookingType", "ActualWorkProtected", "ActualOvertimeWorkProtected",
        "ActiveDirectoryGUID", "CreationDate", "CostCenter", "IsCostResource", "AssnOwner",
        "AssnOwnerGuid", "IsBudget", "ExtendedAttribute", "Baseline", "OutlineCode",
        "AvailabilityPeriods", "Rates", "TimephasedData",
    ];

    /// Microsoft's Assignment children in the same Project 2010+ sequence.
    #[rustfmt::skip]
    const ASSIGNMENT_SEQUENCE: &[&str] = &[
        "UID", "GUID", "TaskUID", "ResourceUID", "PercentWorkComplete", "ActualCost",
        "ActualFinish", "ActualOvertimeCost", "ActualOvertimeWork", "ActualStart",
        "ActualWork", "ACWP", "Confirmed", "Cost", "CostRateTable", "RateScale",
        "CostVariance", "CV", "Delay", "Finish", "FinishVariance", "Hyperlink",
        "HyperlinkAddress", "HyperlinkSubAddress", "WorkVariance", "HasFixedRateUnits",
        "FixedMaterial", "LevelingDelay", "LevelingDelayFormat", "LinkedFields", "Milestone",
        "Notes", "Overallocated", "OvertimeCost", "OvertimeWork", "PeakUnits", "RegularWork",
        "RemainingCost", "RemainingOvertimeCost", "RemainingOvertimeWork", "RemainingWork",
        "ResponsePending", "Start", "Stop", "Resume", "StartVariance", "Summary", "SV",
        "Units", "UpdateNeeded", "VAC", "Work", "WorkContour", "BCWS", "BCWP", "BookingType",
        "ActualWorkProtected", "ActualOvertimeWorkProtected", "CreationDate", "AssnOwner",
        "AssnOwnerGuid", "BudgetCost", "BudgetWork", "ExtendedAttribute", "Baseline",
        "TimephasedData",
    ];

    /// Each `element`'s direct children in written order, a repeated child
    /// (e.g. `ExtendedAttribute`) once per run. The writer puts one child per
    /// line three levels deep, so a deeper `Start` is not the element's own.
    fn child_names<'a>(xml: &'a str, element: &str) -> Vec<Vec<&'a str>> {
        let (open, close) = (format!("    <{element}>"), format!("    </{element}>"));
        let mut out = Vec::new();
        let mut names: Option<Vec<&str>> = None;
        for line in xml.lines() {
            if line == open {
                names = Some(Vec::new());
            } else if line == close {
                out.extend(names.take());
            } else if let Some(names) = &mut names {
                let Some(tag) = line.strip_prefix("      <") else {
                    continue;
                };
                if tag.starts_with([' ', '/']) {
                    continue;
                }
                let name = &tag[..tag.find(['>', ' ']).unwrap()];
                if names.last() != Some(&name) {
                    names.push(name);
                }
            }
        }
        out
    }

    /// #267: every Resource and Assignment child survives a save, in the
    /// schema's sequence. A child missing from the output is either dropped
    /// by the reader or writer, or absent from fixture 13.
    #[test]
    fn resource_and_assignment_children_are_written_complete_and_in_schema_order() {
        let xml = write_mspdi(&read_mspdi(FIXTURE_13).unwrap());
        for (element, sequence) in [
            ("Resource", RESOURCE_SEQUENCE),
            ("Assignment", ASSIGNMENT_SEQUENCE),
        ] {
            let written = child_names(&xml, element);
            assert!(!written.is_empty(), "no {element} written");
            for names in &written {
                // A subsequence: each name found after the previous one.
                let mut rest = sequence.iter();
                for name in names {
                    assert!(
                        rest.any(|s| s == name),
                        "{element}: {name} out of sequence in {names:?}"
                    );
                }
            }
            let missing: Vec<_> = sequence
                .iter()
                .filter(|name| !written.iter().any(|names| names.contains(name)))
                .collect();
            assert!(missing.is_empty(), "{element}: none writes {missing:?}");
        }
    }

    /// #267: a work edit drops what described the old work's overtime, and a
    /// units edit also the peak units; every other stored field stays.
    #[test]
    fn work_and_units_edits_clear_only_what_described_the_old_work() {
        let imported = read_mspdi(FIXTURE_13).unwrap().assignments[0].clone();
        for field in [
            imported.overtime_cost.is_some(),
            imported.remaining_overtime_work_min.is_some(),
            imported.remaining_overtime_cost.is_some(),
            imported.peak_units.is_some(),
            imported.actual_overtime_cost.is_some(),
            imported.budget_work_min.is_some(),
            // Records the planned-work filter must keep (actuals, Baseline).
            imported
                .timephased_data
                .iter()
                .any(|t| t.kind != TimephasedValue::REMAINING_WORK),
        ] {
            assert!(field, "fixture 13 must set the fields this test watches");
        }
        let cleared_by_work = |a: &Assignment| Assignment {
            work_min: a.work_min,
            regular_work_min: None,
            overtime_work_min: None,
            overtime_cost: None,
            remaining_overtime_work_min: None,
            remaining_overtime_cost: None,
            cost: None,
            timephased_data: imported
                .timephased_data
                .iter()
                .filter(|t| t.kind != TimephasedValue::REMAINING_WORK)
                .cloned()
                .collect(),
            ..imported.clone()
        };
        let mut a = imported.clone();
        a.set_work(240);
        assert_eq!(a.work_min, 240);
        // Actual overtime, earned value, the budget and the peak units stay.
        assert_eq!(a, cleared_by_work(&a));
        assert_eq!(a.peak_units, imported.peak_units);
        let mut a = imported.clone();
        a.set_units(0.5, 480);
        assert_eq!((a.units, a.work_min), (0.5, 480));
        assert_eq!(
            a,
            Assignment {
                units: 0.5,
                peak_units: None,
                ..cleared_by_work(&a)
            }
        );
    }

    /// Each `element`'s direct leaf children as (name, text), in written
    /// order; blocks such as `Baseline` are left out.
    fn leaf_children<'a>(xml: &'a str, element: &str) -> Vec<Vec<(&'a str, &'a str)>> {
        let (open, close) = (format!("    <{element}>"), format!("    </{element}>"));
        let mut out = Vec::new();
        let mut leaves: Option<Vec<_>> = None;
        for line in xml.lines() {
            if line == open {
                leaves = Some(Vec::new());
            } else if line == close {
                out.extend(leaves.take());
            } else if let Some(leaves) = &mut leaves {
                let Some(tag) = line.strip_prefix("      <") else {
                    continue;
                };
                if let Some((name, rest)) = tag.split_once('>') {
                    if let Some(text) = rest.strip_suffix(&format!("</{name}>")) {
                        leaves.push((name, text));
                    }
                }
            }
        }
        out
    }

    /// #267: a new element whose value a sibling shares would hide a
    /// reader or writer swap between the two, so fixture 13 gives each its
    /// own. Flags and BookingType, whose only codes 0 and 1 every flag shares,
    /// are left to the one-flag-at-a-time test.
    #[test]
    fn fixture_13_gives_each_new_value_its_own_text() {
        #[rustfmt::skip]
        let new_resource = [
            "GUID", "Phonetics", "NTAccount", "Hyperlink", "HyperlinkAddress",
            "HyperlinkSubAddress", "Start", "Finish", "ActualWork", "ActualOvertimeWork",
            "RemainingOvertimeWork", "PercentWorkComplete", "OvertimeCost", "ActualCost",
            "ActualOvertimeCost", "RemainingCost", "RemainingOvertimeCost", "WorkVariance",
            "CostVariance", "SV", "CV", "ACWP", "BCWS", "BCWP", "ActualWorkProtected",
            "ActualOvertimeWorkProtected", "ActiveDirectoryGUID", "CreationDate", "CostCenter",
            "AssnOwner", "AssnOwnerGuid",
        ];
        #[rustfmt::skip]
        let new_assignment = [
            "GUID", "ActualOvertimeCost", "ActualOvertimeWork", "ACWP", "RateScale", "CV",
            "Hyperlink", "HyperlinkAddress", "HyperlinkSubAddress", "OvertimeCost", "PeakUnits",
            "RemainingOvertimeCost", "RemainingOvertimeWork", "SV", "VAC", "BCWS", "BCWP",
            "ActualWorkProtected", "ActualOvertimeWorkProtected", "CreationDate",
            "AssnOwner", "AssnOwnerGuid", "BudgetCost", "BudgetWork",
        ];
        let xml = write_mspdi(&read_mspdi(FIXTURE_13).unwrap());
        for (element, new) in [
            ("Resource", &new_resource[..]),
            ("Assignment", &new_assignment[..]),
        ] {
            for leaves in leaf_children(&xml, element) {
                for &(name, text) in leaves.iter().filter(|(name, _)| new.contains(name)) {
                    let shared: Vec<_> = leaves
                        .iter()
                        .filter(|&&(other, value)| other != name && value == text)
                        .map(|(other, _)| other)
                        .collect();
                    assert!(
                        shared.is_empty(),
                        "{element} {name} shares {text:?} with {shared:?}"
                    );
                }
            }
        }
    }

    /// #267: two values cannot tell seven flags apart in one fixture, so a
    /// reader or writer swap between two flags of equal value would pass the
    /// fixture tests. Set one flag at a time: it alone must come back set.
    /// BookingType's codes are 0 and 1 too, so it joins the flags.
    #[test]
    fn each_resource_and_assignment_flag_round_trips_as_itself() {
        let one_set = |flags: &[&str], set: &str| -> String {
            flags
                .iter()
                .map(|f| format!("<{f}>{}</{f}>", u8::from(*f == set)))
                .collect()
        };
        let resource_flags = [
            "IsNull",
            "OverAllocated",
            "CanLevel",
            "IsGeneric",
            "IsInactive",
            "IsEnterprise",
            "IsBudget",
            "BookingType",
        ];
        for set in resource_flags {
            let proj = resource_project(&format!(
                "<Resource><UID>1</UID><ID>1</ID><Name>A</Name>{}</Resource>",
                one_set(&resource_flags, set)
            ));
            let xml = write_mspdi(&proj);
            for flag in resource_flags {
                let value = u8::from(flag == set);
                assert!(
                    xml.contains(&format!("<{flag}>{value}</{flag}>")),
                    "{set} set: {flag} not {value}"
                );
            }
        }
        let assignment_flags = [
            "Confirmed",
            "HasFixedRateUnits",
            "FixedMaterial",
            "LinkedFields",
            "Milestone",
            "Overallocated",
            "ResponsePending",
            "Summary",
            "UpdateNeeded",
            "BookingType",
        ];
        for set in assignment_flags {
            let proj = assignment_project(&format!(
                "<Assignment><UID>1</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID>                 {}</Assignment>",
                one_set(&assignment_flags, set)
            ));
            let xml = write_mspdi(&proj);
            let assignments = &xml[xml.find("<Assignments>").unwrap()..];
            for flag in assignment_flags {
                let value = u8::from(flag == set);
                assert!(
                    assignments.contains(&format!("<{flag}>{value}</{flag}>")),
                    "{set} set: {flag} not {value}"
                );
            }
        }
    }
}
