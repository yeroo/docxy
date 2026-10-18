//! The calendar a date combo drops down (#1536): a month grid with a cursor
//! day. Pure date arithmetic; the dialog owns when it is open and the widget
//! draws it.
use projcore::DateTime;

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// The weekday headings, Sunday first (as Project's calendar).
pub(crate) const WEEKDAYS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

/// An open calendar: the date combo it belongs to (a control index) and its
/// cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Calendar {
    pub control: usize,
    year: i64,
    month: u32,
    day: u32,
}

/// Today on the local clock, as DATE fields read it (#768): the system
/// clock plus the OS's zone offset.
pub(crate) fn today() -> DateTime {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    DateTime::from_minutes(secs.div_euclid(60) + gridcore::clock::local_offset_minutes())
        .start_of_day()
}

fn days_in(year: i64, month: u32) -> u32 {
    let first = DateTime::from_ymd_hm(year, month, 1, 0, 0);
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    (DateTime::from_ymd_hm(ny, nm, 1, 0, 0).day_number() - first.day_number()) as u32
}

impl Calendar {
    /// Open on the date `text` starts with (`YYYY-MM-DD`, a time after a `T`
    /// ignored), else on `today`.
    pub fn open(control: usize, text: &str, today: DateTime) -> Self {
        let day = text
            .trim()
            .split('T')
            .next()
            .and_then(DateTime::parse_mspdi)
            .unwrap_or(today);
        let p = day.parts();
        Self {
            control,
            year: p.year,
            month: p.month,
            day: p.day,
        }
    }

    pub fn year(&self) -> i64 {
        self.year
    }

    pub fn month(&self) -> u32 {
        self.month
    }

    pub fn day(&self) -> u32 {
        self.day
    }

    fn cursor(&self) -> DateTime {
        DateTime::from_ymd_hm(self.year, self.month, self.day, 0, 0)
    }

    fn set(&mut self, at: DateTime) {
        let p = at.parts();
        (self.year, self.month, self.day) = (p.year, p.month, p.day);
    }

    /// The cursor `n` days on (or back), the month following it.
    pub fn move_days(&mut self, n: i64) {
        self.set(self.cursor().add_days(n));
    }

    /// The cursor `n` months on (or back), its day kept or cut to the
    /// month's last.
    pub fn move_months(&mut self, n: i32) {
        let index = self.year * 12 + i64::from(self.month - 1) + i64::from(n);
        let (year, month) = (index.div_euclid(12), index.rem_euclid(12) as u32 + 1);
        (self.year, self.month) = (year, month);
        self.day = self.day.min(days_in(year, month));
    }

    /// Put the cursor on day `day` of the shown month; `false` off the month.
    pub fn pick_day(&mut self, day: u32) -> bool {
        let ok = (1..=days_in(self.year, self.month)).contains(&day);
        if ok {
            self.day = day;
        }
        ok
    }

    /// The month's weeks, Sunday first, `None` outside the month.
    pub fn weeks(&self) -> Vec<[Option<u32>; 7]> {
        let first = DateTime::from_ymd_hm(self.year, self.month, 1, 0, 0).weekday() as usize;
        let mut weeks = vec![[None; 7]];
        for day in 1..=days_in(self.year, self.month) {
            let slot = (first + day as usize - 1) % 7;
            if slot == 0 && day > 1 {
                weeks.push([None; 7]);
            }
            weeks.last_mut().expect("a week")[slot] = Some(day);
        }
        weeks
    }

    /// `October 2026`.
    pub fn title(&self) -> String {
        format!("{} {}", MONTHS[(self.month - 1) as usize], self.year)
    }

    /// The cursor day as a date field's text.
    pub fn text(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Whether `day` of the shown month is today.
    pub fn is_today(&self, day: u32, today: DateTime) -> bool {
        let p = today.parts();
        (p.year, p.month, p.day) == (self.year, self.month, day)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Calendar {
        Calendar::open(0, text, DateTime::from_ymd_hm(2026, 3, 9, 0, 0))
    }

    #[test]
    fn opens_on_the_field_date_else_today() {
        assert_eq!(at("2026-10-18T09:30").text(), "2026-10-18");
        assert_eq!(at("").text(), "2026-03-09");
        assert_eq!(at("soon").text(), "2026-03-09");
    }

    #[test]
    fn the_cursor_crosses_months_and_cuts_the_day() {
        let mut c = at("2026-01-31");
        c.move_months(1);
        assert_eq!(c.text(), "2026-02-28");
        c.move_days(-28);
        assert_eq!(c.text(), "2026-01-31");
        c.move_days(1);
        assert_eq!(c.title(), "February 2026");
        c.move_months(-2);
        assert_eq!(c.text(), "2025-12-01");
        assert!(!c.pick_day(32) && c.pick_day(25));
        assert_eq!(c.text(), "2025-12-25");
    }

    #[test]
    fn the_weeks_start_on_sunday() {
        // 2026-10-01 is a Thursday.
        let w = at("2026-10-18").weeks();
        assert_eq!(w[0], [None, None, None, None, Some(1), Some(2), Some(3)]);
        assert_eq!(w[1][0], Some(4));
        // 2026-10-31 is a Saturday.
        assert_eq!(w.last().unwrap()[6], Some(31));
        assert_eq!(w.iter().flatten().flatten().count(), 31);
    }
}
