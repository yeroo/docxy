//! The MPP **VarMeta** index — the table that says which [`crate::vardata`]
//! block holds which field of which item.
//!
//! A VarMeta stream begins with the magic `0xFADFADBA` and an item count, then
//! a run of fixed-size entries, each naming an item's *unique id*, a *field
//! type*, and the *offset* of that field's value in the sibling `Var2Data`
//! stream. The entry layout is **version-specific** — Project 2000-era files
//! (MPP9) and Project 2007+ files (MPP12/14) differ in entry size, field order,
//! and the field-type constant for a task name.
//!
//! Rather than hard-code a version, we **auto-detect**: try each known layout
//! and keep the one whose name-field offsets land on real `Var2Data` block
//! boundaries and decode as text. Alignment with the block stream is a strong,
//! self-validating check — verified against real `.mpp` files.

use crate::vardata;
use std::collections::HashMap;

const MAGIC: u32 = 0xFADF_ADBA;

fn u32le(b: &[u8], o: usize) -> u32 {
    if o + 4 <= b.len() {
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    } else {
        0
    }
}
fn u16le(b: &[u8], o: usize) -> u16 {
    if o + 2 <= b.len() {
        u16::from_le_bytes([b[o], b[o + 1]])
    } else {
        0
    }
}

/// One known VarMeta entry shape: where the field-type `u16` sits relative to
/// the `u32` Var2Data offset. The name field-type is not hard-coded — it's
/// discovered as the most-populated non-zero text field.
const FIELD_RELS: [isize; 3] = [
    -2, // MPP9 (Project 2000/2002/2003):  [uid:u16][field:u16][offset:u32]
    6,  // MPP12/14 (Project 2007+):       [offset:u32][…][field:u16]
    -8, // newest MPP14:  [field:u16][0x0b40][item:u32][offset:u32]
];

fn u16_at(b: &[u8], p: isize) -> u16 {
    if p >= 0 { u16le(b, p as usize) } else { 0 }
}

/// Extract the task (or resource) **names** from a `VarMeta` + `Var2Data` pair,
/// in stream order (which is item order). Empty if the stream isn't a VarMeta.
///
/// Rather than assume a fixed header/stride (which differs by version and isn't
/// always a flat array), we **scan** the whole VarMeta for any `u32` that points
/// at a real `Var2Data` string block, then — for each candidate entry shape —
/// group those hits by the field-type at the expected position and keep the
/// most-populated non-zero field. That field is the name; offset-alignment with
/// the block stream self-validates the decode. Each block belongs to one item,
/// so offsets are claimed once (the real entry precedes the zero-padding that
/// re-points at offset 0). Verified on real MPP9 and MPP14 files.
pub fn names(varmeta: &[u8], var2data: &[u8]) -> Vec<String> {
    if u32le(varmeta, 0) != MAGIC {
        return Vec::new();
    }
    // Read each block *at* the offset its VarMeta entry points at (robust to the
    // non-contiguous Var2Data of newer files, where a sequential walk stalls).
    let blocks: HashMap<usize, String> = {
        let mut m = HashMap::new();
        let mut p = 0usize;
        while p + 4 <= varmeta.len() {
            let off = u32le(varmeta, p) as usize;
            m.entry(off)
                .or_insert_with(|| vardata::string_at(var2data, off));
            p += 2;
        }
        m.into_iter()
            .filter_map(|(o, s)| s.map(|s| (o, s)))
            .collect()
    };

    let mut best: Vec<String> = Vec::new();
    let mut best_score = 0usize;
    for &field_rel in &FIELD_RELS {
        let mut by_field: HashMap<u16, (std::collections::HashSet<usize>, Vec<String>)> =
            HashMap::new();
        let mut p = 0usize;
        while p + 4 <= varmeta.len() {
            let off = u32le(varmeta, p) as usize;
            if let Some(name) = blocks.get(&off) {
                let field = u16_at(varmeta, p as isize + field_rel);
                if field != 0 {
                    let e = by_field.entry(field).or_default();
                    if e.0.insert(off) {
                        e.1.push(name.clone());
                    }
                }
            }
            p += 2; // offsets are 2-byte aligned
        }
        // The name field is the one with the most multi-character strings — but
        // only among *pure* fields (mostly multi-char). A wrong entry layout
        // reads a bogus field-type that merges the real names with a stray
        // 1-char marker field ('); that merged group has the same multi-char
        // count but is only ~half multi-char, so the purity gate rejects it and
        // lets the correctly-separated name field win.
        let mc = |l: &Vec<String>| l.iter().filter(|s| s.chars().count() >= 2).count();
        for (_, list) in by_field.into_values() {
            let score = mc(&list);
            if score * 5 >= list.len() * 4 && score > best_score {
                best_score = score;
                best = list;
            }
        }
    }
    best
}

