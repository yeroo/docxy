//! Parses local-time event-form input into UTC ISO timestamps. A bounded,
//! deterministic grammar (fixed format, `today`/`tomorrow`, bare/12-hour time,
//! `+Nh/m/d` relative) — not open-ended natural language. `now`/`offset_min`
//! are passed in (never read from the clock) so every shape is unit-testable.

use crate::ui::calendar::{civil_from_days, days_from_civil};

/// A local wall-clock instant, no timezone.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LocalDateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub min: u32,
}

/// Minutes since the Unix epoch for a local instant (used for UTC conversion
/// and relative arithmetic — handles all rollover via day-count math).
fn to_epoch_min(t: LocalDateTime) -> i64 {
    days_from_civil(t.year, t.month, t.day) * 1440 + (t.hour as i64) * 60 + t.min as i64
}

fn from_epoch_min(total: i64) -> LocalDateTime {
    let days = total.div_euclid(1440);
    let rem = total.rem_euclid(1440);
    let (y, m, d) = civil_from_days(days);
    LocalDateTime {
        year: y,
        month: m,
        day: d,
        hour: (rem / 60) as u32,
        min: (rem % 60) as u32,
    }
}

/// Formats a local instant as a UTC ISO timestamp, subtracting the local
/// offset (`offset_min` = minutes east of UTC).
fn to_utc_iso(t: LocalDateTime, offset_min: i64) -> String {
    let u = from_epoch_min(to_epoch_min(t) - offset_min);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:00Z",
        u.year, u.month, u.day, u.hour, u.min
    )
}

/// UTC ISO → local instant (for the End field's `+rel` base, and — now
/// `pub(crate)` rather than private — for `App::open_edit_event`'s
/// UTC→local conversion of a stored event's `start_utc`/`end_utc` into the
/// form's display text; see `format_local`).
pub(crate) fn utc_iso_to_local(utc: &str, offset_min: i64) -> Option<LocalDateTime> {
    // YYYY-MM-DDTHH:MM:SSZ
    let (date, rest) = utc.split_once('T')?;
    let (y, m, d) = parse_ymd(date)?;
    let hh: u32 = rest.get(0..2)?.parse().ok()?;
    let mm: u32 = rest.get(3..5)?.parse().ok()?;
    let base = LocalDateTime {
        year: y,
        month: m,
        day: d,
        hour: hh,
        min: mm,
    };
    Some(from_epoch_min(to_epoch_min(base) + offset_min))
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

fn parse_ymd(s: &str) -> Option<(i64, u32, u32)> {
    let mut it = s.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) {
        return None;
    }
    if d < 1 || d > days_in_month(y, m) {
        return None;
    }
    Some((y, m, d))
}

/// `HH:MM` (24-hour) or `H[:MM]am|pm` (12-hour). Returns (hour, min) in 24h.
fn parse_time(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().to_ascii_lowercase();
    if let Some(rest) = s.strip_suffix("am").or_else(|| s.strip_suffix("pm")) {
        let pm = s.ends_with("pm");
        let (h, m) = match rest.split_once(':') {
            Some((h, m)) => (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?),
            None => (rest.trim().parse::<u32>().ok()?, 0),
        };
        if !(1..=12).contains(&h) || m > 59 {
            return None;
        }
        let h24 = match (h, pm) {
            (12, false) => 0, // 12am → 00
            (12, true) => 12, // 12pm → 12
            (h, false) => h,
            (h, true) => h + 12,
        };
        return Some((h24, m));
    }
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some((h, m))
}

/// Parses one non-relative local input into a `LocalDateTime`.
fn parse_local(input: &str, now: LocalDateTime) -> Option<LocalDateTime> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    // "today"/"tomorrow" [time]
    for (word, add_days) in [("today", 0i64), ("tomorrow", 1)] {
        if let Some(rest) = s.strip_prefix(word) {
            let base = from_epoch_min(
                to_epoch_min(LocalDateTime {
                    hour: 0,
                    min: 0,
                    ..now
                }) + add_days * 1440,
            );
            let (h, m) = if rest.trim().is_empty() {
                (0, 0)
            } else {
                parse_time(rest)?
            };
            return Some(LocalDateTime {
                hour: h,
                min: m,
                ..base
            });
        }
    }
    // "YYYY-MM-DD [HH:MM]"
    if s.len() >= 10 && s.as_bytes()[4] == b'-' {
        let (date_part, time_part) = match s.split_once(' ') {
            Some((d, t)) => (d, Some(t)),
            None => (s, None),
        };
        let (y, mo, d) = parse_ymd(date_part)?;
        let (h, m) = match time_part {
            Some(t) => parse_time(t)?,
            None => (0, 0),
        };
        return Some(LocalDateTime {
            year: y,
            month: mo,
            day: d,
            hour: h,
            min: m,
        });
    }
    // bare time → today
    let (h, m) = parse_time(s)?;
    Some(LocalDateTime {
        hour: h,
        min: m,
        ..now
    })
}

/// `+Nh` / `+Nm` / `+Nd` (the leading `+` already stripped). Returns minutes.
fn parse_relative_minutes(s: &str) -> Option<i64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = num.trim().parse().ok()?;
    match unit {
        "h" | "H" => Some(n * 60),
        "m" | "M" => Some(n),
        "d" | "D" => Some(n * 1440),
        _ => None,
    }
}

