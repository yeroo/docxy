//! Reader for MPX, the comma-separated text interchange format Microsoft
//! Project 4.0 to 2010 saved and later versions still open (#309).
//!
//! An MPX file is a header line (`MPX,Microsoft Project for Windows,4.0,ANSI`,
//! the character after `MPX` being the field delimiter) followed by one
//! record per line: a record type number, then its fields. The records read
//! here:
//!
//! - 10 currency and number settings (symbol, position, digits, thousands
//!   and decimal separators), 11 defaults (hours per day and per week),
//!   12 date and time settings (date order, default start time, separators,
//!   am/pm text);
//! - 20 a base calendar (name, then Sunday..Saturday as 0 non-working,
//!   1 working, 2 default), 25 its hours for one day (1 = Sunday, up to three
//!   from/to pairs), 26 an exception (from, to, working, pairs); 55, 56, 57
//!   the same for the resource before them, derived from the base calendar
//!   55 names;
//! - 30 the project header (title, calendar, start date);
//! - 40/41 and 60/61 the resource and task columns: 41 and 61 give the field
//!   id of each column, so every 50 (resource) and 70 (task) row after them
//!   is read by those ids; 51 and 71 are the notes of the row before;
//! - 75 an assignment of the task before it.
//!
//! Anything else is ignored, as is a field that does not parse (the model's
//! default stays). Only a file without the header, or without a task
//! record, is refused.
//!
//! What the file states is read as written; what it does not carry is not
//! guessed. Dates are whole days (Project writes no time), so a constraint,
//! actual or stored date lands on the task calendar's first working minute
//! of the day for a start and its last for a finish ([`crate::editor::day_start`],
//! [`crate::editor::day_end`]). Task calendars, manual scheduling, the
//! Active flag and the "honor constraint dates" option have no MPX field, so
//! a plan using them reads as Project's defaults. A task's Start and Finish
//! columns are the dates Project computed: they are kept as the stored dates
//! the MSPDI reader keeps, never as a constraint, so the schedule is the
//! scheduler's. Enumerated values (constraint type, priority, accrue at, yes
//! and no) are Project's English words; a localised file's words read as the
//! default.
//!
//! The rules were checked against MPXJ 16.8.0's writer and reader on the
//! `corpus/mpx` files (`corpus/mpx/README.md`); there is no Project oracle.

use crate::datetime::DateTime;
use crate::editor::{day_end, day_start, parse_task_predecessors};
use crate::model::{
    AccrueAt, Assignment, Calendar, CalendarException, ConstraintType, DayWorking, LagFormat,
    LagUnit, Project, Rate, Resource, Task, TaskType, WorkingTime,
};

/// Whether `bytes` start as an MPX file does: `MPX` and a `,` or `;`
/// delimiter, after an optional UTF-8 byte order mark.
pub fn is_mpx(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    matches!(bytes, [b'M', b'P', b'X', b',' | b';', ..])
}

/// The project held in the bytes of an MPX file.
pub fn read_mpx(bytes: &[u8]) -> Result<Project, String> {
    let text = decode(bytes);
    let mut chars = text.chars();
    let delimiter = match (chars.next(), chars.next(), chars.next(), chars.next()) {
        (Some('M'), Some('P'), Some('X'), Some(d))
            if !d.is_alphanumeric() && d != '"' && !d.is_whitespace() =>
        {
            d
        }
        _ => return Err("not an MPX file: expected an MPX header".into()),
    };
    let mut reader = Reader::default();
    for record in records(&text, delimiter).into_iter().skip(1) {
        reader.record(&record);
    }
    reader.finish()
}

/// The text of the file: UTF-8 when it is valid UTF-8 (a byte order mark
/// dropped), else Windows-1252, the `ANSI` code page Project writes.
fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes.iter().map(|&b| cp1252(b)).collect(),
    }
}

/// One Windows-1252 byte as a character; the five bytes the code page leaves
/// undefined are U+FFFD.
fn cp1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{FFFD}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{FFFD}', 'Ž',
        '\u{FFFD}', '\u{FFFD}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ',
        '\u{FFFD}', 'ž', 'Ÿ',
    ];
    match byte {
        0x80..=0x9F => HIGH[usize::from(byte - 0x80)],
        _ => char::from(byte),
    }
}

/// The records of `text`, each a list of fields with the record type first.
/// A field in `"` quotes may hold the delimiter, a line break and `""` for a
/// quote; a record ends at a line break outside quotes. Blank lines are none.
fn records(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' && field.is_empty() {
            quoted = true;
        } else if c == delimiter {
            record.push(std::mem::take(&mut field));
        } else if c == '\r' || c == '\n' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            record.push(std::mem::take(&mut field));
            if !(record.len() == 1 && record[0].is_empty()) {
                out.push(std::mem::take(&mut record));
            }
            record.clear();
        } else {
            field.push(c);
        }
    }
    record.push(field);
    if !(record.len() == 1 && record[0].is_empty()) {
        out.push(record);
    }
    out
}