/// One 12-byte keyed VarMeta entry, as MPP12 and current Project write
/// them: the item's UID `u32` at +0, the Var2Data offset `u32` of its value
/// at +4, the field key `u16` at +8 and the table's tag `u16` at +10
/// (`0x0b40` tasks, `0x0c40` resources, `0x0d40` calendars, `0x0f40`
/// assignments).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VarEntry<'a> {
    /// The UID of the row the value belongs to.
    pub uid: u32,
    /// Offset of the value's `u32` length prefix in Var2Data.
    pub offset: usize,
    /// The field key, such as `0x000e` for a task name.
    pub key: u16,
    /// The table's tag, the same on every entry of one table.
    pub tag: u16,
    /// The value: the Var2Data bytes after the length prefix.
    pub value: &'a [u8],
}

/// Read the counted 12-byte entries of a VarMeta stream against its
/// Var2Data, in stream order. The 24-byte header must start with the magic
/// and give the entry count at +8 and the Var2Data length at +20; every
/// value must lie inside Var2Data, and a `(uid, key)` pair may appear only
/// once. Bytes after the counted entries are ignored, as the task reader
/// ignores them (the resource, assignment and calendar readers refuse
/// them). UIDs are not checked against a FixedMeta index and tags are
/// returned as found. MPP9's 8-byte entries are not read.
pub fn var_entries<'a>(varmeta: &[u8], var2data: &'a [u8]) -> Result<Vec<VarEntry<'a>>, String> {
    let keyed = entries(varmeta, var2data, Trailing::Ignored).map_err(|e| match e {
        HeaderError::Magic => "invalid VarMeta header".to_string(),
        HeaderError::Length => "VarMeta count or Var2Data length mismatch".to_string(),
    })?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for e in keyed {
        let i = e.index;
        let value = block(var2data, e.offset).map_err(|b| match b {
            BlockError::Offset => format!("Var2Data offset out of range at entry {i}"),
            BlockError::Length => format!("Var2Data block out of range at entry {i}"),
        })?;
        if !seen.insert((e.uid, e.key)) {
            return Err(format!("duplicate VarMeta key ({},{})", e.uid, e.key));
        }
        out.push(VarEntry {
            uid: e.uid,
            offset: e.offset,
            key: e.key,
            tag: e.tag,
            value: &var2data[value],
        });
    }
    Ok(out)
}

/// What [`entries`] does with bytes after the counted entries (#438). The
/// task reader and [`var_entries`] ignore them; the resource, assignment and
/// calendar readers refuse them, as each did before the copies were shared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Trailing {
    Ignored,
    Refused,
}

/// Why a VarMeta stream's header is refused: no magic or under 24 bytes
/// (`Magic`), or a count that the stream cannot hold (or, with
/// [`Trailing::Refused`], does not fill exactly) or a Var2Data length that
/// is not the sibling stream's (`Length`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeaderError {
    Magic,
    Length,
}

/// One counted 12-byte entry as stored, before its value is checked:
/// `index` is its place among the counted entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawEntry {
    pub index: usize,
    pub uid: u32,
    pub offset: usize,
    pub key: u16,
    pub tag: u16,
}

