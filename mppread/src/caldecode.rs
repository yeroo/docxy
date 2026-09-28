//! Validated calendar-table decoding for current Project `.mpp` files.
use crate::{cfb::Cfb, props};
use projcore::{Calendar, CalendarException, DateTime, DayWorking, WorkingTime};
use std::collections::{HashMap, HashSet};

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn i32_at(b: &[u8], o: usize) -> i32 {
    i32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn block(data: &[u8], off: usize) -> Result<&[u8], String> {
    let header = off
        .checked_add(4)
        .filter(|&n| n <= data.len())
        .ok_or("calendar Var2Data offset out of range")?;
    let end = header
        .checked_add(u32_at(data, off) as usize)
        .filter(|&n| n <= data.len())
        .ok_or("calendar Var2Data block out of range")?;
    Ok(&data[header..end])
}

fn name(value: &[u8]) -> Result<String, String> {
    if value.len() < 4 || !value.len().is_multiple_of(2) {
        return Err("invalid calendar name block".into());
    }
    let units: Vec<_> = value
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    if units.last() != Some(&0) {
        return Err("unterminated calendar name".into());
    }
    let s = String::from_utf16(&units[..units.len() - 1])
        .map_err(|_| "invalid UTF-16 calendar name")?;
    if s.is_empty() || s.chars().any(char::is_control) {
        return Err("invalid calendar name".into());
    }
    Ok(s)
}

fn exception_name(value: &[u8]) -> Result<String, &'static str> {
    if value.len() < 2 || !value.len().is_multiple_of(2) {
        return Err("invalid UTF-16 length");
    }
    let units: Vec<_> = value
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    if units.last() != Some(&0) {
        return Err("unterminated UTF-16");
    }
    let s = String::from_utf16(&units[..units.len() - 1]).map_err(|_| "invalid UTF-16")?;
    if s.chars().any(|c| {
        !matches!(c, '\t' | '\n' | '\r') && !(' '..='\u{FFFD}').contains(&c) && c < '\u{10000}'
    }) {
        return Err("invalid XML character");
    }
    Ok(s)
}

fn working_time(
    start_tenths: u32,
    duration: i32,
    previous: Option<&WorkingTime>,
) -> Result<WorkingTime, &'static str> {
    if !start_tenths.is_multiple_of(10) || duration <= 0 || duration % 10 != 0 {
        return Err("invalid period");
    }
    let from = start_tenths / 10;
    let to = from
        .checked_add(duration as u32 / 10)
        .ok_or("period overflow")?;
    if to > 1440 || previous.is_some_and(|last| from < last.to) {
        return Err("invalid period order");
    }
    Ok(WorkingTime { from, to })
}

fn hours(value: &[u8], base: bool) -> Result<[Option<DayWorking>; 7], String> {
    // Seven 60-byte Sunday-first records; exceptions start at byte 420. Start and
    // duration are tenths of a minute. An inherited base day uses Project's
    // built-in Standard pattern; a derived day inherits its base calendar.
    if value.len() < 428 {
        return Err(format!(
            "unrecognized calendar hours length {}",
            value.len()
        ));
    }
    let standard = Calendar::standard_week();
    let mut week: [Option<DayWorking>; 7] = std::array::from_fn(|_| None);
    for day in 0..7 {
        let rec = &value[day * 60..day * 60 + 60];
        let flag = u16_at(rec, 0);
        let count = u16_at(rec, 2) as usize;
        if flag > 1 || count > 5 {
            return Err(format!(
                "invalid calendar weekday {day} flag or period count"
            ));
        }
        if flag == 1 {
            if base {
                week[day] = Some(standard[day].clone());
            }
            continue;
        }
        let mut times = Vec::with_capacity(count);
        for period in 0..count {
            let from_tenths = u32::from(u16_at(rec, 8 + period * 2));
            let duration = i32_at(rec, 20 + period * 4);
            let time = working_time(from_tenths, duration, times.last())
                .map_err(|e| format!("calendar weekday {day} period {period}: {e}"))?;
            times.push(time);
        }
        week[day] = Some(DayWorking { times });
    }
    Ok(week)
}

