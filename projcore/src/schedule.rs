//! Critical Path Method scheduler over working-time calendars.
//!
//! The engine that makes `projcore` a scheduler rather than a data model: given
//! task durations, dependency links, and calendars, it computes each task's
//! early/late start and finish, its slack, and whether it lies on the critical
//! path — the classic CPM forward and backward passes.
//!
//! ## Calendar arithmetic via a working-minute index
//!
//! Wall-clock scheduling is awkward because 5pm Friday + 1 working hour is 9am
//! Monday. We tame it by mapping each calendar to a **working-minute timeline**:
//! a monotonic function from an instant to "working minutes elapsed since the
//! timeline origin" (`to_index`) and its inverse (`abs_start`/`abs_finish`).
//! Once in index space, "finish = start + duration",
//! "successor = predecessor + lag", and slack are all plain integer arithmetic;
//! we only convert back to a [`DateTime`] at the end. Each distinct calendar
//! gets its own timeline with a shared origin before the project start, so
//! cross-calendar dependencies still compare correctly in wall-clock space.
//!
//! ## Scope (v1)
//!
//! Leaf tasks schedule via FS/SS/FF/SF links with lag, ASAP by default, honoring
//! date constraints. By default MSO/MFO/FNLT/SNLT override conflicting links;
//! disabling HonorConstraints lets the links delay those tasks instead. Both
//! modes report link conflicts as negative total slack within the timeline's
//! bounded horizon. Constraints before the project start do not pull unlinked
//! tasks before it. Summary tasks roll up from their descendants. Resource
//! leveling is separate from CPM. Free slack is computed precisely for
//! finish-to-start successors and falls back to total slack otherwise.

use crate::datetime::DateTime;
use crate::model::{ConstraintType, LinkType, Project, ResourceType, Task};
use std::collections::HashMap;

/// Maximum calendar days on either side of the scheduling anchor.
pub(crate) const HORIZON_DAYS: i64 = 366 * 100;
pub(crate) const HORIZON_PADDING_MIN: i64 = 200 * 480 + 480;

/// Computed schedule for one task.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TaskResult {
    pub uid: i32,
    pub early_start: DateTime,
    pub early_finish: DateTime,
    pub late_start: DateTime,
    pub late_finish: DateTime,
    /// Total slack in working minutes. Late start minus the later of scheduled
    /// and link-driven start, so overriding a link still exposes the conflict.
    /// ≤ 0 ⇒ critical.
    pub total_slack_min: i64,
    /// Free slack in working minutes (delay possible without moving any
    /// successor), floored at zero. For leaves, uses the minimum gap to FS
    /// successors, falling back to total slack when there are no FS successors.
    /// For summaries, uses total slack floored at zero.
    pub free_slack_min: i64,
    pub critical: bool,
}

/// The whole computed schedule, addressable by task UID.
#[derive(Clone, Debug)]
pub struct Schedule {
    results: HashMap<i32, TaskResult>,
    pub project_start: DateTime,
    pub project_finish: DateTime,
}

impl Schedule {
    pub fn get(&self, uid: i32) -> Option<&TaskResult> {
        self.results.get(&uid)
    }

    /// Every task's computed result, in unspecified order.
    pub fn results(&self) -> impl Iterator<Item = &TaskResult> {
        self.results.values()
    }
}

// ---- working-minute timeline ------------------------------------------------

#[derive(Clone, Copy)]
struct Seg {
    start: i64, // absolute minute, inclusive
    end: i64,   // absolute minute, exclusive
}

/// A single calendar's working time, including time before the project start,
/// expressed as absolute working-minute segments with cumulative offsets.
struct Timeline {
    segs: Vec<Seg>,
    cum: Vec<i64>, // cum[i] = working minutes before segs[i]
    total: i64,
}

impl Timeline {
    /// Reserve working days before the anchor for leads, finish-driven starts,
    /// and late dates under conflicting constraints. Empty calendars need none.
    fn origin(week: &[Vec<(u32, u32)>; 7], anchor: i64, min_before: i64) -> i64 {
        if min_before <= 0 || week.iter().flatten().all(|&(from, to)| from >= to) {
            return anchor;
        }
        let mut day = anchor.div_euclid(1440);
        let mut work = 0;
        for _ in 0..HORIZON_DAYS {
            day -= 1;
            let dow = (day + 4).rem_euclid(7) as usize;
            work += week[dow]
                .iter()
                .map(|&(from, to)| to.saturating_sub(from) as i64)
                .sum::<i64>();
            if work >= min_before {
                break;
            }
        }
        day * 1440
    }

    /// Build a timeline for `week` (Sun=0..Sat=6 working patterns) starting at
    /// `origin_abs`, extended until it covers at least `min_total` working
    /// minutes after `anchor_abs` and reaches wall-clock minute `min_reach`.
    fn build(
        week: &[Vec<(u32, u32)>; 7],
        origin_abs: i64,
        anchor_abs: i64,
        min_total: i64,
        min_reach: i64,
    ) -> Timeline {
        let mut segs = Vec::new();
        let mut cum = Vec::new();
        let mut total = 0i64;
        let mut day = origin_abs.div_euclid(1440);
        let origin_mod = origin_abs.rem_euclid(1440) as u32;
        let mut forward_total = 0;
        let mut first = true;
        let mut guard = 0;
        loop {
            let dow = (day + 4).rem_euclid(7) as usize;
            let floor = if first { origin_mod } else { 0 };
            first = false;
            for &(from, to) in &week[dow] {
                let from = from.max(floor);
                if from < to {
                    let s = day * 1440 + from as i64;
                    let e = day * 1440 + to as i64;
                    segs.push(Seg { start: s, end: e });
                    cum.push(total);
                    total += e - s;
                    forward_total += (e - s.max(anchor_abs)).max(0);
                }
            }
            day += 1;
            if day * 1440 > anchor_abs {
                guard += 1;
            }
            let reached = day * 1440;
            if (forward_total >= min_total && reached >= min_reach) || guard > HORIZON_DAYS {
                break;
            }
        }
        Timeline { segs, cum, total }
    }

    /// Working minutes strictly before wall-clock instant `abs`. An instant in a
    /// non-working gap yields the cumulative total up to the previous segment
    /// (so a finish at 17:00 and the next start at 08:00 share an index).
    fn to_index(&self, abs: i64) -> i64 {
        for (i, s) in self.segs.iter().enumerate() {
            if abs <= s.start {
                return self.cum[i];
            }
            if abs < s.end {
                return self.cum[i] + (abs - s.start);
            }
        }
        self.total
    }

    /// Inverse of [`to_index`] for a **start** instant: the wall-clock instant
    /// `k` working minutes after the origin. A `k` on a segment boundary maps to
    /// the *next* segment's start (the next working morning), not the end of the
    /// gap — a task that starts there starts at 08:00.
    fn abs_start(&self, k: i64) -> i64 {
        let k = k.clamp(0, self.total);
        for (i, s) in self.segs.iter().enumerate() {
            let len = s.end - s.start;
            if k < self.cum[i] + len {
                return s.start + (k - self.cum[i]);
            }
        }
        self.segs.last().map(|s| s.end).unwrap_or(0)
    }

    /// Inverse of [`to_index`] for a **finish** instant. A `k` on a segment
    /// boundary maps to the *end* of the current segment (17:00), not the next
    /// morning — the last working minute completes there.
    fn abs_finish(&self, k: i64) -> i64 {
        let k = k.clamp(0, self.total);
        for (i, s) in self.segs.iter().enumerate() {
            let len = s.end - s.start;
            if k <= self.cum[i] + len {
                return s.start + (k - self.cum[i]);
            }
        }
        self.segs.last().map(|s| s.end).unwrap_or(0)
    }

    /// Snap an instant forward to the nearest working **start** instant.
    fn snap(&self, abs: i64) -> i64 {
        self.abs_start(self.to_index(abs))
    }
}

/// A scheduler bound to a project: owns one timeline per calendar and the
/// derived link/order structures.
struct Scheduler<'a> {
    proj: &'a Project,
    timelines: HashMap<i32, Timeline>,
    default_cal: i32,
    anchor: i64,
}

struct ConstraintDates {
    start: i64,
    finish: i64,
    floor: i64,
    late_floor: Option<i64>,
    finish_bound: Option<i64>,
}

