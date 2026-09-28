//! Convert validated MPP metadata, tasks, resources and assignments to a project.

use projcore::editor::default_anchor;
use projcore::{ConstraintType, DateTime, LagFormat, LinkType, Predecessor, Project, Task};

/// Build a project from a structurally recognized `.mpp` task table. Task UID 0
/// is its project summary and supplies a fallback name, but is not imported as
/// a task. Current-layout tasks keep their recorded constraints, including
/// manual tasks and summaries. For an untracked, local automatic leaf whose
/// scheduled start differs from Project's stored start, the importer replaces
/// its constraint with Must-Start-On only if that restores the stored start;
/// Save As writes any accepted pin. Cross-project predecessors, ALAP chains,
/// leveling delay and resource calendars can cause
/// these differences. On the `.mpp` path, an effective pin preserves the
/// stored start, though its finish can still differ. MSPDI import does not
/// add this pin, so both scheduled start and finish can move. Legacy MPP9
/// leaves, including a childless inserted subproject, remain pinned with
/// Must-Start-On at their stored start. Automatic leaves use Project's stored working duration when
/// available. Other durations use the working minutes between start and finish
/// on the task's calendar.
/// Current Project resource identity/type and assignment identity, planned
/// work and dates are imported, together with saved baseline slots 0..10
/// (Start, Finish, Work, Cost). Project's MPP/XML has no assignment-baseline
/// BCWS/BCWP; those remain absent. Assignment progress, rate tables, contours
/// and delays are not decoded. Splits and delayed assignments can therefore
/// schedule an earlier finish than their retained stored finish. Resource
/// calendars and leveling delay can move a task's finish because projcore does
/// not apply either when scheduling. A **manual** leaf keeps its mode, manual start,
/// finish and duration, which hold it where Project put it. Its recorded
/// constraint is retained but does not move its manual dates. The project's
/// new-task mode comes through too.
/// The **outline levels** (WBS depth) decode too, so summary tasks and their
/// rollup come through, and the **predecessor links** decode from the `TBkndCons`
/// table. Each task's recorded **progress**, work and cost come through as
/// read. The scheduler uses actuals for tracked tasks; Project's
/// variances are not stored in the file and stay absent. Validated task fields
/// and blank grid rows come through too. Save As converts it to `.yppx`/MSPDI.
/// Current Project calendar tables keep base and derived calendars, their
/// weekdays and exceptions, and the project's default calendar.
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
    let project_start = table
        .project_start
        .and_then(|value| {
            value
                .map(|d| {
                    parse_mpp_dt(&d).ok_or_else(|| "invalid project StartDate in Props".to_string())
                })
                .transpose()
        })
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
    let tasks = import_tasks(decoded, &cal_ref, has_calendar_table, legacy)?;
    let resources_present = crate::tabledecode::present(bytes, "TBkndRsc")?;
    let assignments_present = crate::tabledecode::present(bytes, "TBkndAssn")?;
    if !legacy && assignments_present && !resources_present {
        return Err("cannot read the assignments of this .mpp (missing resource table)".into());
    }
    let decoded_resources = crate::rscdecode::decode(bytes, legacy)
        .map_err(|e| format!("cannot read the resources of this .mpp ({e})"))?;
    let task_uids = assignment_task_uids(&tasks);
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
    let start = project_start.unwrap_or_else(|| {
        tasks
            .iter()
            .filter_map(|t| t.stored_start)
            .min()
            .unwrap_or_else(default_anchor)
    });
    let mut project = Project {
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
    if !legacy {
        pin_unreproduced_starts(&mut project);
    }
    Ok(project)
}

fn reproduces(schedule: &projcore::schedule::Schedule, task: &Task) -> bool {
    schedule
        .get(task.uid)
        .is_some_and(|result| Some(result.early_start) == task.stored_start)
}

