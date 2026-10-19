//! Leveling as a plan edit (#1594): Resource > Level > Level Selection and
//! Level Resource write each leveled task's own `LevelingDelay` (and its
//! pre-leveled dates) as one undo step, Clear Leveling removes them, and
//! Leveling Options are plan options. The scheduler honours a stored delay,
//! so the leveled dates are the plan's dates, saved with it. Level All stays
//! the display overlay (`Editor::leveled`, #1589) in both hosts;
//! `LevelTarget::All` is the edit no host or verb constructs yet.
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

/// Write `edits` into `proj`'s tasks.
fn apply_delay_edits(proj: &mut crate::model::Project, edits: &[DelayEdit]) {
    for e in edits {
        let t = &mut proj.tasks[e.row];
        t.leveling_delay = e.value;
        t.leveling_delay_format = e.value.map(|_| ELAPSED_DAYS_CODE);
        t.pre_leveled_start = e.pre_start;
        t.pre_leveled_finish = e.pre_finish;
    }
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

    /// Level the plan's tasks as `target` says, writing each task's own
    /// `LevelingDelay` (elapsed, from the start its links, constraints and
    /// leveled predecessors drive it to, to its leveled one: a task that
    /// only follows a delayed predecessor records none, as Project does)
    /// and its pre-leveled dates (its unleveled start and finish) as one
    /// undo step. Tasks outside the target take no delay of their own and
    /// still book their resources; an automatic one still follows its
    /// links, so it moves with a targeted predecessor as the committed plan
    /// schedules it. An ALAP task is never delayed (Project 2024 refuses a
    /// delay on one). With `clear_first` the targeted tasks' stored
    /// delays are dropped before the pass (a priority-1000 task's too, as
    /// measured on Project 2024; it then holds at its unleveled start),
    /// otherwise the pass builds on them. The stored delays reproduce the
    /// leveled dates exactly. An unchanged plan records no step.
    pub fn level_tasks(&mut self, target: &LevelTarget) -> Result<LevelOutcome, String> {
        self.level_tasks_with(target, None)
    }

    /// [`level_tasks`](Self::level_tasks) under `options` for this pass
    /// only (the plan's Leveling Options stay as they are), else the plan's.
    pub fn level_tasks_with(
        &mut self,
        target: &LevelTarget,
        options: Option<&LevelOptions>,
    ) -> Result<LevelOutcome, String> {
        let movable = self.level_movable(target)?;
        let in_scope = |uid: i32| movable.as_ref().is_none_or(|m| m.contains(&uid));
        let options = options.copied().unwrap_or_else(|| self.level_options());
        let is_leaf = |t: &crate::model::Task| !t.summary && !t.is_null && !t.is_external_leaf();
        let targeted: Vec<i32> = self
            .proj
            .tasks
            .iter()
            .filter(|t| is_leaf(t) && in_scope(t.uid))
            .map(|t| t.uid)
            .collect();
        // The plan without the scope's delays. The pre-leveled dates come
        // from it whatever `clear_first` says, so a pass that keeps the
        // stored delays still records the unleveled start, and a repeated
        // pass changes nothing.
        let mut cleared = self.proj.clone();
        for t in &mut cleared.tasks {
            if in_scope(t.uid) {
                t.leveling_delay = None;
            }
        }
        let pre_base = schedule(&cleared);
        let probe = if options.clear_first {
            &cleared
        } else {
            &self.proj
        };
        let leveled = level_with(
            probe,
            &LevelScope {
                movable: movable.as_ref(),
                order: options.order,
                within_slack: options.within_slack,
            },
        );
        // Each targeted leaf's intended values: its own delay (on top of a
        // kept one) and, with a delay, its unleveled dates.
        let pre_for = |uid: i32, value: Option<i64>| match value {
            Some(_) => {
                let pre = pre_base.get(uid);
                (pre.map(|r| r.early_start), pre.map(|r| r.early_finish))
            }
            None => (None, None),
        };
        let mut plan: Vec<DelayEdit> = Vec::new();
        for (i, t) in self.proj.tasks.iter().enumerate() {
            if !is_leaf(t) || !in_scope(t.uid) {
                continue;
            }
            // An ALAP task carries no delay (Project 2024 refuses one) and
            // floats with the finish, so nothing is written or settled for it.
            if t.constraint == crate::model::ConstraintType::AsLateAsPossible {
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
            let (pre_start, pre_finish) = pre_for(t.uid, value);
            plan.push(DelayEdit {
                row: i,
                value,
                pre_start,
                pre_finish,
            });
        }
        // The stored delays must give the leveled dates exactly. The
        // leveler's driven start is an estimate where a constraint rather
        // than a link drove a task, so settle every targeted leaf on a trial
        // of the plan: reschedule it and move each delay by what its start
        // still misses, a few rounds at most. A plan that does not settle is
        // refused with nothing changed.
        let mut trial = self.proj.clone();
        apply_delay_edits(&mut trial, &plan);
        let mut settled = false;
        for _ in 0..6 {
            let sched = schedule(&trial);
            let mut moved = false;
            for e in &mut plan {
                let uid = trial.tasks[e.row].uid;
                let (Some(want), Some(got)) = (leveled.start(uid), sched.get(uid)) else {
                    continue;
                };
                let miss = want.minutes() - got.early_start.minutes();
                if miss == 0 {
                    continue;
                }
                let delay = (e.value.unwrap_or(0) / 10 + miss).max(0);
                e.value = (delay > 0).then_some(delay * 10);
                (e.pre_start, e.pre_finish) = pre_for(uid, e.value);
                moved = true;
            }
            if !moved {
                settled = true;
                break;
            }
            apply_delay_edits(&mut trial, &plan);
        }
        if !settled {
            let sched = schedule(&trial);
            let off = plan
                .iter()
                .map(|e| trial.tasks[e.row].uid)
                .find(|&uid| leveled.start(uid) != sched.get(uid).map(|r| r.early_start))
                .unwrap_or(0);
            return Err(format!(
                "leveling cannot be stored: task {off}'s leveled start is not reachable by a \
                 leveling delay; nothing changed"
            ));
        }
        let count_delayed = |proj: &crate::model::Project| {
            proj.tasks
                .iter()
                .filter(|t| is_leaf(t) && in_scope(t.uid))
                .filter(|t| t.leveling_delay.is_some_and(|d| d > 0))
                .count()
        };
        // Only what differs from the plan as it is becomes an edit.
        let edits: Vec<DelayEdit> = plan
            .into_iter()
            .filter(|e| {
                let t = &self.proj.tasks[e.row];
                (
                    t.leveling_delay.filter(|v| *v != 0),
                    t.pre_leveled_start,
                    t.pre_leveled_finish,
                ) != (e.value, e.pre_start, e.pre_finish)
            })
            .collect();
        if edits.is_empty() {
            return Ok(LevelOutcome {
                changed: 0,
                delayed: count_delayed(&self.proj),
                overallocated: self.shown_overallocated(),
                targeted,
            });
        }
        self.snapshot();
        let changed = edits.len();
        apply_delay_edits(&mut self.proj, &edits);
        self.changed();
        Ok(LevelOutcome {
            changed,
            delayed: count_delayed(&self.proj),
            overallocated: self.shown_overallocated(),
            targeted,
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
                // A uid named twice is one task.
                rows.sort_unstable();
                rows.dedup();
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
    use crate::model::{Assignment, Predecessor, Resource, Task};

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
        // The pre-leveled dates are the unleveled ones, not the kept
        // delay's, and a second pass without clearing changes nothing and
        // records no step (#1594 r1).
        assert_eq!(
            ed.project().task(2).unwrap().pre_leveled_start,
            Some(start(&ed, 1))
        );
        let depth = ed.undo_depth();
        let again = ed.level_tasks(&LevelTarget::All).unwrap();
        assert_eq!(again.changed, 0);
        assert_eq!(ed.undo_depth(), depth);
    }

    /// #1594 r1: a task that only follows a leveled predecessor records no
    /// delay of its own (Project 2024 over COM: the FS successor of a
    /// leveled task keeps LevelingDelay 0), and the stored delays reproduce
    /// the leveled dates exactly, committed and reloaded.
    #[test]
    fn a_successor_follows_a_leveled_predecessor_without_a_delay_of_its_own() {
        let mut p = staffed().project().clone();
        // Build and Paint share Ann for a day each; Trim follows Paint.
        for t in &mut p.tasks {
            t.duration_min = 480;
        }
        p.assignments.retain(|a| a.task_uid != 3);
        p.tasks[2].predecessors.push(Predecessor::fs(2));
        let mut ed = Editor::new(p);
        let anchor = start(&ed, 1);
        let leveled = level_with(ed.project(), &LevelScope::default());
        let out = ed.level_tasks(&LevelTarget::All).unwrap();
        assert_eq!((out.changed, out.delayed), (1, 1));
        assert_eq!(out.targeted, vec![1, 2, 3]);
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(1440 * 10)
        );
        assert_eq!(ed.project().task(3).unwrap().leveling_delay, None);
        assert_eq!(
            start(&ed, 2),
            DateTime::from_minutes(anchor.minutes() + 1440)
        );
        assert!(start(&ed, 3) >= finish(&ed, 2));
        for uid in 1..=3 {
            assert_eq!(start(&ed, uid), leveled.start(uid).unwrap(), "task {uid}");
        }
        let back = crate::mspdi::read_mspdi(&crate::mspdi::write_mspdi(ed.project())).unwrap();
        let sched = crate::schedule::schedule(&back);
        for uid in 1..=3 {
            assert_eq!(
                sched.get(uid).unwrap().early_start,
                start(&ed, uid),
                "reloaded task {uid}"
            );
        }
    }

    /// #1594 r1: within slack is measured against the CPM late start, so a
    /// successor whose leveled predecessor used the shared slack cannot
    /// use it again and move the project finish.
    #[test]
    fn within_slack_counts_the_slack_a_leveled_predecessor_used() {
        let mut p = untitled_project();
        // A and B share R1 for a day each; C on R2 follows B; D holds R2
        // for three days. B and C have a day of slack, D none.
        for (uid, name, days, res) in [
            (1, "A", 1, 1),
            (2, "B", 1, 1),
            (3, "C", 1, 2),
            (4, "D", 3, 2),
        ] {
            p.tasks.push(Task {
                uid,
                id: uid,
                name: name.into(),
                outline_level: 1,
                duration_min: days * 480,
                ..Task::default()
            });
            p.assignments.push(Assignment {
                uid,
                task_uid: uid,
                resource_uid: res,
                units: 1.0,
                ..Assignment::default()
            });
        }
        p.tasks[2].predecessors.push(Predecessor::fs(2));
        for (uid, name) in [(1, "R1"), (2, "R2")] {
            p.resources.push(Resource {
                uid,
                id: uid,
                name: name.into(),
                max_units: 1.0,
                ..Resource::default()
            });
        }
        let mut ed = Editor::new(p);
        let finish_before = ed.schedule().project_finish;
        ed.set_level_options(&LevelOptions {
            within_slack: true,
            ..LevelOptions::default()
        })
        .unwrap();
        let out = ed.level_tasks(&LevelTarget::All).unwrap();
        // B takes its day of slack behind A; C, driven to Wednesday by B,
        // would need Thursday for R2 and stays, overallocated with D.
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(1440 * 10)
        );
        assert_eq!(ed.project().task(3).unwrap().leveling_delay, None);
        assert_eq!(ed.schedule().project_finish, finish_before);
        assert_eq!(out.overallocated, 1);
    }

    /// #1594 r1, measured on Project 2024 over COM: with clear-before-
    /// leveling on, a priority-1000 task's stored delay is cleared like any
    /// other's and the task then sits at its unleveled start, held there
    /// while the others level around it; with it off, it keeps its delay.
    #[test]
    fn clear_first_clears_a_do_not_level_tasks_delay_and_holds_it_there() {
        let mut p = staffed().project().clone();
        p.tasks[2].priority = Some(1000);
        p.tasks[2].leveling_delay = Some(1440 * 10);
        p.tasks[2].leveling_delay_format = Some(8);
        let mut ed = Editor::new(p);
        let anchor = start(&ed, 1);
        assert_eq!(
            start(&ed, 3),
            DateTime::from_minutes(anchor.minutes() + 1440)
        );
        ed.level_tasks(&LevelTarget::All).unwrap();
        assert_eq!(ed.project().task(3).unwrap().leveling_delay, None);
        assert_eq!(start(&ed, 3), anchor);
        assert!(start(&ed, 1) >= finish(&ed, 3));
        assert!(start(&ed, 2) >= finish(&ed, 1));
        let mut p = staffed().project().clone();
        p.tasks[2].priority = Some(1000);
        p.tasks[2].leveling_delay = Some(1440 * 10);
        let mut ed = Editor::new(p);
        ed.set_level_options(&LevelOptions {
            clear_first: false,
            ..LevelOptions::default()
        })
        .unwrap();
        ed.level_tasks(&LevelTarget::All).unwrap();
        assert_eq!(
            ed.project().task(3).unwrap().leveling_delay,
            Some(1440 * 10)
        );
        assert_eq!(
            start(&ed, 3),
            DateTime::from_minutes(anchor.minutes() + 1440)
        );
        // A uid named twice in Clear Leveling is one task.
        assert_eq!(ed.clear_leveling(Some(&[3, 3])).unwrap(), 1);
    }

    /// #1594 r2: with only Paint selected on a chain Build, Paint, Trim (one
    /// day each on Ann, Trim after Paint), Paint takes Tuesday and Trim,
    /// outside the selection, follows it to Wednesday with no delay of its
    /// own: the leveler's dates are the committed plan's.
    #[test]
    fn a_held_successor_follows_its_moved_predecessor() {
        let mut p = staffed().project().clone();
        for t in &mut p.tasks {
            t.duration_min = 480;
        }
        p.tasks[2].predecessors.push(Predecessor::fs(2));
        let mut ed = Editor::new(p);
        let anchor = start(&ed, 1);
        let only_paint: std::collections::HashSet<i32> = [2].into_iter().collect();
        let leveled = level_with(
            ed.project(),
            &LevelScope {
                movable: Some(&only_paint),
                ..LevelScope::default()
            },
        );
        let out = ed.level_tasks(&LevelTarget::Selection(vec![2])).unwrap();
        assert_eq!((out.changed, out.delayed), (1, 1));
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(1440 * 10)
        );
        assert_eq!(ed.project().task(3).unwrap().leveling_delay, None);
        assert_eq!(
            start(&ed, 2),
            DateTime::from_minutes(anchor.minutes() + 1440)
        );
        assert_eq!(
            start(&ed, 3),
            DateTime::from_minutes(anchor.minutes() + 2 * 1440)
        );
        for uid in 1..=3 {
            assert_eq!(start(&ed, uid), leveled.start(uid).unwrap(), "task {uid}");
        }
    }

    /// #1594 r2: settlement covers every targeted leaf, not only those whose
    /// values changed: Trim (selected, after Paint, Start No Earlier Than
    /// Wednesday, on Bob with a Wednesday task of Bob's) lands where the
    /// committed plan puts it, equal to the leveler's start.
    #[test]
    fn settlement_covers_a_constrained_successor() {
        let mut p = staffed().project().clone();
        for t in &mut p.tasks {
            t.duration_min = 480;
        }
        let anchor = p.start_date.unwrap();
        let wednesday = DateTime::from_minutes(anchor.minutes() + 2 * 1440);
        p.tasks[2].predecessors.push(Predecessor::fs(2));
        p.tasks[2].constraint = crate::model::ConstraintType::StartNoEarlierThan;
        p.tasks[2].constraint_date = Some(wednesday);
        p.assignments[2].resource_uid = 2;
        p.tasks.push(Task {
            uid: 4,
            id: 4,
            name: "Other".into(),
            outline_level: 1,
            duration_min: 480,
            constraint: crate::model::ConstraintType::StartNoEarlierThan,
            constraint_date: Some(wednesday),
            ..Task::default()
        });
        p.assignments.push(Assignment {
            uid: 4,
            task_uid: 4,
            resource_uid: 2,
            units: 1.0,
            ..Assignment::default()
        });
        p.resources.push(Resource {
            uid: 2,
            id: 2,
            name: "Bob".into(),
            max_units: 1.0,
            ..Resource::default()
        });
        let mut ed = Editor::new(p);
        let selected: std::collections::HashSet<i32> = [2, 3].into_iter().collect();
        let leveled = level_with(
            ed.project(),
            &LevelScope {
                movable: Some(&selected),
                ..LevelScope::default()
            },
        );
        let out = ed.level_tasks(&LevelTarget::Selection(vec![2, 3])).unwrap();
        assert_eq!(out.targeted, vec![2, 3]);
        for uid in [2, 3] {
            assert_eq!(start(&ed, uid), leveled.start(uid).unwrap(), "task {uid}");
        }
        // Paint moved a day; Trim's SNET already absorbed it, so its own
        // delay is what the leveler needed past Wednesday, if anything.
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(1440 * 10)
        );
        assert!(start(&ed, 3) >= wednesday);
    }

    /// #1594 r2: within slack sees the slack a delayed successor really
    /// leaves. X and A share Ann; B follows A and carries a stored delay of
    /// a day, outside the selection. A has no slack (B's late start less its
    /// delay), so A stays with X, overallocated, and the finish holds.
    #[test]
    fn within_slack_counts_a_delayed_successors_delay() {
        let mut p = staffed().project().clone();
        for t in &mut p.tasks {
            t.duration_min = 480;
        }
        // Build = X, Paint = A, Trim = B (after A, delayed, not on Ann).
        p.tasks[2].predecessors.push(Predecessor::fs(2));
        p.tasks[2].leveling_delay = Some(1440 * 10);
        p.tasks[2].leveling_delay_format = Some(8);
        p.assignments.retain(|a| a.task_uid != 3);
        let mut ed = Editor::new(p);
        let finish_before = ed.schedule().project_finish;
        assert_eq!(ed.schedule().get(2).unwrap().total_slack_min, 0);
        ed.set_level_options(&LevelOptions {
            within_slack: true,
            ..LevelOptions::default()
        })
        .unwrap();
        let out = ed.level_tasks(&LevelTarget::Selection(vec![1, 2])).unwrap();
        assert_eq!(ed.project().task(2).unwrap().leveling_delay, None);
        assert_eq!(ed.schedule().project_finish, finish_before);
        assert_eq!(out.overallocated, 1);
    }

    /// #1594 r2: an ALAP task is never delayed by the editor's pass either.
    #[test]
    fn an_alap_task_is_not_leveled() {
        let mut p = staffed().project().clone();
        p.tasks[2].constraint = crate::model::ConstraintType::AsLateAsPossible;
        let mut ed = Editor::new(p);
        let out = ed.level_tasks(&LevelTarget::All).unwrap();
        // Trim sits as late as possible over Build's and Paint's days, held
        // there: both of them level around it and it takes no delay.
        assert_eq!(out.delayed, 2);
        assert_eq!(ed.project().task(3).unwrap().leveling_delay, None);
        // Still as late as possible: its start is the schedule's late start,
        // which moved with the finish the leveled tasks pushed.
        let trim = ed.schedule().get(3).unwrap();
        assert_eq!(trim.early_start, trim.late_start);
    }
}
