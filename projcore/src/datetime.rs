//! A minimal civil date-time, `std`-only.
//!
//! Project schedules are wall-clock: "task starts 2026-03-02 08:00" with no
//! timezone and no leap seconds. We store an absolute instant as **minutes
//! since 1970-01-01 00:00** (a signed `i64`, so dates before the epoch work
//! too) and convert to/from calendar fields with the standard proleptic
//! Gregorian day-number algorithms (Howard Hinnant's `days_from_civil`).
//!
//! MS Project's MSPDI serializes times as `yyyy-mm-ddThh:mm:ss` with no zone
//! suffix — exactly this wall-clock model — so parse/format are lossless for
//! the values Project emits.

/// An absolute wall-clock instant at whole-minute resolution.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct DateTime {
    /// Minutes since 1970-01-01 00:00 (may be negative).
    min: i64,
}

/// Calendar breakdown of a [`DateTime`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Parts {
    pub year: i64,
    pub month: u32, // 1..=12
    pub day: u32,   // 1..=31
    pub hour: u32,  // 0..=23
    pub minute: u32, // 0..=59
}

/// Days from 1970-01-01 to the given proleptic-Gregorian date.
/// Valid for any date; `month` in 1..=12, `day` in 1..=31.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    // Hinnant's algorithm. Shift so March is month 0 (leap day lands last).
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = m as i64;
    let d = d as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`]: day-number → (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl DateTime {
    pub fn from_minutes(min: i64) -> Self {
        DateTime { min }
    }

    pub fn minutes(self) -> i64 {
        self.min
    }

    pub fn from_ymd_hm(year: i64, month: u32, day: u32, hour: u32, minute: u32) -> Self {
        let days = days_from_civil(year, month, day);
        DateTime { min: days * 1440 + (hour as i64) * 60 + minute as i64 }
    }

    pub fn parts(self) -> Parts {
        let days = self.min.div_euclid(1440);
        let tod = self.min.rem_euclid(1440);
        let (year, month, day) = civil_from_days(days);
        Parts { year, month, day, hour: (tod / 60) as u32, minute: (tod % 60) as u32 }
    }

    /// Whole-day number since the epoch (floor toward negative infinity), i.e.
    /// the calendar date this instant falls on, independent of time of day.
    pub fn day_number(self) -> i64 {
        self.min.div_euclid(1440)
    }

    /// Minute of the day, 0..=1439.
    pub fn minute_of_day(self) -> u32 {
        self.min.rem_euclid(1440) as u32
    }

    /// Day of week, Sunday=0 .. Saturday=6. (1970-01-01 was a Thursday.)
    pub fn weekday(self) -> u32 {
        (self.day_number() + 4).rem_euclid(7) as u32
    }

    /// This instant moved to `min_of_day` on the same calendar date.
    pub fn with_minute_of_day(self, min_of_day: u32) -> Self {
        DateTime { min: self.day_number() * 1440 + min_of_day as i64 }
    }

    /// Midnight (00:00) at the start of this instant's calendar date.
    pub fn start_of_day(self) -> Self {
        DateTime { min: self.day_number() * 1440 }
    }

    pub fn add_minutes(self, m: i64) -> Self {
        DateTime { min: self.min + m }
    }

    pub fn add_days(self, d: i64) -> Self {
        DateTime { min: self.min + d * 1440 }
    }

    /// Parse MSPDI's `yyyy-mm-ddThh:mm:ss` (or a bare `yyyy-mm-dd`). Returns
    /// `None` on any structural surprise rather than guessing.
    pub fn parse_mspdi(s: &str) -> Option<Self> {
        let s = s.trim();
        let (date, time) = match s.split_once('T') {
            Some((d, t)) => (d, Some(t)),
            None => (s, None),
        };
        let mut dp = date.split('-');
        let year: i64 = dp.next()?.parse().ok()?;
        let month: u32 = dp.next()?.parse().ok()?;
        let day: u32 = dp.next()?.parse().ok()?;
        if dp.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        let (hour, minute) = match time {
            None => (0, 0),
            Some(t) => {
                let mut tp = t.split(':');
                let h: u32 = tp.next()?.parse().ok()?;
                let mi: u32 = tp.next()?.parse().ok()?;
                // seconds field, if present, is ignored (minute resolution)
                (h, mi)
            }
        };
        if hour > 23 || minute > 59 {
            return None;
        }
        Some(DateTime::from_ymd_hm(year, month, day, hour, minute))
    }

    /// Format as MSPDI `yyyy-mm-ddThh:mm:ss` (seconds always `00`).
    pub fn to_mspdi(self) -> String {
        let p = self.parts();
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:00",
            p.year, p.month, p.day, p.hour, p.minute
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn known_dates() {
        // 2000-01-01 is 10957 days after the epoch.
        assert_eq!(days_from_civil(2000, 1, 1), 10957);
        // A leap day round-trips.
        let d = days_from_civil(2024, 2, 29);
        assert_eq!(civil_from_days(d), (2024, 2, 29));
    }

    #[test]
    fn weekday_known() {
        // 1970-01-01 Thursday(4); 2026-03-02 is a Monday(1).
        assert_eq!(DateTime::from_ymd_hm(1970, 1, 1, 0, 0).weekday(), 4);
        assert_eq!(DateTime::from_ymd_hm(2026, 3, 2, 8, 0).weekday(), 1);
    }

    #[test]
    fn mspdi_parse_format() {
        let dt = DateTime::parse_mspdi("2026-03-01T08:00:00").unwrap();
        assert_eq!(dt.to_mspdi(), "2026-03-01T08:00:00");
        assert_eq!(
            dt.parts(),
            Parts { year: 2026, month: 3, day: 1, hour: 8, minute: 0 }
        );
        // bare date defaults to midnight
        assert_eq!(
            DateTime::parse_mspdi("2026-03-01").unwrap().minute_of_day(),
            0
        );
        assert!(DateTime::parse_mspdi("not-a-date").is_none());
        assert!(DateTime::parse_mspdi("2026-13-01T00:00:00").is_none());
    }

    #[test]
    fn arithmetic() {
        let dt = DateTime::from_ymd_hm(2026, 3, 2, 8, 0);
        assert_eq!(dt.add_days(1).parts().day, 3);
        assert_eq!(dt.add_minutes(90).parts(), Parts { year: 2026, month: 3, day: 2, hour: 9, minute: 30 });
        assert_eq!(dt.with_minute_of_day(13 * 60).minute_of_day(), 780);
    }
}
