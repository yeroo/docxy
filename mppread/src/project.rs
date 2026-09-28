//! Convert validated MPP metadata, tasks, resources and assignments to a project.

use projcore::editor::default_anchor;
use projcore::{ConstraintType, DateTime, LagFormat, LinkType, Predecessor, Project, Task};

/// Build a project from a structurally recognized `.mpp` task table. Task UID 0
/// is its project summary and supplies a fallback name, but is not imported as
/// a task. Each decoded auto leaf is pinned
/// with a Must-Start-On constraint at its start and given Project's stored
/// working duration when available. Other durations use the working minutes
/// between start and finish on the task's calendar. Current Project resource
/// identity/type and assignment identity, planned work and dates are imported,
/// together with saved baseline slots 0..10 (Start, Finish, Work, Cost).
/// Project's MPP/XML has no assignment-baseline BCWS/BCWP; those remain absent.
/// Assignment progress, rate tables, contours and delays are not decoded.
/// Splits and delayed assignments can therefore schedule an earlier finish
/// than their retained stored finish. A resource calendar can move a resourced
/// task's finish earlier or later because projcore does not schedule on resource
/// calendars; MSPDI import of the same plan behaves the same way. A **manual**
/// leaf keeps its mode and its manual start,
/// finish and duration instead, which hold it where Project put it without a
/// constraint; the project's new-task mode comes through too.
/// The **outline levels** (WBS depth) decode too, so summary tasks and their
/// rollup come through, and the **predecessor links** decode from the `TBkndCons`
/// table. Each task's recorded **progress**, work and cost come through as
/// read (the scheduler ignores them, as it does for MSPDI); Project's
/// variances are not stored in the file and stay absent. Save As converts it
/// to `.yppx`/MSPDI. Current Project calendar tables keep base and derived
/// calendars, their weekdays and exceptions, and the project's default calendar.
/// Task calendar assignments and alternate work weeks are decoded. Auto
/// tasks use their stored working Duration when available; manual tasks keep
/// their manual duration and use the calendar span only when it is absent.
/// An unrecognised calendar record refuses the import.
/// A malformed current assignment/resource table refuses import. Files with
/// no such table, or a present unvalidated layout, retain task-only import.
/// A calendar whose default week is wholly closed is refused even if one of
/// its alternate weeks opens a day, as in MSPDI import.
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
    let decoded_calendars = crate::caldecode::decode(bytes, legacy)
        .map_err(|e| format!("cannot read the calendars of this .mpp ({e})"))?;
    let has_calendar_table = decoded_calendars.is_some();
    if let Some(crate::caldecode::DecodedCalendars {
        calendars,
        default_calendar_uid,
    }) = decoded_calendars
    {
        cal_ref.calendars = calendars;
        cal_ref.default_calendar_uid = default_calendar_uid;
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
            let own_calendar_uid = t
                .calendar_uid
                .filter(|&uid| uid != -1 && has_calendar_table);
            if let Some(uid) = own_calendar_uid {
                if cal_ref.calendar(uid).is_none() {
                    return Err(format!("unknown calendar UID {uid} for task UID {}", t.uid));
                }
            }
            task.calendar_uid = own_calendar_uid;
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
            let span = || {
                projcore::schedule::working_minutes_between_on(&cal_ref, task.calendar_uid, s, f)
            };
            if is_summary {
                task.duration_min = 0;
            } else if t.manual {
                task.duration_min = t.manual_duration_min.unwrap_or_else(span);
            } else {
                task.duration_min = t
                    .duration_format
                    .filter(|&fmt| crate::mpp::working_duration_format(fmt))
                    .and(t.duration_min)
                    .unwrap_or_else(span);
                task.constraint = ConstraintType::MustStartOn;
                task.constraint_date = Some(s);
            }
            Ok(task)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let resources_present = crate::tabledecode::present(bytes, "TBkndRsc")?;
    let assignments_present = crate::tabledecode::present(bytes, "TBkndAssn")?;
    if !legacy && assignments_present && !resources_present {
        return Err("cannot read the assignments of this .mpp (missing resource table)".into());
    }
    let decoded_resources = crate::rscdecode::decode(bytes, legacy)
        .map_err(|e| format!("cannot read the resources of this .mpp ({e})"))?;
    let mut task_uids: std::collections::HashSet<i32> = tasks.iter().map(|t| t.uid).collect();
    task_uids.insert(0); // Project summary assignments can reference UID 0.
    let (resources, assignments) = if let Some(resources) = decoded_resources {
        let decoded_assignments = crate::assndecode::decode(bytes, legacy, &task_uids, &resources)
            .map_err(|e| format!("cannot read the assignments of this .mpp ({e})"))?;
        if assignments_present && decoded_assignments.is_none() {
            (Vec::new(), Vec::new()) // Unsupported assignment layout: keep task-only import.
        } else {
            (resources, decoded_assignments.unwrap_or_default())
        }
    } else {
        // No resource table, or one whose layout has not been validated.
        (Vec::new(), Vec::new())
    };
    let start = tasks
        .iter()
        .filter_map(|t| t.stored_start)
        .min()
        .unwrap_or_else(default_anchor);
    let project = Project {
        name,
        title: info.title,
        start_date: Some(start),
        tasks,
        resources,
        assignments,
        new_tasks_are_manual,
        ..cal_ref
    };
    if let Some(error) = projcore::schedule::calendar_error(&project) {
        return Err(format!("cannot read the calendars of this .mpp ({error})"));
    }
    Ok(project)
}

/// Parse an `mppread`-decoded `YYYY-MM-DD HH:MM` timestamp into a `DateTime`.
pub(crate) fn parse_mpp_dt(s: &str) -> Option<DateTime> {
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
    use crate::cfb::{Node, write_cfb_tree};

    #[test]
    fn unsupported_resource_layout_keeps_task_only_import() {
        let mut rsc_meta = vec![0u8; 16 + 38];
        rsc_meta[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        rsc_meta[8..12].copy_from_slice(&1u32.to_le_bytes());
        let mut assn_meta = vec![0u8; 16 + 34];
        assn_meta[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        assn_meta[8..12].copy_from_slice(&1u32.to_le_bytes());
        let mut assn_data = vec![0u8; 110];
        assn_data[..4].copy_from_slice(&2u32.to_le_bytes());
        assn_data[8..12].copy_from_slice(&1i32.to_le_bytes());
        let mut vm = vec![0u8; 24];
        vm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        let table = |name, data, meta| {
            Node::Storage(
                name,
                vec![
                    Node::Stream("FixedMeta", meta),
                    Node::Stream("FixedData", data),
                    Node::Stream("VarMeta", vm.clone()),
                    Node::Stream("Var2Data", Vec::new()),
                ],
            )
        };
        let mpp = write_cfb_tree(&[Node::Storage(
            "   114",
            vec![
                table("TBkndRsc", vec![0u8; 110], rsc_meta),
                table("TBkndAssn", assn_data, assn_meta),
            ],
        )]);
        let project = project_from_mpp(&mpp).unwrap();
        assert!(
            project.tasks.is_empty()
                && project.resources.is_empty()
                && project.assignments.is_empty()
        );
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
