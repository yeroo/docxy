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
//! project anchor" (`to_index`) and its inverse (`abs_start`/`abs_finish`).
//! Once in index space, "finish = start + duration",
//! "successor = predecessor + lag", and slack are all plain integer arithmetic;
//! we only convert back to a [`DateTime`] at the end. Each distinct calendar
//! gets its own timeline anchored at the same absolute project start, so
//! cross-calendar dependencies still compare correctly in wall-clock space.
//!
//! ## Scope (v1)
//!
//! Leaf tasks schedule via FS/SS/FF/SF links with lag, ASAP by default, honoring
//! the hard date constraints (MSO/SNET/FNET/MFO forward; MFO/FNLT/SNLT/MSO
//! backward). Summary tasks roll up from their descendants. Resource leveling is
//! not modeled. Free slack is computed precisely for finish-to-start successors
//! and falls back to total slack otherwise.

use crate::datetime::DateTime;
use crate::model::{ConstraintType, LinkType, Project, Task};
use std::collections::HashMap;

/// Computed schedule for one task.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TaskResult {
    pub uid: i32,
    pub early_start: DateTime,
    pub early_finish: DateTime,
    pub late_start: DateTime,
    pub late_finish: DateTime,
    /// Total slack in working minutes (late − early). ≤ 0 ⇒ critical.
    pub total_slack_min: i64,
    /// Free slack in working minutes (delay possible without moving any
    /// successor). Precise for FS successors; else equals total slack.
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

/// A single calendar's working time, anchored at the project start, expressed as
/// a run of absolute working-minute segments with cumulative offsets.
struct Timeline {
    segs: Vec<Seg>,
    cum: Vec<i64>, // cum[i] = working minutes before segs[i]
    total: i64,
}

