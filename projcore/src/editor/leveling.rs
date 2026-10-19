//! Leveling as a plan edit (#1594): Resource > Level > Level All, Level
//! Selection and Level Resource write each leveled task's `LevelingDelay`
//! (and its pre-leveled dates) as one undo step, Clear Leveling removes
//! them, and Leveling Options are plan options. The scheduler honours a
//! stored delay, so the leveled dates are the plan's dates, saved with it;
//! the display overlay (`Editor::leveled`, #1589) stays for the
//! display-only mode.
use super::*;
use crate::leveling::{LevelOptions, LevelOutcome, LevelTarget};
use crate::schedule::{LevelScope, level_with, overbookings};
use std::collections::HashSet;

/// MSPDI `LevelingDelayFormat` for elapsed days, the unit Project shows
/// leveling delays in.
const ELAPSED_DAYS_CODE: i32 = 8;

/// One task's new leveling values: its row, its delay in tenths of a minute
/// (`None` for none) and its pre-leveled dates.
struct DelayEdit {
    row: usize,
    value: Option<i64>,
    pre_start: Option<DateTime>,
    pre_finish: Option<DateTime>,
}

impl Editor {
    /// The plan's Leveling Options.
    pub fn level_options(&self) -> LevelOptions {
        LevelOptions::of(&self.proj)
    }

    /// Set the plan's Leveling Options, one undo step; `Ok(false)` when
    /// unchanged.
    pub fn set_level_options(&mut self, options: &LevelOptions) -> Result<bool, String> {
        if LevelOptions::of(&self.proj) == *options {
            return Ok(false);
        }
        self.snapshot();
        options.store(&mut self.proj);
        self.changed();
        Ok(true)
    }

    /// Level the plan's tasks as `target` says, writing each task's
    /// `LevelingDelay` (elapsed, from its unleveled start to its leveled
    /// one) and pre-leveled dates as one undo step. Tasks outside the
    /// target keep their dates and still book their resources. With
    /// `clear_first`, the targeted tasks' stored delays are dropped before
    /// the pass. An unchanged plan records no step.
    pub fn level_tasks(&mut self, target: &LevelTarget) -> Result<LevelOutcome, String> {
        let movable = self.level_movable(target)?;
        let in_scope = |uid: i32| movable.as_ref().is_none_or(|m| m.contains(&uid));
        let options = self.level_options();
        let mut probe = self.proj.clone();
        if options.clear_first {
            for t in &mut probe.tasks {
                if in_scope(t.uid) {
                    t.leveling_delay = None;
                }
            }
        }
        let base = schedule(&probe);
        let leveled = level_with(
            &probe,
            &LevelScope {
                movable: movable.as_ref(),
                order: options.order,
                within_slack: options.within_slack,
            },
        );
        let mut edits: Vec<DelayEdit> = Vec::new();
        let mut delayed = 0;
        for (i, t) in self.proj.tasks.iter().enumerate() {
            if t.summary || t.is_null || t.is_external_leaf() || !in_scope(t.uid) {
                continue;
            }
            let Some(delay) = leveled.delay_min(t.uid) else {
                continue;
            };
            // Without clearing first, the pass builds on the stored delay,
            // which stays part of the task's start.
            let kept = if options.clear_first {
                0
            } else {
                t.leveling_delay.unwrap_or(0).max(0) / 10
            };
            let delay = kept + delay;
            let value = (delay > 0).then_some(delay * 10);
            if value.is_some() {
                delayed += 1;
            }
            let cpm = base.get(t.uid);
            let (pre_start, pre_finish) = match value {
                Some(_) => (cpm.map(|r| r.early_start), cpm.map(|r| r.early_finish)),
                None => (None, None),
            };
            let now = (
                t.leveling_delay.filter(|v| *v != 0),
                t.pre_leveled_start,
                t.pre_leveled_finish,
            );
            if now != (value, pre_start, pre_finish) {
                edits.push(DelayEdit {
                    row: i,
                    value,
                    pre_start,
                    pre_finish,
                });
            }
        }
        if edits.is_empty() {
            return Ok(LevelOutcome {
                changed: 0,
                delayed,
                overallocated: self.shown_overallocated(),
            });
        }
        self.snapshot();
        let changed = edits.len();
        for DelayEdit {
            row,
            value,
            pre_start,
            pre_finish,
        } in edits
        {
            let t = &mut self.proj.tasks[row];
            t.leveling_delay = value;
            t.leveling_delay_format = value.map(|_| ELAPSED_DAYS_CODE);
            t.pre_leveled_start = pre_start;
            t.pre_leveled_finish = pre_finish;
        }
        self.changed();
        Ok(LevelOutcome {
            changed,
            delayed,
            overallocated: self.shown_overallocated(),
        })
    }