fn pin_unreproduced_starts(project: &mut Project) {
    let candidates: Vec<usize> = project
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            !t.is_null && !t.summary && !t.manual && t.tracked().is_none() && !t.is_external_leaf()
        })
        .map(|(i, _)| i)
        .collect();
    let originals: Vec<_> = project
        .tasks
        .iter()
        .map(|t| (t.constraint, t.constraint_date))
        .collect();
    let mut pinned = std::collections::HashSet::new();
    let mut rejected = std::collections::HashSet::new();
    let mut scheduled = projcore::schedule::schedule(project);
    // Each divergent candidate becomes pinned or rejected. Neither is
    // retried, so there are at most candidate-count passes.
    loop {
        let mut divergent = Vec::new();
        for &i in &candidates {
            if pinned.contains(&i) || rejected.contains(&i) {
                continue;
            }
            let task = &project.tasks[i];
            // A missing schedule row does not enter the divergent set.
            if scheduled.get(task.uid).is_some() && !reproduces(&scheduled, task) {
                divergent.push(i);
            }
        }
        if divergent.is_empty() {
            break;
        }
        for &i in &divergent {
            let task = &mut project.tasks[i];
            task.constraint = ConstraintType::MustStartOn;
            task.constraint_date = task.stored_start;
        }
        let trial = projcore::schedule::schedule(project);
        let mut rejected_this_pass = false;
        for i in divergent {
            let task = &project.tasks[i];
            if reproduces(&trial, task) {
                pinned.insert(i);
            } else {
                let task = &mut project.tasks[i];
                (task.constraint, task.constraint_date) = originals[i];
                rejected.insert(i);
                rejected_this_pass = true;
            }
        }
        if rejected_this_pass && !pinned.is_empty() {
            // Restoring an ineffective pin may unfix another from the batch.
            // Those become rejected too; each cleanup pass shrinks pinned.
            scheduled = projcore::schedule::schedule(project);
            loop {
                let unstable: Vec<_> = pinned
                    .iter()
                    .copied()
                    .filter(|&i| {
                        let task = &project.tasks[i];
                        !reproduces(&scheduled, task)
                    })
                    .collect();
                if unstable.is_empty() {
                    break;
                }
                for i in unstable {
                    let task = &mut project.tasks[i];
                    (task.constraint, task.constraint_date) = originals[i];
                    pinned.remove(&i);
                    rejected.insert(i);
                }
                scheduled = projcore::schedule::schedule(project);
            }
        } else if !rejected_this_pass {
            scheduled = trial;
        }
    }

    // Batch pinning can pin a successor that an upstream pin already fixes.
    // Try removing accepted pins from successors toward predecessors. A pin
    // stays removed only if every start currently reproduced remains so.
    // This costs one full schedule per accepted pin, in addition to at most
    // candidate-count accept/reject passes. The 48 newest snapshot files
    // currently need zero pins, so they incur no minimisation schedules.
    if pinned.is_empty() {
        return;
    }
    let uid_to_index: std::collections::HashMap<_, _> = candidates
        .iter()
        .map(|&i| (project.tasks[i].uid, i))
        .collect();
    let mut order: Vec<usize> = projcore::schedule::effective_leaf_order(project)
        .into_iter()
        .filter_map(|uid| uid_to_index.get(&uid).copied())
        .collect();
    // An unschedulable candidate has no result and cannot be pinned, but keep
    // it in the order so cleanup remains exhaustive.
    let seen: std::collections::HashSet<_> = order.iter().copied().collect();
    order.extend(candidates.iter().copied().filter(|i| !seen.contains(i)));
    for i in order.into_iter().rev() {
        if !pinned.contains(&i) {
            continue;
        }
        let matching: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|&j| reproduces(&scheduled, &project.tasks[j]))
            .collect();
        let task = &mut project.tasks[i];
        (task.constraint, task.constraint_date) = originals[i];
        let trial = projcore::schedule::schedule(project);
        if matching
            .iter()
            .all(|&j| reproduces(&trial, &project.tasks[j]))
        {
            pinned.remove(&i);
            scheduled = trial;
        } else {
            let task = &mut project.tasks[i];
            task.constraint = ConstraintType::MustStartOn;
            task.constraint_date = task.stored_start;
        }
    }
}

fn assignment_task_uids(tasks: &[Task]) -> std::collections::HashSet<i32> {
    let mut uids: std::collections::HashSet<i32> =
        tasks.iter().filter(|t| !t.is_null).map(|t| t.uid).collect();
    uids.insert(0); // Project summary assignments can reference UID 0.
    uids
}