/// The order of a date's day, month and year (record 12, field 1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DateOrder {
    Mdy,
    Dmy,
    Ymd,
}

/// The number, date and time settings of records 10 and 12.
#[derive(Debug)]
struct Settings {
    currency: String,
    thousands: char,
    decimal: char,
    date_order: DateOrder,
    date_sep: char,
    time_sep: char,
    am: String,
    pm: String,
    /// Minutes after midnight (record 12, field 3).
    default_start: u32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            currency: "$".into(),
            thousands: ',',
            decimal: '.',
            date_order: DateOrder::Mdy,
            date_sep: '/',
            time_sep: ':',
            am: "am".into(),
            pm: "pm".into(),
            default_start: 8 * 60,
        }
    }
}

/// The first character of a field, if it has exactly one.
fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

impl Settings {
    /// A plain number as the file writes it: the thousands separator
    /// dropped, its decimal separator a point.
    fn number(&self, text: &str) -> Option<f64> {
        let cleaned: String = text
            .trim()
            .chars()
            .filter(|&c| c != self.thousands || c == self.decimal)
            .map(|c| if c == self.decimal { '.' } else { c })
            .collect();
        cleaned.parse::<f64>().ok().filter(|v| v.is_finite())
    }

    /// Money (`$1,250.50`, `-$0.12`, `($0.12)`) as decimal text.
    fn money(&self, text: &str) -> Option<Rate> {
        let mut t = text.trim();
        let mut negative = false;
        if let Some(inner) = t.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
            negative = true;
            t = inner.trim();
        }
        if let Some(rest) = t.strip_prefix('-') {
            negative = !negative;
            t = rest.trim();
        }
        if !self.currency.is_empty() {
            t = t.strip_prefix(self.currency.as_str()).unwrap_or(t).trim();
            t = t.strip_suffix(self.currency.as_str()).unwrap_or(t).trim();
        }
        if let Some(rest) = t.strip_prefix('-') {
            negative = !negative;
            t = rest.trim();
        }
        let value = self.number(t)?;
        Rate::from_f64(if negative { -value } else { value })
    }

    /// A date, with its time when one follows it (`03/02/2026 08:00 am`).
    /// The time is `None` for a bare date.
    fn date(&self, text: &str) -> Option<(DateTime, Option<u32>)> {
        let text = text.trim();
        let (date, time) = match text.split_once(' ') {
            Some((d, t)) => (d, Some(t.trim())),
            None => (text, None),
        };
        let parts: Vec<&str> = date.split([self.date_sep, '/', '-', '.']).collect();
        let [a, b, c] = parts.as_slice() else {
            return None;
        };
        let (y, m, d) = match self.date_order {
            DateOrder::Mdy => (c, a, b),
            DateOrder::Dmy => (c, b, a),
            DateOrder::Ymd => (a, b, c),
        };
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if !(digits(y) && digits(m) && digits(d)) {
            return None;
        }
        let mut year: i64 = y.parse().ok()?;
        if y.len() <= 2 {
            // Two-digit years (exception records): 00-79 are 2000-2079,
            // 80-99 are 1980-1999, as MPXJ's reader reads `26` as 2026.
            year += if year < 80 { 2000 } else { 1900 };
        }
        let month: u32 = m.parse().ok()?;
        let day: u32 = d.parse().ok()?;
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) || !(1..=9999).contains(&year) {
            return None;
        }
        let at = DateTime::from_ymd_hm(year, month, day, 0, 0);
        let p = at.parts();
        if (p.year, p.month, p.day) != (year, month, day) {
            return None;
        }
        let time = match time {
            Some(t) if !t.is_empty() => Some(self.time(t)?),
            _ => None,
        };
        Some((at, time))
    }

    /// A time of day in minutes after midnight: `08:00 am`, `01:00 pm` or
    /// a 24-hour `13:00`.
    fn time(&self, text: &str) -> Option<u32> {
        let t = text.trim().to_lowercase();
        let (clock, half) = if let Some(rest) = strip_suffix_word(&t, &self.am) {
            (rest, Some(false))
        } else if let Some(rest) = strip_suffix_word(&t, &self.pm) {
            (rest, Some(true))
        } else {
            (t.as_str(), None)
        };
        let (h, m) = clock
            .split_once([self.time_sep, ':'])
            .unwrap_or((clock, "0"));
        let (h, m) = (h.trim(), m.trim());
        if h.is_empty() || !h.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if !m.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut hour: u32 = h.parse().ok()?;
        let minute: u32 = if m.is_empty() { 0 } else { m.parse().ok()? };
        match half {
            Some(pm) => {
                if !(1..=12).contains(&hour) {
                    return None;
                }
                hour %= 12;
                if pm {
                    hour += 12;
                }
            }
            None if hour > 24 => return None,
            None => {}
        }
        if minute > 59 || hour * 60 + minute > 24 * 60 {
            return None;
        }
        Some(hour * 60 + minute)
    }
}