impl<'a> Scheduler<'a> {
    fn new(proj: &'a Project) -> Scheduler<'a> {
        // Anchor: explicit project start, else earliest stored or pinned start,
        // else a fixed Monday, snapped to the default calendar's first working
        // instant.
        let default_cal = proj.default_calendar_uid;
        let pinned_starts = proj
            .tasks
            .iter()
            .filter(|t| !t.summary)
            .filter_map(|t| t.pinned_dates().map(|(start, _)| start.minutes()));
        let raw_anchor = proj
            .start_date
            .or_else(|| {
                proj.tasks
                    .iter()
                    .flat_map(|t| [t.stored_start, t.pinned_dates().map(|(start, _)| start)])
                    .flatten()
                    .min()
            })
            .unwrap_or_else(|| DateTime::from_ymd_hm(2020, 1, 6, 8, 0))
            .minutes();

        // Horizon: enough working minutes for all work + lag, plus a wide
        // margin, and enough wall-clock reach to cover any far constraint date.
        let work: i64 = proj.tasks.iter().map(|t| t.duration_min.max(0)).sum();
        let lag: i64 = proj
            .tasks
            .iter()
            .flat_map(|t| &t.predecessors)
            .map(|p| p.lag_min.abs())
            .sum();
        let min_total = work + lag + HORIZON_PADDING_MIN;
        // A pinned task's duration-derived finish lies past its start; reserve
        // Standard-calendar wall-clock reach (about 4.2 minutes per working
        // minute) for it, with the margin below covering sparser calendars.
        let pinned_reach = proj.tasks.iter().filter_map(|t| {
            t.pinned_dates()
                .map(|(start, _)| start.minutes() + t.duration_min.max(0) * 5)
        });
        let far_dates = proj
            .tasks
            .iter()
            .flat_map(|t| [t.constraint_date, t.stored_finish, t.manual_finish])
            .flatten()
            .map(|d| d.minutes())
            .chain(pinned_reach)
            .max()
            .unwrap_or(raw_anchor);
        let min_reach = far_dates.max(raw_anchor) + 90 * 1440;

        let leaf_uids: std::collections::HashSet<_> = proj
            .tasks
            .iter()
            .filter(|t| !t.summary)
            .map(|t| t.uid)
            .collect();
        let used_calendars: std::collections::HashSet<_> = proj
            .tasks
            .iter()
            .filter(|t| !t.summary)
            .filter_map(|t| t.calendar_uid)
            .chain(std::iter::once(default_cal))
            .collect();
        let backward_constraints = proj
            .tasks
            .iter()
            .filter(|t| !t.summary && t.constraint_date.is_some())
            .filter(|t| {
                matches!(
                    t.constraint,
                    ConstraintType::FinishNoLaterThan
                        | ConstraintType::StartNoLaterThan
                        | ConstraintType::MustFinishOn
                        | ConstraintType::MustStartOn
                )
            });
        let has_backward_constraints = backward_constraints.clone().next().is_some();
        // An unlinked deadline is floored at the anchor. Its raw date must not
        // add an unused prefix to every calendar's timeline.
        // A pinned start can precede the anchor, and a violated link into it
        // puts its predecessors' late dates earlier still.
        let earliest_backward_date = backward_constraints
            .filter(|t| t.predecessors.iter().any(|p| leaf_uids.contains(&p.uid)))
            .filter_map(|t| t.constraint_date)
            .map(|date| date.minutes())
            .chain(pinned_starts.clone())
            .min();
        let needs_backward_horizon = has_backward_constraints
            || pinned_starts.clone().next().is_some()
            || proj.tasks.iter().filter(|t| !t.summary).any(|t| {
                t.predecessors.iter().any(|p| {
                    leaf_uids.contains(&p.uid)
                        && (p.lag_min < 0
                            || matches!(p.link, LinkType::StartFinish | LinkType::FinishFinish))
                })
            });

        // Only schedulable calendars affect the horizon. Include the default
        // fallback. Constraints may put late dates before the anchor and, when
        // honored on linked tasks, scheduled dates as well.
        let mut weeks: HashMap<_, _> = proj
            .calendars
            .iter()
            .filter(|cal| used_calendars.contains(&cal.uid))
            .map(|cal| (cal.uid, week_pairs(cal)))
            .collect();
        weeks
            .entry(default_cal)
            .or_insert_with(|| week_pairs(&crate::model::Calendar::standard(default_cal)));
        let origin = if needs_backward_horizon {
            // Late predecessors need the whole work/lag budget before the
            // earliest backward constraint, even when it predates the anchor.
            // Keep the absolute cap tied to the project start, not that date.
            let earliest_origin = (raw_anchor.div_euclid(1440) - HORIZON_DAYS) * 1440;
            let backward_anchor = earliest_backward_date
                .unwrap_or(raw_anchor)
                .min(raw_anchor)
                .max(earliest_origin);
            // Calendar changes between dependency hops can consume gaps beyond
            // work + lag. Reuse the forward padding as a bounded mitigation;
            // an exact per-hop cross-calendar budget is not modeled here.
            weeks
                .values()
                .map(|week| Timeline::origin(week, backward_anchor, min_total).max(earliest_origin))
                .min()
                .unwrap_or(raw_anchor)
        } else {
            raw_anchor
        };

        // Build a timeline per calendar, retaining the full forward horizon.
        let mut timelines = HashMap::new();
        let mut anchor = raw_anchor;
        for (uid, week) in weeks {
            let tl = Timeline::build(&week, origin, raw_anchor, min_total, min_reach);
            if uid == default_cal && tl.total > 0 {
                anchor = tl.snap(raw_anchor);
            }
            timelines.insert(uid, tl);
        }

        Scheduler {
            proj,
            timelines,
            default_cal,
            anchor,
        }
    }

    fn tl(&self, task: &Task) -> &Timeline {
        let uid = task.calendar_uid.unwrap_or(self.default_cal);
        self.timelines
            .get(&uid)
            .or_else(|| self.timelines.get(&self.default_cal))
            .expect("default timeline always present")
    }

    /// Normalize dates identically for CPM and leveling. Only unlinked tasks
    /// retain the project-start floor; linked tasks can use the full horizon.
    fn constraint_dates(&self, task: &Task, linked: bool) -> Option<ConstraintDates> {
        let tl = self.tl(task);
        let floor = if linked {
            tl.abs_start(0)
        } else {
            tl.snap(self.anchor)
        };
        let raw_date = task.constraint_date?.minutes();
        let date = raw_date.max(floor);
        let finish = tl.abs_finish(tl.to_index(date)).max(floor);
        Some(ConstraintDates {
            start: tl.snap(date),
            finish,
            floor,
            // Preserve the legacy pre-start floor only when the unlinked
            // deadline itself was clamped. A post-start deadline can require
            // a pre-start late window to expose the task's negative slack.
            late_floor: (!linked && raw_date < floor).then_some(floor),
            // Keep an SF morning that already meets the actual deadline,
            // even when its working index also represents the prior evening.
            finish_bound: (self.proj.honor_constraints
                && matches!(
                    task.constraint,
                    ConstraintType::MustFinishOn | ConstraintType::FinishNoLaterThan
                ))
            .then_some(date.max(finish)),
        })
    }

    fn run(&self) -> Schedule {
        // Exclude unschedulable leaves before either pass so they cannot affect
        // dependencies, project finish, or summary dates with empty timelines.
        let leaves: Vec<usize> = (0..self.proj.tasks.len())
            .filter(|&i| !self.proj.tasks[i].summary && self.tl(&self.proj.tasks[i]).total > 0)
            .collect();
        let leaf_uids: std::collections::HashSet<i32> =
            leaves.iter().map(|&i| self.proj.tasks[i].uid).collect();
        let idx_of: HashMap<i32, usize> = leaves
            .iter()
            .map(|&i| (self.proj.tasks[i].uid, i))
            .collect();

        let order = topo_order(self.proj, &leaves, &leaf_uids, &idx_of);

        // Successors of each leaf (for backward pass + free slack).
        let mut succs: HashMap<i32, Vec<(i32, LinkType, i64)>> = HashMap::new();
        for &i in &leaves {
            let t = &self.proj.tasks[i];
            for p in &t.predecessors {
                if leaf_uids.contains(&p.uid) {
                    succs
                        .entry(p.uid)
                        .or_default()
                        .push((t.uid, p.link, p.lag_min));
                }
            }
        }

        // ---- forward pass: early start / early finish ----
        let mut es: HashMap<i32, i64> = HashMap::new(); // index space (own calendar)
        let mut ef: HashMap<i32, i64> = HashMap::new();
        let mut ef_abs: HashMap<i32, i64> = HashMap::new();
        let mut es_abs: HashMap<i32, i64> = HashMap::new();
        let mut driven_es: HashMap<i32, i64> = HashMap::new();
        let mut linked_tasks = std::collections::HashSet::new();
        for &i in &order {
            let t = &self.proj.tasks[i];
            let tl = self.tl(t);
            let mut linked_start: Option<i64> = None;
            let mut fs_milestone_start: Option<i64> = None;
            let mut sf_bounds = Vec::new();
            for p in &t.predecessors {
                let Some(&pf_abs) = ef_abs.get(&p.uid) else {
                    continue;
                };
                let Some(&ps_abs) = es_abs.get(&p.uid) else {
                    continue;
                };
                let cand = match p.link {
                    LinkType::FinishStart if t.duration_min == 0 => {
                        let instant = fs_milestone_instant(tl, pf_abs, p.lag_min);
                        fs_milestone_start =
                            Some(fs_milestone_start.map_or(instant, |s| s.max(instant)));
                        instant
                    }
                    LinkType::FinishStart => tl.abs_start(tl.to_index(pf_abs) + p.lag_min),
                    LinkType::StartStart => tl.abs_start(tl.to_index(ps_abs) + p.lag_min),
                    LinkType::FinishFinish => {
                        let cf = tl.abs_finish(tl.to_index(pf_abs) + p.lag_min);
                        tl.abs_start(tl.to_index(cf) - t.duration_min)
                    }
                    LinkType::StartFinish => {
                        let bound = sf_bound(tl, ps_abs, p.lag_min);
                        sf_bounds.push(bound);
                        tl.abs_start(bound.0 - t.duration_min)
                    }
                };
                linked_start = Some(linked_start.map_or(cand, |s| s.max(cand)));
            }
            // Only resolved leaf links may schedule a task before the anchor.
            let mut start_abs = linked_start.unwrap_or(self.anchor);
            let mut driven_start = start_abs;
            let mut finish_bound = None;
            if linked_start.is_some() {
                linked_tasks.insert(t.uid);
            }
            // A manual task stays where the user put it: links and constraints
            // never move it. Its start is kept unsnapped, as Project keeps it.
            // Only links drive its slack, so a violated link shows as negative
            // total slack and an unlinked task never does.
            if let Some((pinned_start, pinned_finish)) = t.pinned_dates() {
                let s_abs = pinned_start.minutes();
                let s_idx = tl.to_index(s_abs);
                let f_abs = match pinned_finish {
                    Some(finish) => finish.minutes().max(s_abs),
                    None => finish_instant(
                        t,
                        tl,
                        s_abs,
                        s_idx + t.duration_min,
                        std::iter::empty(),
                        None,
                    ),
                };
                driven_es.insert(t.uid, tl.to_index(linked_start.unwrap_or(s_abs)));
                es.insert(t.uid, s_idx);
                ef.insert(t.uid, tl.to_index(f_abs));
                es_abs.insert(t.uid, s_abs);
                ef_abs.insert(t.uid, f_abs);
                continue;
            }
            // Snap the constraint date as a start (next morning) or a finish
            // (this evening).
            if let Some(dates) = self.constraint_dates(t, linked_start.is_some()) {
                let ConstraintDates {
                    start: ds,
                    finish: df,
                    floor,
                    ..
                } = dates;
                finish_bound = dates.finish_bound;
                match t.constraint {
                    ConstraintType::MustStartOn => {
                        start_abs = if self.proj.honor_constraints {
                            ds
                        } else {
                            start_abs.max(ds)
                        };
                    }
                    ConstraintType::StartNoEarlierThan => start_abs = start_abs.max(ds),
                    ConstraintType::FinishNoEarlierThan => {
                        start_abs = start_abs
                            .max(tl.abs_start(tl.to_index(df) - t.duration_min).max(floor));
                    }
                    ConstraintType::MustFinishOn | ConstraintType::FinishNoLaterThan
                        if t.constraint == ConstraintType::MustFinishOn
                            || self.proj.honor_constraints =>
                    {
                        let previous_start = start_abs;
                        start_abs = tl.abs_start(tl.to_index(df) - t.duration_min);
                        // Floor the finish date, not the duration-derived start
                        // of a linked task. Unlinked tasks retain anchor semantics.
                        if linked_start.is_none() {
                            start_abs = start_abs.max(floor);
                        }
                        if t.constraint == ConstraintType::FinishNoLaterThan {
                            start_abs = start_abs.min(previous_start);
                        } else if !self.proj.honor_constraints {
                            start_abs = start_abs.max(previous_start);
                        }
                    }
                    ConstraintType::StartNoLaterThan if self.proj.honor_constraints => {
                        start_abs = start_abs.min(ds);
                    }
                    _ => {}
                }
                if matches!(
                    t.constraint,
                    ConstraintType::StartNoEarlierThan | ConstraintType::FinishNoEarlierThan
                ) {
                    driven_start = start_abs;
                }
            }
            // A binding FS milestone occupies the finish instant, even in a
            // nonworking gap. Later start-type links/constraints still snap.
            let s_abs = if fs_milestone_start == Some(start_abs) {
                start_abs
            } else {
                tl.snap(start_abs)
            };
            let s_idx = tl.to_index(s_abs);
            let f_idx = s_idx + t.duration_min;
            let f_abs = finish_instant(t, tl, s_abs, f_idx, sf_bounds.into_iter(), finish_bound);
            driven_es.insert(t.uid, tl.to_index(driven_start));
            es.insert(t.uid, s_idx);
            ef.insert(t.uid, f_idx);
            es_abs.insert(t.uid, s_abs);
            ef_abs.insert(t.uid, f_abs);
        }

        let project_finish_abs = ef_abs.values().copied().max().unwrap_or(self.anchor);

        // ---- backward pass: late finish / late start ----
        let mut lf_abs: HashMap<i32, i64> = HashMap::new();
        let mut ls_abs: HashMap<i32, i64> = HashMap::new();
        for &i in order.iter().rev() {
            let t = &self.proj.tasks[i];
            let tl = self.tl(t);
            // A manual task's working span comes from its pinned dates, which
            // need not match its duration, and its constraints are ignored.
            let pinned = t.pinned_dates().is_some();
            let span = if pinned {
                ef[&t.uid] - es[&t.uid]
            } else {
                t.duration_min
            };
            // Every task must finish by the project finish, even when an
            // SS/SF successor only bounds its start.
            let mut finish_abs = project_finish_abs;
            if let Some(list) = succs.get(&t.uid) {
                for &(suid, link, lag) in list {
                    let sls = ls_abs.get(&suid).copied();
                    let slf = lf_abs.get(&suid).copied();
                    let cand = match link {
                        // this.finish ≤ succ.late_start − lag
                        LinkType::FinishStart => sls.map(|x| tl.abs_finish(tl.to_index(x) - lag)),
                        // succ.start ≥ this.start + lag ⇒ bound this.start, then finish
                        LinkType::StartStart => sls.map(|x| {
                            let this_start = tl.abs_start(tl.to_index(x) - lag);
                            tl.abs_finish(tl.to_index(this_start) + span)
                        }),
                        // this.finish ≤ succ.late_finish − lag
                        LinkType::FinishFinish => slf.map(|x| tl.abs_finish(tl.to_index(x) - lag)),
                        LinkType::StartFinish => slf.map(|x| {
                            let this_start = tl.abs_start(tl.to_index(x) - lag);
                            tl.abs_finish(tl.to_index(this_start) + span)
                        }),
                    };
                    if let Some(c) = cand {
                        finish_abs = finish_abs.min(c);
                    }
                }
            }
            // Hard constraints (backward-affecting).
            let mut late_start_floor = None;
            let mut hard_finish_bound = false;
            if let Some(ConstraintDates {
                start: ds,
                finish: df,
                late_floor,
                ..
            }) = self
                .constraint_dates(t, linked_tasks.contains(&t.uid))
                .filter(|_| !pinned)
            {
                match t.constraint {
                    ConstraintType::MustFinishOn => {
                        finish_abs = df;
                        hard_finish_bound = true;
                        late_start_floor = late_floor;
                    }
                    ConstraintType::FinishNoLaterThan if df <= finish_abs => {
                        finish_abs = df;
                        hard_finish_bound = true;
                        late_start_floor = late_floor;
                    }
                    ConstraintType::StartNoLaterThan => {
                        let bound = tl.abs_finish(tl.to_index(ds) + t.duration_min);
                        if bound <= finish_abs {
                            finish_abs = bound;
                            hard_finish_bound = true;
                        }
                    }
                    ConstraintType::MustStartOn => {
                        finish_abs = tl.abs_finish(tl.to_index(ds) + t.duration_min);
                        hard_finish_bound = true;
                    }
                    _ => {}
                }
            }
            let finish_index = tl.to_index(finish_abs);
            let (s_abs, f_abs) = if finish_abs == ef_abs[&t.uid]
                || (finish_index == ef[&t.uid] && !hard_finish_bound)
            {
                // An SF endpoint (or milestone) can be the morning side of a
                // gap. Successor bounds can remap that index to the evening;
                // reuse the early instants unless a hard date set that bound.
                (es_abs[&t.uid], ef_abs[&t.uid])
            } else {
                let f_abs = tl
                    .abs_finish(finish_index)
                    .max(late_start_floor.unwrap_or(i64::MIN));
                let s_abs = if span == 0 {
                    f_abs
                } else {
                    tl.abs_start(finish_index - span)
                        .max(late_start_floor.unwrap_or(i64::MIN))
                };
                (s_abs, f_abs)
            };
            lf_abs.insert(t.uid, f_abs);
            ls_abs.insert(t.uid, s_abs);
        }

        // ---- assemble leaf results ----
        let mut results: HashMap<i32, TaskResult> = HashMap::new();
        for &i in &leaves {
            let t = &self.proj.tasks[i];
            let tl = self.tl(t);
            let e_s = es_abs[&t.uid];
            let e_f = ef_abs[&t.uid];
            let l_s = ls_abs[&t.uid];
            let l_f = lf_abs[&t.uid];
            // A constraint can move the scheduled start ahead of what its
            // links permit; preserve that conflict instead of reporting zero.
            let total = tl.to_index(l_s) - tl.to_index(e_s).max(driven_es[&t.uid]);
            let free = self.free_slack(t, tl, &es_abs, &succs);
            results.insert(
                t.uid,
                TaskResult {
                    uid: t.uid,
                    early_start: DateTime::from_minutes(e_s),
                    early_finish: DateTime::from_minutes(e_f),
                    late_start: DateTime::from_minutes(l_s),
                    late_finish: DateTime::from_minutes(l_f),
                    total_slack_min: total,
                    free_slack_min: free.unwrap_or(total).max(0),
                    critical: total <= 0,
                },
            );
        }

        // ---- summary rollup ----
        for (i, t) in self.proj.tasks.iter().enumerate() {
            if !t.summary {
                continue;
            }
            let kids = descendant_leaves(self.proj, i);
            let child: Vec<&TaskResult> = kids.iter().filter_map(|u| results.get(u)).collect();
            if child.is_empty() {
                continue;
            }
            let es_min = child.iter().map(|r| r.early_start).min().unwrap();
            let ef_max = child.iter().map(|r| r.early_finish).max().unwrap();
            let ls_min = child.iter().map(|r| r.late_start).min().unwrap();
            let lf_max = child.iter().map(|r| r.late_finish).max().unwrap();
            let total = child.iter().map(|r| r.total_slack_min).min().unwrap();
            results.insert(
                t.uid,
                TaskResult {
                    uid: t.uid,
                    early_start: es_min,
                    early_finish: ef_max,
                    late_start: ls_min,
                    late_finish: lf_max,
                    total_slack_min: total,
                    free_slack_min: total.max(0),
                    critical: child.iter().any(|r| r.critical),
                },
            );
        }

        Schedule {
            results,
            project_start: DateTime::from_minutes(self.anchor),
            project_finish: DateTime::from_minutes(project_finish_abs),
        }
    }

    /// Free slack: for finish-to-start successors, how long this task can slip
    /// before the earliest successor must move. `None` ⇒ fall back to total.
    fn free_slack(
        &self,
        t: &Task,
        tl: &Timeline,
        es_abs: &HashMap<i32, i64>,
        succs: &HashMap<i32, Vec<(i32, LinkType, i64)>>,
    ) -> Option<i64> {
        let list = succs.get(&t.uid)?;
        let ef_idx = tl.to_index(es_abs[&t.uid]) + t.duration_min;
        let mut min_gap: Option<i64> = None;
        for &(suid, link, lag) in list {
            if link != LinkType::FinishStart {
                continue;
            }
            let succ_es = *es_abs.get(&suid)?;
            let gap = tl.to_index(succ_es) - lag - ef_idx;
            min_gap = Some(min_gap.map_or(gap, |m: i64| m.min(gap)));
        }
        min_gap
    }
}

/// Zero-lag FS milestones preserve the actual finish, including across
/// calendars and SF morning endpoints. Nonzero lag uses the finish side of
/// the successor's calendar; only zero lag is verified against Project (#59).
fn fs_milestone_instant(tl: &Timeline, predecessor_finish: i64, lag: i64) -> i64 {
    if lag == 0 {
        predecessor_finish
    } else {
        tl.abs_finish(tl.to_index(predecessor_finish) + lag)
    }
}

/// SF at zero lag preserves the predecessor's actual start, including across
/// calendars. Nonzero lag uses the successor calendar's working-start mapping.
fn sf_bound(tl: &Timeline, predecessor_start: i64, lag: i64) -> (i64, i64) {
    let index = tl.to_index(predecessor_start) + lag;
    (
        index,
        if lag == 0 {
            predecessor_start
        } else {
            tl.abs_start(index)
        },
    )
}

fn finish_instant(
    task: &Task,
    tl: &Timeline,
    start: i64,
    finish_index: i64,
    sf_bounds: impl Iterator<Item = (i64, i64)>,
    finish_bound: Option<i64>,
) -> i64 {
    if task.duration_min == 0 {
        return start;
    }
    // A constraint's evening can share an index with an SF morning. Only
    // preserve SF instants allowed by the active finish bound; selecting an
    // instant never changes the task's working duration.
    if let Some(instant) = sf_bounds
        .filter(|&(index, instant)| {
            index == finish_index && finish_bound.is_none_or(|bound| instant <= bound)
        })
        .map(|(_, instant)| instant)
        .max()
    {
        return instant;
    }
    tl.abs_finish(finish_index)
}

/// Convert a calendar's weekday patterns into sorted `(from, to)` minute pairs.
fn week_pairs(cal: &crate::model::Calendar) -> [Vec<(u32, u32)>; 7] {
    let mut out: [Vec<(u32, u32)>; 7] = Default::default();
    for (d, day) in cal.week.iter().enumerate() {
        let mut v: Vec<(u32, u32)> = day.times.iter().map(|t| (t.from, t.to)).collect();
        v.sort_by_key(|&(f, _)| f);
        out[d] = v;
    }
    out
}

/// Reject tasks that have no working time and are leaves either in the stored
/// schedule or in the outline the editor uses to recompute summary flags.
pub(crate) fn calendar_error(proj: &Project) -> Option<String> {
    // Match Scheduler::new's last-wins calendar map and tl's default fallback.
    let calendars: HashMap<_, _> = proj
        .calendars
        .iter()
        .map(|cal| {
            let has_work = cal
                .week
                .iter()
                .flat_map(|day| &day.times)
                .any(|t| t.from < t.to);
            (cal.uid, (cal, has_work))
        })
        .collect();
    for (i, task) in proj.tasks.iter().enumerate() {
        if task.summary && proj.is_outline_summary(i) {
            continue;
        }
        let uid = task.calendar_uid.unwrap_or(proj.default_calendar_uid);
        let Some((cal, has_work)) = calendars
            .get(&uid)
            .or_else(|| calendars.get(&proj.default_calendar_uid))
        else {
            // The scheduler synthesizes Standard when the default is absent.
            continue;
        };
        if !has_work {
            return Some(format!(
                "calendar {:?} (UID {}) has no working time; task {:?} (UID {}) cannot be scheduled",
                cal.name, cal.uid, task.name, task.uid
            ));
        }
    }
    None
}

/// Kahn topological sort of leaf tasks by predecessor links; on a cycle, the
/// remaining tasks are appended in input order (best effort).
fn topo_order(
    proj: &Project,
    leaves: &[usize],
    leaf_uids: &std::collections::HashSet<i32>,
    idx_of: &HashMap<i32, usize>,
) -> Vec<usize> {
    let mut indeg: HashMap<i32, usize> = leaves.iter().map(|&i| (proj.tasks[i].uid, 0)).collect();
    let mut adj: HashMap<i32, Vec<i32>> = HashMap::new();
    for &i in leaves {
        let t = &proj.tasks[i];
        for p in &t.predecessors {
            if leaf_uids.contains(&p.uid) {
                adj.entry(p.uid).or_default().push(t.uid);
                *indeg.get_mut(&t.uid).unwrap() += 1;
            }
        }
    }
    let mut queue: Vec<i32> = leaves
        .iter()
        .map(|&i| proj.tasks[i].uid)
        .filter(|u| indeg[u] == 0)
        .collect();
    let mut order = Vec::new();
    let mut head = 0;
    while head < queue.len() {
        let u = queue[head];
        head += 1;
        order.push(idx_of[&u]);
        if let Some(next) = adj.get(&u) {
            for &v in next {
                let d = indeg.get_mut(&v).unwrap();
                *d -= 1;
                if *d == 0 {
                    queue.push(v);
                }
            }
        }
    }
    if order.len() < leaves.len() {
        let seen: std::collections::HashSet<usize> = order.iter().copied().collect();
        for &i in leaves {
            if !seen.contains(&i) {
                order.push(i);
            }
        }
    }
    order
}

/// UIDs of the leaf tasks nested under the summary at position `sidx` (those
/// following rows with a deeper outline level, until the level returns).
fn descendant_leaves(proj: &Project, sidx: usize) -> Vec<i32> {
    let level = proj.tasks[sidx].outline_level;
    let mut out = Vec::new();
    for t in &proj.tasks[sidx + 1..] {
        if t.outline_level <= level {
            break;
        }
        if !t.summary {
            out.push(t.uid);
        }
    }
    out
}

/// Schedule a project: run the CPM forward and backward passes and return the
/// computed [`Schedule`].
/// Leaves with no working time have no result. Readers reject files containing
/// such leaves, and structural editor operations validate newly exposed leaves.
pub fn schedule(proj: &Project) -> Schedule {
    Scheduler::new(proj).run()
}

/// Working minutes between two wall-clock instants under the project's default
/// calendar. Used when importing a file that stores computed wall-clock
/// start/finish (a `.mpp`) but not an explicit working-minute duration: the
/// duration is `working_minutes_between(start, finish)`.
pub fn working_minutes_between(proj: &Project, start: DateTime, finish: DateTime) -> i64 {
    let cal = proj
        .calendars
        .iter()
        .find(|c| c.uid == proj.default_calendar_uid)
        .cloned()
        .unwrap_or_else(|| crate::model::Calendar::standard(proj.default_calendar_uid));
    working_minutes_on(&cal, start, finish)
}

/// Working minutes between two wall-clock instants on one calendar, counted
/// through the same timeline the scheduler uses.
pub(crate) fn working_minutes_on(
    cal: &crate::model::Calendar,
    start: DateTime,
    finish: DateTime,
) -> i64 {
    let a = start.minutes().min(finish.minutes());
    let b = start.minutes().max(finish.minutes());
    let tl = Timeline::build(&week_pairs(cal), a, a, (b - a) + 480, b + 1440);
    (tl.to_index(b) - tl.to_index(a)).max(0)
}

// ---- resource leveling ------------------------------------------------------

/// The result of a resource-leveling pass: each task's leveled start/finish.
#[derive(Clone, Debug)]
pub struct Leveled {
    start: HashMap<i32, DateTime>,
    finish: HashMap<i32, DateTime>,
    pub project_finish: DateTime,
}

impl Leveled {
    pub fn start(&self, uid: i32) -> Option<DateTime> {
        self.start.get(&uid).copied()
    }
    pub fn finish(&self, uid: i32) -> Option<DateTime> {
        self.finish.get(&uid).copied()
    }
}

/// Resource-level a project: run CPM, then delay tasks so that no work resource
/// is booked beyond its capacity, never scheduling a task before its CPM early
/// start and never breaking a dependency (a predecessor's leveling delay is
/// propagated to its successors, preserving every link's gap).
///
/// v1 scope: a single-pass, topological-order serial leveler operating in the
/// default calendar's working-minute space; resource occupation is the task's
/// wall-clock span. It only ever moves tasks *later*. Multi-calendar leveling
/// and task splitting are out of scope.
/// If the default calendar has no working time, return the CPM dates unchanged.
pub fn level(proj: &Project) -> Leveled {
    Scheduler::new(proj).level()
}

/// Peak concurrent booked load over `[start, end)` (a sweep over interval ends).
fn max_load_in(bookings: &[(i64, i64, f64)], start: i64, end: i64) -> f64 {
    let mut events: Vec<(i64, f64)> = Vec::new();
    for &(s, e, u) in bookings {
        if s < end && e > start {
            events.push((s.max(start), u));
            events.push((e.min(end), -u));
        }
    }
    events.sort_by_key(|&(t, _)| t);
    let (mut load, mut peak) = (0.0f64, 0.0f64);
    for (_, d) in events {
        load += d;
        if load > peak {
            peak = load;
        }
    }
    peak
}

/// Earliest index ≥ `start` where adding `units` for `dur` keeps one resource
/// within `cap`.
fn earliest_feasible(
    bookings: &[(i64, i64, f64)],
    cap: f64,
    units: f64,
    start: i64,
    dur: i64,
) -> i64 {
    if dur <= 0 {
        return start;
    }
    let mut cand = start;
    loop {
        let end = cand + dur;
        if max_load_in(bookings, cand, end) + units <= cap + 1e-9 {
            return cand;
        }
        // jump to the earliest time a blocking interval frees, then retry
        let next = bookings
            .iter()
            .filter(|&&(s, e, _)| s < end && e > cand)
            .map(|&(_, e, _)| e)
            .filter(|&e| e > cand)
            .min();
        match next {
            Some(n) => cand = n,
            None => return cand,
        }
    }
}

/// Earliest index ≥ `start` feasible for *all* of a task's resources at once.
fn place_all(
    res: &[(i32, f64)],
    bookings: &HashMap<i32, Vec<(i64, i64, f64)>>,
    caps: &HashMap<i32, f64>,
    start: i64,
    dur: i64,
) -> i64 {
    let mut cand = start;
    loop {
        let mut next: Option<i64> = None;
        for &(rid, units) in res {
            let bk = bookings.get(&rid).map(|v| v.as_slice()).unwrap_or(&[]);
            let cap = caps.get(&rid).copied().unwrap_or(1.0);
            let c = earliest_feasible(bk, cap, units, cand, dur);
            if c > cand {
                next = Some(next.map_or(c, |n: i64| n.min(c)));
            }
        }
        match next {
            None => return cand,
            Some(n) => cand = n,
        }
    }
}

impl Scheduler<'_> {
    fn level(&self) -> Leveled {
        let base = self.run();
        let tl = self
            .timelines
            .get(&self.default_cal)
            .expect("default timeline present");

        if tl.total == 0 {
            return Leveled {
                start: base.results().map(|r| (r.uid, r.early_start)).collect(),
                finish: base.results().map(|r| (r.uid, r.early_finish)).collect(),
                project_finish: base.project_finish,
            };
        }

        let leaves: Vec<usize> = (0..self.proj.tasks.len())
            .filter(|&i| !self.proj.tasks[i].summary && self.tl(&self.proj.tasks[i]).total > 0)
            .collect();
        let leaf_uids: std::collections::HashSet<i32> =
            leaves.iter().map(|&i| self.proj.tasks[i].uid).collect();
        let idx_of: HashMap<i32, usize> = leaves
            .iter()
            .map(|&i| (self.proj.tasks[i].uid, i))
            .collect();
        let order = topo_order(self.proj, &leaves, &leaf_uids, &idx_of);

        // Work-resource capacities and per-task assignments.
        let mut caps: HashMap<i32, f64> = HashMap::new();
        for r in &self.proj.resources {
            if r.kind == ResourceType::Work {
                caps.insert(r.uid, if r.max_units > 0.0 { r.max_units } else { 1.0 });
            }
        }
        let mut assign: HashMap<i32, Vec<(i32, f64)>> = HashMap::new();
        for a in &self.proj.assignments {
            if caps.contains_key(&a.resource_uid) {
                assign
                    .entry(a.task_uid)
                    .or_default()
                    .push((a.resource_uid, if a.units > 0.0 { a.units } else { 1.0 }));
            }
        }

        let mut delay: HashMap<i32, i64> = HashMap::new();
        let mut bookings: HashMap<i32, Vec<(i64, i64, f64)>> = HashMap::new();
        let mut start: HashMap<i32, DateTime> = HashMap::new();
        let mut finish: HashMap<i32, DateTime> = HashMap::new();

        // Manual tasks never move, so book them before placing anything else:
        // auto tasks earlier in topological order must level around them.
        for &i in &order {
            let t = &self.proj.tasks[i];
            if t.pinned_dates().is_none() {
                continue;
            }
            let cpm = base.get(t.uid).expect("leaf scheduled");
            let s_idx = tl.to_index(cpm.early_start.minutes());
            let f_idx = tl.to_index(cpm.early_finish.minutes());
            for (rid, units) in assign.get(&t.uid).into_iter().flatten() {
                bookings
                    .entry(*rid)
                    .or_default()
                    .push((s_idx, f_idx, *units));
            }
        }

        for &i in &order {
            let t = &self.proj.tasks[i];
            let cpm = base.get(t.uid).expect("leaf scheduled");
            // A manual task keeps its pinned dates and passes no delay on.
            if t.pinned_dates().is_some() {
                delay.insert(t.uid, 0);
                start.insert(t.uid, cpm.early_start);
                finish.insert(t.uid, cpm.early_finish);
                continue;
            }
            let cpm_start_idx = tl.to_index(cpm.early_start.minutes());
            // Preserve every link's gap by inheriting the largest predecessor delay.
            let floor = t
                .predecessors
                .iter()
                .filter_map(|p| delay.get(&p.uid).copied())
                .max()
                .unwrap_or(0);
            let earliest = cpm_start_idx + floor;
            let res = assign.get(&t.uid).cloned().unwrap_or_default();
            let placed = place_all(&res, &bookings, &caps, earliest, t.duration_min);
            delay.insert(t.uid, placed - cpm_start_idx);
            for (rid, units) in &res {
                bookings
                    .entry(*rid)
                    .or_default()
                    .push((placed, placed + t.duration_min, *units));
            }
            let (s_abs, f_abs) = if placed == cpm_start_idx && floor == 0 {
                (cpm.early_start.minutes(), cpm.early_finish.minutes())
            } else {
                let mut s_abs = tl.abs_start(placed);
                if t.duration_min == 0 {
                    let mut fs_instant: Option<i64> = None;
                    let mut start_bound = cpm.early_start.minutes();
                    for p in &t.predecessors {
                        let (Some(ps), Some(pf)) = (start.get(&p.uid), finish.get(&p.uid)) else {
                            continue;
                        };
                        if p.link == LinkType::FinishStart {
                            let instant = fs_milestone_instant(tl, pf.minutes(), p.lag_min);
                            if tl.to_index(instant) == placed {
                                fs_instant = Some(fs_instant.map_or(instant, |s| s.max(instant)));
                            }
                        } else {
                            // SS/SF use the predecessor's start; FF its finish.
                            // Zero-duration successors map all three to starts.
                            let endpoint = if p.link == LinkType::FinishFinish {
                                pf
                            } else {
                                ps
                            };
                            let index = tl.to_index(endpoint.minutes()) + p.lag_min;
                            if index == placed {
                                start_bound = start_bound.max(tl.abs_start(index));
                            }
                        }
                    }
                    // Leveling can change which link binds. Choose the actual
                    // leveled FS instant unless a current non-FS bound or the
                    // absolute CPM floor requires a later instant. That floor
                    // already carries the task's SNET/MSO start bounds.
                    if let Some(instant) = fs_instant {
                        s_abs = instant.max(start_bound);
                    }
                }
                let bounds = t
                    .predecessors
                    .iter()
                    .filter(|p| p.link == LinkType::StartFinish)
                    .filter_map(|p| {
                        start
                            .get(&p.uid)
                            .map(|s| sf_bound(tl, s.minutes(), p.lag_min))
                    });
                (
                    s_abs,
                    finish_instant(
                        t,
                        tl,
                        s_abs,
                        placed + t.duration_min,
                        bounds,
                        self.constraint_dates(
                            t,
                            t.predecessors.iter().any(|p| start.contains_key(&p.uid)),
                        )
                        .and_then(|dates| dates.finish_bound),
                    ),
                )
            };
            start.insert(t.uid, DateTime::from_minutes(s_abs));
            finish.insert(t.uid, DateTime::from_minutes(f_abs));
        }

        // Roll leveled dates up into summary tasks.
        for (i, t) in self.proj.tasks.iter().enumerate() {
            if !t.summary {
                continue;
            }
            let kids = descendant_leaves(self.proj, i);
            let cs: Vec<DateTime> = kids.iter().filter_map(|u| start.get(u).copied()).collect();
            let cf: Vec<DateTime> = kids.iter().filter_map(|u| finish.get(u).copied()).collect();
            if let (Some(&s), Some(&f)) = (cs.iter().min(), cf.iter().max()) {
                start.insert(t.uid, s);
                finish.insert(t.uid, f);
            }
        }

        let project_finish = finish
            .values()
            .map(|d| d.minutes())
            .max()
            .map(DateTime::from_minutes)
            .unwrap_or(base.project_finish);

        Leveled {
            start,
            finish,
            project_finish,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn task(uid: i32, name: &str, days_min: i64) -> Task {
        Task {
            uid,
            id: uid,
            name: name.into(),
            outline_level: 1,
            duration_min: days_min,
            ..Task::default()
        }
    }
    fn fs(uid: i32) -> Predecessor {
        Predecessor {
            uid,
            link: LinkType::FinishStart,
            lag_min: 0,
        }
    }

    fn fs_milestone_project(duration: i64) -> Project {
        let mut milestone = task(2, "Sign-off", 0);
        milestone.predecessors.push(fs(1));
        Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![task(1, "A", duration), milestone],
            ..Project::default()
        }
    }

    #[test]
    fn fs_milestone_lands_at_predecessor_finish_instant() {
        for (duration, day, hour) in [(960, 3, 17), (240, 2, 12), (2400, 6, 17)] {
            let proj = fs_milestone_project(duration);
            let sched = schedule(&proj);
            let milestone = sched.get(2).unwrap();
            let expected = DateTime::from_ymd_hm(2026, 3, day, hour, 0);
            assert_eq!(milestone.early_start, expected);
            assert_eq!(milestone.early_finish, expected);
            assert_eq!(milestone.late_start, expected);
            assert_eq!(milestone.late_finish, expected);
            assert_eq!(milestone.total_slack_min, 0);
            assert_eq!(milestone.free_slack_min, 0);
            assert!(milestone.critical);
            let leveled = level(&proj);
            assert_eq!(leveled.start(2), Some(expected));
            assert_eq!(leveled.finish(2), Some(expected));
        }
    }

    #[test]
    fn fs_milestone_chain_matches_project() {
        let mut proj = fs_milestone_project(480);
        let mut b = task(3, "B", 2400);
        b.predecessors.push(fs(2));
        let mut m2 = task(4, "M2", 0);
        m2.predecessors.push(fs(3));
        proj.tasks.extend([b, m2]);
        let sched = schedule(&proj);
        let leveled = level(&proj);
        for (uid, start, finish) in [
            (2, "2026-03-02T17:00:00", "2026-03-02T17:00:00"),
            (3, "2026-03-03T08:00:00", "2026-03-09T17:00:00"),
            (4, "2026-03-09T17:00:00", "2026-03-09T17:00:00"),
        ] {
            let result = sched.get(uid).unwrap();
            assert_eq!(result.early_start.to_mspdi(), start);
            assert_eq!(result.early_finish.to_mspdi(), finish);
            assert_eq!(result.total_slack_min, 0);
            assert!(result.critical);
            assert_eq!(result.late_start, result.early_start);
            assert_eq!(result.late_finish, result.early_finish);
            assert_eq!(leveled.start(uid), Some(result.early_start));
            assert_eq!(leveled.finish(uid), Some(result.early_finish));
        }
    }

    #[test]
    fn fs_milestone_with_later_start_driver_stays_morning() {
        let morning = DateTime::from_ymd_hm(2026, 3, 4, 8, 0);
        for constraint in [
            ConstraintType::StartNoEarlierThan,
            ConstraintType::MustStartOn,
        ] {
            for honor in [false, true] {
                let mut proj = fs_milestone_project(960);
                proj.honor_constraints = honor;
                proj.tasks[1].constraint = constraint;
                proj.tasks[1].constraint_date = Some(morning);
                let sched = schedule(&proj);
                assert_eq!(sched.get(2).unwrap().early_start, morning);
                assert_eq!(sched.get(2).unwrap().early_finish, morning);
                assert_eq!(level(&proj).start(2), Some(morning));
            }
        }
        let mut proj = fs_milestone_project(960);
        let mut r = task(3, "Start driver", 480);
        r.predecessors.push(fs(1));
        proj.tasks[1].predecessors.push(Predecessor {
            uid: 3,
            link: LinkType::StartStart,
            lag_min: 0,
        });
        proj.tasks.push(r);
        let sched = schedule(&proj);
        assert_eq!(sched.get(2).unwrap().early_start, morning);
        assert_eq!(sched.get(2).unwrap().early_finish, morning);
    }

    #[test]
    fn fs_milestone_takes_latest_fs_instant() {
        for reverse in [false, true] {
            let mut proj = fs_milestone_project(240);
            proj.tasks.push(task(3, "Later finish", 960));
            proj.tasks[1].predecessors.push(fs(3));
            if reverse {
                proj.tasks[1].predecessors.reverse();
            }
            let sched = schedule(&proj);
            assert_eq!(
                sched.get(2).unwrap().early_start,
                sched.get(3).unwrap().early_finish
            );
            assert_eq!(
                sched.get(2).unwrap().early_finish,
                sched.get(3).unwrap().early_finish
            );
        }
    }

    #[test]
    fn fs_milestone_zero_lag_keeps_sf_preserved_morning_finish() {
        let mut proj = sf_project();
        let mut milestone = task(3, "Sign-off", 0);
        milestone.predecessors.push(fs(2));
        proj.tasks.push(milestone);
        let sched = schedule(&proj);
        let expected = DateTime::from_ymd_hm(2026, 3, 4, 8, 0);
        assert_eq!(sched.get(2).unwrap().early_finish, expected);
        assert_eq!(sched.get(3).unwrap().early_start, expected);
        assert_eq!(sched.get(3).unwrap().early_finish, expected);
        proj.tasks.insert(0, task(4, "Busy resource", 1920));
        proj.resources = vec![worker(1, "Shared", 1.0)];
        proj.assignments = vec![assign(1, 4, 1, 1.0), assign(2, 1, 1, 1.0)];
        let leveled = level(&proj);
        let delayed = DateTime::from_ymd_hm(2026, 3, 6, 8, 0);
        assert_eq!(leveled.finish(2), Some(delayed));
        assert_eq!(leveled.start(3), Some(delayed));
        assert_eq!(leveled.finish(3), Some(delayed));
    }

    #[test]
    fn fs_milestone_cross_calendar_keeps_predecessor_instant() {
        let mut proj = fs_milestone_project(960);
        let mut shorter = Calendar::standard(2);
        for day in &mut shorter.week {
            if !day.times.is_empty() {
                day.times = vec![WorkingTime {
                    from: 9 * 60,
                    to: 16 * 60,
                }];
            }
        }
        proj.calendars.push(shorter);
        proj.tasks[1].calendar_uid = Some(2);
        let sched = schedule(&proj);
        let expected = DateTime::from_ymd_hm(2026, 3, 3, 17, 0);
        assert_eq!(sched.get(2).unwrap().early_start, expected);
        assert_eq!(sched.get(2).unwrap().early_finish, expected);
        assert_eq!(level(&proj).start(2), Some(expected));
    }

    #[test]
    fn fs_milestone_with_lag() {
        // Only zero lag has been checked against Project; nonzero lag uses
        // the finish side of the successor calendar's working-time boundary.
        for (lag, day) in [(480, 4), (-480, 2)] {
            let mut proj = fs_milestone_project(960);
            proj.tasks[1].predecessors[0].lag_min = lag;
            let sched = schedule(&proj);
            let expected = DateTime::from_ymd_hm(2026, 3, day, 17, 0);
            assert_eq!(sched.get(2).unwrap().early_start, expected);
            assert_eq!(sched.get(2).unwrap().early_finish, expected);
        }
    }

    fn delayed_fs_milestone_project() -> Project {
        let mut proj = fs_milestone_project(480);
        proj.tasks.insert(0, task(4, "Busy resource", 480));
        proj.resources = vec![worker(1, "Shared", 1.0)];
        proj.assignments = vec![assign(1, 4, 1, 1.0), assign(2, 1, 1, 1.0)];
        proj
    }

    #[test]
    fn leveling_delayed_fs_milestone_lands_at_leveled_finish() {
        for (lag, day) in [(0, 3), (480, 4), (-480, 2)] {
            let mut proj = delayed_fs_milestone_project();
            proj.tasks[2].predecessors[0].lag_min = lag;
            let leveled = level(&proj);
            let expected = DateTime::from_ymd_hm(2026, 3, day, 17, 0);
            assert_eq!(
                leveled.finish(1),
                Some(DateTime::from_ymd_hm(2026, 3, 3, 17, 0))
            );
            assert_eq!(leveled.start(2), Some(expected));
            assert_eq!(leveled.finish(2), Some(expected));
        }
    }

    #[test]
    fn leveling_fs_milestone_overtakes_original_constraint_driver() {
        for constraint in [
            ConstraintType::StartNoEarlierThan,
            ConstraintType::MustStartOn,
        ] {
            for honor in [false, true] {
                let mut proj = delayed_fs_milestone_project();
                proj.honor_constraints = honor;
                proj.tasks[2].constraint = constraint;
                proj.tasks[2].constraint_date = Some(DateTime::from_ymd_hm(2026, 3, 3, 8, 0));
                // The unchanged date constraint is now earlier than A's
                // leveled finish, which selects the milestone's instant.
                let leveled = level(&proj);
                let expected = DateTime::from_ymd_hm(2026, 3, 3, 17, 0);
                assert_eq!(leveled.start(2), Some(expected));
                assert_eq!(leveled.finish(2), Some(expected));
            }
        }
    }

    #[test]
    fn leveling_fs_milestone_overtakes_undelayed_ss_driver() {
        let mut proj = delayed_fs_milestone_project();
        let q = task(5, "Independent predecessor", 480);
        let mut r = task(3, "Undelayed start driver", 480);
        r.predecessors.push(fs(5));
        proj.tasks.extend([q, r]);
        proj.tasks[2].predecessors.push(Predecessor {
            uid: 3,
            link: LinkType::StartStart,
            lag_min: 0,
        });
        let sched = schedule(&proj);
        let morning = DateTime::from_ymd_hm(2026, 3, 3, 8, 0);
        assert_eq!(sched.get(2).unwrap().early_start, morning);
        let leveled = level(&proj);
        let evening = DateTime::from_ymd_hm(2026, 3, 3, 17, 0);
        assert_eq!(leveled.start(3), Some(morning));
        assert_eq!(leveled.finish(1), Some(evening));
        assert_eq!(leveled.start(2), Some(evening));
        assert_eq!(leveled.finish(2), Some(evening));
        assert!(leveled.start(2).unwrap() >= sched.get(2).unwrap().early_start);
    }

    #[test]
    fn leveling_fs_milestone_respects_later_ss_driver() {
        let mut proj = delayed_fs_milestone_project();
        let mut r = task(3, "Start driver", 480);
        r.predecessors.push(fs(1));
        proj.tasks.push(r);
        proj.tasks[2].predecessors.push(Predecessor {
            uid: 3,
            link: LinkType::StartStart,
            lag_min: 0,
        });
        let sched = schedule(&proj);
        assert_eq!(
            sched.get(2).unwrap().early_start,
            DateTime::from_ymd_hm(2026, 3, 3, 8, 0)
        );
        let leveled = level(&proj);
        let expected = DateTime::from_ymd_hm(2026, 3, 4, 8, 0);
        assert_eq!(leveled.start(3), Some(expected));
        assert_eq!(leveled.start(2), Some(expected));
        assert_eq!(leveled.finish(2), Some(expected));
        assert!(leveled.start(2).unwrap() >= sched.get(2).unwrap().early_start);
    }

    fn closed_calendar(uid: i32) -> Calendar {
        Calendar {
            uid,
            name: "Closed".into(),
            week: Default::default(),
        }
    }

    #[test]
    fn twenty_four_hour_calendar_matches_project() {
        let mut cal = Calendar::standard(3);
        cal.name = "24 Hours".into();
        for day in &mut cal.week {
            day.times = vec![WorkingTime { from: 0, to: 1440 }];
        }
        let mut build = task(1, "Build", 3 * 480);
        build.calendar_uid = Some(3);
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![build],
            calendars: vec![Calendar::standard(1), cal],
            ..Project::default()
        };
        let sched = schedule(&proj);
        let result = sched.get(1).unwrap();
        assert_eq!(result.early_start.to_mspdi(), "2026-03-02T08:00:00");
        assert_eq!(result.early_finish.to_mspdi(), "2026-03-03T08:00:00");
    }