fn exceptions(value: &[u8]) -> Result<(Vec<CalendarException>, bool), String> {
    if value.len() < 428 {
        return Err("truncated calendar exception header".into());
    }
    let count = u16_at(value, 420) as usize;
    if u16_at(value, 422) != 0 {
        return Err("invalid calendar exception header".into());
    }
    let mut offset = 424usize;
    let mut out = Vec::with_capacity(count.min(64));
    for index in 0..count {
        let end = offset
            .checked_add(92)
            .filter(|&n| n <= value.len())
            .ok_or_else(|| format!("truncated calendar exception {index}"))?;
        let rec = &value[offset..end];
        let first = u16_at(rec, 0);
        let last = u16_at(rec, 2);
        if last < first {
            return Err(format!("calendar exception {index} ends before it starts"));
        }
        // These reserved bytes are zero in the generated snapshots, the
        // MSPDI-resaved probes and the COM Exceptions.Add probes, unlike the
        // Type 1 pattern word.
        if u16_at(rec, 6) != 0
            || rec[9..14].iter().any(|&b| b != 0)
            || rec[73..76].iter().any(|&b| b != 0)
            || u32_at(rec, 84) != 0
        {
            return Err(format!("unrecognized calendar exception {index} fields"));
        }
        let occurrences = u32_at(rec, 4);
        let entered = u32_at(rec, 8);
        if occurrences == 0 || occurrences > i32::MAX as u32 || entered > 1 {
            return Err(format!("invalid calendar exception {index} recurrence"));
        }
        let period_count = u16_at(rec, 14) as usize;
        if period_count > 5 {
            return Err(format!("invalid calendar exception {index} period count"));
        }
        if u16_at(rec, 16) != (if period_count > 0 { u16_at(rec, 20) } else { 0 })
            || u16_at(rec, 18) != 0
        {
            return Err(format!("invalid calendar exception {index} first period"));
        }
        let mut times = Vec::with_capacity(period_count);
        let mut cumulative = 0u32;
        for p in 0..period_count {
            let start = u32::from(u16_at(rec, 20 + 2 * p));
            let duration = i32_at(rec, 32 + 4 * p);
            let time = working_time(start, duration, times.last())
                .map_err(|e| format!("calendar exception {index} period {p}: {e}"))?;
            cumulative = cumulative
                .checked_add(duration as u32)
                .ok_or("calendar exception cumulative period overflow")?;
            if i32_at(rec, 52 + 4 * p) != cumulative as i32 {
                return Err(format!(
                    "invalid calendar exception {index} cumulative period"
                ));
            }
            times.push(time);
        }
        let kind = u32_at(rec, 72);
        let b = &rec[76..80];
        let mut exception = CalendarException {
            from: Some(DateTime::from_minutes(
                (crate::mpp::MPP_EPOCH_DAYS + i64::from(first)) * 1440,
            )),
            to: Some(DateTime::from_minutes(
                (crate::mpp::MPP_EPOCH_DAYS + i64::from(last)) * 1440 + 1439,
            )),
            kind: Some(kind as i32),
            occurrences: Some(occurrences as i32),
            entered_by_occurrences: Some(entered != 0),
            day: DayWorking { times },
            ..CalendarException::default()
        };
        match kind {
            // Type 1's pattern word is not stable: 0x0230 in the generated
            // snapshots, zero in MSPDI-resaved and COM Exceptions.Add probes.
            // It does not appear in Project's XML for one-off exceptions.
            1 => {}
            2 => {
                exception.month = Some(i32::from(b[0]));
                exception.month_day = Some(i32::from(b[1]));
            }
            3 => {
                exception.month = Some(i32::from(b[0]));
                exception.month_position = Some(i32::from(b[1]));
                exception.month_item = Some(i32::from(b[2]));
            }
            4 => {
                exception.month_day = Some(i32::from(b[0]));
                exception.period = Some(i32::from(u16_at(rec, 78)));
            }
            5 => {
                exception.month_position = Some(i32::from(b[0]));
                exception.month_item = Some(i32::from(b[1]));
                exception.period = Some(i32::from(u16_at(rec, 78)));
            }
            6 => {
                exception.days_of_week = Some(i32::from(b[0]));
                exception.period = Some(i32::from(u16_at(rec, 78)));
            }
            7 => {
                exception.period = Some(i32::from(u16_at(rec, 76)));
            }
            _ => {
                return Err(format!(
                    "unknown calendar exception {index} recurrence type {kind}"
                ));
            }
        }
        let name_len = u32_at(rec, 88) as usize;
        let padded = name_len
            .checked_add(3)
            .map(|n| n & !3)
            .ok_or("calendar exception name length overflow")?;
        let next = end
            .checked_add(padded)
            .filter(|&n| n <= value.len())
            .ok_or_else(|| format!("calendar exception {index} name past block"))?;
        // Project writes an empty <Name> for zero-length binary names.
        exception.name = Some(if name_len == 0 {
            String::new()
        } else {
            exception_name(&value[end..end + name_len])
                .map_err(|e| format!("calendar exception {index} name: {e}"))?
        });
        out.push(exception);
        offset = next;
    }
    if offset.checked_add(4).is_none_or(|n| n > value.len()) {
        return Err("truncated calendar work-week header".into());
    }
    Ok((out, u32_at(value, offset) != 0))
}

