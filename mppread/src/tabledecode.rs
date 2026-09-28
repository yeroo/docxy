//! Strict readers for the current Project resource and assignment tables.
use crate::cfb::Cfb;
use std::collections::{HashMap, HashSet};

pub(crate) fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
pub(crate) fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
pub(crate) fn i32_at(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
pub(crate) fn f64_at(b: &[u8], at: usize) -> f64 {
    f64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

pub(crate) struct Table {
    pub rows: Vec<Vec<u8>>,
    pub fields: HashMap<(u32, u16), Vec<u8>>,
}

pub(crate) fn read(
    bytes: &[u8],
    name: &str,
    meta_stride: usize,
    row_len: usize,
    marker: u16,
) -> Result<Option<Table>, String> {
    let cfb = Cfb::open(bytes)?;
    let paths = cfb.paths();
    let suffix = format!("{name}/FixedMeta");
    let any = paths.iter().any(|p| p.contains(&format!("{name}/")));
    if !any {
        return Ok(None);
    }
    let path = paths
        .iter()
        .find(|p| p.ends_with(&suffix))
        .ok_or_else(|| format!("partial {name} stream set: missing FixedMeta"))?;
    let prefix = path.trim_end_matches("FixedMeta");
    let get = |stream: &str| {
        cfb.read_path(&format!("{prefix}{stream}"))
            .ok_or_else(|| format!("partial {name} stream set: missing {stream}"))
    };
    let (fm, fd, vm, v2) = (
        get("FixedMeta")?,
        get("FixedData")?,
        get("VarMeta")?,
        get("Var2Data")?,
    );
    if fm.len() < 16 || fm[..4] != [0xba, 0xad, 0xdf, 0xfa] {
        return Err(format!("{name}: invalid FixedMeta header"));
    }
    let count = u32_at(&fm, 8) as usize;
    let stubs = if name == "TBkndRsc" { 3 } else { 0 };
    let data_len = count
        .checked_sub(stubs)
        .and_then(|n| n.checked_mul(row_len))
        .and_then(|n| n.checked_add(stubs * 16));
    if 16usize.checked_add(
        count
            .checked_mul(meta_stride)
            .ok_or("FixedMeta count overflow")?,
    ) != Some(fm.len())
        || data_len != Some(fd.len())
    {
        return Err(format!(
            "{name}: FixedMeta count or FixedData length mismatch"
        ));
    }
    let mut rows = Vec::with_capacity(count);
    let mut uids = HashSet::new();
    for i in 0..count {
        let m = &fm[16 + i * meta_stride..16 + (i + 1) * meta_stride];
        let off = u32_at(m, 4) as usize;
        let expected_off = if i < stubs {
            i * 16
        } else {
            stubs * 16 + (i - stubs) * row_len
        };
        let kind = u16_at(m, 0);
        let kind_ok = if i < stubs {
            kind == 4
        } else if name == "TBkndAssn" {
            matches!(kind, 0 | 2)
        } else {
            kind == 0
        };
        if off != expected_off || !kind_ok {
            return Err(format!("{name}: unrecognized FixedMeta record {i}"));
        }
        let len = if i < stubs { 16 } else { row_len };
        let row = fd[off..off + len].to_vec();
        let uid = u32_at(&row, 0);
        if i >= stubs && !uids.insert(uid) {
            return Err(format!("{name}: duplicate UID {uid}"));
        }
        rows.push(row);
    }
    if vm.len() < 24 || vm[..4] != [0xba, 0xad, 0xdf, 0xfa] {
        return Err(format!("{name}: invalid VarMeta header"));
    }
    let vars = u32_at(&vm, 8) as usize;
    if 24usize
        .checked_add(vars.checked_mul(12).ok_or("VarMeta count overflow")?)
        .is_none_or(|n| n > vm.len())
        || u32_at(&vm, 20) as usize != v2.len()
    {
        return Err(format!("{name}: VarMeta count or Var2Data length mismatch"));
    }
    let mut fields = HashMap::new();
    for i in 0..vars {
        let entry = &vm[24 + i * 12..24 + (i + 1) * 12];
        let (uid, off, key) = (
            u32_at(entry, 0),
            u32_at(entry, 4) as usize,
            u16_at(entry, 8),
        );
        if u16_at(entry, 10) != marker || !uids.contains(&uid) {
            return Err(format!("{name}: invalid VarMeta entry {i}"));
        }
        let Some(head) = off.checked_add(4).filter(|&n| n <= v2.len()) else {
            return Err(format!("{name}: Var2Data offset at entry {i}"));
        };
        let len = u32_at(&v2, off) as usize;
        let Some(end) = head.checked_add(len).filter(|&n| n <= v2.len()) else {
            return Err(format!("{name}: Var2Data length at entry {i}"));
        };
        if fields.insert((uid, key), v2[head..end].to_vec()).is_some() {
            return Err(format!("{name}: duplicate VarMeta key ({uid},{key})"));
        }
    }
    Ok(Some(Table { rows, fields }))
}

pub(crate) fn name(bytes: &[u8], uid: u32) -> Result<String, String> {
    if bytes.len() < 2 || !bytes.len().is_multiple_of(2) || bytes[bytes.len() - 2..] != [0, 0] {
        return Err(format!("invalid resource name for UID {uid}"));
    }
    let units: Vec<_> = bytes[..bytes.len() - 2]
        .chunks_exact(2)
        .map(|x| u16::from_le_bytes([x[0], x[1]]))
        .collect();
    let value =
        String::from_utf16(&units).map_err(|_| format!("invalid resource name for UID {uid}"))?;
    if value.chars().any(char::is_control) {
        return Err(format!("invalid resource name for UID {uid}"));
    }
    Ok(value)
}
