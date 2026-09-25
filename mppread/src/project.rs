//! Convert validated MPP metadata and tasks to a schedulable project.

use projcore::editor::default_anchor;
use projcore::{ConstraintType, DateTime, LinkType, Predecessor, Project, Task};

/// Build a project from a structurally recognized `.mpp` task table. Task UID 0
/// is its project summary and supplies a fallback name, but is not imported as
/// a task. Each decoded auto leaf is pinned
/// with a Must-Start-On constraint at its start and given a duration equal to
/// the working minutes between its start and finish, so the scheduler reproduces
/// the real dates. A **manual** leaf keeps its mode and its manual start,
/// finish and duration instead, which hold it where Project put it without a
/// constraint; the project's new-task mode comes through too.
/// The **outline levels** (WBS depth) decode too, so summary tasks and their
/// rollup come through, and the **predecessor links** decode from the `TBkndCons`
/// table. Save As converts it to `.yppx`/MSPDI.
pub fn project_from_mpp(bytes: &[u8]) -> Result<Project, String> {
    let info = crate::read_mpp(bytes)?;
    let decoded = crate::mpp::decode_tasks(bytes)
        .map_err(|e| format!("cannot read the task table of this .mpp ({e})"))?;
    let new_tasks_are_manual = crate::mpp::decode_new_tasks_are_manual(bytes)
        .map_err(|e| format!("cannot read the project options of this .mpp ({e})"))?;
    let name = [
        info.title.clone(),
        info.subject.clone(),
        info.company.clone(),
    ]
    .into_iter()
    .find(|s| !s.is_empty())
    .unwrap_or_else(|| {
        decoded
            .iter()
            .find(|t| t.uid == 0)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| "Imported project".into())
    });
    let cal_ref = Project::default();
    let decoded: Vec<_> = decoded.into_iter().filter(|t| t.uid != 0).collect();
    // A task is a summary when the next task sits one WBS level deeper.
    let levels: Vec<u32> = decoded
        .iter()
        .map(|t| t.outline_level.expect("validated task outline"))
        .collect();
    let tasks: Vec<Task> = decoded
        .iter()
        .enumerate()
        .map(|(i, t)| -> Result<Task, String> {
            let is_summary = levels.get(i + 1).is_some_and(|&nxt| nxt > levels[i]);
            let mut task = Task {
                uid: t.uid as i32,
                id: t.id as i32,
                name: t.name.clone(),
                outline_level: levels[i],
                summary: is_summary,
                duration_min: 480,
                ..Task::default()
            };
            // Binary links identify predecessor tasks by their stable UID.
            task.predecessors = t
                .predecessors
                .iter()
                .map(|p| {
                    Ok(Predecessor {
                        uid: p.pred_uid as i32,
                        link: LinkType::from_code(p.kind as i64).ok_or_else(|| {
                            format!("unsupported link type {} for UID {}", p.kind, t.uid)
                        })?,
                        lag_min: p.lag_min,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let s = t
                .start
                .as_deref()
                .and_then(parse_mpp_dt)
                .ok_or_else(|| format!("invalid start date for UID {}", t.uid))?;
            let f = t
                .finish
                .as_deref()
                .and_then(parse_mpp_dt)
                .ok_or_else(|| format!("invalid finish date for UID {}", t.uid))?;
            task.stored_start = Some(s);
            task.stored_finish = Some(f);
            task.manual = t.manual;
            let manual_date = |d: &Option<String>, what: &str| {
                d.as_deref()
                    .map(|d| {
                        parse_mpp_dt(d)
                            .ok_or_else(|| format!("invalid manual {what} for UID {}", t.uid))
                    })
                    .transpose()
            };
            task.manual_start = manual_date(&t.manual_start, "start")?;
            task.manual_finish = manual_date(&t.manual_finish, "finish")?;
            task.manual_duration_min = t.manual_duration_min;
            // Pin only leaf tasks; a summary's dates roll up from its
            // children, so a constraint on it would fight the rollup. A
            // manual leaf is held by its pinned dates, not a constraint.
            if is_summary {
                task.duration_min = 0;
            } else if t.manual {
                task.duration_min = t
                    .manual_duration_min
                    .unwrap_or_else(|| projcore::schedule::working_minutes_between(&cal_ref, s, f));
            } else {
                task.duration_min = projcore::schedule::working_minutes_between(&cal_ref, s, f);
                task.constraint = ConstraintType::MustStartOn;
                task.constraint_date = Some(s);
            }
            Ok(task)
        })
        .collect::<Result<Vec<_>, _>>()?;
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
        new_tasks_are_manual,
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