/// `text` without a trailing `word` (any case, `word` lower-cased by the
/// caller's settings), when it ends with it.
fn strip_suffix_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let word = word.trim().to_lowercase();
    if word.is_empty() {
        return None;
    }
    text.strip_suffix(word.as_str()).map(str::trim_end)
}

/// A task row (record 70) as read, before the tasks are ordered and the
/// fields that need the whole plan (links, dates on the calendar) resolve.
#[derive(Default)]
struct RawTask {
    task: Task,
    /// Field 90, the row's ID; `None` keeps the row where the file has it.
    id: Option<i32>,
    uid: Option<i32>,
    /// Field 3 was present: a row with no name and no outline level is a
    /// blank row.
    has_outline_level: bool,
    milestone: Option<bool>,
    predecessors: String,
    stored_start: Option<(DateTime, Option<u32>)>,
    stored_finish: Option<(DateTime, Option<u32>)>,
    constraint_date: Option<(DateTime, Option<u32>)>,
    actual_start: Option<(DateTime, Option<u32>)>,
    actual_finish: Option<(DateTime, Option<u32>)>,
    stop: Option<(DateTime, Option<u32>)>,
    resume: Option<(DateTime, Option<u32>)>,
}

/// An assignment row (record 75) and the task row it follows.
struct RawAssignment {
    task: usize,
    fields: Vec<String>,
}

/// A calendar the file defines, while its day records arrive.
struct CalendarDraft {
    index: usize,
    /// Record 20/55's day flags, Sunday first: 0 non-working, 1 working,
    /// 2 the base calendar's (a resource calendar's) or the default.
    flags: [u8; 7],
}

#[derive(Default)]
struct Reader {
    settings: Settings,
    hours_per_day: Option<f64>,
    hours_per_week: Option<f64>,
    title: String,
    calendar_name: Option<String>,
    start: Option<(DateTime, Option<u32>)>,
    calendars: Vec<Calendar>,
    /// The base calendar each resource calendar names (record 55, field 1).
    calendar_bases: Vec<(usize, String)>,
    /// The calendar the day records (25, 26, 56, 57) belong to.
    open_calendar: Option<CalendarDraft>,
    task_fields: Vec<u32>,
    resource_fields: Vec<u32>,
    tasks: Vec<RawTask>,
    resources: Vec<Resource>,
    /// Each resource's file UID, `None` when the row gave none.
    resource_uids: Vec<Option<i32>>,
    assignments: Vec<RawAssignment>,
    /// Whether the last row record was a task (70) or a resource (50), for
    /// the notes records 71 and 51.
    last_row_is_task: bool,
}

impl Reader {
    fn record(&mut self, record: &[String]) {
        let Some(kind) = record.first().and_then(|k| k.trim().parse::<u32>().ok()) else {
            return;
        };
        let fields = &record[1..];
        let field = |i: usize| fields.get(i - 1).map(|f| f.trim()).unwrap_or("");
        match kind {
            10 => {
                // Untrimmed: a space is a thousands separator in some locales.
                let raw = |i: usize| fields.get(i - 1).map_or("", String::as_str);
                self.settings.currency = raw(1).trim().to_string();
                if let Some(c) = single_char(raw(4)) {
                    self.settings.thousands = c;
                }
                if let Some(c) = single_char(raw(5)) {
                    self.settings.decimal = c;
                }
                if self.settings.thousands == self.settings.decimal {
                    self.settings.thousands = '\0';
                }
            }
            11 => {
                self.hours_per_day = self
                    .settings
                    .number(field(4))
                    .filter(|h| *h > 0.0 && *h <= 24.0);
                self.hours_per_week = self
                    .settings
                    .number(field(5))
                    .filter(|h| *h > 0.0 && *h <= 168.0);
            }
            12 => {
                self.settings.date_order = match field(1) {
                    "1" => DateOrder::Dmy,
                    "2" => DateOrder::Ymd,
                    _ => DateOrder::Mdy,
                };
                if let Some(minutes) = field(3).parse::<u32>().ok().filter(|m| *m < 24 * 60) {
                    self.settings.default_start = minutes;
                }
                if let Some(c) = single_char(field(4)) {
                    self.settings.date_sep = c;
                }
                if let Some(c) = single_char(field(5)) {
                    self.settings.time_sep = c;
                }
                if !field(6).is_empty() {
                    self.settings.am = field(6).to_string();
                }
                if !field(7).is_empty() {
                    self.settings.pm = field(7).to_string();
                }
            }
            20 => self.open_calendar(field(1), fields, None),
            55 => {
                // A resource calendar: of the last resource, derived from the
                // base calendar field 1 names.
                if self.last_row_is_task || self.resources.is_empty() {
                    self.open_calendar = None;
                    return;
                }
                let resource = self.resources.len() - 1;
                let name = self.resources[resource].name.clone();
                self.open_calendar(&name, fields, Some(field(1).to_string()));
                if let Some(draft) = &self.open_calendar {
                    self.resources[resource].calendar_uid = Some(self.calendars[draft.index].uid);
                }
            }
            25 | 56 => self.day_hours(fields),
            26 | 57 => self.exception(fields),
            30 => {
                self.title = field(1).to_string();
                if !field(4).is_empty() {
                    self.calendar_name = Some(field(4).to_string());
                }
                self.start = self.settings.date(field(5));
            }
            41 => self.resource_fields = ids(fields),
            61 => self.task_fields = ids(fields),
            50 => {
                self.open_calendar = None;
                self.last_row_is_task = false;
                self.resource(fields);
            }
            51 => {
                if !self.last_row_is_task {
                    if let Some(r) = self.resources.last_mut() {
                        r.notes = Some(crate::text::normalize_newlines(field(1)));
                    }
                }
            }
            70 => {
                self.open_calendar = None;
                self.last_row_is_task = true;
                self.task(fields);
            }
            71 => {
                if self.last_row_is_task {
                    if let Some(t) = self.tasks.last_mut() {
                        t.task.notes = Some(crate::text::normalize_newlines(field(1)));
                    }
                }
            }
            75 if self.last_row_is_task && !self.tasks.is_empty() => {
                self.assignments.push(RawAssignment {
                    task: self.tasks.len() - 1,
                    fields: fields.to_vec(),
                });
            }
            _ => {}
        }
    }

