//! Counted FixedMeta index for task records.
use std::collections::HashSet;

#[derive(Clone, Copy, Debug)]
pub(crate) struct CurrentRecord {
    /// Position of this record's FixedMeta entry, which also indexes Fixed2Meta.
    pub entry: usize,
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
            entry: i,
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

/// Length of a current Project `Fixed2Data` task record.
pub(crate) const FIXED2_LEN: usize = 64;
const FIXED2_META_LEN: usize = 96;

/// A task's current Project `Fixed2Meta` entry and `Fixed2Data` record.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Fixed2<'a> {
    pub meta: &'a [u8],
    pub data: &'a [u8],
}

/// Pair each current row with its second fixed block. Fixed2Meta counts the
/// same entries as FixedMeta, in the same order, one 64-byte record each. The
/// records carry no UID, so the pairing is checked another way: a task
/// record holds its GUID at +0 and its row sort key (an f64, fractional for
/// inserted rows) at +16, and a blank row holds neither. Sorted by task ID,
/// the tasks' sort keys must strictly increase.
pub(crate) fn current_fixed2<'a>(
    meta: &'a [u8],
    data: &'a [u8],
    fixed_count: usize,
    rows: &[CurrentRecord],
) -> Result<Vec<Fixed2<'a>>, String> {
    let u32_at = |o: usize| u32::from_le_bytes(meta[o..o + 4].try_into().unwrap()) as usize;
    if meta.len() < 16 || meta[..4] != [0xba, 0xad, 0xdf, 0xfa] {
        return Err("invalid Fixed2Meta header".into());
    }
    let count = u32_at(8);
    if count != fixed_count
        || Some(meta.len()) != count.checked_mul(FIXED2_META_LEN).map(|n| n + 16)
        || u32_at(12) != data.len()
        || Some(data.len()) != count.checked_mul(FIXED2_LEN)
    {
        return Err("Fixed2Meta count or Fixed2Data length mismatch".into());
    }
    if (0..count).any(|i| u32_at(16 + i * FIXED2_META_LEN + 4) != i * FIXED2_LEN) {
        return Err("invalid Fixed2Meta offsets".into());
    }
    let mut last_key = 0f64;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let rec = &data[row.entry * FIXED2_LEN..(row.entry + 1) * FIXED2_LEN];
        let blank_guid = rec[..16].iter().all(|&b| b == 0);
        let key = f64::from_le_bytes(rec[16..24].try_into().unwrap());
        if row.is_null {
            if !blank_guid || key != 0.0 {
                return Err(format!("Fixed2Data blank row {} carries a task", row.id));
            }
        } else if blank_guid || !key.is_finite() || key <= last_key {
            return Err(format!(
                "Fixed2Data record out of step with task UID {}",
                row.uid
            ));
        } else {
            last_key = key;
        }
        let m = 16 + row.entry * FIXED2_META_LEN;
        out.push(Fixed2 {
            meta: &meta[m..m + FIXED2_META_LEN],
            data: rec,
        });
    }
    Ok(out)
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
