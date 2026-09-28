//! Current Project's keyed project property stream (`114/Props`).
//!
//! A 16-byte header (the stream length less 4, twice; `0x10000`; the entry
//! count) and then counted entries: a `u32` size, `u32` key, `u32` attribute
//! and `size` bytes of value, padded to an even length. The entries must end
//! exactly at the end of the stream.
use std::collections::HashMap;

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// Keyed values of a validated Props stream.
pub(crate) fn entries(b: &[u8]) -> Result<HashMap<u32, &[u8]>, String> {
    let header_len = (b.len() as u64).checked_sub(4);
    if b.len() < 16
        || Some(u32_at(b, 0) as u64) != header_len
        || u32_at(b, 4) != u32_at(b, 0)
        || u32_at(b, 8) != 0x10000
    {
        return Err("invalid project Props header".into());
    }
    let count = u32_at(b, 12) as usize;
    let mut out = HashMap::new();
    let mut o = 16usize;
    for _ in 0..count {
        if o + 12 > b.len() {
            return Err("project Props entry out of range".into());
        }
        let size = u32_at(b, o) as usize;
        let key = u32_at(b, o + 4);
        let Some(end) = (o + 12).checked_add(size).filter(|&n| n <= b.len()) else {
            return Err("project Props value out of range".into());
        };
        if out.insert(key, &b[o + 12..end]).is_some() {
            return Err(format!("duplicate project Props key {key:#x}"));
        }
        o = end + size % 2;
    }
    if o != b.len() {
        return Err("project Props entries do not fill the stream".into());
    }
    Ok(out)
}

/// `NewTasksAreManual`: a 2-byte value, `0`, `1`, or `0x00ff`.
pub(crate) const NEW_TASKS_ARE_MANUAL: u32 = 0x0240_13c8;
const PROJECT_START: u32 = 0x0240_0002;

/// Project StartDate in the same four-byte format as task timestamps.
pub(crate) fn project_start(b: &[u8]) -> Result<Option<String>, String> {
    let entries = entries(b)?;
    let Some(raw) = entries.get(&PROJECT_START) else {
        return Ok(None);
    };
    if raw.len() != 4 {
        return Err("invalid project StartDate in Props".into());
    }
    let time = u16::from_le_bytes([raw[0], raw[1]]);
    let days = u16::from_le_bytes([raw[2], raw[3]]);
    if days == 0xffff {
        return Ok(None);
    }
    if time != 0xffff && time >= 14400 {
        return Err("invalid project StartDate in Props".into());
    }
    crate::mpp::decode_timestamp(raw, 0)
        .ok_or_else(|| "invalid project StartDate in Props".into())
        .map(Some)
}
/// WBS code mask. Project writes four zero bytes for its ordinary numeric
/// outline mask in the paired corpus; absent also means no custom mask.
const WBS_CODE_MASK: u32 = 0x0240_138b;

pub(crate) fn has_default_wbs_mask(b: &[u8]) -> Result<bool, String> {
    Ok(matches!(
        entries(b)?.get(&WBS_CODE_MASK),
        None | Some(&[0, 0, 0, 0])
    ))
}

/// Project's default base calendar name, observed changing from Standard to
/// Night in the paired c1 calendar probe.
const DEFAULT_CALENDAR_NAME: u32 = 0x0240_000e;

pub(crate) fn default_calendar_name(b: &[u8]) -> Result<Option<String>, String> {
    let entries = entries(b)?;
    let Some(raw) = entries.get(&DEFAULT_CALENDAR_NAME) else {
        return Ok(None);
    };
    if raw.len() < 4 || !raw.len().is_multiple_of(2) {
        return Err("invalid default calendar name in Props".into());
    }
    let units: Vec<_> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let end = units
        .iter()
        .position(|&u| u == 0)
        .ok_or("unterminated default calendar name")?;
    if end == 0 || units[end..].iter().any(|&u| u != 0) {
        return Err("invalid default calendar name padding".into());
    }
    let name =
        String::from_utf16(&units[..end]).map_err(|_| "invalid UTF-16 default calendar name")?;
    Ok(Some(name))
}