    /// Start a calendar (record 20, or 55 with its base's name).
    fn open_calendar(&mut self, name: &str, fields: &[String], base: Option<String>) {
        let mut flags = [if base.is_some() { 2 } else { 1 }; 7];
        for (day, flag) in flags.iter_mut().enumerate() {
            if let Some(f) = fields
                .get(day + 1)
                .and_then(|f| f.trim().parse::<u8>().ok())
            {
                if f <= 2 {
                    *flag = f;
                }
            }
        }
        let standard = Calendar::standard_week();
        let derived = base.is_some();
        let week: [Option<DayWorking>; 7] = std::array::from_fn(|day| match flags[day] {
            0 => Some(DayWorking::default()),
            1 => Some(DayWorking {
                times: Calendar::standard_week()[1].times.clone(),
            }),
            _ if derived => None,
            _ => Some(standard[day].clone()),
        });
        let uid = self.calendars.iter().map(|c| c.uid).max().unwrap_or(0) + 1;
        let index = self.calendars.len();
        self.calendars.push(Calendar {
            uid,
            name: name.to_string(),
            base_calendar_uid: None,
            is_baseline_calendar: false,
            week,
            exceptions: Vec::new(),
            work_weeks: Vec::new(),
        });
        if let Some(base) = base {
            self.calendar_bases.push((index, base));
        }
        self.open_calendar = Some(CalendarDraft { index, flags });
    }

    /// Record 25 or 56: one day's working hours.
    fn day_hours(&mut self, fields: &[String]) {
        let Some(draft) = &self.open_calendar else {
            return;
        };
        let Some(day) = fields
            .first()
            .and_then(|d| d.trim().parse::<usize>().ok())
            .filter(|d| (1..=7).contains(d))
            .map(|d| d - 1)
        else {
            return;
        };
        let times = self.working_times(&fields[1..]);
        // A non-working day stays non-working; a stated day without hours
        // keeps what its flag gave it.
        if draft.flags[day] == 0 || times.is_empty() {
            return;
        }
        self.calendars[draft.index].week[day] = Some(DayWorking { times });
    }

    /// Up to three from/to pairs; a pair ending at midnight ends at 24:00.
    fn working_times(&self, fields: &[String]) -> Vec<WorkingTime> {
        let mut times = Vec::new();
        for pair in fields.chunks(2).take(3) {
            let [from, to] = pair else {
                break;
            };
            let (Some(from), Some(to)) = (self.settings.time(from), self.settings.time(to)) else {
                continue;
            };
            let to = if to == 0 { 24 * 60 } else { to };
            if from < to
                && times
                    .last()
                    .is_none_or(|last: &WorkingTime| last.to <= from)
            {
                times.push(WorkingTime { from, to });
            }
        }
        times
    }

    /// Record 26 or 57: an exception from one date to another, working with
    /// the hours given or not working.
    fn exception(&mut self, fields: &[String]) {
        let Some(draft) = &self.open_calendar else {
            return;
        };
        let field = |i: usize| fields.get(i).map(|f| f.trim()).unwrap_or("");
        let Some((from, _)) = self.settings.date(field(0)) else {
            return;
        };
        let to = self
            .settings
            .date(field(1))
            .map_or(from, |(to, _)| to)
            .max(from);
        let working = field(2) == "1";
        let mut day = DayWorking::default();
        if working {
            day.times = self.working_times(fields.get(3..).unwrap_or(&[]));
            if day.times.is_empty() {
                day.times = Calendar::standard_week()[1].times.clone();
            }
        }
        let to = to.add_minutes(23 * 60 + 59);
        self.calendars[draft.index]
            .exceptions
            .push(CalendarException::date_range(from, to, day));
    }

