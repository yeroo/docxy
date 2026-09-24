//! Counted FixedMeta index for task records.
use std::collections::HashSet;

#[derive(Clone, Copy, Debug)]
pub(crate) struct CurrentRecord {
    pub id: u32,
    pub uid: u32,
    pub offset: usize,
    pub len: usize,
    pub is_null: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LegacyRecord {
    pub id: u32,
    pub uid: u32,
    pub offset: usize,
    pub len: usize,
}

pub(crate) enum TaskIndex {
    Current(Vec<CurrentRecord>),
    Legacy(Vec<LegacyRecord>),
}

pub(crate) fn index(meta: &[u8], data: &[u8]) -> Result<TaskIndex, String> {
    if meta.len() < 16 || meta[..4] != [0xba, 0xad, 0xdf, 0xfa] {
        return Err("invalid FixedMeta header".into());
    }
    let count = u32::from_le_bytes(meta[8..12].try_into().unwrap()) as usize;
    if count < 3
        || 16usize.checked_add(count.checked_mul(47).ok_or("FixedMeta count overflow")?)
            != Some(meta.len())
    {
        return Err("FixedMeta count or length mismatch".into());
    }
    let offsets: Vec<_> = (0..count)
        .map(|i| {
            u32::from_le_bytes(meta[16 + i * 47 + 4..16 + i * 47 + 8].try_into().unwrap()) as usize
        })
        .collect();
    let stub = offsets[1];
    if !matches!(stub, 8 | 16)
        || offsets[0] != 0
        || offsets[2] != stub * 2
        || offsets.get(3).is_some_and(|&x| x != stub * 3)
        || offsets.windows(2).any(|w| w[0] >= w[1])
    {
        return Err("invalid FixedMeta offsets or schema stubs".into());
    }
    for (i, &off) in offsets.iter().enumerate() {
        if off >= data.len() {
            return Err(format!("FixedMeta record {i} offset out of range"));
        }
    }
    if stub == 16 {
        current_index(meta, data, &offsets).map(TaskIndex::Current)
    } else {
        legacy_index(data, &offsets).map(TaskIndex::Legacy)
    }
}

/// Current Project keeps active records in creation order. A 16-byte kind-4
/// record is a null grid row; a 202-byte kind-2 record is a deleted version.
fn current_index(
    meta: &[u8],
    data: &[u8],
    offsets: &[usize],
) -> Result<Vec<CurrentRecord>, String> {
    let mut rows = Vec::new();
    let mut ids = HashSet::new();
    let mut uids = HashSet::new();
    for i in 3..offsets.len() {
        let entry = &meta[16 + i * 47..16 + (i + 1) * 47];
        let kind = u16::from_le_bytes(entry[..2].try_into().unwrap());
        let begin = offsets[i];
        let end = offsets.get(i + 1).copied().unwrap_or(data.len());
        let len = end
            .checked_sub(begin)
            .ok_or_else(|| format!("FixedMeta record {i} offset out of range"))?;
        let (id, uid, is_null) = match (kind, len) {
            (0, 202) => (
                u32::from_le_bytes(data[begin..begin + 4].try_into().unwrap()),
                u32::from_le_bytes(data[begin + 4..begin + 8].try_into().unwrap()),
                false,
            ),
            (4, 16) => (
                u32::from_le_bytes(data[begin + 4..begin + 8].try_into().unwrap()),
                u32::from_le_bytes(data[begin..begin + 4].try_into().unwrap()),
                true,
            ),
            (2, 202) => continue,
            _ => {
                return Err(format!(
                    "unrecognized FixedMeta kind {kind} with record length {len}"
                ));
            }
        };
        if id > i32::MAX as u32 || uid > i32::MAX as u32 || !ids.insert(id) || !uids.insert(uid) {
            return Err(format!(
                "duplicate or out-of-range task ID {id} / UID {uid}"
            ));
        }
        rows.push(CurrentRecord {
            id,
            uid,
            offset: begin,
            len,
            is_null,
        });
    }
    rows.sort_by_key(|r| r.id);
    if rows.iter().enumerate().any(|(i, r)| r.id as usize != i) {
        return Err("task IDs are not contiguous from zero".into());
    }
    Ok(rows)
}

fn legacy_index(data: &[u8], offsets: &[usize]) -> Result<Vec<LegacyRecord>, String> {
    let mut rows = Vec::new();
    let mut ids = HashSet::new();
    let mut uids = HashSet::new();
    let mut record_len = None;
    for i in 3..offsets.len() {
        let begin = offsets[i];
        let end = offsets.get(i + 1).copied().unwrap_or(data.len());
        let len = end
            .checked_sub(begin)
            .ok_or_else(|| format!("FixedMeta record {i} offset out of range"))?;
        if len < 176 || record_len.is_some_and(|n| n != len) {
            return Err(format!(
                "FixedMeta task record length {len} is inconsistent"
            ));
        }
        record_len = Some(len);
        let uid = u32::from_le_bytes(data[begin..begin + 4].try_into().unwrap());
        let id = u32::from_le_bytes(data[begin + 4..begin + 8].try_into().unwrap());
        if id > i32::MAX as u32 || uid > i32::MAX as u32 || !ids.insert(id) || !uids.insert(uid) {
            return Err(format!(
                "duplicate or out-of-range task ID {id} / UID {uid}"
            ));
        }
        rows.push(LegacyRecord {
            id,
            uid,
            offset: begin,
            len,
        });
    }
    rows.sort_by_key(|r| r.id);
    if rows.iter().enumerate().any(|(i, r)| r.id as usize != i) {
        return Err("task IDs are not contiguous from zero".into());
    }
    Ok(rows)
}
