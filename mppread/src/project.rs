//! Convert decoded legacy MPP metadata and tasks to a schedulable project.

use projcore::editor::default_anchor;
use projcore::{ConstraintType, DateTime, LinkType, Predecessor, Project, Task};

/// Build a partial project from a legacy binary `.mpp`. Decodes the documented
/// metadata (title/author/…), the **task names** (from the VarMeta/Var2Data
/// container), and — when the fixed-record layout is recognized — each task's
/// **start/finish** dates from the FixedData records. A decoded task is pinned
/// with a Must-Start-On constraint at its start and given a duration equal to
/// the working minutes between its start and finish, so the scheduler reproduces
/// the real dates; tasks without decoded dates keep a default 1-day duration.
/// The **outline levels** (WBS depth) decode too, so summary tasks and their
/// rollup come through, and the **predecessor links** decode from the `TBkndCons`
/// table. Save As converts it to `.yppx`/MSPDI.
pub fn project_from_mpp(bytes: &[u8]) -> Result<Project, String> {
    let info = crate::read_mpp(bytes)?;
    let name = [
        info.title.clone(),
        info.subject.clone(),
        info.company.clone(),
    ]
    .into_iter()
    .find(|s| !s.is_empty())
    .unwrap_or_else(|| "Imported project".into());
    let cal_ref = Project::default();
    let decoded = crate::mpp::tasks(bytes);
    // A task is a summary when the next task sits one WBS level deeper.
    let levels: Vec<u32> = decoded
        .iter()
        .map(|t| t.outline_level.unwrap_or(1))
        .collect();
    let tasks: Vec<Task> = decoded
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let is_summary = levels.get(i + 1).is_some_and(|&nxt| nxt > levels[i]);
            let mut task = Task {
                uid: i as i32 + 1,
                id: i as i32 + 1,
                name: t.name.clone(),
                outline_level: levels[i],
                summary: is_summary,
                duration_min: 480,
                ..Task::default()
            };
            // Predecessor links (indices → uids); lag isn't decoded yet.
            task.predecessors = t
                .predecessors
                .iter()
                .filter_map(|p| {
                    Some(Predecessor {
                        uid: p.pred as i32 + 1,
                        link: LinkType::from_code(p.kind as i64)?,
                        lag_min: 0,
                    })
                })
                .collect();
            let s = t.start.as_deref().and_then(parse_mpp_dt);
            let f = t.finish.as_deref().and_then(parse_mpp_dt);
            if let (Some(s), Some(f)) = (s, f) {
                task.stored_start = Some(s);
                task.stored_finish = Some(f);
                // Pin only leaf tasks; a summary's dates roll up from its
                // children, so a constraint on it would fight the rollup. The
                // Must-Start-On keeps dates exact; the decoded links (validated
                // against these dates) add the dependency structure.
                if is_summary {
                    task.duration_min = 0;
                } else {
                    task.duration_min = projcore::schedule::working_minutes_between(&cal_ref, s, f);
                    task.constraint = ConstraintType::MustStartOn;
                    task.constraint_date = Some(s);
                }
            } else if is_summary {
                task.duration_min = 0;
            }
            task
        })
        .collect();
    let start = tasks
        .iter()
        .filter_map(|t| t.stored_start)
        .min()
        .unwrap_or_else(default_anchor);
    Ok(Project {
        name,
        title: info.title,
        start_date: Some(start),
        tasks,
        ..Project::default()
    })
}

/// Parse an `mppread`-decoded `YYYY-MM-DD HH:MM` timestamp into a `DateTime`.
fn parse_mpp_dt(s: &str) -> Option<DateTime> {
    let (date, time) = s.split_once(' ')?;
    let mut d = date.split('-');
    let (y, mo, da) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let (hh, mm) = time.split_once(':')?;
    Some(DateTime::from_ymd_hm(
        y,
        mo,
        da,
        hh.parse().ok()?,
        mm.parse().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opens_mpp_metadata_as_partial_project() {
        // Build a minimal .mpp: a SummaryInformation property set with a title,
        // plus a stub task-data stream, wrapped in the CFB container.
        let title = "Bridge Retrofit";
        let mut sval: Vec<u8> = title.bytes().collect();
        sval.push(0);
        while !sval.len().is_multiple_of(4) {
            sval.push(0);
        }
        let mut values = Vec::new();
        values.extend_from_slice(&30u32.to_le_bytes()); // VT_LPSTR
        values.extend_from_slice(&((title.len() + 1) as u32).to_le_bytes());
        values.extend_from_slice(&sval);
        let mut index = Vec::new();
        index.extend_from_slice(&2u32.to_le_bytes()); // PID_TITLE
        index.extend_from_slice(&16u32.to_le_bytes()); // value offset within section
        let cb = 8 + index.len() + values.len();
        let mut sec = Vec::new();
        sec.extend_from_slice(&(cb as u32).to_le_bytes());
        sec.extend_from_slice(&1u32.to_le_bytes());
        sec.extend_from_slice(&index);
        sec.extend_from_slice(&values);
        let mut summary = Vec::new();
        summary.extend_from_slice(&0xFFFEu16.to_le_bytes());
        summary.extend_from_slice(&0u16.to_le_bytes());
        summary.extend_from_slice(&0u32.to_le_bytes());
        summary.extend_from_slice(&[0u8; 16]);
        summary.extend_from_slice(&1u32.to_le_bytes());
        summary.extend_from_slice(&[0u8; 16]);
        summary.extend_from_slice(&48u32.to_le_bytes());
        summary.extend_from_slice(&sec);

        let mpp = crate::write_cfb(&[
            ("\u{5}SummaryInformation", summary),
            ("Props", vec![0u8; 12]),
        ]);
        let proj = project_from_mpp(&mpp).unwrap();
        assert_eq!(proj.name, "Bridge Retrofit");
        assert!(proj.tasks.is_empty()); // this stub has no TBkndTask streams to decode
    }
}