    /// Record 50: a resource, read by the ids of the last record 41.
    fn resource(&mut self, fields: &[String]) {
        let mut r = Resource {
            max_units: 1.0,
            ..Resource::default()
        };
        let mut uid = None;
        for (&id, value) in self.resource_fields.iter().zip(fields) {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            match id {
                1 => r.name = value.to_string(),
                2 => r.initials = Some(value.to_string()),
                3 => r.group = Some(value.to_string()),
                4 => r.code = Some(value.to_string()),
                11 => r.email_address = Some(value.to_string()),
                40 => r.id = value.parse().unwrap_or(0),
                41 => {
                    if let Some(units) = self.units(value) {
                        r.max_units = units;
                    }
                }
                42 => {
                    if let Some((rate, format)) = self.rate(value) {
                        r.standard_rate = Some(rate);
                        r.standard_rate_format = Some(format);
                    }
                }
                43 => {
                    if let Some((rate, format)) = self.rate(value) {
                        r.overtime_rate = Some(rate);
                        r.overtime_rate_format = Some(format);
                    }
                }
                44 => r.cost_per_use = self.settings.money(value),
                45 => r.accrue_at = accrue_at(value),
                49 => uid = value.parse().ok(),
                _ => {}
            }
        }
        self.resources.push(r);
        self.resource_uids.push(uid);
    }

    /// Units as the file writes them, a fraction (`1` is 100%), or a
    /// percentage with its sign (`50%`).
    fn units(&self, text: &str) -> Option<f64> {
        let value = match text.trim().strip_suffix('%') {
            Some(percent) => self.settings.number(percent)? / 100.0,
            None => self.settings.number(text)?,
        };
        (value >= 0.0).then_some(value)
    }

    /// A rate (`$400.00/d`) as MSPDI holds it: per hour, with the unit it is
    /// shown in (MSPDI's rate format: 1 m, 2 h, 3 d, 4 w, 5 mo, 7 y).
    fn rate(&self, text: &str) -> Option<(Rate, u8)> {
        let (amount, unit) = text.trim().rsplit_once('/').unwrap_or((text, "h"));
        let value = self.settings.money(amount)?.to_f64()?;
        let hours_per_day = self.hours_per_day.unwrap_or(8.0);
        let hours_per_week = self.hours_per_week.unwrap_or(40.0);
        let (per_hour, format) = match unit.trim().to_ascii_lowercase().as_str() {
            "m" | "min" => (value * 60.0, 1),
            "h" | "hr" => (value, 2),
            "d" | "day" => (value / hours_per_day, 3),
            "w" | "wk" => (value / hours_per_week, 4),
            "mo" | "mon" => (value / (20.0 * hours_per_day), 5),
            "y" | "yr" => (value / (52.0 * hours_per_week), 7),
            _ => return None,
        };
        Some((Rate::from_f64(round_cents(per_hour))?, format))
    }

    /// Record 70: a task, read by the ids of the last record 61.
    fn task(&mut self, fields: &[String]) {
        let mut raw = RawTask::default();
        for (&id, value) in self.task_fields.iter().zip(fields) {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            let t = &mut raw.task;
            match id {
                1 => t.name = value.to_string(),
                2 => t.wbs = Some(value.to_string()),
                3 => {
                    if let Ok(level) = value.parse::<u32>() {
                        t.outline_level = level.min(100);
                        raw.has_outline_level = true;
                    }
                }
                15 => t.contact = Some(value.to_string()),
                40 => {
                    if let Some(d) = self.duration(value) {
                        t.duration_min = d.minutes;
                        t.duration_format = Some(d.format.code() as u8).filter(|&f| f != 7);
                        t.estimated = d.format.estimated().then_some(true);
                    }
                }
                42 => t.actual_duration_min = self.duration(value).map(|d| d.minutes),
                43 => t.remaining_duration_min = self.duration(value).map(|d| d.minutes),
                44 => t.percent_complete = percent(value),
                50 => raw.stored_start = self.settings.date(value),
                51 => raw.stored_finish = self.settings.date(value),
                58 => raw.actual_start = self.settings.date(value),
                59 => raw.actual_finish = self.settings.date(value),
                68 => raw.constraint_date = self.settings.date(value),
                70 => raw.predecessors = value.to_string(),
                80 => {
                    if yes_no(value) == Some(true) {
                        t.task_type = Some(TaskType::FixedDuration);
                    }
                }
                81 => raw.milestone = yes_no(value),
                84 => t.rollup = yes_no(value),
                90 => raw.id = value.parse().ok(),
                91 => t.constraint = constraint_type(value).unwrap_or_default(),
                92 => {
                    if let Some(d) = self.duration(value) {
                        t.leveling_delay = d.minutes.checked_mul(10);
                        t.leveling_delay_format = Some(d.format.code() as i32);
                    }
                }
                95 => t.priority = priority(value),
                98 => raw.uid = value.parse().ok(),
                123 => t.hide_bar = yes_no(value),
                125 => {
                    t.create_date = self
                        .settings
                        .date(value)
                        .map(|(d, time)| d.add_minutes(i64::from(time.unwrap_or(0))));
                }
                150 => raw.stop = self.settings.date(value),
                152 => raw.resume = self.settings.date(value),
                _ => {}
            }
        }
        self.tasks.push(raw);
    }

