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
//! default stays). Only a file without the header, without a task record,
//! or whose calendar has no working time (as the MSPDI reader refuses it)
//! is refused.
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
//! scheduler's.
//!
//! Enumerated values (constraint type, priority, accrue at, yes and no),
//! duration and rate units and link types are read in the words of one
//! locale for the whole file, chosen from what its rows hold
//! (`locale::choose`): MPXJ's English, German, French, Italian,
//! Portuguese, Swedish, Spanish and Russian (#1805). A word the locale does
//! not know is read as English, as is a unit whose locale suffix leaves no
//! number (German `2em`), else the model's default stays. A file whose
//! header names the code page `RU` is Russian and, unless it is UTF-8,
//! Windows-1251; Chinese (`ZH`) is read as an English file. English units are
//! MPXJ's (`m h d w mon y`, elapsed with a leading `e`) and Project's `mo`.
//!
//! Dates are numeric, or spell the month in English (`March 05 2026`,
//! `05 Mar '26`, MPXJ's date formats 2, 3, 5 and 10), in the file's date
//! order, optionally after a weekday (`Mon 03/02/26`, `Mo. 02.03.26`, `mån
//! 26-03-02`). Localised month names are not read.
//!
//! The rules were checked against MPXJ 16.8.0's writer and reader on the
//! `corpus/mpx` files (`corpus/mpx/README.md`); there is no Project oracle.

mod locale;

use crate::datetime::DateTime;
use crate::editor::{day_end, day_start};
use crate::model::{
    Assignment, Calendar, CalendarException, ConstraintType, DayWorking, LagFormat, LagUnit,
    Project, Rate, Resource, Task, TaskType, WorkingTime,
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
    let records = records(&text, delimiter);
    let mut reader = Reader {
        delimiter,
        locale: Some(locale::choose(&records, delimiter)),
        ..Reader::default()
    };
    for record in records.into_iter().skip(1) {
        reader.record(&record);
    }
    reader.finish()
}

/// The text of the file: UTF-8 when it is valid UTF-8 (a byte order mark
/// dropped), else the header's code page ([`header_code_page`]).
fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            let page = header_code_page(bytes);
            // The bytes a code page leaves undefined are their C1 controls,
            // as in every other legacy import (`opccore::xml`).
            bytes
                .iter()
                .map(|&b| opccore::codepage::decode(page, b).unwrap_or(char::from(b)))
                .collect()
        }
    }
}

