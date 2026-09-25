//! Validated task-table decoding for current Project and MPP9 storage.
use crate::{
    cfb::Cfb,
    fixedmeta,
    mpp::{MppPred, MppTask, decode_timestamp},
};
use std::collections::{HashMap, HashSet};

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
struct TaskLayout {
    length: usize,
    start: usize,
    finish: usize,
    level: usize,
}
const NEWEST: TaskLayout = TaskLayout {
    length: 202,
    start: 0x68,
    finish: 0x6c,
    level: 172,
};
/// Manual scheduling in the newest layout, found by saving one plan with one
/// task auto and then manual (corpus/tools/gen_mpp_manual_cases.py). The mode
/// is a Fixed2Meta flag; the manual start, finish and duration sit in the
/// task's Fixed2Data record, the duration in tenths of a minute followed by
/// its MSPDI DurationFormat code.
const MANUAL_FLAG: (usize, u8) = (8, 0x80);
const MANUAL_START: usize = 50;
const MANUAL_FINISH: usize = 54;
const MANUAL_DURATION: usize = 58;
const MANUAL_DURATION_FORMAT: usize = 62;
const LEGACY: TaskLayout = TaskLayout {
    length: 264,
    start: 88,
    finish: 92,
    level: 40,
};
struct LinkLayout {
    lag: usize,
    format: usize,
    formats: &'static [u16],
}
const NEWEST_LINK: LinkLayout = LinkLayout {
    lag: 14,
    format: 18,
    formats: &[3, 7],
};
// The three MPP9 corpus files have format 7 and zero lag throughout. The
// +16/+14 positions follow their 20-byte records; nonzero lag has no oracle.
const LEGACY_LINK: LinkLayout = LinkLayout {
    lag: 16,
    format: 14,
    formats: &[7],
};

fn links(cfb: &Cfb, prefix: &str, out: &mut [MppTask], layout: LinkLayout) -> Result<(), String> {
    let cons_path = prefix.replace("TBkndTask/", "TBkndCons/FixedData");
    let Some(cons) = cfb.read_path(&cons_path) else {
        return Ok(());
    };
    if !cons.len().is_multiple_of(20) {
        return Err("link record length mismatch".into());
    }
    let positions: HashMap<_, _> = out.iter().enumerate().map(|(i, t)| (t.uid, i)).collect();
    for rec in cons.as_chunks::<20>().0 {
        let pred_uid = u32_at(rec, 4);
        let succ_uid = u32_at(rec, 8);
        if pred_uid == 0 || succ_uid == 0 {
            return Err("link endpoint UID 0 is the project summary".into());
        }
        let kind = u16_at(rec, 12);
        let lag = i32::from_le_bytes(rec[layout.lag..layout.lag + 4].try_into().unwrap());
        let format = u16_at(rec, layout.format);
        if !positions.contains_key(&pred_uid) || !positions.contains_key(&succ_uid) {
            return Err(format!(
                "link refers to unknown UID {pred_uid} or {succ_uid}"
            ));
        }
        if kind > 3 || !layout.formats.contains(&format) {
            return Err(format!(
                "unsupported link type {kind} or LagFormat {format}"
            ));
        }
        let succ = positions[&succ_uid];
        out[succ].predecessors.push(MppPred {
            pred_uid,
            kind: kind as u8,
            lag_min: (lag as f64 / 10.0).round() as i64,
        });
    }
    Ok(())
}

fn decode_name(v2: &[u8], off: usize, uid: u32) -> Result<String, String> {
    let Some(header_end) = off.checked_add(4).filter(|&n| n <= v2.len()) else {
        return Err(format!("Var2Data offset out of range for UID {uid}"));
    };
    let len = u32_at(v2, off) as usize;
    let Some(end) = header_end.checked_add(len).filter(|&n| n <= v2.len()) else {
        return Err(format!("Var2Data block out of range for UID {uid}"));
    };
    let value = &v2[header_end..end];
    if value.len() < 4 || !value.len().is_multiple_of(2) {
        return Err(format!("invalid task name block for UID {uid}"));
    }
    let units: Vec<u16> = value
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    if *units.last().unwrap() != 0 {
        return Err(format!("unterminated task name for UID {uid}"));
    }
    let name = String::from_utf16(&units[..units.len() - 1])
        .map_err(|_| format!("invalid UTF-16 task name for UID {uid}"))?;
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err(format!("invalid task name for UID {uid}"));
    }
    Ok(name)
}