    /// Clear the stored leveling delays (and pre-leveled dates) of `uids`,
    /// or of every task, one undo step; the count of tasks changed (0
    /// records no step).
    pub fn clear_leveling(&mut self, uids: Option<&[i32]>) -> Result<usize, String> {
        let rows: Vec<usize> = match uids {
            None => (0..self.proj.tasks.len()).collect(),
            Some(uids) => {
                let mut rows = Vec::new();
                for &uid in uids {
                    let i = self
                        .proj
                        .tasks
                        .iter()
                        .position(|t| t.uid == uid)
                        .ok_or_else(|| format!("no task with uid {uid}"))?;
                    rows.push(i);
                }
                rows
            }
        };
        let changed: Vec<usize> = rows
            .into_iter()
            .filter(|&i| {
                let t = &self.proj.tasks[i];
                t.leveling_delay.is_some_and(|d| d != 0)
                    || t.pre_leveled_start.is_some()
                    || t.pre_leveled_finish.is_some()
            })
            .collect();
        if changed.is_empty() {
            return Ok(0);
        }
        self.snapshot();
        for &i in &changed {
            let t = &mut self.proj.tasks[i];
            t.leveling_delay = None;
            t.leveling_delay_format = None;
            t.pre_leveled_start = None;
            t.pre_leveled_finish = None;
        }
        self.changed();
        Ok(changed.len())
    }

    /// The tasks a target lets the leveler move: `None` for all. A summary
    /// in a selection stands for the tasks under it.
    fn level_movable(&self, target: &LevelTarget) -> Result<Option<HashSet<i32>>, String> {
        match target {
            LevelTarget::All => Ok(None),
            LevelTarget::Selection(uids) => {
                if uids.is_empty() {
                    return Err("level the selection: no tasks selected".into());
                }
                let mut set = HashSet::new();
                for &uid in uids {
                    let range = self
                        .subtree(uid)
                        .map_err(|_| format!("no task with uid {uid}"))?;
                    set.extend(self.proj.tasks[range].iter().map(|t| t.uid));
                }
                Ok(Some(set))
            }
            LevelTarget::Resource(uid) => {
                let resource = self
                    .proj
                    .resources
                    .iter()
                    .find(|r| r.uid == *uid)
                    .ok_or_else(|| format!("no resource with uid {uid}"))?;
                let set: HashSet<i32> = self
                    .proj
                    .assignments
                    .iter()
                    .filter(|a| a.resource_uid == *uid)
                    .map(|a| a.task_uid)
                    .collect();
                if set.is_empty() {
                    return Err(format!("{} has no assignments to level", resource.name));
                }
                Ok(Some(set))
            }
        }
    }

