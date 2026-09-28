//! Current Project assignment identity and saved baseline slots.
use crate::tabledecode::{f64_at, i32_at, u32_at};
use projcore::model::{AssignmentBaseline, ResourceType};
use projcore::{Assignment, DateTime, Rate, Resource};
use std::collections::HashSet;

fn date(bytes: &[u8], uid: u32, what: &str) -> Result<Option<DateTime>, String> {
    if bytes.len() != 4 {
        return Err(format!("invalid {what} length for assignment UID {uid}"));
    }
    let t = u16::from_le_bytes(bytes[..2].try_into().unwrap());
    let day = u16::from_le_bytes(bytes[2..].try_into().unwrap());
    if day == 0xffff {
        return Ok(None);
    }
    if t >= 14400 && t != 0xffff {
        return Err(format!("invalid {what} time for assignment UID {uid}"));
    }
    let s = crate::mpp::decode_timestamp(bytes, 0)
        .ok_or_else(|| format!("invalid {what} for assignment UID {uid}"))?;
    crate::project::parse_mpp_dt(&s)
        .map(Some)
        .ok_or_else(|| format!("invalid {what} for assignment UID {uid}"))
}
fn money(v: f64, uid: u32, what: &str) -> Result<Rate, String> {
    if !v.is_finite() {
        return Err(format!("invalid {what} for assignment UID {uid}"));
    }
    crate::taskdecode::cost_rate(v)
        .ok_or_else(|| format!("invalid {what} for assignment UID {uid}"))
}
fn work(v: f64, uid: u32, what: &str) -> Result<i64, String> {
    crate::taskdecode::work_minutes(v)
        .ok_or_else(|| format!("invalid {what} for assignment UID {uid}"))
}