    #[test]
    fn calendar_validation_matches_scheduler_resolution() {
        let cases = [
            (Some(999), vec![closed_calendar(1)], true),
            (Some(999), vec![closed_calendar(3)], false),
            (Some(3), vec![closed_calendar(3)], true),
            (None, vec![], false),
            (
                Some(3),
                vec![closed_calendar(3), Calendar::standard(3)],
                false,
            ),
            (
                Some(3),
                vec![Calendar::standard(3), closed_calendar(3)],
                true,
            ),
            (None, vec![closed_calendar(1), Calendar::standard(1)], false),
            (None, vec![Calendar::standard(1), closed_calendar(1)], true),
        ];
        for (uid, calendars, invalid) in cases {
            let mut build = task(1, "Build", 480);
            build.calendar_uid = uid;
            let proj = Project {
                tasks: vec![build],
                calendars,
                ..Project::default()
            };
            assert_eq!(calendar_error(&proj).is_some(), invalid);
            let engine = Scheduler::new(&proj);
            assert_eq!(engine.tl(&proj.tasks[0]).total == 0, invalid);
            assert_eq!(engine.run().get(1).is_none(), invalid);
        }
    }

    #[test]
    fn duplicate_uid_cannot_resurrect_a_leaf_on_an_empty_calendar() {
        let valid = task(5, "Valid", 480);
        let mut invalid = task(5, "Closed", 960);
        invalid.calendar_uid = Some(3);
        let mut proj = Project {
            tasks: vec![valid.clone()],
            calendars: vec![Calendar::standard(1), closed_calendar(3)],
            ..Project::default()
        };
        let expected = schedule(&proj);
        for tasks in [vec![valid.clone(), invalid.clone()], vec![invalid, valid]] {
            proj.tasks = tasks;
            let actual = schedule(&proj);
            assert_eq!(actual.get(5), expected.get(5));
            assert_eq!(actual.project_finish, expected.project_finish);
            let leveled = level(&proj);
            assert_eq!(leveled.start(5), Some(expected.get(5).unwrap().early_start));
            assert_eq!(
                leveled.finish(5),
                Some(expected.get(5).unwrap().early_finish)
            );
        }
    }

