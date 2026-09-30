//! The host's clock as an Excel serial: what `TODAY()`/`NOW()` read
//! (`Engine::clock`) and what a typed date without a year takes its year
//! from. Excel's clock is local time, so the hosts want
//! [`local_now_serial`]; [`utc_now_serial`] is for stamps that are written
//! as UTC (threaded-comment `…Z` times, crash logs).
//!
//! std has no time-zone database, so the local offset comes from the OS
//! through one hand-declared call — `GetTimeZoneInformation` on Windows,
//! `localtime_r` on unix — rather than a crate. Those two `extern` blocks are
//! the only `unsafe` in gridcore. Anywhere else (wasm, whose host passes its
//! own local clock) the local clock is UTC.

use std::time::SystemTime;

/// Days from the 1900-system epoch to 1970-01-01.
const UNIX_EPOCH_SERIAL: f64 = 25_569.0;

/// Now, in UTC, as a 1900-system serial. `None` when the system clock is
/// before 1970.
pub fn utc_now_serial() -> Option<f64> {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    Some(unix_secs_to_serial(secs))
}

/// Now, in local time, as a 1900-system serial.
pub fn local_now_serial() -> Option<f64> {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let utc = unix_secs_to_serial(secs as f64);
    Some(shift_serial(utc, local_offset_minutes(secs as i64)))
}

fn unix_secs_to_serial(secs: f64) -> f64 {
    secs / 86_400.0 + UNIX_EPOCH_SERIAL
}

/// `serial` (UTC) seen from a zone `offset_minutes` ahead of UTC.
pub fn shift_serial(serial: f64, offset_minutes: i64) -> f64 {
    serial + offset_minutes as f64 / 1_440.0
}

/// Minutes the local zone is ahead of UTC (local = UTC + this) at
/// `unix_secs`. The daylight bias applies while daylight time is in effect.
#[cfg(windows)]
fn local_offset_minutes(_unix_secs: i64) -> i64 {
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SystemTime16 {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    #[repr(C)]
    struct TimeZoneInformation {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: SystemTime16,
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: SystemTime16,
        daylight_bias: i32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "GetTimeZoneInformation"]
        fn get_time_zone_information(info: *mut TimeZoneInformation) -> u32;
    }

    // SAFETY: `TimeZoneInformation` is a field-for-field `#[repr(C)]` mirror
    // of Win32's `TIME_ZONE_INFORMATION`, and all-zero bytes are a valid
    // value of it. The call only writes through the pointer it is given and
    // keeps no reference to it after returning.
    let mut info: TimeZoneInformation = unsafe { std::mem::zeroed() };
    let result = unsafe { get_time_zone_information(&mut info) };
    // 2 = daylight time, 1 = standard time, 0 = the zone has no daylight
    // rule (`Bias` alone). `u32::MAX` is failure: fall back to UTC.
    let bias = match result {
        2 => info.bias + info.daylight_bias,
        1 => info.bias + info.standard_bias,
        0 => info.bias,
        _ => 0,
    };
    // Win32's bias is UTC = local + bias; this wants local = UTC + offset.
    -(bias as i64)
}

/// Minutes the local zone is ahead of UTC at `unix_secs`, from the C
/// library's `localtime_r` (which honours `TZ` and the zone database).
#[cfg(all(unix, target_pointer_width = "64"))]
fn local_offset_minutes(unix_secs: i64) -> i64 {
    // The leading fields of `struct tm` as glibc, musl, macOS and the BSDs
    // lay them out: nine `int`s, then `tm_gmtoff` and `tm_zone`. The
    // trailing padding covers any extra fields a libc adds after those.
    #[repr(C)]
    struct Tm {
        tm_sec: i32,
        tm_min: i32,
        tm_hour: i32,
        tm_mday: i32,
        tm_mon: i32,
        tm_year: i32,
        tm_wday: i32,
        tm_yday: i32,
        tm_isdst: i32,
        tm_gmtoff: std::ffi::c_long,
        tm_zone: *const std::ffi::c_char,
        _reserved: [u64; 8],
    }

    unsafe extern "C" {
        fn localtime_r(time: *const i64, result: *mut Tm) -> *mut Tm;
    }

    let t: i64 = unix_secs;
    // SAFETY: `Tm` mirrors the leading fields of the platform's `struct tm`
    // (with spare room after them), all-zero bytes are a valid value of it,
    // and `localtime_r` only writes into the `result` it is given and reads
    // the `time_t` behind `time` — both live for the whole call. `time_t` is
    // 64-bit on the 64-bit targets this is compiled for.
    let mut tm: Tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !localtime_r(&t, &mut tm).is_null() };
    if ok { tm.tm_gmtoff as i64 / 60 } else { 0 }
}

#[cfg(not(any(windows, all(unix, target_pointer_width = "64"))))]
fn local_offset_minutes(_unix_secs: i64) -> i64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offset_shifts_the_serial_by_its_share_of_a_day() {
        // 2024-01-15 23:30 UTC.
        let utc = 45_306.0 + 23.5 / 24.0;
        let east = shift_serial(utc, 120);
        assert!((east - (utc + 120.0 / 1_440.0)).abs() < 1e-12);
        // Two hours east of 23:30 is already the next day.
        assert_eq!(east.floor(), 45_307.0);
        // Five hours west of 01:00 is still the day before.
        let early = 45_306.0 + 1.0 / 24.0;
        assert_eq!(shift_serial(early, -300).floor(), 45_305.0);
        assert_eq!(shift_serial(utc, 0), utc);
    }

    #[test]
    fn the_local_clock_is_within_a_day_of_utc() {
        let (utc, local) = (utc_now_serial().unwrap(), local_now_serial().unwrap());
        // Zones run from UTC-12 to UTC+14.
        assert!(
            (local - utc).abs() <= 14.0 / 24.0 + 1e-3,
            "{local} vs {utc}"
        );
        assert!(utc > 45_000.0, "after 2023: {utc}");
    }

    #[test]
    fn the_offset_is_whole_minutes_within_the_zones_that_exist() {
        let m = local_offset_minutes(1_700_000_000);
        assert!((-12 * 60..=14 * 60).contains(&m), "{m}");
    }
}
