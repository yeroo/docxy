//! Current Project assignment/resource records used to recover task OverAllocated.
//! Project exports the task flag as the OR of its direct assignments' flags.
//! The assignment flag is based on its own units against its resource's
//! availability during the assignment, not on other assignments' load.
//! Current FixedData offsets, checked against Project's XML corpus:
//! assignment records (110 bytes) have UID +0, TaskUID +4, ResourceUID +8,
//! Units × 10000 at +12, Start +52, Finish +56. Resource records (172 bytes)
//! have row ID +0, UID +4, MaxUnits × 10000 at +8, and work/nonwork type at +170; a
//! Fixed2Meta +8 bit 0x10 marks a cost resource among the nonwork ones.
//! Resource VarMeta key 0x0114 holds availability boundaries and capacities.
use crate::{cfb::Cfb, mpp::MppTask};
use std::collections::{HashMap, HashSet};

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn f64_at(b: &[u8], at: usize) -> f64 {
    f64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
pub(crate) fn stream(cfb: &Cfb, table: &str, name: &str) -> Result<Vec<u8>, String> {
    let suffix = format!("{table}/{name}");
    let path = cfb
        .paths()
        .into_iter()
        .find(|p| p.ends_with(&suffix))
        .ok_or_else(|| format!("missing {suffix}"))?;
    cfb.read_path(&path)
        .ok_or_else(|| format!("missing {suffix}"))
}
pub(crate) fn count(
    meta: &[u8],
    stride: usize,
    data_len: usize,
    minimum: usize,
) -> Result<usize, String> {
    if meta.len() < 16 || meta[..4] != [0xba, 0xad, 0xdf, 0xfa] {
        return Err("invalid fixed table header".into());
    }
    let n = u32_at(meta, 8) as usize;
    if n < minimum
        || 16usize.checked_add(n.checked_mul(stride).ok_or("table too large")?) != Some(meta.len())
        || u32_at(meta, 12) as usize != data_len
    {
        return Err("fixed table count mismatch".into());
    }
    Ok(n)
}
fn row_time(b: &[u8], at: usize) -> Option<u32> {
    let time = u16_at(b, at);
    let day = u16_at(b, at + 2);
    (time < 14_400 && day != u16::MAX).then_some(day as u32 * 14_400 + time as u32)
}

#[derive(Clone, Debug)]
pub(crate) struct Resource {
    pub uid: u32,
    pub max_units: f64,
    /// Project's binary type: 0=work, 1=material, 2=cost.
    pub kind: u16,
    /// Exclusive end, capacity, and whether Project exports this as an
    /// availability period. A zero-capacity gap has `exported = false`.
    pub periods: Vec<(u32, f64, bool)>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Assignment {
    pub uid: u32,
    pub task_uid: u32,
    pub resource_uid: i32,
    pub units: f64,
    pub start: u32,
    pub finish: u32,
}

fn availability(block: &[u8]) -> Result<Vec<(u32, f64, bool)>, String> {
    if block.len() < 36 || u16_at(block, 2) != 4 || u32_at(block, 4) != 16 {
        return Err("unrecognized availability block".into());
    }
    let count = u16_at(block, 0) as usize;
    if 16usize.checked_add(count.checked_mul(20).ok_or("availability too large")?)
        != Some(block.len())
    {
        return Err("availability block length mismatch".into());
    }
    let mut periods = Vec::new();
    for row in block[16..].as_chunks::<20>().0 {
        let capacity = f64_at(row, 0) / 10_000.0;
        let marker = u32_at(row, 8);
        let end = u32_at(row, 16);
        if !matches!(marker, 0xfe | 0xff)
            || row[12..16] != [0, 0, 0, 0]
            || (marker == 0xfe && capacity != 0.0)
        {
            return Err("unrecognized availability interval".into());
        }
        if !capacity.is_finite()
            || capacity < 0.0
            || periods.last().is_some_and(|(last, _, _)| *last >= end)
        {
            return Err("invalid availability period".into());
        }
        periods.push((end, capacity, marker == 0xff));
    }
    // Project writes a zero-capacity terminal sentinel after the exported
    // availability periods. Its boundary is still useful for validation.
    if periods
        .last()
        .is_none_or(|(_, cap, exported)| *cap != 0.0 || *exported)
    {
        return Err("missing availability sentinel".into());
    }
    periods.pop();
    Ok(periods)
}

pub(crate) fn resources(cfb: &Cfb) -> Result<HashMap<u32, Resource>, String> {
    let fm = stream(cfb, "TBkndRsc", "FixedMeta")?;
    let fd = stream(cfb, "TBkndRsc", "FixedData")?;
    let f2m = stream(cfb, "TBkndRsc", "Fixed2Meta")?;
    let vm = stream(cfb, "TBkndRsc", "VarMeta")?;
    let v2 = stream(cfb, "TBkndRsc", "Var2Data")?;
    let n = count(&fm, 37, fd.len(), 3)?;
    if f2m.len() != 16 + n * 51
        || f2m[..4] != [0xba, 0xad, 0xdf, 0xfa]
        || u32_at(&f2m, 8) as usize != n
    {
        return Err("resource second fixed table mismatch".into());
    }
    if vm.len() < 24
        || vm[..4] != [0xba, 0xad, 0xdf, 0xfa]
        || 24usize.checked_add(
            (u32_at(&vm, 8) as usize)
                .checked_mul(12)
                .ok_or("resource variable table too large")?,
        ) != Some(vm.len())
        || u32_at(&vm, 20) as usize != v2.len()
    {
        return Err("resource variable table mismatch".into());
    }
    for i in 0..3 {
        if u16_at(&fm, 16 + i * 37) != 4 || u32_at(&fm, 16 + i * 37 + 4) as usize != i * 16 {
            return Err("unrecognized resource schema row".into());
        }
    }
    let schema_end = if n > 3 {
        u32_at(&fm, 16 + 3 * 37 + 4) as usize
    } else {
        fd.len()
    };
    if schema_end != 48 {
        return Err("resource schema length mismatch".into());
    }
    let mut out = HashMap::new();
    for i in 3..n {
        let meta_kind = u16_at(&fm, 16 + i * 37);
        let at = u32_at(&fm, 16 + i * 37 + 4) as usize;
        let end = if i + 1 < n {
            u32_at(&fm, 16 + (i + 1) * 37 + 4) as usize
        } else {
            fd.len()
        };
        if end < at || end > fd.len() {
            return Err("resource offset out of range".into());
        }
        if meta_kind == 4 && end - at == 16 {
            continue; // Blank resource row.
        }
        if !matches!(meta_kind, 0 | 2) || end - at != 172 {
            return Err("unrecognized resource record length".into());
        }
        if meta_kind == 2 {
            continue; // Deleted row may reuse a live UID or ID.
        }
        let row = &fd[at..end];
        let uid = u32_at(row, 4);
        if uid == 0 {
            continue; // Implicit unassigned resource.
        }
        let max_units = f64_at(row, 8) / 10_000.0;
        let raw_kind = u16_at(row, 170);
        let f2 = &f2m[16 + i * 51..16 + (i + 1) * 51];
        if u32_at(f2, 4) as usize != i * 40 {
            return Err("resource second fixed offset mismatch".into());
        }
        let kind = if raw_kind == 1 && f2[8] & 0x10 != 0 {
            2
        } else {
            raw_kind
        };
        if !max_units.is_finite()
            || max_units < 0.0
            || kind > 2
            || out
                .insert(
                    uid,
                    Resource {
                        uid,
                        max_units,
                        kind,
                        periods: Vec::new(),
                    },
                )
                .is_some()
        {
            return Err("invalid resource record".into());
        }
    }
    for e in vm[24..].as_chunks::<12>().0 {
        if u16_at(e, 8) != 0x0114 {
            continue;
        }
        let uid = u32_at(e, 0);
        let offset = u32_at(e, 4) as usize;
        if offset.checked_add(4).is_none_or(|end| end > v2.len()) {
            return Err("availability offset out of range".into());
        }
        let len = u32_at(&v2, offset) as usize;
        let end = offset
            .checked_add(4)
            .and_then(|n| n.checked_add(len))
            .filter(|&n| n <= v2.len())
            .ok_or("availability data out of range")?;
        let Some(r) = out.get_mut(&uid) else {
            continue; // A deleted or implicit resource can retain variable fields.
        };
        if !r.periods.is_empty() {
            return Err("duplicate availability".into());
        }
        r.periods = availability(&v2[offset + 4..end])?;
    }
    Ok(out)
}

/// Live 110-byte assignment records with their FixedMeta entry index, which
/// also indexes the assignment Fixed2Meta. Blank (kind 4, 16 bytes) and
/// deleted (kind 2) rows are skipped; any other kind or length is an error.
pub(crate) fn live_assignment_rows<'a>(
    fm: &[u8],
    fd: &'a [u8],
) -> Result<Vec<(usize, &'a [u8])>, String> {
    let n = count(fm, 34, fd.len(), 0)?;
    let mut out = Vec::new();
    let mut previous_end = 0;
    for i in 0..n {
        let meta = &fm[16 + i * 34..16 + (i + 1) * 34];
        let at = u32_at(meta, 4) as usize;
        let end = if i + 1 < n {
            u32_at(fm, 16 + (i + 1) * 34 + 4) as usize
        } else {
            fd.len()
        };
        if at != previous_end || end < at || end > fd.len() {
            return Err("assignment offset mismatch".into());
        }
        previous_end = end;
        let kind = u16_at(meta, 0);
        if kind == 4 && end - at == 16 {
            continue; // Blank assignment row.
        }
        if !matches!(kind, 0 | 2) || end - at != 110 {
            return Err("assignment record length mismatch".into());
        }
        if kind == 2 {
            continue; // Deleted assignment row.
        }
        out.push((i, &fd[at..end]));
    }
    Ok(out)
}

pub(crate) fn assignments(cfb: &Cfb) -> Result<Vec<Assignment>, String> {
    let fm = stream(cfb, "TBkndAssn", "FixedMeta")?;
    let fd = stream(cfb, "TBkndAssn", "FixedData")?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (_, row) in live_assignment_rows(&fm, &fd)? {
        let uid = u32_at(row, 0);
        let task_uid = u32_at(row, 4);
        let resource_uid = u32_at(row, 8) as i32;
        if task_uid == 0 && resource_uid == 0 {
            continue; // Internal placeholder omitted by the assignment importer.
        }
        let units = f64_at(row, 12) / 10_000.0;
        let start = row_time(row, 52).ok_or("invalid assignment start")?;
        let finish = row_time(row, 56).ok_or("invalid assignment finish")?;
        if uid == 0 || !seen.insert(uid) || !units.is_finite() || units < 0.0 || start > finish {
            return Err("invalid assignment record".into());
        }
        out.push(Assignment {
            uid,
            task_uid,
            resource_uid,
            units,
            start,
            finish,
        });
    }
    Ok(out)
}

fn normalize_cost_units(assignments: &mut [Assignment], resources: &HashMap<u32, Resource>) {
    // Cost assignments have no editable units in Project; its XML writes the
    // default 1 while this binary slot carries a cost/work value.
    for a in assignments {
        if a.resource_uid >= 0
            && resources
                .get(&(a.resource_uid as u32))
                .is_some_and(|r| r.kind == 2)
        {
            a.units = 1.0;
        }
    }
}

/// Compare an assignment's own units with available capacity over its span.
/// Any overlap with an availability interval counts. The binary records do
/// not tell us which instants are working; calendar exceptions/contours are
/// not decoded here and remain undecided by the corpus.
pub(crate) fn assignment_overallocated(a: &Assignment, r: &Resource) -> bool {
    if r.kind != 0 || a.start >= a.finish {
        return false;
    }
    if r.periods.is_empty() {
        return a.units > r.max_units + 1e-9;
    }
    let mut begin = 0;
    for &(end, capacity, _) in &r.periods {
        if a.start < end && a.finish > begin && a.units > capacity + 1e-9 {
            return true;
        }
        begin = end;
    }
    // Project's f10 end case marks an assignment crossing the final boundary
    // overallocated: capacity after that boundary is zero.
    a.finish > begin && a.units > 1e-9
}

pub(crate) fn decode(cfb: &Cfb, tasks: &[MppTask]) -> Result<HashMap<u32, bool>, String> {
    let mut assignments = assignments(cfb)?;
    let known: HashSet<_> = tasks.iter().filter(|t| !t.is_null).map(|t| t.uid).collect();
    let mut out: HashMap<_, _> = known.iter().map(|&uid| (uid, false)).collect();
    if assignments.is_empty() {
        return Ok(out);
    }
    let resources = resources(cfb)?;
    normalize_cost_units(&mut assignments, &resources);
    for a in &assignments {
        if !known.contains(&a.task_uid) {
            return Err("assignment for unknown task".into());
        }
        if a.resource_uid == -65_535 {
            continue;
        } // Project's unassigned sentinel (0xffff0001)
        if a.resource_uid < 0 {
            return Err(format!(
                "assignment {} has unknown resource sentinel",
                a.uid
            ));
        }
        let r = resources
            .get(&(a.resource_uid as u32))
            .ok_or_else(|| format!("assignment {} for unknown resource", a.uid))?;
        debug_assert_eq!(r.uid, a.resource_uid as u32);
        if assignment_overallocated(a, r) {
            out.insert(a.task_uid, true);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfb::{Node, write_cfb_tree};
    use std::path::Path;

    fn date(raw: u32) -> String {
        let mut b = Vec::new();
        b.extend_from_slice(&((raw % 14_400) as u16).to_le_bytes());
        b.extend_from_slice(&((raw / 14_400) as u16).to_le_bytes());
        crate::mpp::decode_timestamp(&b, 0).unwrap()
    }
    fn xml_date(d: Option<projcore::DateTime>) -> Option<String> {
        d.map(|d| d.to_mspdi().replace('T', " ")[..16].to_string())
    }
    #[test]
    fn project_assignment_and_resource_offsets() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mpp");
        if !base.join("snapshots/01-empty.mpp").exists() {
            return; // The private Project corpus is not shipped with the source.
        }
        let mut checked = 0;
        let mut snapshot_pairs = 0;
        for folder in [
            "task-fields",
            "snapshots",
            "order",
            "manual",
            "lag",
            "progress",
        ] {
            let Ok(entries) = std::fs::read_dir(base.join(folder)) else {
                continue;
            };
            // Sorted, so the first failing file is the same on every machine.
            // `read_dir` order is whatever the filesystem keeps: macOS reached
            // 31-calendar-hours first and Windows another file, and a failure
            // common to every snapshot looked like one peculiar to 31.
            let mut paths: Vec<_> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "mpp"))
                .collect();
            paths.sort();
            for path in paths {
                let stem = path.file_stem().unwrap().to_string_lossy();
                // x-recurring's XML was saved after a legacy conversion and
                // does not describe its current MPP's assignment records.
                if stem.ends_with("-mpp12") || stem == "x-recurring" {
                    continue;
                }
                let xml = path.with_extension("xml");
                assert!(
                    xml.exists(),
                    "missing Project oracle for {}",
                    path.display()
                );
                let oracle =
                    projcore::mspdi::read_mspdi(&std::fs::read_to_string(&xml).unwrap()).unwrap();
                let bytes = std::fs::read(&path).unwrap();
                let cfb = Cfb::open(&bytes).unwrap();
                let rr =
                    resources(&cfb).unwrap_or_else(|e| panic!("{} resources: {e}", path.display()));
                let mut aa = assignments(&cfb)
                    .unwrap_or_else(|e| panic!("{} assignments: {e}", path.display()));
                normalize_cost_units(&mut aa, &rr);
                // ⚠️ Project's XML always lists an implicit unassigned resource
                // with UID 0. The decoder deliberately leaves it out: nothing
                // looks it up, because unassigned work carries the binary
                // sentinel -65535 instead. So compare only the real resources,
                // and pin both halves of that so neither side can drift
                // unnoticed — a count that included UID 0 on one side only is
                // exactly what broke this test on every snapshot.
                assert!(
                    oracle.resources.iter().any(|r| r.uid == 0),
                    "{} XML no longer lists Project's implicit UID-0 resource",
                    path.display()
                );
                assert!(
                    !rr.contains_key(&0),
                    "{} decoder returned Project's implicit UID-0 resource",
                    path.display()
                );
                let real: Vec<_> = oracle.resources.iter().filter(|r| r.uid != 0).collect();
                assert_eq!(rr.len(), real.len(), "{} resource count", path.display());
                for expected in real {
                    let actual = &rr[&(expected.uid as u32)];
                    assert!(
                        (actual.max_units - expected.max_units).abs() < 1e-9,
                        "{} resource {} max units",
                        path.display(),
                        expected.uid
                    );
                    let kind = match expected.kind {
                        projcore::model::ResourceType::Work => 0,
                        projcore::model::ResourceType::Material => 1,
                        projcore::model::ResourceType::Cost => 2,
                    };
                    assert_eq!(
                        actual.kind,
                        kind,
                        "{} resource {} type",
                        path.display(),
                        expected.uid
                    );
                    assert_eq!(
                        actual
                            .periods
                            .iter()
                            .filter(|(_, _, exported)| *exported)
                            .count(),
                        expected.availability_periods.len(),
                        "{} resource {} period count",
                        path.display(),
                        expected.uid
                    );
                    let mut from = 0;
                    let mut exported_index = 0;
                    for &(end, cap, exported) in &actual.periods {
                        if !exported {
                            from = end;
                            continue;
                        }
                        let i = exported_index;
                        exported_index += 1;
                        let p = &expected.availability_periods[i];
                        let from_date = if i == 0 {
                            "1984-01-01 00:00".to_string()
                        } else {
                            date(from)
                        };
                        assert_eq!(
                            Some(from_date),
                            xml_date(p.available_from),
                            "{} resource {} period {i} from",
                            path.display(),
                            expected.uid
                        );
                        assert_eq!(
                            Some(date(end - 10)),
                            xml_date(p.available_to),
                            "{} resource {} period {i} to",
                            path.display(),
                            expected.uid
                        );
                        let want = p
                            .available_units
                            .as_ref()
                            .unwrap()
                            .as_str()
                            .parse::<f64>()
                            .unwrap();
                        assert!(
                            (cap - want).abs() < 1e-9,
                            "{} resource {} period {i} capacity",
                            path.display(),
                            expected.uid
                        );
                        from = end;
                    }
                }
                let by_uid: HashMap<_, _> = aa.iter().map(|a| (a.uid as i32, a)).collect();
                assert_eq!(
                    by_uid.len(),
                    oracle.assignments.len(),
                    "{} assignment count",
                    path.display()
                );
                for expected in &oracle.assignments {
                    let actual = by_uid[&expected.uid];
                    assert_eq!(
                        actual.task_uid as i32,
                        expected.task_uid,
                        "{} assignment {} task",
                        path.display(),
                        expected.uid
                    );
                    assert_eq!(
                        actual.resource_uid,
                        expected.resource_uid,
                        "{} assignment {} resource",
                        path.display(),
                        expected.uid
                    );
                    assert!(
                        (actual.units - expected.units).abs() < 1e-9,
                        "{} assignment {} units {} vs {}",
                        path.display(),
                        expected.uid,
                        actual.units,
                        expected.units
                    );
                    assert_eq!(
                        Some(date(actual.start)),
                        xml_date(expected.start),
                        "{} assignment {} start",
                        path.display(),
                        expected.uid
                    );
                    assert_eq!(
                        Some(date(actual.finish)),
                        xml_date(expected.finish),
                        "{} assignment {} finish",
                        path.display(),
                        expected.uid
                    );
                    if expected.resource_uid >= 0 {
                        assert_eq!(
                            Some(assignment_overallocated(
                                actual,
                                &rr[&(expected.resource_uid as u32)]
                            )),
                            expected.overallocated,
                            "{} assignment {} overallocated",
                            path.display(),
                            expected.uid
                        );
                    }
                }
                checked += 1;
                if folder == "snapshots" {
                    snapshot_pairs += 1;
                }
            }
        }
        // The fetched corpus (yeroo/mpp-corpus) provides only `snapshots/`: 47
        // eligible pairs today. The other folders are generated locally with
        // Project (corpus/tools/gen_mpp_task_field_cases.py) and are checked on
        // top when present, so they cannot be what this guard counts on. Fewer
        // snapshot pairs than a complete fetch means a partial fetch.
        assert!(
            snapshot_pairs >= 47,
            "only {snapshot_pairs} snapshot pairs checked ({checked} in all); \
             a complete corpus/tools/fetch-mpp-corpus.* fetch provides 47"
        );
    }

    fn fixed_header(count: u32, data_len: u32, stride: usize) -> Vec<u8> {
        let mut b = vec![0; 16 + count as usize * stride];
        b[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        b[8..12].copy_from_slice(&count.to_le_bytes());
        b[12..16].copy_from_slice(&data_len.to_le_bytes());
        b
    }
    fn synthetic(
        assignment: bool,
        corrupt: bool,
        include_assignment_table: bool,
        resource_uid: i32,
    ) -> Cfb {
        let mut resource_meta = fixed_header(4, 220, 37);
        for i in 0..4 {
            resource_meta[16 + i * 37 + 4..16 + i * 37 + 8]
                .copy_from_slice(&((i * 16) as u32).to_le_bytes());
            if i < 3 {
                resource_meta[16 + i * 37..16 + i * 37 + 2].copy_from_slice(&4u16.to_le_bytes());
            }
        }
        let mut resource_data = vec![0; 220];
        resource_data[48..52].copy_from_slice(&10u32.to_le_bytes());
        resource_data[52..56].copy_from_slice(&2u32.to_le_bytes());
        resource_data[56..64].copy_from_slice(&5000f64.to_le_bytes());
        let mut resource_fixed2 = fixed_header(4, 160, 51);
        for i in 0..4 {
            resource_fixed2[16 + i * 51 + 4..16 + i * 51 + 8]
                .copy_from_slice(&((i * 40) as u32).to_le_bytes());
        }
        let mut resource_var = fixed_header(0, 0, 12);
        resource_var.resize(24, 0);
        let mut nodes = vec![Node::Storage(
            "TBkndRsc",
            vec![
                Node::Stream("FixedMeta", resource_meta),
                Node::Stream("FixedData", resource_data),
                Node::Stream("Fixed2Meta", resource_fixed2),
                Node::Stream("VarMeta", resource_var),
                Node::Stream("Var2Data", vec![]),
            ],
        )];
        if include_assignment_table {
            let n = u32::from(assignment);
            let mut meta = fixed_header(n, n * 110, 34);
            let mut data = vec![0; n as usize * 110];
            if assignment {
                meta[20..24].copy_from_slice(&0u32.to_le_bytes());
                data[..4].copy_from_slice(&7u32.to_le_bytes());
                data[4..8].copy_from_slice(&1u32.to_le_bytes());
                data[8..12].copy_from_slice(&resource_uid.to_le_bytes());
                data[12..20].copy_from_slice(&6000f64.to_le_bytes());
                data[52..56].copy_from_slice(&[0xc0, 0x12, 0x2a, 0x3c]);
                data[56..60].copy_from_slice(&[0xd8, 0x27, 0x2a, 0x3c]);
            }
            if corrupt {
                meta[8..12].copy_from_slice(&9u32.to_le_bytes());
            }
            nodes.push(Node::Storage(
                "TBkndAssn",
                vec![
                    Node::Stream("FixedMeta", meta),
                    Node::Stream("FixedData", data),
                ],
            ));
        }
        Cfb::open(&write_cfb_tree(&nodes)).unwrap()
    }
    #[test]
    fn assignment_units_capacity_and_optional_table() {
        let tasks = [MppTask {
            uid: 1,
            ..MppTask::default()
        }];
        assert!(decode(&synthetic(true, false, true, 2), &tasks).unwrap()[&1]);
        assert!(!decode(&synthetic(false, false, true, 2), &tasks).unwrap()[&1]);
        assert!(decode(&synthetic(true, true, true, 2), &tasks).is_err());
        assert!(decode(&synthetic(false, false, false, 2), &tasks).is_err());
        assert!(decode(&synthetic(true, false, true, -1), &tasks).is_err());
        assert!(!decode(&synthetic(true, false, true, -65_535), &tasks).unwrap()[&1]);
        let empty_only = Cfb::open(&write_cfb_tree(&[Node::Storage(
            "TBkndAssn",
            vec![
                Node::Stream("FixedMeta", fixed_header(0, 0, 34)),
                Node::Stream("FixedData", vec![]),
            ],
        )]))
        .unwrap();
        assert!(!decode(&empty_only, &tasks).unwrap()[&1]);
    }
    #[test]
    fn availability_boundaries_and_nonwork_resource() {
        let a = Assignment {
            uid: 7,
            task_uid: 1,
            resource_uid: 2,
            units: 0.6,
            start: 100,
            finish: 300,
        };
        let mut r = Resource {
            uid: 2,
            max_units: 0.5,
            kind: 0,
            periods: vec![(200, 1.0, true), (400, 0.5, true)],
        };
        assert!(assignment_overallocated(&a, &r));
        assert!(!assignment_overallocated(
            &Assignment { finish: 200, ..a },
            &r
        ));
        r.kind = 1;
        assert!(!assignment_overallocated(&a, &r));
        r.kind = 2;
        assert!(!assignment_overallocated(&a, &r));
        r.kind = 0;
        r.periods = vec![(200, 1.0, true), (400, 0.0, false), (500, 1.0, true)];
        assert!(assignment_overallocated(&a, &r)); // Project's f10 gap case
        r.periods = vec![(200, 1.0, true)];
        assert!(assignment_overallocated(&a, &r)); // Project's f10 end case
        assert!(!assignment_overallocated(
            &Assignment { finish: 200, ..a },
            &r
        ));
    }
}