pub(crate) fn new_tasks_are_manual(b: &[u8]) -> Result<bool, String> {
    match entries(b)?.get(&NEW_TASKS_ARE_MANUAL) {
        Some([0, 0]) => Ok(false),
        Some([1, 0] | [0xff, 0]) => Ok(true),
        Some(v) => Err(format!("unrecognized NewTasksAreManual value {v:02x?}")),
        None => Err("project Props has no NewTasksAreManual".into()),
    }
}

/// Build a Props stream, for tests.
#[cfg(test)]
pub(crate) fn stream(values: &[(u32, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (key, value) in values {
        body.extend_from_slice(&(value.len() as u32).to_le_bytes());
        body.extend_from_slice(&key.to_le_bytes());
        body.extend_from_slice(&2u32.to_le_bytes());
        body.extend_from_slice(value);
        if value.len() % 2 == 1 {
            body.push(0);
        }
    }
    let len = (body.len() + 12) as u32;
    let mut out = Vec::new();
    for v in [len, len, 0x10000, values.len() as u32] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_start_distinguishes_missing_na_valid_and_invalid_values() {
        assert_eq!(project_start(&stream(&[])), Ok(None));
        assert_eq!(
            project_start(&stream(&[(PROJECT_START, &[0xff; 4])])),
            Ok(None)
        );
        assert_eq!(
            project_start(&stream(&[(PROJECT_START, &[0xc0, 0x12, 0x86, 0x3a])])),
            Ok(Some("2025-01-06 08:00".into()))
        );
        for value in [&[0u8; 3][..], &[0x41, 0x38, 0x86, 0x3a][..]] {
            assert_eq!(
                project_start(&stream(&[(PROJECT_START, value)])),
                Err("invalid project StartDate in Props".into())
            );
        }
    }

    #[test]
    fn reads_the_new_task_default_and_refuses_what_it_does_not_know() {
        let odd: &[u8] = &[1, 2, 3];
        let off = stream(&[(1, odd), (NEW_TASKS_ARE_MANUAL, &[0, 0])]);
        assert_eq!(new_tasks_are_manual(&off), Ok(false));
        let on = stream(&[(NEW_TASKS_ARE_MANUAL, &[0xff, 0])]);
        assert_eq!(new_tasks_are_manual(&on), Ok(true));
        assert_eq!(
            new_tasks_are_manual(&stream(&[(NEW_TASKS_ARE_MANUAL, &[1, 0])])),
            Ok(true)
        );
        for value in [&[1, 1][..], &[2, 0], &[0, 1], &[1], &[1, 0, 0]] {
            assert!(
                new_tasks_are_manual(&stream(&[(NEW_TASKS_ARE_MANUAL, value)])).is_err(),
                "accepted {value:02x?}"
            );
        }
        assert!(new_tasks_are_manual(&stream(&[(1, &[0, 0])])).is_err());
        let mut short = on.clone();
        short.pop();
        assert!(new_tasks_are_manual(&short).is_err()); // header length disagrees
        let mut trailing = on.clone();
        trailing.extend_from_slice(&[0, 0]);
        let len = ((trailing.len() - 4) as u32).to_le_bytes();
        trailing[..4].copy_from_slice(&len);
        trailing[4..8].copy_from_slice(&len);
        assert!(new_tasks_are_manual(&trailing).is_err()); // bytes past the entries
        let dup = stream(&[
            (NEW_TASKS_ARE_MANUAL, &[0, 0]),
            (NEW_TASKS_ARE_MANUAL, &[0, 0]),
        ]);
        assert!(new_tasks_are_manual(&dup).is_err());
    }

    #[test]
    fn recognizes_default_and_custom_wbs_masks() {
        assert_eq!(
            has_default_wbs_mask(&stream(&[(WBS_CODE_MASK, &[0, 0, 0, 0])])),
            Ok(true)
        );
        assert_eq!(has_default_wbs_mask(&stream(&[])), Ok(true));
        assert_eq!(
            has_default_wbs_mask(&stream(&[(WBS_CODE_MASK, &[1, 0, 0, 0])])),
            Ok(false)
        );
    }
}