    #[test]
    fn empty_calendar_leaves_do_not_affect_dependencies_or_summaries() {
        let mut summary = task(10, "Phase", 0);
        summary.summary = true;
        let mut invalid = task(2, "Closed", 480);
        invalid.calendar_uid = Some(3);
        invalid.predecessors.push(fs(1));
        let mut successor = task(4, "After closed", 480);
        // Also exercise the backward-horizon path with an unresolved SF link.
        successor.predecessors.push(Predecessor {
            uid: 2,
            link: LinkType::StartFinish,
            lag_min: -60,
        });
        let mut proj = Project {
            tasks: vec![summary, task(1, "Valid", 480), invalid, successor],
            calendars: vec![Calendar::standard(1), closed_calendar(3)],
            ..Project::default()
        };
        for t in &mut proj.tasks[1..] {
            t.outline_level = 2;
        }
        let actual = schedule(&proj);
        let leveled = level(&proj);
        assert!(actual.get(2).is_none());
        assert!(leveled.start(2).is_none());
        assert!(leveled.finish(2).is_none());
        proj.tasks.retain(|t| t.uid != 2);
        for t in &mut proj.tasks {
            t.predecessors.retain(|p| p.uid != 2);
        }
        let expected = schedule(&proj);
        let expected_leveled = level(&proj);
        assert_eq!(actual.project_start, expected.project_start);
        assert_eq!(actual.project_finish, expected.project_finish);
        for uid in [1, 4, 10] {
            assert_eq!(actual.get(uid), expected.get(uid));
            assert_eq!(leveled.start(uid), expected_leveled.start(uid));
            assert_eq!(leveled.finish(uid), expected_leveled.finish(uid));
        }
    }

