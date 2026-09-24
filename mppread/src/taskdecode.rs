//! Validated task-table decoding for Project's newest MPP storage.
use crate::{
    cfb::Cfb,
    fixedmeta,
    mpp::{MppPred, MppTask, decode_timestamp, detect_date_layout, detect_outline_column},
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
fn timestamp(b: &[u8], o: usize) -> Option<String> {
    let days = u16_at(b, o + 2);
    if days == 0xffff {
        return None;
    }
    // Project's current task table stores a one-based day ordinal. For example,
    // 03-first-task.mpp stores 0x3a86 while its Project XML export says
    // 2025-01-06; interpreting that as days since 1984 yields 2025-01-07.
    let mut value = [b[o], b[o + 1], b[o + 2], b[o + 3]];
    value[2..4].copy_from_slice(&days.checked_sub(1)?.to_le_bytes());
    decode_timestamp(&value, 0)
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
        let Some(end) = header_end.checked_add(len).filter(|&n| n <= v2.len()) else {
            return Err(format!("Var2Data block out of range at entry {i}"));
        };
        if key == 0x000e {
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
            names.insert(uid, name);
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
            let name = crate::vardata::string_at(v2, off)
                .ok_or_else(|| format!("invalid legacy task name for UID {uid}"))?;
            out.insert(uid, name);
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
    let indexed = fixedmeta::index(&fm, &fd)?;
    let uids: HashSet<_> = indexed.iter().map(|(uid, _, _)| *uid).collect();
    let record_len = indexed.first().map(|x| x.2).unwrap_or(202);
    if record_len == 264 {
        return decode_legacy(&cfb, prefix, &fd, &vm, &v2, indexed, &uids);
    }
    let named = names(&vm, &v2, &uids)?;
    let mut out = Vec::new();
    for (uid, off, len) in indexed {
        if len != NEWEST.length {
            return Err(format!("unrecognized task record length {len}"));
        }
        let rec = &fd[off..off + len];
        let start = timestamp(rec, NEWEST.start);
        let finish = timestamp(rec, NEWEST.finish);
        let level = rec[NEWEST.level] as u32;
        if level > 20 {
            return Err(format!("invalid outline level {level} for UID {uid}"));
        }
        if start
            .as_ref()
            .zip(finish.as_ref())
            .is_some_and(|(s, f)| s > f)
        {
            return Err(format!("inverted task dates for UID {uid}"));
        }
        out.push(MppTask {
            uid,
            name: named[&uid].clone(),
            start,
            finish,
            outline_level: Some(level),
            predecessors: Vec::new(),
        });
    }
    let positions: HashMap<_, _> = out.iter().enumerate().map(|(i, t)| (t.uid, i)).collect();
    let cons_path = prefix.replace("TBkndTask/", "TBkndCons/FixedData");
    if let Some(cons) = cfb.read_path(&cons_path) {
        if !cons.len().is_multiple_of(20) {
            return Err("link record length mismatch".into());
        }
        for rec in cons.chunks_exact(20) {
            let pred_uid = u32_at(rec, 4);
            let succ_uid = u32_at(rec, 8);
            let kind = u16_at(rec, 12);
            let lag = i32::from_le_bytes(rec[14..18].try_into().unwrap());
            let format = u16_at(rec, 18);
            let Some(&pred) = positions.get(&pred_uid) else {
                return Err(format!("link to unknown predecessor UID {pred_uid}"));
            };
            let Some(&succ) = positions.get(&succ_uid) else {
                return Err(format!("link to unknown successor UID {succ_uid}"));
            };
            if kind > 3 || !matches!(format, 3 | 7) {
                return Err(format!(
                    "unsupported link type {kind} or LagFormat {format}"
                ));
            }
            let lag_min = (lag as f64 / 10.0).round() as i64;
            out[succ].predecessors.push(MppPred {
                pred,
                pred_uid,
                kind: kind as u8,
                lag_min,
            });
        }
    }
    Ok(out)
}

fn decode_legacy(
    cfb: &Cfb,
    prefix: &str,
    fd: &[u8],
    vm: &[u8],
    v2: &[u8],
    indexed: Vec<(u32, usize, usize)>,
    uids: &HashSet<u32>,
) -> Result<Vec<MppTask>, String> {
    let named = legacy_names(vm, v2, uids)?;
    let mut packed = Vec::with_capacity(indexed.len() * 264);
    for (_, off, len) in &indexed {
        if *len != 264 {
            return Err("legacy task record length mismatch".into());
        }
        packed.extend_from_slice(&fd[*off..*off + *len]);
    }
    let pos: HashMap<_, _> = indexed
        .iter()
        .enumerate()
        .map(|(i, (uid, _, _))| (*uid, i))
        .collect();
    let cons_path = prefix.replace("TBkndTask/", "TBkndCons/FixedData");
    let cons = cfb.read_path(&cons_path).unwrap_or_default();
    if !cons.len().is_multiple_of(20) {
        return Err("legacy link record length mismatch".into());
    }
    let links: Vec<_> = cons
        .chunks_exact(20)
        .filter(|r| u16_at(r, 12) == 1)
        .filter_map(|r| Some((*pos.get(&u32_at(r, 4))?, *pos.get(&u32_at(r, 8))?)))
        .collect();
    let Some((stride, date_off)) = detect_date_layout(&packed, indexed.len(), &links) else {
        return Err("legacy task dates do not fit an indexed layout".into());
    };
    if stride != 264 {
        return Err(format!(
            "legacy date layout stride {stride} does not match FixedMeta"
        ));
    }
    let outline = detect_outline_column(&packed, 264, indexed.len());
    let mut out = Vec::new();
    for (i, (uid, _, _)) in indexed.iter().enumerate() {
        out.push(MppTask {
            uid: *uid,
            name: named[uid].clone(),
            start: decode_timestamp(&packed, i * 264 + date_off),
            finish: decode_timestamp(&packed, i * 264 + date_off + 4),
            outline_level: outline.map(|off| packed[i * 264 + off] as u32 + 1),
            predecessors: Vec::new(),
        });
    }
    for rec in cons.chunks_exact(20) {
        let pred_uid = u32_at(rec, 4);
        let succ_uid = u32_at(rec, 8);
        let kind = u16_at(rec, 12);
        let format = u16_at(rec, 14);
        let lag = i32::from_le_bytes(rec[16..20].try_into().unwrap());
        let Some(&pred) = pos.get(&pred_uid) else {
            return Err(format!("legacy link to unknown UID {pred_uid}"));
        };
        let Some(&succ) = pos.get(&succ_uid) else {
            return Err(format!("legacy link to unknown UID {succ_uid}"));
        };
        if kind > 3 || !matches!(format, 3 | 7) {
            return Err(format!(
                "unsupported legacy link type {kind} or LagFormat {format}"
            ));
        }
        out[succ].predecessors.push(MppPred {
            pred,
            pred_uid,
            kind: kind as u8,
            lag_min: (lag as f64 / 10.0).round() as i64,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfb::{Node, write_cfb_tree};

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
    fn conversion_preserves_uid_and_uses_summary_name() {
        let mut s = fixture();
        s.fd[250..254].copy_from_slice(&7u32.to_le_bytes());
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
        s.fd[250..254].copy_from_slice(&0u32.to_le_bytes());
        reject(&s); // duplicate UID
        let mut s = fixture();
        s.fd[250..254].copy_from_slice(&u32::MAX.to_le_bytes());
        reject(&s); // UID cannot fit projcore
        let mut s = fixture();
        s.vm[24 + 12 + 4..24 + 12 + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        reject(&s); // bad offset
        let mut s = fixture();
        s.vm[24 + 12 + 4..24 + 12 + 8].copy_from_slice(&1u32.to_le_bytes());
        reject(&s); // not a block boundary
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
        s.cons = link(0, 1, 99);
        reject(&s); // unknown lag format
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