impl Timeline {
    /// Build a timeline for `week` (Sun=0..Sat=6 working patterns) starting at
    /// `anchor_abs`, extended until it covers at least `min_total` working
    /// minutes and reaches wall-clock minute `min_reach`.
    fn build(week: &[Vec<(u32, u32)>; 7], anchor_abs: i64, min_total: i64, min_reach: i64) -> Timeline {
        let mut segs = Vec::new();
        let mut cum = Vec::new();
        let mut total = 0i64;
        let mut day = anchor_abs.div_euclid(1440);
        let anchor_mod = anchor_abs.rem_euclid(1440) as u32;
        let mut first = true;
        let mut guard = 0;
        loop {
            let dow = (day + 4).rem_euclid(7) as usize;
            let floor = if first { anchor_mod } else { 0 };
            first = false;
            for &(from, to) in &week[dow] {
                let from = from.max(floor);
                if from < to {
                    let s = day * 1440 + from as i64;
                    let e = day * 1440 + to as i64;
                    segs.push(Seg { start: s, end: e });
                    cum.push(total);
                    total += e - s;
                }
            }
            day += 1;
            guard += 1;
            let reached = day * 1440;
            if (total >= min_total && reached >= min_reach) || guard > 366 * 100 {
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
    /// `k` working minutes after the anchor. A `k` on a segment boundary maps to
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

impl<'a> Scheduler<'a> {
    fn new(proj: &'a Project) -> Scheduler<'a> {
        // Anchor: explicit project start, else earliest stored start, else a
        // fixed Monday, snapped to the default calendar's first working instant.
        let default_cal = proj.default_calendar_uid;
        let raw_anchor = proj
            .start_date
            .or_else(|| proj.tasks.iter().filter_map(|t| t.stored_start).min())
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
        let min_total = work + lag + 200 * 480 + 480;
        let far_dates = proj
            .tasks
            .iter()
            .flat_map(|t| [t.constraint_date, t.stored_finish])
            .flatten()
            .map(|d| d.minutes())
            .max()
            .unwrap_or(raw_anchor);
        let min_reach = far_dates.max(raw_anchor) + 90 * 1440;

        // Build a timeline per calendar; ensure the default exists.
        let mut timelines = HashMap::new();
        let mut anchor = raw_anchor;
        let mut have_default = false;
        for cal in &proj.calendars {
            let week = week_pairs(cal);
            let tl = Timeline::build(&week, raw_anchor, min_total, min_reach);
            if cal.uid == default_cal {
                anchor = tl.snap(raw_anchor);
                have_default = true;
            }
            timelines.insert(cal.uid, tl);
        }
        if !have_default {
            let std = crate::model::Calendar::standard(default_cal);
            let tl = Timeline::build(&week_pairs(&std), raw_anchor, min_total, min_reach);
            anchor = tl.snap(raw_anchor);
            timelines.insert(default_cal, tl);
        }

        Scheduler { proj, timelines, default_cal, anchor }
    }

    fn tl(&self, task: &Task) -> &Timeline {
        let uid = task.calendar_uid.unwrap_or(self.default_cal);
        self.timelines
            .get(&uid)
            .or_else(|| self.timelines.get(&self.default_cal))
            .expect("default timeline always present")
    }

    fn run(&self) -> Schedule {
        // Leaf tasks are the schedulable units; summaries roll up afterward.
        let leaves: Vec<usize> = (0..self.proj.tasks.len())
            .filter(|&i| !self.proj.tasks[i].summary)
            .collect();
        let leaf_uids: std::collections::HashSet<i32> =
            leaves.iter().map(|&i| self.proj.tasks[i].uid).collect();
        let idx_of: HashMap<i32, usize> =
            self.proj.tasks.iter().enumerate().map(|(i, t)| (t.uid, i)).collect();

        let order = topo_order(self.proj, &leaves, &leaf_uids, &idx_of);

        // Successors of each leaf (for backward pass + free slack).
        let mut succs: HashMap<i32, Vec<(i32, LinkType, i64)>> = HashMap::new();
        for &i in &leaves {
            let t = &self.proj.tasks[i];
            for p in &t.predecessors {
                if leaf_uids.contains(&p.uid) {
                    succs.entry(p.uid).or_default().push((t.uid, p.link, p.lag_min));
                }
            }
        }

        // ---- forward pass: early start / early finish ----
        let mut es: HashMap<i32, i64> = HashMap::new(); // index space (own calendar)
        let mut ef: HashMap<i32, i64> = HashMap::new();
        let mut ef_abs: HashMap<i32, i64> = HashMap::new();
        let mut es_abs: HashMap<i32, i64> = HashMap::new();
        for &i in &order {
            let t = &self.proj.tasks[i];
            let tl = self.tl(t);
            let mut start_abs = self.anchor;
            for p in &t.predecessors {
                let Some(&pf_abs) = ef_abs.get(&p.uid) else { continue };
                let Some(&ps_abs) = es_abs.get(&p.uid) else { continue };
                let cand = match p.link {
                    LinkType::FinishStart => tl.abs_start(tl.to_index(pf_abs) + p.lag_min),
                    LinkType::StartStart => tl.abs_start(tl.to_index(ps_abs) + p.lag_min),
                    LinkType::FinishFinish => {
                        let cf = tl.abs_finish(tl.to_index(pf_abs) + p.lag_min);
                        tl.abs_start(tl.to_index(cf) - t.duration_min)
                    }
                    LinkType::StartFinish => {
                        let cf = tl.abs_finish(tl.to_index(ps_abs) + p.lag_min);
                        tl.abs_start(tl.to_index(cf) - t.duration_min)
                    }
                };
                start_abs = start_abs.max(cand);
            }
            // Hard constraints (forward-affecting). Snap the constraint date two
            // ways: as a start (next morning) or a finish (this evening).
            if let Some(cd) = t.constraint_date {
                let ds = tl.snap(cd.minutes());
                let df = tl.abs_finish(tl.to_index(cd.minutes()));
                match t.constraint {
                    ConstraintType::MustStartOn => start_abs = ds,
                    ConstraintType::StartNoEarlierThan => start_abs = start_abs.max(ds),
                    ConstraintType::FinishNoEarlierThan => {
                        start_abs = start_abs.max(tl.abs_start(tl.to_index(df) - t.duration_min));
                    }
                    ConstraintType::MustFinishOn => {
                        start_abs = tl.abs_start(tl.to_index(df) - t.duration_min);
                    }
                    _ => {}
                }
            }
            let s_abs = tl.snap(start_abs);
            let s_idx = tl.to_index(s_abs);
            let f_idx = s_idx + t.duration_min;
            // A milestone (zero duration) finishes exactly when it starts.
            let f_abs = if t.duration_min == 0 { s_abs } else { tl.abs_finish(f_idx) };
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
            let mut finish_abs = project_finish_abs;
            if let Some(list) = succs.get(&t.uid) {
                if !list.is_empty() {
                    finish_abs = i64::MAX;
                    for &(suid, link, lag) in list {
                        let sls = ls_abs.get(&suid).copied();
                        let slf = lf_abs.get(&suid).copied();
                        let cand = match link {
                            // this.finish ≤ succ.late_start − lag
                            LinkType::FinishStart => {
                                sls.map(|x| tl.abs_finish(tl.to_index(x) - lag))
                            }
                            // succ.start ≥ this.start + lag ⇒ bound this.start, then finish
                            LinkType::StartStart => sls.map(|x| {
                                let this_start = tl.abs_start(tl.to_index(x) - lag);
                                tl.abs_finish(tl.to_index(this_start) + t.duration_min)
                            }),
                            // this.finish ≤ succ.late_finish − lag
                            LinkType::FinishFinish => {
                                slf.map(|x| tl.abs_finish(tl.to_index(x) - lag))
                            }
                            LinkType::StartFinish => slf.map(|x| {
                                let this_start = tl.abs_start(tl.to_index(x) - lag);
                                tl.abs_finish(tl.to_index(this_start) + t.duration_min)
                            }),
                        };
                        if let Some(c) = cand {
                            finish_abs = finish_abs.min(c);
                        }
                    }
                    if finish_abs == i64::MAX {
                        finish_abs = project_finish_abs;
                    }
                }
            }
            // Hard constraints (backward-affecting).
            if let Some(cd) = t.constraint_date {
                let ds = tl.snap(cd.minutes());
                let df = tl.abs_finish(tl.to_index(cd.minutes()));
                match t.constraint {
                    ConstraintType::MustFinishOn => finish_abs = df,
                    ConstraintType::FinishNoLaterThan => finish_abs = finish_abs.min(df),
                    ConstraintType::StartNoLaterThan => {
                        finish_abs =
                            finish_abs.min(tl.abs_finish(tl.to_index(ds) + t.duration_min));
                    }
                    ConstraintType::MustStartOn => {
                        finish_abs = tl.abs_finish(tl.to_index(ds) + t.duration_min);
                    }
                    _ => {}
                }
            }
            let f_abs = tl.abs_finish(tl.to_index(finish_abs));
            let s_abs = if t.duration_min == 0 {
                f_abs
            } else {
                tl.abs_start(tl.to_index(f_abs) - t.duration_min)
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
            let total = tl.to_index(l_s) - tl.to_index(e_s);
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
            let child: Vec<&TaskResult> =
                kids.iter().filter_map(|u| results.get(u)).collect();
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
    let a = start.minutes().min(finish.minutes());
    let b = start.minutes().max(finish.minutes());
    let tl = Timeline::build(&week_pairs(&cal), a, (b - a) + 480, b + 1440);
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
    events.sort_by(|a, b| a.0.cmp(&b.0));
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
fn earliest_feasible(bookings: &[(i64, i64, f64)], cap: f64, units: f64, start: i64, dur: i64) -> i64 {
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

        let leaves: Vec<usize> = (0..self.proj.tasks.len())
            .filter(|&i| !self.proj.tasks[i].summary)
            .collect();
        let leaf_uids: std::collections::HashSet<i32> =
            leaves.iter().map(|&i| self.proj.tasks[i].uid).collect();
        let idx_of: HashMap<i32, usize> =
            self.proj.tasks.iter().enumerate().map(|(i, t)| (t.uid, i)).collect();
        let order = topo_order(self.proj, &leaves, &leaf_uids, &idx_of);

        // Work-resource capacities and per-task assignments.
        let mut caps: HashMap<i32, f64> = HashMap::new();
        for r in &self.proj.resources {
            if r.is_work {
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

        for &i in &order {
            let t = &self.proj.tasks[i];
            let cpm = base.get(t.uid).expect("leaf scheduled");
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
                bookings.entry(*rid).or_default().push((placed, placed + t.duration_min, *units));
            }
            let s_abs = tl.abs_start(placed);
            let f_abs = if t.duration_min == 0 { s_abs } else { tl.abs_finish(placed + t.duration_min) };
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

        Leveled { start, finish, project_finish }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn task(uid: i32, name: &str, days_min: i64) -> Task {
        Task { uid, id: uid, name: name.into(), outline_level: 1, duration_min: days_min, ..Task::default() }
    }
    fn fs(uid: i32) -> Predecessor {
        Predecessor { uid, link: LinkType::FinishStart, lag_min: 0 }
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
        Resource { uid, id: uid, name: name.into(), is_work: true, max_units: units, calendar_uid: None }
    }
    fn assign(uid: i32, task: i32, res: i32, units: f64) -> Assignment {
        Assignment { uid, task_uid: task, resource_uid: res, units, work_min: 0 }
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
        assert_eq!(s.get(1).unwrap().early_start.to_mspdi(), "2026-03-02T08:00:00");
        assert_eq!(s.get(2).unwrap().early_start.to_mspdi(), "2026-03-02T08:00:00");
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
        let proj = Project { start_date: Some(start), tasks: vec![t], ..Project::default() };
        let s = schedule(&proj);
        assert_eq!(s.get(1).unwrap().early_start.to_mspdi(), "2026-03-06T08:00:00");
        assert_eq!(s.get(1).unwrap().early_finish.to_mspdi(), "2026-03-09T17:00:00");
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
        assert_eq!(s.get(1).unwrap().early_start.to_mspdi(), "2026-03-02T08:00:00");
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
        b.predecessors = vec![Predecessor { uid: 1, link: LinkType::FinishStart, lag_min: 480 }];
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![a, b],
            ..Project::default()
        };
        let s = schedule(&proj);
        assert_eq!(s.get(2).unwrap().early_start.to_mspdi(), "2026-03-04T08:00:00");
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
        assert_eq!(s.get(1).unwrap().early_start.to_mspdi(), "2026-03-05T08:00:00");
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
}