    #[test]
    fn empty_default_preserves_anchor_with_no_schedulable_leaves() {
        let anchor = DateTime::from_ymd_hm(2026, 3, 2, 8, 0);
        for tasks in [
            vec![],
            vec![task(1, "Closed", 480)],
            vec![Task {
                summary: true,
                ..task(1, "Summary", 0)
            }],
        ] {
            let proj = Project {
                start_date: Some(anchor),
                tasks,
                calendars: vec![closed_calendar(1)],
                ..Project::default()
            };
            let sched = schedule(&proj);
            assert_eq!(sched.results().count(), 0);
            assert_eq!(sched.project_start, anchor);
            assert_eq!(sched.project_finish, anchor);
            assert_eq!(level(&proj).project_finish, anchor);
        }
    }

    #[test]
    fn unused_empty_default_loads_and_leveling_preserves_cpm() {
        let anchor = DateTime::from_ymd_hm(2026, 3, 2, 8, 0);
        let mut summary = task(10, "Phase", 0);
        summary.summary = true;
        let mut proj = Project {
            start_date: Some(anchor),
            calendars: vec![closed_calendar(1), Calendar::standard(3)],
            tasks: vec![summary, task(1, "A", 480), task(2, "B", 480)],
            resources: vec![Resource {
                uid: 1,
                max_units: 1.0,
                ..Resource::default()
            }],
            ..Project::default()
        };
        for t in &mut proj.tasks[1..] {
            t.calendar_uid = Some(3);
            t.outline_level = 2;
            proj.assignments.push(Assignment {
                uid: t.uid,
                task_uid: t.uid,
                resource_uid: 1,
                units: 1.0,
                work_min: 480,
            });
        }
        let proj = crate::mspdi::read_mspdi(&crate::mspdi::write_mspdi(&proj)).unwrap();
        let sched = schedule(&proj);
        assert_eq!(sched.project_start, anchor);
        let leveled = level(&proj);
        for uid in [1, 2, 10] {
            let r = sched.get(uid).unwrap();
            assert_eq!(r.early_start, anchor);
            assert_eq!(r.early_finish, DateTime::from_ymd_hm(2026, 3, 2, 17, 0));
            assert_eq!(leveled.start(uid), Some(r.early_start));
            assert_eq!(leveled.finish(uid), Some(r.early_finish));
        }
        assert_eq!(leveled.project_finish, sched.project_finish);
    }

    #[test]
    fn ss_predecessor_finishing_last_is_critical() {
        let a = task(1, "A", 960);
        let mut b = task(2, "B", 480);
        b.predecessors.push(Predecessor {
            uid: 1,
            link: LinkType::StartStart,
            lag_min: 0,
        });
        let proj = Project {
            tasks: vec![a, b],
            ..Project::default()
        };
        let sched = schedule(&proj);
        let a = sched.get(1).unwrap();
        assert_eq!(a.early_finish, sched.project_finish);
        assert_eq!(a.total_slack_min, 0);
        assert!(a.critical);
    }

