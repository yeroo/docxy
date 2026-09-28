//! Convert validated MPP metadata and tasks to a schedulable project.

use projcore::editor::default_anchor;
use projcore::{
    ConstraintType, DateTime, DayWorking, LagFormat, LinkType, Predecessor, Project, Task,
};
use std::collections::HashSet;

/// Build a project from a structurally recognized `.mpp` task table. Task UID 0
/// is its project summary and supplies a fallback name, but is not imported as
/// a task. Each decoded auto leaf is pinned with a Must-Start-On constraint at
/// its start. When its effective calendar has weekly working time and no
/// undecoded work weeks in its base chain, it uses Project's stored duration
/// and its own calendar where one is assigned. Otherwise it keeps the working
/// span on the project calendar and no task calendar. An unknown task calendar
/// UID keeps the project-calendar span; invalid negative UIDs, stored
/// durations and duration formats refuse the file. Delayed assignments, splits,
/// resource calendars (their weeks, hours, work weeks and exceptions; #220)
/// and recurring exceptions the scheduler cannot expand can still make
/// scheduled and stored finishes differ. A **manual**
/// leaf keeps its mode and manual dates and duration instead, which hold it
/// where Project put it without a constraint; the project's new-task mode
/// comes through too.
/// The **outline levels** (WBS depth) decode too, so summary tasks and their
/// rollup come through, and the **predecessor links** decode from the `TBkndCons`
/// table. Each task's recorded **progress**, work and cost come through as
/// read (the scheduler ignores them, as it does for MSPDI); Project's
/// variances are not stored in the file and stay absent. Save As converts it
/// to `.yppx`/MSPDI. Current Project calendar tables keep base and derived
/// calendars, their weekdays and exceptions, and the project's default calendar.
/// Work weeks are not yet decoded; their presence suppresses stored-duration
/// scheduling on that calendar. An unrecognised exception record refuses import.
pub fn project_from_mpp(bytes: &[u8]) -> Result<Project, String> {
    let info = crate::read_mpp(bytes)?;
    let table = crate::taskdecode::decode_table(bytes)
        .map_err(|e| format!("cannot read the task table of this .mpp ({e})"))?;
    let legacy = table.legacy;
    let new_tasks_are_manual = table
        .new_tasks_are_manual
        .map_err(|e| format!("cannot read the project options of this .mpp ({e})"))?;
    let decoded = table.tasks;
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
    let mut cal_ref = Project::default();
    let mut work_week_uids = HashSet::new();
    if let Some(decoded_calendars) = crate::caldecode::decode(bytes, legacy)
        .map_err(|e| format!("cannot read the calendars of this .mpp ({e})"))?
    {
        cal_ref.calendars = decoded_calendars.calendars;
        cal_ref.default_calendar_uid = decoded_calendars.default_uid;
        work_week_uids = decoded_calendars.work_week_uids;
    }
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
                        lag: p.lag,
                        lag_format: LagFormat::from_code(i64::from(p.lag_format)).ok_or_else(
                            || format!("unsupported LagFormat {} for UID {}", p.lag_format, t.uid),
                        )?,
                        ..Predecessor::fs(p.pred_uid as i32)
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
            let own_calendar = t
                .calendar_uid
                .filter(|&uid| cal_ref.calendar(uid).is_some());
            let effective_uid = own_calendar.unwrap_or(cal_ref.default_calendar_uid);
            let effective_cal = cal_ref
                .calendar(effective_uid)
                .expect("validated or synthesized project calendar");
            let faithful = (t.calendar_uid.is_none() || own_calendar.is_some())
                && cal_ref
                    .resolved_week(effective_cal)
                    .iter()
                    .any(DayWorking::working)
                && !calendar_chain_has_work_weeks(&cal_ref, effective_uid, &work_week_uids);
            task.calendar_uid = if faithful { own_calendar } else { None };
            let project_span = || projcore::schedule::working_minutes_between(&cal_ref, s, f);
            let working_span = || {
                if faithful {
                    let cal = cal_ref.resolved_calendar(effective_cal);
                    projcore::schedule::working_minutes_on(&cal, s, f)
                } else {
                    project_span()
                }
            };
            task.manual = t.manual;
            let date = |d: &Option<String>, what: &str| {
                d.as_deref()
                    .map(|d| {
                        parse_mpp_dt(d).ok_or_else(|| format!("invalid {what} for UID {}", t.uid))
                    })
                    .transpose()
            };
            task.manual_start = date(&t.manual_start, "manual start")?;
            task.manual_finish = date(&t.manual_finish, "manual finish")?;
            task.manual_duration_min = t.manual_duration_min;
            if let Some(p) = &t.progress {
                task.percent_complete = Some(p.percent_complete);
                task.percent_work_complete = Some(p.percent_work_complete);
                task.physical_percent_complete = Some(p.physical_percent_complete);
                task.actual_start = date(&p.actual_start, "actual start")?;
                task.actual_finish = date(&p.actual_finish, "actual finish")?;
                task.stop = date(&p.stop, "stop")?;
                task.resume = date(&p.resume, "resume")?;
                task.actual_duration_min = Some(p.actual_duration_min);
                task.remaining_duration_min = Some(p.remaining_duration_min);
                task.work_min = Some(p.work_min);
                task.actual_work_min = Some(p.actual_work_min);
                task.remaining_work_min = Some(p.remaining_work_min);
                task.cost = Some(p.cost.clone());
                task.actual_cost = Some(p.actual_cost.clone());
                task.remaining_cost = Some(p.remaining_cost.clone());
            }
            // Pin only leaf tasks; a summary's dates roll up from its
            // children, so a constraint on it would fight the rollup. A
            // manual leaf is held by its pinned dates, not a constraint.
            if is_summary {
                task.duration_min = 0;
            } else if t.manual {
                task.duration_min = t.manual_duration_min.unwrap_or_else(working_span);
            } else {
                task.duration_min = t
                    .duration_min
                    .filter(|_| faithful)
                    .unwrap_or_else(working_span);
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
        ..cal_ref
    })
}

fn calendar_chain_has_work_weeks(
    project: &Project,
    uid: i32,
    work_week_uids: &HashSet<i32>,
) -> bool {
    let mut next = Some(uid);
    while let Some(uid) = next {
        if work_week_uids.contains(&uid) {
            return true;
        }
        next = project.calendar(uid).and_then(|cal| cal.base_calendar_uid);
    }
    false
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
    fn undecoded_base_work_week_marks_a_derived_calendar_unfaithful() {
        let mut project = Project::default();
        let mut derived = projcore::Calendar::standard(3);
        derived.base_calendar_uid = Some(1);
        project.calendars.push(derived);
        let mut work_week_uids = HashSet::new();
        work_week_uids.insert(1);
        assert!(calendar_chain_has_work_weeks(&project, 3, &work_week_uids));
        work_week_uids.clear();
        assert!(!calendar_chain_has_work_weeks(&project, 3, &work_week_uids));
    }
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