    /// A duration or lag-like delay: `2d`, `16h`, `1440m`, `1w`, `1mo`,
    /// elapsed `2ed`, estimated `2d?`; a bare number is days.
    fn duration(&self, text: &str) -> Option<Duration> {
        let t = text.trim().to_ascii_lowercase();
        let (t, estimated) = match t.strip_suffix('?') {
            Some(rest) => (rest.trim_end().to_string(), true),
            None => (t, false),
        };
        let (number, unit, elapsed) = [
            ("emo", LagUnit::Month, true),
            ("em", LagUnit::Minute, true),
            ("eh", LagUnit::Hour, true),
            ("ed", LagUnit::Day, true),
            ("ew", LagUnit::Week, true),
            ("mo", LagUnit::Month, false),
            ("m", LagUnit::Minute, false),
            ("h", LagUnit::Hour, false),
            ("d", LagUnit::Day, false),
            ("w", LagUnit::Week, false),
        ]
        .into_iter()
        .find_map(|(suffix, unit, elapsed)| {
            t.strip_suffix(suffix)
                .map(|n| (n.trim().to_string(), unit, elapsed))
        })
        .unwrap_or((t.clone(), LagUnit::Day, false));
        let value = self.settings.number(&number)?;
        let per = if elapsed {
            match unit {
                LagUnit::Minute => 1.0,
                LagUnit::Hour => 60.0,
                LagUnit::Day => 1440.0,
                LagUnit::Week => 7.0 * 1440.0,
                _ => 30.0 * 1440.0,
            }
        } else {
            let hours_per_day = self.hours_per_day.unwrap_or(8.0);
            match unit {
                LagUnit::Minute => 1.0,
                LagUnit::Hour => 60.0,
                LagUnit::Day => hours_per_day * 60.0,
                LagUnit::Week => self.hours_per_week.unwrap_or(hours_per_day * 5.0) * 60.0,
                _ => 20.0 * hours_per_day * 60.0,
            }
        };
        let minutes = (value * per).round();
        if !(minutes.is_finite() && minutes >= 0.0 && minutes < i64::MAX as f64 / 10.0) {
            return None;
        }
        Some(Duration {
            minutes: minutes as i64,
            format: LagFormat::new(unit, elapsed, estimated)?,
        })
    }

    /// Record 75's fields: resource ID, units, work, baseline work, actual
    /// work, overtime work, cost, baseline cost, actual cost, start, finish,
    /// delay, resource UID (as MPXJ writes and reads them).
    fn assignment(&self, raw: &RawAssignment) -> ParsedAssignment {
        let field = |i: usize| raw.fields.get(i - 1).map(|f| f.trim()).unwrap_or("");
        let minutes = |i: usize| self.duration(field(i)).map(|d| d.minutes);
        ParsedAssignment {
            task: raw.task,
            resource_id: field(1).parse().ok(),
            resource_uid: field(13).parse().ok(),
            assignment: Assignment {
                units: self.units(field(2)).unwrap_or(1.0),
                work_min: minutes(3).unwrap_or(0),
                actual_work_min: minutes(5),
                overtime_work_min: minutes(6),
                cost: self.settings.money(field(7)),
                actual_cost: self.settings.money(field(9)),
                delay: minutes(12).and_then(|m| m.checked_mul(10)),
                ..Assignment::default()
            },
        }
    }

