//! Strict readers for the current Project resource and assignment tables.
//!
//! A table is the four streams `FixedMeta`, `FixedData`, `VarMeta` and
//! `Var2Data` of one `TBknd*` storage. FixedMeta is a 16-byte header (magic
//! `ba ad df fa`, entry count at +8) and fixed-stride entries holding the
//! kind `u16` at +0 and the FixedData offset `u32` at +4; each record runs to
//! the next entry's offset. VarMeta is a 24-byte header (magic, entry count
//! at +8, Var2Data length at +20) and 12-byte entries: UID `u32` at +0,
//! Var2Data offset `u32` at +4, field key `u16` at +8 and the table's tag
//! `u16` at +10. Each Var2Data value is a `u32` length and that many bytes.
use crate::cfb::Cfb;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// The little-endian `u16` at `at`. Panics past the end of `b`.
pub fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
/// The little-endian `u32` at `at`. Panics past the end of `b`.
pub fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
/// The little-endian `i32` at `at`. Panics past the end of `b`.
pub fn i32_at(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
/// The little-endian `f64` at `at`. Panics past the end of `b`.
pub fn f64_at(b: &[u8], at: usize) -> f64 {
    f64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn record_ok(i: usize, kind: u16, len: usize, stubs: usize, row_len: usize) -> bool {
    if i < stubs {
        kind == 4 && len == 16
    } else {
        (matches!(kind, 0 | 2) && len == row_len) || (kind == 4 && len == 16)
    }
}

/// The shape [`read`] validates a table against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableLayout {
    /// The storage name, such as `TBkndRsc`.
    pub name: &'static str,
    /// Length of one FixedMeta entry.
    pub meta_stride: usize,
    /// Length of one live FixedData row.
    pub row_len: usize,
    /// The VarMeta tag `u16` at +10 of every entry.
    pub marker: u16,
}

impl TableLayout {
    /// [`read`] this table from a `.mpp` file's bytes.
    pub fn read(&self, bytes: &[u8]) -> Result<Option<Table>, String> {
        read(
            bytes,
            self.name,
            self.meta_stride,
            self.row_len,
            self.marker,
        )
    }
}

/// Current Project resources: 37-byte FixedMeta entries, three 16-byte
/// schema stubs, 172-byte rows (ID at +0, UID at +4, max units × 10000 as an
/// f64 at +8, standard rate format at +166, work/other flag at +170) and
/// VarMeta tag `0x0c40` (name under key 1, material label under key 299).
/// Validated against newest-generation Project files only.
pub const RESOURCE_TABLE: TableLayout = TableLayout {
    name: "TBkndRsc",
    meta_stride: 37,
    row_len: 172,
    marker: 0x0c40,
};

/// Current Project assignments: 34-byte FixedMeta entries, no schema stubs,
/// 110-byte rows (UID at +0, task UID at +4, resource UID at +8, units ×
/// 10000 as an f64 at +12, work in thousandths of a minute as an f64 at +20,
/// start and finish timestamps at +52 and +56) and VarMeta tag `0x0f40`
/// (Baseline work, cost, start and finish under keys `0x10`, `0x20`, `0x92`
/// and `0x93`; Baseline*n* under `0x121`, `0x122`, `0x127` and `0x128` plus
/// `9 × (n - 1)`). Validated against newest-generation Project files only.
pub const ASSIGNMENT_TABLE: TableLayout = TableLayout {
    name: "TBkndAssn",
    meta_stride: 34,
    row_len: 110,
    marker: 0x0f40,
};

/// A validated table: its live rows and the Var2Data values VarMeta keys.
#[derive(Debug)]
pub struct Table {
    /// The FixedData bytes of each live (kind 0) row, in FixedMeta order.
    /// Schema stubs, kind-4 16-byte rows and superseded (kind 2) rows are
    /// left out.
    pub rows: Vec<Vec<u8>>,
    fields: HashMap<(u32, u16), Range<usize>>,
    v2: Vec<u8>,
}

impl Table {
    /// The Var2Data value VarMeta keys under `(uid, key)`, without its
    /// length prefix.
    pub fn field(&self, uid: u32, key: u16) -> Option<&[u8]> {
        self.fields
            .get(&(uid, key))
            .map(|range| &self.v2[range.clone()])
    }

    /// Every keyed value as `(uid, key, value)`, sorted by UID and key.
    pub fn fields(&self) -> Vec<(u32, u16, &[u8])> {
        let mut out: Vec<_> = self
            .fields
            .iter()
            .map(|(&(uid, key), range)| (uid, key, &self.v2[range.clone()]))
            .collect();
        out.sort_by_key(|&(uid, key, _)| (uid, key));
        out
    }
}

/// Whether the file has any stream under a `name/` storage.
pub fn present(bytes: &[u8], name: &str) -> Result<bool, String> {
    let cfb = Cfb::open(bytes)?;
    Ok(cfb.paths().iter().any(|p| p.contains(&format!("{name}/"))))
}

/// Read and validate the `name` table (see [`RESOURCE_TABLE`] and
/// [`ASSIGNMENT_TABLE`] for the validated layouts). `Ok(None)` means the
/// file has no such table, or has one in another entry stride or row layout,
/// which an older or unvalidated generation writes. A torn index, offsets out
/// of order, a partial stream set, duplicate live UIDs, or a VarMeta entry
/// with another tag, an unindexed UID or a block outside Var2Data is an error.
/// `TBkndRsc` is read with three schema stubs and its UID at row +4; any
/// other name with no stubs and its UID at row +0.
pub fn read(
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