/// The one VarMeta header check (#438): magic, the entry count at +8, the
/// Var2Data length at +20. Yields the counted 12-byte entries in stream
/// order. Callers check tags, UIDs, values ([`block`]) and duplicates in
/// their own order, so each keeps its own error for a malformed entry.
pub(crate) fn entries<'a>(
    varmeta: &'a [u8],
    var2data: &[u8],
    trailing: Trailing,
) -> Result<impl Iterator<Item = RawEntry> + 'a, HeaderError> {
    let count = counted(varmeta, var2data, trailing, 12)?;
    Ok((0..count).map(move |index| {
        let e = &varmeta[24 + index * 12..36 + index * 12];
        RawEntry {
            index,
            uid: u32le(e, 0),
            offset: u32le(e, 4) as usize,
            key: u16le(e, 8),
            tag: u16le(e, 10),
        }
    }))
}

/// [`entries`] for MPP9's 8-byte entries (`[uid u16][key u16][offset
/// u32]`), which carry no tag (`tag` is 0).
pub(crate) fn legacy_entries<'a>(
    varmeta: &'a [u8],
    var2data: &[u8],
) -> Result<impl Iterator<Item = RawEntry> + 'a, HeaderError> {
    let count = counted(varmeta, var2data, Trailing::Ignored, 8)?;
    Ok((0..count).map(move |index| {
        let e = &varmeta[24 + index * 8..32 + index * 8];
        RawEntry {
            index,
            uid: u32::from(u16le(e, 0)),
            offset: u32le(e, 4) as usize,
            key: u16le(e, 2),
            tag: 0,
        }
    }))
}

/// The header check behind [`entries`]: the number of `stride`-byte
/// entries the header counts.
fn counted(
    varmeta: &[u8],
    var2data: &[u8],
    trailing: Trailing,
    stride: usize,
) -> Result<usize, HeaderError> {
    if varmeta.len() < 24 || u32le(varmeta, 0) != MAGIC {
        return Err(HeaderError::Magic);
    }
    let count = u32le(varmeta, 8) as usize;
    let end = count.checked_mul(stride).and_then(|n| n.checked_add(24));
    let fits = match trailing {
        Trailing::Ignored => end.is_some_and(|n| n <= varmeta.len()),
        Trailing::Refused => end == Some(varmeta.len()),
    };
    if !fits || u32le(varmeta, 20) as usize != var2data.len() {
        return Err(HeaderError::Length);
    }
    Ok(count)
}

/// Why a Var2Data block is refused: its `u32` length prefix is not inside
/// the stream (`Offset`), or the value it counts runs past the end
/// (`Length`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockError {
    Offset,
    Length,
}