/// The Windows code page of the header's fourth field, read as ASCII before
/// the file is decoded: `RU` is 1251, as MPXJ writes Russian; `ANSI` and
/// anything else 1252, the code page Project writes.
fn header_code_page(bytes: &[u8]) -> i32 {
    let line = bytes
        .split(|&b| b == b'\r' || b == b'\n')
        .next()
        .unwrap_or(&[]);
    let Some(&delimiter) = line.get(3) else {
        return 1252;
    };
    match line.split(|&b| b == delimiter).nth(3) {
        Some(field) if field.trim_ascii().eq_ignore_ascii_case(b"RU") => 1251,
        _ => 1252,
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
        self.point_decimals(text.trim())
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
    }

    /// `text` with record 10's thousands separator dropped and its decimal
    /// separator a point, as [`Settings::number`] and the cell grammar read
    /// numbers: `1.234,5` in a decimal-comma file is `1234.5`. Of the
    /// links, only a `%` lag entry goes through it ([`Reader::link_entry`]).
    fn point_decimals(&self, text: &str) -> String {
        text.chars()
            .filter(|&c| c != self.thousands || c == self.decimal)
            .map(|c| if c == self.decimal { '.' } else { c })
            .collect()
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
    /// The time is `None` for a bare date. The month may be an English name
    /// (`March 05 2026`, `05 Mar '26`), and a weekday may come first.
    fn date(&self, text: &str) -> Option<(DateTime, Option<u32>)> {
        let text = text.trim();
        // A leading weekday (`Mon 03/02/26`, `Thu Mar 05 '26`, German
        // `Mo. 02.03.26`, Swedish `mån 26-03-02`): a word, maybe ending in a
        // dot, that is not an English month (MDY `Mar 05 '26`), or is one
        // before a numeric date (Spanish or Italian `mar`, Tuesday:
        // `mar 03/03/26`).
        let numeric_date = |word: &str| {
            !word.contains(char::is_alphabetic)
                && word.split([self.date_sep, '/', '-', '.']).count() == 3
        };
        let text = match text.split_once(' ') {
            Some((day, rest))
                if is_word(day)
                    && (month_number(day).is_none()
                        || numeric_date(rest.trim_start().split(' ').next().unwrap_or(""))) =>
            {
                rest.trim_start()
            }
            _ => text,
        };
        // A numeric date is one word of digits and separators; its time may
        // hold letters (`am`).
        let first = text.split(' ').next().unwrap_or("");
        let numeric =
            !first.contains(char::is_alphabetic) && first.contains([self.date_sep, '/', '-', '.']);
        let (y, m, d, time) = if !numeric && text.contains(char::is_alphabetic) {
            self.month_name_date(text)?
        } else {
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
            (*y, m.parse().ok()?, d.parse().ok()?, time)
        };
        let mut year: i64 = y.parse().ok()?;
        if y.len() <= 2 {
            // Two-digit years (exception records): a fixed pivot of our own,
            // 00-79 are 2000-2079 and 80-99 are 1980-1999, so a file reads
            // the same on any day. MPXJ's reader uses a window around today
            // instead; both read the corpus's `26` as 2026.
            year += if year < 80 { 2000 } else { 1900 };
        }
        let (month, day): (u32, u32) = (m, d);
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

    /// The year, month, day and time of a date that spells its month in
    /// English (MPXJ's date formats 2, 3, 5 and 10): three words, one of
    /// them the month, the other two the day and the year in the file's date
    /// order with the month taken out (day then year, or year then day for
    /// YMD), a year maybe written `'26`; what follows is the time. The three
    /// may also be joined by a date separator (`05-Mar-2026`).
    fn month_name_date<'a>(&self, text: &'a str) -> Option<(&'a str, u32, u32, Option<&'a str>)> {
        let (date, time) = match text.split_once(' ') {
            Some((first, rest)) if first.split([self.date_sep, '/', '-']).count() == 3 => (
                first.split([self.date_sep, '/', '-']).collect::<Vec<_>>(),
                Some(rest),
            ),
            None if text.split([self.date_sep, '/', '-']).count() == 3 => {
                (text.split([self.date_sep, '/', '-']).collect(), None)
            }
            _ => {
                let mut words = text.splitn(4, ' ').filter(|w| !w.is_empty());
                let date: Vec<&str> = words.by_ref().take(3).collect();
                (date, words.next())
            }
        };
        let [a, b, c] = date.as_slice() else {
            return None;
        };
        let at = [a, b, c]
            .iter()
            .position(|w| w.contains(char::is_alphabetic))?;
        let month = month_number(date[at])?;
        let mut rest = date
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != at)
            .map(|(_, w)| *w);
        let (first, second) = (rest.next()?, rest.next()?);
        let (y, d) = match self.date_order {
            DateOrder::Ymd => (first, second),
            DateOrder::Mdy | DateOrder::Dmy => (second, first),
        };
        let y = y.strip_prefix('\'').unwrap_or(y);
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if !(digits(y) && digits(d)) {
            return None;
        }
        Some((y, month, d.parse().ok()?, time.map(str::trim)))
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

/// Letters, maybe ending in one `.`: a weekday's or month's abbreviation.
fn is_word(text: &str) -> bool {
    let text = text.strip_suffix('.').unwrap_or(text);
    !text.is_empty() && text.chars().all(char::is_alphabetic)
}

/// The month (1 to 12) an English month name or its first three letters
/// name, any case, with an optional final `.` (`Sept` too).
fn month_number(text: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    let word = text.strip_suffix('.').unwrap_or(text).to_lowercase();
    if word == "sept" {
        return Some(9);
    }
    MONTHS
        .iter()
        .position(|m| *m == word || (word.chars().count() == 3 && m.starts_with(&word)))
        .map(|i| i as u32 + 1)
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
    /// The header's field delimiter, which also separates the entries of a
    /// Predecessors list (MPXJ joins them with it: `"1;2"` in a `MPX;` file).
    delimiter: char,
    /// The words the file is written in; `None` (a test's bare reader) is
    /// English.
    locale: Option<&'static locale::Locale>,
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
    fn locale(&self) -> &'static locale::Locale {
        self.locale.unwrap_or(&locale::EN)
    }

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
                        r.notes = Some(note(field(1)));
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
                        t.task.notes = Some(note(field(1)));
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
                45 => r.accrue_at = self.locale().accrue_at(value),
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
        let unit = unit.trim().to_lowercase();
        // The locale's working units (`/t` is a German day), else these.
        let unit = match self.locale().rate_unit(&unit) {
            Some(u) if !u.elapsed => match (u.unit, u.year) {
                (_, true) => "y",
                (LagUnit::Minute, _) => "m",
                (LagUnit::Hour, _) => "h",
                (LagUnit::Day, _) => "d",
                (LagUnit::Week, _) => "w",
                _ => "mo",
            },
            _ => unit.as_str(),
        };
        let (per_hour, format) = match unit {
            "m" | "min" => (value * 60.0, 1),
            "h" | "hr" => (value, 2),
            "d" | "day" => (value / hours_per_day, 3),
            "w" | "wk" => (value / hours_per_week, 4),
            "mo" | "mon" => (value / (20.0 * hours_per_day), 5),
            "y" | "yr" => (value / (52.0 * hours_per_week), 7),
            _ => return None,
        };
        // Unrounded: $1.00/d is 0.125 an hour, which prices a day at $1.00.
        Some((Rate::from_f64(per_hour)?, format))
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
                    if self.locale().yes_no(value) == Some(true) {
                        t.task_type = Some(TaskType::FixedDuration);
                    }
                }
                81 => raw.milestone = self.locale().yes_no(value),
                84 => t.rollup = self.locale().yes_no(value),
                90 => raw.id = value.parse().ok(),
                91 => t.constraint = self.locale().constraint_type(value).unwrap_or_default(),
                92 => {
                    if let Some(d) = self.duration(value) {
                        t.leveling_delay = d.minutes.checked_mul(10);
                        t.leveling_delay_format = Some(d.format.code() as i32);
                    }
                }
                95 => t.priority = self.locale().priority(value),
                98 => raw.uid = value.parse().ok(),
                123 => t.hide_bar = self.locale().yes_no(value),
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
    /// elapsed `2ed`, estimated `2d?`, in the file's locale's units
    /// ([`locale::Locale::unit`]: German `2t`, Russian `2день`); a bare
    /// number is days.
    fn duration(&self, text: &str) -> Option<Duration> {
        let t = text.trim().to_lowercase();
        let (t, estimated) = match t.strip_suffix('?') {
            Some(rest) => (rest.trim_end(), true),
            None => (t.as_str(), false),
        };
        // A year has no MSPDI format, so it is kept in days.
        let (
            number,
            locale::Unit {
                unit,
                elapsed,
                year,
            },
        ) = self.locale().unit(t).unwrap_or((
            t,
            locale::Unit {
                unit: LagUnit::Day,
                elapsed: false,
                year: false,
            },
        ));
        let value = self.settings.number(number)?;
        // Elapsed: a month is 30 days and a year 365. Working: a month is
        // Project's default 20 days and a year 52 weeks, as `rate` counts.
        let hours_per_day = self.hours_per_day.unwrap_or(8.0);
        let hours_per_week = self.hours_per_week.unwrap_or(hours_per_day * 5.0);
        let per = match (elapsed, year, unit) {
            (true, true, _) => 365.0 * 1440.0,
            (false, true, _) => 52.0 * hours_per_week * 60.0,
            (_, _, LagUnit::Minute) => 1.0,
            (_, _, LagUnit::Hour) => 60.0,
            (true, _, LagUnit::Day) => 1440.0,
            (true, _, LagUnit::Week) => 7.0 * 1440.0,
            (true, _, _) => 30.0 * 1440.0,
            (false, _, LagUnit::Day) => hours_per_day * 60.0,
            (false, _, LagUnit::Week) => hours_per_week * 60.0,
            (false, _, _) => 20.0 * hours_per_day * 60.0,
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

    /// One Predecessors entry: the text the cell grammar reads (the task ID
    /// and link type, or the whole entry for a `%` lag, its numbers in the
    /// cell's decimals) and, for any other lag, the lag read here by
    /// [`Reader::duration`] on the file's own separators, once, signed.
    /// `None` when the lag does not read: the entry is dropped.
    fn link_entry(&self, entry: &str) -> Option<LinkEntry> {
        // A localised link type (`2EA+1t`) in English first.
        let entry = &self.locale().english_link(entry);
        // The lag starts at the first sign: an ID is digits, a type letters.
        let Some(sign) = entry.find(['+', '-']) else {
            return Some(LinkEntry {
                link: entry.to_string(),
                lag: None,
            });
        };
        let lag = entry[sign + 1..].trim();
        if lag.trim_end_matches('?').trim_end().ends_with('%') {
            return Some(LinkEntry {
                link: self.settings.point_decimals(entry),
                lag: None,
            });
        }
        let d = self.duration(lag)?;
        let minutes = if entry[sign..].starts_with('-') {
            -d.minutes
        } else {
            d.minutes
        };
        Some(LinkEntry {
            link: entry[..sign].trim_end().to_string(),
            lag: Some((minutes, d.format)),
        })
    }

    /// The project the records described.
    fn finish(self) -> Result<Project, String> {
        if self.tasks.is_empty() {
            return Err("not an MPX project: no task records".into());
        }
        // Each task's link entries, split on the delimiter, with their lags
        // read ([`Reader::link_entry`]).
        let link_entries: Vec<Vec<LinkEntry>> = self
            .tasks
            .iter()
            .map(|t| {
                t.predecessors
                    .split(self.delimiter)
                    .map(str::trim)
                    .filter(|e| !e.is_empty())
                    .filter_map(|e| self.link_entry(e))
                    .collect()
            })
            .collect();
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
        // The umbrella row (the first in ID order, stating outline level 0)
        // without a UID of its own is UID 0, the project summary, unless a
        // row claims UID 0.
        let umbrella = order.first().copied().filter(|&i| {
            let raw = &tasks[i];
            raw.has_outline_level
                && raw.task.outline_level == 0
                && raw.uid.is_none()
                && !tasks.iter().any(|t| t.uid == Some(0))
        });
        let mut used: std::collections::HashSet<i32> = std::collections::HashSet::new();
        let mut next = tasks.iter().filter_map(|t| t.uid).max().unwrap_or(0);
        let mut uids = vec![0; tasks.len()];
        for (i, raw) in tasks.iter().enumerate() {
            let uid = if umbrella == Some(i) {
                Some(0)
            } else {
                raw.uid
            };
            uids[i] = match uid {
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
        // A row without an ID takes one no row of the file uses, so the
        // links, which name the file's IDs, stay unambiguous.
        let mut next_id = tasks.iter().filter_map(|t| t.id).max().unwrap_or(0);
        proj.tasks = order
            .iter()
            .map(|&i| {
                let raw = &mut tasks[i];
                let mut t = std::mem::take(&mut raw.task);
                t.uid = uids[i];
                t.id = raw.id.unwrap_or_else(|| {
                    next_id = next_id.saturating_add(1);
                    next_id
                });
                t.is_null = t.name.is_empty() && !raw.has_outline_level;
                if t.is_null {
                    t.outline_level = 0;
                    t.wbs = None;
                } else {
                    // A stated level 0 is Project's umbrella row, the
                    // project summary (MPXJ: "an invisible task with an
                    // outline level as zero"), kept as `read_mspdi` keeps it;
                    // only a row that states no level becomes level 1.
                    if !raw.has_outline_level {
                        t.outline_level = 1;
                    }
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
            if link_entries[i].is_empty() {
                continue;
            }
            let mut links = Vec::new();
            for entry in &link_entries[i] {
                let Ok(parsed) = crate::editor::parse_predecessors(&entry.link, &proj) else {
                    continue;
                };
                for mut link in parsed {
                    if let Some((lag, format)) = entry.lag {
                        link.lag = lag;
                        link.lag_format = format;
                    }
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

/// A Predecessors entry: the text the cell grammar parses and the lag read
/// apart from it (minutes of its kind and the format), if it has one.
struct LinkEntry {
    link: String,
    lag: Option<(i64, LagFormat)>,
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

/// A notes record's text: MPXJ writes a line break inside a note as U+007F.
fn note(text: &str) -> String {
    crate::text::normalize_newlines(&text.replace('\u{7F}', "\n"))
}

/// Field ids of a record 41 or 61.
fn ids(fields: &[String]) -> Vec<u32> {
    fields
        .iter()
        .map(|f| f.trim().parse().unwrap_or(0))
        .collect()
}

/// `50%` or `50` as a whole percentage, 0 to 100.
fn percent(text: &str) -> Option<u8> {
    let t = text.trim();
    let t = t.strip_suffix('%').unwrap_or(t).trim();
    let value: f64 = t.parse().ok()?;
    (0.0..=100.0).contains(&value).then(|| value.round() as u8)
}

#[cfg(test)]
mod tests;