fn validate_legacy_level(index: usize, uid: u32, level: u32, previous: u32) -> Result<(), String> {
    if (index == 0 && (uid != 0 || level != 0))
        || level > 20
        || (index > 0 && (level == 0 || level > previous + 1))
    {
        return Err(format!(
            "invalid legacy outline level {level} for UID {uid}"
        ));
    }
    Ok(())
}

fn names(vm: &[u8], v2: &[u8], uids: &HashSet<u32>) -> Result<HashMap<u32, String>, String> {
    if vm.len() < 24 || vm[..4] != [0xba, 0xad, 0xdf, 0xfa] {
        return Err("invalid VarMeta header".into());
    }
    let count = u32_at(vm, 8) as usize;
    if 24usize
        .checked_add(count.checked_mul(12).ok_or("VarMeta count overflow")?)
        .is_none_or(|n| n > vm.len())
        || u32_at(vm, 20) as usize != v2.len()
    {
        return Err("VarMeta count or Var2Data length mismatch".into());
    }
    let mut seen = HashSet::new();
    let mut names = HashMap::new();
    for i in 0..count {
        let e = &vm[24 + i * 12..24 + (i + 1) * 12];
        let uid = u32_at(e, 0);
        let off = u32_at(e, 4) as usize;
        let key = u16_at(e, 8);
        if u16_at(e, 10) != 0x0b40 || !uids.contains(&uid) {
            return Err(format!("invalid VarMeta entry {i}"));
        }
        if !seen.insert((uid, key)) {
            return Err(format!("duplicate VarMeta key ({uid},{key})"));
        }
        let Some(header_end) = off.checked_add(4).filter(|&n| n <= v2.len()) else {
            return Err(format!("Var2Data offset out of range at entry {i}"));
        };
        let len = u32_at(v2, off) as usize;
        let Some(_end) = header_end.checked_add(len).filter(|&n| n <= v2.len()) else {
            return Err(format!("Var2Data block out of range at entry {i}"));
        };
        if key == 0x000e {
            names.insert(uid, decode_name(v2, off, uid)?);
        }
    }
    if names.len() != uids.len() {
        return Err("task names do not cover the FixedMeta UIDs".into());
    }
    Ok(names)
}

fn legacy_names(vm: &[u8], v2: &[u8], uids: &HashSet<u32>) -> Result<HashMap<u32, String>, String> {
    if vm.len() < 24 || vm[..4] != [0xba, 0xad, 0xdf, 0xfa] {
        return Err("unrecognized legacy VarMeta layout".into());
    }
    let count = u32_at(vm, 8) as usize;
    let end = 24usize
        .checked_add(
            count
                .checked_mul(8)
                .ok_or("legacy VarMeta count overflow")?,
        )
        .ok_or("legacy VarMeta count overflow")?;
    if end > vm.len() || u32_at(vm, 20) as usize != v2.len() {
        return Err("legacy VarMeta count or Var2Data length mismatch".into());
    }
    let mut seen = HashSet::new();
    let mut out = HashMap::new();
    for e in vm[24..end].as_chunks::<8>().0 {
        let uid = u16_at(e, 0) as u32;
        let key = u16_at(e, 2);
        let off = u32_at(e, 4) as usize;
        if off.checked_add(4).is_none_or(|n| n > v2.len()) {
            return Err("legacy Var2Data offset out of range".into());
        }
        let len = u32_at(v2, off) as usize;
        if off
            .checked_add(4)
            .and_then(|n| n.checked_add(len))
            .is_none_or(|n| n > v2.len())
        {
            return Err("legacy Var2Data block out of range".into());
        }
        if key == 0x0b00 {
            if !seen.insert((uid, key)) {
                return Err(format!("duplicate legacy task name for UID {uid}"));
            }
            if !uids.contains(&uid) {
                return Err(format!("legacy task name references unknown UID {uid}"));
            }
            out.insert(uid, decode_name(v2, off, uid)?);
        }
    }
    if out.len() != uids.len() {
        return Err(format!(
            "legacy task names cover {}/{} UIDs",
            out.len(),
            uids.len()
        ));
    }
    Ok(out)
}

/// A manual task's stored start, finish and duration.
type ManualFields = (Option<String>, Option<String>, Option<i64>);