/// Formats a local instant as the event form's display text (`YYYY-MM-DD
/// HH:MM`) — the inverse direction of what `parse_start`/`parse_end` accept
/// as input. Used by `App::open_new_event` (formatting the prefilled
/// Start/End) and `App::open_edit_event` (formatting a stored event's
/// UTC→local Start/End via `utc_iso_to_local`).
///
/// Both call sites are methods not yet bound to a key (Task 7 wires `c`/`e`
/// in Calendar mode); `cfg_attr` silences `dead_code` only outside tests,
/// same pattern as `parse_start`/`parse_end` below.
#[cfg_attr(not(test), allow(dead_code))]
pub fn format_local(t: LocalDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        t.year, t.month, t.day, t.hour, t.min
    )
}

/// Adds `minutes` to a local instant, handling hour/day/month/year rollover
/// via the same `to_epoch_min`/`from_epoch_min` day-count math every other
/// conversion in this module uses. Used by `App::open_new_event` to compute
/// the End prefill (Start + 1h) from whatever Start was rounded to.
///
/// See `format_local`'s doc comment for the same "not yet wired" `cfg_attr`
/// note.
#[cfg_attr(not(test), allow(dead_code))]
pub fn add_minutes(t: LocalDateTime, minutes: i64) -> LocalDateTime {
    from_epoch_min(to_epoch_min(t) + minutes)
}

/// Not yet called from production code — a later task's create/edit-event
/// form wiring is what will call this; `cfg_attr` silences `dead_code` only
/// outside tests, same pattern already used for `Compose::new`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse_start(input: &str, now: LocalDateTime, offset_min: i64) -> Option<String> {
    Some(to_utc_iso(parse_local(input, now)?, offset_min))
}

/// See `parse_start` — same "not yet wired" note applies.
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse_end(
    input: &str,
    start_utc: &str,
    now: LocalDateTime,
    offset_min: i64,
) -> Option<String> {
    let s = input.trim();
    if let Some(rel) = s.strip_prefix('+') {
        let base = utc_iso_to_local(start_utc, offset_min)?;
        let delta = parse_relative_minutes(rel)?;
        return Some(to_utc_iso(
            from_epoch_min(to_epoch_min(base) + delta),
            offset_min,
        ));
    }
    Some(to_utc_iso(parse_local(s, now)?, offset_min))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-07-19 10:00 local, UTC+3 (offset_min = 180) — like EPAM/MSK.
    fn now() -> LocalDateTime {
        LocalDateTime {
            year: 2026,
            month: 7,
            day: 19,
            hour: 10,
            min: 0,
        }
    }
    const OFF: i64 = 180;

    #[test]
    fn parses_fixed_datetime_to_utc() {
        // 14:00 local at +3 → 11:00 UTC
        assert_eq!(
            parse_start("2026-07-20 14:00", now(), OFF),
            Some("2026-07-20T11:00:00Z".into())
        );
    }
    #[test]
    fn parses_bare_date_as_midnight() {
        // 2026-07-20 00:00 local at +3 → 2026-07-19 21:00 UTC
        assert_eq!(
            parse_start("2026-07-20", now(), OFF),
            Some("2026-07-19T21:00:00Z".into())
        );
    }
    #[test]
    fn parses_today_and_tomorrow_with_time() {
        assert_eq!(
            parse_start("today 09:30", now(), OFF),
            Some("2026-07-19T06:30:00Z".into())
        );
        assert_eq!(
            parse_start("tomorrow 09:30", now(), OFF),
            Some("2026-07-20T06:30:00Z".into())
        );
    }
    #[test]
    fn parses_bare_time_and_12h() {
        assert_eq!(
            parse_start("14:00", now(), OFF),
            Some("2026-07-19T11:00:00Z".into())
        );
        assert_eq!(
            parse_start("2pm", now(), OFF),
            Some("2026-07-19T11:00:00Z".into())
        );
        assert_eq!(
            parse_start("2:30pm", now(), OFF),
            Some("2026-07-19T11:30:00Z".into())
        );
        // 12am → 00:00 today (2026-07-19) local; at +3 that's 21:00 the PREVIOUS UTC day
        assert_eq!(
            parse_start("12am", now(), OFF),
            Some("2026-07-18T21:00:00Z".into())
        );
    }
    #[test]
    fn end_accepts_relative_to_start() {
        let start = "2026-07-20T11:00:00Z"; // 14:00 local
        assert_eq!(
            parse_end("+1h", start, now(), OFF),
            Some("2026-07-20T12:00:00Z".into())
        );
        assert_eq!(
            parse_end("+90m", start, now(), OFF),
            Some("2026-07-20T12:30:00Z".into())
        );
        assert_eq!(
            parse_end("+1d", start, now(), OFF),
            Some("2026-07-21T11:00:00Z".into())
        );
        // non-relative end still works
        assert_eq!(
            parse_end("2026-07-20 15:00", start, now(), OFF),
            Some("2026-07-20T12:00:00Z".into())
        );
    }
    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_start("", now(), OFF), None);
        assert_eq!(parse_start("not a time", now(), OFF), None);
        assert_eq!(parse_start("25:99", now(), OFF), None);
        assert_eq!(parse_start("2026-13-40", now(), OFF), None);
    }
    #[test]
    fn rejects_invalid_day_of_month() {
        let now = LocalDateTime {
            year: 2026,
            month: 7,
            day: 19,
            hour: 10,
            min: 0,
        };
        assert_eq!(parse_start("2026-02-30", now, 180), None); // Feb never has 30
        assert_eq!(parse_start("2026-04-31", now, 180), None); // Apr has 30
        assert_eq!(parse_start("2026-02-29", now, 180), None); // 2026 not a leap year
        // valid boundaries still parse:
        assert!(parse_start("2024-02-29", now, 180).is_some()); // 2024 IS a leap year
        assert!(parse_start("2026-01-31", now, 180).is_some());
    }
}