    fn sf_project() -> Project {
        let mut a = task(1, "A", 960);
        a.constraint = ConstraintType::StartNoEarlierThan;
        a.constraint_date = Some(DateTime::from_ymd_hm(2026, 3, 4, 8, 0));
        let mut b = task(2, "B", 480);
        b.predecessors.push(Predecessor {
            uid: 1,
            link: LinkType::StartFinish,
            lag_min: 0,
        });
        Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![a, b],
            ..Project::default()
        }
    }

    #[test]
    fn sf_finish_preserves_start_instant_and_work_with_lag() {
        for (lag, start_day, start_hour, finish_day, finish_hour) in [
            (0, 3, 8, 4, 8),
            (480, 4, 8, 5, 8),
            (-480, 2, 8, 3, 8),
            (120, 3, 10, 4, 10),
        ] {
            let mut proj = sf_project();
            proj.tasks[1].predecessors[0].lag_min = lag;
            let sched = schedule(&proj);
            let b = sched.get(2).unwrap();
            assert_eq!(
                b.early_start,
                DateTime::from_ymd_hm(2026, 3, start_day, start_hour, 0)
            );
            assert_eq!(
                b.early_finish,
                DateTime::from_ymd_hm(2026, 3, finish_day, finish_hour, 0)
            );
            assert_eq!(
                working_minutes_between(&proj, b.early_start, b.early_finish),
                480
            );
        }
        let sched = schedule(&sf_project());
        assert_eq!(sched.get(1).unwrap().total_slack_min, 0);
        assert!(sched.get(1).unwrap().critical);
        assert_eq!(sched.get(2).unwrap().total_slack_min, 960);
    }

    #[test]
    fn sf_tied_fs_driver_and_later_drivers() {
        let mut proj = sf_project();
        proj.tasks.push(task(3, "FS driver", 480));
        proj.tasks[1].predecessors.push(fs(3));
        let b = *schedule(&proj).get(2).unwrap();
        assert_eq!(b.early_finish, DateTime::from_ymd_hm(2026, 3, 4, 8, 0));
        // Reordering tied predecessors must not change the chosen finish.
        proj.tasks[1].predecessors.reverse();
        assert_eq!(*schedule(&proj).get(2).unwrap(), b);
        proj.tasks[2].duration_min = 960;
        assert_eq!(
            schedule(&proj).get(2).unwrap().early_finish,
            DateTime::from_ymd_hm(2026, 3, 4, 17, 0)
        );
        let mut proj = sf_project();
        proj.tasks[1].constraint = ConstraintType::StartNoEarlierThan;
        proj.tasks[1].constraint_date = Some(DateTime::from_ymd_hm(2026, 3, 5, 8, 0));
        assert_eq!(
            schedule(&proj).get(2).unwrap().early_finish,
            DateTime::from_ymd_hm(2026, 3, 5, 17, 0)
        );
    }

    #[test]
    fn sf_hard_constraints_override_same_and_different_index_bounds() {
        for (constraint, day, hour, finish_day) in [
            (ConstraintType::MustFinishOn, 3, 17, 3),
            (ConstraintType::MustFinishOn, 2, 17, 2),
            (ConstraintType::MustStartOn, 2, 8, 2),
        ] {
            let mut proj = sf_project();
            proj.tasks[1].constraint = constraint;
            proj.tasks[1].constraint_date = Some(DateTime::from_ymd_hm(2026, 3, day, hour, 0));
            let sched = schedule(&proj);
            let b = sched.get(2).unwrap();
            assert_eq!(
                b.early_finish,
                DateTime::from_ymd_hm(2026, 3, finish_day, 17, 0)
            );
            assert_eq!(
                working_minutes_between(&proj, b.early_start, b.early_finish),
                480
            );
            assert_eq!(level(&proj).finish(2), Some(b.early_finish));
        }
    }

    fn before_start_project() -> Project {
        let mut a = task(1, "A", 960);
        a.predecessors.push(Predecessor {
            uid: 2,
            link: LinkType::StartFinish,
            lag_min: 0,
        });
        Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![a, task(2, "B", 480)],
            ..Project::default()
        }
    }

    #[test]
    fn sf_can_schedule_before_project_start_without_clamping_work() {
        let proj = before_start_project();
        let sched = schedule(&proj);
        let a = sched.get(1).unwrap();
        assert_eq!(a.early_start, DateTime::from_ymd_hm(2026, 2, 26, 8, 0));
        assert_eq!(a.early_finish, proj.start_date.unwrap());
        assert_eq!(
            working_minutes_between(&proj, a.early_start, a.early_finish),
            960
        );
        assert_eq!(sched.project_start, proj.start_date.unwrap());
        assert!(!a.critical);
        assert!(sched.get(2).unwrap().critical);
        assert_eq!(sched.get(2).unwrap().total_slack_min, 0);
    }

    #[test]
    fn fs_lead_can_start_before_anchor() {
        let mut proj = before_start_project();
        proj.tasks[0].predecessors[0].link = LinkType::FinishStart;
        proj.tasks[0].predecessors[0].lag_min = -960;
        let sched = schedule(&proj);
        assert_eq!(
            sched.get(1).unwrap().early_start,
            DateTime::from_ymd_hm(2026, 2, 27, 8, 0)
        );
    }

    #[test]
    fn unresolved_and_summary_predecessors_keep_anchor_floor() {
        for summary in [false, true] {
            let mut proj = before_start_project();
            if summary {
                proj.tasks[1].summary = true;
            } else {
                proj.tasks.pop();
            }
            assert_eq!(
                schedule(&proj).get(1).unwrap().early_start,
                proj.start_date.unwrap()
            );
        }
    }

    #[test]
    fn pre_anchor_constraints_do_not_depend_on_unrelated_work() {
        for constraint in [
            ConstraintType::MustStartOn,
            ConstraintType::MustFinishOn,
            ConstraintType::FinishNoLaterThan,
            ConstraintType::StartNoLaterThan,
        ] {
            for linked in [false, true] {
                let mut proj = before_start_project();
                proj.tasks.clear();
                let mut constrained = task(10, "Constrained", 480);
                constrained.constraint = constraint;
                constrained.constraint_date = Some(DateTime::from_ymd_hm(2026, 2, 16, 8, 0));
                proj.tasks.push(constrained);
                if linked {
                    // Keep a backward horizon even after the no-links fast path.
                    let mut successor = task(12, "SF successor", 480);
                    successor.predecessors.push(Predecessor {
                        uid: 11,
                        link: LinkType::StartFinish,
                        lag_min: 0,
                    });
                    proj.tasks
                        .extend([task(11, "Anchor milestone", 0), successor]);
                }
                let before = *schedule(&proj).get(10).unwrap();
                proj.tasks.push(task(13, "Unrelated", 5 * 480));
                let after = *schedule(&proj).get(10).unwrap();
                assert_eq!(before, after, "{constraint:?}, linked={linked}");
                let anchor = proj.start_date.unwrap();
                assert_eq!(before.early_start, anchor, "{constraint:?}");
                assert_eq!(
                    before.early_finish,
                    DateTime::from_ymd_hm(2026, 3, 2, 17, 0)
                );
                assert_eq!(before.late_start, anchor, "{constraint:?}");
                let finish_hour = if matches!(
                    constraint,
                    ConstraintType::MustFinishOn | ConstraintType::FinishNoLaterThan
                ) {
                    8
                } else {
                    17
                };
                assert_eq!(
                    before.late_finish,
                    DateTime::from_ymd_hm(2026, 3, 2, finish_hour, 0)
                );
                assert_eq!(before.total_slack_min, 0, "{constraint:?}");
            }
        }
    }

    #[test]
    fn satisfied_constraints_preserve_link_driven_pre_start_dates() {
        let baseline = *schedule(&before_start_project()).get(1).unwrap();
        for (constraint, date) in [
            (
                ConstraintType::StartNoEarlierThan,
                DateTime::from_ymd_hm(2026, 2, 26, 8, 0),
            ),
            (
                ConstraintType::StartNoEarlierThan,
                DateTime::from_ymd_hm(2026, 2, 2, 8, 0),
            ),
            (
                ConstraintType::FinishNoEarlierThan,
                DateTime::from_ymd_hm(2026, 2, 27, 17, 0),
            ),
            (
                ConstraintType::FinishNoEarlierThan,
                DateTime::from_ymd_hm(2026, 2, 25, 17, 0),
            ),
            (
                ConstraintType::MustFinishOn,
                DateTime::from_ymd_hm(2026, 2, 27, 17, 0),
            ),
        ] {
            let mut proj = before_start_project();
            proj.tasks[0].constraint = constraint;
            proj.tasks[0].constraint_date = Some(date);
            let sched = schedule(&proj);
            let a = sched.get(1).unwrap();
            assert_eq!(a.early_start, baseline.early_start, "{constraint:?}");
            // MFO retains the specified evening rather than the equivalent SF
            // morning, following the hard-finish precedence established in v3.
            let finish = if constraint == ConstraintType::MustFinishOn {
                date
            } else {
                baseline.early_finish
            };
            assert_eq!(a.early_finish, finish, "{constraint:?}");
            assert!(sched.get(2).unwrap().critical, "{constraint:?}");
            assert_eq!(
                working_minutes_between(&proj, a.early_start, a.early_finish),
                960
            );
        }
    }

    #[test]
    fn linked_task_far_earlier_mso_does_not_depend_on_origin() {
        let mut proj = before_start_project();
        proj.tasks[0].constraint = ConstraintType::MustStartOn;
        proj.tasks[0].constraint_date = Some(DateTime::from_ymd_hm(2026, 2, 2, 8, 0));
        let before = *schedule(&proj).get(1).unwrap();
        assert_eq!(before.early_start, DateTime::from_ymd_hm(2026, 2, 2, 8, 0));
        proj.tasks.push(task(3, "Unrelated", 5 * 480));
        assert_eq!(*schedule(&proj).get(1).unwrap(), before);
    }

    #[test]
    fn linked_no_later_constraints_keep_dates_before_the_link_driven_start() {
        let expected_start = DateTime::from_ymd_hm(2026, 2, 19, 8, 0);
        let expected_finish = DateTime::from_ymd_hm(2026, 2, 20, 17, 0);
        for (constraint, date) in [
            (ConstraintType::FinishNoLaterThan, expected_finish),
            (ConstraintType::StartNoLaterThan, expected_start),
        ] {
            for honor in [true, false] {
                let mut proj = before_start_project();
                let baseline = *schedule(&proj).get(1).unwrap();
                proj.honor_constraints = honor;
                proj.tasks[0].constraint = constraint;
                proj.tasks[0].constraint_date = Some(date);
                let sched = schedule(&proj);
                let a = sched.get(1).unwrap();
                assert_eq!(
                    a.early_start,
                    if honor {
                        expected_start
                    } else {
                        baseline.early_start
                    }
                );
                assert_eq!(
                    a.early_finish,
                    if honor {
                        expected_finish
                    } else {
                        baseline.early_finish
                    }
                );
                assert_eq!(a.late_start, expected_start);
                assert_eq!(a.late_finish, expected_finish);
                assert_eq!(a.total_slack_min, -2400);
                assert_eq!(sched.get(2).unwrap().total_slack_min, -2400);
                assert_eq!(
                    working_minutes_between(&proj, a.early_start, a.early_finish),
                    960
                );
                assert_eq!(
                    working_minutes_between(&proj, a.late_start, a.late_finish),
                    960
                );
            }
        }
    }

    #[test]
    fn pre_start_finish_constraints_preserve_late_dates_and_slack() {
        let baseline = *schedule(&before_start_project()).get(1).unwrap();
        assert_eq!(baseline.total_slack_min, 480);
        for (constraint, date, slack) in [
            (
                ConstraintType::FinishNoLaterThan,
                DateTime::from_ymd_hm(2026, 3, 2, 12, 0),
                240,
            ),
            (
                ConstraintType::FinishNoLaterThan,
                DateTime::from_ymd_hm(2026, 3, 2, 17, 0),
                480,
            ),
            (
                ConstraintType::MustFinishOn,
                DateTime::from_ymd_hm(2026, 2, 27, 17, 0),
                0,
            ),
        ] {
            let mut proj = before_start_project();
            proj.tasks[0].constraint = constraint;
            proj.tasks[0].constraint_date = Some(date);
            let sched = schedule(&proj);
            let a = sched.get(1).unwrap();
            assert_eq!(a.total_slack_min, slack, "{constraint:?} {date:?}");
            assert_eq!(a.late_finish, date);
            assert_eq!(
                working_minutes_between(&proj, a.late_start, a.late_finish),
                960
            );
            if slack == 480 {
                assert_eq!(*a, baseline);
            }
        }
    }

    #[test]
    fn nonnegative_fs_and_ss_links_do_not_extend_the_timeline_backward() {
        for link in [LinkType::FinishStart, LinkType::StartStart] {
            for lag_min in [0, 480] {
                let mut proj = before_start_project();
                proj.tasks[0].predecessors[0] = Predecessor {
                    uid: 2,
                    link,
                    lag_min,
                };
                let engine = Scheduler::new(&proj);
                assert_eq!(engine.tl(&proj.tasks[0]).segs[0].start, engine.anchor);
            }
        }
    }

    #[test]
    fn sf_late_finish_preserves_an_earlier_same_index_deadline() {
        let mut proj = sf_project();
        proj.honor_constraints = false;
        let deadline = DateTime::from_ymd_hm(2026, 3, 3, 17, 0);
        proj.tasks[1].constraint = ConstraintType::FinishNoLaterThan;
        proj.tasks[1].constraint_date = Some(deadline);
        let sched = schedule(&proj);
        let b = sched.get(2).unwrap();
        assert_eq!(b.early_finish, DateTime::from_ymd_hm(2026, 3, 4, 8, 0));
        assert_eq!(b.late_finish, deadline);
    }

    #[test]
    fn unused_sparse_calendar_does_not_extend_default_timeline() {
        let mut proj = before_start_project();
        proj.tasks.push(task(3, "Long task", 1000 * 480));
        let baseline = Scheduler::new(&proj);
        let tl = baseline.tl(&proj.tasks[0]);
        let expected = (tl.segs[0].start, tl.segs.len());
        let mut sparse = Calendar::standard(2);
        for day in &mut sparse.week {
            day.times.clear();
        }
        sparse.week[1].times.push(WorkingTime {
            from: 8 * 60,
            to: 9 * 60,
        });
        proj.calendars.push(sparse);
        let mut summary = task(4, "Unused summary calendar", 0);
        summary.summary = true;
        summary.calendar_uid = Some(2);
        proj.tasks.push(summary);
        let engine = Scheduler::new(&proj);
        let tl = engine.tl(&proj.tasks[0]);
        assert_eq!((tl.segs[0].start, tl.segs.len()), expected);
    }

    #[test]
    fn timeline_starts_at_anchor_without_resolved_leaf_links() {
        for predecessor in [None, Some(999), Some(2)] {
            let mut proj = before_start_project();
            proj.tasks[0].predecessors.clear();
            proj.tasks[1].summary = true;
            if let Some(uid) = predecessor {
                proj.tasks[0].predecessors.push(Predecessor {
                    uid,
                    link: LinkType::StartFinish,
                    lag_min: 0,
                });
            }
            let engine = Scheduler::new(&proj);
            assert_eq!(engine.tl(&proj.tasks[0]).segs[0].start, engine.anchor);
        }
    }

    #[test]
    fn missing_default_calendar_supplies_pre_anchor_work() {
        let mut proj = before_start_project();
        proj.calendars.clear();
        let sched = schedule(&proj);
        let a = sched.get(1).unwrap();
        assert_eq!(a.early_start, DateTime::from_ymd_hm(2026, 2, 26, 8, 0));
        assert_eq!(a.early_finish, proj.start_date.unwrap());
        assert_eq!(
            working_minutes_between(&proj, a.early_start, a.early_finish),
            960
        );
    }

    #[test]
    fn sf_zero_slack_late_dates_preserve_early_instants() {
        let mut proj = sf_project();
        proj.tasks[0].duration_min = 0;
        let sched = schedule(&proj);
        let b = sched.get(2).unwrap();
        assert_eq!(b.early_finish, DateTime::from_ymd_hm(2026, 3, 4, 8, 0));
        assert_eq!(b.early_finish, sched.project_finish);
        assert_eq!(b.total_slack_min, 0);
        assert_eq!(b.late_finish, b.early_finish);
        assert_eq!(b.late_start, b.early_start);
    }

    #[test]
    fn sf_with_fs_successor_preserves_zero_slack_late_instants() {
        for (mut proj, sf_uid) in [(before_start_project(), 1), (sf_project(), 2)] {
            let mut c = task(3, "FS successor", 960);
            c.predecessors.push(fs(sf_uid));
            proj.tasks.push(c);
            let sched = schedule(&proj);
            let sf = sched.get(sf_uid).unwrap();
            assert_eq!(sf.total_slack_min, 0);
            assert_eq!(sf.late_finish, sf.early_finish);
            assert_eq!(sf.late_start, sf.early_start);
        }
    }

    #[test]
    fn binding_mfo_preserves_duration_before_link_driven_start() {
        let mut proj = before_start_project();
        let finish = DateTime::from_ymd_hm(2026, 2, 26, 17, 0);
        proj.tasks[0].constraint = ConstraintType::MustFinishOn;
        proj.tasks[0].constraint_date = Some(finish);
        let sched = schedule(&proj);
        let a = sched.get(1).unwrap();
        assert_eq!(a.early_start, DateTime::from_ymd_hm(2026, 2, 25, 8, 0));
        assert_eq!(a.early_finish, finish);
        assert_eq!(
            working_minutes_between(&proj, a.early_start, a.early_finish),
            960
        );
        assert_eq!(a.late_finish, a.early_finish);
        assert_eq!(a.late_start, a.early_start);
        assert_eq!(a.total_slack_min, -480);
    }

    #[test]
    fn binding_fnlt_preserves_late_duration_before_early_start() {
        let mut proj = before_start_project();
        let finish = DateTime::from_ymd_hm(2026, 2, 26, 17, 0);
        proj.tasks[0].constraint = ConstraintType::FinishNoLaterThan;
        proj.tasks[0].constraint_date = Some(finish);
        let sched = schedule(&proj);
        let a = sched.get(1).unwrap();
        assert_eq!(a.late_finish, finish);
        assert_eq!(a.late_start, DateTime::from_ymd_hm(2026, 2, 25, 8, 0));
        assert_eq!(
            working_minutes_between(&proj, a.late_start, a.late_finish),
            960
        );
        assert_eq!(a.total_slack_min, -480);
    }

    #[test]
    fn sparse_calendar_has_work_before_anchor_and_full_forward_horizon() {
        let mut proj = before_start_project();
        let mut cal = Calendar::standard(2);
        for dow in [0, 2, 3, 4, 5, 6] {
            cal.week[dow].times.clear();
        }
        proj.tasks[0].calendar_uid = Some(2);
        // The used Mondays-only calendar determines the shared origin here.
        proj.calendars = vec![cal];
        let engine = Scheduler::new(&proj);
        let tl = engine.tl(&proj.tasks[0]);
        assert!(tl.total - tl.to_index(engine.anchor) >= 1440 + 200 * 480 + 480);
        let sched = engine.run();
        let a = sched.get(1).unwrap();
        assert_eq!(a.early_start, DateTime::from_ymd_hm(2026, 2, 16, 8, 0));
        assert_eq!(a.early_finish, proj.start_date.unwrap());
        assert_eq!(
            tl.to_index(a.early_finish.minutes()) - tl.to_index(a.early_start.minutes()),
            960
        );
    }

    #[test]
    fn mixed_calendar_sf_preserves_zero_lag_instant_but_milestone_stays_an_instant() {
        let mut proj = sf_project();
        let mut sunday = Calendar::standard(2);
        sunday.week[0] = sunday.week[1].clone();
        for dow in 1..7 {
            sunday.week[dow].times.clear();
        }
        proj.calendars.push(sunday);
        proj.tasks[0].calendar_uid = Some(2);
        proj.tasks[0].constraint_date = Some(DateTime::from_ymd_hm(2026, 3, 8, 8, 0));
        let sched = schedule(&proj);
        assert_eq!(
            sched.get(2).unwrap().early_finish,
            sched.get(1).unwrap().early_start
        );
        assert_eq!(
            sched.get(2).unwrap().early_finish,
            DateTime::from_ymd_hm(2026, 3, 8, 8, 0)
        );
        proj.tasks[1].duration_min = 0;
        let sched = schedule(&proj);
        let b = sched.get(2).unwrap();
        assert_eq!(b.early_start, DateTime::from_ymd_hm(2026, 3, 9, 8, 0));
        assert_eq!(b.early_finish, b.early_start);
        let leveled = level(&proj);
        assert_eq!(leveled.start(2), leveled.finish(2));
    }

    #[test]
    fn leveling_preserves_sf_dates_without_resources_including_before_anchor() {
        for proj in [sf_project(), before_start_project()] {
            let sched = schedule(&proj);
            let leveled = level(&proj);
            for t in &proj.tasks {
                let r = sched.get(t.uid).unwrap();
                assert_eq!(leveled.start(t.uid), Some(r.early_start));
                assert_eq!(leveled.finish(t.uid), Some(r.early_finish));
            }
        }
    }

    #[test]
    fn leveling_propagates_sf_finish_instant_after_resource_delay() {
        let mut proj = sf_project();
        proj.tasks.insert(0, task(3, "Busy resource", 1920));
        proj.resources = vec![worker(1, "Shared", 1.0)];
        proj.assignments = vec![assign(1, 3, 1, 1.0), assign(2, 1, 1, 1.0)];
        let leveled = level(&proj);
        assert_eq!(
            leveled.start(1).unwrap(),
            DateTime::from_ymd_hm(2026, 3, 6, 8, 0)
        );
        assert_eq!(leveled.finish(2), leveled.start(1));
        assert_eq!(
            working_minutes_between(&proj, leveled.start(2).unwrap(), leveled.finish(2).unwrap()),
            480
        );
        // The delayed milestone path must also preserve finish == start.
        proj.tasks[2].duration_min = 0;
        let leveled = level(&proj);
        assert_eq!(leveled.start(2), leveled.start(1));
        assert_eq!(leveled.start(2), leveled.finish(2));
    }

    #[test]
    fn leveling_applies_finish_bounds_after_resource_delay() {
        // FNLT bounds bracket the delayed SF morning. MFO fixes the CPM finish,
        // so use its original evening and check its bound after resource delay.
        for (constraint, day, hour) in [
            (ConstraintType::FinishNoLaterThan, 5, 17),
            (ConstraintType::FinishNoLaterThan, 6, 8),
            (ConstraintType::MustFinishOn, 3, 17),
        ] {
            for honor in [true, false] {
                let mut proj = sf_project();
                proj.honor_constraints = honor;
                proj.tasks[1].constraint = constraint;
                proj.tasks[1].constraint_date = Some(DateTime::from_ymd_hm(2026, 3, day, hour, 0));
                proj.tasks.insert(0, task(3, "Busy resource", 1920));
                proj.resources = vec![worker(1, "Shared", 1.0)];
                proj.assignments = vec![assign(1, 3, 1, 1.0), assign(2, 1, 1, 1.0)];
                let sched = schedule(&proj);
                let leveled = level(&proj);
                assert_eq!(
                    leveled.start(1),
                    Some(DateTime::from_ymd_hm(2026, 3, 6, 8, 0))
                );
                assert!(leveled.start(2).unwrap() > sched.get(2).unwrap().early_start);
                assert_eq!(
                    leveled.finish(2),
                    Some(if honor && hour == 17 {
                        DateTime::from_ymd_hm(2026, 3, 5, 17, 0)
                    } else {
                        DateTime::from_ymd_hm(2026, 3, 6, 8, 0)
                    })
                );
                assert_eq!(
                    working_minutes_between(
                        &proj,
                        leveled.start(2).unwrap(),
                        leveled.finish(2).unwrap()
                    ),
                    480
                );
            }
        }
    }

    #[test]
    fn fnlt_keeps_an_sf_morning_that_already_meets_the_deadline() {
        for honor in [true, false] {
            let mut proj = sf_project();
            proj.honor_constraints = honor;
            let baseline = *schedule(&proj).get(2).unwrap();
            proj.tasks[1].constraint = ConstraintType::FinishNoLaterThan;
            proj.tasks[1].constraint_date = Some(baseline.early_finish);
            let sched = schedule(&proj);
            let b = sched.get(2).unwrap();
            assert_eq!(b.early_start, baseline.early_start);
            assert_eq!(b.early_finish, DateTime::from_ymd_hm(2026, 3, 4, 8, 0));
            assert_eq!(level(&proj).finish(2), Some(b.early_finish));
        }
    }

    fn constraint_conflict(constraint: ConstraintType, date: DateTime, honor: bool) -> Project {
        let mut b = task(2, "B", 5 * 480);
        b.predecessors.push(fs(1));
        b.constraint = constraint;
        b.constraint_date = Some(date);
        Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            honor_constraints: honor,
            tasks: vec![task(1, "A", 5 * 480), b],
            ..Project::default()
        }
    }

    #[test]
    fn unlinked_stale_deadline_keeps_the_anchor_based_horizon() {
        // The SF link already needs an anchor-based backward budget, making
        // origins directly comparable when the unlinked task's date is absent.
        let mut proj = before_start_project();
        let mut unlinked = task(3, "Old template deadline", 480);
        unlinked.constraint = ConstraintType::FinishNoLaterThan;
        proj.tasks.push(unlinked);
        let baseline_engine = Scheduler::new(&proj);
        let baseline_origin = baseline_engine.tl(&proj.tasks[2]).segs[0].start;
        let baseline = baseline_engine.run();
        proj.tasks[2].constraint_date = Some(DateTime::from_ymd_hm(1900, 1, 1, 17, 0));
        let engine = Scheduler::new(&proj);
        assert_eq!(engine.tl(&proj.tasks[2]).segs[0].start, baseline_origin);
        let sched = engine.run();
        for uid in [1, 2, 3] {
            let before = baseline.get(uid).unwrap();
            let after = sched.get(uid).unwrap();
            assert_eq!(after.early_start, before.early_start);
            assert_eq!(after.early_finish, before.early_finish);
            assert_eq!(after.total_slack_min, before.total_slack_min);
        }
        // The existing pre-anchor contract still floors this deadline's late
        // dates at the anchor; it cannot pull the unlinked task into 1900.
        let unlinked = sched.get(3).unwrap();
        assert_eq!(unlinked.late_start, proj.start_date.unwrap());
        assert_eq!(unlinked.late_finish, proj.start_date.unwrap());
    }

    #[test]
    fn unlinked_post_start_finish_conflict_preserves_negative_slack_and_late_duration() {
        for constraint in [
            ConstraintType::FinishNoLaterThan,
            ConstraintType::MustFinishOn,
        ] {
            for honor in [true, false] {
                let deadline = DateTime::from_ymd_hm(2026, 3, 4, 17, 0);
                let mut proj = constraint_conflict(constraint, deadline, honor);
                proj.tasks.remove(0);
                proj.tasks[0].predecessors.clear();
                let sched = schedule(&proj);
                let r = sched.get(2).unwrap();
                assert_eq!(r.early_start, proj.start_date.unwrap());
                assert_eq!(r.early_finish, DateTime::from_ymd_hm(2026, 3, 6, 17, 0));
                assert_eq!(r.late_finish, deadline);
                assert_eq!(r.late_start, DateTime::from_ymd_hm(2026, 2, 26, 8, 0));
                assert_eq!(r.total_slack_min, -960);
                assert_eq!(
                    working_minutes_between(&proj, r.late_start, r.late_finish),
                    2400
                );
            }
        }
    }

    #[test]
    fn far_earlier_linked_constraints_do_not_depend_on_unrelated_work() {
        let start = DateTime::from_ymd_hm(2024, 1, 8, 8, 0);
        let finish = DateTime::from_ymd_hm(2024, 1, 12, 17, 0);
        for (constraint, date) in [
            (ConstraintType::FinishNoLaterThan, finish),
            (ConstraintType::MustStartOn, start),
        ] {
            for honor in [true, false] {
                let mut proj = constraint_conflict(constraint, date, honor);
                let sched = schedule(&proj);
                let a = *sched.get(1).unwrap();
                let b = *sched.get(2).unwrap();
                assert_eq!(
                    b.early_start,
                    if honor {
                        start
                    } else {
                        DateTime::from_ymd_hm(2026, 3, 9, 8, 0)
                    }
                );
                assert_eq!(
                    b.early_finish,
                    if honor {
                        finish
                    } else {
                        DateTime::from_ymd_hm(2026, 3, 13, 17, 0)
                    }
                );
                assert_eq!(b.late_start, start);
                assert_eq!(b.late_finish, finish);
                assert_eq!(a.late_start, DateTime::from_ymd_hm(2024, 1, 1, 8, 0));
                assert_eq!(a.late_finish, DateTime::from_ymd_hm(2024, 1, 5, 17, 0));
                for r in [a, b] {
                    assert_eq!(r.total_slack_min, -271200);
                    assert_eq!(
                        working_minutes_between(&proj, r.late_start, r.late_finish),
                        2400
                    );
                    assert_eq!(
                        working_minutes_between(&proj, r.early_start, r.early_finish),
                        2400
                    );
                }
                proj.tasks.push(task(3, "Unrelated", 400 * 480));
                let with_unrelated = schedule(&proj);
                assert_eq!(*with_unrelated.get(1).unwrap(), a);
                assert_eq!(*with_unrelated.get(2).unwrap(), b);
            }
        }
    }

    #[test]
    fn far_constraint_horizon_remains_bounded_from_project_anchor() {
        let proj = constraint_conflict(
            ConstraintType::FinishNoLaterThan,
            DateTime::from_ymd_hm(1800, 1, 1, 17, 0),
            true,
        );
        let engine = Scheduler::new(&proj);
        let earliest_day = proj.start_date.unwrap().day_number() - HORIZON_DAYS;
        assert!(engine.tl(&proj.tasks[1]).segs[0].start >= earliest_day * 1440);
    }

    #[test]
    fn fnlt_conflict_reports_negative_slack_in_both_precedence_modes() {
        let deadline = DateTime::from_ymd_hm(2026, 3, 6, 17, 0);
        assert!(Project::default().honor_constraints);
        for honor in [true, false] {
            let proj = constraint_conflict(ConstraintType::FinishNoLaterThan, deadline, honor);
            let sched = schedule(&proj);
            let a = sched.get(1).unwrap();
            let b = sched.get(2).unwrap();
            assert_eq!(a.early_start, proj.start_date.unwrap());
            assert_eq!(a.early_finish, deadline);
            assert_eq!(
                b.early_start,
                DateTime::from_ymd_hm(2026, 3, if honor { 2 } else { 9 }, 8, 0)
            );
            assert_eq!(
                b.early_finish,
                DateTime::from_ymd_hm(2026, 3, if honor { 6 } else { 13 }, 17, 0)
            );
            let leveled = level(&proj);
            for uid in [1, 2] {
                let r = sched.get(uid).unwrap();
                assert_eq!(r.total_slack_min, -2400);
                assert!(r.critical);
                assert_eq!(
                    working_minutes_between(&proj, r.early_start, r.early_finish),
                    2400
                );
                assert_eq!(leveled.start(uid), Some(r.early_start));
                assert_eq!(leveled.finish(uid), Some(r.early_finish));
            }
            let md = crate::gantt::to_markdown(&proj, &sched);
            assert_eq!(
                md.lines()
                    .filter(|line| line.starts_with("| A |") || line.starts_with("| B |"))
                    .filter(|line| line.contains("| -5d |"))
                    .count(),
                2,
                "{md}"
            );
        }
    }

    #[test]
    fn fnlt_partial_and_satisfied_constraints_preserve_dates_and_slack() {
        for honor in [true, false] {
            for (deadline_day, start_day, finish_day, slack) in [
                (
                    10,
                    if honor { 4 } else { 9 },
                    if honor { 10 } else { 13 },
                    -1440,
                ),
                (13, 9, 13, 0),
            ] {
                let proj = constraint_conflict(
                    ConstraintType::FinishNoLaterThan,
                    DateTime::from_ymd_hm(2026, 3, deadline_day, 17, 0),
                    honor,
                );
                let sched = schedule(&proj);
                let b = sched.get(2).unwrap();
                assert_eq!(
                    b.early_start,
                    DateTime::from_ymd_hm(2026, 3, start_day, 8, 0)
                );
                assert_eq!(
                    b.early_finish,
                    DateTime::from_ymd_hm(2026, 3, finish_day, 17, 0)
                );
                for uid in [1, 2] {
                    assert_eq!(sched.get(uid).unwrap().total_slack_min, slack);
                }
            }
        }
    }

    #[test]
    fn snlt_conflicting_and_satisfied_dates_in_both_modes() {
        for honor in [true, false] {
            for (day, slack) in [(2, -2400), (9, 0)] {
                let proj = constraint_conflict(
                    ConstraintType::StartNoLaterThan,
                    DateTime::from_ymd_hm(2026, 3, day, 8, 0),
                    honor,
                );
                let sched = schedule(&proj);
                let b = sched.get(2).unwrap();
                assert_eq!(
                    b.early_start,
                    DateTime::from_ymd_hm(2026, 3, if honor { day } else { 9 }, 8, 0)
                );
                assert_eq!(
                    working_minutes_between(&proj, b.early_start, b.early_finish),
                    2400
                );
                for uid in [1, 2] {
                    assert_eq!(sched.get(uid).unwrap().total_slack_min, slack);
                }
            }
        }
    }

    #[test]
    fn must_constraints_follow_precedence_and_expose_conflicts() {
        // Honor-off expectations specify our link-precedence contract; these
        // values have not been independently verified against Project.
        for (constraint, day, hour) in [
            (ConstraintType::MustStartOn, 2, 8),
            (ConstraintType::MustFinishOn, 6, 17),
        ] {
            for honor in [true, false] {
                let proj = constraint_conflict(
                    constraint,
                    DateTime::from_ymd_hm(2026, 3, day, hour, 0),
                    honor,
                );
                let sched = schedule(&proj);
                let b = sched.get(2).unwrap();
                assert_eq!(
                    b.early_start,
                    DateTime::from_ymd_hm(2026, 3, if honor { 2 } else { 9 }, 8, 0)
                );
                assert_eq!(
                    b.early_finish,
                    DateTime::from_ymd_hm(2026, 3, if honor { 6 } else { 13 }, 17, 0)
                );
                for uid in [1, 2] {
                    assert_eq!(sched.get(uid).unwrap().total_slack_min, -2400);
                }
            }
        }
    }

    #[test]
    fn fnlt_conflict_propagates_through_chain_without_penalizing_parallel_branch() {
        for honor in [true, false] {
            let mut proj = constraint_conflict(
                ConstraintType::FinishNoLaterThan,
                DateTime::from_ymd_hm(2026, 3, 13, 17, 0),
                honor,
            );
            proj.tasks[1].constraint = ConstraintType::AsSoonAsPossible;
            proj.tasks[1].constraint_date = None;
            let mut c = task(3, "C", 2400);
            c.predecessors.push(fs(2));
            c.constraint = ConstraintType::FinishNoLaterThan;
            c.constraint_date = Some(DateTime::from_ymd_hm(2026, 3, 13, 17, 0));
            proj.tasks.extend([c, task(4, "Parallel", 480)]);
            let sched = schedule(&proj);
            for uid in [1, 2, 3] {
                assert_eq!(sched.get(uid).unwrap().total_slack_min, -2400);
            }
            assert!(sched.get(4).unwrap().total_slack_min >= 0);
            assert_eq!(
                sched.get(3).unwrap().early_start,
                DateTime::from_ymd_hm(2026, 3, if honor { 9 } else { 16 }, 8, 0)
            );
        }
    }

    #[test]
    fn sf_finish_constraints_compare_wall_clock_instants_in_both_modes() {
        for constraint in [
            ConstraintType::FinishNoLaterThan,
            ConstraintType::MustFinishOn,
        ] {
            for honor in [true, false] {
                let mut proj = sf_project();
                proj.honor_constraints = honor;
                proj.tasks[1].constraint = constraint;
                proj.tasks[1].constraint_date = Some(DateTime::from_ymd_hm(2026, 3, 3, 17, 0));
                let sched = schedule(&proj);
                let b = sched.get(2).unwrap();
                assert_eq!(
                    b.early_finish,
                    if honor {
                        DateTime::from_ymd_hm(2026, 3, 3, 17, 0)
                    } else {
                        DateTime::from_ymd_hm(2026, 3, 4, 8, 0)
                    }
                );
                assert_eq!(
                    working_minutes_between(&proj, b.early_start, b.early_finish),
                    480
                );
                assert_eq!(level(&proj).finish(2), Some(b.early_finish));
            }
        }
        for honor in [true, false] {
            let mut proj = sf_project();
            proj.honor_constraints = honor;
            let baseline = *schedule(&proj).get(2).unwrap();
            proj.tasks[1].constraint = ConstraintType::FinishNoLaterThan;
            proj.tasks[1].constraint_date = Some(DateTime::from_ymd_hm(2026, 3, 5, 17, 0));
            assert_eq!(*schedule(&proj).get(2).unwrap(), baseline);
        }
    }

    /// The classic worked example: A(2d)→B(3d), A→C(1d), B→D, C→D.
    /// Critical path A→B→D = 7 days; C has 2 days of slack.
    fn diamond() -> Project {
        let mut a = task(1, "A", 960); // 2d
        let mut b = task(2, "B", 1440); // 3d
        let mut c = task(3, "C", 480); // 1d
        let mut d = task(4, "D", 960); // 2d
        a.id = 1;
        b.predecessors = vec![fs(1)];
        c.predecessors = vec![fs(1)];
        d.predecessors = vec![fs(2), fs(3)];
        Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)), // a Monday
            tasks: vec![a, b, c, d],
            ..Project::default()
        }
    }

    fn worker(uid: i32, name: &str, units: f64) -> Resource {
        Resource {
            uid,
            id: uid,
            name: name.into(),
            kind: ResourceType::Work,
            max_units: units,
            ..Resource::default()
        }
    }
    fn assign(uid: i32, task: i32, res: i32, units: f64) -> Assignment {
        Assignment {
            uid,
            task_uid: task,
            resource_uid: res,
            units,
            work_min: 0,
        }
    }

    #[test]
    fn leveling_only_work_resources_constrain_capacity() {
        for kind in [
            ResourceType::Work,
            ResourceType::Material,
            ResourceType::Cost,
        ] {
            let proj = Project {
                start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
                tasks: vec![task(1, "A", 960), task(2, "B", 960)],
                resources: vec![Resource {
                    kind,
                    ..worker(1, "Shared", 1.0)
                }],
                assignments: vec![assign(1, 1, 1, 1.0), assign(2, 2, 1, 1.0)],
                ..Project::default()
            };
            let cpm = schedule(&proj);
            let leveled = level(&proj);
            assert_eq!(leveled.start(1), Some(cpm.get(1).unwrap().early_start));
            assert_eq!(leveled.finish(1), Some(cpm.get(1).unwrap().early_finish));
            if kind == ResourceType::Work {
                assert_eq!(leveled.start(2).unwrap().to_mspdi(), "2026-03-04T08:00:00");
                assert_eq!(leveled.finish(2).unwrap().to_mspdi(), "2026-03-05T17:00:00");
            } else {
                assert_eq!(
                    leveled.start(2),
                    Some(cpm.get(2).unwrap().early_start),
                    "{kind:?}"
                );
                assert_eq!(
                    leveled.finish(2),
                    Some(cpm.get(2).unwrap().early_finish),
                    "{kind:?}"
                );
            }
        }
    }

    #[test]
    fn leveling_serializes_a_shared_resource() {
        // Two independent 2-day tasks, both staffed by Alice (capacity 1).
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![task(1, "A", 960), task(2, "B", 960)],
            resources: vec![worker(1, "Alice", 1.0)],
            assignments: vec![assign(1, 1, 1, 1.0), assign(2, 2, 1, 1.0)],
            ..Project::default()
        };
        // Unleveled, both start Monday (they'd overlap).
        let s = schedule(&proj);
        assert_eq!(
            s.get(1).unwrap().early_start.to_mspdi(),
            "2026-03-02T08:00:00"
        );
        assert_eq!(
            s.get(2).unwrap().early_start.to_mspdi(),
            "2026-03-02T08:00:00"
        );
        // Leveled, B waits until A frees Alice: A Mon–Tue, B Wed–Thu.
        let lv = level(&proj);
        assert_eq!(lv.start(1).unwrap().to_mspdi(), "2026-03-02T08:00:00");
        assert_eq!(lv.start(2).unwrap().to_mspdi(), "2026-03-04T08:00:00");
        assert_eq!(lv.project_finish.to_mspdi(), "2026-03-05T17:00:00");
    }

    #[test]
    fn working_minutes_span_skips_weekends() {
        let proj = Project::default(); // default (standard Mon–Fri 8h) calendar
        // Mon 08:00 → Tue 17:00 is two full 8h working days.
        let mon = DateTime::from_ymd_hm(2026, 3, 2, 8, 0);
        let tue = DateTime::from_ymd_hm(2026, 3, 3, 17, 0);
        assert_eq!(working_minutes_between(&proj, mon, tue), 960);
        // Fri 08:00 → Mon 17:00 is also two working days (the weekend is skipped).
        let fri = DateTime::from_ymd_hm(2026, 3, 6, 8, 0);
        let nextmon = DateTime::from_ymd_hm(2026, 3, 9, 17, 0);
        assert_eq!(working_minutes_between(&proj, fri, nextmon), 960);
    }

    #[test]
    fn must_start_on_reproduces_imported_dates() {
        // An imported .mpp task: pinned start + a duration derived from the
        // stored start/finish must reschedule back to the same wall-clock dates.
        let start = DateTime::from_ymd_hm(2026, 3, 6, 8, 0); // Friday
        let finish = DateTime::from_ymd_hm(2026, 3, 9, 17, 0); // next Monday
        let proj = Project::default();
        let dur = working_minutes_between(&proj, start, finish);
        let mut t = task(1, "Imported", dur);
        t.constraint = ConstraintType::MustStartOn;
        t.constraint_date = Some(start);
        let proj = Project {
            start_date: Some(start),
            tasks: vec![t],
            ..Project::default()
        };
        let s = schedule(&proj);
        assert_eq!(
            s.get(1).unwrap().early_start.to_mspdi(),
            "2026-03-06T08:00:00"
        );
        assert_eq!(
            s.get(1).unwrap().early_finish.to_mspdi(),
            "2026-03-09T17:00:00"
        );
    }

    #[test]
    fn leveling_allows_overlap_within_capacity() {
        // A resource with capacity 2 can run both unit-1 tasks at once.
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![task(1, "A", 960), task(2, "B", 960)],
            resources: vec![worker(1, "Team", 2.0)],
            assignments: vec![assign(1, 1, 1, 1.0), assign(2, 2, 1, 1.0)],
            ..Project::default()
        };
        let lv = level(&proj);
        assert_eq!(lv.start(1).unwrap().to_mspdi(), "2026-03-02T08:00:00");
        assert_eq!(lv.start(2).unwrap().to_mspdi(), "2026-03-02T08:00:00"); // no delay
    }

    #[test]
    fn leveling_without_resources_matches_cpm() {
        let proj = diamond();
        let s = schedule(&proj);
        let lv = level(&proj);
        for uid in 1..=4 {
            assert_eq!(lv.start(uid), Some(s.get(uid).unwrap().early_start));
            assert_eq!(lv.finish(uid), Some(s.get(uid).unwrap().early_finish));
        }
    }

    #[test]
    fn diamond_critical_path() {
        let proj = diamond();
        let s = schedule(&proj);
        // A starts Monday 08:00.
        assert_eq!(
            s.get(1).unwrap().early_start.to_mspdi(),
            "2026-03-02T08:00:00"
        );
        // Critical path is A, B, D; C is not critical.
        assert!(s.get(1).unwrap().critical);
        assert!(s.get(2).unwrap().critical);
        assert!(!s.get(3).unwrap().critical);
        assert!(s.get(4).unwrap().critical);
        // C has 2 working days of total slack (B is 3d, C is 1d, both feed D).
        assert_eq!(s.get(3).unwrap().total_slack_min, 960);
        // Project finish = 7 working days after Monday 08:00 = next Wednesday 17:00.
        // Mon+Tue = A(2d); Wed..Fri = B(3d); Mon..Tue = D(2d) → finish Tue 17:00.
        assert_eq!(s.project_finish.to_mspdi(), "2026-03-10T17:00:00");
    }

    #[test]
    fn finish_crosses_weekend() {
        // One 5-day task from Monday finishes Friday 17:00 (no weekend work).
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![task(1, "week", 2400)], // 5d
            ..Project::default()
        };
        let s = schedule(&proj);
        let r = s.get(1).unwrap();
        assert_eq!(r.early_start.to_mspdi(), "2026-03-02T08:00:00");
        assert_eq!(r.early_finish.to_mspdi(), "2026-03-06T17:00:00"); // Friday
    }

    #[test]
    fn fs_lag_pushes_successor() {
        // A(1d) → B(1d) with +1d lag. A: Mon; lag skips Tue; B: Wed.
        let mut a = task(1, "A", 480);
        a.id = 1;
        let mut b = task(2, "B", 480);
        b.predecessors = vec![Predecessor {
            uid: 1,
            link: LinkType::FinishStart,
            lag_min: 480,
        }];
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![a, b],
            ..Project::default()
        };
        let s = schedule(&proj);
        assert_eq!(
            s.get(2).unwrap().early_start.to_mspdi(),
            "2026-03-04T08:00:00"
        );
    }

    #[test]
    fn snet_constraint_delays_start() {
        let mut a = task(1, "A", 480);
        a.constraint = ConstraintType::StartNoEarlierThan;
        a.constraint_date = Some(DateTime::from_ymd_hm(2026, 3, 5, 8, 0)); // Thursday
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![a],
            ..Project::default()
        };
        let s = schedule(&proj);
        assert_eq!(
            s.get(1).unwrap().early_start.to_mspdi(),
            "2026-03-05T08:00:00"
        );
    }

    #[test]
    fn summary_rolls_up_children() {
        let mut sum = task(1, "Phase", 0);
        sum.summary = true;
        sum.outline_level = 1;
        let mut a = task(2, "A", 480);
        a.outline_level = 2;
        let mut b = task(3, "B", 480);
        b.outline_level = 2;
        b.predecessors = vec![fs(2)];
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![sum, a, b],
            ..Project::default()
        };
        let s = schedule(&proj);
        let r = s.get(1).unwrap();
        assert_eq!(r.early_start.to_mspdi(), "2026-03-02T08:00:00"); // A start
        assert_eq!(r.early_finish.to_mspdi(), "2026-03-03T17:00:00"); // B finish (Tue)
    }

    fn at(d: u32, h: u32) -> DateTime {
        DateTime::from_ymd_hm(2026, 3, d, h, 0)
    }

    /// A manually scheduled task pinned at `start` (March 2026, day `d`).
    fn manual(uid: i32, name: &str, duration: i64, start: DateTime) -> Task {
        Task {
            manual: true,
            manual_start: Some(start),
            ..task(uid, name, duration)
        }
    }

    fn dates(s: &Schedule, uid: i32) -> (String, String) {
        let r = s.get(uid).unwrap();
        (r.early_start.to_mspdi(), r.early_finish.to_mspdi())
    }

    fn march2(tasks: Vec<Task>) -> Project {
        Project {
            start_date: Some(at(2, 8)),
            tasks,
            ..Project::default()
        }
    }

    #[test]
    fn manual_task_is_not_moved_by_a_violated_link() {
        let mut m = manual(2, "M", 480, at(2, 8));
        m.predecessors.push(fs(1));
        let s = schedule(&march2(vec![task(1, "P", 1440), m]));
        assert_eq!(
            dates(&s, 2),
            ("2026-03-02T08:00:00".into(), "2026-03-02T17:00:00".into())
        );
        // The link wants Thursday; the task stays Monday and the late finish is
        // the project finish (Wednesday), so the violation is -1 day of slack.
        assert_eq!(s.get(2).unwrap().total_slack_min, -480);
        // Its predecessor's late dates fall before the project start and stay
        // finite: P must finish by M's late start.
        let p = s.get(1).unwrap();
        assert_eq!(p.late_start.to_mspdi(), "2026-02-27T08:00:00");
        assert_eq!(p.total_slack_min, -480);
    }

    #[test]
    fn auto_successor_follows_a_manual_task_pinned_after_its_link() {
        let mut m = manual(2, "M", 480, at(9, 8));
        m.predecessors.push(fs(1));
        let mut succ = task(3, "S", 480);
        succ.predecessors.push(fs(2));
        let s = schedule(&march2(vec![task(1, "P", 1440), m, succ]));
        assert_eq!(dates(&s, 2).0, "2026-03-09T08:00:00");
        assert_eq!(
            dates(&s, 3),
            ("2026-03-10T08:00:00".into(), "2026-03-10T17:00:00".into())
        );
        assert_eq!(s.get(2).unwrap().total_slack_min, 0);
        assert!(s.get(1).unwrap().total_slack_min > 0);
    }

    #[test]
    fn manual_task_before_the_project_start_keeps_its_successor_and_slack() {
        let mut succ = task(2, "S", 480);
        succ.predecessors.push(fs(1));
        let proj = Project {
            start_date: Some(at(9, 8)),
            tasks: vec![manual(1, "M", 480, at(2, 8)), succ],
            ..Project::default()
        };
        let s = schedule(&proj);
        assert_eq!(dates(&s, 1).0, "2026-03-02T08:00:00");
        assert_eq!(dates(&s, 2).0, "2026-03-03T08:00:00");
        // Unlinked, it violates nothing: no negative slack from the anchor.
        assert_eq!(s.get(1).unwrap().total_slack_min, 0);
        assert_eq!(s.get(2).unwrap().total_slack_min, 0);
    }

    #[test]
    fn manual_task_ignores_a_stale_constraint() {
        let mut m = manual(2, "M", 480, at(2, 8));
        m.constraint = ConstraintType::MustStartOn;
        m.constraint_date = Some(at(16, 8));
        let s = schedule(&march2(vec![task(1, "A", 2400), m]));
        assert_eq!(dates(&s, 2).0, "2026-03-02T08:00:00");
        // Slack comes from the project finish (Friday), not the MSO date.
        assert_eq!(s.get(2).unwrap().total_slack_min, 1920);
    }

    #[test]
    fn manual_start_on_a_weekend_is_kept_and_finishes_by_duration() {
        let s = schedule(&march2(vec![manual(1, "M", 480, at(7, 8))]));
        assert_eq!(
            dates(&s, 1),
            ("2026-03-07T08:00:00".into(), "2026-03-09T17:00:00".into())
        );
    }

    #[test]
    fn manual_finish_pins_the_finish_and_stored_finish_never_does() {
        let mut pinned = manual(1, "Pinned", 480, at(2, 8));
        pinned.manual_finish = Some(at(4, 17));
        let mut succ = task(2, "S", 480);
        succ.predecessors.push(fs(1));
        // A stale stored finish (the file's value before a duration edit).
        let mut edited = manual(3, "Edited", 480, at(2, 8));
        edited.stored_finish = Some(at(6, 17));
        let long = task(4, "Long", 4800);
        let s = schedule(&march2(vec![pinned, succ, edited, long]));
        assert_eq!(dates(&s, 1).1, "2026-03-04T17:00:00");
        assert_eq!(dates(&s, 2).0, "2026-03-05T08:00:00");
        assert_eq!(dates(&s, 3).1, "2026-03-02T17:00:00");
        // Late dates use the pinned three-day span, not the one-day duration:
        // S must start by Friday the 13th, so the pinned task by Tuesday.
        let r = s.get(1).unwrap();
        assert_eq!(r.late_start.to_mspdi(), "2026-03-10T08:00:00");
        assert_eq!(r.total_slack_min, 6 * 480);
    }

    #[test]
    fn manual_task_without_a_start_schedules_like_an_auto_task() {
        let mut tbd = Task {
            manual: true,
            ..task(2, "TBD", 480)
        };
        tbd.predecessors.push(fs(1));
        let s = schedule(&march2(vec![task(1, "P", 480), tbd]));
        assert_eq!(dates(&s, 2).0, "2026-03-03T08:00:00");
    }

    #[test]
    fn leveling_moves_auto_tasks_around_a_manual_one() {
        let mut proj = march2(vec![task(1, "Auto", 960), manual(2, "M", 960, at(2, 8))]);
        proj.resources = vec![worker(1, "R", 1.0)];
        proj.assignments = vec![assign(1, 1, 1, 1.0), assign(2, 2, 1, 1.0)];
        let lv = level(&proj);
        assert_eq!(lv.start(2).unwrap().to_mspdi(), "2026-03-02T08:00:00");
        assert_eq!(lv.finish(2).unwrap().to_mspdi(), "2026-03-03T17:00:00");
        assert_eq!(lv.start(1).unwrap().to_mspdi(), "2026-03-04T08:00:00");
    }

    #[test]
    fn leveling_never_moves_a_manual_task_behind_a_delayed_predecessor() {
        let mut m = manual(3, "M", 480, at(5, 8));
        m.predecessors.push(fs(2));
        let mut proj = march2(vec![task(1, "A", 960), task(2, "B", 960), m]);
        proj.resources = vec![worker(1, "R", 1.0)];
        proj.assignments = vec![assign(1, 1, 1, 1.0), assign(2, 2, 1, 1.0)];
        let lv = level(&proj);
        // B is leveled behind A to Wednesday-Thursday; M stays on Thursday.
        assert_eq!(lv.start(2).unwrap().to_mspdi(), "2026-03-04T08:00:00");
        assert_eq!(lv.start(3).unwrap().to_mspdi(), "2026-03-05T08:00:00");
    }
}