    /// The project the records described.
    fn finish(self) -> Result<Project, String> {
        if self.tasks.is_empty() {
            return Err("not an MPX project: no task records".into());
        }
        let assignments: Vec<ParsedAssignment> = self
            .assignments
            .iter()
            .map(|raw| self.assignment(raw))
            .collect();
        let mut proj = Project {
            calendars: self.calendars,
            ..Project::default()
        };
        proj.title = self.title;
        proj.name = proj.title.clone();
        if let Some(h) = self.hours_per_day {
            proj.hours_per_day = h;
        }
        proj.hours_per_week = self.hours_per_week.unwrap_or(proj.hours_per_day * 5.0);
        // Resource calendars derive from the base calendar they name, by
        // name among the base calendars (record 20), else the first one.
        let bases: Vec<(String, i32)> = proj
            .calendars
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.calendar_bases.iter().any(|(d, _)| d == i))
            .map(|(_, c)| (c.name.clone(), c.uid))
            .collect();
        for (index, base) in &self.calendar_bases {
            proj.calendars[*index].base_calendar_uid = bases
                .iter()
                .find(|(name, _)| name == base)
                .or(bases.first())
                .map(|(_, uid)| *uid);
        }
        if proj.calendars.is_empty() {
            proj.calendars.push(Calendar::standard(1));
        }
        proj.default_calendar_uid = self
            .calendar_name
            .as_deref()
            .and_then(|name| bases.iter().find(|(n, _)| n == name))
            .or(bases.first())
            .map_or(proj.calendars[0].uid, |(_, uid)| *uid);
        proj.start_date = self.start.map(|(day, time)| {
            day.add_minutes(i64::from(time.unwrap_or(self.settings.default_start)))
        });

        // Resources: unique UIDs, the file's where it gave them.
        let mut used: std::collections::HashSet<i32> = std::collections::HashSet::new();
        let mut resources = self.resources;
        let mut next = self
            .resource_uids
            .iter()
            .flatten()
            .copied()
            .max()
            .unwrap_or(0);
        for (r, uid) in resources.iter_mut().zip(&self.resource_uids) {
            r.uid = match uid {
                Some(uid) if used.insert(*uid) => *uid,
                _ => {
                    next = next.saturating_add(1);
                    used.insert(next);
                    next
                }
            };
        }
        proj.resources = resources;

        // Tasks: by ID, stably; a row without an ID keeps its place.
        let mut tasks = self.tasks;
        let mut order: Vec<usize> = (0..tasks.len()).collect();
        let mut last_id = i64::MIN;
        let keys: Vec<i64> = tasks
            .iter()
            .map(|t| {
                if let Some(id) = t.id {
                    last_id = i64::from(id);
                }
                last_id
            })
            .collect();
        order.sort_by_key(|&i| keys[i]);
        let mut used: std::collections::HashSet<i32> = std::collections::HashSet::new();
        let mut next = tasks.iter().filter_map(|t| t.uid).max().unwrap_or(0);
        let mut uids = vec![0; tasks.len()];
        for (i, raw) in tasks.iter().enumerate() {
            uids[i] = match raw.uid {
                Some(uid) if used.insert(uid) => uid,
                _ => {
                    next = next.saturating_add(1);
                    used.insert(next);
                    next
                }
            };
        }
        let mut position = vec![0; tasks.len()];
        for (row, &i) in order.iter().enumerate() {
            position[i] = row;
        }
        proj.tasks = order
            .iter()
            .enumerate()
            .map(|(row, &i)| {
                let raw = &mut tasks[i];
                let mut t = std::mem::take(&mut raw.task);
                t.uid = uids[i];
                t.id = raw.id.unwrap_or(row as i32 + 1);
                t.is_null = t.name.is_empty() && !raw.has_outline_level;
                if t.is_null {
                    t.outline_level = 0;
                    t.wbs = None;
                } else {
                    t.outline_level = t.outline_level.max(1);
                    t.milestone = raw.milestone.unwrap_or(t.duration_min == 0);
                }
                t
            })
            .collect();
        for i in 0..proj.tasks.len() {
            let summary =
                !proj.tasks[i].is_null && crate::model::is_outline_summary_in(&proj.tasks, i);
            proj.tasks[i].summary = summary;
        }

        // Dates on the task's calendar, now that the calendars resolve.
        for (row, &i) in order.iter().enumerate() {
            let raw = &tasks[i];
            let task = &proj.tasks[row];
            let start = |d: Option<(DateTime, Option<u32>)>| {
                d.map(|(day, time)| match time {
                    Some(m) => day.add_minutes(i64::from(m)),
                    None => day_start(&proj, task, day),
                })
            };
            let end = |d: Option<(DateTime, Option<u32>)>| {
                d.map(|(day, time)| match time {
                    Some(m) => day.add_minutes(i64::from(m)),
                    None => day_end(&proj, task, day),
                })
            };
            let stored_start = start(raw.stored_start);
            let stored_finish = if task.milestone {
                stored_start.or_else(|| start(raw.stored_finish))
            } else {
                end(raw.stored_finish)
            };
            use ConstraintType::*;
            let constraint_date = match task.constraint {
                AsSoonAsPossible | AsLateAsPossible => None,
                MustFinishOn | FinishNoEarlierThan | FinishNoLaterThan => end(raw.constraint_date),
                MustStartOn | StartNoEarlierThan | StartNoLaterThan => start(raw.constraint_date),
            };
            let actual_start = start(raw.actual_start);
            let actual_finish = end(raw.actual_finish);
            let stop = end(raw.stop);
            // A resume on the stop's own day is the stop instant: Project
            // writes Resume = Stop when nothing is left to resume.
            let resume = match (raw.resume, stop) {
                (Some((day, None)), Some(stop)) if day.day_number() == stop.day_number() => {
                    Some(stop)
                }
                (resume, _) => start(resume),
            };
            let t = &mut proj.tasks[row];
            t.stored_start = stored_start;
            t.stored_finish = stored_finish;
            t.constraint_date = constraint_date;
            t.actual_start = actual_start;
            t.actual_finish = actual_finish;
            t.stop = stop;
            t.resume = resume;
        }