fn import_tasks(
    decoded: Vec<crate::mpp::MppTask>,
    cal_ref: &Project,
    has_calendar_table: bool,
    legacy: bool,
) -> Result<Vec<Task>, String> {
    let decoded: Vec<_> = decoded.into_iter().filter(|t| t.uid != 0).collect();
    // Only a local deeper row can form a schedulable outline summary. Project
    // labels childless inserted subprojects as summaries in XML, but they must
    // remain leaves here so the scheduler includes them.
    let summaries: Vec<bool> = decoded
        .iter()
        .enumerate()
        .map(|(i, t)| {
            !t.is_null
                && decoded[i + 1..]
                    .iter()
                    .find(|next| !next.is_null)
                    .is_some_and(|next| next.outline_level > t.outline_level)
        })
        .collect();
    let tasks: Vec<Task> = decoded
        .iter()
        .enumerate()
        .map(|(i, t)| -> Result<Task, String> {
            if t.is_null {
                return Ok(Task {
                    uid: t.uid as i32,
                    id: t.id as i32,
                    is_null: true,
                    ..Task::default()
                });
            }
            let level = t.outline_level.expect("validated task outline");
            let outline_summary = summaries[i];
            let mut task = Task {
                uid: t.uid as i32,
                id: t.id as i32,
                name: t.name.clone(),
                outline_level: level,
                summary: outline_summary,
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
            if let Some(fields) = &t.fields {
                task.guid = fields.guid.clone();
                task.create_date = date(&fields.create_date, "create date")?;
                task.notes = fields.notes.clone();
                task.wbs = fields.wbs.clone();
                task.task_type = fields.task_type;
                if let Some(constraint) = fields.constraint_type {
                    task.constraint = constraint;
                    task.constraint_date = date(&fields.constraint_date, "constraint date")?;
                }
                task.active = fields.active;
                task.effort_driven = fields.effort_driven;
                task.estimated = fields.estimated;
                task.priority = fields.priority;
                task.deadline = date(&fields.deadline, "deadline")?;
                task.level_assignments = fields.level_assignments;
                task.leveling_can_split = fields.leveling_can_split;
                task.leveling_delay = fields.leveling_delay;
                task.leveling_delay_format = fields.leveling_delay_format;
                task.ignore_resource_calendar = fields.ignore_resource_calendar;
                task.earned_value_method = fields.earned_value_method;
                task.recurring = fields.recurring;
                task.hide_bar = fields.hide_bar;
                task.rollup = fields.rollup;
                task.external_task = fields.external_task;
                task.is_subproject = fields.is_subproject;
                task.is_subproject_read_only = fields.is_subproject_read_only;
                task.over_allocated = fields.over_allocated;
                task.milestone = fields.milestone.unwrap_or(false);
            }
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
            // Outline summaries roll up; manual leaves use their own dates.
            let span =
                || projcore::schedule::working_minutes_between_on(cal_ref, task.calendar_uid, s, f);
            if outline_summary {
                task.duration_min = 0;
            } else if t.manual {
                task.duration_min = t.manual_duration_min.unwrap_or_else(span);
            } else {
                task.duration_min = t
                    .duration_format
                    .filter(|&fmt| crate::mpp::working_duration_format(fmt))
                    .and(t.duration_min)
                    .unwrap_or_else(span);
                if legacy {
                    task.constraint = ConstraintType::MustStartOn;
                    task.constraint_date = Some(s);
                }
            }
            Ok(task)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(tasks)
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

    fn import_tasks(decoded: Vec<crate::mpp::MppTask>) -> Result<Vec<Task>, String> {
        super::import_tasks(decoded, &Project::default(), false, true)
    }

    fn import_current(decoded: Vec<crate::mpp::MppTask>) -> Result<Vec<Task>, String> {
        let tasks = super::import_tasks(decoded, &Project::default(), false, false)?;
        let start_date = tasks.iter().filter_map(|t| t.stored_start).min();
        let mut project = Project {
            start_date,
            tasks,
            ..Project::default()
        };
        super::pin_unreproduced_starts(&mut project);
        Ok(project.tasks)
    }

    fn task(id: u32, uid: u32, name: &str, level: u32) -> crate::mpp::MppTask {
        crate::mpp::MppTask {
            id,
            uid,
            name: name.into(),
            start: Some("2026-03-02 08:00".into()),
            finish: Some("2026-03-03 08:00".into()),
            outline_level: Some(level),
            ..crate::mpp::MppTask::default()
        }
    }

    fn blank(id: u32, uid: u32) -> crate::mpp::MppTask {
        crate::mpp::MppTask {
            id,
            uid,
            is_null: true,
            ..crate::mpp::MppTask::default()
        }
    }

    fn current(id: u32, uid: u32, name: &str, level: u32) -> crate::mpp::MppTask {
        let mut row = task(id, uid, name, level);
        row.fields = Some(crate::mpp::MppTaskFields {
            constraint_type: Some(ConstraintType::AsSoonAsPossible),
            ..crate::mpp::MppTaskFields::default()
        });
        row
    }
    fn later(mut row: crate::mpp::MppTask, start: &str, finish: &str) -> crate::mpp::MppTask {
        row.start = Some(start.into());
        row.finish = Some(finish.into());
        row
    }

    fn link(pred_uid: u32) -> crate::mpp::MppPred {
        crate::mpp::MppPred {
            pred_uid,
            kind: 1,
            lag_format: 8,
            ..Default::default()
        }
    }

    #[test]
    fn alap_predecessor_of_a_summary_is_pinned_but_asap_is_not() {
        let mut late = current(1, 1, "ALAP predecessor", 1);
        late.fields.as_mut().unwrap().constraint_type = Some(ConstraintType::AsLateAsPossible);
        let early = current(2, 2, "ASAP predecessor", 1);
        let mut summary = current(3, 3, "Summary", 1);
        summary.predecessors = vec![link(1), link(2)];
        let tail = later(
            current(5, 5, "Long tail", 1),
            "2026-03-02 08:00",
            "2026-03-20 08:00",
        );
        let rows = import_current(vec![
            current(0, 0, "Project", 0),
            late,
            early,
            summary,
            later(
                current(4, 4, "Child", 2),
                "2026-03-03 08:00",
                "2026-03-04 08:00",
            ),
            tail,
        ])
        .unwrap();
        assert_eq!(rows[0].constraint, ConstraintType::MustStartOn);
        assert_eq!(rows[0].constraint_date, rows[0].stored_start);
        assert_eq!(rows[3].constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(rows[1].constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(rows[1].constraint_date, None);
    }

    #[test]
    fn alap_child_of_a_summary_predecessor_is_pinned_but_asap_sibling_is_not() {
        let mut late = current(2, 2, "ALAP child", 2);
        late.fields.as_mut().unwrap().constraint_type = Some(ConstraintType::AsLateAsPossible);
        let mut successor = later(
            current(4, 4, "Successor", 1),
            "2026-03-03 08:00",
            "2026-03-04 08:00",
        );
        successor.predecessors.push(link(1));
        let tail = later(
            current(5, 5, "Long tail", 1),
            "2026-03-02 08:00",
            "2026-03-20 08:00",
        );
        let rows = import_current(vec![
            current(0, 0, "Project", 0),
            current(1, 1, "Summary", 1),
            late,
            current(3, 3, "ASAP sibling", 2),
            successor,
            tail,
        ])
        .unwrap();
        assert_eq!(rows[1].constraint, ConstraintType::MustStartOn);
        assert_eq!(rows[1].constraint_date, rows[1].stored_start);
        assert_eq!(rows[3].constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(rows[2].constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(rows[2].constraint_date, None);
    }

    #[test]
    fn alap_to_asap_to_summary_chain_needs_only_upstream_pin() {
        let mut a = current(1, 1, "ALAP A", 1);
        a.fields.as_mut().unwrap().constraint_type = Some(ConstraintType::AsLateAsPossible);
        let mut b = later(
            current(2, 2, "ASAP B", 1),
            "2026-03-03 08:00",
            "2026-03-04 08:00",
        );
        b.predecessors.push(link(1));
        let mut summary = current(3, 3, "Summary", 1);
        summary.predecessors.push(link(2));
        let tail = later(
            current(5, 5, "Long tail", 1),
            "2026-03-02 08:00",
            "2026-03-20 08:00",
        );
        let rows = import_current(vec![
            current(0, 0, "Project", 0),
            a,
            b,
            summary,
            later(
                current(4, 4, "C", 2),
                "2026-03-04 08:00",
                "2026-03-05 08:00",
            ),
            tail,
        ])
        .unwrap();
        assert_eq!(rows[0].constraint, ConstraintType::MustStartOn);
        assert_eq!(rows[1].constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(rows[3].constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(rows[0].constraint_date, rows[0].stored_start);
        assert_eq!(rows[1].constraint_date, None);
    }

    #[test]
    fn independent_alap_divergences_both_need_pins() {
        let mut a = current(1, 1, "ALAP A", 1);
        a.fields.as_mut().unwrap().constraint_type = Some(ConstraintType::AsLateAsPossible);
        let mut b = current(2, 2, "ALAP B", 1);
        b.fields.as_mut().unwrap().constraint_type = Some(ConstraintType::AsLateAsPossible);
        let tail = later(
            current(3, 3, "Long tail", 1),
            "2026-03-02 08:00",
            "2026-03-20 08:00",
        );
        let rows = import_current(vec![current(0, 0, "Project", 0), a, b, tail]).unwrap();
        for row in &rows[..2] {
            assert_eq!(row.constraint, ConstraintType::MustStartOn);
            assert_eq!(row.constraint_date, row.stored_start);
        }
    }

    #[test]
    fn external_predecessor_does_not_lose_its_constraint_but_local_leaf_is_pinned() {
        let mut external = current(1, 1, "External", 1);
        external.fields.as_mut().unwrap().external_task = Some(true);
        let mut local = later(
            current(2, 2, "Local", 1),
            "2026-03-10 08:00",
            "2026-03-11 08:00",
        );
        local.predecessors.push(link(1));
        let rows = import_current(vec![current(0, 0, "Project", 0), external, local]).unwrap();
        assert_eq!(rows[0].constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(rows[1].constraint, ConstraintType::MustStartOn);
        assert_eq!(rows[1].constraint_date, rows[1].stored_start);
    }

    #[test]
    fn tracked_leaf_under_snet_summary_keeps_its_constraint() {
        let mut summary = current(1, 1, "SNET summary", 1);
        summary.fields.as_mut().unwrap().constraint_type = Some(ConstraintType::StartNoEarlierThan);
        summary.fields.as_mut().unwrap().constraint_date = Some("2026-03-09 08:00".into());
        let child = later(
            current(2, 2, "Tracked child", 2),
            "2026-03-09 08:00",
            "2026-03-10 08:00",
        );
        let tasks = super::import_tasks(
            vec![current(0, 0, "Project", 0), summary, child],
            &Project::default(),
            false,
            false,
        )
        .unwrap();
        let mut project = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks,
            ..Project::default()
        };
        project.tasks[1].actual_start = Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0));
        assert!(project.tasks[1].tracked().is_some());
        super::pin_unreproduced_starts(&mut project);
        assert_eq!(
            project.tasks[1].constraint,
            ConstraintType::AsSoonAsPossible
        );
        assert_eq!(project.tasks[1].constraint_date, None);
    }

    #[test]
    fn ineffective_pin_on_closed_start_preserves_asap() {
        let mut calendar = projcore::Calendar::standard(1);
        for day in calendar.week.iter_mut().flatten() {
            if let Some(first) = day.times.first_mut() {
                first.from = 510; // Standard now opens 08:30.
            }
        }
        let start = DateTime::from_ymd_hm(2026, 3, 2, 8, 0);
        let milestone = Task {
            uid: 1,
            id: 1,
            name: "Kickoff".into(),
            stored_start: Some(start),
            stored_finish: Some(start),
            duration_min: 0,
            ..Task::default()
        };
        let elapsed = Task {
            uid: 2,
            id: 2,
            name: "Elapsed".into(),
            stored_start: Some(start),
            stored_finish: Some(DateTime::from_ymd_hm(2026, 3, 5, 8, 0)),
            duration_min: 3 * 1440,
            ..Task::default()
        };
        let mut project = Project {
            start_date: Some(start),
            calendars: vec![calendar],
            tasks: vec![milestone, elapsed],
            ..Project::default()
        };
        super::pin_unreproduced_starts(&mut project);
        for task in &project.tasks {
            assert_eq!(task.constraint, ConstraintType::AsSoonAsPossible);
            assert_eq!(task.constraint_date, None);
            assert_eq!(
                projcore::schedule::schedule(&project)
                    .get(task.uid)
                    .unwrap()
                    .early_start,
                DateTime::from_ymd_hm(2026, 3, 2, 8, 30)
            );
        }
    }

    #[test]
    fn current_leaves_under_constrained_or_linked_summaries_keep_their_dates() {
        let mut constrained = current(1, 1, "SNET parent", 1);
        let fields = constrained.fields.as_mut().unwrap();
        fields.constraint_type = Some(ConstraintType::StartNoEarlierThan);
        fields.constraint_date = Some("2026-03-09 08:00".into());
        let mut linked = current(3, 3, "Linked parent", 1);
        linked.predecessors.push(crate::mpp::MppPred {
            pred_uid: 1,
            kind: 1,
            lag_format: 8,
            ..Default::default()
        });
        let decoded = vec![
            current(0, 0, "Project", 0),
            constrained,
            later(
                current(2, 2, "Child under SNET", 2),
                "2026-03-09 08:00",
                "2026-03-10 08:00",
            ),
            linked,
            later(
                current(4, 4, "Child under link", 2),
                "2026-03-10 08:00",
                "2026-03-11 08:00",
            ),
            current(5, 5, "Independent", 1),
        ];
        let unpinned =
            super::import_tasks(decoded.clone(), &Project::default(), false, false).unwrap();
        let project = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: unpinned,
            ..Project::default()
        };
        let schedule = projcore::schedule::schedule(&project);
        for uid in [2, 4] {
            let child = project.tasks.iter().find(|t| t.uid == uid).unwrap();
            assert_eq!(
                Some(schedule.get(uid).unwrap().early_start),
                child.stored_start
            );
        }
        let rows = import_current(decoded).unwrap();
        assert_eq!(rows[0].constraint, ConstraintType::StartNoEarlierThan);
        for uid in [2, 4] {
            let child = rows.iter().find(|t| t.uid == uid).unwrap();
            assert_eq!(
                child.constraint,
                ConstraintType::AsSoonAsPossible,
                "UID {uid}"
            );
            assert_eq!(child.constraint_date, None, "UID {uid}");
        }
        assert_eq!(rows[4].constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(rows[4].constraint_date, None);
    }

    #[test]
    fn leaf_linked_to_a_summary_keeps_its_stored_start() {
        let mut linked_leaf = later(
            current(3, 3, "Linked leaf", 1),
            "2026-03-03 08:00",
            "2026-03-04 08:00",
        );
        linked_leaf.predecessors.push(crate::mpp::MppPred {
            pred_uid: 1,
            kind: 1,
            lag_format: 8,
            ..Default::default()
        });
        let decoded = vec![
            current(0, 0, "Project", 0),
            current(1, 1, "Summary", 1),
            current(2, 2, "Child", 2),
            linked_leaf,
            current(4, 4, "Independent", 1),
        ];
        let unpinned =
            super::import_tasks(decoded.clone(), &Project::default(), false, false).unwrap();
        let project = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: unpinned,
            ..Project::default()
        };
        let linked = project.tasks.iter().find(|t| t.uid == 3).unwrap();
        assert_eq!(
            Some(
                projcore::schedule::schedule(&project)
                    .get(3)
                    .unwrap()
                    .early_start
            ),
            linked.stored_start
        );
        let rows = import_current(decoded).unwrap();
        assert_eq!(rows[2].constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(rows[2].constraint_date, None);
        assert_eq!(rows[1].constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(rows[3].constraint, ConstraintType::AsSoonAsPossible);
    }

    #[test]
    fn assignment_on_null_task_is_refused() {
        let tasks = import_tasks(vec![task(0, 0, "Project", 0), blank(1, 4)]).unwrap();
        let uids = assignment_task_uids(&tasks);
        assert_eq!(uids, [0].into_iter().collect());

        let mut fm = vec![0u8; 16 + 34];
        fm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        fm[8..12].copy_from_slice(&1u32.to_le_bytes());
        let mut row = vec![0u8; 110];
        row[..4].copy_from_slice(&7u32.to_le_bytes());
        row[4..8].copy_from_slice(&4i32.to_le_bytes());
        row[8..12].copy_from_slice(&1i32.to_le_bytes());
        let mut vm = vec![0u8; 24];
        vm[..4].copy_from_slice(&[0xba, 0xad, 0xdf, 0xfa]);
        let bytes = write_cfb_tree(&[Node::Storage(
            "   114",
            vec![Node::Storage(
                "TBkndAssn",
                vec![
                    Node::Stream("FixedMeta", fm),
                    Node::Stream("FixedData", row),
                    Node::Stream("VarMeta", vm),
                    Node::Stream("Var2Data", Vec::new()),
                ],
            )],
        )]);
        let resources = [projcore::Resource {
            uid: 1,
            ..projcore::Resource::default()
        }];
        assert!(
            crate::assndecode::decode(&bytes, false, &uids, &resources)
                .unwrap_err()
                .contains("unknown task UID 4")
        );
    }

    #[test]
    fn blank_rows_between_before_child_and_trailing_preserve_outline() {
        let rows = import_tasks(vec![
            task(0, 0, "Project", 0),
            task(1, 1, "Summary", 1),
            blank(2, 4), // immediately before the child
            task(3, 2, "Child", 2),
            blank(4, 5), // between real tasks
            task(5, 3, "After", 1),
            blank(6, 7), // trailing blank cannot make After a summary
        ])
        .unwrap();
        assert_eq!(
            rows.iter()
                .map(|t| (t.id, t.uid, t.is_null))
                .collect::<Vec<_>>(),
            [
                (1, 1, false),
                (2, 4, true),
                (3, 2, false),
                (4, 5, true),
                (5, 3, false),
                (6, 7, true)
            ]
        );
        assert!(rows[0].summary);
        assert_eq!(rows[0].duration_min, 0);
        assert!(!rows[2].summary);
        assert!(!rows[4].summary);
        assert!(rows[4].duration_min > 0);
        assert_eq!(rows[4].constraint, ConstraintType::MustStartOn);
        let p = Project {
            tasks: rows,
            ..Project::default()
        };
        let xml = projcore::mspdi::write_mspdi(&p);
        let round = projcore::mspdi::read_mspdi(&xml).unwrap();
        assert_eq!(
            round
                .tasks
                .iter()
                .filter(|t| t.is_null)
                .map(|t| t.uid)
                .collect::<Vec<_>>(),
            [4, 5, 7]
        );
    }

    #[test]
    fn childless_subproject_keeps_its_duration_and_recorded_constraint() {
        let mut sub = task(1, 1, "Inserted plan", 1);
        sub.start = Some("2026-03-04 08:00".into());
        sub.finish = Some("2026-03-05 08:00".into());
        sub.fields = Some(crate::mpp::MppTaskFields {
            is_subproject: Some(true),
            constraint_type: Some(ConstraintType::StartNoEarlierThan),
            constraint_date: Some("2026-03-04 08:00".into()),
            ..crate::mpp::MppTaskFields::default()
        });
        let rows = import_current(vec![task(0, 0, "Project", 0), sub]).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].summary);
        assert_eq!(rows[0].is_subproject, Some(true));
        assert!(rows[0].duration_min > 0);
        assert_eq!(rows[0].constraint, ConstraintType::StartNoEarlierThan);
        assert_eq!(
            rows[0].constraint_date.unwrap().to_mspdi(),
            "2026-03-04T08:00:00"
        );
        let project = Project {
            start_date: rows[0].stored_start,
            tasks: rows,
            ..Project::default()
        };
        let scheduled = projcore::schedule::schedule(&project);
        assert_eq!(
            scheduled.get(1).map(|r| r.early_start),
            project.tasks[0].constraint_date
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