    /// Work resources booked past their capacity on the plan as shown.
    fn shown_overallocated(&self) -> usize {
        overbookings(&self.proj, |uid| {
            Some((self.disp_start(uid)?, self.disp_finish(uid)?))
        })
        .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leveling::{LevelOptions, LevelOrder, LevelTarget};
    use crate::model::{Assignment, Resource, Task};

    /// Ann on three parallel two-day tasks: Build (1), Paint (2), Trim (3).
    fn staffed() -> Editor {
        let mut p = untitled_project();
        for (uid, name) in [(1, "Build"), (2, "Paint"), (3, "Trim")] {
            p.tasks.push(Task {
                uid,
                id: uid,
                name: name.into(),
                outline_level: 1,
                duration_min: 2 * 480,
                ..Task::default()
            });
            p.assignments.push(Assignment {
                uid,
                task_uid: uid,
                resource_uid: 1,
                units: 1.0,
                ..Assignment::default()
            });
        }
        p.resources.push(Resource {
            uid: 1,
            id: 1,
            name: "Ann".into(),
            max_units: 1.0,
            ..Resource::default()
        });
        Editor::new(p)
    }

    fn start(ed: &Editor, uid: i32) -> DateTime {
        ed.schedule().get(uid).unwrap().early_start
    }
    fn finish(ed: &Editor, uid: i32) -> DateTime {
        ed.schedule().get(uid).unwrap().early_finish
    }

    #[test]
    fn level_all_writes_each_delay_as_one_undo_step_and_the_schedule_follows() {
        let mut ed = staffed();
        let anchor = start(&ed, 1);
        assert_eq!(start(&ed, 2), anchor, "parallel before leveling");
        let depth = ed.undo_depth();
        let out = ed.level_tasks(&LevelTarget::All).unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (2, 2, 0));
        assert_eq!(ed.undo_depth(), depth + 1);
        assert!(ed.dirty());
        // Paint follows Build, Trim follows Paint: two and four elapsed days
        // (Project stores tenths of a minute, shown in elapsed days).
        let paint = ed.project().task(2).unwrap();
        assert_eq!(paint.leveling_delay, Some(2 * 1440 * 10));
        assert_eq!(paint.leveling_delay_format, Some(8));
        assert_eq!(paint.pre_leveled_start, Some(anchor));
        assert_eq!(
            ed.project().task(3).unwrap().leveling_delay,
            Some(4 * 1440 * 10)
        );
        assert_eq!(ed.project().task(1).unwrap().leveling_delay, None);
        assert_eq!(
            start(&ed, 2),
            finish(&ed, 1).max(DateTime::from_minutes(anchor.minutes() + 2 * 1440))
        );
        assert!(start(&ed, 2) >= finish(&ed, 1));
        assert!(start(&ed, 3) >= finish(&ed, 2));
        // The schedule reproduces, so a save writes the leveled dates.
        let back = crate::mspdi::read_mspdi(&crate::mspdi::write_mspdi(ed.project())).unwrap();
        assert_eq!(back.task(2).unwrap().leveling_delay, Some(2 * 1440 * 10));
        assert_eq!(
            crate::schedule::schedule(&back).get(2).unwrap().early_start,
            start(&ed, 2)
        );
        // Leveled again, nothing changes and no step is recorded.
        let again = ed.level_tasks(&LevelTarget::All).unwrap();
        assert_eq!((again.changed, again.delayed), (0, 2));
        assert_eq!(ed.undo_depth(), depth + 1);
        ed.undo();
        assert_eq!(ed.project().task(2).unwrap().leveling_delay, None);
        assert_eq!(start(&ed, 2), anchor);
        ed.redo();
        assert_eq!(
            ed.project().task(3).unwrap().leveling_delay,
            Some(4 * 1440 * 10)
        );
    }

    #[test]
    fn clear_leveling_drops_the_delays_of_the_named_tasks_or_all() {
        let mut ed = staffed();
        let anchor = start(&ed, 1);
        ed.level_tasks(&LevelTarget::All).unwrap();
        let depth = ed.undo_depth();
        assert_eq!(ed.clear_leveling(Some(&[2])).unwrap(), 1);
        assert_eq!(ed.undo_depth(), depth + 1);
        assert_eq!(ed.project().task(2).unwrap().leveling_delay, None);
        assert_eq!(ed.project().task(2).unwrap().pre_leveled_start, None);
        assert_eq!(start(&ed, 2), anchor);
        assert_eq!(
            ed.project().task(3).unwrap().leveling_delay,
            Some(4 * 1440 * 10)
        );
        assert_eq!(ed.clear_leveling(None).unwrap(), 1);
        assert_eq!(ed.clear_leveling(None).unwrap(), 0, "nothing left: no step");
        assert_eq!(ed.undo_depth(), depth + 2);
        assert_eq!(
            ed.clear_leveling(Some(&[9])).unwrap_err(),
            "no task with uid 9"
        );
    }

