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
                    // An unknown element at this level: consume it whole so its
                    // children can't be mistaken for header fields.
                    _ => p.skip_element(),
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
    let mut s = String::new();
    loop {
        match p.next() {
            Event::Text => XmlParser::append_decoded(p.text(), &mut s),
            Event::Start => p.skip_element(),
            Event::End | Event::Eof => break,
        }
    }
    s
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
    let mut a = Assignment {
        uid: 0,
        task_uid: 0,
        resource_uid: 0,
        units: 1.0,
        work_min: 0,
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
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    a
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
        week: Default::default(),
    };
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "UID" => cal.uid = int_of(p) as i32,
                    "Name" => cal.name = text_of(p),
                    "WeekDays" => parse_weekdays(p, &mut cal.week),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    cal
}

fn parse_weekdays(p: &mut XmlParser, week: &mut [DayWorking; 7]) {
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

fn parse_weekday(p: &mut XmlParser, week: &mut [DayWorking; 7]) {
    // MSPDI DayType: 1=Sunday .. 7=Saturday. Our week[] is Sunday=0..Saturday=6.
    let mut day_type: Option<usize> = None;
    let mut working = false;
    let mut times: Vec<WorkingTime> = Vec::new();
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "DayType" => day_type = Some((int_of(p) as usize).saturating_sub(1)),
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
        if d < 7 {
            // A non-working day yields empty times even if some were present.
            week[d] = DayWorking {
                times: if working { times } else { Vec::new() },
            };
        }
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
/// and for our own reader to round-trip. Elements outside the model (custom
/// fields, views, extended attributes) are not preserved: this is a
/// model-faithful writer, not a byte-faithful one. Each task's stored
/// `Start`/`Finish` are written when present (e.g. after scheduling and
/// stamping them back), so a scheduled project exports with dates Project can
/// display without recalculating.
pub fn write_mspdi(proj: &Project) -> String {
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    s.push_str("<Project xmlns=\"http://schemas.microsoft.com/project\">\n");
    tag(&mut s, 1, "Name", &proj.name);
    if !proj.title.is_empty() {
        tag(&mut s, 1, "Title", &proj.title);
    }
    tag(
        &mut s,
        1,
        "MinutesPerDay",
        &((proj.hours_per_day * 60.0).round() as i64).to_string(),
    );
    tag(
        &mut s,
        1,
        "MinutesPerWeek",
        &((proj.hours_per_week * 60.0).round() as i64).to_string(),
    );
    tag(
        &mut s,
        1,
        "CalendarUID",
        &proj.default_calendar_uid.to_string(),
    );
    if let Some(d) = proj.start_date {
        tag(&mut s, 1, "StartDate", &d.to_mspdi());
    }
    tag(
        &mut s,
        1,
        "HonorConstraints",
        if proj.honor_constraints { "1" } else { "0" },
    );

    s.push_str("  <Tasks>\n");
    for t in &proj.tasks {
        write_task(&mut s, t);
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

fn write_task(s: &mut String, t: &Task) {
    s.push_str("    <Task>\n");
    tag(s, 3, "UID", &t.uid.to_string());
    tag(s, 3, "ID", &t.id.to_string());
    tag(s, 3, "Name", &t.name);
    tag(s, 3, "OutlineLevel", &t.outline_level.to_string());
    tag(s, 3, "Summary", if t.summary { "1" } else { "0" });
    tag(s, 3, "Milestone", if t.milestone { "1" } else { "0" });
    tag(s, 3, "Duration", &min_to_iso(t.duration_min));
    tag(s, 3, "DurationFormat", "7");
    if let Some(d) = t.stored_start {
        tag(s, 3, "Start", &d.to_mspdi());
    }
    if let Some(d) = t.stored_finish {
        tag(s, 3, "Finish", &d.to_mspdi());
    }
    tag(s, 3, "ConstraintType", &t.constraint.code().to_string());
    if let Some(d) = t.constraint_date {
        tag(s, 3, "ConstraintDate", &d.to_mspdi());
    }
    if let Some(c) = t.calendar_uid {
        tag(s, 3, "CalendarUID", &c.to_string());
    }
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
    tag(s, 3, "Units", &fmt_f(a.units));
    tag(s, 3, "Work", &min_to_iso(a.work_min));
    s.push_str("    </Assignment>\n");
}

fn write_calendar(s: &mut String, c: &Calendar) {
    s.push_str("    <Calendar>\n");
    tag(s, 3, "UID", &c.uid.to_string());
    tag(s, 3, "Name", &c.name);
    tag(s, 3, "IsBaseCalendar", "1");
    s.push_str("      <WeekDays>\n");
    for (idx, day) in c.week.iter().enumerate() {
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
                ["", "abc", "NaN", "inf", "-infinity", "1e999"]
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
        assert_eq!(cal.week[1].minutes(), 480); // Monday (DayType 2) = 8h
        assert!(!cal.week[0].working()); // Sunday (DayType 1) off
    }

    fn empty_calendar_project() -> Project {
        let mut proj = read_mspdi(MINIMAL).unwrap();
        proj.calendars.push(Calendar {
            uid: 3,
            name: "Closed".into(),
            week: Default::default(),
        });
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
        for day in &mut proj.calendars[0].week {
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
        assert_eq!(cal.week[1].minutes(), 480); // Monday still 8h
        assert_eq!(cal.week[1].times.len(), 2); // two shifts preserved
        assert!(!cal.week[0].working() && !cal.week[6].working()); // weekend off
    }
}
