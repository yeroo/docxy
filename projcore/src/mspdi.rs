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
    let mut proj = Project { calendars: Vec::new(), ..Project::default() };
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
                    "Tasks" => parse_tasks(&mut p, &mut proj.tasks),
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

    if let Some(m) = minutes_per_day {
        proj.hours_per_day = m / 60.0;
    }
    proj.hours_per_week = minutes_per_week
        .map(|m| m / 60.0)
        .unwrap_or(proj.hours_per_day * 5.0);
    if proj.calendars.is_empty() {
        proj.calendars.push(Calendar::standard(proj.default_calendar_uid));
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

fn parse_tasks(p: &mut XmlParser, out: &mut Vec<Task>) {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "Task" {
                    out.push(parse_task(p));
                } else {
                    p.skip_element();
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
}

fn parse_task(p: &mut XmlParser) -> Task {
    let mut t = Task::default();
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "UID" => t.uid = int_of(p) as i32,
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
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
    t
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
                    "Type" => link = LinkType::from_code(int_of(p)).unwrap_or(LinkType::FinishStart),
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
    let mut r = Resource { max_units: 1.0, is_work: true, ..Resource::default() };
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name().to_string();
                match name.as_str() {
                    "UID" => r.uid = int_of(p) as i32,
                    "ID" => r.id = int_of(p) as i32,
                    "Name" => r.name = text_of(p),
                    "Type" => r.is_work = int_of(p) == 1,
                    "MaxUnits" => r.max_units = float_of(p),
                    "CalendarUID" => r.calendar_uid = Some(int_of(p) as i32),
                    _ => p.skip_element(),
                }
            }
            Event::End | Event::Eof => break,
            _ => {}
        }
    }
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
    let mut a = Assignment { uid: 0, task_uid: 0, resource_uid: 0, units: 1.0, work_min: 0 };
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
    let mut cal = Calendar { uid: 0, name: String::new(), week: Default::default() };
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
            week[d] = DayWorking { times: if working { times } else { Vec::new() } };
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
    // Project encodes a shift ending at midnight as 00:00; treat as end-of-day.
    if to == 0 && from != 0 {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
