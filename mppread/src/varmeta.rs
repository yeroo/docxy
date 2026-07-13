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
    if p >= 0 {
        u16le(b, p as usize)
    } else {
        0
    }
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
            m.entry(off).or_insert_with(|| vardata::string_at(var2data, off));
            p += 2;
        }
        m.into_iter().filter_map(|(o, s)| s.map(|s| (o, s))).collect()
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(got, vec!["Alpha".to_string(), "Beta".into(), "Gamma".into()]);
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
        let (v2, offs) = build_var2(&["Business Understanding", "'", "Modeling", "'", "Deployment", "'"]);
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
        assert_eq!(got, vec!["Business Understanding".to_string(), "Modeling".into(), "Deployment".into()]);
    }

    #[test]
    fn rejects_non_varmeta() {
        assert!(names(b"not varmeta at all", &[]).is_empty());
    }
}