        // Links: the file names tasks by ID; each entry reads on its own, so
        // one that names no task drops only itself.
        for (row, &i) in order.iter().enumerate() {
            let text = &tasks[i].predecessors;
            if text.is_empty() {
                continue;
            }
            let mut links = Vec::new();
            for entry in text.split(',') {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                let Ok(parsed) = parse_task_predecessors(entry, &Task::default(), &proj) else {
                    continue;
                };
                for link in parsed {
                    if link.uid != proj.tasks[row].uid
                        && !links
                            .iter()
                            .any(|l: &crate::model::Predecessor| l.uid == link.uid)
                    {
                        links.push(link);
                    }
                }
            }
            proj.tasks[row].predecessors = links;
        }

        // Assignments: the task before the row, the resource by UID (field
        // 13), else by ID (field 1).
        for (n, a) in assignments.into_iter().enumerate() {
            let by_uid = a
                .resource_uid
                .and_then(|uid| self.resource_uids.iter().position(|u| *u == Some(uid)))
                .map(|index| proj.resources[index].uid);
            let by_id = a
                .resource_id
                .and_then(|id| proj.resources.iter().find(|r| r.id == id))
                .map(|r| r.uid);
            let Some(resource_uid) = by_uid.or(by_id) else {
                continue;
            };
            let task = &proj.tasks[position[a.task]];
            if task.is_null {
                continue;
            }
            proj.assignments.push(Assignment {
                uid: n as i32 + 1,
                task_uid: task.uid,
                resource_uid,
                ..a.assignment
            });
        }

        if let Some(error) = crate::schedule::calendar_error(&proj) {
            return Err(error);
        }
        Ok(proj)
    }
}

/// A record 75 read, before its resource resolves.
struct ParsedAssignment {
    task: usize,
    resource_id: Option<i32>,
    resource_uid: Option<i32>,
    assignment: Assignment,
}

/// A duration and the format it is written in.
struct Duration {
    minutes: i64,
    format: LagFormat,
}

/// Two decimals, as Project shows money.
fn round_cents(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Field ids of a record 41 or 61.
fn ids(fields: &[String]) -> Vec<u32> {
    fields
        .iter()
        .map(|f| f.trim().parse().unwrap_or(0))
        .collect()
}

/// `Yes`/`No` (also `1`/`0`, `True`/`False`).
fn yes_no(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "yes" | "1" | "true" => Some(true),
        "no" | "0" | "false" => Some(false),
        _ => None,
    }
}

/// `50%` or `50` as a whole percentage, 0 to 100.
fn percent(text: &str) -> Option<u8> {
    let t = text.trim();
    let t = t.strip_suffix('%').unwrap_or(t).trim();
    let value: f64 = t.parse().ok()?;
    (0.0..=100.0).contains(&value).then(|| value.round() as u8)
}

/// Project's constraint type names, in English.
fn constraint_type(text: &str) -> Option<ConstraintType> {
    use ConstraintType::*;
    let name = text.trim().to_ascii_lowercase();
    Some(match name.as_str() {
        "as soon as possible" => AsSoonAsPossible,
        "as late as possible" => AsLateAsPossible,
        "must start on" => MustStartOn,
        "must finish on" => MustFinishOn,
        "start no earlier than" => StartNoEarlierThan,
        "start no later than" => StartNoLaterThan,
        "finish no earlier than" => FinishNoEarlierThan,
        "finish no later than" => FinishNoLaterThan,
        _ => return name.parse().ok().and_then(ConstraintType::from_code),
    })
}

/// Project's priority names, as MPXJ maps them (`Medium` is 500,
/// `Do Not Level` 1000), or a number from 0 to 1000.
fn priority(text: &str) -> Option<i32> {
    let name = text.trim().to_ascii_lowercase();
    Some(match name.as_str() {
        "lowest" => 100,
        "very low" => 200,
        "lower" => 300,
        "low" => 400,
        "medium" => 500,
        "high" => 600,
        "higher" => 700,
        "very high" => 800,
        "highest" => 900,
        "do not level" => 1000,
        _ => return name.parse().ok().filter(|n| (0..=1000).contains(n)),
    })
}

/// Project's Accrue At names.
fn accrue_at(text: &str) -> Option<AccrueAt> {
    match text.trim().to_ascii_lowercase().as_str() {
        "start" => Some(AccrueAt::Start),
        "end" => Some(AccrueAt::End),
        "prorated" => Some(AccrueAt::Prorated),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
