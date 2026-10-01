//! Task IsPublished, kept by current Project on the task's assignment rows.
//!
//! Saving one plan twice with only a task's Publish changed alters nothing in
//! the task table: the assignment Fixed2Meta entry (53 bytes, one per
//! FixedMeta entry) clears bit 0x40 of its byte +8. Every non-summary task has
//! an assignment row, the unassigned placeholder (resource -65535) included,
//! and summaries have none. Checked against every Project XML row of the
//! snapshot, task-fields, task-extra and progress corpora: a task exports
//! IsPublished=1 exactly when it has assignments and all of them carry the bit
//! (task-extra/e2-published unpublishes an ordinary task; Project exports 0 for
//! summaries and inactive tasks whatever was set before).
use crate::cfb::Cfb;
use crate::overalloc::{count, stream};
use std::collections::{HashMap, HashSet};

const ASSIGNMENT_META: usize = 34;
const ASSIGNMENT_ROW: usize = 110;
const ASSIGNMENT_FIXED2_META: usize = 53;
const PUBLISHED: (usize, u8) = (8, 0x40);

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// IsPublished for each of `tasks`. A task whose assignments disagree has no
/// oracle and is left out.
pub(crate) fn decode(cfb: &Cfb, tasks: &HashSet<u32>) -> Result<HashMap<u32, bool>, String> {
    let fm = stream(cfb, "TBkndAssn", "FixedMeta")?;
    let fd = stream(cfb, "TBkndAssn", "FixedData")?;
    let f2m = stream(cfb, "TBkndAssn", "Fixed2Meta")?;
    let f2d = stream(cfb, "TBkndAssn", "Fixed2Data")?;
    let n = count(&fm, ASSIGNMENT_META, fd.len(), 0)?;
    if count(&f2m, ASSIGNMENT_FIXED2_META, f2d.len(), 0)? != n {
        return Err("assignment Fixed2Meta count mismatch".into());
    }
    let mut marks: HashMap<u32, (bool, bool)> = HashMap::new();
    for i in 0..n {
        let meta = &fm[16 + i * ASSIGNMENT_META..16 + (i + 1) * ASSIGNMENT_META];
        let at = u32_at(meta, 4) as usize;
        let end = if i + 1 < n {
            u32_at(&fm, 16 + (i + 1) * ASSIGNMENT_META + 4) as usize
        } else {
            fd.len()
        };
        if end < at || end > fd.len() {
            return Err("assignment offset mismatch".into());
        }
        // Blank (kind 4) and deleted (kind 2) rows carry no live assignment.
        if u16_at(meta, 0) != 0 {
            continue;
        }
        if end - at != ASSIGNMENT_ROW {
            return Err("assignment record length mismatch".into());
        }
        let row = &fd[at..end];
        let (task_uid, resource_uid) = (u32_at(row, 4), u32_at(row, 8));
        if task_uid == 0 && resource_uid == 0 {
            continue; // Internal placeholder absent from Project XML.
        }
        if !tasks.contains(&task_uid) {
            return Err("assignment for unknown task".into());
        }
        let entry = &f2m[16 + i * ASSIGNMENT_FIXED2_META..16 + (i + 1) * ASSIGNMENT_FIXED2_META];
        let published = entry[PUBLISHED.0] & PUBLISHED.1 != 0;
        let (any, all) = marks.entry(task_uid).or_insert((false, true));
        *any |= published;
        *all &= published;
    }
    Ok(tasks
        .iter()
        .filter_map(|&uid| match marks.get(&uid) {
            None => Some((uid, false)),
            Some(&(any, all)) if any == all => Some((uid, all)),
            Some(_) => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfb::{Node, write_cfb_tree};

    /// Assignment tables of (kind, task UID, resource UID, published) rows.
    fn file(rows: &[(u16, u32, u32, bool)]) -> Vec<u8> {
        let n = rows.len();
        let header = |stride: usize, data_len: usize| {
            let mut h = vec![0u8; 16 + n * stride];
            h[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
            h[8..12].copy_from_slice(&(n as u32).to_le_bytes());
            h[12..16].copy_from_slice(&(data_len as u32).to_le_bytes());
            h
        };
        let (mut fd, mut offsets) = (Vec::new(), Vec::new());
        for (i, &(kind, task, resource, _)) in rows.iter().enumerate() {
            offsets.push(fd.len());
            let mut row = vec![0u8; if kind == 4 { 16 } else { ASSIGNMENT_ROW }];
            if kind != 4 {
                row[..4].copy_from_slice(&(i as u32 + 1).to_le_bytes());
                row[4..8].copy_from_slice(&task.to_le_bytes());
                row[8..12].copy_from_slice(&resource.to_le_bytes());
            }
            fd.extend(row);
        }
        let mut fm = header(ASSIGNMENT_META, fd.len());
        let mut f2m = header(ASSIGNMENT_FIXED2_META, n * 48);
        for (i, &(kind, _, _, published)) in rows.iter().enumerate() {
            let m = 16 + i * ASSIGNMENT_META;
            fm[m..m + 2].copy_from_slice(&kind.to_le_bytes());
            fm[m + 4..m + 8].copy_from_slice(&(offsets[i] as u32).to_le_bytes());
            // Byte +8 is 0x4f/0x0f in Project's files: only bit 0x40 is Publish.
            f2m[16 + i * ASSIGNMENT_FIXED2_META + PUBLISHED.0] =
                0x0f | if published { 0x40 } else { 0 };
        }
        write_cfb_tree(&[Node::Storage(
            "   114",
            vec![Node::Storage(
                "TBkndAssn",
                vec![
                    Node::Stream("FixedMeta", fm),
                    Node::Stream("FixedData", fd),
                    Node::Stream("Fixed2Meta", f2m),
                    Node::Stream("Fixed2Data", vec![0; n * 48]),
                ],
            )],
        )])
    }

    fn published(rows: &[(u16, u32, u32, bool)]) -> Result<Vec<(u32, bool)>, String> {
        let bytes = file(rows);
        let cfb = Cfb::open(&bytes).unwrap();
        let mut out: Vec<_> = decode(&cfb, &HashSet::from([0, 1, 2, 3, 4, 5]))?
            .into_iter()
            .collect();
        out.sort();
        Ok(out)
    }

    #[test]
    fn a_task_is_published_when_all_its_assignments_are_marked() {
        const UNASSIGNED: u32 = (-65535i32) as u32;
        assert_eq!(
            published(&[
                (0, 0, 0, true),           // internal placeholder: ignored
                (0, 1, UNASSIGNED, true),  // ordinary task
                (0, 2, UNASSIGNED, false), // e2: Publish cleared
                (0, 3, 7, true),
                (0, 3, 8, true),
                (0, 4, 7, true), // mixed: no oracle
                (0, 4, 8, false),
                (2, 5, 7, true), // deleted row
                (4, 0, 0, false),
            ]),
            Ok(vec![
                (0, false),
                (1, true),
                (2, false),
                (3, true),
                (5, false)
            ])
        );
        assert_eq!(
            published(&[]),
            Ok((0..=5).map(|uid| (uid, false)).collect())
        );
        assert!(published(&[(0, 9, UNASSIGNED, true)]).is_err());
    }
}
