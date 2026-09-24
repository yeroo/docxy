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
    for rec in cons.chunks_exact(20) {
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
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
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
    for e in vm[24..end].chunks_exact(8) {
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

pub(crate) fn decode(bytes: &[u8]) -> Result<Vec<MppTask>, String> {
    let cfb = Cfb::open(bytes)?;
    let paths = cfb.paths();
    let task_paths: Vec<_> = paths.iter().filter(|p| p.contains("TBkndTask/")).collect();
    if task_paths.is_empty() {
        return Ok(Vec::new());
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
            decode_current(&cfb, prefix, &fd, &vm, &v2, indexed)
        }
        fixedmeta::TaskIndex::Legacy(indexed) => {
            let uids: HashSet<_> = indexed.iter().map(|r| r.uid).collect();
            decode_legacy(&cfb, prefix, &fd, &vm, &v2, indexed, &uids)
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
) -> Result<Vec<MppTask>, String> {
    let uids: HashSet<_> = indexed
        .iter()
        .filter(|r| !r.is_null)
        .map(|r| r.uid)
        .collect();
    let named = names(vm, v2, &uids)?;
    let mut out = Vec::new();
    for row in indexed {
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
        out.push(MppTask {
            id: row.id,
            uid: row.uid,
            name: named[&row.uid].clone(),
            start,
            finish,
            outline_level: Some(level),
            predecessors: Vec::new(),
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
        Streams {
            fm,
            fd,
            vm,
            v2,
            cons: Vec::new(),
        }
    }
    fn file(s: &Streams, include_fd: bool) -> Vec<u8> {
        let mut task = vec![
            Node::Stream("FixedMeta", s.fm.clone()),
            Node::Stream("VarMeta", s.vm.clone()),
            Node::Stream("Var2Data", s.v2.clone()),
        ];
        if include_fd {
            task.push(Node::Stream("FixedData", s.fd.clone()));
        }
        write_cfb_tree(&[Node::Storage(
            "   114",
            vec![
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
    fn link(pred: u32, succ: u32, format: u16) -> Vec<u8> {
        let mut r = vec![0u8; 20];
        r[4..8].copy_from_slice(&pred.to_le_bytes());
        r[8..12].copy_from_slice(&succ.to_le_bytes());
        r[12..14].copy_from_slice(&1u16.to_le_bytes());
        r[18..20].copy_from_slice(&format.to_le_bytes());
        r
    }
}