/// The value of the Var2Data block at `offset`: the bytes after its `u32`
/// length prefix, as a range of `var2data`.
pub(crate) fn block(var2data: &[u8], offset: usize) -> Result<std::ops::Range<usize>, BlockError> {
    let start = offset
        .checked_add(4)
        .filter(|&n| n <= var2data.len())
        .ok_or(BlockError::Offset)?;
    let end = start
        .checked_add(u32le(var2data, offset) as usize)
        .filter(|&n| n <= var2data.len())
        .ok_or(BlockError::Length)?;
    Ok(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A VarMeta header counting `count` entries of `stride` bytes, followed
    /// by `entries` and then `extra` trailing bytes, for a Var2Data of
    /// `v2_len` bytes.
    fn varmeta(count: u32, entries: &[u8], extra: usize, v2_len: u32) -> Vec<u8> {
        let mut vm = vec![0u8; 24];
        vm[..4].copy_from_slice(&MAGIC.to_le_bytes());
        vm[8..12].copy_from_slice(&count.to_le_bytes());
        vm[20..24].copy_from_slice(&v2_len.to_le_bytes());
        vm.extend_from_slice(entries);
        vm.extend(std::iter::repeat_n(0, extra));
        vm
    }

    fn entry(uid: u32, offset: u32, key: u16, tag: u16) -> Vec<u8> {
        let mut e = uid.to_le_bytes().to_vec();
        e.extend(offset.to_le_bytes());
        e.extend(key.to_le_bytes());
        e.extend(tag.to_le_bytes());
        e
    }

    #[test]
    fn shared_header_keeps_each_callers_trailing_rule() {
        let v2 = [2, 0, 0, 0, 0x41, 0];
        let vm = varmeta(1, &entry(7, 0, 0x0e, 0x0b40), 4, 6);
        let got: Vec<_> = entries(&vm, &v2, Trailing::Ignored).unwrap().collect();
        assert_eq!(
            got,
            [RawEntry {
                index: 0,
                uid: 7,
                offset: 0,
                key: 0x0e,
                tag: 0x0b40
            }]
        );
        assert_eq!(
            entries(&vm, &v2, Trailing::Refused).err(),
            Some(HeaderError::Length)
        );
        let exact = varmeta(1, &entry(7, 0, 0x0e, 0x0b40), 0, 6);
        assert_eq!(entries(&exact, &v2, Trailing::Refused).unwrap().count(), 1);
        // A count past the stream, a wrong Var2Data length or no magic.
        assert!(entries(&varmeta(2, &[], 12, 6), &v2, Trailing::Ignored).is_err());
        assert!(entries(&varmeta(1, &[0; 12], 0, 5), &v2, Trailing::Ignored).is_err());
        assert_eq!(
            entries(&[0; 24], &v2, Trailing::Ignored).err(),
            Some(HeaderError::Magic)
        );
    }

    #[test]
    fn legacy_entries_read_eight_byte_entries() {
        let mut e = 3u16.to_le_bytes().to_vec();
        e.extend(0x0b00u16.to_le_bytes());
        e.extend(0u32.to_le_bytes());
        let vm = varmeta(1, &e, 0, 4);
        let got: Vec<_> = legacy_entries(&vm, &[0; 4]).unwrap().collect();
        assert_eq!((got[0].uid, got[0].key, got[0].tag), (3, 0x0b00, 0));
    }

    #[test]
    fn blocks_are_bounded_by_var2data() {
        let v2 = [2, 0, 0, 0, 0x41, 0];
        assert_eq!(block(&v2, 0), Ok(4..6));
        assert_eq!(block(&v2, 3), Err(BlockError::Offset));
        assert_eq!(block(&[9, 0, 0, 0, 0], 0), Err(BlockError::Length));
        assert_eq!(block(&v2, usize::MAX), Err(BlockError::Offset));
    }

    // Build a Var2Data stream from named blocks; return the bytes and each
    // name's block offset.
    fn build_var2(names: &[&str]) -> (Vec<u8>, Vec<usize>) {
        let mut data = Vec::new();
        let mut offs = Vec::new();
        for n in names {
            offs.push(data.len());
            let mut s: Vec<u8> = n.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
            s.extend_from_slice(&[0, 0]);
            data.extend_from_slice(&(s.len() as u32).to_le_bytes());
            data.extend_from_slice(&s);
        }
        (data, offs)
    }

    #[test]
    fn decodes_mpp9_layout() {
        let (v2, offs) = build_var2(&["Alpha", "Beta", "Gamma"]);
        let mut vm = Vec::new();
        vm.extend_from_slice(&MAGIC.to_le_bytes());
        vm.resize(24, 0); // header
        for (uid, off) in offs.iter().enumerate() {
            vm.extend_from_slice(&(uid as u16).to_le_bytes());
            vm.extend_from_slice(&0x0B00u16.to_le_bytes());
            vm.extend_from_slice(&(*off as u32).to_le_bytes());
        }
        let got = names(&vm, &v2);
        assert_eq!(
            got,
            vec!["Alpha".to_string(), "Beta".into(), "Gamma".into()]
        );
    }

    #[test]
    fn decodes_mpp14_layout() {
        let (v2, offs) = build_var2(&["One", "Two"]);
        let mut vm = Vec::new();
        vm.extend_from_slice(&MAGIC.to_le_bytes());
        vm.resize(28, 0);
        for (uid, off) in offs.iter().enumerate() {
            vm.extend_from_slice(&(*off as u32).to_le_bytes());
            vm.extend_from_slice(&(uid as u16).to_le_bytes());
            vm.extend_from_slice(&0x0B40u16.to_le_bytes());
            vm.extend_from_slice(&0u32.to_le_bytes()); // pad
        }
        let got = names(&vm, &v2);
        assert_eq!(got, vec!["One".to_string(), "Two".into()]);
    }

    #[test]
    fn decodes_newest_layout_over_marker_field() {
        // Newest MPP14: 12-byte entries [field:u16][0x0B40][item:u32][offset:u32]
        // (field at offset−8). A stray 1-char marker field ('), one per task,
        // shares the constant 0x0B40 marker position — so a wrong entry layout
        // merges names+markers into one impure group. The correct field split
        // plus the purity gate must recover the names alone.
        let (v2, offs) = build_var2(&[
            "Business Understanding",
            "'",
            "Modeling",
            "'",
            "Deployment",
            "'",
        ]);
        let mut vm = Vec::new();
        vm.extend_from_slice(&MAGIC.to_le_bytes());
        vm.resize(32, 0); // header
        // field 0x0006 = names (even offsets), 0x00AA = the ' marker (odd offsets)
        for (i, off) in offs.iter().enumerate() {
            let field: u16 = if i % 2 == 0 { 0x0006 } else { 0x00AA };
            vm.extend_from_slice(&field.to_le_bytes());
            vm.extend_from_slice(&0x0B40u16.to_le_bytes());
            vm.extend_from_slice(&(i as u32).to_le_bytes());
            vm.extend_from_slice(&(*off as u32).to_le_bytes());
        }
        let got = names(&vm, &v2);
        assert_eq!(
            got,
            vec![
                "Business Understanding".to_string(),
                "Modeling".into(),
                "Deployment".into()
            ]
        );
    }

    #[test]
    fn rejects_non_varmeta() {
        assert!(names(b"not varmeta at all", &[]).is_empty());
    }

    fn keyed(entries: &[(u32, u32, u16)], v2_len: usize) -> Vec<u8> {
        let mut vm = MAGIC.to_le_bytes().to_vec();
        vm.resize(24, 0);
        vm[8..12].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        vm[20..24].copy_from_slice(&(v2_len as u32).to_le_bytes());
        for &(uid, off, key) in entries {
            vm.extend_from_slice(&uid.to_le_bytes());
            vm.extend_from_slice(&off.to_le_bytes());
            vm.extend_from_slice(&key.to_le_bytes());
            vm.extend_from_slice(&0x0b40u16.to_le_bytes());
        }
        vm
    }

    #[test]
    fn var_entries_read_counted_keyed_values() {
        let (v2, offs) = build_var2(&["One", "Two"]);
        let mut vm = keyed(
            &[(7, offs[0] as u32, 0x0e), (8, offs[1] as u32, 0x0e)],
            v2.len(),
        );
        let got = var_entries(&vm, &v2).unwrap();
        assert_eq!(
            got.iter()
                .map(|e| (e.uid, e.key, e.tag, e.offset))
                .collect::<Vec<_>>(),
            [(7, 0x0e, 0x0b40, offs[0]), (8, 0x0e, 0x0b40, offs[1])]
        );
        assert_eq!(
            vardata::utf16le_string(got[1].value).as_deref(),
            Some("Two")
        );
        // Bytes past the counted entries have no standing.
        vm.extend_from_slice(&[0xff; 12]);
        assert_eq!(var_entries(&vm, &v2).unwrap().len(), 2);
        let err = |vm: &[u8], v2: &[u8]| var_entries(vm, v2).unwrap_err();
        assert!(err(&vm[..20], &v2).contains("header"));
        assert!(err(&vm, &v2[1..]).contains("length mismatch"));
        let past = keyed(&[(7, v2.len() as u32, 0x0e)], v2.len());
        assert!(err(&past, &v2).contains("offset out of range"));
        let torn = keyed(&[(7, offs[1] as u32 + 1, 0x0e)], v2.len());
        assert!(err(&torn, &v2).contains("block out of range"));
        let twice = keyed(&[(7, 0, 0x0e), (7, offs[1] as u32, 0x0e)], v2.len());
        assert!(err(&twice, &v2).contains("duplicate"));
        let mut short = keyed(&[(7, 0, 0x0e)], v2.len());
        short[8..12].copy_from_slice(&2u32.to_le_bytes());
        assert!(err(&short, &v2).contains("count"));
    }
}
