//! Strict readers for the current Project resource and assignment tables.
use crate::cfb::Cfb;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

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

fn record_ok(i: usize, kind: u16, len: usize, stubs: usize, row_len: usize) -> bool {
    if i < stubs {
        kind == 4 && len == 16
    } else {
        (matches!(kind, 0 | 2) && len == row_len) || (kind == 4 && len == 16)
    }
}

#[derive(Debug)]
pub(crate) struct Table {
    pub rows: Vec<Vec<u8>>,
    fields: HashMap<(u32, u16), Range<usize>>,
    v2: Vec<u8>,
}

impl Table {
    pub(crate) fn field(&self, uid: u32, key: u16) -> Option<&[u8]> {
        self.fields
            .get(&(uid, key))
            .map(|range| &self.v2[range.clone()])
    }
}

pub(crate) fn present(bytes: &[u8], name: &str) -> Result<bool, String> {
    let cfb = Cfb::open(bytes)?;
    Ok(cfb.paths().iter().any(|p| p.contains(&format!("{name}/"))))
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
    let expected_meta_len = 16usize.checked_add(
        count
            .checked_mul(meta_stride)
            .ok_or("FixedMeta count overflow")?,
    );
    if expected_meta_len != Some(fm.len()) {
        // A different complete entry stride identifies an older/unvalidated
        // layout. A torn index or impossible count is corruption.
        let payload = fm.len() - 16;
        if count == 0 || payload == 0 || payload % count != 0 {
            return Err(format!("{name}: FixedMeta count or length mismatch"));
        }
        let stride = payload / count;
        if stride < meta_stride {
            return Err(format!("{name}: FixedMeta count or length mismatch"));
        }
        // If the bytes form complete entries in the validated layout, the
        // header count is wrong. In particular, a lowered count can otherwise
        // masquerade as a larger unsupported stride.
        if payload.is_multiple_of(meta_stride) {
            let actual = payload / meta_stride;
            let offsets: Vec<_> = (0..actual)
                .map(|i| u32_at(&fm, 16 + i * meta_stride + 4) as usize)
                .collect();
            let valid = offsets.first() == Some(&0)
                && offsets.windows(2).all(|pair| pair[0] < pair[1])
                && offsets.iter().enumerate().all(|(i, &off)| {
                    let end = offsets.get(i + 1).copied().unwrap_or(fd.len());
                    let len = end.saturating_sub(off);
                    let kind = u16_at(&fm, 16 + i * meta_stride);
                    end <= fd.len() && record_ok(i, kind, len, stubs, row_len)
                });
            if valid {
                return Err(format!("{name}: FixedMeta count or length mismatch"));
            }
        }
        let offsets: Vec<_> = (0..count)
            .map(|i| u32_at(&fm, 16 + i * stride + 4) as usize)
            .collect();
        if offsets.first() != Some(&0)
            || offsets.windows(2).any(|pair| pair[0] >= pair[1])
            || offsets.last().is_some_and(|&last| last >= fd.len())
        {
            return Err(format!("{name}: FixedMeta count or length mismatch"));
        }
        return Ok(None);
    }
    if count < stubs {
        return Err(format!("{name}: FixedMeta count or length mismatch"));
    }
    let mut rows = Vec::with_capacity(count.saturating_sub(stubs));
    let mut indexed_uids = HashSet::new();
    let mut live_uids = HashSet::new();
    let mut previous_end = 0;
    for i in 0..count {
        let m = &fm[16 + i * meta_stride..16 + (i + 1) * meta_stride];
        let off = u32_at(m, 4) as usize;
        let end = if i + 1 < count {
            u32_at(&fm, 16 + (i + 1) * meta_stride + 4) as usize
        } else {
            fd.len()
        };
        let len = end
            .checked_sub(off)
            .ok_or_else(|| format!("{name}: FixedMeta offset order at record {i}"))?;
        let kind = u16_at(m, 0);
        let kind_ok = record_ok(i, kind, len, stubs, row_len);
        if off != previous_end || end > fd.len() {
            return Err(format!(
                "{name}: FixedMeta offset or length mismatch at record {i}"
            ));
        }
        if !kind_ok {
            return Ok(None); // Present table in an unvalidated row layout.
        }
        previous_end = end;
        if i < stubs || kind == 4 {
            continue;
        }
        let row = &fd[off..end];
        let uid = u32_at(row, if name == "TBkndRsc" { 4 } else { 0 });
        indexed_uids.insert(uid);
        if kind == 2 {
            continue;
        }
        if !live_uids.insert(uid) {
            return Err(format!("{name}: duplicate UID {uid}"));
        }
        rows.push(row.to_vec());
    }
    if vm.len() < 24 || vm[..4] != [0xba, 0xad, 0xdf, 0xfa] {
        return Err(format!("{name}: invalid VarMeta header"));
    }
    let vars = u32_at(&vm, 8) as usize;
    if 24usize.checked_add(vars.checked_mul(12).ok_or("VarMeta count overflow")?) != Some(vm.len())
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
        if u16_at(entry, 10) != marker || !indexed_uids.contains(&uid) {
            return Err(format!("{name}: invalid VarMeta entry {i}"));
        }
        let Some(head) = off.checked_add(4).filter(|&n| n <= v2.len()) else {
            return Err(format!("{name}: Var2Data offset at entry {i}"));
        };
        let len = u32_at(&v2, off) as usize;
        let Some(end) = head.checked_add(len).filter(|&n| n <= v2.len()) else {
            return Err(format!("{name}: Var2Data length at entry {i}"));
        };
        if fields.insert((uid, key), head..end).is_some() {
            return Err(format!("{name}: duplicate VarMeta key ({uid},{key})"));
        }
    }
    Ok(Some(Table { rows, fields, v2 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfb::{Node, write_cfb_tree};

    fn fixture(stride: usize, kind: u16, offset: u32) -> Vec<u8> {
        fixture_with_counts(stride, kind, offset, 1, 1, false)
    }

    fn fixture_with_counts(
        stride: usize,
        kind: u16,
        offset: u32,
        actual: usize,
        declared: u32,
        var_trailer: bool,
    ) -> Vec<u8> {
        let mut fm = vec![0u8; 16 + stride * actual];
        fm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        fm[8..12].copy_from_slice(&declared.to_le_bytes());
        let mut fd = vec![0u8; 110 * actual];
        for i in 0..actual {
            let at = 16 + i * stride;
            fm[at..at + 2].copy_from_slice(&kind.to_le_bytes());
            fm[at + 4..at + 8].copy_from_slice(&(offset + i as u32 * 110).to_le_bytes());
            fd[i * 110..i * 110 + 4].copy_from_slice(&(7 + i as u32).to_le_bytes());
        }
        let mut vm = vec![0u8; 24];
        vm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        if var_trailer {
            vm.extend([0u8; 12]);
        }
        write_cfb_tree(&[Node::Storage(
            "   114",
            vec![Node::Storage(
                "TBkndAssn",
                vec![
                    Node::Stream("FixedMeta", fm),
                    Node::Stream("FixedData", fd),
                    Node::Stream("VarMeta", vm),
                    Node::Stream("Var2Data", Vec::new()),
                ],
            )],
        )])
    }

    fn resource_fixture(first_kind: u16, declared: u32) -> Vec<u8> {
        let mut fm = vec![0u8; 16 + 4 * 37];
        fm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        fm[8..12].copy_from_slice(&declared.to_le_bytes());
        for (i, off) in [0u32, 16, 32, 48].iter().enumerate() {
            let at = 16 + i * 37;
            let kind = if i == 0 {
                first_kind
            } else if i < 3 {
                4
            } else {
                0
            };
            fm[at..at + 2].copy_from_slice(&kind.to_le_bytes());
            fm[at + 4..at + 8].copy_from_slice(&off.to_le_bytes());
        }
        let mut vm = vec![0u8; 24];
        vm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        write_cfb_tree(&[Node::Storage(
            "   114",
            vec![Node::Storage(
                "TBkndRsc",
                vec![
                    Node::Stream("FixedMeta", fm),
                    Node::Stream("FixedData", vec![0u8; 48 + 172]),
                    Node::Stream("VarMeta", vm),
                    Node::Stream("Var2Data", Vec::new()),
                ],
            )],
        )])
    }

    #[test]
    fn unsupported_layout_falls_back_but_corruption_errors() {
        assert!(present(&fixture(34, 0, 0), "TBkndAssn").unwrap());
        assert!(
            read(&fixture(35, 0, 0), "TBkndAssn", 34, 110, 0x0f40)
                .unwrap()
                .is_none()
        );
        assert!(
            read(&fixture(34, 9, 0), "TBkndAssn", 34, 110, 0x0f40)
                .unwrap()
                .is_none()
        );
        assert!(
            read(&fixture(34, 0, 1), "TBkndAssn", 34, 110, 0x0f40)
                .unwrap_err()
                .contains("offset")
        );
        assert!(
            read(&fixture(34, 0, 0), "TBkndAssn", 34, 110, 0x0f40)
                .unwrap()
                .is_some()
        );
        for count in [1, 2, 6] {
            assert!(
                read(
                    &fixture_with_counts(34, 0, 0, 3, count, false),
                    "TBkndAssn",
                    34,
                    110,
                    0x0f40
                )
                .unwrap_err()
                .contains("FixedMeta count or length mismatch")
            );
        }
        assert!(
            read(
                &fixture_with_counts(34, 0, 0, 0, 1, false),
                "TBkndAssn",
                34,
                110,
                0x0f40
            )
            .unwrap_err()
            .contains("FixedMeta count or length mismatch")
        );
        assert!(
            read(
                &fixture_with_counts(34, 0, 0, 1, 1, true),
                "TBkndAssn",
                34,
                110,
                0x0f40
            )
            .unwrap_err()
            .contains("VarMeta count or Var2Data length mismatch")
        );
    }

    #[test]
    fn lowered_resource_count_respects_schema_stubs() {
        assert!(
            read(&resource_fixture(4, 2), "TBkndRsc", 37, 172, 0x0c40)
                .unwrap_err()
                .contains("FixedMeta count or length mismatch")
        );
        assert!(
            read(&resource_fixture(0, 2), "TBkndRsc", 37, 172, 0x0c40)
                .unwrap()
                .is_none()
        );
        assert!(
            read(&resource_fixture(0, 4), "TBkndRsc", 37, 172, 0x0c40)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn shared_var_block_is_stored_once() {
        let mut fm = vec![0u8; 50];
        fm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        fm[8..12].copy_from_slice(&1u32.to_le_bytes());
        let mut fd = vec![0u8; 110];
        fd[..4].copy_from_slice(&7u32.to_le_bytes());
        let mut vm = vec![0u8; 48];
        vm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        vm[8..12].copy_from_slice(&2u32.to_le_bytes());
        let mut v2 = vec![42u8; 65536];
        v2.splice(0..0, 65536u32.to_le_bytes());
        vm[20..24].copy_from_slice(&(v2.len() as u32).to_le_bytes());
        for i in 0..2 {
            let at = 24 + i * 12;
            vm[at..at + 4].copy_from_slice(&7u32.to_le_bytes());
            vm[at + 8..at + 10].copy_from_slice(&((i + 1) as u16).to_le_bytes());
            vm[at + 10..at + 12].copy_from_slice(&0x0f40u16.to_le_bytes());
        }
        let bytes = write_cfb_tree(&[Node::Storage(
            "   114",
            vec![Node::Storage(
                "TBkndAssn",
                vec![
                    Node::Stream("FixedMeta", fm),
                    Node::Stream("FixedData", fd),
                    Node::Stream("VarMeta", vm),
                    Node::Stream("Var2Data", v2),
                ],
            )],
        )]);
        let table = read(&bytes, "TBkndAssn", 34, 110, 0x0f40).unwrap().unwrap();
        assert_eq!(table.v2.len(), 65540);
        assert_eq!(
            table.field(7, 1).unwrap().as_ptr(),
            table.field(7, 2).unwrap().as_ptr()
        );
    }
}