    #[test]
    fn a_selection_or_a_resource_levels_only_those_tasks_around_the_rest() {
        let mut ed = staffed();
        let anchor = start(&ed, 1);
        // Trim alone: Build and Paint stay (and stay overallocated), Trim
        // follows their booking.
        let out = ed.level_tasks(&LevelTarget::Selection(vec![3])).unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (1, 1, 1));
        assert_eq!(start(&ed, 2), anchor);
        assert_eq!(
            start(&ed, 3),
            finish(&ed, 1).max(DateTime::from_minutes(anchor.minutes() + 2 * 1440))
        );
        // The resource's tasks: all three, the delays rebuilt from clean
        // dates (clear before leveling).
        let out = ed.level_tasks(&LevelTarget::Resource(1)).unwrap();
        assert_eq!((out.delayed, out.overallocated), (2, 0));
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(2 * 1440 * 10)
        );
        assert_eq!(
            ed.project().task(3).unwrap().leveling_delay,
            Some(4 * 1440 * 10)
        );
        // Refusals name the problem and change nothing.
        let depth = ed.undo_depth();
        assert_eq!(
            ed.level_tasks(&LevelTarget::Selection(vec![])).unwrap_err(),
            "level the selection: no tasks selected"
        );
        assert_eq!(
            ed.level_tasks(&LevelTarget::Selection(vec![7]))
                .unwrap_err(),
            "no task with uid 7"
        );
        assert_eq!(
            ed.level_tasks(&LevelTarget::Resource(2)).unwrap_err(),
            "no resource with uid 2"
        );
        let mut p = ed.project().clone();
        p.resources.push(Resource {
            uid: 2,
            id: 2,
            name: "Bob".into(),
            max_units: 1.0,
            ..Resource::default()
        });
        let mut idle = Editor::new(p);
        assert_eq!(
            idle.level_tasks(&LevelTarget::Resource(2)).unwrap_err(),
            "Bob has no assignments to level"
        );
        assert_eq!(ed.undo_depth(), depth);
    }

    #[test]
    fn options_are_a_plan_edit_and_drive_the_order_and_the_slack_rule() {
        let mut ed = staffed();
        let anchor = start(&ed, 1);
        assert_eq!(ed.level_options(), LevelOptions::default());
        assert!(!ed.set_level_options(&LevelOptions::default()).unwrap());
        let depth = ed.undo_depth();
        let priority = LevelOptions {
            order: LevelOrder::PriorityStandard,
            ..LevelOptions::default()
        };
        assert!(ed.set_level_options(&priority).unwrap());
        assert_eq!(ed.undo_depth(), depth + 1);
        assert_eq!(ed.level_options(), priority);
        assert_eq!(ed.project().option("LevelingOrder"), Some("Priority"));
        // Paint outranks Trim outranks Build: Paint keeps its dates.
        let mut p = ed.project().clone();
        p.tasks[1].priority = Some(900);
        p.tasks[2].priority = Some(700);
        let mut ed = Editor::new(p);
        ed.level_tasks(&LevelTarget::All).unwrap();
        assert_eq!(start(&ed, 2), anchor);
        assert!(start(&ed, 3) >= finish(&ed, 2));
        assert!(start(&ed, 1) >= finish(&ed, 3));
        // ID order: Build, Paint, Trim.
        let id_only = LevelOptions {
            order: LevelOrder::IdOnly,
            ..LevelOptions::default()
        };
        ed.set_level_options(&id_only).unwrap();
        ed.level_tasks(&LevelTarget::All).unwrap();
        assert_eq!(start(&ed, 1), anchor);
        assert!(start(&ed, 2) >= finish(&ed, 1) && start(&ed, 3) >= finish(&ed, 2));
        // Priority 1000 is never leveled, whatever the order.
        let mut p = staffed().project().clone();
        p.tasks[2].priority = Some(1000);
        let mut ed = Editor::new(p);
        ed.level_tasks(&LevelTarget::All).unwrap();
        assert_eq!(start(&ed, 3), anchor);
        assert_eq!(ed.project().task(3).unwrap().leveling_delay, None);
        assert!(start(&ed, 1) >= finish(&ed, 3));
        // Within slack: three parallel tasks have none, so nothing moves.
        let mut ed = staffed();
        ed.set_level_options(&LevelOptions {
            within_slack: true,
            ..LevelOptions::default()
        })
        .unwrap();
        let out = ed.level_tasks(&LevelTarget::All).unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (0, 0, 1));
        assert_eq!(start(&ed, 2), anchor);
    }

    #[test]
    fn a_stored_delay_is_part_of_the_schedule_and_a_pass_starts_clean() {
        let mut p = staffed().project().clone();
        // One elapsed day, as Project stores it (tenths of a minute).
        p.tasks[1].leveling_delay = Some(1440 * 10);
        p.tasks[1].leveling_delay_format = Some(8);
        let mut ed = Editor::new(p);
        let anchor = start(&ed, 1);
        assert_eq!(
            start(&ed, 2),
            DateTime::from_minutes(anchor.minutes() + 1440)
        );
        // Leveling clears the old delay first: Paint lands after Build, two
        // days, not one plus two.
        ed.level_tasks(&LevelTarget::All).unwrap();
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(2 * 1440 * 10)
        );
        // Without clearing first, the old delay is the floor it builds on.
        let mut p = staffed().project().clone();
        p.tasks[1].leveling_delay = Some(3 * 1440 * 10);
        let mut ed = Editor::new(p);
        ed.set_level_options(&LevelOptions {
            clear_first: false,
            ..LevelOptions::default()
        })
        .unwrap();
        ed.level_tasks(&LevelTarget::All).unwrap();
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(3 * 1440 * 10)
        );
        assert!(start(&ed, 3) >= finish(&ed, 2));
    }
}