fn resource_names(cfb: &Cfb, prefix: &str) -> Result<HashMap<i32, String>, String> {
    let resource_prefix = prefix.replace("TBkndCal/", "TBkndRsc/");
    let vm = cfb
        .read_path(&format!("{resource_prefix}VarMeta"))
        .ok_or("missing resource VarMeta for calendar names")?;
    let v2 = cfb
        .read_path(&format!("{resource_prefix}Var2Data"))
        .ok_or("missing resource Var2Data for calendar names")?;
    if vm.len() < 24 || vm[..4] != [0xba, 0xad, 0xdf, 0xfa] {
        return Err("invalid resource VarMeta for calendar names".into());
    }
    let count = u32_at(&vm, 8) as usize;
    if 24usize.checked_add(
        count
            .checked_mul(12)
            .ok_or("resource VarMeta count overflow")?,
    ) != Some(vm.len())
        || u32_at(&vm, 20) as usize != v2.len()
    {
        return Err("resource VarMeta count or Var2Data length mismatch".into());
    }
    let mut names = HashMap::new();
    for i in 0..count {
        let e = &vm[24 + i * 12..36 + i * 12];
        if u16_at(e, 8) != 1 {
            continue;
        }
        let uid = i32_at(e, 0);
        if uid <= 0 || u16_at(e, 10) != 0x0c40 {
            return Err(format!("invalid resource name entry {i}"));
        }
        let value = block(&v2, u32_at(e, 4) as usize)?;
        if names.insert(uid, name(value)?).is_some() {
            return Err(format!("duplicate resource name UID {uid}"));
        }
    }
    Ok(names)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DecodedCalendars {
    pub calendars: Vec<Calendar>,
    pub default_uid: i32,
    pub work_week_uids: HashSet<i32>,
}

pub(crate) fn decode(bytes: &[u8], legacy: bool) -> Result<Option<DecodedCalendars>, String> {
    if legacy {
        return Ok(None);
    }
    let cfb = Cfb::open(bytes)?;
    let paths = cfb.paths();
    let cal_paths: Vec<_> = paths.iter().filter(|p| p.contains("TBkndCal/")).collect();
    if cal_paths.is_empty() {
        return Ok(None);
    }
    let meta_path = paths
        .iter()
        .find(|p| p.ends_with("TBkndCal/FixedMeta"))
        .ok_or("calendar table missing FixedMeta")?;
    let prefix = meta_path.trim_end_matches("FixedMeta");
    let read = |which: &str| {
        cfb.read_path(&format!("{prefix}{which}"))
            .ok_or_else(|| format!("calendar table missing {which}"))
    };
    let fm = read("FixedMeta")?;
    let fd = read("FixedData")?;
    let vm = read("VarMeta")?;
    let v2 = read("Var2Data")?;
    if fm.len() < 66 || fm[..4] != [0xba, 0xad, 0xdf, 0xfa] {
        return Err("invalid calendar FixedMeta header".into());
    }
    let count = u32_at(&fm, 8) as usize;
    if count < 5
        || 16usize.checked_add(count.checked_mul(10).ok_or("calendar count overflow")?)
            != Some(fm.len())
        || u32_at(&fm, 12) as usize != fd.len()
    {
        return Err("calendar FixedMeta count or FixedData length mismatch".into());
    }
    let mut rows = Vec::new();
    for i in 0..count {
        let e = &fm[16 + i * 10..26 + i * 10];
        let off = u32_at(e, 4) as usize;
        let end = if i + 1 < count {
            u32_at(&fm, 16 + (i + 1) * 10 + 4) as usize
        } else {
            fd.len()
        };
        if off >= end || end > fd.len() {
            return Err(format!("calendar FixedMeta offset {i} out of range"));
        }
        let len = end - off;
        if i < 4 {
            if len != 16 || u16_at(e, 0) != 4 {
                return Err("unrecognized calendar schema stubs".into());
            }
        } else if i + 1 == count && len == 14 {
            // Some files append a 14-byte terminal row. Others end with an
            // ordinary 12-byte calendar row.
        } else {
            if len != 12 || u16_at(e, 0) != 0 {
                return Err(format!("unrecognized calendar record {i}"));
            }
            let rec = &fd[off..end];
            let uid = i32_at(rec, 8);
            let base = i32_at(rec, 0);
            if uid <= 0 || base < -1 {
                return Err(format!("invalid calendar UID {uid} or base UID {base}"));
            }
            rows.push((
                uid,
                (base > 0 && base != uid).then_some(base),
                i32_at(rec, 4),
            ));
        }
    }
    let uids: HashSet<_> = rows.iter().map(|&(uid, _, _)| uid).collect();
    if uids.len() != rows.len() {
        return Err("duplicate calendar UID".into());
    }
    if vm.len() < 24 || vm[..4] != [0xba, 0xad, 0xdf, 0xfa] {
        return Err("invalid calendar VarMeta header".into());
    }
    let var_count = u32_at(&vm, 8) as usize;
    if 24usize.checked_add(
        var_count
            .checked_mul(12)
            .ok_or("calendar VarMeta count overflow")?,
    ) != Some(vm.len())
        || u32_at(&vm, 20) as usize != v2.len()
    {
        return Err("calendar VarMeta count or Var2Data length mismatch".into());
    }
    let mut fields: HashMap<(i32, u16), &[u8]> = HashMap::new();
    for i in 0..var_count {
        let e = &vm[24 + i * 12..36 + i * 12];
        let uid = i32_at(e, 0);
        let key = u16_at(e, 8);
        if !uids.contains(&uid) || u16_at(e, 10) != 0x0d40 {
            return Err(format!(
                "invalid calendar VarMeta entry {i}: UID {uid}, key {key}, tag {:#x}",
                u16_at(e, 10)
            ));
        }
        let value = block(&v2, u32_at(e, 4) as usize)?;
        if fields.insert((uid, key), value).is_some() {
            return Err(format!("duplicate calendar VarMeta key ({uid},{key})"));
        }
    }
    let mut calendars = Vec::with_capacity(rows.len());
    let mut work_week_uids = HashSet::new();
    let mut resources = None;
    for (uid, base_uid, resource_uid) in rows {
        // FixedData also contains unnamed internal/unused calendar rows.
        // Project omits these from MSPDI, and they have no VarMeta entries.
        if resource_uid <= 0 && !fields.keys().any(|&(field_uid, _)| field_uid == uid) {
            continue;
        }
        let calendar_name = if let Some(label) = fields.get(&(uid, 1)) {
            name(label)?
        } else if resource_uid > 0 {
            let names = match &resources {
                Some(names) => names,
                None => resources.insert(resource_names(&cfb, prefix)?),
            };
            names.get(&resource_uid).cloned().ok_or_else(|| {
                format!("calendar UID {uid} has no name or resource UID {resource_uid} name")
            })?
        } else {
            return Err(format!("missing calendar name for UID {uid}"));
        };
        let (week, exceptions) = match fields.get(&(uid, 8)) {
            Some(value) => {
                let (exceptions, has_work_weeks) = exceptions(value)?;
                if has_work_weeks {
                    work_week_uids.insert(uid);
                }
                (hours(value, base_uid.is_none())?, exceptions)
            }
            None if base_uid.is_some() => (std::array::from_fn(|_| None), Vec::new()),
            // A base without a key-8 block uses Project's built-in week,
            // regardless of its name. MPXJ applies the same default.
            None => (Calendar::standard_week().map(Some), Vec::new()),
        };
        calendars.push(Calendar {
            uid,
            name: calendar_name,
            base_calendar_uid: base_uid,
            is_baseline_calendar: false,
            week,
            exceptions,
            work_weeks: Vec::new(),
        });
    }
    calendars.sort_by_key(|c| c.uid); // Project's MSPDI export uses UID order.
    for cal in &calendars {
        if let Some(base) = cal.base_calendar_uid {
            if !calendars
                .iter()
                .any(|c| c.uid == base && c.base_calendar_uid.is_none())
            {
                return Err(format!(
                    "calendar {} has missing or derived base UID {base}",
                    cal.uid
                ));
            }
        }
    }
    let props_path = format!("{}Props", prefix.trim_end_matches("TBkndCal/"));
    let default_name = cfb
        .read_path(&props_path)
        .ok_or_else(|| "missing calendar project Props".to_string())
        .and_then(|p| props::default_calendar_name(&p))?;
    let default_uid = match default_name {
        Some(name) => calendars
            .iter()
            .find(|c| c.name == name && c.base_calendar_uid.is_none())
            .map(|c| c.uid)
            .ok_or_else(|| format!("unknown default calendar {name:?}"))?,
        None => calendars
            .iter()
            .find(|c| c.uid == 1 && c.base_calendar_uid.is_none())
            .map(|c| c.uid)
            .ok_or("calendar project Props has no default and UID 1 is absent")?,
    };
    let default = calendars.iter().find(|c| c.uid == default_uid).unwrap();
    if !default
        .resolve_week(|uid| calendars.iter().find(|c| c.uid == uid))
        .iter()
        .any(DayWorking::working)
    {
        return Err(format!(
            "default calendar UID {default_uid} has no working time"
        ));
    }
    Ok(Some(DecodedCalendars {
        calendars,
        default_uid,
        work_week_uids,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfb::{Node, write_cfb_tree};

    #[test]
    fn generated_snapshots_have_no_undecoded_work_weeks() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp/snapshots");
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "mpp")
                || path
                    .file_stem()
                    .is_some_and(|stem| stem.to_string_lossy().ends_with("-mpp12"))
            {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let work_week_uids = decode(&bytes, false).unwrap().unwrap().work_week_uids;
            assert!(
                work_week_uids.is_empty(),
                "{}: undecoded work weeks on calendars {work_week_uids:?}",
                path.display()
            );
        }
    }

    fn exception_block() -> Vec<u8> {
        let mut b = vec![0u8; 424 + 92];
        b[420..422].copy_from_slice(&1u16.to_le_bytes());
        let rec = &mut b[424..516];
        rec[0..2].copy_from_slice(&0x3a8fu16.to_le_bytes());
        rec[2..4].copy_from_slice(&0x3a8fu16.to_le_bytes());
        rec[4..8].copy_from_slice(&1u32.to_le_bytes());
        rec[72..76].copy_from_slice(&1u32.to_le_bytes());
        let name: Vec<u8> = "Holiday"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        rec[88..92].copy_from_slice(&(name.len() as u32).to_le_bytes());
        b.extend_from_slice(&name);
        b.extend_from_slice(&[0; 4]);
        b
    }

    #[test]
    fn decodes_one_exception_and_empty_header() {
        assert_eq!(
            crate::mpp::MPP_EPOCH_DAYS,
            DateTime::from_ymd_hm(1983, 12, 31, 0, 0).day_number()
        );
        let (ex, has_work_weeks) = exceptions(&exception_block()).unwrap();
        assert!(!has_work_weeks);
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0].name.as_deref(), Some("Holiday"));
        assert_eq!(ex[0].from.unwrap().to_mspdi(), "2025-01-15T00:00:00");
        assert_eq!(ex[0].to.unwrap().to_mspdi(), "2025-01-15T23:59:00");
        assert_eq!(ex[0].kind, Some(1));
        assert_eq!(ex[0].occurrences, Some(1));
        assert!(!ex[0].entered_by_occurrences.unwrap());
        assert!(exceptions(&vec![0; 428]).unwrap().0.is_empty());

        let mut alternate = exception_block();
        alternate[500..504].copy_from_slice(&0x0100_0230u32.to_le_bytes());
        assert_eq!(exceptions(&alternate).unwrap().0, ex);
        let mut work_week = exception_block();
        let count = work_week.len() - 4;
        work_week[count..].copy_from_slice(&1u32.to_le_bytes());
        assert!(exceptions(&work_week).unwrap().1);
    }

    #[test]
    fn recurrence_periods_are_u16_and_unmapped_pattern_bytes_are_ignored() {
        let mut monthly = exception_block();
        monthly[496..500].copy_from_slice(&4u32.to_le_bytes());
        monthly[500..504].copy_from_slice(&[4, 2, 44, 1]);
        let decoded = exceptions(&monthly).unwrap().0;
        assert_eq!(decoded[0].month_day, Some(4));
        assert_eq!(decoded[0].period, Some(300));

        let mut daily = exception_block();
        daily[496..500].copy_from_slice(&7u32.to_le_bytes());
        daily[500..504].copy_from_slice(&[44, 1, 5, 6]);
        let decoded = exceptions(&daily).unwrap().0;
        assert_eq!(decoded[0].period, Some(300));
    }

    #[test]
    fn exception_names_allow_empty_and_xml_whitespace_but_refuse_bad_chars() {
        let renamed = |raw: &[u8]| {
            let mut b = exception_block();
            b.truncate(516);
            b[512..516].copy_from_slice(&(raw.len() as u32).to_le_bytes());
            b.extend_from_slice(raw);
            b.resize(b.len().next_multiple_of(4), 0);
            b.extend_from_slice(&[0; 4]);
            b
        };
        assert_eq!(
            exceptions(&renamed(&[])).unwrap().0[0].name.as_deref(),
            Some("")
        );
        assert_eq!(
            exceptions(&renamed(&[0, 0])).unwrap().0[0].name.as_deref(),
            Some("")
        );
        let control: Vec<_> = "A\nB"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(
            exceptions(&renamed(&control)).unwrap().0[0].name.as_deref(),
            Some("A\nB")
        );
        let error = exceptions(&renamed(&[0, 0xd8, 0, 0])).unwrap_err();
        assert!(error.contains("exception 0 name"), "{error}");
        let invalid_xml: Vec<_> = "A\u{0001}"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        let error = exceptions(&renamed(&invalid_xml)).unwrap_err();
        assert!(
            error.contains("exception 0 name: invalid XML character"),
            "{error}"
        );
    }

    #[test]
    fn refuses_malformed_exceptions() {
        let mut b = exception_block();
        b.truncate(424 + 91);
        assert!(exceptions(&b).is_err(), "truncated record");

        let mut b = exception_block();
        b[512..516].copy_from_slice(&1000u32.to_le_bytes());
        assert!(exceptions(&b).is_err(), "name past block");

        let mut b = exception_block();
        b[438..440].copy_from_slice(&1u16.to_le_bytes());
        b[440..442].copy_from_slice(&15000u16.to_le_bytes());
        b[444..446].copy_from_slice(&15000u16.to_le_bytes());
        b[456..460].copy_from_slice(&100i32.to_le_bytes());
        b[476..480].copy_from_slice(&100i32.to_le_bytes());
        assert!(exceptions(&b).is_err(), "period past midnight");

        let mut b = exception_block();
        b[424..426].copy_from_slice(&0x3a90u16.to_le_bytes());
        assert!(exceptions(&b).is_err(), "backward date range");

        let mut b = exception_block();
        b[496..500].copy_from_slice(&99u32.to_le_bytes());
        assert!(exceptions(&b).is_err(), "unknown recurrence type");
    }

    fn fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut fd = vec![0u8; 64];
        for i in 0..4 {
            fd[i * 16..i * 16 + 2].copy_from_slice(&(i as u16).to_le_bytes());
        }
        for (base, resource, uid) in [(-1i32, -1i32, 1i32), (1, 1, 5)] {
            fd.extend_from_slice(&base.to_le_bytes());
            fd.extend_from_slice(&resource.to_le_bytes());
            fd.extend_from_slice(&uid.to_le_bytes());
        }
        let mut fm = vec![0u8; 16 + 6 * 10];
        fm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        fm[8..12].copy_from_slice(&6u32.to_le_bytes());
        fm[12..16].copy_from_slice(&(fd.len() as u32).to_le_bytes());
        for (i, off) in [0, 16, 32, 48, 64, 76].into_iter().enumerate() {
            let p = 16 + i * 10;
            fm[p..p + 2].copy_from_slice(&(if i < 4 { 4u16 } else { 0 }).to_le_bytes());
            fm[p + 4..p + 8].copy_from_slice(&(off as u32).to_le_bytes());
        }
        let mut v2 = Vec::new();
        let mut entries = Vec::new();
        for (uid, key, value) in [
            (
                1u32,
                1u16,
                "Standard"
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            ),
            (1, 8, {
                let mut b = vec![0u8; 428];
                for d in 0..7 {
                    b[d * 60..d * 60 + 2].copy_from_slice(&1u16.to_le_bytes());
                }
                b
            }),
            (
                5,
                1,
                "Alice"
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            ),
            (5, 8, {
                let mut b = vec![0u8; 428];
                for d in 0..7 {
                    b[d * 60..d * 60 + 2].copy_from_slice(&1u16.to_le_bytes());
                }
                let p = 2 * 60;
                b[p..p + 2].copy_from_slice(&0u16.to_le_bytes());
                b[p + 2..p + 4].copy_from_slice(&1u16.to_le_bytes());
                b[p + 8..p + 10].copy_from_slice(&4200u16.to_le_bytes());
                b[p + 20..p + 24].copy_from_slice(&3000i32.to_le_bytes());
                b
            }),
        ] {
            entries.extend_from_slice(&uid.to_le_bytes());
            entries.extend_from_slice(&(v2.len() as u32).to_le_bytes());
            entries.extend_from_slice(&key.to_le_bytes());
            entries.extend_from_slice(&0x0d40u16.to_le_bytes());
            v2.extend_from_slice(&(value.len() as u32).to_le_bytes());
            v2.extend_from_slice(&value);
        }
        let mut vm = vec![0u8; 24];
        vm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        vm[8..12].copy_from_slice(&4u32.to_le_bytes());
        vm[20..24].copy_from_slice(&(v2.len() as u32).to_le_bytes());
        vm.extend_from_slice(&entries);
        (fm, fd, vm, v2)
    }

    fn file_with_default(
        fm: Vec<u8>,
        fd: Vec<u8>,
        vm: Vec<u8>,
        v2: Vec<u8>,
        default_name: &str,
    ) -> Vec<u8> {
        let default: Vec<_> = default_name
            .encode_utf16()
            .chain([0, 0])
            .flat_map(u16::to_le_bytes)
            .collect();
        write_cfb_tree(&[Node::Storage(
            "   114",
            vec![
                Node::Stream("Props", props::stream(&[(0x0240_000e, &default)])),
                Node::Storage(
                    "TBkndCal",
                    vec![
                        Node::Stream("FixedMeta", fm),
                        Node::Stream("FixedData", fd),
                        Node::Stream("VarMeta", vm),
                        Node::Stream("Var2Data", v2),
                    ],
                ),
            ],
        )])
    }

    fn file(fm: Vec<u8>, fd: Vec<u8>, vm: Vec<u8>, v2: Vec<u8>) -> Vec<u8> {
        file_with_default(fm, fd, vm, v2, "Standard")
    }

    #[test]
    fn hours_less_base_uses_builtin_week_regardless_of_name() {
        let (fm, fd, mut vm, mut v2) = fixture();
        // "Workdays" and "Standard" have equal UTF-16 block sizes.
        for (i, unit) in "Workdays".encode_utf16().enumerate() {
            v2[4 + i * 2..6 + i * 2].copy_from_slice(&unit.to_le_bytes());
        }
        vm.drain(36..48); // remove the base calendar's key-8 entry
        vm[8..12].copy_from_slice(&3u32.to_le_bytes());
        let DecodedCalendars {
            calendars,
            default_uid: uid,
            ..
        } = decode(&file_with_default(fm, fd, vm, v2, "Workdays"), false)
            .unwrap()
            .unwrap();
        assert_eq!(uid, 1);
        assert_eq!(calendars[0].name, "Workdays");
        assert_eq!(calendars[0].week, Calendar::standard_week().map(Some));
    }

    #[test]
    fn decodes_base_and_derived_weekdays() {
        let (fm, fd, vm, v2) = fixture();
        let DecodedCalendars {
            calendars: cals,
            default_uid: default,
            work_week_uids: work_weeks,
        } = decode(&file(fm, fd, vm, v2), false).unwrap().unwrap();
        assert!(work_weeks.is_empty());
        assert_eq!(default, 1);
        assert_eq!(
            cals.iter()
                .map(|c| (c.uid, c.base_calendar_uid))
                .collect::<Vec<_>>(),
            [(1, None), (5, Some(1))]
        );
        assert_eq!(cals[1].week[0], None);
        assert_eq!(
            cals[1].week[2].as_ref().unwrap().times,
            [WorkingTime { from: 420, to: 720 }]
        );
    }

    #[test]
    fn refuses_corrupt_calendar_indexes_and_periods() {
        let (fm, fd, vm, v2) = fixture();
        let mut bad = fm.clone();
        bad[0] = 0;
        assert!(decode(&file(bad, fd.clone(), vm.clone(), v2.clone()), false).is_err());
        let mut bad = fm.clone();
        bad[8..12].copy_from_slice(&7u32.to_le_bytes());
        assert!(decode(&file(bad, fd.clone(), vm.clone(), v2.clone()), false).is_err());
        let mut bad = vm.clone();
        bad[8..12].copy_from_slice(&5u32.to_le_bytes());
        assert!(decode(&file(fm.clone(), fd.clone(), bad, v2.clone()), false).is_err());
        let mut bad = vm.clone();
        bad[24 + 3 * 12 + 4..24 + 3 * 12 + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&file(fm.clone(), fd.clone(), bad, v2.clone()), false).is_err());
        let mut bad = v2.clone();
        let off = u32_at(&vm, 24 + 3 * 12 + 4) as usize;
        bad[off + 4 + 2 * 60 + 2..off + 4 + 2 * 60 + 4].copy_from_slice(&6u16.to_le_bytes());
        assert!(decode(&file(fm.clone(), fd.clone(), vm.clone(), bad), false).is_err());
        let mut bad = fd.clone();
        bad[76..80].copy_from_slice(&99i32.to_le_bytes());
        assert!(decode(&file(fm, bad, vm, v2), false).is_err());
    }

    #[test]
    fn refuses_unknown_uids_duplicate_keys_and_bad_names() {
        let (fm, fd, vm, v2) = fixture();
        let mut bad = vm.clone();
        bad[24..28].copy_from_slice(&99u32.to_le_bytes());
        assert!(decode(&file(fm.clone(), fd.clone(), bad, v2.clone()), false).is_err());
        let mut bad = vm.clone();
        bad[24 + 12 + 8..24 + 12 + 10].copy_from_slice(&1u16.to_le_bytes());
        assert!(decode(&file(fm.clone(), fd.clone(), bad, v2.clone()), false).is_err());
        let mut bad = v2.clone();
        bad[4..6].copy_from_slice(&0xd800u16.to_le_bytes());
        assert!(decode(&file(fm, fd, vm, bad), false).is_err());
    }

    #[test]
    fn reads_full_day_and_refuses_overlapping_periods() {
        let mut b = vec![0u8; 428];
        for day in 0..7 {
            b[day * 60..day * 60 + 2].copy_from_slice(&1u16.to_le_bytes());
        }
        b[0..2].copy_from_slice(&0u16.to_le_bytes());
        b[2..4].copy_from_slice(&1u16.to_le_bytes());
        b[20..24].copy_from_slice(&14400i32.to_le_bytes());
        assert_eq!(
            hours(&b, false).unwrap()[0].as_ref().unwrap().times,
            [WorkingTime { from: 0, to: 1440 }]
        );
        b[2..4].copy_from_slice(&2u16.to_le_bytes());
        b[10..12].copy_from_slice(&100u16.to_le_bytes());
        b[24..28].copy_from_slice(&100i32.to_le_bytes());
        assert!(hours(&b, false).is_err());
    }

    #[test]
    fn no_calendar_storage_keeps_synthesized_standard() {
        let bytes = write_cfb_tree(&[Node::Stream("Props", vec![0u8; 4])]);
        assert_eq!(decode(&bytes, false), Ok(None));
    }

    #[test]
    fn legacy_task_layout_ignores_its_unrecognized_calendar_storage() {
        let mut fm = vec![0u8; 16 + 4 * 47];
        fm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        fm[8..12].copy_from_slice(&4u32.to_le_bytes());
        for (i, offset) in [0u32, 8, 16, 24].into_iter().enumerate() {
            let p = 16 + i * 47 + 4;
            fm[p..p + 4].copy_from_slice(&offset.to_le_bytes());
        }
        let mut fd = vec![0u8; 24 + 264];
        for offset in [24 + 88, 24 + 92] {
            fd[offset..offset + 4].copy_from_slice(&[0xc0, 0x12, 0x86, 0x3a]);
        }
        let mut vm = vec![0u8; 24];
        vm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        vm[8..12].copy_from_slice(&1u32.to_le_bytes());
        vm[20..24].copy_from_slice(&8u32.to_le_bytes());
        vm.extend_from_slice(&0u16.to_le_bytes());
        vm.extend_from_slice(&0x0b00u16.to_le_bytes());
        vm.extend_from_slice(&0u32.to_le_bytes());
        let v2 = vec![4, 0, 0, 0, b'P', 0, 0, 0];
        let bytes = write_cfb_tree(&[Node::Storage(
            "   114",
            vec![
                Node::Storage(
                    "TBkndTask",
                    vec![
                        Node::Stream("FixedMeta", fm),
                        Node::Stream("FixedData", fd),
                        Node::Stream("VarMeta", vm),
                        Node::Stream("Var2Data", v2),
                    ],
                ),
                Node::Storage("TBkndCal", vec![Node::Stream("VarMeta", vec![1, 2])]),
            ],
        )]);
        assert!(crate::taskdecode::decode_table(&bytes).unwrap().legacy);
        let project = crate::project::project_from_mpp(&bytes).unwrap();
        assert_eq!(project.calendars, vec![Calendar::standard(1)]);
    }
}
