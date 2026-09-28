//! Convert validated MPP metadata and tasks to a schedulable project.

use projcore::editor::default_anchor;
use projcore::{ConstraintType, DateTime, LagFormat, LinkType, Predecessor, Project, Task};

/// Build a project from a structurally recognized `.mpp` task table. Task UID 0
/// is its project summary and supplies a fallback name, but is not imported as
/// a task. Each decoded automatic task without local children, including an
/// inserted subproject, is pinned
/// with a Must-Start-On constraint at its start and given a duration equal to
/// the working minutes between its start and finish, so the scheduler reproduces
/// the real dates. A **manual** leaf keeps its mode and its manual start,
/// finish and duration instead, which hold it where Project put it without a
/// constraint; the project's new-task mode comes through too.
/// The **outline levels** (WBS depth) decode too, so summary tasks and their
/// rollup come through, and the **predecessor links** decode from the `TBkndCons`
/// table. Each task's recorded **progress**, work and cost come through as
/// read (the scheduler ignores them, as it does for MSPDI); Project's
/// variances are not stored in the file and stay absent. Validated task fields
/// and blank grid rows come through too. Save As converts it
/// to `.yppx`/MSPDI.
pub fn project_from_mpp(bytes: &[u8]) -> Result<Project, String> {
    let info = crate::read_mpp(bytes)?;
    let table = crate::taskdecode::decode_table(bytes)
        .map_err(|e| format!("cannot read the task table of this .mpp ({e})"))?;
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
    let tasks = import_tasks(decoded)?;
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

fn import_tasks(decoded: Vec<crate::mpp::MppTask>) -> Result<Vec<Task>, String> {
    let cal_ref = Project::default();
    let decoded: Vec<_> = decoded.into_iter().filter(|t| t.uid != 0).collect();
    // Outline summaries own the next deeper row; Project also labels an
    // inserted subproject as a summary even when its children live elsewhere.
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
            let outline_summary = decoded[i + 1..]
                .iter()
                .find(|next| !next.is_null)
                .is_some_and(|next| next.outline_level.is_some_and(|nxt| nxt > level));
            let project_summary = t
                .fields
                .as_ref()
                .is_some_and(|f| f.is_subproject == Some(true));
            let mut task = Task {
                uid: t.uid as i32,
                id: t.id as i32,
                name: t.name.clone(),
                outline_level: level,
                summary: outline_summary || project_summary,
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
                task.wbs = fields.wbs.clone();
                task.task_type = fields.task_type;
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
            // Pin auto leaves and childless inserted subprojects. Outline
            // summaries roll up from children, so a constraint would fight
            // the rollup. Manual leaves use their stored dates instead.
            if outline_summary {
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
    Ok(tasks)
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
    fn childless_subproject_keeps_its_duration_and_start_pin() {
        let mut sub = task(1, 1, "Inserted plan", 1);
        sub.fields = Some(crate::mpp::MppTaskFields {
            is_subproject: Some(true),
            ..crate::mpp::MppTaskFields::default()
        });
        let rows = import_tasks(vec![task(0, 0, "Project", 0), sub]).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].summary);
        assert_eq!(rows[0].is_subproject, Some(true));
        assert!(rows[0].duration_min > 0);
        assert_eq!(rows[0].constraint, ConstraintType::MustStartOn);
        assert_eq!(rows[0].constraint_date, rows[0].stored_start);
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
