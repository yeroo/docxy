//! Leveling as a plan edit (#1594): Resource > Level > Level Selection and
//! Level Resource write each leveled task's own `LevelingDelay` (and its
//! pre-leveled dates) as one undo step, Clear Leveling removes them, and
//! Leveling Options are plan options. The scheduler honours a stored delay,
//! so the leveled dates are the plan's dates, saved with it. Level All stays
//! the display overlay (`Editor::leveled`, #1589) in both hosts;
//! `LevelTarget::All` is the edit no host or verb constructs yet.
use super::*;
use crate::leveling::{LevelOptions, LevelOutcome, LevelTarget};
use crate::schedule::{LevelScope, level_with, overallocation, overbookings};
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
    /// are booked at the dates the plan gives them now; an automatic one
    /// still follows its links once the delays are written, so the pass
    /// repeats on the plan with its delays written while a round still
    /// changes the plan's overallocation (a smaller overbooked extent on the
    /// committed schedule, or conflicts not seen before); a round that
    /// leaves the same conflicts is dropped (a follower moving in lockstep
    /// with its predecessor would ask for more delay for ever; a round that
    /// would carry such a pair into a later stretch of more capacity is
    /// dropped the same way, a known limit: select the follower too), and a
    /// conflict that remains
    /// is reported as overallocated (a later pass resolves it only with the
    /// follower in its scope, or with `clear_first` off, since the default
    /// rebuilds the same cleared plan). Within slack a task whose own delay
    /// would move the project finish (a written delay stretches its
    /// summary's span, and a percentage lag on the summary's link grows
    /// with it) is held at its earliest start, overallocated, and the round
    /// runs again without it; a move no task explains alone or by its
    /// restoration drops the round. An ALAP task is never delayed nor
    /// booked (Project 2024 refuses a delay on one; its late dates float
    /// with the finish), so an overallocation it causes stays. Only the
    /// delays the leveler asks for are written: a task a constraint or
    /// another predecessor already holds takes none, as Project leaves it.
    /// With `clear_first` the targeted tasks' stored
    /// delays are dropped before the pass (a priority-1000 task's too, as
    /// measured on Project 2024; it then holds at its unleveled start),
    /// otherwise the pass builds on them. The reply reports the committed
    /// schedule. An unchanged plan records no step.
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
        let is_alap =
            |t: &crate::model::Task| t.constraint == crate::model::ConstraintType::AsLateAsPossible;
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
        // Each targeted leaf's intended values: its own delay (on top of a
        // kept one) and, with a delay, its unleveled dates.
        let pre_for = |uid: i32, value: Option<i64>| match value {
            Some(_) => {
                let pre = pre_base.get(uid);
                (pre.map(|r| r.early_start), pre.map(|r| r.early_finish))
            }
            None => (None, None),
        };
        // The pass writes each targeted leaf's own delay (the one the
        // leveler reports, on top of a kept one) and nothing else: the
        // leveler's placement is an estimate where a constraint or another
        // predecessor drives the task, and a delay the leveler did not ask
        // for would hold a task that had no conflict (Project leaves such a
        // task's LevelingDelay 0). A follower outside the scope moves with
        // its links once the delays are written and may land on a targeted
        // task, so the pass repeats on the plan with its delays written,
        // building on them, while a round still changes the overallocation:
        // a smaller overbooked extent, or conflicts the pass has not seen
        // (one traded for another, heavier or not, which a later round may
        // clear). A round that leaves the same conflicts, or ones seen
        // before, is dropped: a follower that moves in lockstep with its
        // predecessor (Start-to-Start, say) would ask for one more step for
        // ever (#1594 r6 to r8). Bounded by the finite conflict states, not
        // a count, and the pass ends on the lightest plan it reached.
        let mut trial = self.proj.clone();
        for t in &mut trial.tasks {
            if in_scope(t.uid) && options.clear_first && !is_alap(t) {
                t.leveling_delay = None;
            }
        }
        // The overallocation a plan holds on its committed schedule.
        let pressure = |p: &crate::model::Project| {
            let sched = schedule(p);
            let over = overallocation(p, |uid| {
                sched.get(uid).map(|r| (r.early_start, r.early_finish))
            });
            (over, sched.project_finish)
        };
        let (mut before, finish_at_start) = pressure(&trial);
        let mut seen = vec![before.conflicts.clone()];
        // The lightest plan the rounds reach: a trade may leave a heavier
        // conflict than it resolved, and the pass never ends worse than
        // its best.
        let mut best = (before.extent, trial.clone());
        // Within slack, the tasks whose own delay moved the project finish
        // (#1594 r15): held for the rounds that follow, so the round's
        // other conflicts still resolve.
        let mut pinned: HashSet<i32> = HashSet::new();
        loop {
            let scope: Option<HashSet<i32>> = if pinned.is_empty() {
                movable.clone()
            } else {
                let mut set = movable.clone().unwrap_or_else(|| {
                    trial
                        .tasks
                        .iter()
                        .filter(|t| is_leaf(t))
                        .map(|t| t.uid)
                        .collect()
                });
                set.retain(|uid| !pinned.contains(uid));
                Some(set)
            };
            let leveled = level_with(
                &trial,
                &LevelScope {
                    movable: scope.as_ref(),
                    order: options.order,
                    within_slack: options.within_slack,
                },
            );
            let mut round = trial.clone();
            // The rows that took an own delay this round.
            let mut moved: Vec<usize> = Vec::new();
            for (i, t) in round.tasks.iter_mut().enumerate() {
                if !is_leaf(t) || !in_scope(t.uid) || is_alap(t) || t.priority == Some(1000) {
                    continue;
                }
                let Some(own) = leveled.delay_min(t.uid).filter(|d| *d > 0) else {
                    continue;
                };
                let kept = t.leveling_delay.unwrap_or(0).max(0) / 10;
                t.leveling_delay = Some((kept + own) * 10);
                t.leveling_delay_format = Some(ELAPSED_DAYS_CODE);
                moved.push(i);
            }
            if moved.is_empty() {
                break;
            }
            let (after, finish) = pressure(&round);
            // Within slack the project finish never moves (#1594 r14): a
            // written delay stretches its summary's span, and a percentage
            // lag on the summary's link grows with it, past the slack the
            // leaf alone had. The tasks whose delay did it (each one whose
            // kept delay, restored, brings the finish back, or whose delay
            // alone moves it) are held and the round runs again without them
            // (r15, r16); a move no task explains alone or by its restoration
            // drops the round, and the held tasks stay at their earliest
            // start, overallocated, as the option promises.
            if options.within_slack && finish > finish_at_start {
                let culprits: Vec<i32> = moved
                    .iter()
                    .filter_map(|&i| {
                        // Two probes (r16): restored to its kept delay, does
                        // the finish come back (the delay was the one move)?
                        // Applied alone to the pass's plan, does its delay
                        // move the finish (one of several moves)? Either
                        // holds the task, so two tasks that each move the
                        // finish are both held and the rest still level.
                        let mut restored = round.clone();
                        restored.tasks[i].leveling_delay = trial.tasks[i].leveling_delay;
                        restored.tasks[i].leveling_delay_format =
                            trial.tasks[i].leveling_delay_format;
                        let mut alone = trial.clone();
                        alone.tasks[i].leveling_delay = round.tasks[i].leveling_delay;
                        alone.tasks[i].leveling_delay_format = round.tasks[i].leveling_delay_format;
                        (pressure(&restored).1 <= finish_at_start
                            || pressure(&alone).1 > finish_at_start)
                            .then_some(round.tasks[i].uid)
                    })
                    .collect();
                if culprits.is_empty() {
                    break;
                }
                pinned.extend(culprits);
                continue;
            }
            let resolves = after.extent < before.extent - 1e-6;
            // A trade goes on whatever its weight (#1594 r8): the follower
            // it uncovers may be heavier or longer than the conflict it
            // resolved, and the next round clears it; the unseen conflict
            // states bound the loop.
            let trades = after.conflicts != before.conflicts && !seen.contains(&after.conflicts);
            if !(resolves || trades) {
                break;
            }
            seen.push(after.conflicts.clone());
            trial = round;
            before = after;
            if before.extent < best.0 - 1e-6 {
                best = (before.extent, trial.clone());
            }
        }
        if before.extent > best.0 + 1e-6 {
            trial = best.1;
        }
        // What the trial holds for each targeted leaf, against the plan.
        let mut plan: Vec<DelayEdit> = Vec::new();
        for (i, t) in trial.tasks.iter().enumerate() {
            if !is_leaf(t) || !in_scope(t.uid) || is_alap(t) {
                continue;
            }
            let value = t.leveling_delay.filter(|v| *v > 0);
            let (pre_start, pre_finish) = pre_for(t.uid, value);
            plan.push(DelayEdit {
                row: i,
                value,
                pre_start,
                pre_finish,
            });
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
                overallocated: self.committed_overallocated(),
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
            overallocated: self.committed_overallocated(),
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

    /// Work resources booked past their capacity on the plan as committed
    /// (the schedule, not the display overlay, #1594 r4): what an edit's
    /// reply reports.
    pub fn committed_overallocated(&self) -> usize {
        let sched = self.schedule();
        overbookings(&self.proj, |uid| {
            sched.get(uid).map(|r| (r.early_start, r.early_finish))
        })
        .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leveling::{LevelOptions, LevelOrder, LevelTarget};
    use crate::model::{Assignment, LinkType, Predecessor, Resource, Task};

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

    /// #1594 r3: with only Paint selected on a chain Build, Paint, Trim (one
    /// day each on Ann, Trim after Paint), Trim is booked where the plan has
    /// it (Tuesday), so Paint levels past it to Wednesday, takes no delay of
    /// its own beyond that, and the committed plan moves Trim to Thursday
    /// after it: the targeted task lands where the leveler put it, the
    /// follower where its link drives it, and nothing is double-booked.
    #[test]
    fn a_held_successor_is_booked_where_the_plan_has_it() {
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
        assert_eq!((out.changed, out.delayed, out.overallocated), (1, 1, 0));
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(2 * 1440 * 10)
        );
        assert_eq!(ed.project().task(3).unwrap().leveling_delay, None);
        assert_eq!(start(&ed, 2), leveled.start(2).unwrap());
        assert_eq!(
            start(&ed, 2),
            DateTime::from_minutes(anchor.minutes() + 2 * 1440)
        );
        assert_eq!(
            start(&ed, 3),
            DateTime::from_minutes(anchor.minutes() + 3 * 1440)
        );
    }

    /// #1594 r6: a held follower that lands on a targeted task once the
    /// delays are written takes a second round: X, P, H (after P, outside
    /// the selection) and M, one day each on Ann from Monday, Level
    /// Selection P and M. Round one puts P on Wednesday (past X and H's
    /// Tuesday) and M on Thursday; written, H follows P to Thursday, onto
    /// M, and round two moves M to Friday. Nothing is double-booked.
    #[test]
    fn a_held_follower_landing_on_a_targeted_task_takes_a_second_round() {
        let mut p = staffed().project().clone();
        for t in &mut p.tasks {
            t.duration_min = 480;
        }
        // X = Build (1), P = Paint (2), H = Trim (3, after P); M is new.
        p.tasks[2].predecessors.push(Predecessor::fs(2));
        p.tasks.push(Task {
            uid: 4,
            id: 4,
            name: "M".into(),
            outline_level: 1,
            duration_min: 480,
            ..Task::default()
        });
        p.assignments.push(Assignment {
            uid: 4,
            task_uid: 4,
            resource_uid: 1,
            units: 1.0,
            ..Assignment::default()
        });
        let mut ed = Editor::new(p);
        let anchor = start(&ed, 1);
        let out = ed.level_tasks(&LevelTarget::Selection(vec![2, 4])).unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (2, 2, 0));
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(2 * 1440 * 10)
        );
        assert_eq!(
            ed.project().task(4).unwrap().leveling_delay,
            Some(4 * 1440 * 10),
            "M: three days in round one, one more in round two"
        );
        assert_eq!(
            start(&ed, 3),
            DateTime::from_minutes(anchor.minutes() + 3 * 1440)
        );
        assert_eq!(
            start(&ed, 4),
            DateTime::from_minutes(anchor.minutes() + 4 * 1440)
        );
    }

    /// #1594 r6: a follower that moves in lockstep with its predecessor
    /// keeps asking for more delay: M and H (one day each on Ann from
    /// Monday), H Start-to-Start after M, Level Selection M. A round would
    /// move M a day and H with it, resolving nothing; the pass sees that,
    /// writes no delay and reports the overallocation.
    #[test]
    fn a_lockstep_follower_ends_the_pass_without_a_useless_delay() {
        let mut p = staffed().project().clone();
        p.tasks.truncate(2);
        p.assignments.truncate(2);
        for t in &mut p.tasks {
            t.duration_min = 480;
        }
        p.tasks[1]
            .predecessors
            .push(Predecessor::working(1, LinkType::StartStart, 0));
        let mut ed = Editor::new(p);
        let depth = ed.undo_depth();
        let out = ed.level_tasks(&LevelTarget::Selection(vec![1])).unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (0, 0, 1));
        assert_eq!(ed.project().task(1).unwrap().leveling_delay, None);
        assert_eq!(ed.undo_depth(), depth, "nothing to record");
    }

    /// #1594 r6: a pass takes as many rounds as keep resolving something: P1
    /// to P7 (half an hour each, on R1 to R7) are selected; X (half an hour,
    /// R1) holds the project start, and each Hi (half an hour, R(i+1),
    /// outside the selection) starts with Pi. Every round settles one more
    /// stage and the seventh leaves nothing double-booked.
    #[test]
    fn the_pass_keeps_going_while_a_round_resolves_something() {
        let mut p = untitled_project();
        for r in 1..=7 {
            p.resources.push(Resource {
                uid: r,
                id: r,
                name: format!("R{r}"),
                max_units: 1.0,
                ..Resource::default()
            });
        }
        let mut next = 0;
        let mut add = |p: &mut crate::model::Project, name: &str, res: i32, pred: Option<i32>| {
            next += 1;
            let mut t = Task {
                uid: next,
                id: next,
                name: name.into(),
                outline_level: 1,
                duration_min: 30,
                ..Task::default()
            };
            if let Some(pr) = pred {
                t.predecessors
                    .push(Predecessor::working(pr, LinkType::StartStart, 0));
            }
            p.tasks.push(t);
            p.assignments.push(Assignment {
                uid: next,
                task_uid: next,
                resource_uid: res,
                units: 1.0,
                ..Assignment::default()
            });
            next
        };
        let x = add(&mut p, "X", 1, None);
        let ps: Vec<i32> = (1..=7)
            .map(|i| add(&mut p, &format!("P{i}"), i, None))
            .collect();
        for i in 1..=6 {
            add(&mut p, &format!("H{i}"), i + 1, Some(ps[i as usize - 1]));
        }
        let mut ed = Editor::new(p);
        let anchor = start(&ed, x);
        let out = ed.level_tasks(&LevelTarget::Selection(ps.clone())).unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (7, 7, 0));
        for (i, &uid) in ps.iter().enumerate() {
            let steps = i as i64 + 1;
            assert_eq!(
                start(&ed, uid),
                DateTime::from_minutes(anchor.minutes() + steps * 30),
                "P{} after {steps} rounds",
                i + 1
            );
        }
    }

    /// #1594 r7: a kept delay's own increment is measured so that the forward
    /// pass's one snap lands on the placement. From Thursday the 5th of
    /// March: B (one day on R2, no predecessor) keeps a stored two-elapsed-day
    /// delay (Saturday, snapped to Monday); Y (unselected) holds R2 on Monday.
    /// Level Selection B without clearing: B lands Tuesday with a delay of
    /// five elapsed days, and nothing is overbooked.
    #[test]
    fn a_kept_delays_increment_lands_the_task_where_it_was_placed() {
        let mut p = staffed().project().clone();
        p.start_date = Some(DateTime::from_ymd_hm(2026, 3, 5, 8, 0));
        p.tasks.truncate(2);
        p.assignments.truncate(2);
        for t in &mut p.tasks {
            t.duration_min = 480;
        }
        // Build = B with its stored delay; Paint = Y, pinned to Monday.
        p.tasks[0].leveling_delay = Some(2 * 1440 * 10);
        p.tasks[0].leveling_delay_format = Some(8);
        p.tasks[1].constraint = crate::model::ConstraintType::StartNoEarlierThan;
        p.tasks[1].constraint_date = Some(DateTime::from_ymd_hm(2026, 3, 9, 8, 0));
        let mut ed = Editor::new(p);
        assert_eq!(
            start(&ed, 1),
            DateTime::from_ymd_hm(2026, 3, 9, 8, 0),
            "Monday, on Y"
        );
        let out = ed
            .level_tasks_with(
                &LevelTarget::Selection(vec![1]),
                Some(&LevelOptions {
                    clear_first: false,
                    ..LevelOptions::default()
                }),
            )
            .unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (1, 1, 0));
        assert_eq!(
            ed.project().task(1).unwrap().leveling_delay,
            Some(5 * 1440 * 10),
            "Thursday to Tuesday"
        );
        assert_eq!(
            start(&ed, 1),
            DateTime::from_ymd_hm(2026, 3, 10, 8, 0),
            "Tuesday"
        );
    }

    /// #1594 r7: a round that resolves the selected conflict is kept although
    /// an untouched conflict elsewhere keeps the resource's peak: Ann on X
    /// and A (Monday) and on U and V (Friday). Level Selection A moves A to
    /// Tuesday; Friday's pair stays, reported.
    #[test]
    fn a_round_that_resolves_one_conflict_beside_another_is_kept() {
        let mut p = staffed().project().clone();
        for t in &mut p.tasks {
            t.duration_min = 480;
        }
        // Build = X, Paint = A, Trim = U (Friday); V is new, Friday too.
        let friday = DateTime::from_minutes(p.start_date.unwrap().minutes() + 4 * 1440);
        p.tasks[2].constraint = crate::model::ConstraintType::StartNoEarlierThan;
        p.tasks[2].constraint_date = Some(friday);
        p.tasks.push(Task {
            uid: 4,
            id: 4,
            name: "V".into(),
            outline_level: 1,
            duration_min: 480,
            constraint: crate::model::ConstraintType::StartNoEarlierThan,
            constraint_date: Some(friday),
            ..Task::default()
        });
        p.assignments.push(Assignment {
            uid: 4,
            task_uid: 4,
            resource_uid: 1,
            units: 1.0,
            ..Assignment::default()
        });
        let mut ed = Editor::new(p);
        let anchor = start(&ed, 1);
        let out = ed.level_tasks(&LevelTarget::Selection(vec![2])).unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (1, 1, 1));
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(1440 * 10)
        );
        assert_eq!(
            start(&ed, 2),
            DateTime::from_minutes(anchor.minutes() + 1440)
        );
        assert_eq!(start(&ed, 4), friday, "the Friday pair is untouched");
    }

    /// #1594 r7: a round that trades one conflict for another is kept, and
    /// the next round clears it: X, A, H (after A, unselected) and B (Start
    /// No Earlier Than Wednesday), one day each on Ann from Monday, Level
    /// Selection A and B. Round one moves A to Wednesday and B to Thursday;
    /// written, H follows A onto Thursday, over B; round two moves B to
    /// Friday.
    #[test]
    fn a_round_that_trades_a_conflict_for_another_is_kept_and_the_next_clears_it() {
        let mut p = staffed().project().clone();
        for t in &mut p.tasks {
            t.duration_min = 480;
        }
        let anchor = p.start_date.unwrap();
        let wednesday = DateTime::from_minutes(anchor.minutes() + 2 * 1440);
        // Build = X, Paint = A, Trim = H (after A); B is new.
        p.tasks[2].predecessors.push(Predecessor::fs(2));
        p.tasks.push(Task {
            uid: 4,
            id: 4,
            name: "B".into(),
            outline_level: 1,
            duration_min: 480,
            constraint: crate::model::ConstraintType::StartNoEarlierThan,
            constraint_date: Some(wednesday),
            ..Task::default()
        });
        p.assignments.push(Assignment {
            uid: 4,
            task_uid: 4,
            resource_uid: 1,
            units: 1.0,
            ..Assignment::default()
        });
        let mut ed = Editor::new(p);
        let out = ed.level_tasks(&LevelTarget::Selection(vec![2, 4])).unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (2, 2, 0));
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(2 * 1440 * 10),
            "A: Wednesday"
        );
        assert_eq!(
            ed.project().task(4).unwrap().leveling_delay,
            Some(2 * 1440 * 10),
            "B: a day in each round"
        );
        assert_eq!(
            start(&ed, 3),
            DateTime::from_minutes(anchor.minutes() + 3 * 1440),
            "H Thursday"
        );
        assert_eq!(
            start(&ed, 4),
            DateTime::from_minutes(anchor.minutes() + 4 * 1440),
            "B Friday"
        );
    }

    /// #1594 r8: a follower another predecessor still drives takes no
    /// delay and starts where that predecessor drives it, with nothing
    /// overbooked: X and A share Ann from Monday, B (three days, no
    /// resource) runs beside them, T follows A and B on Bob, D holds Bob on
    /// Friday. Level Selection X, A, B, T.
    #[test]
    fn a_follower_another_predecessor_still_drives_takes_no_delay() {
        let mut p = staffed().project().clone();
        for t in &mut p.tasks {
            t.duration_min = 480;
        }
        let anchor = p.start_date.unwrap();
        // Build = X, Paint = A, Trim = B (three days, no resource); T and D are new, on Bob.
        p.tasks[2].duration_min = 3 * 480;
        p.assignments.retain(|a| a.task_uid != 3);
        p.resources.push(Resource {
            uid: 2,
            id: 2,
            name: "Bob".into(),
            max_units: 1.0,
            ..Resource::default()
        });
        let mut t = Task {
            uid: 4,
            id: 4,
            name: "T".into(),
            outline_level: 1,
            duration_min: 480,
            ..Task::default()
        };
        t.predecessors.push(Predecessor::fs(2));
        t.predecessors.push(Predecessor::fs(3));
        p.tasks.push(t);
        p.tasks.push(Task {
            uid: 5,
            id: 5,
            name: "D".into(),
            outline_level: 1,
            duration_min: 480,
            constraint: crate::model::ConstraintType::StartNoEarlierThan,
            constraint_date: Some(DateTime::from_minutes(anchor.minutes() + 4 * 1440)),
            ..Task::default()
        });
        for uid in [4, 5] {
            p.assignments.push(Assignment {
                uid,
                task_uid: uid,
                resource_uid: 2,
                units: 1.0,
                ..Assignment::default()
            });
        }
        let mut ed = Editor::new(p);
        let out = ed
            .level_tasks(&LevelTarget::Selection(vec![1, 2, 3, 4]))
            .unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (1, 1, 0));
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(1440 * 10),
            "A: Tuesday"
        );
        assert_eq!(
            ed.project().task(4).unwrap().leveling_delay,
            None,
            "T: B drives it"
        );
        assert_eq!(
            start(&ed, 4),
            DateTime::from_minutes(anchor.minutes() + 3 * 1440),
            "Thursday"
        );
    }

    /// #1594 r16: two tasks whose delays each move the finish are both held,
    /// and the conflict that moves nothing still levels. The r15 plan
    /// doubled: S1 {A, B} (Ann) with C after S1 at a 100% lag, S2 {X, Y}
    /// (Bob) with Z after S2 at a 100% lag, D setting Thursday, and W, V
    /// (Carol, unlinked). Within slack V takes its day; B and Y (and A and X
    /// in their turn) are held; the finish stays Thursday.
    #[test]
    fn within_slack_holds_every_task_whose_delay_moves_the_finish() {
        let mut p = untitled_project();
        let anchor = p.start_date.unwrap();
        let day = |n: i64, min: i64| DateTime::from_minutes(anchor.minutes() + n * 1440 + min);
        let leaf = |uid: i32, name: &str, level: u32, minutes: i64| Task {
            uid,
            id: uid,
            name: name.into(),
            outline_level: level,
            duration_min: minutes,
            ..Task::default()
        };
        let percent_after = |uid: i32, name: &str, summary: i32| {
            let mut t = leaf(uid, name, 1, 480);
            t.predecessors.push(Predecessor {
                lag: 100,
                lag_format: crate::model::LagFormat::from_code(19).unwrap(),
                ..Predecessor::fs(summary)
            });
            t
        };
        p.tasks.push(Task {
            summary: true,
            ..leaf(1, "S1", 1, 0)
        });
        p.tasks.push(leaf(2, "A", 2, 480));
        p.tasks.push(leaf(3, "B", 2, 480));
        p.tasks.push(percent_after(4, "C", 1));
        p.tasks.push(Task {
            summary: true,
            ..leaf(5, "S2", 1, 0)
        });
        p.tasks.push(leaf(6, "X", 2, 480));
        p.tasks.push(leaf(7, "Y", 2, 480));
        p.tasks.push(percent_after(8, "Z", 5));
        p.tasks.push(leaf(9, "D", 1, 4 * 480));
        p.tasks.push(leaf(10, "W", 1, 480));
        p.tasks.push(leaf(11, "V", 1, 480));
        for (uid, name) in [(1, "Ann"), (2, "Bob"), (3, "Carol")] {
            p.resources.push(Resource {
                uid,
                id: uid,
                name: name.into(),
                max_units: 1.0,
                ..Resource::default()
            });
        }
        for (uid, res) in [(2, 1), (3, 1), (6, 2), (7, 2), (10, 3), (11, 3)] {
            p.assignments.push(Assignment {
                uid,
                task_uid: uid,
                resource_uid: res,
                units: 1.0,
                ..Assignment::default()
            });
        }
        let thursday = day(3, 9 * 60);
        let mut ed = Editor::new(p);
        assert_eq!(ed.schedule().project_finish, thursday);
        ed.set_level_options(&LevelOptions {
            within_slack: true,
            ..LevelOptions::default()
        })
        .unwrap();
        let out = ed
            .level_tasks(&LevelTarget::Selection(vec![2, 3, 6, 7, 10, 11]))
            .unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (1, 1, 2));
        assert_eq!(
            ed.project().task(11).unwrap().leveling_delay,
            Some(1440 * 10),
            "V takes its day"
        );
        for uid in [2, 3, 6, 7] {
            assert_eq!(
                ed.project().task(uid).unwrap().leveling_delay,
                None,
                "{uid} is held"
            );
        }
        assert_eq!(
            ed.schedule().project_finish,
            thursday,
            "the finish stays Thursday"
        );
    }

    /// #1594 r15: the task that moves the finish is held, the others still
    /// level. The r14 plan plus X and Y (one day each, Bob, unlinked):
    /// within slack B is held (its delay would move the finish through C's
    /// lag) while Y takes its day, so the finish stays Thursday and only
    /// B's conflict remains.
    #[test]
    fn within_slack_holds_only_the_task_that_would_move_the_finish() {
        let mut p = untitled_project();
        let anchor = p.start_date.unwrap();
        let day = |n: i64, min: i64| DateTime::from_minutes(anchor.minutes() + n * 1440 + min);
        let leaf = |uid: i32, name: &str, level: u32, minutes: i64| Task {
            uid,
            id: uid,
            name: name.into(),
            outline_level: level,
            duration_min: minutes,
            ..Task::default()
        };
        p.tasks.push(Task {
            summary: true,
            ..leaf(1, "S", 1, 0)
        });
        p.tasks.push(leaf(2, "A", 2, 480));
        p.tasks.push(leaf(3, "B", 2, 480));
        let mut c = leaf(4, "C", 1, 480);
        c.predecessors.push(Predecessor {
            lag: 100,
            lag_format: crate::model::LagFormat::from_code(19).unwrap(),
            ..Predecessor::fs(1)
        });
        p.tasks.push(c);
        p.tasks.push(leaf(5, "D", 1, 4 * 480));
        p.tasks.push(leaf(6, "X", 1, 480));
        p.tasks.push(leaf(7, "Y", 1, 480));
        for (uid, name) in [(1, "Ann"), (2, "Bob")] {
            p.resources.push(Resource {
                uid,
                id: uid,
                name: name.into(),
                max_units: 1.0,
                ..Resource::default()
            });
        }
        for (uid, res) in [(2, 1), (3, 1), (6, 2), (7, 2)] {
            p.assignments.push(Assignment {
                uid,
                task_uid: uid,
                resource_uid: res,
                units: 1.0,
                ..Assignment::default()
            });
        }
        let thursday = day(3, 9 * 60);
        let mut ed = Editor::new(p);
        ed.set_level_options(&LevelOptions {
            within_slack: true,
            ..LevelOptions::default()
        })
        .unwrap();
        let out = ed
            .level_tasks(&LevelTarget::Selection(vec![2, 3, 6, 7]))
            .unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (1, 1, 1));
        assert_eq!(
            ed.project().task(7).unwrap().leveling_delay,
            Some(1440 * 10),
            "Y takes its day"
        );
        assert_eq!(
            ed.project().task(3).unwrap().leveling_delay,
            None,
            "B is held"
        );
        assert_eq!(
            ed.schedule().project_finish,
            thursday,
            "the finish stays Thursday"
        );
    }

    /// #1594 r14: within slack the project finish never moves. Summary S
    /// holds A and B (one day each, Ann); C follows S with a 100% lag; D
    /// (four days) ends the project Thursday. B has a day of slack, but its
    /// delay would stretch S to two days, double C's lag and end the project
    /// Friday: B is held, A is held in turn when the rerun moves it instead,
    /// and both stay, overallocated. Without the option the same pass moves
    /// the finish.
    #[test]
    fn within_slack_never_moves_the_project_finish_through_a_summarys_percent_lag() {
        let mut p = untitled_project();
        let anchor = p.start_date.unwrap();
        let day = |n: i64, min: i64| DateTime::from_minutes(anchor.minutes() + n * 1440 + min);
        let leaf = |uid: i32, name: &str, level: u32, minutes: i64| Task {
            uid,
            id: uid,
            name: name.into(),
            outline_level: level,
            duration_min: minutes,
            ..Task::default()
        };
        p.tasks.push(Task {
            summary: true,
            ..leaf(1, "S", 1, 0)
        });
        p.tasks.push(leaf(2, "A", 2, 480));
        p.tasks.push(leaf(3, "B", 2, 480));
        let mut c = leaf(4, "C", 1, 480);
        c.predecessors.push(Predecessor {
            lag: 100,
            lag_format: crate::model::LagFormat::from_code(19).unwrap(),
            ..Predecessor::fs(1)
        });
        p.tasks.push(c);
        p.tasks.push(leaf(5, "D", 1, 4 * 480));
        p.resources.push(Resource {
            uid: 1,
            id: 1,
            name: "Ann".into(),
            max_units: 1.0,
            ..Resource::default()
        });
        for uid in [2, 3] {
            p.assignments.push(Assignment {
                uid,
                task_uid: uid,
                resource_uid: 1,
                units: 1.0,
                ..Assignment::default()
            });
        }
        let thursday = day(3, 9 * 60);
        let mut ed = Editor::new(p.clone());
        assert_eq!(ed.schedule().project_finish, thursday);
        ed.set_level_options(&LevelOptions {
            within_slack: true,
            ..LevelOptions::default()
        })
        .unwrap();
        let out = ed.level_tasks(&LevelTarget::Selection(vec![2, 3])).unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (0, 0, 1));
        assert_eq!(
            ed.project().task(3).unwrap().leveling_delay,
            None,
            "B stays"
        );
        assert_eq!(
            ed.schedule().project_finish,
            thursday,
            "the finish stays Thursday"
        );
        // Without the option the pass moves B and the finish.
        let mut ed = Editor::new(p);
        let out = ed.level_tasks(&LevelTarget::Selection(vec![2, 3])).unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (1, 1, 0));
        assert_eq!(ed.schedule().project_finish, day(4, 9 * 60), "Friday");
    }

    /// #1594 r13: a milestone's kept delay ends at the delay's snap, not at
    /// the evening it counts from. Build = X and Paint = A (one day each,
    /// Ann); M (a kept hour) follows A; T (one hour, Bob) follows M an
    /// elapsed hour later; Y (one hour, Bob, held) holds Wednesday 08:00. A
    /// levels to Tuesday, M is Tuesday 17:00 plus the hour, snapped to
    /// Wednesday 08:00, T starts at 09:00, after Y, with no delay of its own.
    #[test]
    fn a_kept_milestone_delay_ends_at_its_snap_in_the_plan() {
        let mut p = staffed().project().clone();
        for t in &mut p.tasks {
            t.duration_min = 480;
        }
        p.tasks.remove(2);
        p.assignments.retain(|a| a.task_uid != 3);
        let anchor = p.start_date.unwrap();
        let day = |n: i64, min: i64| DateTime::from_minutes(anchor.minutes() + n * 1440 + min);
        p.resources.push(Resource {
            uid: 2,
            id: 2,
            name: "Bob".into(),
            max_units: 1.0,
            ..Resource::default()
        });
        p.tasks.push(Task {
            uid: 4,
            id: 4,
            name: "M".into(),
            outline_level: 1,
            duration_min: 0,
            predecessors: vec![Predecessor::fs(2)],
            leveling_delay: Some(60 * 10),
            leveling_delay_format: Some(8),
            ..Task::default()
        });
        p.tasks.push(Task {
            uid: 5,
            id: 5,
            name: "T".into(),
            outline_level: 1,
            duration_min: 60,
            predecessors: vec![Predecessor {
                lag: 60,
                lag_format: crate::model::LagFormat::from_code(8).unwrap(),
                ..Predecessor::fs(4)
            }],
            ..Task::default()
        });
        p.tasks.push(Task {
            uid: 6,
            id: 6,
            name: "Y".into(),
            outline_level: 1,
            duration_min: 60,
            constraint: crate::model::ConstraintType::StartNoEarlierThan,
            constraint_date: Some(day(2, 0)),
            ..Task::default()
        });
        for uid in [5, 6] {
            p.assignments.push(Assignment {
                uid,
                task_uid: uid,
                resource_uid: 2,
                units: 1.0,
                ..Assignment::default()
            });
        }
        let mut ed = Editor::new(p);
        ed.set_level_options(&LevelOptions {
            clear_first: false,
            ..LevelOptions::default()
        })
        .unwrap();
        let out = ed
            .level_tasks(&LevelTarget::Selection(vec![2, 4, 5]))
            .unwrap();
        // A's new delay and M's kept one are the pass's two delays.
        assert_eq!((out.changed, out.delayed, out.overallocated), (2, 2, 0));
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(1440 * 10),
            "A: Tuesday"
        );
        assert_eq!(
            ed.project().task(5).unwrap().leveling_delay,
            None,
            "T: no delay of its own"
        );
        assert_eq!(start(&ed, 4), day(2, 0), "M Wednesday 08:00");
        assert_eq!(start(&ed, 5), day(2, 60), "T Wednesday 09:00, after Y");
    }

    /// #1594 r11: a kept delay on a milestone held at an evening instant.
    /// Build = X and Paint = A (one day each, Ann); Trim = Q (one day, no
    /// resource) follows A; M (a milestone, Start No Earlier Than Tuesday
    /// 17:00, a kept hour) follows Q start-to-start; T (one hour, Bob)
    /// follows M; Y (one hour, Bob, held) holds Wednesday 08:00. A levels
    /// to Tuesday and Q to Wednesday, whose 08:00 shares an index with M's
    /// Tuesday 17:00 and is the later instant: M is Wednesday 09:00, T
    /// starts after Y with no delay of its own, and the plan ends clean.
    #[test]
    fn an_evening_milestones_later_link_demand_on_the_same_index_takes_no_delay() {
        let mut p = staffed().project().clone();
        for t in &mut p.tasks {
            t.duration_min = 480;
        }
        let anchor = p.start_date.unwrap();
        let day = |n: i64, min: i64| DateTime::from_minutes(anchor.minutes() + n * 1440 + min);
        p.tasks[2].predecessors.push(Predecessor::fs(2));
        p.assignments.retain(|a| a.task_uid != 3);
        p.resources.push(Resource {
            uid: 2,
            id: 2,
            name: "Bob".into(),
            max_units: 1.0,
            ..Resource::default()
        });
        p.tasks.push(Task {
            uid: 4,
            id: 4,
            name: "M".into(),
            outline_level: 1,
            duration_min: 0,
            predecessors: vec![Predecessor {
                link: LinkType::StartStart,
                ..Predecessor::fs(3)
            }],
            constraint: crate::model::ConstraintType::StartNoEarlierThan,
            constraint_date: Some(day(1, 9 * 60)),
            leveling_delay: Some(60 * 10),
            leveling_delay_format: Some(8),
            ..Task::default()
        });
        let mut t = Task {
            uid: 5,
            id: 5,
            name: "T".into(),
            outline_level: 1,
            duration_min: 60,
            ..Task::default()
        };
        t.predecessors.push(Predecessor::fs(4));
        p.tasks.push(t);
        p.tasks.push(Task {
            uid: 6,
            id: 6,
            name: "Y".into(),
            outline_level: 1,
            duration_min: 60,
            constraint: crate::model::ConstraintType::StartNoEarlierThan,
            constraint_date: Some(day(2, 0)),
            ..Task::default()
        });
        for uid in [5, 6] {
            p.assignments.push(Assignment {
                uid,
                task_uid: uid,
                resource_uid: 2,
                units: 1.0,
                ..Assignment::default()
            });
        }
        let mut ed = Editor::new(p);
        assert_eq!(
            start(&ed, 4),
            day(2, 0),
            "M before: Tuesday 17:00 plus the hour"
        );
        ed.set_level_options(&LevelOptions {
            clear_first: false,
            ..LevelOptions::default()
        })
        .unwrap();
        let out = ed
            .level_tasks(&LevelTarget::Selection(vec![1, 2, 3, 4, 5]))
            .unwrap();
        // A's new delay and M's kept one are the pass's two delays.
        assert_eq!((out.changed, out.delayed, out.overallocated), (2, 2, 0));
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(1440 * 10),
            "A: Tuesday"
        );
        assert_eq!(
            ed.project().task(5).unwrap().leveling_delay,
            None,
            "T: no delay of its own"
        );
        assert_eq!(start(&ed, 4), day(2, 60), "M Wednesday 09:00");
        assert_eq!(start(&ed, 5), day(2, 60), "T Wednesday 09:00, after Y");
    }

    /// #1594 r11: a capped milestone whose cap and start-to-start demand
    /// share an index, through the editor. Build = X (two days, Ann), Paint
    /// = A (one day, Ann); M (Finish No Later Than Tuesday 17:00, a kept
    /// hour) follows A start-to-start; T (one hour, Bob) follows M; Y (one
    /// hour, Bob, held) holds Wednesday 09:00. A levels to Wednesday, M is
    /// held Tuesday 17:00 plus the hour, T starts Wednesday 08:00 and ends
    /// with no stored delay.
    #[test]
    fn a_capped_milestones_tie_leaves_its_follower_no_delay_in_the_plan() {
        let mut p = staffed().project().clone();
        p.tasks[0].duration_min = 2 * 480;
        p.tasks[1].duration_min = 480;
        p.tasks.remove(2);
        p.assignments.retain(|a| a.task_uid != 3);
        let anchor = p.start_date.unwrap();
        let day = |n: i64, min: i64| DateTime::from_minutes(anchor.minutes() + n * 1440 + min);
        p.resources.push(Resource {
            uid: 2,
            id: 2,
            name: "Bob".into(),
            max_units: 1.0,
            ..Resource::default()
        });
        p.tasks.push(Task {
            uid: 4,
            id: 4,
            name: "M".into(),
            outline_level: 1,
            duration_min: 0,
            predecessors: vec![Predecessor {
                link: LinkType::StartStart,
                ..Predecessor::fs(2)
            }],
            constraint: crate::model::ConstraintType::FinishNoLaterThan,
            constraint_date: Some(day(1, 9 * 60)),
            leveling_delay: Some(60 * 10),
            leveling_delay_format: Some(8),
            ..Task::default()
        });
        let mut t = Task {
            uid: 5,
            id: 5,
            name: "T".into(),
            outline_level: 1,
            duration_min: 60,
            ..Task::default()
        };
        t.predecessors.push(Predecessor::fs(4));
        p.tasks.push(t);
        p.tasks.push(Task {
            uid: 6,
            id: 6,
            name: "Y".into(),
            outline_level: 1,
            duration_min: 60,
            constraint: crate::model::ConstraintType::StartNoEarlierThan,
            constraint_date: Some(day(2, 60)),
            ..Task::default()
        });
        for uid in [5, 6] {
            p.assignments.push(Assignment {
                uid,
                task_uid: uid,
                resource_uid: 2,
                units: 1.0,
                ..Assignment::default()
            });
        }
        let mut ed = Editor::new(p);
        ed.set_level_options(&LevelOptions {
            clear_first: false,
            ..LevelOptions::default()
        })
        .unwrap();
        let out = ed
            .level_tasks(&LevelTarget::Selection(vec![2, 4, 5]))
            .unwrap();
        // A's new delay and M's kept one are the pass's two delays.
        assert_eq!((out.changed, out.delayed, out.overallocated), (2, 2, 0));
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(2880 * 10),
            "A: Wednesday"
        );
        assert_eq!(
            ed.project().task(5).unwrap().leveling_delay,
            None,
            "T: no stored delay"
        );
        assert_eq!(start(&ed, 4), day(2, 0), "M Wednesday 08:00");
        assert_eq!(start(&ed, 5), day(2, 0), "T Wednesday 08:00, before Y");
    }

    /// #1594 r8: a trade that uncovers a heavier conflict is kept and the
    /// next round clears it. X (50%) and A (100%), one hour each on Ann from
    /// Monday 08:00; H (unselected) follows A on Bob for an hour; B
    /// (selected) needs Bob for an hour from 10:00. A moves to 09:00; H
    /// follows onto B at 10:00, a heavier conflict than the one resolved;
    /// round two moves B to 11:00.
    #[test]
    fn a_trade_for_a_heavier_conflict_is_kept_and_cleared_next_round() {
        let mut p = staffed().project().clone();
        let anchor = p.start_date.unwrap();
        // Build = X (50%), Paint = A, Trim = H (after A, on Bob); B is new, on Bob from 10:00.
        for t in &mut p.tasks {
            t.duration_min = 60;
        }
        p.assignments[0].units = 0.5;
        p.tasks[2].predecessors.push(Predecessor::fs(2));
        p.assignments[2].resource_uid = 2;
        p.resources.push(Resource {
            uid: 2,
            id: 2,
            name: "Bob".into(),
            max_units: 1.0,
            ..Resource::default()
        });
        p.tasks.push(Task {
            uid: 4,
            id: 4,
            name: "B".into(),
            outline_level: 1,
            duration_min: 60,
            constraint: crate::model::ConstraintType::StartNoEarlierThan,
            constraint_date: Some(DateTime::from_minutes(anchor.minutes() + 120)),
            ..Task::default()
        });
        p.assignments.push(Assignment {
            uid: 4,
            task_uid: 4,
            resource_uid: 2,
            units: 1.0,
            ..Assignment::default()
        });
        let mut ed = Editor::new(p);
        let out = ed.level_tasks(&LevelTarget::Selection(vec![2, 4])).unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (2, 2, 0));
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(60 * 10),
            "A: 09:00"
        );
        assert_eq!(
            ed.project().task(4).unwrap().leveling_delay,
            Some(60 * 10),
            "B: 11:00"
        );
        assert_eq!(
            start(&ed, 3),
            DateTime::from_minutes(anchor.minutes() + 120),
            "H 10:00"
        );
        assert_eq!(
            start(&ed, 4),
            DateTime::from_minutes(anchor.minutes() + 180),
            "B 11:00"
        );
    }

    /// #1594 r8: an untouched conflict elsewhere does not hide a trade. U
    /// and V (unselected) overlap on Ann on Monday; X (unselected) and A
    /// (selected) start Tuesday; H (unselected) follows A; B (selected) is
    /// held to Thursday. Round one moves A to Thursday and B to Friday,
    /// where H lands on B; round two moves B past H. Monday's pair stays,
    /// reported.
    #[test]
    fn an_untouched_conflict_does_not_hide_a_trade() {
        let mut p = staffed().project().clone();
        let anchor = p.start_date.unwrap();
        let day = |n: i64| DateTime::from_minutes(anchor.minutes() + n * 1440);
        for t in &mut p.tasks {
            t.duration_min = 480;
        }
        // Build = U, Paint = V (both Monday), Trim = X (Tuesday); A, H, B are new.
        p.tasks[2].constraint = crate::model::ConstraintType::StartNoEarlierThan;
        p.tasks[2].constraint_date = Some(day(1));
        let mut a = Task {
            uid: 4,
            id: 4,
            name: "A".into(),
            outline_level: 1,
            duration_min: 480,
            constraint: crate::model::ConstraintType::StartNoEarlierThan,
            constraint_date: Some(day(1)),
            ..Task::default()
        };
        a.predecessors.clear();
        let mut h = Task {
            uid: 5,
            id: 5,
            name: "H".into(),
            outline_level: 1,
            duration_min: 480,
            ..Task::default()
        };
        h.predecessors.push(Predecessor::fs(4));
        let b = Task {
            uid: 6,
            id: 6,
            name: "B".into(),
            outline_level: 1,
            duration_min: 480,
            constraint: crate::model::ConstraintType::StartNoEarlierThan,
            constraint_date: Some(day(3)),
            ..Task::default()
        };
        p.tasks.extend([a, h, b]);
        for uid in [4, 5, 6] {
            p.assignments.push(Assignment {
                uid,
                task_uid: uid,
                resource_uid: 1,
                units: 1.0,
                ..Assignment::default()
            });
        }
        let mut ed = Editor::new(p);
        let out = ed.level_tasks(&LevelTarget::Selection(vec![4, 6])).unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (2, 2, 1));
        assert_eq!(start(&ed, 4), day(3), "A Thursday");
        assert_eq!(start(&ed, 5), day(4), "H Friday");
        assert_eq!(start(&ed, 6), day(7), "B the next Monday, past H");
        assert_eq!(start(&ed, 1), anchor, "U untouched");
        assert_eq!(start(&ed, 2), anchor, "V untouched");
    }

    /// #1594 r4: a follower whose own constraint already holds it takes no
    /// delay: Trim (after Paint, Start No Earlier Than Wednesday, on Bob,
    /// who is free) stays Wednesday with `LevelingDelay` none when Paint
    /// moves to Tuesday, within slack or not; only Paint is delayed.
    #[test]
    fn a_follower_held_by_its_constraint_takes_no_delay() {
        for within_slack in [false, true] {
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
            p.resources.push(Resource {
                uid: 2,
                id: 2,
                name: "Bob".into(),
                max_units: 1.0,
                ..Resource::default()
            });
            let mut ed = Editor::new(p);
            ed.set_level_options(&LevelOptions {
                within_slack,
                ..LevelOptions::default()
            })
            .unwrap();
            let out = ed.level_tasks(&LevelTarget::All).unwrap();
            assert_eq!(
                (out.delayed, out.overallocated),
                (1, 0),
                "within {within_slack}"
            );
            assert_eq!(
                ed.project().task(2).unwrap().leveling_delay,
                Some(1440 * 10)
            );
            assert_eq!(ed.project().task(3).unwrap().leveling_delay, None);
            assert_eq!(start(&ed, 3), wednesday);
        }
    }

    /// #1594 r4: a movable task after a held follower of a delayed task is
    /// not refused: Q and P share Ann, H (no resource, outside the scope)
    /// follows P, M on Ann follows H. Level Resource Ann delays P a day, H
    /// and M follow their links, M takes no delay and nothing is refused.
    #[test]
    fn a_movable_task_after_a_held_follower_is_not_refused() {
        let mut p = staffed().project().clone();
        for t in &mut p.tasks {
            t.duration_min = 480;
        }
        // Build = Q, Paint = P, Trim = H (after P, no resource); M is new.
        p.tasks[2].predecessors.push(Predecessor::fs(2));
        p.assignments.retain(|a| a.task_uid != 3);
        let mut m = Task {
            uid: 4,
            id: 4,
            name: "M".into(),
            outline_level: 1,
            duration_min: 480,
            ..Task::default()
        };
        m.predecessors.push(Predecessor::fs(3));
        p.tasks.push(m);
        p.assignments.push(Assignment {
            uid: 4,
            task_uid: 4,
            resource_uid: 1,
            units: 1.0,
            ..Assignment::default()
        });
        let mut ed = Editor::new(p);
        let anchor = start(&ed, 1);
        let out = ed.level_tasks(&LevelTarget::Resource(1)).unwrap();
        assert_eq!((out.changed, out.delayed, out.overallocated), (1, 1, 0));
        assert_eq!(
            ed.project().task(2).unwrap().leveling_delay,
            Some(1440 * 10)
        );
        assert_eq!(ed.project().task(4).unwrap().leveling_delay, None);
        assert_eq!(
            start(&ed, 2),
            DateTime::from_minutes(anchor.minutes() + 1440)
        );
        assert_eq!(
            start(&ed, 4),
            DateTime::from_minutes(anchor.minutes() + 3 * 1440)
        );
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

    /// #1594 r3: an ALAP task is neither delayed nor booked by a pass: Build
    /// and Paint level between themselves, Trim (ALAP) floats onto the new
    /// finish over Paint, and the overallocation that leaves is reported, as
    /// Project reports it cannot level an ALAP task.
    #[test]
    fn an_alap_task_is_not_leveled() {
        let mut p = staffed().project().clone();
        p.tasks[2].constraint = crate::model::ConstraintType::AsLateAsPossible;
        let mut ed = Editor::new(p);
        let anchor = start(&ed, 1);
        let out = ed.level_tasks(&LevelTarget::All).unwrap();
        assert_eq!((out.delayed, out.overallocated), (1, 1));
        assert_eq!(ed.project().task(3).unwrap().leveling_delay, None);
        assert_eq!(start(&ed, 1), anchor);
        assert!(start(&ed, 2) >= finish(&ed, 1));
        // Still as late as possible: its start is the schedule's late start.
        let trim = ed.schedule().get(3).unwrap();
        assert_eq!(trim.early_start, trim.late_start);
        assert_eq!(trim.early_start, start(&ed, 2), "over Paint");
    }
}
