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

/// `NewTasksAreManual`: a 2-byte value, `0` or `0x00ff`.
pub(crate) const NEW_TASKS_ARE_MANUAL: u32 = 0x0240_13c8;

pub(crate) fn new_tasks_are_manual(b: &[u8]) -> Result<bool, String> {
    match entries(b)?.get(&NEW_TASKS_ARE_MANUAL) {
        Some([0, 0]) => Ok(false),
        Some([0xff, 0]) => Ok(true),
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
    fn reads_the_new_task_default_and_refuses_what_it_does_not_know() {
        let odd: &[u8] = &[1, 2, 3];
        let off = stream(&[(1, odd), (NEW_TASKS_ARE_MANUAL, &[0, 0])]);
        assert_eq!(new_tasks_are_manual(&off), Ok(false));
        let on = stream(&[(NEW_TASKS_ARE_MANUAL, &[0xff, 0])]);
        assert_eq!(new_tasks_are_manual(&on), Ok(true));
        assert!(new_tasks_are_manual(&stream(&[(NEW_TASKS_ARE_MANUAL, &[1, 0])])).is_err());
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
}