pub(crate) fn decode(
    bytes: &[u8],
    legacy: bool,
    task_uids: &HashSet<i32>,
    resources: &[Resource],
) -> Result<Option<Vec<Assignment>>, String> {
    if legacy {
        return Ok(None);
    }
    let Some(table) = crate::tabledecode::read(bytes, "TBkndAssn", 34, 110, 0x0f40)? else {
        return Ok(None);
    };
    let resource_uids: HashSet<_> = resources.iter().map(|r| r.uid).collect();
    let mut out = Vec::new();
    for row in &table.rows {
        let (uid, task_uid, resource_uid) = (u32_at(row, 0), i32_at(row, 4), i32_at(row, 8));
        // A task-0/resource-0 row is an internal placeholder absent from
        // Project XML. Other task-0 rows are real summary/budget assignments.
        if task_uid == 0 && resource_uid == 0 {
            continue;
        }
        if uid > i32::MAX as u32 {
            return Err(format!("assignment UID {uid} is out of range"));
        }
        if !task_uids.contains(&task_uid) {
            return Err(format!(
                "assignment UID {uid} refers to unknown task UID {task_uid}"
            ));
        }
        if resource_uid != -65535 && !resource_uids.contains(&resource_uid) {
            return Err(format!(
                "assignment UID {uid} refers to unknown resource UID {resource_uid}"
            ));
        }
        // a0-work and a2-material-cost-unassigned: the 110-byte fixed row
        // carries UID/task/resource at +0/+4/+8, units at +12 (1/10000),
        // work at +20 (1/1000 minute), and Start/Finish at +52/+56.
        let raw_units = f64_at(row, 12) / 10000.0;
        if !raw_units.is_finite() || raw_units < 0.0 {
            return Err(format!("invalid units for assignment UID {uid}"));
        }
        let resource = resources.iter().find(|r| r.uid == resource_uid);
        // Project omits Units on cost assignments; MSPDI's default is 1.
        let units = if resource.is_some_and(|r| r.kind == ResourceType::Cost) {
            1.0
        } else {
            raw_units
        };
        let start = date(&row[52..56], uid, "start")?;
        let finish = date(&row[56..60], uid, "finish")?;
        let mut baselines = Vec::new();
        for slot in 0..=10u8 {
            // a1-slots-progress pins the slot-1 and slot-10 keys. Intervening
            // slots advance by nine keys: Work/Cost and Start/Finish.
            let (wk, ck, sk, fk) = if slot == 0 {
                (0x10, 0x20, 0x92, 0x93)
            } else {
                let delta = 9 * (u16::from(slot) - 1);
                (0x121 + delta, 0x122 + delta, 0x127 + delta, 0x128 + delta)
            };
            let field = |key| table.field(uid, key);
            let numeric = |key: u16, what: &str| -> Result<Option<f64>, String> {
                field(key)
                    .map(|b| {
                        if b.len() != 8 {
                            return Err(format!(
                                "invalid baseline {what} length for assignment UID {uid}"
                            ));
                        }
                        Ok(f64_at(b, 0))
                    })
                    .transpose()
            };
            let s = field(sk)
                .map(|b| date(b, uid, "baseline start"))
                .transpose()?
                .flatten();
            let f = field(fk)
                .map(|b| date(b, uid, "baseline finish"))
                .transpose()?
                .flatten();
            let w = numeric(wk, "work")?;
            let c = numeric(ck, "cost")?;
            if s.is_none() && f.is_none() && w.unwrap_or(0.0) == 0.0 && c.unwrap_or(0.0) == 0.0 {
                continue;
            }
            if s.zip(f).is_some_and(|(s, f)| s > f) {
                return Err(format!("baseline dates reversed for assignment UID {uid}"));
            }
            let work_min = if resource.is_some_and(|r| r.kind == ResourceType::Cost) {
                None
            } else {
                w.filter(|&v| v != 0.0)
                    .map(|v| work(v, uid, "baseline work"))
                    .transpose()?
            };
            let cost = c
                .filter(|&v| v != 0.0)
                .map(|v| money(v, uid, "baseline cost"))
                .transpose()?;
            baselines.push(AssignmentBaseline {
                number: slot,
                start: s,
                finish: f,
                work_min,
                cost,
                bcws: None,
                bcwp: None,
            });
        }
        out.push(Assignment {
            uid: uid as i32,
            task_uid,
            resource_uid,
            units,
            work_min: work(f64_at(row, 20), uid, "work")?,
            start,
            finish,
            baselines,
            ..Assignment::default()
        });
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfb::{Node, write_cfb_tree};

    fn file_with_count(
        mut rows: Vec<[u8; 110]>,
        vars: Vec<(u32, u16, Vec<u8>)>,
        declared: Option<u32>,
    ) -> Vec<u8> {
        let mut fm = vec![0u8; 16];
        fm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        fm[8..12].copy_from_slice(&declared.unwrap_or(rows.len() as u32).to_le_bytes());
        let mut fd = Vec::new();
        for (i, row) in rows.drain(..).enumerate() {
            let mut meta = [0u8; 34];
            meta[4..8].copy_from_slice(&((i * 110) as u32).to_le_bytes());
            fm.extend(meta);
            fd.extend(row);
        }
        let mut vm = vec![0u8; 24];
        vm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        vm[8..12].copy_from_slice(&(vars.len() as u32).to_le_bytes());
        let mut v2 = Vec::new();
        for (uid, key, value) in vars {
            vm.extend(uid.to_le_bytes());
            vm.extend((v2.len() as u32).to_le_bytes());
            vm.extend(key.to_le_bytes());
            vm.extend(0x0f40u16.to_le_bytes());
            v2.extend((value.len() as u32).to_le_bytes());
            v2.extend(value);
        }
        vm[20..24].copy_from_slice(&(v2.len() as u32).to_le_bytes());
        write_cfb_tree(&[Node::Storage(
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
        )])
    }
    fn file(rows: Vec<[u8; 110]>, vars: Vec<(u32, u16, Vec<u8>)>) -> Vec<u8> {
        file_with_count(rows, vars, None)
    }
    fn rows() -> Vec<[u8; 110]> {
        let mut first = [0u8; 110];
        first[..4].copy_from_slice(&7u32.to_le_bytes());
        first[4..8].copy_from_slice(&1i32.to_le_bytes());
        first[8..12].copy_from_slice(&1i32.to_le_bytes());
        first[12..20].copy_from_slice(&10000f64.to_le_bytes());
        first[20..28].copy_from_slice(&960000f64.to_le_bytes());
        first[52..56].copy_from_slice(&[0xc0, 0x12, 0x2a, 0x3c]);
        first[56..60].copy_from_slice(&[0xd8, 0x27, 0x2b, 0x3c]);
        let mut second = first;
        second[..4].copy_from_slice(&8u32.to_le_bytes());
        second[4..8].copy_from_slice(&2i32.to_le_bytes());
        second[8..12].copy_from_slice(&(-65535i32).to_le_bytes());
        vec![first, second]
    }
    fn vars() -> Vec<(u32, u16, Vec<u8>)> {
        let mut result = Vec::new();
        for (slot, w, c) in [
            (0, 960000f64, 80000f64),
            (1, 1440000f64, 120000f64),
            (10, 1920000f64, 160000f64),
        ] {
            let (wk, ck, sk, fk) = if slot == 0 {
                (0x10, 0x20, 0x92, 0x93)
            } else {
                let delta = (slot - 1) * 9;
                (0x121 + delta, 0x122 + delta, 0x127 + delta, 0x128 + delta)
            };
            result.extend([
                (7, wk, w.to_le_bytes().to_vec()),
                (7, ck, c.to_le_bytes().to_vec()),
                (7, sk, vec![0xc0, 0x12, 0x2a, 0x3c]),
                (7, fk, vec![0xd8, 0x27, 0x2b, 0x3c]),
            ]);
        }
        result
    }
    fn resources() -> Vec<Resource> {
        vec![Resource {
            uid: 1,
            kind: ResourceType::Work,
            ..Resource::default()
        }]
    }
    fn tasks() -> HashSet<i32> {
        [1, 2].into_iter().collect()
    }
    #[test]
    fn decodes_all_stored_baseline_slots_and_empty_assignment() {
        let result = decode(&file(rows(), vars()), false, &tasks(), &resources())
            .unwrap()
            .unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(
            result[0]
                .baselines
                .iter()
                .map(|b| (
                    b.number,
                    b.work_min,
                    b.cost.as_ref().unwrap().to_f64().unwrap()
                ))
                .collect::<Vec<_>>(),
            vec![
                (0, Some(960), 80000.0),
                (1, Some(1440), 120000.0),
                (10, Some(1920), 160000.0)
            ]
        );
        assert!(result[0].baselines.iter().all(|b| b.start.is_some()
            && b.finish.is_some()
            && b.bcws.is_none()
            && b.bcwp.is_none()));
        assert!(result[1].baselines.is_empty());
    }
    #[test]
    fn refuses_bad_indexes_references_and_baseline_payloads() {
        let good = file(rows(), vars());
        assert_eq!(
            decode(&good, false, &tasks(), &resources())
                .unwrap()
                .unwrap()
                .len(),
            2
        );
        let mut bad = rows();
        bad[1][..4].copy_from_slice(&7u32.to_le_bytes());
        assert!(
            decode(&file(bad, vars()), false, &tasks(), &resources())
                .unwrap_err()
                .contains("duplicate")
        );
        assert!(
            decode(&good, false, &[2].into_iter().collect(), &resources())
                .unwrap_err()
                .contains("unknown task")
        );
        assert!(
            decode(&good, false, &tasks(), &[])
                .unwrap_err()
                .contains("unknown resource")
        );
        let mut duplicate = vars();
        duplicate.push(duplicate[0].clone());
        assert!(
            decode(&file(rows(), duplicate), false, &tasks(), &resources())
                .unwrap_err()
                .contains("duplicate VarMeta")
        );
        let mut short = vars();
        short[0].2 = vec![1];
        assert!(
            decode(&file(rows(), short), false, &tasks(), &resources())
                .unwrap_err()
                .contains("length")
        );
        let mut date = vars();
        date[2].2 = vec![0x40, 0x38, 0x2a, 0x3c];
        assert!(
            decode(&file(rows(), date), false, &tasks(), &resources())
                .unwrap_err()
                .contains("time")
        );
        let mut midnight = vars();
        midnight[2].2 = vec![0xff, 0xff, 0x2a, 0x3c];
        assert!(decode(&file(rows(), midnight), false, &tasks(), &resources()).is_ok());
        assert!(
            decode(
                &file_with_count(rows(), vars(), Some(3)),
                false,
                &tasks(),
                &resources()
            )
            .unwrap_err()
            .contains("count")
        );
    }

    #[test]
    fn project_deletion_probe_skips_superseded_rows() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../corpus/mpp/assnbaseline/a3-deleted-rows.mpp");
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let resources = crate::rscdecode::decode(&bytes, false).unwrap().unwrap();
        assert_eq!(
            resources
                .iter()
                .map(|r| (r.uid, r.id, r.name.as_str()))
                .collect::<Vec<_>>(),
            vec![(1, 1, "Keeper"), (3, 2, "")]
        );
        let assignments = decode(&bytes, false, &[1].into_iter().collect(), &resources)
            .unwrap()
            .unwrap();
        assert_eq!(
            assignments.iter().map(|a| a.uid).collect::<Vec<_>>(),
            vec![3]
        );
    }

    #[test]
    fn accepts_signed_cost_large_units_and_summary_assignment() {
        let mut records = rows();
        records[0][4..8].copy_from_slice(&0i32.to_le_bytes());
        records[0][12..20].copy_from_slice(&20_000_000f64.to_le_bytes());
        let mut fields = vars();
        fields[1].2 = (-25f64).to_le_bytes().to_vec();
        let decoded = decode(
            &file(records[..1].to_vec(), fields),
            false,
            &[0].into_iter().collect(),
            &resources(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].task_uid, 0);
        assert_eq!(decoded[0].units, 2000.0);
        assert_eq!(
            decoded[0].baselines[0].cost.as_ref().unwrap().to_f64(),
            Some(-25.0)
        );
    }
}