fn manual_fields(rec: &[u8], uid: u32) -> Result<ManualFields, String> {
    let start = decode_timestamp(rec, MANUAL_START);
    let finish = decode_timestamp(rec, MANUAL_FINISH);
    if start
        .as_ref()
        .zip(finish.as_ref())
        .is_some_and(|(s, f)| s > f)
    {
        return Err(format!("inverted manual dates for UID {uid}"));
    }
    let raw = u32_at(rec, MANUAL_DURATION);
    if raw == u32::MAX {
        return Ok((start, finish, None));
    }
    if raw > i32::MAX as u32 {
        return Err(format!("invalid manual duration for UID {uid}"));
    }
    // DurationFormat without its estimated (`?`) bit.
    let duration = match u16_at(rec, MANUAL_DURATION_FORMAT) & !32 {
        // Minutes, hours, days, weeks, months, or blank: working time.
        3 | 5 | 7 | 9 | 11 | 21 => Some(raw as i64 / 10),
        // Elapsed units have no oracle yet: keep the duration unknown.
        4 | 6 | 8 | 10 | 12 => None,
        format => {
            return Err(format!(
                "unrecognized manual DurationFormat {format} for UID {uid}"
            ));
        }
    };
    Ok((start, finish, duration))
}

/// A validated task table and the project's new-task mode. The mode is kept
/// as its own result so a bad project option refuses an import but not
/// [`decode`].
pub(crate) struct Table {
    pub tasks: Vec<MppTask>,
    pub new_tasks_are_manual: Result<bool, String>,
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Vec<MppTask>, String> {
    decode_table(bytes).map(|table| table.tasks)
}

/// The project's new-task mode; see [`crate::mpp::decode_new_tasks_are_manual`].
pub(crate) fn new_tasks_are_manual(bytes: &[u8]) -> Result<bool, String> {
    decode_table(bytes)?.new_tasks_are_manual
}

/// Decode the task table. MPP9 predates manual tasks and a file without a
/// task table has nothing to default, so both have an auto default; the
/// newest layout reads it from the project Props beside the table.
pub(crate) fn decode_table(bytes: &[u8]) -> Result<Table, String> {
    let cfb = Cfb::open(bytes)?;
    let paths = cfb.paths();
    let task_paths: Vec<_> = paths.iter().filter(|p| p.contains("TBkndTask/")).collect();
    if task_paths.is_empty() {
        return Ok(Table {
            tasks: Vec::new(),
            new_tasks_are_manual: Ok(false),
        });
    }
    let Some(meta_path) = paths.iter().find(|p| p.ends_with("TBkndTask/FixedMeta")) else {
        return Err("partial task stream set: missing FixedMeta".into());
    };
    let prefix = meta_path.trim_end_matches("FixedMeta");
    let read = |name: &str| {
        cfb.read_path(&format!("{prefix}{name}"))
            .ok_or_else(|| format!("partial task stream set: missing {name}"))
    };
    let fm = read("FixedMeta")?;
    let fd = read("FixedData")?;
    let vm = read("VarMeta")?;
    let v2 = read("Var2Data")?;
    match fixedmeta::index(&fm, &fd)? {
        fixedmeta::TaskIndex::Current(indexed) => {
            let fixed_count = u32_at(&fm, 8) as usize;
            let (f2m, f2d) = (read("Fixed2Meta")?, read("Fixed2Data")?);
            let fixed2 = fixedmeta::current_fixed2(&f2m, &f2d, fixed_count, &indexed)?;
            let tasks = decode_current(&cfb, prefix, &fd, &vm, &v2, indexed, &fixed2)?;
            let props = prefix.trim_end_matches("TBkndTask/").to_string() + "Props";
            let new_tasks_are_manual = cfb
                .read_path(&props)
                .ok_or_else(|| "missing project Props stream".to_string())
                .and_then(|props| crate::props::new_tasks_are_manual(&props));
            Ok(Table {
                tasks,
                new_tasks_are_manual,
            })
        }
        fixedmeta::TaskIndex::Legacy(indexed) => {
            let uids: HashSet<_> = indexed.iter().map(|r| r.uid).collect();
            Ok(Table {
                tasks: decode_legacy(&cfb, prefix, &fd, &vm, &v2, indexed, &uids)?,
                new_tasks_are_manual: Ok(false),
            })
        }
    }
}

fn decode_current(
    cfb: &Cfb,
    prefix: &str,
    fd: &[u8],
    vm: &[u8],
    v2: &[u8],
    indexed: Vec<fixedmeta::CurrentRecord>,
    fixed2: &[fixedmeta::Fixed2],
) -> Result<Vec<MppTask>, String> {
    let uids: HashSet<_> = indexed
        .iter()
        .filter(|r| !r.is_null)
        .map(|r| r.uid)
        .collect();
    let named = names(vm, v2, &uids)?;
    let mut out = Vec::new();
    for (row, fixed2) in indexed.into_iter().zip(fixed2) {
        if row.is_null {
            continue;
        }
        if row.len != NEWEST.length {
            return Err(format!("unrecognized task record length {}", row.len));
        }
        let rec = &fd[row.offset..row.offset + row.len];
        let start = decode_timestamp(rec, NEWEST.start);
        let finish = decode_timestamp(rec, NEWEST.finish);
        let level = rec[NEWEST.level] as u32;
        if level > 20 {
            return Err(format!("invalid outline level {level} for UID {}", row.uid));
        }
        if start
            .as_ref()
            .zip(finish.as_ref())
            .is_none_or(|(s, f)| s > f)
        {
            return Err(format!(
                "missing or inverted task dates for UID {}",
                row.uid
            ));
        }
        let manual = fixed2.meta[MANUAL_FLAG.0] & MANUAL_FLAG.1 != 0;
        let (manual_start, manual_finish, manual_duration_min) = if manual {
            manual_fields(fixed2.data, row.uid)?
        } else {
            (None, None, None)
        };
        out.push(MppTask {
            id: row.id,
            uid: row.uid,
            name: named[&row.uid].clone(),
            start,
            finish,
            outline_level: Some(level),
            predecessors: Vec::new(),
            manual,
            manual_start,
            manual_finish,
            manual_duration_min,
        });
    }
    links(cfb, prefix, &mut out, NEWEST_LINK)?;
    Ok(out)
}

fn decode_legacy(
    cfb: &Cfb,
    prefix: &str,
    fd: &[u8],
    vm: &[u8],
    v2: &[u8],
    indexed: Vec<fixedmeta::LegacyRecord>,
    uids: &HashSet<u32>,
) -> Result<Vec<MppTask>, String> {
    let named = legacy_names(vm, v2, uids)?;
    let mut out = Vec::new();
    let mut previous_level = 0u32;
    for (i, row) in indexed.iter().enumerate() {
        if row.len != LEGACY.length {
            return Err("legacy task record length mismatch".into());
        }
        let uid = row.uid;
        let rec = &fd[row.offset..row.offset + row.len];
        let level = rec[LEGACY.level] as u32;
        validate_legacy_level(i, uid, level, previous_level)?;
        previous_level = level;
        let start = decode_timestamp(rec, LEGACY.start);
        let finish = decode_timestamp(rec, LEGACY.finish);
        if start
            .as_ref()
            .zip(finish.as_ref())
            .is_none_or(|(s, f)| s > f)
        {
            return Err(format!(
                "missing or inverted legacy task dates for UID {uid}"
            ));
        }
        out.push(MppTask {
            id: row.id,
            uid,
            name: named[&uid].clone(),
            start,
            finish,
            outline_level: Some(level),
            predecessors: Vec::new(),
            // Project 2003 has no manual scheduling.
            ..MppTask::default()
        });
    }
    links(cfb, prefix, &mut out, LEGACY_LINK)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfb::{Node, write_cfb_tree};

    #[test]
    fn legacy_accepts_short_non_latin_names_and_flat_children() {
        let mut block = 6u32.to_le_bytes().to_vec();
        for unit in "中文".encode_utf16() {
            block.extend_from_slice(&unit.to_le_bytes());
        }
        block.extend_from_slice(&0u16.to_le_bytes());
        assert_eq!(decode_name(&block, 0, 1).unwrap(), "中文");
        let mut previous = 0;
        for (i, level) in [0, 1, 2, 2].into_iter().enumerate() {
            validate_legacy_level(i, i as u32, level, previous).unwrap();
            previous = level;
        }
        assert!(validate_legacy_level(2, 2, 3, 1).is_err());
    }

    struct Streams {
        fm: Vec<u8>,
        fd: Vec<u8>,
        vm: Vec<u8>,
        v2: Vec<u8>,
        cons: Vec<u8>,
        f2m: Vec<u8>,
        f2d: Vec<u8>,
        props: Vec<u8>,
    }
    fn fixture() -> Streams {
        let mut fd = vec![0u8; 452];
        fd[0..4].copy_from_slice(&0u32.to_le_bytes());
        fd[16..20].copy_from_slice(&1u32.to_le_bytes());
        fd[32..36].copy_from_slice(&2u32.to_le_bytes());
        for (i, uid) in [0u32, 1].into_iter().enumerate() {
            let o = 48 + i * 202;
            fd[o..o + 4].copy_from_slice(&uid.to_le_bytes());
            fd[o + 4..o + 8].copy_from_slice(&uid.to_le_bytes());
            fd[o + 172] = i as u8;
            for d in [0x68, 0x6c] {
                fd[o + d..o + d + 4].copy_from_slice(&[0xc0, 0x12, 0x86, 0x3a]);
            }
        }
        let mut fm = vec![0u8; 16 + 5 * 47];
        fm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        fm[8..12].copy_from_slice(&5u32.to_le_bytes());
        for (i, off) in [0u32, 16, 32, 48, 250].into_iter().enumerate() {
            let p = 16 + i * 47;
            fm[p + 4..p + 8].copy_from_slice(&off.to_le_bytes());
            if i < 3 {
                fm[p..p + 2].copy_from_slice(&4u16.to_le_bytes());
            }
        }
        let mut vm = vec![0u8; 24];
        vm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        vm[8..12].copy_from_slice(&2u32.to_le_bytes());
        let mut v2 = Vec::new();
        for (uid, name) in [(0u32, 'A'), (1u32, 'B')] {
            let off = v2.len() as u32;
            v2.extend_from_slice(&4u32.to_le_bytes());
            v2.extend_from_slice(&(name as u16).to_le_bytes());
            v2.extend_from_slice(&0u16.to_le_bytes());
            vm.extend_from_slice(&uid.to_le_bytes());
            vm.extend_from_slice(&off.to_le_bytes());
            vm.extend_from_slice(&0x000eu16.to_le_bytes());
            vm.extend_from_slice(&0x0b40u16.to_le_bytes());
        }
        vm[20..24].copy_from_slice(&(v2.len() as u32).to_le_bytes());
        let (f2m, f2d) = fixed2_for(&fm);
        Streams {
            fm,
            fd,
            vm,
            v2,
            cons: Vec::new(),
            f2m,
            f2d,
            props: props(&[0, 0]),
        }
    }
    /// Fixed2 streams that match `fm`: a GUID and a rising sort key for each
    /// task entry, nothing for the schema stubs and blank rows.
    fn fixed2_for(fm: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let count = (fm.len() - 16) / 47;
        let mut meta = vec![0u8; 16 + count * 96];
        meta[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        meta[8..12].copy_from_slice(&(count as u32).to_le_bytes());
        meta[12..16].copy_from_slice(&((count * 64) as u32).to_le_bytes());
        let mut data = vec![0u8; count * 64];
        for i in 0..count {
            let m = 16 + i * 96;
            meta[m + 4..m + 8].copy_from_slice(&((i * 64) as u32).to_le_bytes());
            if i >= 3 && fm[16 + i * 47..16 + i * 47 + 2] == [0, 0] {
                data[i * 64] = i as u8;
                data[i * 64 + 16..i * 64 + 24].copy_from_slice(&(i as f64).to_le_bytes());
            }
        }
        (meta, data)
    }
    fn props(new_tasks_are_manual: &[u8]) -> Vec<u8> {
        crate::props::stream(&[(crate::props::NEW_TASKS_ARE_MANUAL, new_tasks_are_manual)])
    }
    fn file(s: &Streams, include_fd: bool) -> Vec<u8> {
        let mut task = vec![
            Node::Stream("FixedMeta", s.fm.clone()),
            Node::Stream("VarMeta", s.vm.clone()),
            Node::Stream("Var2Data", s.v2.clone()),
            Node::Stream("Fixed2Meta", s.f2m.clone()),
            Node::Stream("Fixed2Data", s.f2d.clone()),
        ];
        if include_fd {
            task.push(Node::Stream("FixedData", s.fd.clone()));
        }
        write_cfb_tree(&[Node::Storage(
            "   114",
            vec![
                Node::Stream("Props", s.props.clone()),
                Node::Storage("TBkndTask", task),
                Node::Storage("TBkndCons", vec![Node::Stream("FixedData", s.cons.clone())]),
            ],
        )])
    }
    fn reject(s: &Streams) {
        assert!(decode(&file(s, true)).is_err());
    }

    #[test]
    fn keyed_single_letter_names_and_declared_count() {
        let mut s = fixture();
        let tasks = decode(&file(&s, true)).unwrap();
        assert_eq!(
            tasks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["A", "B"]
        );
        // Stale bytes after the declared table have no standing as entries.
        s.vm.extend_from_within(24..36);
        assert!(decode(&file(&s, true)).is_ok());
        s.vm[8..12].copy_from_slice(&4u32.to_le_bytes());
        reject(&s);
    }
    #[test]
    fn null_rows_are_identified_by_fixedmeta_kind_and_count_for_id_gaps() {
        let mut s = fixture();
        s.fd[250..254].copy_from_slice(&2u32.to_le_bytes()); // real B is row 2
        let mut null = [0u8; 16];
        null[0..4].copy_from_slice(&4u32.to_le_bytes()); // null UID
        null[4..8].copy_from_slice(&1u32.to_le_bytes()); // null row ID
        s.fd.extend_from_slice(&null);
        s.fm[8..12].copy_from_slice(&6u32.to_le_bytes());
        s.fm.extend_from_slice(&[0u8; 47]);
        let p = 16 + 5 * 47;
        s.fm[p..p + 2].copy_from_slice(&4u16.to_le_bytes());
        s.fm[p + 4..p + 8].copy_from_slice(&452u32.to_le_bytes());
        (s.f2m, s.f2d) = fixed2_for(&s.fm);
        let decoded = decode(&file(&s, true)).unwrap();
        assert_eq!(
            decoded.iter().map(|t| (t.id, t.uid)).collect::<Vec<_>>(),
            [(0, 0), (2, 1)]
        );
        s.fm[p..p + 2].copy_from_slice(&0u16.to_le_bytes());
        reject(&s); // a short record without the null marker is malformed
        s.fm[p..p + 2].copy_from_slice(&4u16.to_le_bytes());
        s.fd[452..456].copy_from_slice(&1u32.to_le_bytes());
        reject(&s); // duplicate null UID is malformed
        s.fd[452..456].copy_from_slice(&4u32.to_le_bytes());
        s.fd[456..460].copy_from_slice(&4u32.to_le_bytes());
        reject(&s); // task IDs must be contiguous even across null rows
    }
    #[test]
    fn conversion_preserves_uid_and_uses_summary_name() {
        let mut s = fixture();
        s.fd[254..258].copy_from_slice(&7u32.to_le_bytes());
        s.vm[36..40].copy_from_slice(&7u32.to_le_bytes());
        let project = crate::project::project_from_mpp(&file(&s, true)).unwrap();
        assert_eq!(project.name, "A");
        assert_eq!(project.tasks.len(), 1);
        assert_eq!(project.tasks[0].uid, 7);
        assert_eq!(project.tasks[0].name, "B");
        let err = crate::project::project_from_mpp(&file(&s, false)).unwrap_err();
        assert!(err.starts_with("cannot read the task table of this .mpp ("));
    }
    #[test]
    fn refuses_structural_corruption() {
        let mut s = fixture();
        s.v2[4] = 1;
        reject(&s); // binary/control name
        let mut s = fixture();
        s.fd[250 + 0x68 + 2..250 + 0x68 + 4].copy_from_slice(&0xffffu16.to_le_bytes());
        reject(&s); // a task with no start date cannot be imported
        let mut s = fixture();
        s.fd[254..258].copy_from_slice(&0u32.to_le_bytes());
        reject(&s); // duplicate UID
        let mut s = fixture();
        s.fd[254..258].copy_from_slice(&u32::MAX.to_le_bytes());
        reject(&s); // UID cannot fit projcore
        let mut s = fixture();
        s.vm[24 + 12 + 4..24 + 12 + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        reject(&s); // bad offset
        let mut s = fixture();
        s.vm[24 + 12 + 4..24 + 12 + 8].copy_from_slice(&1u32.to_le_bytes());
        reject(&s); // reading at this mid-block offset gives an invalid length
        let mut s = fixture();
        s.fd.truncate(249);
        reject(&s); // last FixedMeta offset beyond truncated FixedData
        let mut s = fixture();
        s.fm[8..12].copy_from_slice(&6u32.to_le_bytes());
        reject(&s); // count overrun
        let mut s = fixture();
        s.vm[24 + 12..24 + 12 + 4].copy_from_slice(&0u32.to_le_bytes());
        reject(&s); // duplicate key
        let s = fixture();
        assert!(decode(&file(&s, false)).is_err()); // partial stream set
        let mut s = fixture();
        s.cons = link(9, 1, 7);
        reject(&s); // unknown predecessor
        let mut s = fixture();
        s.cons = link(1, 1, 99);
        reject(&s); // unknown lag format
        let mut s = fixture();
        s.cons = link(0, 1, 7);
        reject(&s); // project summary cannot be a link endpoint
    }
    /// Make FixedMeta entry 4 (task B, UID 1) a manual task: 2026-03-02 08:00
    /// to 2026-03-03 17:00, 16 hours shown in days (DurationFormat 7).
    fn make_manual(s: &mut Streams) {
        s.f2m[16 + 4 * 96 + 8] |= 0x80;
        let r = 4 * 64;
        s.f2d[r + 50..r + 54].copy_from_slice(&[0xc0, 0x12, 0x2a, 0x3c]);
        s.f2d[r + 54..r + 58].copy_from_slice(&[0xd8, 0x27, 0x2b, 0x3c]);
        s.f2d[r + 58..r + 62].copy_from_slice(&9600u32.to_le_bytes());
        s.f2d[r + 62..r + 64].copy_from_slice(&7u16.to_le_bytes());
    }
    #[test]
    fn manual_mode_and_fields_come_from_the_second_fixed_block() {
        let mut s = fixture();
        // Manual bytes on an auto task are stale, not its manual fields.
        s.f2d[4 * 64 + 50..4 * 64 + 64].fill(0x11);
        let tasks = decode(&file(&s, true)).unwrap();
        assert!(!tasks[1].manual);
        assert_eq!(tasks[1].manual_start, None);
        assert_eq!(tasks[1].manual_duration_min, None);
        make_manual(&mut s);
        let tasks = decode(&file(&s, true)).unwrap();
        assert!(!tasks[0].manual);
        let b = &tasks[1];
        assert!(b.manual);
        assert_eq!(b.manual_start.as_deref(), Some("2026-03-02 08:00"));
        assert_eq!(b.manual_finish.as_deref(), Some("2026-03-03 17:00"));
        assert_eq!(b.manual_duration_min, Some(960));
        let r = 4 * 64;
        s.f2d[r + 62..r + 64].copy_from_slice(&39u16.to_le_bytes()); // estimated days
        assert_eq!(
            decode(&file(&s, true)).unwrap()[1].manual_duration_min,
            Some(960)
        );
        s.f2d[r + 62..r + 64].copy_from_slice(&8u16.to_le_bytes()); // elapsed days
        assert_eq!(
            decode(&file(&s, true)).unwrap()[1].manual_duration_min,
            None
        );
        s.f2d[r + 62..r + 64].copy_from_slice(&7u16.to_le_bytes());
        s.f2d[r + 58..r + 62].fill(0xff); // no stored duration
        assert_eq!(
            decode(&file(&s, true)).unwrap()[1].manual_duration_min,
            None
        );
    }
    #[test]
    fn refuses_a_second_fixed_block_that_does_not_match_the_tasks() {
        let r = 4 * 64;
        let manual = || {
            let mut s = fixture();
            make_manual(&mut s);
            s
        };
        assert!(decode(&file(&manual(), true)).is_ok());
        let mut s = manual();
        s.f2d[r + 62..r + 64].copy_from_slice(&2u16.to_le_bytes());
        reject(&s); // unknown DurationFormat
        let mut s = manual();
        s.f2d[r + 54..r + 58].copy_from_slice(&[0xc0, 0x12, 0x29, 0x3c]);
        reject(&s); // manual finish before manual start
        let mut s = fixture();
        s.f2d[r + 16..r + 24].copy_from_slice(&1f64.to_le_bytes());
        reject(&s); // sort keys out of task ID order: records misaligned
        let mut s = fixture();
        s.f2d[r..r + 16].fill(0);
        reject(&s); // a task record without its GUID
        let mut s = fixture();
        s.f2m[8..12].copy_from_slice(&4u32.to_le_bytes());
        reject(&s); // Fixed2Meta counts a different number of entries
        let mut s = fixture();
        s.f2d.truncate(4 * 64);
        s.f2m[12..16].copy_from_slice(&(4 * 64u32).to_le_bytes());
        reject(&s); // Fixed2Data shorter than its entries
        let mut s = fixture();
        s.f2m[16 + 4 * 96 + 4..16 + 4 * 96 + 8].copy_from_slice(&(3 * 64u32).to_le_bytes());
        reject(&s); // entry offsets must step by one record
        let mut s = fixture();
        s.f2m.clear();
        reject(&s); // a current layout without Fixed2Meta is partial
    }
    #[test]
    fn reads_the_new_task_default_from_project_props() {
        let mut s = fixture();
        assert_eq!(new_tasks_are_manual(&file(&s, true)), Ok(false));
        s.props = props(&[0xff, 0]);
        assert_eq!(new_tasks_are_manual(&file(&s, true)), Ok(true));
        let project = crate::project::project_from_mpp(&file(&s, true)).unwrap();
        assert!(project.new_tasks_are_manual);
        s.props = props(&[1, 1]);
        assert!(new_tasks_are_manual(&file(&s, true)).is_err());
        assert!(crate::project::project_from_mpp(&file(&s, true)).is_err());
        s.props = crate::props::stream(&[]);
        assert!(new_tasks_are_manual(&file(&s, true)).is_err());
        // A file without a task table has no default to read.
        let no_tasks = write_cfb_tree(&[Node::Stream("Props", vec![0u8; 4])]);
        assert_eq!(new_tasks_are_manual(&no_tasks), Ok(false));
    }
    #[test]
    fn the_default_is_refused_with_a_task_table_that_is_refused() {
        let s = fixture();
        // Task streams without FixedMeta are a partial set, not "no table".
        let partial = write_cfb_tree(&[Node::Storage(
            "   114",
            vec![
                Node::Stream("Props", s.props.clone()),
                Node::Storage("TBkndTask", vec![Node::Stream("VarMeta", s.vm.clone())]),
            ],
        )]);
        assert!(decode(&partial).is_err());
        assert!(new_tasks_are_manual(&partial).is_err());
        // A readable default beside a refused table is not answered either.
        let mut s = fixture();
        s.v2[4] = 1; // control character in a task name
        assert!(decode(&file(&s, true)).is_err());
        assert!(new_tasks_are_manual(&file(&s, true)).is_err());
        let mut s = fixture();
        s.f2m.clear();
        assert!(new_tasks_are_manual(&file(&s, true)).is_err());
    }
    #[test]
    fn manual_leaf_imports_pinned_by_its_dates_and_auto_leaf_by_a_constraint() {
        use projcore::ConstraintType;
        let mut s = fixture();
        let auto = crate::project::project_from_mpp(&file(&s, true)).unwrap();
        let task = &auto.tasks[0];
        assert!(!task.manual);
        assert_eq!(task.constraint, ConstraintType::MustStartOn);
        assert_eq!(task.pinned_dates(), None);
        make_manual(&mut s);
        let manual = crate::project::project_from_mpp(&file(&s, true)).unwrap();
        let task = &manual.tasks[0];
        assert!(task.manual);
        assert_eq!(task.constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(task.constraint_date, None);
        assert_eq!(task.duration_min, 960);
        assert_eq!(task.manual_duration_min, Some(960));
        let (start, finish) = task.pinned_dates().unwrap();
        assert_eq!(start.to_mspdi(), "2026-03-02T08:00:00");
        assert_eq!(finish.unwrap().to_mspdi(), "2026-03-03T17:00:00");
    }
    fn link(pred: u32, succ: u32, format: u16) -> Vec<u8> {
        let mut r = vec![0u8; 20];
        r[4..8].copy_from_slice(&pred.to_le_bytes());
        r[8..12].copy_from_slice(&succ.to_le_bytes());
        r[12..14].copy_from_slice(&1u16.to_le_bytes());
        r[18..20].copy_from_slice(&format.to_le_bytes());
        r
    }
}
