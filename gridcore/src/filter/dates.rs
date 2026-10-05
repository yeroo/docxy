//! Date filters: the relative periods of `<dynamicFilter>` (This Week, Year
//! to Date, …), worked out from "today", and the all-years periods (`M3`,
//! `Q1`).

use crate::sheet::{parts_to_serial, serial_to_parts};

/// The `<dynamicFilter type>`s Excel writes.
pub const DYNAMIC_KINDS: &[&str] = &[
    "null",
    "aboveAverage",
    "belowAverage",
    "tomorrow",
    "today",
    "yesterday",
    "nextWeek",
    "thisWeek",
    "lastWeek",
    "nextMonth",
    "thisMonth",
    "lastMonth",
    "nextQuarter",
    "thisQuarter",
    "lastQuarter",
    "nextYear",
    "thisYear",
    "lastYear",
    "yearToDate",
    "Q1",
    "Q2",
    "Q3",
    "Q4",
    "M1",
    "M2",
    "M3",
    "M4",
    "M5",
    "M6",
    "M7",
    "M8",
    "M9",
    "M10",
    "M11",
    "M12",
];

/// The serial of the first day of the month `months` after (`y`, `m`).
fn month_start(y: i64, m: u32, months: i64, date1904: bool) -> f64 {
    let k = y * 12 + (m as i64 - 1) + months;
    parts_to_serial(
        k.div_euclid(12),
        (k.rem_euclid(12) + 1) as u32,
        1,
        0,
        date1904,
    )
}

/// Day of the week of a calendar date, Sunday 0 (Sakamoto).
fn weekday(y: i64, m: u32, d: u32) -> i64 {
    const T: [i64; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    (y + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400) + T[m as usize - 1] + d as i64)
        .rem_euclid(7)
}

/// A relative period's window `[start, end)` as serials, from `today` (a
/// serial in the workbook's date system). Weeks start on Sunday, as Excel's
/// do. `None` for a kind with no window (the averages, `M`n, `Q`n).
pub fn period_window(kind: &str, today: f64, date1904: bool) -> Option<(f64, f64)> {
    let p = serial_to_parts(today, date1904)?;
    let day = today.floor();
    let (y, m) = (p.year, p.month);
    let week = day - weekday(y, m, p.day) as f64;
    let quarter = ((m - 1) / 3) * 3 + 1;
    let year = |k: i64| {
        (
            parts_to_serial(y + k, 1, 1, 0, date1904),
            parts_to_serial(y + k + 1, 1, 1, 0, date1904),
        )
    };
    let month = |k: i64| {
        (
            month_start(y, m, k, date1904),
            month_start(y, m, k + 1, date1904),
        )
    };
    let qtr = |k: i64| {
        (
            month_start(y, quarter, 3 * k, date1904),
            month_start(y, quarter, 3 * k + 3, date1904),
        )
    };
    Some(match kind {
        "today" => (day, day + 1.0),
        "yesterday" => (day - 1.0, day),
        "tomorrow" => (day + 1.0, day + 2.0),
        "thisWeek" => (week, week + 7.0),
        "lastWeek" => (week - 7.0, week),
        "nextWeek" => (week + 7.0, week + 14.0),
        "thisMonth" => month(0),
        "lastMonth" => month(-1),
        "nextMonth" => month(1),
        "thisQuarter" => qtr(0),
        "lastQuarter" => qtr(-1),
        "nextQuarter" => qtr(1),
        "thisYear" => year(0),
        "lastYear" => year(-1),
        "nextYear" => year(1),
        "yearToDate" => (year(0).0, day + 1.0),
        _ => return None,
    })
}

/// The months (1–12) an all-years period matches: `M3` is March, `Q1`
/// January to March. `None` for any other kind.
pub fn period_months(kind: &str) -> Option<std::ops::RangeInclusive<u32>> {
    if let Some(n) = kind.strip_prefix('M').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=12).contains(&n).then_some(n..=n);
    }
    if let Some(q) = kind.strip_prefix('Q').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=4).contains(&q).then_some(q * 3 - 2..=q * 3);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i64, m: u32, day: u32) -> f64 {
        parts_to_serial(y, m, day, 0, false)
    }

    #[test]
    fn periods_around_wednesday_2024_03_13() {
        let today = d(2024, 3, 13) + 0.5; // noon: the time doesn't matter
        let w = |k: &str| period_window(k, today, false).unwrap();
        assert_eq!(w("today"), (d(2024, 3, 13), d(2024, 3, 14)));
        assert_eq!(w("yesterday"), (d(2024, 3, 12), d(2024, 3, 13)));
        assert_eq!(w("tomorrow"), (d(2024, 3, 14), d(2024, 3, 15)));
        // Sunday 10 to Saturday 16 March.
        assert_eq!(w("thisWeek"), (d(2024, 3, 10), d(2024, 3, 17)));
        assert_eq!(w("lastWeek"), (d(2024, 3, 3), d(2024, 3, 10)));
        assert_eq!(w("nextWeek"), (d(2024, 3, 17), d(2024, 3, 24)));
        assert_eq!(w("thisMonth"), (d(2024, 3, 1), d(2024, 4, 1)));
        assert_eq!(w("lastMonth"), (d(2024, 2, 1), d(2024, 3, 1)));
        assert_eq!(w("nextMonth"), (d(2024, 4, 1), d(2024, 5, 1)));
        assert_eq!(w("thisQuarter"), (d(2024, 1, 1), d(2024, 4, 1)));
        assert_eq!(w("lastQuarter"), (d(2023, 10, 1), d(2024, 1, 1)));
        assert_eq!(w("nextQuarter"), (d(2024, 4, 1), d(2024, 7, 1)));
        assert_eq!(w("thisYear"), (d(2024, 1, 1), d(2025, 1, 1)));
        assert_eq!(w("lastYear"), (d(2023, 1, 1), d(2024, 1, 1)));
        assert_eq!(w("nextYear"), (d(2025, 1, 1), d(2026, 1, 1)));
        assert_eq!(w("yearToDate"), (d(2024, 1, 1), d(2024, 3, 14)));
        assert_eq!(period_window("aboveAverage", today, false), None);
        // A Sunday is the first day of its own week.
        let sun = period_window("thisWeek", d(2024, 3, 10), false).unwrap();
        assert_eq!(sun, (d(2024, 3, 10), d(2024, 3, 17)));
        // Across a year end.
        let jan = period_window("lastMonth", d(2025, 1, 2), false).unwrap();
        assert_eq!(jan, (d(2024, 12, 1), d(2025, 1, 1)));
    }

    #[test]
    fn all_years_periods() {
        assert_eq!(period_months("M3"), Some(3..=3));
        assert_eq!(period_months("M12"), Some(12..=12));
        assert_eq!(period_months("Q1"), Some(1..=3));
        assert_eq!(period_months("Q4"), Some(10..=12));
        assert_eq!(period_months("M13"), None);
        assert_eq!(period_months("thisMonth"), None);
    }
}
