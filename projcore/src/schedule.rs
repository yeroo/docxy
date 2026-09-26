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
//! tasks before it, but a deadline there still reports its miss as negative
//! slack. A task's deadline bounds only its late finish, like an FNLT would,
//! whatever HonorConstraints says: it never moves scheduled dates, and a
//! missed one shows as negative total slack on the task and its drivers.
//! Summary tasks roll up from their descendants, except that a manually
//! scheduled summary keeps its own dates: they floor its unconstrained
//! subtasks' starts and extend the project finish (see [`Schedule::rolled_up`]
//! and [`manual_warning`]). Resource
//! leveling is separate from CPM. Free slack is computed precisely for
//! finish-to-start successors and falls back to total slack otherwise.

use crate::datetime::DateTime;
use crate::model::{
    Calendar, ConstraintType, LagKind, LinkType, Predecessor, Project, ResourceType, Task, Week,
    WorkCalendar, WorkingTime,
};
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
    /// Start and finish slack in working minutes (MSPDI `StartSlack` and
    /// `FinishSlack`). A leaf's start slack is its total slack; its finish
    /// slack differs by how much longer its late span is than its early span.
    /// A summary measures late minus early start (finish) on the summary
    /// calendar.
    pub start_slack_min: i64,
    pub finish_slack_min: i64,
    pub critical: bool,
}

/// The whole computed schedule, addressable by task UID.
#[derive(Clone, Debug)]
pub struct Schedule {
    results: HashMap<i32, TaskResult>,
    /// Each summary's rolled-up span, see [`Schedule::rolled_up`].
    rollups: HashMap<i32, (DateTime, DateTime)>,
    pub project_start: DateTime,
    pub project_finish: DateTime,
}

impl Schedule {
    pub fn get(&self, uid: i32) -> Option<&TaskResult> {
        self.results.get(&uid)
    }

    /// A summary's rolled-up span: the earliest start and latest finish of
    /// its scheduled descendant leaves and of its descendant manual summaries,
    /// which contribute their own (manual) dates. An auto summary's early
    /// dates are this span; a manual summary keeps its manual dates and this
    /// is the span Project draws beside them. `None` for leaves and for a
    /// summary with nothing scheduled below it.
    pub fn rolled_up(&self, uid: i32) -> Option<(DateTime, DateTime)> {
        self.rollups.get(&uid).copied()
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
    /// and late dates under conflicting constraints. Calendars without weekly
    /// working time need none.
    fn origin(cal: &WorkCalendar, anchor: i64, min_before: i64) -> i64 {
        if min_before <= 0 || !cal.has_working_time() {
            return anchor;
        }
        let mut day = anchor.div_euclid(1440);
        let mut work = 0;
        let mut slots = Vec::new();
        for _ in 0..HORIZON_DAYS {
            day -= 1;
            cal.day_into(day, &mut slots);
            work += slots.iter().map(|t| (t.to - t.from) as i64).sum::<i64>();
            if work >= min_before {
                break;
            }
        }
        day * 1440
    }

    /// Build a timeline for `cal` starting at `origin_abs`, extended until it
    /// covers at least `min_total` working minutes after `anchor_abs` and
    /// reaches wall-clock minute `min_reach`.
    fn build(
        cal: &WorkCalendar,
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
        let mut slots = Vec::new();
        loop {
            cal.day_into(day, &mut slots);
            let floor = if first { origin_mod } else { 0 };
            first = false;
            for &WorkingTime { from, to } in &slots {
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

    /// Clamp an instant into the span this timeline covers.
    fn clamp(&self, abs: i64) -> i64 {
        match (self.segs.first(), self.segs.last()) {
            (Some(first), Some(last)) => abs.clamp(first.start, last.end),
            _ => abs,
        }
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
    weeks: HashMap<i32, WorkCalendar>,
    default_cal: i32,
    anchor: i64,
    /// No timeline starts before this day, however far back a date lies.
    earliest_origin: i64,
    /// Each task's working duration, which a percent lag is a share of.
    durations: HashMap<i32, i64>,
    /// The calendar summaries measure their spans and slack on.
    summary_cal: WorkCalendar,
    /// Each manual summary's own start and finish (absolute minutes).
    manual_spans: HashMap<i32, (i64, i64)>,
    /// The start no ASAP auto leaf may precede: its nearest manual-summary
    /// ancestor's start.
    floors: HashMap<i32, i64>,
}

struct ConstraintDates {
    start: i64,
    finish: i64,
    floor: i64,
    /// The unclamped constraint instant.
    raw: i64,
    /// An unlinked task's date fell before the project-start floor.
    clamped: bool,
    finish_bound: Option<i64>,
    /// A milestone's finish-constraint instant: the date itself when it is a
    /// working start instant (a morning deadline stays on its morning), else
    /// the evening that shares its index.
    milestone: i64,
}

impl<'a> Scheduler<'a> {
    fn new(proj: &'a Project) -> Scheduler<'a> {
        // Anchor: explicit project start, else the earliest stored or pinned
        // start of a leaf `run()` schedules, else a fixed Monday, snapped to
        // the default calendar's first working instant. Summaries and leaves
        // without working time never place the anchor.
        let default_cal = proj.default_calendar_uid;
        let calendars = CalendarResolver::new(proj);
        let schedulable = proj.tasks.iter().filter(|t| calendars.schedulable(t));
        let pinned_starts = schedulable
            .clone()
            .filter_map(|t| t.pinned_dates().map(|(start, _)| start.minutes()));
        let raw_anchor = proj
            .start_date
            .or_else(|| {
                schedulable
                    .clone()
                    .flat_map(|t| [t.stored_start, t.pinned_dates().map(|(start, _)| start)])
                    .flatten()
                    .min()
            })
            .unwrap_or_else(|| DateTime::from_ymd_hm(2020, 1, 6, 8, 0))
            .minutes();

        // Horizon: enough working minutes for all work + lag, plus a wide
        // margin, and enough wall-clock reach to cover any far constraint date.
        let work: i64 = proj.tasks.iter().map(|t| t.duration_min.max(0)).sum();
        let durations = durations(proj);
        // Elapsed lag is calendar minutes, which over-estimates the working
        // minutes it spans: safe for a working-minute budget.
        let lag: i64 = proj
            .tasks
            .iter()
            .flat_map(|t| &t.predecessors)
            .map(|p| lag_minutes(p, &durations).saturating_abs())
            .fold(0, i64::saturating_add);
        let min_total = work.saturating_add(lag).saturating_add(HORIZON_PADDING_MIN);

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
            .filter(|t| is_backward(t.constraint));
        // A deadline bounds late dates like a backward constraint does.
        let deadlines = proj
            .tasks
            .iter()
            .filter(|t| !t.summary && t.deadline.is_some());
        let has_backward_constraints =
            backward_constraints.clone().next().is_some() || deadlines.clone().next().is_some();
        let linked = |t: &&Task| t.predecessors.iter().any(|p| leaf_uids.contains(&p.uid));
        // An unlinked deadline is floored at the anchor. Its raw date must not
        // add an unused prefix to every calendar's timeline.
        // A pinned start can precede the anchor, and a violated link into it
        // puts its predecessors' late dates earlier still.
        let earliest_backward_date = backward_constraints
            .filter(linked)
            .filter_map(|t| t.constraint_date)
            .chain(deadlines.filter(linked).filter_map(|t| t.deadline))
            .map(|date| date.minutes())
            .chain(pinned_starts.clone())
            .min();
        let needs_backward_horizon = has_backward_constraints
            || pinned_starts.clone().next().is_some()
            || proj.tasks.iter().filter(|t| !t.summary).any(|t| {
                t.predecessors.iter().any(|p| {
                    leaf_uids.contains(&p.uid)
                        && (lag_minutes(p, &durations) < 0
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
            .map(|cal| (cal.uid, calendars.calendar(cal)))
            .collect();
        weeks
            .entry(default_cal)
            .or_insert_with(|| WorkCalendar::weekly(Calendar::standard_week()));

        // A pinned task's duration-derived finish lies past its start; reserve
        // wall-clock reach for its duration on its calendar, resolved exactly
        // as `tl()` resolves it, exceptions included: the forward horizon is
        // counted from the anchor, not from a pinned start, so holidays in a
        // far task's span need reach of their own. A calendar without weekly
        // working time cannot schedule the task at all.
        let pinned_reach = proj.tasks.iter().filter_map(|t| {
            let (start, _) = t.pinned_dates()?;
            let cal = weeks
                .get(&t.calendar_uid.unwrap_or(default_cal))
                .or_else(|| weeks.get(&default_cal))?;
            cal.has_working_time()
                .then(|| reach_after(cal, start.minutes(), t.duration_min.max(0)))
        });
        // A manual summary's span, and room after its start for the subtasks
        // it floors there.
        let summary_cal = summary_calendar(proj);
        let manual_spans = manual_summary_spans(proj, &summary_cal);
        let floor_reach = manual_spans.values().flat_map(|&(start, finish)| {
            let floored = weeks
                .get(&default_cal)
                .filter(|cal| cal.has_working_time())
                .map(|cal| reach_after(cal, start, work));
            [Some(finish), floored].into_iter().flatten()
        });
        let far_dates = proj
            .tasks
            .iter()
            .flat_map(|t| [t.constraint_date, t.stored_finish, t.manual_finish])
            .flatten()
            .map(|d| d.minutes())
            .chain(pinned_reach)
            .chain(floor_reach)
            .max()
            .unwrap_or(raw_anchor);
        let min_reach = far_dates.max(raw_anchor) + 90 * 1440;
        // Keep the absolute cap tied to the project start, not to any date.
        let earliest_origin = (raw_anchor.div_euclid(1440) - HORIZON_DAYS) * 1440;
        let origin = if needs_backward_horizon {
            // Late predecessors need the whole work/lag budget before the
            // earliest backward constraint, even when it predates the anchor.
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
        for (&uid, week) in &weeks {
            let tl = Timeline::build(week, origin, raw_anchor, min_total, min_reach);
            if uid == default_cal && tl.total > 0 {
                anchor = tl.snap(raw_anchor);
            }
            timelines.insert(uid, tl);
        }

        Scheduler {
            proj,
            timelines,
            weeks,
            default_cal,
            anchor,
            earliest_origin,
            durations,
            floors: manual_summary_floors(proj, &manual_spans),
            summary_cal,
            manual_spans,
        }
    }

    /// The calendar whose timeline `tl()` returns for `task`.
    fn calendar_uid(&self, task: &Task) -> i32 {
        task.calendar_uid
            .filter(|uid| self.timelines.contains_key(uid))
            .unwrap_or(self.default_cal)
    }

    /// A link's lag as this run applies it.
    fn offset(&self, p: &Predecessor) -> Offset {
        let minutes = lag_minutes(p, &self.durations);
        match p.lag_format.kind() {
            LagKind::Elapsed => Offset::Elapsed(minutes),
            LagKind::Working | LagKind::Percent => Offset::Working(minutes),
        }
    }

    fn tl(&self, task: &Task) -> &Timeline {
        self.timelines
            .get(&self.calendar_uid(task))
            .expect("default timeline always present")
    }

    /// An unlinked task stays at the project start, but a backward constraint
    /// before it still needs a late window to report the miss. Measure those
    /// windows on per-calendar timelines of their own, so a stale template
    /// date cannot extend the shared origin that every other task uses.
    /// Index differences do not depend on where a timeline starts, so the
    /// earliest such date on a calendar cannot change another task's window;
    /// only the shared absolute cap can clamp it.
    fn pre_start_timelines(
        &self,
        leaves: &[usize],
        linked: &std::collections::HashSet<i32>,
    ) -> HashMap<i32, Timeline> {
        // Per calendar: earliest date, latest early start, longest duration.
        let mut spans: HashMap<i32, (i64, i64, i64)> = HashMap::new();
        for &i in leaves {
            let t = &self.proj.tasks[i];
            if linked.contains(&t.uid) || t.pinned_dates().is_some() || !is_backward(t.constraint) {
                continue;
            }
            let Some(dates) = self.constraint_dates(t, false).filter(|d| d.clamped) else {
                continue;
            };
            let span = spans
                .entry(self.calendar_uid(t))
                .or_insert((i64::MAX, i64::MIN, 0));
            span.0 = span.0.min(dates.raw);
            span.1 = span.1.max(dates.floor);
            span.2 = span.2.max(t.duration_min.max(0));
        }
        spans
            .into_iter()
            .map(|(uid, (earliest, reach, duration))| {
                let week = &self.weeks[&uid];
                // Room for the longest late window before the earliest date,
                // and past the early start for a start constraint's finish.
                // Windows at or before the absolute cap start at the cap.
                let budget = duration + 480;
                let origin = Timeline::origin(week, earliest, budget).max(self.earliest_origin);
                (
                    uid,
                    Timeline::build(week, origin, reach, budget, reach + 1440),
                )
            })
            .collect()
    }

    /// The earliest instant a task's dates may take. Only unlinked tasks
    /// retain the project-start floor; linked tasks can use the full horizon.
    fn date_floor(&self, task: &Task, linked: bool) -> i64 {
        let tl = self.tl(task);
        if linked {
            tl.abs_start(0)
        } else {
            tl.snap(self.anchor)
        }
    }

    /// Normalize dates identically for CPM and leveling, floored as
    /// [`Self::date_floor`] says.
    fn constraint_dates(&self, task: &Task, linked: bool) -> Option<ConstraintDates> {
        let tl = self.tl(task);
        let floor = self.date_floor(task, linked);
        let raw_date = task.constraint_date?.minutes();
        let date = raw_date.max(floor);
        let (finish, milestone) = finish_instants(tl, raw_date, floor);
        Some(ConstraintDates {
            start: tl.snap(date),
            finish,
            floor,
            milestone,
            raw: raw_date,
            // The forward pass keeps the floor; the backward pass measures
            // the raw date on a pre-start timeline (`pre_start_timelines`).
            clamped: !linked && raw_date < floor,
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
        let mut succs: HashMap<i32, Vec<(i32, LinkType, Offset)>> = HashMap::new();
        for &i in &leaves {
            let t = &self.proj.tasks[i];
            for p in &t.predecessors {
                if leaf_uids.contains(&p.uid) {
                    succs
                        .entry(p.uid)
                        .or_default()
                        .push((t.uid, p.link, self.offset(p)));
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
            let mut finish_bounds = Vec::new();
            for p in &t.predecessors {
                let Some(&pf_abs) = ef_abs.get(&p.uid) else {
                    continue;
                };
                let Some(&ps_abs) = es_abs.get(&p.uid) else {
                    continue;
                };
                let offset = self.offset(p);
                let (pf_abs, lag) = offset.forward(pf_abs);
                let (ps_abs, _) = offset.forward(ps_abs);
                let cand = match p.link {
                    LinkType::FinishStart if t.duration_min == 0 => {
                        let instant = fs_milestone_instant(tl, pf_abs, lag);
                        fs_milestone_start =
                            Some(fs_milestone_start.map_or(instant, |s| s.max(instant)));
                        instant
                    }
                    LinkType::FinishStart => tl.abs_start(tl.to_index(pf_abs) + lag),
                    LinkType::StartStart => tl.abs_start(tl.to_index(ps_abs) + lag),
                    LinkType::FinishFinish => {
                        let cf = tl.abs_finish(tl.to_index(pf_abs) + lag);
                        // An elapsed lag can end in nonworking time; the
                        // finish keeps that instant, as Project's does (#104).
                        if let Offset::Elapsed(_) = offset {
                            finish_bounds.push((tl.to_index(pf_abs), pf_abs));
                        }
                        tl.abs_start(tl.to_index(cf) - t.duration_min)
                    }
                    LinkType::StartFinish => {
                        let bound = sf_bound(tl, ps_abs, lag);
                        finish_bounds.push(bound);
                        tl.abs_start(bound.0 - t.duration_min)
                    }
                };
                linked_start = Some(linked_start.map_or(cand, |s| s.max(cand)));
            }
            // Only resolved leaf links may schedule a task before the anchor.
            let mut start_abs = linked_start.unwrap_or(self.anchor);
            let mut driven_start = start_abs;
            let mut finish_bound = None;
            let mut held_milestone: Option<i64> = None;
            if linked_start.is_some() {
                linked_tasks.insert(t.uid);
            }
            // A manual task stays where the user put it: links and constraints
            // never move it. Its start is kept unsnapped, as Project keeps it.
            // Only links drive its slack, so a violated link shows as negative
            // total slack and an unlinked task never does.
            if let Some((pinned_start, pinned_finish)) = t.pinned_dates() {
                // A date beyond the timeline is clamped into it, leaving room
                // for the duration, so the finish never precedes the start.
                let latest = tl.abs_start((tl.total - t.duration_min).max(0));
                let s_abs = tl.clamp(pinned_start.minutes()).min(latest);
                let s_idx = tl.to_index(s_abs);
                let f_abs = match pinned_finish {
                    Some(finish) => tl.clamp(finish.minutes()).max(s_abs),
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
            // An ASAP task starts no earlier than its nearest manual summary,
            // though a later link still wins. It is not a link: slack stays
            // measured from the links. Project ignores the floor for a task
            // with any constraint, even a start-no-earlier-than one.
            if t.constraint == ConstraintType::AsSoonAsPossible
                && let Some(&floor) = self.floors.get(&t.uid)
            {
                start_abs = start_abs.max(floor);
            }
            // Snap the constraint date as a start (next morning) or a finish
            // (this evening).
            if let Some(dates) = self.constraint_dates(t, linked_start.is_some()) {
                let ConstraintDates {
                    start: ds,
                    finish: df,
                    floor,
                    milestone,
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
                    ConstraintType::StartNoEarlierThan => {
                        // A milestone dated at the end of a working period
                        // occupies that instant, as a finish-constrained one
                        // does, not the next morning that shares its index.
                        let bound = if t.duration_min == 0 && milestone == dates.raw.max(floor) {
                            held_milestone = Some(milestone);
                            milestone
                        } else {
                            ds
                        };
                        start_abs = start_abs.max(bound);
                    }
                    ConstraintType::FinishNoEarlierThan => {
                        start_abs = start_abs
                            .max(tl.abs_start(tl.to_index(df) - t.duration_min).max(floor));
                    }
                    ConstraintType::MustFinishOn | ConstraintType::FinishNoLaterThan
                        if t.constraint == ConstraintType::MustFinishOn
                            || self.proj.honor_constraints =>
                    {
                        let previous_start = start_abs;
                        // A milestone occupies the constraint's own instant,
                        // not the next morning that shares its index.
                        start_abs = if t.duration_min == 0 {
                            held_milestone = Some(milestone);
                            milestone
                        } else {
                            tl.abs_start(tl.to_index(df) - t.duration_min)
                        };
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
            // nonworking gap, as does a binding MFO/FNLT milestone or one with
            // an SNET at a period's end. Later start-type links/constraints
            // still snap.
            let s_abs =
                if fs_milestone_start == Some(start_abs) || held_milestone == Some(start_abs) {
                    start_abs
                } else {
                    tl.snap(start_abs)
                };
            let s_idx = tl.to_index(s_abs);
            let f_idx = s_idx + t.duration_min;
            let f_abs =
                finish_instant(t, tl, s_abs, f_idx, finish_bounds.into_iter(), finish_bound);
            driven_es.insert(t.uid, tl.to_index(driven_start));
            es.insert(t.uid, s_idx);
            ef.insert(t.uid, f_idx);
            es_abs.insert(t.uid, s_abs);
            ef_abs.insert(t.uid, f_abs);
        }

        // A manual summary's finish extends the project finish, so every
        // task's slack is measured to it.
        let project_finish_abs = ef_abs
            .values()
            .copied()
            .chain(self.manual_spans.values().map(|&(_, finish)| finish))
            .max()
            .unwrap_or(self.anchor);
        let pre_start = self.pre_start_timelines(&leaves, &linked_tasks);

        // ---- backward pass: late finish / late start ----
        let mut lf_abs: HashMap<i32, i64> = HashMap::new();
        let mut ls_abs: HashMap<i32, i64> = HashMap::new();
        // Tasks whose late window was measured on a pre-start timeline.
        let mut late_tl: HashMap<i32, &Timeline> = HashMap::new();
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
            // The latest instant the successor bounds allow. A milestone's
            // start and finish are one instant, so a zero-lag link bounds it
            // by the successor's own late start (FS, SS) or late finish (FF,
            // SF), even on the morning side of the index `finish_abs` holds.
            let mut bound_instant = project_finish_abs;
            if let Some(list) = succs.get(&t.uid) {
                for &(suid, link, offset) in list {
                    let lag = offset.index_lag();
                    let sls = ls_abs.get(&suid).map(|&x| offset.backward(x).0);
                    let slf = lf_abs.get(&suid).map(|&x| offset.backward(x).0);
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
                        let endpoint = match link {
                            LinkType::FinishStart | LinkType::StartStart => sls,
                            LinkType::FinishFinish | LinkType::StartFinish => slf,
                        };
                        let allowed = match endpoint {
                            Some(x) if span == 0 && lag == 0 => x,
                            _ => c,
                        };
                        bound_instant = bound_instant.min(allowed);
                    }
                }
            }
            // Hard constraints (backward-affecting).
            let mut hard_finish_bound = false;
            // A milestone's MFO/FNLT/deadline instant, kept when the bound
            // binds. An FNLT or deadline never moves it past what the
            // successors allow on its own working index.
            let mut milestone_late = None;
            let mut pre_start_window = None;
            if let Some(dates) = self
                .constraint_dates(t, linked_tasks.contains(&t.uid))
                .filter(|_| !pinned)
            {
                let ConstraintDates {
                    start: ds,
                    finish: df,
                    milestone,
                    ..
                } = dates;
                // A clamped date keeps its raw instant on the pre-start
                // timeline. It still competes with successor bounds, which
                // can lie earlier still, in absolute time.
                if let Some(pre) = pre_start
                    .get(&self.calendar_uid(t))
                    .filter(|_| dates.clamped && is_backward(t.constraint))
                {
                    let finish_constraint = matches!(
                        t.constraint,
                        ConstraintType::MustFinishOn | ConstraintType::FinishNoLaterThan
                    );
                    let finish_index = if finish_constraint {
                        pre.to_index(dates.raw)
                    } else {
                        pre.to_index(pre.snap(dates.raw)) + t.duration_min
                    };
                    // A date at or before the absolute cap is clamped there,
                    // keeping the full duration after the first instant.
                    let mut finish_index = finish_index.max(t.duration_min);
                    // A start constraint's window can finish after the
                    // project start, where a deadline may bound it tighter.
                    // Like any unlinked deadline it is floored at the project
                    // start, so it never binds a finish constraint's earlier
                    // raw date, nor a milestone's window, which ends by then.
                    if let Some(deadline) = t.deadline.filter(|_| !finish_constraint) {
                        let (d_f, _) = finish_instants(pre, deadline.minutes(), dates.floor);
                        let d_index = pre.to_index(d_f).max(t.duration_min);
                        if d_index < finish_index {
                            debug_assert_ne!(t.duration_min, 0);
                            finish_index = d_index;
                        }
                    }
                    let f_abs = pre.abs_finish(finish_index);
                    let binds = matches!(
                        t.constraint,
                        ConstraintType::MustFinishOn | ConstraintType::MustStartOn
                    ) || f_abs <= finish_abs;
                    if binds {
                        let (s_abs, f_abs) = if t.duration_min != 0 {
                            (pre.abs_start(finish_index - t.duration_min), f_abs)
                        } else if finish_constraint {
                            // As `milestone`, on the pre-start timeline: a
                            // morning deadline stays on its morning.
                            let mut m = if pre.snap(dates.raw) == dates.raw {
                                dates.raw
                            } else {
                                f_abs
                            };
                            if t.constraint == ConstraintType::FinishNoLaterThan {
                                m = m.min(bound_instant);
                            }
                            (m, m)
                        } else {
                            (f_abs, f_abs)
                        };
                        pre_start_window = Some((pre, s_abs, f_abs));
                    }
                }
                match t.constraint {
                    _ if pre_start_window.is_some() => {}
                    ConstraintType::MustFinishOn => {
                        finish_abs = df;
                        hard_finish_bound = true;
                        milestone_late = (span == 0).then_some(milestone);
                    }
                    ConstraintType::FinishNoLaterThan if df <= finish_abs => {
                        finish_abs = df;
                        milestone_late =
                            (span == 0).then(|| capped_milestone(tl, milestone, bound_instant));
                        hard_finish_bound = true;
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
            // A deadline bounds the late finish like an FNLT, but it is not a
            // constraint: it applies whatever HonorConstraints says and never
            // moves scheduled dates. A pre-start window has already taken it
            // into account.
            if let Some(deadline) = t.deadline.filter(|_| !pinned && pre_start_window.is_none()) {
                let floor = self.date_floor(t, linked_tasks.contains(&t.uid));
                let (df, milestone) = finish_instants(tl, deadline.minutes(), floor);
                if df <= finish_abs {
                    finish_abs = df;
                    hard_finish_bound = true;
                    if span == 0 {
                        // An MFO/MSO can have set `finish_abs` past the
                        // successor bounds, so only a bound on the deadline's
                        // own index caps it.
                        let m = capped_milestone(tl, milestone, bound_instant);
                        milestone_late = Some(milestone_late.map_or(m, |late: i64| late.min(m)));
                    }
                }
            }
            let finish_index = tl.to_index(finish_abs);
            let (s_abs, f_abs) = if let Some((pre, s_abs, f_abs)) = pre_start_window {
                late_tl.insert(t.uid, pre);
                (s_abs, f_abs)
            } else if finish_abs == ef_abs[&t.uid]
                || (finish_index == ef[&t.uid] && !hard_finish_bound)
            {
                // An SF endpoint (or milestone) can be the morning side of a
                // gap. Successor bounds can remap that index to the evening;
                // reuse the early instants unless a hard date set that bound.
                (es_abs[&t.uid], ef_abs[&t.uid])
            } else {
                // A binding MFO/FNLT/deadline milestone sits on its own
                // instant (an FNLT or deadline capped by the successor bound),
                // which can be the morning side of that date's index.
                if let Some(m) = milestone_late {
                    debug_assert_eq!(tl.to_index(m), finish_index);
                }
                let f_abs = milestone_late.unwrap_or_else(|| tl.abs_finish(finish_index));
                let s_abs = if span == 0 {
                    f_abs
                } else {
                    tl.abs_start(finish_index - span)
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
            let (early, driven) = (tl.to_index(e_s), driven_es[&t.uid]);
            let late = late_tl.get(&t.uid);
            let mut total = match late {
                // Unlinked, so nothing but the anchor drives its start.
                Some(pre) => {
                    debug_assert_eq!(early, driven);
                    pre.to_index(l_s) - pre.to_index(e_s)
                }
                None => tl.to_index(l_s) - early.max(driven),
            };
            // Late span minus early span, on the timeline `total` used: zero
            // unless an endpoint lands on a different instant of its index.
            let span_gap = {
                let on: &Timeline = late.map_or(tl, |pre| pre);
                (on.to_index(l_f) - on.to_index(l_s)) - (on.to_index(e_f) - on.to_index(e_s))
            };
            // A manual task pinned before its link-driven start violates the
            // link: report at least that gap as negative slack, even off the
            // critical path.
            if t.pinned_dates().is_some() && driven > early {
                total = total.min(early - driven);
            }
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
                    start_slack_min: total,
                    finish_slack_min: total + span_gap,
                    critical: total <= 0,
                },
            );
        }

        // ---- summary rollup ----
        // Deepest first, so a nested manual summary's result exists when its
        // ancestors roll it up.
        let cal = &self.summary_cal;
        let slack = |early: DateTime, late: DateTime| {
            let gap = working_minutes_on(cal, early, late);
            if late < early { -gap } else { gap }
        };
        let mut rollups = HashMap::new();
        for i in summaries_deepest_first(self.proj) {
            let t = &self.proj.tasks[i];
            let nodes: Vec<Node> = rollup_nodes(self.proj, i, &leaves)
                .filter_map(|k| {
                    let r = results.get(&self.proj.tasks[k].uid)?;
                    Some(if self.proj.tasks[k].summary {
                        // Every ancestor, manual or auto, sees a manual
                        // summary's own span as fixed: its late window is its
                        // early one (Project, probes r1 and r1b).
                        Node {
                            late_start: r.early_start,
                            late_finish: r.early_finish,
                            fixed: true,
                            ..Node::from(r)
                        }
                    } else {
                        Node::from(r)
                    })
                })
                .collect();
            let span = (
                nodes.iter().map(|n| n.start).min(),
                nodes.iter().map(|n| n.finish).max(),
            );
            if let (Some(start), Some(finish)) = span {
                rollups.insert(t.uid, (start, finish));
            }
            let result = match self.manual_spans.get(&t.uid) {
                // Nothing scheduled below: the manual dates alone.
                Some(&(start, finish)) if nodes.is_empty() => {
                    let (start, finish) = (
                        DateTime::from_minutes(start),
                        DateTime::from_minutes(finish),
                    );
                    TaskResult {
                        uid: t.uid,
                        early_start: start,
                        early_finish: finish,
                        late_start: start,
                        late_finish: finish,
                        total_slack_min: 0,
                        free_slack_min: 0,
                        start_slack_min: 0,
                        finish_slack_min: 0,
                        critical: false,
                    }
                }
                // A manual summary keeps its dates. Its late window spans its
                // subtasks' late dates, never starting before its own start
                // nor finishing before its own finish, so its slack is never
                // negative. It is critical only when that slack is zero:
                // subtasks on the critical path do not make it so.
                Some(&(start, finish)) => {
                    let (start, finish) = (
                        DateTime::from_minutes(start),
                        DateTime::from_minutes(finish),
                    );
                    let late_start = nodes.iter().map(|n| n.late_start).min().unwrap().max(start);
                    let late_finish = nodes
                        .iter()
                        .map(|n| n.late_finish)
                        .max()
                        .unwrap()
                        .max(finish);
                    let (start_slack, finish_slack) =
                        (slack(start, late_start), slack(finish, late_finish));
                    let total = start_slack.min(finish_slack);
                    TaskResult {
                        uid: t.uid,
                        early_start: start,
                        early_finish: finish,
                        late_start,
                        late_finish,
                        total_slack_min: total,
                        free_slack_min: total.max(0),
                        start_slack_min: start_slack,
                        finish_slack_min: finish_slack,
                        critical: total <= 0,
                    }
                }
                None => {
                    let (Some(es_min), Some(ef_max)) = span else {
                        continue;
                    };
                    let ls_min = nodes.iter().map(|n| n.late_start).min().unwrap();
                    let lf_max = nodes.iter().map(|n| n.late_finish).max().unwrap();
                    let (start_slack, finish_slack) =
                        (slack(es_min, ls_min), slack(ef_max, lf_max));
                    // Over a manual summary, Project measures the slack of
                    // this late window, which the manual summary's fixed span
                    // bounds: its subtasks' slack does not free it.
                    let (total, critical) = if nodes.iter().any(|n| n.fixed) {
                        let total = start_slack.min(finish_slack);
                        (total, total <= 0)
                    } else {
                        (
                            nodes.iter().map(|n| n.total).min().unwrap(),
                            nodes.iter().any(|n| n.critical),
                        )
                    };
                    TaskResult {
                        uid: t.uid,
                        early_start: es_min,
                        early_finish: ef_max,
                        late_start: ls_min,
                        late_finish: lf_max,
                        total_slack_min: total,
                        free_slack_min: total.max(0),
                        start_slack_min: start_slack,
                        finish_slack_min: finish_slack,
                        critical,
                    }
                }
            };
            results.insert(t.uid, result);
        }

        Schedule {
            results,
            rollups,
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
        succs: &HashMap<i32, Vec<(i32, LinkType, Offset)>>,
    ) -> Option<i64> {
        let list = succs.get(&t.uid)?;
        let ef_idx = tl.to_index(es_abs[&t.uid]) + t.duration_min;
        let mut min_gap: Option<i64> = None;
        for &(suid, link, offset) in list {
            if link != LinkType::FinishStart {
                continue;
            }
            let (succ_es, lag) = offset.backward(*es_abs.get(&suid)?);
            let gap = tl.to_index(succ_es) - lag - ef_idx;
            min_gap = Some(min_gap.map_or(gap, |m: i64| m.min(gap)));
        }
        min_gap
    }
}

/// The wall-clock instant at which `work` working minutes on `cal`, counted
/// from `start`, are done; at most [`HORIZON_DAYS`] days past `start`.
fn reach_after(cal: &WorkCalendar, start: i64, mut work: i64) -> i64 {
    let mut slots = Vec::new();
    let first = start.div_euclid(1440);
    for day in first..=first + HORIZON_DAYS {
        cal.day_into(day, &mut slots);
        for t in &slots {
            let from = (day * 1440 + i64::from(t.from)).max(start);
            let to = day * 1440 + i64::from(t.to);
            if from < to {
                if work <= to - from {
                    return from + work;
                }
                work -= to - from;
            }
        }
    }
    start.saturating_add(HORIZON_DAYS * 1440)
}

/// A finish date's instants on `tl`, floored at `floor`: the evening that
/// completes its working index, and a milestone's instant, which is the date
/// itself when it is a working start instant (a morning deadline stays on its
/// morning), else that evening.
fn finish_instants(tl: &Timeline, date: i64, floor: i64) -> (i64, i64) {
    let date = date.max(floor);
    let finish = tl.abs_finish(tl.to_index(date)).max(floor);
    let milestone = if tl.snap(date) == date { date } else { finish };
    (finish, milestone)
}

/// A milestone's late instant under a finish bound whose own instant is
/// `milestone`: the successors' zero-lag `bound_instant` caps it only on the
/// same working index (its morning or evening side). An MFO/MSO overrides the
/// successor bounds, so a bound on an earlier index must not pull it there.
fn capped_milestone(tl: &Timeline, milestone: i64, bound_instant: i64) -> i64 {
    if tl.to_index(bound_instant) == tl.to_index(milestone) {
        milestone.min(bound_instant)
    } else {
        milestone
    }
}

/// Constraints that bound a task's late dates.
fn is_backward(constraint: ConstraintType) -> bool {
    matches!(
        constraint,
        ConstraintType::FinishNoLaterThan
            | ConstraintType::StartNoLaterThan
            | ConstraintType::MustFinishOn
            | ConstraintType::MustStartOn
    )
}

/// A link's lag resolved for one run: working minutes on the successor's
/// calendar (a percent lag resolves to these), or elapsed calendar minutes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Offset {
    Working(i64),
    Elapsed(i64),
}

impl Offset {
    /// The predecessor-side instant a successor bound counts from, and the
    /// working lag still to add in index space. Project applies an elapsed
    /// lag as wall-clock time and then treats the link as a zero-lag one from
    /// that instant, keeping it even in nonworking time (#104): an FS
    /// milestone or an FF/SF finish sits there, other starts snap forward.
    fn forward(self, abs: i64) -> (i64, i64) {
        match self {
            Offset::Working(lag) => (abs, lag),
            Offset::Elapsed(lag) => (abs.saturating_add(lag), 0),
        }
    }

    /// The lag left to apply in index space once the instant is shifted.
    fn index_lag(self) -> i64 {
        match self {
            Offset::Working(lag) => lag,
            Offset::Elapsed(_) => 0,
        }
    }

    /// [`Self::forward`] mirrored for a successor's late instant: the lag is
    /// still to be subtracted in index space.
    fn backward(self, abs: i64) -> (i64, i64) {
        match self {
            Offset::Working(lag) => (abs, lag),
            Offset::Elapsed(lag) => (abs.saturating_sub(lag), 0),
        }
    }
}

/// Each task's working duration by UID; the first of a duplicated UID wins,
/// as [`Project::task`] finds it.
fn durations(proj: &Project) -> HashMap<i32, i64> {
    let mut out = HashMap::new();
    for t in &proj.tasks {
        out.entry(t.uid).or_insert(t.duration_min);
    }
    out
}

/// A link's lag in minutes of its kind (see [`Predecessor::lag_minutes`]); a
/// percent of a missing predecessor is zero. No timeline spans more than
/// [`MAX_LAG_MIN`], so a longer lag (an overflowing percent included) is
/// clamped to it: it lands past either end all the same, and index and
/// horizon arithmetic cannot overflow.
fn lag_minutes(p: &Predecessor, durations: &HashMap<i32, i64>) -> i64 {
    let duration = durations.get(&p.uid).copied().unwrap_or(0);
    p.lag_minutes(duration)
        .unwrap_or(if p.lag < 0 { i64::MIN } else { i64::MAX })
        .clamp(-MAX_LAG_MIN, MAX_LAG_MIN)
}

/// Wall-clock minutes of the widest timeline, plus a day.
const MAX_LAG_MIN: i64 = (2 * HORIZON_DAYS + 2) * 1440;

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
    finish_bounds: impl Iterator<Item = (i64, i64)>,
    finish_bound: Option<i64>,
) -> i64 {
    if task.duration_min == 0 {
        return start;
    }
    // A constraint's evening can share an index with an SF morning. Only
    // preserve SF instants allowed by the active finish bound; selecting an
    // instant never changes the task's working duration.
    if let Some(instant) = finish_bounds
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

fn has_working_time(week: &Week) -> bool {
    week.iter()
        .flat_map(|day| &day.times)
        .any(|t| t.from < t.to)
}

/// Resolves a task's calendar exactly as the scheduler does: the last calendar
/// with a UID wins, an unknown `calendar_uid` falls back to the default, and a
/// missing default is a synthesized Standard (which always has working time).
/// A derived calendar's working time comes from its base chain, found by the
/// same rule.
struct CalendarResolver<'a> {
    calendars: HashMap<i32, &'a Calendar>,
    default_uid: i32,
}

impl<'a> CalendarResolver<'a> {
    fn new(proj: &'a Project) -> Self {
        CalendarResolver {
            calendars: proj.calendars.iter().map(|cal| (cal.uid, cal)).collect(),
            default_uid: proj.default_calendar_uid,
        }
    }

    /// The calendar `calendar_uid` schedules on; `None` for the synthesized
    /// Standard.
    fn resolve(&self, calendar_uid: Option<i32>) -> Option<&'a Calendar> {
        self.calendars
            .get(&calendar_uid.unwrap_or(self.default_uid))
            .or_else(|| self.calendars.get(&self.default_uid))
            .copied()
    }

    /// `cal`'s working week, resolved through its base chain. Whether a
    /// calendar can schedule at all is decided on this weekly pattern alone.
    fn week(&self, cal: &'a Calendar) -> Week {
        cal.resolve_week(|uid| self.calendars.get(&uid).copied())
    }

    /// `cal`'s working time by date, exceptions included, resolved through its
    /// base chain.
    fn calendar(&self, cal: &'a Calendar) -> WorkCalendar {
        cal.resolve(|uid| self.calendars.get(&uid).copied())
    }

    /// A leaf the scheduler keeps: its resolved calendar has working time.
    fn schedulable(&self, task: &Task) -> bool {
        !task.summary
            && !task.is_null
            && self
                .resolve(task.calendar_uid)
                .is_none_or(|cal| has_working_time(&self.week(cal)))
    }
}

/// Reject tasks that have no working time and are leaves either in the stored
/// schedule or in the outline the editor uses to recompute summary flags.
pub(crate) fn calendar_error(proj: &Project) -> Option<String> {
    let calendars = CalendarResolver::new(proj);
    for (i, task) in proj.tasks.iter().enumerate() {
        if task.is_null || (task.summary && proj.is_outline_summary(i)) {
            continue;
        }
        let Some(cal) = calendars.resolve(task.calendar_uid) else {
            // The scheduler synthesizes Standard when the default is absent.
            continue;
        };
        if !has_working_time(&calendars.week(cal)) {
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

/// Project's warning on a manually scheduled task, from its shown finishes
/// and rollups: a manual summary whose subtasks finish after its own finish,
/// or a manual task (summary or leaf) finishing after its direct parent when
/// that parent is a manual summary. An auto summary in between is a direct
/// parent that never warns, and starting early never warns.
pub fn manual_warning(
    proj: &Project,
    uid: i32,
    finish: impl Fn(i32) -> Option<DateTime>,
    rollup: impl Fn(i32) -> Option<(DateTime, DateTime)>,
) -> bool {
    let Some(i) = proj.tasks.iter().position(|t| t.uid == uid && !t.is_null) else {
        return false;
    };
    let task = &proj.tasks[i];
    let summary = task.manual_summary_dates().is_some();
    if !summary && task.pinned_dates().is_none() {
        return false;
    }
    let Some(own) = finish(uid) else {
        return false;
    };
    if summary && rollup(uid).is_some_and(|(_, rolled)| rolled > own) {
        return true;
    }
    proj.tasks[..i]
        .iter()
        .rev()
        .filter(|p| !p.is_null)
        .find(|p| p.outline_level < task.outline_level)
        .filter(|p| p.manual_summary_dates().is_some())
        .and_then(|p| finish(p.uid))
        .is_some_and(|parent| own > parent)
}

/// Summary task indices, deepest outline level first.
fn summaries_deepest_first(proj: &Project) -> Vec<usize> {
    let mut out: Vec<usize> = (0..proj.tasks.len())
        .filter(|&i| proj.tasks[i].summary)
        .collect();
    out.sort_by_key(|&i| std::cmp::Reverse(proj.tasks[i].outline_level));
    out
}

/// What summary `sidx` rolls up: the indices of its descendant scheduled
/// `leaves` and of its descendant manual summaries (with a start), including
/// leaves under those. Auto summaries in between are looked through.
/// Leaf membership is by position in `leaves` (sorted), so a dropped leaf
/// cannot pull in a scheduled task elsewhere that shares its UID.
fn rollup_nodes<'a>(
    proj: &'a Project,
    sidx: usize,
    leaves: &'a [usize],
) -> impl Iterator<Item = usize> + 'a {
    let level = proj.tasks[sidx].outline_level;
    proj.tasks
        .iter()
        .enumerate()
        .skip(sidx + 1)
        .take_while(move |(_, t)| t.outline_level > level)
        .filter(|&(i, t)| {
            if t.summary {
                t.manual_summary_dates().is_some()
            } else {
                leaves.binary_search(&i).is_ok()
            }
        })
        .map(|(i, _)| i)
}

/// One task a summary rolls up, with the dates it contributes.
struct Node {
    start: DateTime,
    finish: DateTime,
    late_start: DateTime,
    late_finish: DateTime,
    total: i64,
    critical: bool,
    /// A nested manual summary, whose span does not move.
    fixed: bool,
}

impl From<&TaskResult> for Node {
    fn from(r: &TaskResult) -> Node {
        Node {
            start: r.early_start,
            finish: r.early_finish,
            late_start: r.late_start,
            late_finish: r.late_finish,
            total: r.total_slack_min,
            critical: r.critical,
            fixed: false,
        }
    }
}

/// Each manual summary's own span in absolute minutes: its start, and its
/// manual finish, else its start plus its manual duration on the summary
/// calendar, else its start. A finish before the start is raised to it.
fn manual_summary_spans(proj: &Project, cal: &WorkCalendar) -> HashMap<i32, (i64, i64)> {
    proj.tasks
        .iter()
        .filter_map(|t| {
            let (start, finish) = t.manual_summary_dates()?;
            let start = start.minutes();
            let finish = match (finish, t.manual_duration_min) {
                (Some(finish), _) => finish.minutes(),
                (None, Some(work)) if work > 0 && cal.has_working_time() => {
                    reach_after(cal, start, work)
                }
                _ => start,
            };
            Some((t.uid, (start, finish.max(start))))
        })
        .collect()
}

/// Each leaf's floor: the start of its nearest manual-summary ancestor. Auto
/// summaries and a manual summary with no start pass their parent's on.
fn manual_summary_floors(proj: &Project, spans: &HashMap<i32, (i64, i64)>) -> HashMap<i32, i64> {
    let mut floors = HashMap::new();
    if spans.is_empty() {
        return floors;
    }
    // (outline level, floor) of the summaries enclosing the current task.
    let mut stack: Vec<(u32, Option<i64>)> = Vec::new();
    for t in &proj.tasks {
        let level = t.outline_level;
        while stack.last().is_some_and(|&(l, _)| l >= level) {
            stack.pop();
        }
        let inherited = stack.last().and_then(|&(_, floor)| floor);
        if t.summary {
            let own = spans.get(&t.uid).map(|&(start, _)| start);
            stack.push((level, own.or(inherited)));
        } else if let Some(floor) = inherited {
            floors.insert(t.uid, floor);
        }
    }
    floors
}

/// Schedule a project: run the CPM forward and backward passes and return the
/// computed [`Schedule`].
/// Leaves with no working time have no result. Readers reject files containing
/// such leaves, and structural editor operations validate newly exposed leaves.
///
/// Task UIDs must be unique: results, links and assignments are keyed by UID.
/// Readers reject duplicates; a code-built project must not contain them.
pub fn schedule(proj: &Project) -> Schedule {
    Scheduler::new(&without_blank_rows(proj)).run()
}

/// The project the scheduler sees: blank rows (`is_null`) removed, with the
/// links and assignments that name them. A blank row therefore gets no
/// result, never bounds a summary, and cannot drive another task.
fn without_blank_rows(proj: &Project) -> std::borrow::Cow<'_, Project> {
    if !proj.tasks.iter().any(|t| t.is_null) {
        return std::borrow::Cow::Borrowed(proj);
    }
    let blank: std::collections::HashSet<i32> = proj
        .tasks
        .iter()
        .filter(|t| t.is_null)
        .map(|t| t.uid)
        .collect();
    let mut kept = proj.clone();
    kept.tasks.retain(|t| !t.is_null);
    for t in &mut kept.tasks {
        t.predecessors.retain(|p| !blank.contains(&p.uid));
    }
    kept.assignments.retain(|a| !blank.contains(&a.task_uid));
    std::borrow::Cow::Owned(kept)
}

/// Working minutes between two wall-clock instants under the project's default
/// calendar. Used when importing a file that stores computed wall-clock
/// start/finish (a `.mpp`) but not an explicit working-minute duration: the
/// duration is `working_minutes_between(start, finish)`. A summary's duration
/// is measured by [`task_duration_min`] instead, which falls back to the leaves'
/// calendars when this one has no working time.
pub fn working_minutes_between(proj: &Project, start: DateTime, finish: DateTime) -> i64 {
    let cal = match proj.calendar(proj.default_calendar_uid) {
        Some(cal) => proj.resolved_calendar(cal),
        None => WorkCalendar::weekly(Calendar::standard_week()),
    };
    working_minutes_on(&cal, start, finish)
}

/// Working minutes between two wall-clock instants on one calendar, counted
/// through the same timeline the scheduler uses.
pub(crate) fn working_minutes_on(cal: &WorkCalendar, start: DateTime, finish: DateTime) -> i64 {
    let a = start.minutes().min(finish.minutes());
    let b = start.minutes().max(finish.minutes());
    let tl = Timeline::build(cal, a, a, (b - a) + 480, b + 1440);
    (tl.to_index(b) - tl.to_index(a)).max(0)
}

/// A task's scheduled duration in working minutes: a leaf's own
/// `duration_min`, or for a summary the working time spanned by its scheduled
/// early start/finish (rolled up, or a manual summary's own span). The stored `duration_min` of a summary is never
/// recomputed, so every surface that shows one must derive it here. `None` when
/// the task has no schedule result.
pub fn task_duration_min(proj: &Project, sched: &Schedule, task: &Task) -> Option<i64> {
    let r = sched.get(task.uid)?;
    Some(summary_or_leaf_min(
        proj,
        task,
        r.early_start,
        r.early_finish,
    ))
}

/// The one rule behind [`task_duration_min`] and the editor's displayed
/// duration, which passes leveled dates instead of early ones.
pub(crate) fn summary_or_leaf_min(
    proj: &Project,
    task: &Task,
    start: DateTime,
    finish: DateTime,
) -> i64 {
    if task.summary {
        working_minutes_on(&summary_calendar(proj), start, finish)
    } else {
        task.duration_min
    }
}

/// The calendar every summary is measured on. MS Project uses the project
/// calendar, resolved as the scheduler resolves it, exceptions included. When
/// that calendar has no weekly working time (allowed as long as every leaf uses
/// its own working calendar), the substitute is the per-date union of the
/// working time of the calendars of all schedulable leaves, so equal dates
/// still mean equal durations across summaries.
fn summary_calendar(proj: &Project) -> WorkCalendar {
    let calendars = CalendarResolver::new(proj);
    let Some(default) = calendars.resolve(None) else {
        return WorkCalendar::weekly(Calendar::standard_week());
    };
    if has_working_time(&calendars.week(default)) {
        return calendars.calendar(default);
    }
    let mut seen = std::collections::HashSet::new();
    WorkCalendar::union(
        proj.tasks
            .iter()
            .filter(|t| calendars.schedulable(t))
            .filter_map(|t| calendars.resolve(t.calendar_uid))
            .filter(|cal| seen.insert(cal.uid))
            .map(|cal| calendars.calendar(cal))
            .collect(),
    )
}

// ---- resource leveling ------------------------------------------------------

/// The result of a resource-leveling pass: each task's leveled start/finish.
#[derive(Clone, Debug)]
pub struct Leveled {
    start: HashMap<i32, DateTime>,
    finish: HashMap<i32, DateTime>,
    rollups: HashMap<i32, (DateTime, DateTime)>,
    pub project_finish: DateTime,
}

impl Leveled {
    pub fn start(&self, uid: i32) -> Option<DateTime> {
        self.start.get(&uid).copied()
    }
    pub fn finish(&self, uid: i32) -> Option<DateTime> {
        self.finish.get(&uid).copied()
    }
    /// [`Schedule::rolled_up`] over the leveled dates.
    pub fn rolled_up(&self, uid: i32) -> Option<(DateTime, DateTime)> {
        self.rollups.get(&uid).copied()
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
    Scheduler::new(&without_blank_rows(proj)).level()
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
    /// A milestone that leveling leaves on its CPM working index, recomputed
    /// only because a predecessor's instant moved (#104). As in the CPM
    /// pass, it takes the latest leveled FS instant on that index, never
    /// earlier than its CPM instant; a non-FS link at an unchanged index
    /// cannot move it. An honored MSO/MFO keeps the CPM instant, and an
    /// honored FNLT/SNLT caps it, as the CPM pass does.
    fn releveled_milestone(
        &self,
        t: &Task,
        tl: &Timeline,
        cpm: &TaskResult,
        finish: &HashMap<i32, DateTime>,
        linked: bool,
    ) -> i64 {
        let cpm_start = cpm.early_start.minutes();
        let honored = |c| self.proj.honor_constraints && t.constraint == c;
        if honored(ConstraintType::MustFinishOn) || honored(ConstraintType::MustStartOn) {
            return cpm_start;
        }
        let placed = tl.to_index(cpm_start);
        let mut s_abs = cpm_start;
        for p in t
            .predecessors
            .iter()
            .filter(|p| p.link == LinkType::FinishStart)
        {
            let Some(&now) = finish.get(&p.uid) else {
                continue;
            };
            let (now, lag) = self.offset(p).forward(now.minutes());
            let now = fs_milestone_instant(tl, now, lag);
            if tl.to_index(now) == placed {
                s_abs = s_abs.max(now);
            }
        }
        let cap = self
            .constraint_dates(t, linked)
            .and_then(|dates| match t.constraint {
                ConstraintType::FinishNoLaterThan => Some(dates.milestone),
                ConstraintType::StartNoLaterThan => Some(dates.start),
                _ => None,
            })
            .filter(|_| self.proj.honor_constraints);
        match cap {
            Some(cap) => s_abs.min(cap.max(cpm_start)),
            None => s_abs,
        }
    }

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
                rollups: base.rollups.clone(),
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
        // Auto tasks whose leveled start or finish differs from CPM.
        let mut moved_start = std::collections::HashSet::new();
        let mut moved_finish = std::collections::HashSet::new();

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
            // Preserve every link's gap by inheriting the largest predecessor
            // delay. An elapsed lag counts wall-clock time from the
            // predecessor's leveled instant, so its gap is measured again
            // there: a delay that ends in nonworking time can move the
            // successor less than it moved the predecessor, or only its
            // instant (#104).
            let floor = t
                .predecessors
                .iter()
                .filter_map(|p| {
                    let delay = *delay.get(&p.uid)?;
                    let Offset::Elapsed(lag) = self.offset(p) else {
                        return Some(delay);
                    };
                    let pred = base.get(p.uid)?;
                    let (was, now) = match p.link {
                        LinkType::FinishStart | LinkType::FinishFinish => {
                            (pred.early_finish, *finish.get(&p.uid)?)
                        }
                        LinkType::StartStart | LinkType::StartFinish => {
                            (pred.early_start, *start.get(&p.uid)?)
                        }
                    };
                    let bound = |at: DateTime| tl.to_index(at.minutes().saturating_add(lag));
                    Some(bound(now) - bound(was))
                })
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
            // A predecessor can move only its instant, keeping its working
            // index (an elapsed lag into nonworking time, #104); a successor
            // that keeps that instant (a milestone, an SF finish) must follow.
            // Only the endpoint the link counts from matters.
            let pred_moved = t.predecessors.iter().any(|p| match p.link {
                LinkType::FinishStart | LinkType::FinishFinish => moved_finish.contains(&p.uid),
                LinkType::StartStart | LinkType::StartFinish => moved_start.contains(&p.uid),
            });
            let (s_abs, f_abs) = if placed == cpm_start_idx && floor == 0 && !pred_moved {
                (cpm.early_start.minutes(), cpm.early_finish.minutes())
            } else {
                let mut s_abs = tl.abs_start(placed);
                if t.duration_min == 0 && placed == cpm_start_idx {
                    let linked = t.predecessors.iter().any(|p| start.contains_key(&p.uid));
                    s_abs = self.releveled_milestone(t, tl, cpm, &finish, linked);
                } else if t.duration_min == 0 {
                    let mut fs_instant: Option<i64> = None;
                    let mut start_bound = cpm.early_start.minutes();
                    for p in &t.predecessors {
                        let (Some(ps), Some(pf)) = (start.get(&p.uid), finish.get(&p.uid)) else {
                            continue;
                        };
                        let offset = self.offset(p);
                        let (pf, lag) = offset.forward(pf.minutes());
                        let (ps, _) = offset.forward(ps.minutes());
                        if p.link == LinkType::FinishStart {
                            let instant = fs_milestone_instant(tl, pf, lag);
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
                            let index = tl.to_index(endpoint) + lag;
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
                let bounds = t.predecessors.iter().filter_map(|p| {
                    let offset = self.offset(p);
                    match p.link {
                        LinkType::StartFinish => start.get(&p.uid).map(|s| {
                            let (s, lag) = offset.forward(s.minutes());
                            sf_bound(tl, s, lag)
                        }),
                        // An elapsed FF lag keeps its instant, as in CPM.
                        LinkType::FinishFinish if matches!(offset, Offset::Elapsed(_)) => {
                            finish.get(&p.uid).map(|f| {
                                let (f, _) = offset.forward(f.minutes());
                                (tl.to_index(f), f)
                            })
                        }
                        _ => None,
                    }
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
            if s_abs != cpm.early_start.minutes() {
                moved_start.insert(t.uid);
            }
            if f_abs != cpm.early_finish.minutes() {
                moved_finish.insert(t.uid);
            }
            start.insert(t.uid, DateTime::from_minutes(s_abs));
            finish.insert(t.uid, DateTime::from_minutes(f_abs));
        }

        // Roll leveled dates up into summary tasks, deepest first: a manual
        // summary keeps its own dates, which its ancestors roll up.
        let mut rollups = HashMap::new();
        for i in summaries_deepest_first(self.proj) {
            let t = &self.proj.tasks[i];
            let uids: Vec<i32> = rollup_nodes(self.proj, i, &leaves)
                .map(|k| self.proj.tasks[k].uid)
                .collect();
            let cs = uids.iter().filter_map(|u| start.get(u).copied()).min();
            let cf = uids.iter().filter_map(|u| finish.get(u).copied()).max();
            if let (Some(s), Some(f)) = (cs, cf) {
                rollups.insert(t.uid, (s, f));
            }
            let own = match self.manual_spans.get(&t.uid) {
                Some(_) => base.get(t.uid).map(|r| (r.early_start, r.early_finish)),
                None => cs.zip(cf),
            };
            if let Some((s, f)) = own {
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
            rollups,
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
        Predecessor::fs(uid)
    }

    /// Phase (summary) over A and B, B after A, then C after B. With
    /// `blank`, a blank row sits between A and B at outline level 0, claims
    /// to be a summary, links to A, is linked from B and C, has a resource,
    /// and uses a calendar with no working time.
    fn blank_row_project(blank: bool) -> Project {
        let mut phase = task(1, "Phase", 0);
        phase.summary = true;
        let mut a = task(2, "A", 960);
        a.outline_level = 2;
        let mut b = task(4, "B", 480);
        b.outline_level = 2;
        b.predecessors.push(fs(2));
        let mut c = task(5, "C", 480);
        c.predecessors.push(fs(4));
        let mut proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![phase, a, b, c],
            resources: vec![Resource {
                uid: 1,
                name: "R".into(),
                max_units: 1.0,
                ..Resource::default()
            }],
            ..Project::default()
        };
        if blank {
            let mut row = task(3, "", 2400);
            row.outline_level = 0;
            row.is_null = true;
            row.summary = true;
            row.calendar_uid = Some(9);
            row.predecessors.push(fs(2));
            proj.tasks.insert(2, row);
            proj.tasks[3].predecessors.push(fs(3));
            proj.tasks[4].predecessors.push(fs(3));
            proj.calendars
                .push(Calendar::base(9, "Never", Default::default()));
            proj.assignments.push(Assignment {
                uid: 1,
                task_uid: 3,
                resource_uid: 1,
                units: 1.0,
                work_min: 2400,
                ..Assignment::default()
            });
        }
        proj
    }

    #[test]
    fn blank_rows_are_outside_the_schedule() {
        let (with, without) = (blank_row_project(true), blank_row_project(false));
        assert_eq!(calendar_error(&with), None);
        let (sched, plain) = (schedule(&with), schedule(&without));
        assert!(sched.get(3).is_none());
        for uid in [1, 2, 4, 5] {
            assert_eq!(sched.get(uid), plain.get(uid), "task {uid}");
        }
        // The summary still spans B, below the blank row.
        assert_eq!(
            sched.get(1).unwrap().early_finish,
            DateTime::from_ymd_hm(2026, 3, 4, 17, 0)
        );
        assert_eq!(sched.project_finish, plain.project_finish);
        let (leveled, plain) = (level(&with), level(&without));
        assert_eq!(leveled.start(3), None);
        for uid in [1, 2, 4, 5] {
            assert_eq!(leveled.start(uid), plain.start(uid), "task {uid}");
            assert_eq!(leveled.finish(uid), plain.finish(uid), "task {uid}");
        }
    }

    #[test]
    fn start_and_finish_slack() {
        // A (24h) and, under a summary, B (8h) with 16h of slack.
        let mut phase = task(2, "Phase", 0);
        phase.summary = true;
        let mut b = task(3, "B", 480);
        b.outline_level = 2;
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![task(1, "A", 1440), phase, b],
            ..Project::default()
        };
        let sched = schedule(&proj);
        for (uid, slack) in [(1, 0), (2, 960), (3, 960)] {
            let r = sched.get(uid).unwrap();
            assert_eq!(r.total_slack_min, slack, "task {uid}");
            assert_eq!(r.start_slack_min, slack, "task {uid}");
            assert_eq!(r.finish_slack_min, slack, "task {uid}");
        }
        // A milestone after an FS link: both endpoints are one instant.
        let proj = fs_milestone_project(480);
        let r = *schedule(&proj).get(2).unwrap();
        assert_eq!((r.start_slack_min, r.finish_slack_min), (0, 0));
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

    /// Milestone M, FS after A (or unlinked), with a finish constraint.
    fn finish_constrained_milestone(
        duration: i64,
        constraint: ConstraintType,
        date: DateTime,
        honor: bool,
        linked: bool,
    ) -> Project {
        let mut proj = fs_milestone_project(duration);
        proj.honor_constraints = honor;
        let milestone = &mut proj.tasks[1];
        if !linked {
            milestone.predecessors.clear();
        }
        milestone.constraint = constraint;
        milestone.constraint_date = Some(date);
        proj
    }

    fn assert_milestone_at(proj: &Project, expected: DateTime, slack: i64, case: &str) {
        let sched = schedule(proj);
        let m = sched.get(2).unwrap();
        assert_eq!(m.early_start, expected, "{case}");
        assert_eq!(m.early_finish, expected, "{case}");
        assert_eq!(m.late_start, expected, "{case}");
        assert_eq!(m.late_finish, expected, "{case}");
        assert_eq!(m.total_slack_min, slack, "{case}");
        let leveled = level(proj);
        assert_eq!(leveled.start(2), Some(expected), "{case}");
        assert_eq!(leveled.finish(2), Some(expected), "{case}");
    }

    #[test]
    fn finish_constrained_milestone_lands_at_constraint_evening() {
        use ConstraintType::{FinishNoLaterThan as Fnlt, MustFinishOn as Mfo};
        let friday = DateTime::from_ymd_hm(2026, 3, 6, 17, 0);
        // (A duration, constraint, honor, linked, total slack)
        for (duration, constraint, honor, linked, slack) in [
            (2400, Mfo, true, true, 0),
            (2400, Mfo, false, true, 0),
            (2880, Fnlt, true, true, -480),
            (2880, Mfo, true, true, -480),
            (2400, Mfo, true, false, 0),
        ] {
            let proj = finish_constrained_milestone(duration, constraint, friday, honor, linked);
            let case = format!("{duration} {constraint:?} honor={honor} linked={linked}");
            assert_milestone_at(&proj, friday, slack, &case);
        }
    }

    #[test]
    fn snet_milestone_at_a_working_period_end_stays_on_that_instant() {
        let at = |day, hour| DateTime::from_ymd_hm(2026, 3, day, hour, 0);
        // (A duration, linked, SNET, expected milestone instant)
        for (duration, linked, snet, expected) in [
            // The evening, not the next morning that shares its index.
            (480, false, at(3, 17), at(3, 17)),
            (480, true, at(3, 17), at(3, 17)),
            // The end of the morning period, likewise.
            (480, false, at(3, 12), at(3, 12)),
            // Not a period end: the next working start, as before.
            (480, false, at(3, 19), at(4, 8)),
            (480, false, at(3, 10), at(3, 10)),
            // A later FS link still wins.
            (1440, true, at(3, 17), at(4, 17)),
        ] {
            let mut proj = fs_milestone_project(duration);
            let milestone = &mut proj.tasks[1];
            if !linked {
                milestone.predecessors.clear();
            }
            milestone.constraint = ConstraintType::StartNoEarlierThan;
            milestone.constraint_date = Some(snet);
            let case = format!("{duration} linked={linked} {snet:?}");
            let m = *schedule(&proj).get(2).unwrap();
            assert_eq!(
                (m.early_start, m.early_finish),
                (expected, expected),
                "{case}"
            );
            let leveled = level(&proj);
            assert_eq!(leveled.start(2), Some(expected), "{case}");
            assert_eq!(leveled.finish(2), Some(expected), "{case}");
        }
    }

    #[test]
    fn finish_constrained_milestone_keeps_morning_deadline() {
        let monday = DateTime::from_ymd_hm(2026, 3, 9, 8, 0);
        let friday = DateTime::from_ymd_hm(2026, 3, 6, 17, 0);
        // A morning MFO date stays on its morning, early and late alike.
        for linked in [true, false] {
            let proj = finish_constrained_milestone(
                2400,
                ConstraintType::MustFinishOn,
                monday,
                true,
                linked,
            );
            assert_milestone_at(&proj, monday, 0, &format!("MFO Mon 08:00 linked={linked}"));
        }
        // Late dates keep the morning deadline when the early dates are later
        // (negative slack) or when a successor maps the index to the prior
        // evening. (constraint, honor, FS successor, early instant)
        let monday_evening = DateTime::from_ymd_hm(2026, 3, 9, 17, 0);
        for (constraint, honor, successor, early) in [
            (ConstraintType::MustFinishOn, false, false, monday_evening),
            (ConstraintType::FinishNoLaterThan, true, true, monday),
            (
                ConstraintType::FinishNoLaterThan,
                false,
                false,
                monday_evening,
            ),
        ] {
            let mut proj = finish_constrained_milestone(2880, constraint, monday, honor, true);
            if successor {
                let mut b = task(3, "B", 480);
                b.predecessors.push(fs(2));
                proj.tasks.push(b);
            }
            let case = format!("{constraint:?} honor={honor} successor={successor}");
            let sched = schedule(&proj);
            let m = sched.get(2).unwrap();
            assert_eq!(m.early_start, early, "{case}");
            assert_eq!(m.early_finish, early, "{case}");
            assert_eq!(m.late_start, monday, "{case}");
            assert_eq!(m.late_finish, monday, "{case}");
            assert_eq!(m.total_slack_min, -480, "{case}");
        }
        // An FNLT on the same index never loosens the FS instant.
        let proj = finish_constrained_milestone(
            2400,
            ConstraintType::FinishNoLaterThan,
            monday,
            true,
            true,
        );
        assert_milestone_at(&proj, friday, 0, "FNLT Mon 08:00");
    }

    #[test]
    fn fnlt_milestone_never_loosens_a_same_index_evening_bound() {
        // M finishes Wed; the project finish (C) is Fri 17:00. A Mon 08:00
        // FNLT shares that index but must not move LF past the Friday evening.
        let monday = DateTime::from_ymd_hm(2026, 3, 9, 8, 0);
        let friday = DateTime::from_ymd_hm(2026, 3, 6, 17, 0);
        for ff_successor in [false, true] {
            let mut proj = finish_constrained_milestone(
                1440,
                ConstraintType::FinishNoLaterThan,
                monday,
                true,
                true,
            );
            proj.tasks.push(task(3, "C", 2400));
            if ff_successor {
                let mut d = task(4, "D", 480);
                d.predecessors
                    .push(Predecessor::working(2, LinkType::FinishFinish, 0));
                proj.tasks.push(d);
            }
            let sched = schedule(&proj);
            let m = sched.get(2).unwrap();
            let case = format!("ff_successor={ff_successor}");
            assert_eq!(
                m.early_start,
                DateTime::from_ymd_hm(2026, 3, 4, 17, 0),
                "{case}"
            );
            assert_eq!(m.late_start, friday, "{case}");
            assert_eq!(m.late_finish, friday, "{case}");
            assert!(m.late_finish <= sched.project_finish, "{case}");
            assert_eq!(m.total_slack_min, 960, "{case}");
        }
    }

    #[test]
    fn fnlt_milestone_morning_deadline_holds_for_every_zero_lag_successor_link() {
        // A zero-lag successor bounds the milestone's one instant by its own
        // late start or finish, so no link type maps LF to the prior evening.
        let monday = DateTime::from_ymd_hm(2026, 3, 9, 8, 0);
        for link in [
            LinkType::FinishStart,
            LinkType::StartStart,
            LinkType::FinishFinish,
            LinkType::StartFinish,
        ] {
            let mut proj = finish_constrained_milestone(
                2880,
                ConstraintType::FinishNoLaterThan,
                monday,
                true,
                true,
            );
            let mut b = task(3, "B", 480);
            b.predecessors.push(Predecessor::working(2, link, 0));
            proj.tasks.push(b);
            let sched = schedule(&proj);
            let m = sched.get(2).unwrap();
            assert_eq!(m.early_start, monday, "{link:?}");
            assert_eq!(m.early_finish, monday, "{link:?}");
            assert_eq!(m.late_start, monday, "{link:?}");
            assert_eq!(m.late_finish, monday, "{link:?}");
            assert_eq!(m.total_slack_min, -480, "{link:?}");
        }
    }

    #[test]
    fn nonbinding_fnlt_milestone_keeps_link_or_anchor_date() {
        let friday = DateTime::from_ymd_hm(2026, 3, 6, 17, 0);
        for honor in [true, false] {
            let proj = finish_constrained_milestone(
                2400,
                ConstraintType::FinishNoLaterThan,
                friday,
                honor,
                true,
            );
            assert_milestone_at(&proj, friday, 0, &format!("linked honor={honor}"));
            // Unlinked, the milestone keeps the project start and its slack.
            let proj = finish_constrained_milestone(
                2400,
                ConstraintType::FinishNoLaterThan,
                friday,
                honor,
                false,
            );
            let sched = schedule(&proj);
            let m = sched.get(2).unwrap();
            let anchor = proj.start_date.unwrap();
            assert_eq!(m.early_start, anchor, "honor={honor}");
            assert_eq!(m.early_finish, anchor, "honor={honor}");
            assert_eq!(m.late_finish, friday, "honor={honor}");
            assert_eq!(m.total_slack_min, 2400, "honor={honor}");
            let leveled = level(&proj);
            assert_eq!(leveled.start(2), Some(anchor), "honor={honor}");
            assert_eq!(leveled.finish(2), Some(anchor), "honor={honor}");
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
        proj.tasks[1]
            .predecessors
            .push(Predecessor::working(3, LinkType::StartStart, 0));
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
        for day in shorter.week.iter_mut().flatten() {
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
            proj.tasks[1].predecessors[0].lag = lag;
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
            proj.tasks[2].predecessors[0].lag = lag;
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
        proj.tasks[2]
            .predecessors
            .push(Predecessor::working(3, LinkType::StartStart, 0));
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
        proj.tasks[2]
            .predecessors
            .push(Predecessor::working(3, LinkType::StartStart, 0));
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
        Calendar::base(uid, "Closed", Default::default())
    }

    #[test]
    fn twenty_four_hour_calendar_matches_project() {
        let mut cal = Calendar::standard(3);
        cal.name = "24 Hours".into();
        for day in cal.week.iter_mut().flatten() {
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
        successor
            .predecessors
            .push(Predecessor::working(2, LinkType::StartFinish, -60));
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
                ..Assignment::default()
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
        b.predecessors
            .push(Predecessor::working(1, LinkType::StartStart, 0));
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
        b.predecessors
            .push(Predecessor::working(1, LinkType::StartFinish, 0));
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
            proj.tasks[1].predecessors[0].lag = lag;
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
        a.predecessors
            .push(Predecessor::working(2, LinkType::StartFinish, 0));
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
        proj.tasks[0].predecessors[0].lag = -960;
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
                    successor
                        .predecessors
                        .push(Predecessor::working(11, LinkType::StartFinish, 0));
                    proj.tasks
                        .extend([task(11, "Anchor milestone", 0), successor]);
                }
                let before = *schedule(&proj).get(10).unwrap();
                // A stale unlinked deadline on the same calendar moves the
                // pre-start timeline's origin, never this task's window.
                let mut stale = task(14, "Stale template deadline", 480);
                stale.constraint = ConstraintType::FinishNoLaterThan;
                stale.constraint_date = Some(DateTime::from_ymd_hm(1900, 1, 1, 17, 0));
                for unrelated in [
                    task(13, "Unrelated", 5 * 480),
                    task(13, "Unrelated", 400 * 480),
                    stale,
                ] {
                    let mut with_unrelated = proj.clone();
                    with_unrelated.tasks.push(unrelated);
                    let after = *schedule(&with_unrelated).get(10).unwrap();
                    assert_eq!(before, after, "{constraint:?}, linked={linked}");
                }
                let anchor = proj.start_date.unwrap();
                assert_eq!(before.early_start, anchor, "{constraint:?}");
                assert_eq!(
                    before.early_finish,
                    DateTime::from_ymd_hm(2026, 3, 2, 17, 0)
                );
                // Either way the constrained task itself stays unlinked; the
                // missed date shows as negative slack.
                if matches!(
                    constraint,
                    ConstraintType::MustFinishOn | ConstraintType::FinishNoLaterThan
                ) {
                    // Monday 08:00 is the end of Friday's work.
                    assert_eq!(before.late_start, DateTime::from_ymd_hm(2026, 2, 13, 8, 0));
                    assert_eq!(
                        before.late_finish,
                        DateTime::from_ymd_hm(2026, 2, 13, 17, 0)
                    );
                    assert_eq!(before.total_slack_min, -11 * 480, "{constraint:?}");
                    assert!(before.critical);
                } else {
                    assert_eq!(before.late_start, DateTime::from_ymd_hm(2026, 2, 16, 8, 0));
                    assert_eq!(
                        before.late_finish,
                        DateTime::from_ymd_hm(2026, 2, 16, 17, 0)
                    );
                    assert_eq!(before.total_slack_min, -10 * 480, "{constraint:?}");
                    assert!(before.critical);
                }
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
                proj.tasks[0].predecessors[0] = Predecessor::working(2, link, lag_min);
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
        for day in sparse.week.iter_mut().flatten() {
            day.times.clear();
        }
        sparse.week[1].as_mut().unwrap().times.push(WorkingTime {
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
                proj.tasks[0].predecessors.push(Predecessor::working(
                    uid,
                    LinkType::StartFinish,
                    0,
                ));
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
            cal.week[dow].as_mut().unwrap().times.clear();
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
            sunday.week[dow].as_mut().unwrap().times.clear();
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
        for uid in [1, 2] {
            assert_eq!(sched.get(uid), baseline.get(uid), "task {uid}");
        }
        // The deadline cannot pull the unlinked task into 1900, but its late
        // window runs back to the absolute cap on a timeline of its own.
        let anchor = proj.start_date.unwrap();
        let cap = (anchor.day_number() - HORIZON_DAYS) * 1440;
        let pre_start = engine.pre_start_timelines(&[2], &Default::default());
        let side = &pre_start[&proj.default_calendar_uid];
        assert!(side.segs[0].start >= cap);
        let unlinked = sched.get(3).unwrap();
        assert_eq!(unlinked.early_start, baseline.get(3).unwrap().early_start);
        assert_eq!(unlinked.early_finish, baseline.get(3).unwrap().early_finish);
        assert_eq!(unlinked.late_start.minutes(), side.segs[0].start);
        assert_eq!(
            working_minutes_between(&proj, unlinked.late_start, unlinked.late_finish),
            480
        );
        assert!(unlinked.late_start.minutes() < cap + 7 * 1440);
        assert_eq!(
            unlinked.total_slack_min,
            -weekday_minutes(unlinked.late_start, anchor)
        );
        assert!(unlinked.critical);
    }

    /// Standard-calendar working minutes from the start of `from`'s day to the
    /// start of `to`'s day, counted independently of `Timeline`.
    fn weekday_minutes(from: DateTime, to: DateTime) -> i64 {
        (from.day_number()..to.day_number())
            .filter(|day| (1..=5).contains(&(day + 4).rem_euclid(7)))
            .count() as i64
            * 480
    }

    fn unlinked_deadline(constraint: ConstraintType, date: DateTime, honor: bool) -> Project {
        let mut phase = task(1, "Phase", 0);
        phase.summary = true;
        let mut t = task(2, "Deadline", 2 * 480);
        t.outline_level = 2;
        t.constraint = constraint;
        t.constraint_date = Some(date);
        Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            honor_constraints: honor,
            tasks: vec![phase, t],
            ..Project::default()
        }
    }

    #[test]
    fn unlinked_pre_start_deadline_reports_negative_slack() {
        for constraint in [
            ConstraintType::FinishNoLaterThan,
            ConstraintType::MustFinishOn,
            ConstraintType::StartNoLaterThan,
            ConstraintType::MustStartOn,
        ] {
            for honor in [true, false] {
                let finish = matches!(
                    constraint,
                    ConstraintType::FinishNoLaterThan | ConstraintType::MustFinishOn
                );
                let date = DateTime::from_ymd_hm(2026, 2, 16, if finish { 17 } else { 8 }, 0);
                let proj = unlinked_deadline(constraint, date, honor);
                let sched = schedule(&proj);
                let r = *sched.get(2).unwrap();
                let case = format!("{constraint:?}, honor={honor}");
                assert_eq!(r.early_start, proj.start_date.unwrap(), "{case}");
                assert_eq!(r.early_finish, DateTime::from_ymd_hm(2026, 3, 3, 17, 0));
                let (late_start, late_finish, slack) = if finish {
                    (DateTime::from_ymd_hm(2026, 2, 13, 8, 0), date, -11 * 480)
                } else {
                    (date, DateTime::from_ymd_hm(2026, 2, 17, 17, 0), -10 * 480)
                };
                assert_eq!(r.late_start, late_start, "{case}");
                assert_eq!(r.late_finish, late_finish, "{case}");
                assert_eq!(r.total_slack_min, slack, "{case}");
                assert_eq!(
                    r.total_slack_min,
                    -working_minutes_between(&proj, r.late_start, r.early_start)
                );
                assert!(r.critical, "{case}");
                assert_eq!(r.free_slack_min, 0, "{case}");
                let phase = sched.get(1).unwrap();
                assert_eq!(phase.total_slack_min, slack, "{case}");
                assert_eq!(phase.late_start, late_start, "{case}");
                assert!(phase.critical, "{case}");
            }
        }
    }

    #[test]
    fn unlinked_pre_start_start_milestone_reports_negative_slack() {
        // An SNLT/MSO milestone takes the evening side of its date's index,
        // as the shared timeline places a post-start one; only finish
        // constraints keep a morning deadline on its morning (#89).
        let friday_evening = DateTime::from_ymd_hm(2026, 2, 13, 17, 0);
        let monday_noon = DateTime::from_ymd_hm(2026, 2, 16, 12, 0);
        for (date, late, slack) in [
            (
                DateTime::from_ymd_hm(2026, 2, 16, 8, 0),
                friday_evening,
                -10 * 480,
            ),
            (monday_noon, monday_noon, -9 * 480 - 240),
        ] {
            for constraint in [
                ConstraintType::StartNoLaterThan,
                ConstraintType::MustStartOn,
            ] {
                for honor in [true, false] {
                    let mut proj = unlinked_deadline(constraint, date, honor);
                    proj.tasks[1].duration_min = 0;
                    let case = format!("{constraint:?} {date:?} honor={honor}");
                    let r = *schedule(&proj).get(2).unwrap();
                    assert_eq!(r.early_start, proj.start_date.unwrap(), "{case}");
                    assert_eq!(r.early_finish, proj.start_date.unwrap(), "{case}");
                    assert_eq!(r.late_start, late, "{case}");
                    assert_eq!(r.late_finish, late, "{case}");
                    assert_eq!(r.total_slack_min, slack, "{case}");
                    assert!(r.critical, "{case}");
                }
            }
        }
    }

    #[test]
    fn unlinked_pre_start_milestone_keeps_morning_deadline() {
        let monday = DateTime::from_ymd_hm(2026, 2, 16, 8, 0);
        let monday_evening = DateTime::from_ymd_hm(2026, 2, 16, 17, 0);
        // (date, slack): a morning deadline stays on its
        // morning, an evening one on its evening.
        for (date, slack) in [(monday, -10 * 480), (monday_evening, -9 * 480)] {
            for constraint in [
                ConstraintType::MustFinishOn,
                ConstraintType::FinishNoLaterThan,
            ] {
                for honor in [true, false] {
                    for successor in [false, true] {
                        let mut proj = unlinked_deadline(constraint, date, honor);
                        proj.tasks[1].duration_min = 0;
                        if successor {
                            let mut b = task(3, "B", 480);
                            b.predecessors.push(fs(2));
                            proj.tasks.push(b);
                        }
                        let case =
                            format!("{constraint:?} {date:?} honor={honor} successor={successor}");
                        let r = *schedule(&proj).get(2).unwrap();
                        assert_eq!(r.early_start, proj.start_date.unwrap(), "{case}");
                        assert_eq!(r.early_finish, proj.start_date.unwrap(), "{case}");
                        assert_eq!(r.late_start, date, "{case}");
                        assert_eq!(r.late_finish, date, "{case}");
                        assert_eq!(r.total_slack_min, slack, "{case}");
                    }
                }
            }
        }
        // An FNLT never moves the milestone past its successor's own instant:
        // a linked milestone due Friday evening holds the Monday morning back.
        for honor in [true, false] {
            let mut proj = unlinked_deadline(ConstraintType::FinishNoLaterThan, monday, honor);
            proj.tasks[1].duration_min = 0;
            let mut c = task(3, "C", 0);
            c.predecessors.push(fs(2));
            c.constraint = ConstraintType::FinishNoLaterThan;
            c.constraint_date = Some(DateTime::from_ymd_hm(2026, 2, 13, 17, 0));
            proj.tasks.push(c);
            let r = *schedule(&proj).get(2).unwrap();
            let friday_evening = DateTime::from_ymd_hm(2026, 2, 13, 17, 0);
            assert_eq!(r.late_start, friday_evening, "honor={honor}");
            assert_eq!(r.late_finish, friday_evening, "honor={honor}");
            assert_eq!(r.total_slack_min, -10 * 480, "honor={honor}");
        }
    }

    #[test]
    fn clamped_deadlines_outside_the_linked_horizon_report_negative_slack() {
        // Its only predecessor has no working time, so the task is unlinked,
        // although the horizon treats it as linked.
        let mut proj = unlinked_deadline(
            ConstraintType::FinishNoLaterThan,
            DateTime::from_ymd_hm(2026, 2, 16, 17, 0),
            true,
        );
        let mut closed = task(3, "Closed", 480);
        closed.calendar_uid = Some(3);
        proj.tasks[1].predecessors.push(fs(3));
        proj.tasks.push(closed);
        proj.calendars = vec![Calendar::standard(1), closed_calendar(3)];
        let r = *schedule(&proj).get(2).unwrap();
        assert_eq!(r.early_start, proj.start_date.unwrap());
        assert_eq!(r.late_start, DateTime::from_ymd_hm(2026, 2, 13, 8, 0));
        assert_eq!(r.late_finish, DateTime::from_ymd_hm(2026, 2, 16, 17, 0));
        assert_eq!(r.total_slack_min, -11 * 480);

        // A Saturday start snaps to Monday; a Sunday deadline lies after the
        // raw start but before the floor, and is clamped all the same.
        let mut proj = unlinked_deadline(
            ConstraintType::FinishNoLaterThan,
            DateTime::from_ymd_hm(2026, 3, 1, 12, 0),
            true,
        );
        proj.start_date = Some(DateTime::from_ymd_hm(2026, 2, 28, 8, 0));
        let r = *schedule(&proj).get(2).unwrap();
        assert_eq!(r.early_start, DateTime::from_ymd_hm(2026, 3, 2, 8, 0));
        assert_eq!(r.late_start, DateTime::from_ymd_hm(2026, 2, 26, 8, 0));
        assert_eq!(r.late_finish, DateTime::from_ymd_hm(2026, 2, 27, 17, 0));
        assert_eq!(r.total_slack_min, -2 * 480);
    }

    #[test]
    fn unlinked_pre_start_deadline_yields_to_an_earlier_successor_bound() {
        let start = DateTime::from_ymd_hm(2024, 1, 8, 8, 0);
        let finish = DateTime::from_ymd_hm(2024, 1, 12, 17, 0);
        for (constraint, date) in [
            (ConstraintType::FinishNoLaterThan, finish),
            (ConstraintType::MustStartOn, start),
        ] {
            for honor in [true, false] {
                let mut proj = constraint_conflict(constraint, date, honor);
                let expected = *schedule(&proj).get(1).unwrap();
                proj.tasks[0].constraint = ConstraintType::FinishNoLaterThan;
                proj.tasks[0].constraint_date = Some(DateTime::from_ymd_hm(2026, 2, 16, 17, 0));
                let a = *schedule(&proj).get(1).unwrap();
                assert_eq!(a, expected, "{constraint:?}, honor={honor}");
                assert_eq!(a.late_start, DateTime::from_ymd_hm(2024, 1, 1, 8, 0));
                assert_eq!(a.late_finish, DateTime::from_ymd_hm(2024, 1, 5, 17, 0));
                assert_eq!(a.total_slack_min, -271200);
            }
        }
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
            ..Assignment::default()
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
        b.predecessors = vec![Predecessor::working(1, LinkType::FinishStart, 480)];
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

    /// A manually scheduled task pinned at `start`.
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
        // The link wants Thursday; the task stays Monday: -3 days of slack.
        assert_eq!(s.get(2).unwrap().total_slack_min, -1440);
        // Its predecessor's late dates fall before the project start and stay
        // finite: P must finish by M's late start.
        let p = s.get(1).unwrap();
        assert_eq!(p.late_start.to_mspdi(), "2026-02-27T08:00:00");
        assert_eq!(p.total_slack_min, -480);
    }

    #[test]
    fn violated_link_off_the_critical_path_still_shows_negative_slack() {
        let mut m = manual(2, "M", 480, at(3, 8));
        m.predecessors.push(fs(1));
        let s = schedule(&march2(vec![task(1, "P", 1440), m, task(3, "Long", 4800)]));
        // The link wants Thursday the 5th; pinned on Tuesday the 3rd.
        assert_eq!(s.get(2).unwrap().total_slack_min, -960);
        assert!(s.get(2).unwrap().critical);
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
    fn manual_dates_beyond_the_timeline_keep_finish_after_start() {
        for start in [
            DateTime::from_ymd_hm(2200, 1, 1, 8, 0),
            DateTime::from_ymd_hm(1026, 3, 2, 8, 0),
        ] {
            let mut succ = task(2, "S", 480);
            succ.predecessors.push(fs(1));
            let s = schedule(&march2(vec![manual(1, "M", 480, start), succ]));
            let m = s.get(1).unwrap();
            assert!(m.early_finish > m.early_start, "{start:?}");
            // Clamped into the timeline, a one-day task stays about a day
            // long rather than spanning the gap back to the real timeline.
            let span = m.early_finish.minutes() - m.early_start.minutes();
            assert!(span <= 4 * 1440, "{start:?}: {span} minutes");
            // The auto successor may run off the timeline end, as any auto
            // task can, but never finishes before it starts.
            let succ = s.get(2).unwrap();
            assert!(succ.early_finish >= succ.early_start, "{start:?}");
        }
    }

    #[test]
    fn long_manual_task_on_a_sparse_calendar_keeps_its_full_duration() {
        // Mondays only: 480 working minutes a week.
        let mut mondays = Calendar::standard(2);
        for day in [2, 3, 4, 5] {
            mondays.week[day] = Some(DayWorking::default());
        }
        // Pinned beyond the working-minute budget after the anchor.
        let start = DateTime::from_ymd_hm(2031, 3, 3, 8, 0);
        assert_eq!(start.weekday(), 1);
        let mut m = manual(1, "M", 40 * 480, start);
        m.calendar_uid = Some(2);
        let mut proj = march2(vec![m]);
        proj.calendars.push(mondays);
        let s = schedule(&proj);
        // Forty Mondays: the last is 39 weeks after the first.
        let finish = start.add_days(39 * 7).with_minute_of_day(17 * 60);
        assert_eq!(s.get(1).unwrap().early_finish, finish);
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

    #[test]
    fn task_duration_derives_summaries_from_rolled_up_dates() {
        // The summary's stored duration (0) is stale; its children span 3d.
        let mut sum = task(1, "Phase", 0);
        sum.summary = true;
        let mut a = task(2, "A", 480);
        a.outline_level = 2;
        let mut b = task(3, "B", 960);
        b.outline_level = 2;
        b.predecessors = vec![fs(2)];
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![sum, a, b],
            ..Project::default()
        };
        let s = schedule(&proj);
        assert_eq!(task_duration_min(&proj, &s, &proj.tasks[0]), Some(1440));
        assert_eq!(task_duration_min(&proj, &s, &proj.tasks[2]), Some(960));
        // No schedule result for a task outside the scheduled project.
        assert_eq!(task_duration_min(&proj, &s, &task(99, "Ghost", 480)), None);
    }

    fn weekly(uid: i32, days: &[(usize, u32, u32)]) -> Calendar {
        let mut cal = closed_calendar(uid);
        for &(dow, from, to) in days {
            cal.week[dow]
                .as_mut()
                .unwrap()
                .times
                .push(WorkingTime { from, to });
        }
        cal
    }

    /// Default calendar 1 has no working time; every leaf sets its own.
    fn closed_default(tasks: Vec<Task>, calendars: Vec<Calendar>) -> Project {
        Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks,
            calendars: [vec![closed_calendar(1)], calendars].concat(),
            ..Project::default()
        }
    }

    fn child(uid: i32, name: &str, duration: i64, calendar: i32) -> Task {
        Task {
            outline_level: 2,
            calendar_uid: Some(calendar),
            ..task(uid, name, duration)
        }
    }

    fn phase(uid: i32) -> Task {
        Task {
            summary: true,
            ..task(uid, "Phase", 0)
        }
    }

    #[test]
    fn summary_duration_uses_leaf_calendars_when_default_has_no_work() {
        let mut b = child(3, "B", 480, 3);
        b.predecessors.push(fs(2));
        let proj = closed_default(
            vec![phase(1), child(2, "A", 480, 3), b],
            vec![Calendar::standard(3)],
        );
        let s = schedule(&proj);
        assert_eq!(dates(&s, 1).1, "2026-03-03T17:00:00");
        assert_eq!(task_duration_min(&proj, &s, &proj.tasks[0]), Some(960));
        let lv = level(&proj);
        assert_eq!(
            summary_or_leaf_min(
                &proj,
                &proj.tasks[0],
                lv.start(1).unwrap(),
                lv.finish(1).unwrap()
            ),
            960
        );
    }

    #[test]
    fn summary_duration_unions_every_leaf_calendar() {
        // Standard Mon-Fri, a Saturday morning, and a long Monday overlapping
        // both Standard shifts: Monday counts 08:00-18:00 once, not twice.
        let saturday = weekly(4, &[(6, 480, 720)]);
        let long_monday = weekly(5, &[(1, 600, 1080)]);
        let mut sat = child(3, "Sat", 240, 4);
        sat.predecessors.push(fs(2));
        let proj = closed_default(
            vec![
                phase(1),
                child(2, "A", 480, 3),
                sat,
                child(4, "Mon", 480, 5),
            ],
            vec![Calendar::standard(3), saturday, long_monday],
        );
        let s = schedule(&proj);
        assert_eq!(
            dates(&s, 1),
            ("2026-03-02T08:00:00".into(), "2026-03-07T12:00:00".into())
        );
        let monday = 600;
        let tue_to_fri = 4 * 480;
        let saturday = 240;
        assert_eq!(
            task_duration_min(&proj, &s, &proj.tasks[0]),
            Some(monday + tue_to_fri + saturday)
        );
    }

    #[test]
    fn summary_duration_stays_on_a_working_default_calendar() {
        // Leaves on a 24-hour calendar: the summary is still measured on the
        // project's Standard calendar, as MS Project measures it.
        let mut always = closed_calendar(3);
        for day in always.week.iter_mut().flatten() {
            day.times = vec![WorkingTime { from: 0, to: 1440 }];
        }
        let mut b = child(3, "B", 480, 3);
        b.predecessors.push(fs(2));
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![phase(1), child(2, "A", 480, 3), b],
            calendars: vec![Calendar::standard(1), always],
            ..Project::default()
        };
        let s = schedule(&proj);
        assert_eq!(
            dates(&s, 1),
            ("2026-03-02T08:00:00".into(), "2026-03-03T00:00:00".into())
        );
        assert_eq!(task_duration_min(&proj, &s, &proj.tasks[0]), Some(480));
    }

    /// A calendar derived from `base` that states only `own` days.
    fn derived(uid: i32, base: i32, own: &[(usize, DayWorking)]) -> Calendar {
        let mut cal = Calendar {
            base_calendar_uid: Some(base),
            week: Default::default(),
            ..Calendar::standard(uid)
        };
        cal.name = format!("Derived {uid}");
        for (day, working) in own {
            cal.week[*day] = Some(working.clone());
        }
        cal
    }

    #[test]
    fn derived_calendar_resolves_through_its_base_at_schedule_time() {
        // Friday off, everything else inherited from Standard.
        let mut a = task(1, "A", 5 * 480);
        a.calendar_uid = Some(2);
        let mut proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![a],
            calendars: vec![
                Calendar::standard(1),
                derived(2, 1, &[(5, DayWorking::default())]),
            ],
            ..Project::default()
        };
        assert_eq!(calendar_error(&proj), None);
        // Mon-Thu, then Friday is skipped.
        assert_eq!(
            dates(&schedule(&proj), 1),
            ("2026-03-02T08:00:00".into(), "2026-03-09T17:00:00".into())
        );
        // Shorten the base calendar's Monday only: the derived calendar follows.
        proj.calendars[0].week[1] = Some(DayWorking {
            times: vec![WorkingTime {
                from: 8 * 60,
                to: 12 * 60,
            }],
        });
        assert_eq!(
            dates(&schedule(&proj), 1),
            ("2026-03-02T08:00:00".into(), "2026-03-10T17:00:00".into())
        );
    }

    #[test]
    fn derived_calendar_without_own_days_is_not_rejected() {
        let mut a = task(1, "A", 480);
        a.calendar_uid = Some(2);
        let mut proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![a],
            calendars: vec![Calendar::standard(1), derived(2, 1, &[])],
            ..Project::default()
        };
        assert_eq!(calendar_error(&proj), None);
        assert_eq!(
            dates(&schedule(&proj), 1),
            ("2026-03-02T08:00:00".into(), "2026-03-02T17:00:00".into())
        );
        // Without a base to inherit from, it has no working time at all.
        proj.calendars[1].base_calendar_uid = Some(99);
        assert!(
            calendar_error(&proj)
                .unwrap()
                .contains("calendar \"Derived 2\" (UID 2) has no working time")
        );
    }

    #[test]
    fn summary_is_measured_on_a_derived_project_calendar() {
        // As `summary_duration_stays_on_a_working_default_calendar`, but
        // the project calendar derives from Standard and states no days. Read
        // as its own days only, it would have no working time and the summary
        // would be measured on the leaves' 24-hour calendar instead.
        let mut always = closed_calendar(3);
        for day in always.week.iter_mut().flatten() {
            day.times = vec![WorkingTime { from: 0, to: 1440 }];
        }
        let mut b = child(3, "B", 480, 3);
        b.predecessors.push(fs(2));
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![phase(1), child(2, "A", 480, 3), b],
            calendars: vec![Calendar::standard(5), derived(1, 5, &[]), always],
            ..Project::default()
        };
        let s = schedule(&proj);
        assert_eq!(
            dates(&s, 1),
            ("2026-03-02T08:00:00".into(), "2026-03-03T00:00:00".into())
        );
        assert_eq!(task_duration_min(&proj, &s, &proj.tasks[0]), Some(480));
    }

    #[test]
    fn base_chain_lookup_takes_the_last_calendar_with_a_uid() {
        // Two calendars share UID 1; the second works 07:00-19:00 on Mondays.
        let mut long = Calendar::standard(1);
        long.week[1] = Some(DayWorking {
            times: vec![WorkingTime {
                from: 7 * 60,
                to: 19 * 60,
            }],
        });
        let mut a = task(1, "A", 12 * 60);
        a.calendar_uid = Some(2);
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 7, 0)),
            tasks: vec![a],
            calendars: vec![Calendar::standard(1), long, derived(2, 1, &[])],
            ..Project::default()
        };
        assert_eq!(
            dates(&schedule(&proj), 1),
            ("2026-03-02T07:00:00".into(), "2026-03-02T19:00:00".into())
        );
    }

    #[test]
    fn anchor_ignores_starts_of_tasks_that_are_not_scheduled() {
        let early = DateTime::from_ymd_hm(2023, 6, 5, 8, 0);
        let mut valid = task(1, "Valid", 480);
        valid.stored_start = Some(DateTime::from_ymd_hm(2024, 1, 8, 8, 0));
        let mut dropped = task(2, "Closed", 480);
        dropped.calendar_uid = Some(3);
        dropped.stored_start = Some(early);
        let pinned = Task {
            manual: true,
            manual_start: Some(early),
            manual_finish: Some(DateTime::from_ymd_hm(2023, 6, 6, 17, 0)),
            ..dropped.clone()
        };
        let mut summary = phase(10);
        summary.stored_start = Some(early);
        let nested = Task {
            outline_level: 2,
            ..valid.clone()
        };
        let project = |tasks| Project {
            tasks,
            calendars: vec![Calendar::standard(1), closed_calendar(3)],
            ..Project::default()
        };
        let expected = schedule(&project(vec![valid.clone()]));
        assert_eq!(expected.project_start, valid.stored_start.unwrap());
        for tasks in [
            vec![dropped, valid.clone()],
            vec![pinned, valid],
            vec![summary, nested],
        ] {
            let actual = schedule(&project(tasks));
            assert_eq!(actual.project_start, expected.project_start);
            assert_eq!(actual.get(1), expected.get(1));
            assert_eq!(actual.project_finish, expected.project_finish);
        }
    }

    #[test]
    fn rollup_ignores_an_unscheduled_leaf_sharing_a_uid() {
        // Summary 10 holds valid leaf 2 and dropped leaf 5; a valid task 5
        // outside it runs a week later and must not widen the summary.
        let mut outside = task(5, "Outside", 2400);
        outside.predecessors.push(fs(2));
        let proj = Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
            tasks: vec![
                phase(10),
                child(2, "Valid", 480, 1),
                child(5, "Closed", 480, 3),
                outside,
            ],
            calendars: vec![Calendar::standard(1), closed_calendar(3)],
            ..Project::default()
        };
        let s = schedule(&proj);
        let (sum, kid) = (s.get(10).unwrap(), s.get(2).unwrap());
        assert!(s.get(5).unwrap().early_finish > kid.early_finish);
        assert_eq!(
            (
                sum.early_start,
                sum.early_finish,
                sum.late_start,
                sum.late_finish
            ),
            (
                kid.early_start,
                kid.early_finish,
                kid.late_start,
                kid.late_finish
            )
        );
        let lv = level(&proj);
        assert_eq!(lv.start(10), lv.start(2));
        assert_eq!(lv.finish(10), lv.finish(2));
    }

    fn dt(day: u32, hour: u32) -> DateTime {
        DateTime::from_ymd_hm(2026, 3, day, hour, 0)
    }

    /// A (5d) -> B (5d) FS from Mon 2026-03-02; B finishes Fri 03-13 17:00.
    fn deadline_chain(deadline: Option<DateTime>, honor: bool) -> Project {
        let mut b = task(2, "B", 2400);
        b.predecessors.push(fs(1));
        b.deadline = deadline;
        Project {
            start_date: Some(dt(2, 8)),
            honor_constraints: honor,
            tasks: vec![task(1, "A", 2400), b],
            ..Project::default()
        }
    }

    fn early_dates(sched: &Schedule, uid: i32) -> (DateTime, DateTime) {
        let r = sched.get(uid).unwrap();
        (r.early_start, r.early_finish)
    }

    #[test]
    fn missed_deadline_gives_task_and_its_driver_negative_slack() {
        // #100: Project 2024 reports -5d total slack and 0 free slack on both.
        for honor in [true, false] {
            let baseline = schedule(&deadline_chain(None, honor));
            let sched = schedule(&deadline_chain(Some(dt(6, 17)), honor));
            for uid in [1, 2] {
                let r = sched.get(uid).unwrap();
                assert_eq!(r.total_slack_min, -2400, "task {uid} honor={honor}");
                assert_eq!(r.free_slack_min, 0, "task {uid} honor={honor}");
                assert!(r.critical, "task {uid} honor={honor}");
                assert_eq!(early_dates(&sched, uid), early_dates(&baseline, uid));
            }
            assert_eq!(sched.get(2).unwrap().late_finish, dt(6, 17));
            assert_eq!(sched.project_finish, dt(13, 17));
            let leveled = level(&deadline_chain(Some(dt(6, 17)), honor));
            assert_eq!(leveled.finish(2), Some(dt(13, 17)));
        }
    }

    #[test]
    fn deadline_after_the_project_finish_changes_nothing() {
        for honor in [true, false] {
            let baseline = schedule(&deadline_chain(None, honor));
            let sched = schedule(&deadline_chain(Some(dt(18, 17)), honor));
            for uid in [1, 2] {
                assert_eq!(
                    sched.get(uid),
                    baseline.get(uid),
                    "task {uid} honor={honor}"
                );
            }
        }
    }

    #[test]
    fn met_deadline_tightens_a_non_critical_branch_to_the_deadline_gap() {
        // A (5d) is critical; X (1d) -> C (1d) has 3d of total slack until C's
        // deadline on Wed 03-04 17:00 leaves 1d, for C and for X that drives it.
        let mut c = task(3, "C", 480);
        c.predecessors.push(fs(2));
        let mut proj = Project {
            start_date: Some(dt(2, 8)),
            tasks: vec![task(1, "A", 2400), task(2, "X", 480), c],
            ..Project::default()
        };
        let baseline = schedule(&proj);
        assert_eq!(baseline.get(3).unwrap().total_slack_min, 3 * 480);
        proj.tasks[2].deadline = Some(dt(4, 17));
        let sched = schedule(&proj);
        for uid in [2, 3] {
            let r = sched.get(uid).unwrap();
            assert_eq!(r.total_slack_min, 480, "task {uid}");
            assert!(!r.critical, "task {uid}");
            assert_eq!(early_dates(&sched, uid), early_dates(&baseline, uid));
        }
        assert_eq!(sched.get(3).unwrap().late_finish, dt(4, 17));
        assert_eq!(sched.get(1), baseline.get(1));
    }

    #[test]
    fn deadline_and_finish_constraint_take_the_earlier_bound() {
        use ConstraintType::{FinishNoLaterThan as Fnlt, MustFinishOn as Mfo};
        // A (5d) -> B (5d) beside an unlinked Z (15d), so B finishing Fri
        // 03-13 has 5d of slack to the project finish on Fri 03-20. Neither
        // constraint date moves B's early dates.
        // (constraint, constraint date, deadline, B's total slack)
        for (constraint, date, deadline, slack) in [
            (Fnlt, dt(18, 17), dt(17, 17), 2 * 480),
            (Fnlt, dt(18, 17), dt(19, 17), 3 * 480),
            (Mfo, dt(13, 17), dt(12, 17), -480),
            (Mfo, dt(13, 17), dt(17, 17), 0),
        ] {
            for honor in [true, false] {
                let mut proj = deadline_chain(Some(deadline), honor);
                proj.tasks.push(task(3, "Z", 15 * 480));
                let baseline = schedule(&proj);
                proj.tasks[1].constraint = constraint;
                proj.tasks[1].constraint_date = Some(date);
                let sched = schedule(&proj);
                let case = format!("{constraint:?} {deadline:?} honor={honor}");
                let b = sched.get(2).unwrap();
                assert_eq!(b.total_slack_min, slack, "{case}");
                assert_eq!(sched.get(1).unwrap().total_slack_min, slack, "{case}");
                assert_eq!(b.late_finish, date.min(deadline), "{case}");
                assert_eq!(early_dates(&sched, 2), early_dates(&baseline, 2), "{case}");
            }
        }
    }

    #[test]
    fn milestone_deadline_keeps_fnlt_morning_semantics() {
        // Milestone M lands at Fri 03-06 17:00 after A (5d); an unlinked Z
        // (10d) keeps the project finish at Fri 03-13 so M has slack to lose.
        // A morning deadline stays on its morning, exactly as an FNLT does.
        // (deadline, M's total slack)
        for (deadline, slack) in [(dt(10, 8), 480), (dt(5, 8), -960), (dt(9, 17), 480)] {
            let mut proj = fs_milestone_project(2400);
            proj.honor_constraints = false;
            proj.tasks.push(task(3, "Z", 10 * 480));
            let baseline = schedule(&proj);
            let mut fnlt = proj.clone();
            fnlt.tasks[1].constraint = ConstraintType::FinishNoLaterThan;
            fnlt.tasks[1].constraint_date = Some(deadline);
            proj.tasks[1].deadline = Some(deadline);
            let sched = schedule(&proj);
            let m = sched.get(2).unwrap();
            let case = format!("{deadline:?}");
            assert_eq!(m.late_start, deadline, "{case}");
            assert_eq!(m.late_finish, deadline, "{case}");
            assert_eq!(m.total_slack_min, slack, "{case}");
            assert_eq!(early_dates(&sched, 2), early_dates(&baseline, 2), "{case}");
            assert_eq!(sched.get(2), schedule(&fnlt).get(2), "{case}");
        }
    }

    #[test]
    fn linked_deadline_far_before_the_early_dates_stays_on_the_timeline() {
        // A deadline a year early still measures its full miss rather than
        // clamping to a timeline origin that the early dates alone would set.
        let deadline = DateTime::from_ymd_hm(2025, 6, 2, 17, 0);
        let sched = schedule(&deadline_chain(Some(deadline), true));
        let b = sched.get(2).unwrap();
        assert_eq!(b.late_finish, deadline);
        // Working time from Mon 2025-06-02 17:00 to Fri 2026-03-13 17:00.
        let miss = weekday_minutes(
            DateTime::from_ymd_hm(2025, 6, 3, 8, 0),
            DateTime::from_ymd_hm(2026, 3, 14, 8, 0),
        );
        for uid in [1, 2] {
            assert_eq!(sched.get(uid).unwrap().total_slack_min, -miss, "task {uid}");
        }
        assert_eq!(early_dates(&sched, 2), (dt(9, 8), dt(13, 17)));
    }

    #[test]
    fn unlinked_pre_start_deadline_reports_negative_slack_without_moving_dates() {
        // The exact pre-start value is a follow-up; the miss must still show.
        for honor in [true, false] {
            let mut proj = deadline_chain(None, honor);
            proj.tasks[1].predecessors.clear();
            let baseline = schedule(&proj);
            proj.tasks[1].deadline = Some(DateTime::from_ymd_hm(2026, 2, 16, 17, 0));
            let sched = schedule(&proj);
            let b = sched.get(2).unwrap();
            assert!(b.total_slack_min < 0, "honor={honor}: {b:?}");
            assert!(b.critical);
            assert_eq!(early_dates(&sched, 2), early_dates(&baseline, 2));
            assert_eq!(sched.get(1), baseline.get(1));
        }
    }

    #[test]
    fn deadline_defers_to_a_pre_start_constraint_window() {
        let date = DateTime::from_ymd_hm(2026, 2, 16, 17, 0);
        for honor in [true, false] {
            let constraint_only = schedule(&unlinked_deadline(
                ConstraintType::FinishNoLaterThan,
                date,
                honor,
            ));
            for deadline in [DateTime::from_ymd_hm(2026, 2, 27, 17, 0), dt(2, 17)] {
                let mut proj = unlinked_deadline(ConstraintType::FinishNoLaterThan, date, honor);
                proj.tasks[1].deadline = Some(deadline);
                let sched = schedule(&proj);
                assert_eq!(
                    sched.get(2),
                    constraint_only.get(2),
                    "{deadline:?} honor={honor}"
                );
            }
        }
    }

    #[test]
    fn milestone_deadline_under_a_hard_constraint_ignores_an_earlier_successor_bound() {
        use ConstraintType::{MustFinishOn as Mfo, MustStartOn as Mso};
        // M is pinned late by an MFO/MSO, which overrides its successor S.
        // S's own deadline puts S's late start on Thu 03-05, days before M's
        // deadline (Tue 03-10 17:00): that earlier instant must not become
        // M's late finish, which is the deadline's.
        // (constraint, constraint date)
        for (constraint, date) in [
            (Mfo, dt(20, 17)),
            (Mso, dt(23, 8)),
            (Mfo, dt(10, 17)),
            (Mso, dt(11, 8)),
        ] {
            for honor in [true, false] {
                let mut m = task(1, "M", 0);
                m.constraint = constraint;
                m.constraint_date = Some(date);
                m.deadline = Some(dt(10, 17));
                let mut s = task(2, "S", 480);
                s.predecessors.push(fs(1));
                s.deadline = Some(dt(5, 17));
                let proj = Project {
                    start_date: Some(dt(2, 8)),
                    honor_constraints: honor,
                    tasks: vec![m, s],
                    ..Project::default()
                };
                let sched = schedule(&proj);
                let case = format!("{constraint:?} {date:?} honor={honor}");
                assert_eq!(sched.get(2).unwrap().late_start, dt(5, 8), "{case}");
                let m = sched.get(1).unwrap();
                assert_eq!(m.late_start, dt(10, 17), "{case}");
                assert_eq!(m.late_finish, dt(10, 17), "{case}");
            }
        }
    }

    #[test]
    fn deadline_tightens_a_pre_start_start_constraint_window() {
        // An unlinked 10d task with an SNLT/MSO on Fri 02-27, before the
        // project start: the window alone finishes Thu 03-12 (-1d of slack).
        // A deadline on Tue 03-03 17:00 moves the late start to Wed 02-18,
        // -8d from the Mon 03-02 start; a deadline after 03-12 changes nothing.
        let date = DateTime::from_ymd_hm(2026, 2, 27, 8, 0);
        for constraint in [
            ConstraintType::StartNoLaterThan,
            ConstraintType::MustStartOn,
        ] {
            for honor in [true, false] {
                let window = |deadline: Option<DateTime>| {
                    let mut proj = unlinked_deadline(constraint, date, honor);
                    proj.tasks[1].duration_min = 10 * 480;
                    proj.tasks[1].deadline = deadline;
                    *schedule(&proj).get(2).unwrap()
                };
                let case = format!("{constraint:?} honor={honor}");
                let alone = window(None);
                assert_eq!(alone.late_finish, dt(12, 17), "{case}");
                assert_eq!(alone.total_slack_min, -480, "{case}");
                let tight = window(Some(dt(3, 17)));
                assert_eq!(tight.late_finish, dt(3, 17), "{case}");
                assert_eq!(
                    tight.late_start,
                    DateTime::from_ymd_hm(2026, 2, 18, 8, 0),
                    "{case}"
                );
                assert_eq!(tight.total_slack_min, -8 * 480, "{case}");
                assert_eq!(
                    (tight.early_start, tight.early_finish),
                    (alone.early_start, alone.early_finish),
                    "{case}"
                );
                assert_eq!(window(Some(dt(13, 17))), alone, "{case}");
            }
        }
    }

    #[test]
    fn pre_start_window_floors_its_deadline_at_the_project_start() {
        // X (1d) is unlinked with an SNLT on Fri 02-27, before the Mon 03-02
        // start, and a deadline four weeks earlier still. Like every unlinked
        // deadline, it is floored at the project start, so it cannot tighten
        // the window (-1d). An unrelated pre-start task on the same calendar,
        // which moves the pre-start timeline's origin, must not change X.
        let snlt = |uid: i32, day: u32| {
            let mut t = task(uid, "X", 480);
            t.constraint = ConstraintType::StartNoLaterThan;
            t.constraint_date = Some(DateTime::from_ymd_hm(2026, 2, day, 8, 0));
            t
        };
        for honor in [true, false] {
            for deadline in [None, Some(DateTime::from_ymd_hm(2026, 2, 2, 17, 0))] {
                let mut x = snlt(1, 27);
                x.deadline = deadline;
                let mut proj = Project {
                    start_date: Some(dt(2, 8)),
                    honor_constraints: honor,
                    tasks: vec![x],
                    ..Project::default()
                };
                let alone = *schedule(&proj).get(1).unwrap();
                let case = format!("{deadline:?} honor={honor}");
                assert_eq!(alone.total_slack_min, -480, "{case}");
                proj.tasks.push(snlt(2, 2));
                assert_eq!(schedule(&proj).get(1), Some(&alone), "{case}");
            }
        }
    }

    #[test]
    fn pinned_task_ignores_its_deadline() {
        let mut proj = deadline_chain(None, true);
        proj.tasks[1].manual = true;
        proj.tasks[1].manual_start = Some(dt(9, 8));
        let baseline = schedule(&proj);
        proj.tasks[1].deadline = Some(dt(6, 17));
        assert_eq!(schedule(&proj).get(2), baseline.get(2));
    }

    /// A non-working date-range exception over 2026-03-`first`..=`last`.
    fn holiday(first: u32, last: u32) -> CalendarException {
        CalendarException::date_range(
            at(first, 0),
            DateTime::from_ymd_hm(2026, 3, last, 23, 59),
            DayWorking::default(),
        )
    }

    /// A working exception on 2026-03-`day` with these `(from, to)` hours.
    fn worked(day: u32, hours: &[(u32, u32)]) -> CalendarException {
        CalendarException::date_range(
            at(day, 0),
            DateTime::from_ymd_hm(2026, 3, day, 23, 59),
            DayWorking {
                times: hours
                    .iter()
                    .map(|&(from, to)| WorkingTime {
                        from: from * 60,
                        to: to * 60,
                    })
                    .collect(),
            },
        )
    }

    #[test]
    fn a_holiday_inside_a_task_pushes_its_finish_out_by_that_day() {
        let mut proj = march2(vec![task(1, "A", 3 * 480)]);
        assert_eq!(dates(&schedule(&proj), 1).1, "2026-03-04T17:00:00");
        proj.calendars[0].exceptions.push(holiday(4, 4));
        assert_eq!(
            dates(&schedule(&proj), 1),
            ("2026-03-02T08:00:00".into(), "2026-03-05T17:00:00".into())
        );
        assert_eq!(level(&proj).finish(1), Some(at(5, 17)));
        // A holiday on the project start moves the start to the next day.
        proj.calendars[0].exceptions = vec![holiday(2, 2)];
        assert_eq!(
            dates(&schedule(&proj), 1),
            ("2026-03-03T08:00:00".into(), "2026-03-05T17:00:00".into())
        );
    }

    #[test]
    fn a_multi_day_holiday_covers_each_date_from_first_to_last() {
        let mut proj = march2(vec![task(1, "A", 3 * 480)]);
        proj.calendars[0].exceptions.push(holiday(3, 5));
        // Monday, Friday, then the next Monday.
        assert_eq!(dates(&schedule(&proj), 1).1, "2026-03-09T17:00:00");
    }

    #[test]
    fn a_working_exception_replaces_that_days_hours() {
        // Tuesday 08:00-12:00 only: two days end Wednesday noon.
        let mut proj = march2(vec![task(1, "A", 2 * 480)]);
        proj.calendars[0].exceptions.push(worked(3, &[(8, 12)]));
        assert_eq!(dates(&schedule(&proj), 1).1, "2026-03-04T12:00:00");
        // A working Saturday: six days end that Saturday.
        let mut proj = march2(vec![task(1, "A", 6 * 480)]);
        assert_eq!(dates(&schedule(&proj), 1).1, "2026-03-09T17:00:00");
        proj.calendars[0]
            .exceptions
            .push(worked(7, &[(8, 12), (13, 17)]));
        assert_eq!(dates(&schedule(&proj), 1).1, "2026-03-07T17:00:00");
    }

    #[test]
    fn a_task_on_a_derived_calendar_skips_its_base_calendars_holiday() {
        let mut a = task(1, "A", 4 * 480);
        a.calendar_uid = Some(2);
        let mut standard = Calendar::standard(1);
        standard.exceptions.push(holiday(4, 4));
        let mut proj = march2(vec![a]);
        proj.calendars = vec![standard, derived(2, 1, &[(5, DayWorking::default())])];
        // Mon, Tue, Thu, then (Friday off) Monday.
        assert_eq!(dates(&schedule(&proj), 1).1, "2026-03-09T17:00:00");
        // Stating Wednesday itself does not bring the base's holiday back.
        proj.calendars[1].week[3] = Some(Calendar::standard_week()[3].clone());
        assert_eq!(dates(&schedule(&proj), 1).1, "2026-03-09T17:00:00");
        // Only its own working exception does.
        proj.calendars[1]
            .exceptions
            .push(worked(4, &[(8, 12), (13, 17)]));
        assert_eq!(dates(&schedule(&proj), 1).1, "2026-03-05T17:00:00");
    }

    #[test]
    fn exceptions_never_make_an_empty_calendar_schedulable() {
        let mut closed = closed_calendar(2);
        closed.exceptions.push(worked(7, &[(8, 17)]));
        let mut a = task(1, "A", 480);
        a.calendar_uid = Some(2);
        let mut proj = march2(vec![a]);
        proj.calendars.push(closed);
        assert!(
            calendar_error(&proj)
                .unwrap()
                .contains("has no working time")
        );
    }

    #[test]
    fn summary_duration_and_working_minutes_skip_a_holiday() {
        let mut b = child(3, "B", 480, 1);
        b.predecessors.push(fs(2));
        let mut proj = march2(vec![phase(1), child(2, "A", 480, 1), b]);
        proj.calendars[0].exceptions.push(holiday(3, 3));
        let s = schedule(&proj);
        assert_eq!(dates(&s, 3).1, "2026-03-04T17:00:00");
        assert_eq!(task_duration_min(&proj, &s, &proj.tasks[0]), Some(960));
        assert_eq!(working_minutes_between(&proj, at(2, 8), at(4, 17)), 960);
        // On a closed project calendar the leaves' calendars, holidays
        // included, measure the summary.
        let mut b = child(3, "B", 480, 3);
        b.predecessors.push(fs(2));
        let mut standard = Calendar::standard(3);
        standard.exceptions.push(holiday(3, 3));
        let proj = closed_default(vec![phase(1), child(2, "A", 480, 3), b], vec![standard]);
        let s = schedule(&proj);
        assert_eq!(dates(&s, 1).1, "2026-03-04T17:00:00");
        assert_eq!(task_duration_min(&proj, &s, &proj.tasks[0]), Some(960));
    }

    #[test]
    fn a_far_pinned_task_across_a_long_holiday_keeps_its_start_and_finish() {
        // Pinned well past the working-minute padding, with a holiday of almost
        // four months inside its ten days: its reach must count the holiday.
        let start = DateTime::from_ymd_hm(2027, 6, 7, 8, 0);
        let mut proj = march2(vec![manual(1, "Far", 10 * 480, start)]);
        proj.calendars[0]
            .exceptions
            .push(CalendarException::date_range(
                DateTime::from_ymd_hm(2027, 6, 8, 0, 0),
                DateTime::from_ymd_hm(2027, 9, 30, 23, 59),
                DayWorking::default(),
            ));
        assert_eq!(
            dates(&schedule(&proj), 1),
            ("2027-06-07T08:00:00".into(), "2027-10-13T17:00:00".into())
        );
    }

    #[test]
    fn reach_after_counts_working_time_from_the_start_instant() {
        let cal = WorkCalendar::weekly(Calendar::standard_week());
        // Friday 10:00 plus eight hours: Friday's remaining 6h, then Monday's
        // first 2h.
        assert_eq!(
            reach_after(&cal, at(6, 10).minutes(), 480),
            at(9, 10).minutes()
        );
        assert_eq!(
            reach_after(&cal, at(6, 10).minutes(), 0),
            at(6, 10).minutes()
        );
        let closed = WorkCalendar::weekly(Default::default());
        assert_eq!(
            reach_after(&closed, at(6, 10).minutes(), 480),
            at(6, 10).minutes() + HORIZON_DAYS * 1440
        );
    }

    /// A link from `uid` with this MSPDI lag format code and lag.
    fn lag_link(uid: i32, link: LinkType, lag: i64, code: i64) -> Predecessor {
        Predecessor {
            uid,
            link,
            lag,
            lag_format: LagFormat::from_code(code).unwrap(),
        }
    }

    /// A (4d) from Mon 2 and one successor per link; every expected value is
    /// Project 2024's (corpus/tools/gen_mpp_lag_cases.py, #104).
    #[test]
    fn percent_and_elapsed_lags_schedule_as_project_does() {
        use LinkType::*;
        let ed = 1440;
        let cases = [
            // (link, lag, format, duration, start, finish)
            (FinishStart, 50, 19, 480, at(10, 8), at(10, 17)),
            (FinishStart, -25, 19, 480, at(5, 8), at(5, 17)),
            (FinishStart, 2 * ed, 8, 480, at(9, 8), at(9, 17)),
            (FinishStart, -ed, 8, 480, at(5, 8), at(5, 17)),
            (StartStart, 5 * ed, 8, 480, at(9, 8), at(9, 17)),
            (FinishFinish, 2 * ed, 8, 480, at(6, 8), at(7, 17)),
            (FinishStart, 7 * ed, 42, 480, at(13, 8), at(13, 17)),
            (FinishStart, 2 * ed, 8, 0, at(7, 17), at(7, 17)),
            (StartFinish, 2 * ed, 8, 480, at(3, 8), at(4, 8)),
            (FinishStart, 180, 5, 480, at(6, 11), at(9, 11)),
        ];
        for (link, lag, code, duration, start, finish) in cases {
            let mut b = task(2, "B", duration);
            b.predecessors = vec![lag_link(1, link, lag, code)];
            let proj = Project {
                start_date: Some(at(2, 8)),
                tasks: vec![task(1, "A", 4 * 480), b],
                ..Project::default()
            };
            let r = schedule(&proj);
            let r = r.get(2).unwrap();
            let label = format!("{link:?} {lag} format {code}");
            assert_eq!((r.early_start, r.early_finish), (start, finish), "{label}");
        }
    }

    #[test]
    fn elapsed_lead_crosses_a_weekend_backwards_and_percent_uses_its_own_predecessor() {
        // P (6d) finishes Mon 9 17:00; -2ed is Sat 7 17:00, so Q starts Mon 9.
        // X is 150% of P's 6 days after P's start: Fri 13 (Project 2024).
        let mut q = task(2, "Q", 480);
        q.predecessors = vec![lag_link(1, LinkType::FinishStart, -2880, 8)];
        let mut x = task(3, "X", 480);
        x.predecessors = vec![lag_link(1, LinkType::StartStart, 150, 19)];
        let proj = Project {
            start_date: Some(at(2, 8)),
            tasks: vec![task(1, "P", 6 * 480), q, x],
            ..Project::default()
        };
        let s = schedule(&proj);
        assert_eq!(s.get(2).unwrap().early_start, at(9, 8));
        assert_eq!(s.get(3).unwrap().early_start, at(13, 8));
        // A percent lag follows the predecessor's duration.
        let mut longer = proj.clone();
        longer.tasks[0].duration_min = 2 * 480;
        assert_eq!(schedule(&longer).get(3).unwrap().early_start, at(5, 8));
    }

    #[test]
    fn slack_across_an_elapsed_lag_matches_project() {
        // Project 2024 (l2-elapsed-free-slack): A (4d), D 1FS+2ed, and an
        // unrelated Z (10d) that sets the finish. A can slip one day (to Fri
        // 17:00, still before Sat 08:00 = Mon 08:00 - 2ed) before D moves,
        // and three in all.
        let mut d = task(2, "D", 480);
        d.predecessors = vec![lag_link(1, LinkType::FinishStart, 2880, 8)];
        let proj = Project {
            start_date: Some(at(2, 8)),
            tasks: vec![task(1, "A", 4 * 480), d, task(3, "Z", 10 * 480)],
            ..Project::default()
        };
        let s = schedule(&proj);
        let a = s.get(1).unwrap();
        assert_eq!((a.free_slack_min, a.total_slack_min), (480, 1440));
        let d = s.get(2).unwrap();
        assert_eq!((d.free_slack_min, d.total_slack_min), (1920, 1920));
    }

    #[test]
    fn huge_percent_lags_saturate_instead_of_overflowing() {
        let mut b = task(2, "B", 480);
        b.predecessors = vec![lag_link(1, LinkType::FinishStart, i64::MAX, 19)];
        let proj = Project {
            start_date: Some(at(2, 8)),
            tasks: vec![task(1, "A", 480), b],
            ..Project::default()
        };
        let _ = schedule(&proj); // must not panic
        let _ = level(&proj);
    }

    #[test]
    fn leveling_measures_an_elapsed_gap_from_the_leveled_instant() {
        // Busy and A share a resource, so leveling moves A (3d) to Tue 3 - Thu
        // 5 17:00. Two elapsed days later is Sat 7 17:00: D still starts Mon
        // 9, while the milestone M and G's FF finish take that Saturday
        // instant. Project 2024's LevelNow, with Busy at priority 1000, gives
        // the same dates; inheriting A's one-day working delay would not.
        let mut d = task(2, "D", 480);
        d.predecessors = vec![lag_link(1, LinkType::FinishStart, 2880, 8)];
        let mut m = task(3, "M", 0);
        m.predecessors = vec![lag_link(1, LinkType::FinishStart, 2880, 8)];
        let mut g = task(5, "G", 480);
        g.predecessors = vec![lag_link(1, LinkType::FinishFinish, 2880, 8)];
        // Zero-lag milestones after G and after M: only G's and M's instants
        // move, not their working indices, and they must follow: Project's
        // LevelNow puts both at Sat 7 17:00 too, and gives every date below.
        let mut h = task(6, "H", 0);
        h.predecessors = vec![Predecessor::fs(5)];
        let mut n = task(7, "N", 0);
        n.predecessors = vec![Predecessor::fs(3)];
        // Milestones whose CPM instant is held at a period's end must keep
        // it when their own index does not move: K by an MFO that no FS
        // instant reaches, X by an SNET behind an SS link to G, whose start
        // did not move.
        let mut k = task(8, "K", 0);
        k.predecessors = vec![Predecessor::fs(5)];
        k.constraint = ConstraintType::MustFinishOn;
        k.constraint_date = Some(at(13, 17));
        let mut x = task(9, "X", 0);
        x.predecessors = vec![Predecessor::working(5, LinkType::StartStart, 0)];
        x.constraint = ConstraintType::StartNoEarlierThan;
        x.constraint_date = Some(at(6, 17));
        // An honored MFO or FNLT caps a milestone as in CPM: K2 (FS on G and
        // FF on P1, which ends Fri 13), K3 (FF on G) and K4 (FS on G) keep
        // their MFO instants; F1's FNLT Fri 13 does not bind, so it follows
        // G to Saturday, while F2's FNLT Fri 6 holds it there.
        let milestone = |uid, name, preds: Vec<Predecessor>, constraint, date| {
            let mut t = task(uid, name, 0);
            t.predecessors = preds;
            t.constraint = constraint;
            t.constraint_date = Some(date);
            t
        };
        let mut p1 = task(10, "P1", 480);
        p1.constraint = ConstraintType::FinishNoEarlierThan;
        p1.constraint_date = Some(at(13, 17));
        let ff = |uid| Predecessor::working(uid, LinkType::FinishFinish, 0);
        let (mfo, fnlt) = (
            ConstraintType::MustFinishOn,
            ConstraintType::FinishNoLaterThan,
        );
        let k2 = milestone(11, "K2", vec![Predecessor::fs(5), ff(10)], mfo, at(13, 17));
        let k3 = milestone(12, "K3", vec![ff(5)], mfo, at(6, 17));
        let k4 = milestone(13, "K4", vec![Predecessor::fs(5)], mfo, at(6, 17));
        let f1 = milestone(14, "F1", vec![Predecessor::fs(5)], fnlt, at(13, 17));
        let f2 = milestone(15, "F2", vec![Predecessor::fs(5)], fnlt, at(6, 17));
        // Two FS links: Q never moves and places Z at Sat 7 12:00 (19
        // elapsed hours after Fri 6 17:00) in CPM, but G's leveled instant
        // on the same index is later, so Z and Z2 (FNLT Fri 13) follow G,
        // and Z3, on Q alone, stays.
        let mut q = task(16, "Q", 480);
        q.constraint = ConstraintType::StartNoEarlierThan;
        q.constraint_date = Some(at(6, 8));
        let eh19 = lag_link(16, LinkType::FinishStart, 19 * 60, 6);
        let z_preds = vec![Predecessor::fs(5), eh19];
        let mut z = task(17, "Z", 0);
        z.predecessors = z_preds.clone();
        let z2 = milestone(18, "Z2", z_preds, fnlt, at(13, 17));
        let mut z3 = task(19, "Z3", 0);
        z3.predecessors = vec![eh19];
        // Both FS predecessors (G and M) move to Sat 7 17:00: Z6 follows,
        // and Z7's FNLT Fri 6 17:00 holds it.
        let both = vec![Predecessor::fs(5), Predecessor::fs(3)];
        let mut z6 = task(20, "Z6", 0);
        z6.predecessors = both.clone();
        let z7 = milestone(21, "Z7", both, fnlt, at(6, 17));
        let mut proj = Project {
            start_date: Some(at(2, 8)),
            tasks: vec![
                task(4, "Busy", 480),
                task(1, "A", 3 * 480),
                d,
                m,
                g,
                h,
                n,
                k,
                x,
                p1,
                k2,
                k3,
                k4,
                f1,
                f2,
                q,
                z,
                z2,
                z3,
                z6,
                z7,
            ],
            ..Project::default()
        };
        proj.resources = vec![worker(1, "Shared", 1.0)];
        proj.assignments = vec![assign(1, 4, 1, 1.0), assign(2, 1, 1, 1.0)];
        let leveled = level(&proj);
        let dates = |uid| (leveled.start(uid).unwrap(), leveled.finish(uid).unwrap());
        assert_eq!(dates(1), (at(3, 8), at(5, 17)));
        assert_eq!(dates(2), (at(9, 8), at(9, 17)));
        assert_eq!(dates(3), (at(7, 17), at(7, 17)));
        assert_eq!(dates(5), (at(6, 8), at(7, 17)));
        assert_eq!(dates(6), (at(7, 17), at(7, 17)));
        assert_eq!(dates(7), (at(7, 17), at(7, 17)));
        assert_eq!(dates(8), (at(13, 17), at(13, 17)));
        assert_eq!(dates(9), (at(6, 17), at(6, 17)));
        assert_eq!(dates(10), (at(13, 8), at(13, 17)));
        for (uid, expected) in [(11, at(13, 17)), (12, at(6, 17)), (13, at(6, 17))] {
            assert_eq!(dates(uid), (expected, expected), "MFO milestone {uid}");
        }
        assert_eq!(dates(14), (at(7, 17), at(7, 17)), "F1");
        assert_eq!(dates(15), (at(6, 17), at(6, 17)), "F2");
        assert_eq!(dates(16), (at(6, 8), at(6, 17)), "Q");
        assert_eq!(dates(17), (at(7, 17), at(7, 17)), "Z");
        assert_eq!(dates(18), (at(7, 17), at(7, 17)), "Z2");
        assert_eq!(dates(19), (at(7, 12), at(7, 12)), "Z3");
        assert_eq!(dates(20), (at(7, 17), at(7, 17)), "Z6");
        assert_eq!(dates(21), (at(6, 17), at(6, 17)), "Z7");
    }

    // ---- manual summaries (#124) ----
    //
    // Each case is one Microsoft Project measured over COM (anchor Monday
    // 2026-03-02 08:00, standard calendar): every task's dates, total slack
    // and critical flag, each summary's rolled-up span and the project finish.

    const DAY: i64 = 480;

    /// A manually scheduled summary at `level` pinned to `start`..`finish`.
    fn msum(uid: i32, level: u32, start: DateTime, finish: Option<DateTime>) -> Task {
        Task {
            summary: true,
            manual: true,
            manual_start: Some(start),
            manual_finish: finish,
            outline_level: level,
            ..task(uid, "M", 0)
        }
    }

    fn asum(uid: i32, level: u32) -> Task {
        Task {
            summary: true,
            outline_level: level,
            ..task(uid, "P", 0)
        }
    }

    fn sub(uid: i32, days: i64, level: u32, preds: &[i32]) -> Task {
        Task {
            outline_level: level,
            predecessors: preds.iter().map(|&u| fs(u)).collect(),
            ..task(uid, "T", days * DAY)
        }
    }

    /// Dates (day of March, hour), total slack in days and critical flag.
    fn expect(s: &Schedule, uid: i32, from: (u32, u32), to: (u32, u32), slack: i64, crit: bool) {
        let r = s
            .get(uid)
            .unwrap_or_else(|| panic!("task {uid} unscheduled"));
        assert_eq!(
            (r.early_start, r.early_finish, r.total_slack_min, r.critical),
            (at(from.0, from.1), at(to.0, to.1), slack * DAY, crit),
            "task {uid}"
        );
    }

    /// [`manual_warning`] on the scheduled dates.
    fn warns(proj: &Project, s: &Schedule, uid: i32) -> bool {
        manual_warning(
            proj,
            uid,
            |u| s.get(u).map(|r| r.early_finish),
            |u| s.rolled_up(u),
        )
    }

    fn late(s: &Schedule, uid: i32) -> (DateTime, DateTime) {
        let r = s.get(uid).unwrap();
        (r.late_start, r.late_finish)
    }

    /// c1/c2: S (manual) over A 2d and B 3d (FS A); C 1d (FS B) outside S.
    fn c_base(finish: DateTime) -> Project {
        march2(vec![
            msum(1, 1, at(2, 8), Some(finish)),
            sub(2, 2, 2, &[]),
            sub(3, 3, 2, &[2]),
            sub(4, 1, 1, &[3]),
        ])
    }

    #[test]
    fn manual_summary_shorter_than_its_subtasks_keeps_its_dates_and_warns() {
        let proj = c_base(at(4, 17));
        let s = schedule(&proj);
        expect(&s, 1, (2, 8), (4, 17), 0, true);
        assert_eq!(late(&s, 1), (at(2, 8), at(6, 17)));
        assert_eq!(s.rolled_up(1), Some((at(2, 8), at(6, 17))));
        assert!(warns(&proj, &s, 1));
        expect(&s, 2, (2, 8), (3, 17), 0, true);
        expect(&s, 3, (4, 8), (6, 17), 0, true);
        expect(&s, 4, (9, 8), (9, 17), 0, true);
        assert_eq!(s.project_finish, at(9, 17));
    }

    #[test]
    fn manual_summary_finish_extends_the_project_finish() {
        let proj = c_base(at(20, 17));
        let s = schedule(&proj);
        expect(&s, 1, (2, 8), (20, 17), 0, true);
        assert_eq!(late(&s, 1), (at(13, 8), at(20, 17)));
        assert_eq!(s.rolled_up(1), Some((at(2, 8), at(6, 17))));
        assert!(!warns(&proj, &s, 1));
        expect(&s, 2, (2, 8), (3, 17), 9, false);
        expect(&s, 3, (4, 8), (6, 17), 9, false);
        expect(&s, 4, (9, 8), (9, 17), 9, false);
        assert_eq!(s.project_finish, at(20, 17));
    }

    #[test]
    fn a_link_pushes_subtasks_past_the_manual_start_floor() {
        // c8: S from 3/9; Z 10d outside, A after Z.
        let proj = march2(vec![
            msum(1, 1, at(9, 8), Some(at(13, 17))),
            sub(2, 2, 2, &[5]),
            sub(3, 3, 2, &[2]),
            sub(4, 1, 1, &[3]),
            sub(5, 10, 1, &[]),
        ]);
        let s = schedule(&proj);
        expect(&s, 1, (9, 8), (13, 17), 5, false);
        assert_eq!(late(&s, 1), (at(16, 8), at(20, 17)));
        assert_eq!(s.get(1).unwrap().free_slack_min, 5 * DAY);
        assert_eq!(s.rolled_up(1), Some((at(16, 8), at(20, 17))));
        assert!(warns(&proj, &s, 1));
        expect(&s, 2, (16, 8), (17, 17), 0, true);
        expect(&s, 3, (18, 8), (20, 17), 0, true);
        expect(&s, 4, (23, 8), (23, 17), 0, true);
        expect(&s, 5, (2, 8), (13, 17), 0, true);
        assert_eq!(s.project_finish, at(23, 17));
    }

    #[test]
    fn manual_summary_off_the_critical_path_of_its_subtasks() {
        // c9: S 3/2..3/4; Z 3d outside, A after Z.
        let proj = march2(vec![
            msum(1, 1, at(2, 8), Some(at(4, 17))),
            sub(2, 2, 2, &[5]),
            sub(3, 3, 2, &[2]),
            sub(4, 1, 1, &[3]),
            sub(5, 3, 1, &[]),
        ]);
        let s = schedule(&proj);
        expect(&s, 1, (2, 8), (4, 17), 3, false);
        assert_eq!(late(&s, 1), (at(5, 8), at(11, 17)));
        assert_eq!(s.rolled_up(1), Some((at(5, 8), at(11, 17))));
        assert!(warns(&proj, &s, 1));
        expect(&s, 2, (5, 8), (6, 17), 0, true);
        expect(&s, 3, (9, 8), (11, 17), 0, true);
        expect(&s, 4, (12, 8), (12, 17), 0, true);
        expect(&s, 5, (2, 8), (4, 17), 0, true);
        assert_eq!(s.project_finish, at(12, 17));
    }

    #[test]
    fn manual_start_floors_unlinked_subtasks() {
        // c7: S moved to 3/9 carries A (no constraint written) with it.
        let proj = march2(vec![
            msum(1, 1, at(9, 8), Some(at(13, 17))),
            sub(2, 2, 2, &[]),
            sub(3, 3, 2, &[2]),
            sub(4, 1, 1, &[3]),
        ]);
        let s = schedule(&proj);
        expect(&s, 1, (9, 8), (13, 17), 0, true);
        expect(&s, 2, (9, 8), (10, 17), 0, true);
        expect(&s, 3, (11, 8), (13, 17), 0, true);
        expect(&s, 4, (16, 8), (16, 17), 0, true);
        assert!(!warns(&proj, &s, 1));
    }

    fn p1(finish: DateTime) -> Project {
        march2(vec![
            asum(1, 1),
            msum(2, 2, at(2, 8), Some(finish)),
            sub(3, 2, 3, &[]),
            sub(4, 3, 3, &[3]),
            sub(5, 1, 1, &[4]),
        ])
    }

    #[test]
    fn auto_summary_rolls_up_through_a_short_manual_summary() {
        let proj = p1(at(4, 17));
        let s = schedule(&proj);
        expect(&s, 1, (2, 8), (6, 17), 0, true);
        assert_eq!(s.rolled_up(1), Some((at(2, 8), at(6, 17))));
        expect(&s, 2, (2, 8), (4, 17), 0, true);
        assert_eq!(s.rolled_up(2), Some((at(2, 8), at(6, 17))));
        assert!(!warns(&proj, &s, 1));
        assert!(warns(&proj, &s, 2));
        assert_eq!(s.project_finish, at(9, 17));
    }

    #[test]
    fn auto_summary_rolls_up_a_long_manual_summary_s_own_dates() {
        let s = schedule(&p1(at(20, 17)));
        expect(&s, 1, (2, 8), (20, 17), 0, true);
        assert_eq!(s.rolled_up(1), Some((at(2, 8), at(20, 17))));
        expect(&s, 2, (2, 8), (20, 17), 0, true);
        assert_eq!(s.rolled_up(2), Some((at(2, 8), at(6, 17))));
        expect(&s, 3, (2, 8), (3, 17), 9, false);
        expect(&s, 4, (4, 8), (6, 17), 9, false);
        expect(&s, 5, (9, 8), (9, 17), 9, false);
        assert_eq!(s.project_finish, at(20, 17));
    }

    #[test]
    fn auto_summary_over_a_manual_summary_is_critical_through_its_fixed_span() {
        // p1b: P over S (3/2..3/4, 3d of slack) whose A follows Z.
        let proj = march2(vec![
            asum(1, 1),
            msum(2, 2, at(2, 8), Some(at(4, 17))),
            sub(3, 2, 3, &[5]),
            sub(4, 3, 3, &[3]),
            sub(5, 3, 1, &[]),
        ]);
        let s = schedule(&proj);
        expect(&s, 1, (2, 8), (11, 17), 0, true);
        expect(&s, 2, (2, 8), (4, 17), 3, false);
        expect(&s, 3, (5, 8), (6, 17), 0, true);
        expect(&s, 4, (9, 8), (11, 17), 0, true);
        expect(&s, 5, (2, 8), (4, 17), 0, true);
        assert_eq!(s.project_finish, at(11, 17));
    }

    #[test]
    fn auto_summary_over_a_manual_summary_is_bound_by_its_fixed_span() {
        // Fixture 27's P: S1 (3/2..3/4) and its subtasks all have 9d of
        // slack, but P's late start is S1's own start, so P has none.
        let proj = march2(vec![
            asum(1, 1),
            msum(2, 2, at(2, 8), Some(at(4, 17))),
            sub(3, 2, 3, &[]),
            sub(4, 3, 3, &[3]),
            sub(5, 1, 1, &[4]),
            msum(6, 1, at(9, 8), Some(at(20, 17))),
        ]);
        let s = schedule(&proj);
        expect(&s, 1, (2, 8), (6, 17), 0, true);
        assert_eq!(late(&s, 1), (at(2, 8), at(19, 17)));
        expect(&s, 2, (2, 8), (4, 17), 9, false);
        for uid in [3, 4, 5] {
            assert_eq!(s.get(uid).unwrap().total_slack_min, 9 * DAY, "task {uid}");
        }
        // p1b's late window: S's own start, B's late finish.
        let proj = march2(vec![
            asum(1, 1),
            msum(2, 2, at(2, 8), Some(at(4, 17))),
            sub(3, 2, 3, &[5]),
            sub(4, 3, 3, &[3]),
            sub(5, 3, 1, &[]),
        ]);
        assert_eq!(late(&schedule(&proj), 1), (at(2, 8), at(11, 17)));
    }

    #[test]
    fn nested_manual_summaries_roll_up_their_leaves_and_each_other() {
        // p2: O (3/2) > I (3/2..3/3) > A, B; D in O.
        let proj = march2(vec![
            msum(1, 1, at(2, 8), Some(at(2, 17))),
            msum(2, 2, at(2, 8), Some(at(3, 17))),
            sub(3, 2, 3, &[]),
            sub(4, 3, 3, &[3]),
            sub(5, 1, 2, &[]),
        ]);
        let s = schedule(&proj);
        expect(&s, 1, (2, 8), (2, 17), 0, true);
        assert_eq!(late(&s, 1), (at(2, 8), at(6, 17)));
        assert_eq!(s.rolled_up(1), Some((at(2, 8), at(6, 17))));
        expect(&s, 2, (2, 8), (3, 17), 0, true);
        assert_eq!(s.rolled_up(2), Some((at(2, 8), at(6, 17))));
        expect(&s, 3, (2, 8), (3, 17), 0, true);
        expect(&s, 4, (4, 8), (6, 17), 0, true);
        expect(&s, 5, (2, 8), (2, 17), 4, false);
        assert_eq!(s.project_finish, at(6, 17));

        // p2b: O (3/2..3/3) > I (3/2..3/20) > A, B.
        let proj = march2(vec![
            msum(1, 1, at(2, 8), Some(at(3, 17))),
            msum(2, 2, at(2, 8), Some(at(20, 17))),
            sub(3, 2, 3, &[]),
            sub(4, 3, 3, &[3]),
        ]);
        let s = schedule(&proj);
        expect(&s, 1, (2, 8), (3, 17), 0, true);
        assert_eq!(late(&s, 1), (at(2, 8), at(20, 17)));
        assert_eq!(s.rolled_up(1), Some((at(2, 8), at(20, 17))));
        expect(&s, 2, (2, 8), (20, 17), 0, true);
        assert_eq!(late(&s, 2), (at(16, 8), at(20, 17)));
        assert_eq!(s.rolled_up(2), Some((at(2, 8), at(6, 17))));
        // I finishes after O, its manual parent: Project warns on both.
        assert!(warns(&proj, &s, 1));
        assert!(warns(&proj, &s, 2));
        expect(&s, 3, (2, 8), (3, 17), 10, false);
        expect(&s, 4, (4, 8), (6, 17), 10, false);
        assert_eq!(s.project_finish, at(20, 17));
    }

    #[test]
    fn a_manual_parent_sees_a_nested_manual_summary_as_fixed() {
        // r1: O (3/2..3/3) > I (3/9..3/10) > A; Z 15d sets the finish.
        let proj = march2(vec![
            msum(1, 1, at(2, 8), Some(at(3, 17))),
            msum(2, 2, at(9, 8), Some(at(10, 17))),
            sub(3, 2, 3, &[]),
            sub(4, 15, 1, &[]),
        ]);
        let s = schedule(&proj);
        expect(&s, 1, (2, 8), (3, 17), 5, false);
        assert_eq!(late(&s, 1), (at(9, 8), at(20, 17)));
        assert_eq!(s.rolled_up(1), Some((at(9, 8), at(10, 17))));
        expect(&s, 2, (9, 8), (10, 17), 8, false);
        assert_eq!(late(&s, 2), (at(19, 8), at(20, 17)));
        assert_eq!(s.rolled_up(2), Some((at(9, 8), at(10, 17))));
        expect(&s, 3, (9, 8), (10, 17), 8, false);
        expect(&s, 4, (2, 8), (20, 17), 0, true);
        assert_eq!(s.project_finish, at(20, 17));
        // O's subtasks run past it; I runs past O, though not past itself.
        assert!(warns(&proj, &s, 1));
        assert!(warns(&proj, &s, 2));

        // r1b: O (3/2..3/13) > I (3/4..3/5) > A.
        let proj = march2(vec![
            msum(1, 1, at(2, 8), Some(at(13, 17))),
            msum(2, 2, at(4, 8), Some(at(5, 17))),
            sub(3, 2, 3, &[]),
            sub(4, 15, 1, &[]),
        ]);
        let s = schedule(&proj);
        expect(&s, 1, (2, 8), (13, 17), 2, false);
        assert_eq!(late(&s, 1), (at(4, 8), at(20, 17)));
        assert_eq!(s.rolled_up(1), Some((at(4, 8), at(5, 17))));
        expect(&s, 2, (4, 8), (5, 17), 11, false);
        assert_eq!(s.rolled_up(2), Some((at(4, 8), at(5, 17))));
        expect(&s, 3, (4, 8), (5, 17), 11, false);
        expect(&s, 4, (2, 8), (20, 17), 0, true);
        assert!(!warns(&proj, &s, 1));
        assert!(!warns(&proj, &s, 2));
    }

    #[test]
    fn a_manual_task_warns_only_past_its_direct_manual_parent() {
        // w1: a manual leaf finishing after its manual summary.
        let proj = march2(vec![
            msum(1, 1, at(2, 8), Some(at(3, 17))),
            Task {
                outline_level: 2,
                manual_finish: Some(at(6, 17)),
                ..manual(2, "M", 5 * DAY, at(2, 8))
            },
        ]);
        let s = schedule(&proj);
        expect(&s, 1, (2, 8), (3, 17), 0, true);
        expect(&s, 2, (2, 8), (6, 17), 0, true);
        assert!(warns(&proj, &s, 1));
        assert!(warns(&proj, &s, 2));
        // w2: an auto summary between them: I is not O's direct subtask.
        let proj = march2(vec![
            msum(1, 1, at(2, 8), Some(at(13, 17))),
            asum(2, 2),
            msum(3, 3, at(2, 8), Some(at(20, 17))),
            sub(4, 1, 4, &[]),
        ]);
        let s = schedule(&proj);
        expect(&s, 1, (2, 8), (13, 17), 0, true);
        expect(&s, 2, (2, 8), (20, 17), 0, true);
        expect(&s, 3, (2, 8), (20, 17), 0, true);
        assert_eq!(late(&s, 3), (at(20, 8), at(20, 17)));
        expect(&s, 4, (2, 8), (2, 17), 14, false);
        assert!(warns(&proj, &s, 1));
        assert!(!warns(&proj, &s, 3));
        // w3: I starts before O and finishes within it.
        let proj = march2(vec![
            msum(1, 1, at(4, 8), Some(at(10, 17))),
            msum(2, 2, at(2, 8), Some(at(3, 17))),
            sub(3, 1, 3, &[]),
        ]);
        let s = schedule(&proj);
        expect(&s, 1, (4, 8), (10, 17), 0, true);
        expect(&s, 2, (2, 8), (3, 17), 5, false);
        expect(&s, 3, (2, 8), (2, 17), 6, false);
        assert!(!warns(&proj, &s, 1));
        assert!(!warns(&proj, &s, 2));
        // Auto tasks and auto summaries never warn.
        assert!(!warns(&proj, &s, 3));
    }

    #[test]
    fn the_nearest_manual_summary_floors_a_subtask() {
        // p2c: O from 3/9 > I from 3/4 > A: A takes I's floor, not O's.
        let proj = march2(vec![
            msum(1, 1, at(9, 8), Some(at(10, 17))),
            msum(2, 2, at(4, 8), Some(at(5, 17))),
            sub(3, 2, 3, &[]),
        ]);
        let s = schedule(&proj);
        expect(&s, 1, (9, 8), (10, 17), 0, true);
        assert_eq!(late(&s, 1), (at(9, 8), at(10, 17)));
        assert_eq!(s.rolled_up(1), Some((at(4, 8), at(5, 17))));
        expect(&s, 2, (4, 8), (5, 17), 3, false);
        assert_eq!(late(&s, 2), (at(9, 8), at(10, 17)));
        expect(&s, 3, (4, 8), (5, 17), 3, false);
        assert!(!warns(&proj, &s, 1));
        assert!(!warns(&proj, &s, 2));
        assert_eq!(s.project_finish, at(10, 17));

        // q1: an auto summary between them passes the floor through.
        let proj = march2(vec![
            msum(1, 1, at(9, 8), Some(at(10, 17))),
            asum(2, 2),
            sub(3, 2, 3, &[]),
        ]);
        let s = schedule(&proj);
        expect(&s, 1, (9, 8), (10, 17), 0, true);
        expect(&s, 2, (9, 8), (10, 17), 0, true);
        expect(&s, 3, (9, 8), (10, 17), 0, true);
        assert_eq!(s.project_finish, at(10, 17));
    }

    /// f1/f4: S from 3/9 over A 2d with `constraint` on 3/3, and B 1d.
    fn constrained_under_floor(constraint: ConstraintType) -> (Project, Schedule) {
        let mut a = sub(2, 2, 2, &[]);
        a.constraint = constraint;
        a.constraint_date = Some(at(3, 8));
        let proj = march2(vec![
            msum(1, 1, at(9, 8), Some(at(10, 17))),
            a,
            sub(3, 1, 2, &[]),
        ]);
        let s = schedule(&proj);
        (proj, s)
    }

    #[test]
    fn a_constrained_subtask_ignores_the_floor() {
        // f1: MSO 3/3. The summary's late start never precedes its start.
        let (proj, s) = constrained_under_floor(ConstraintType::MustStartOn);
        expect(&s, 1, (9, 8), (10, 17), 0, true);
        assert_eq!(late(&s, 1), (at(9, 8), at(10, 17)));
        assert_eq!(s.rolled_up(1), Some((at(3, 8), at(9, 17))));
        assert!(!warns(&proj, &s, 1));
        expect(&s, 2, (3, 8), (4, 17), 0, true);
        expect(&s, 3, (9, 8), (9, 17), 1, false);
        assert_eq!(s.project_finish, at(10, 17));

        // f4: even a start-no-earlier-than before the floor wins.
        let (proj, s) = constrained_under_floor(ConstraintType::StartNoEarlierThan);
        expect(&s, 1, (9, 8), (10, 17), 0, true);
        assert!(!warns(&proj, &s, 1));
        expect(&s, 2, (3, 8), (4, 17), 4, false);
        expect(&s, 3, (9, 8), (9, 17), 1, false);

        // q3: a linked subtask with an SNET ignores it too.
        let mut a = sub(2, 2, 2, &[3]);
        a.constraint = ConstraintType::StartNoEarlierThan;
        a.constraint_date = Some(at(4, 8));
        let s = schedule(&march2(vec![
            msum(1, 1, at(9, 8), Some(at(10, 17))),
            a,
            sub(3, 1, 1, &[]),
        ]));
        expect(&s, 1, (9, 8), (10, 17), 0, true);
        expect(&s, 2, (4, 8), (5, 17), 3, false);
        expect(&s, 3, (2, 8), (2, 17), 4, false);
    }

    #[test]
    fn manual_subtask_of_a_manual_summary_stays_pinned() {
        let s = schedule(&march2(vec![
            msum(1, 1, at(9, 8), Some(at(10, 17))),
            Task {
                outline_level: 2,
                ..manual(2, "Pinned", DAY, at(3, 8))
            },
        ]));
        let r = s.get(2).unwrap();
        assert_eq!((r.early_start, r.early_finish), (at(3, 8), at(3, 17)));
        assert_eq!(s.rolled_up(1), Some((at(3, 8), at(3, 17))));
    }

    #[test]
    fn manual_summary_finish_follows_its_manual_duration() {
        let mut m = msum(1, 1, at(9, 8), None);
        m.manual_duration_min = Some(3 * DAY);
        let s = schedule(&march2(vec![m, sub(2, 1, 2, &[])]));
        expect(&s, 1, (9, 8), (11, 17), 0, true);
        expect(&s, 2, (9, 8), (9, 17), 2, false);
        assert_eq!(s.project_finish, at(11, 17));
        // Neither a finish nor a duration: the span is the start alone.
        let s = schedule(&march2(vec![msum(1, 1, at(9, 8), None), sub(2, 1, 2, &[])]));
        let r = s.get(1).unwrap();
        assert_eq!((r.early_start, r.early_finish), (at(9, 8), at(9, 8)));
    }

    #[test]
    fn manual_summary_with_nothing_scheduled_keeps_its_dates() {
        // Its only subtask sits on a calendar with no working time.
        let mut kid = sub(2, 1, 2, &[]);
        kid.calendar_uid = Some(3);
        let mut proj = march2(vec![
            msum(1, 1, at(9, 8), Some(at(10, 17))),
            kid,
            task(4, "X", DAY),
        ]);
        proj.calendars = vec![Calendar::standard(1), closed_calendar(3)];
        let s = schedule(&proj);
        assert!(s.get(2).is_none());
        let r = *s.get(1).unwrap();
        assert_eq!(
            (r.early_start, r.early_finish, r.late_start, r.late_finish),
            (at(9, 8), at(10, 17), at(9, 8), at(10, 17))
        );
        assert_eq!((r.total_slack_min, r.critical), (0, false));
        assert_eq!(s.rolled_up(1), None);
        assert!(!warns(&proj, &s, 1));
        assert_eq!(s.project_finish, at(10, 17));
    }

    #[test]
    fn manual_summary_without_a_start_rolls_up_as_an_auto_summary() {
        let run = |summary: Task| {
            schedule(&march2(vec![
                summary,
                sub(2, 2, 2, &[]),
                sub(3, 3, 2, &[2]),
                sub(4, 1, 1, &[3]),
            ]))
        };
        let tbd = Task {
            manual: true,
            ..asum(1, 1)
        };
        let (tbd, auto) = (run(tbd), run(asum(1, 1)));
        for uid in 1..=4 {
            assert_eq!(tbd.get(uid), auto.get(uid), "task {uid}");
            assert_eq!(tbd.rolled_up(uid), auto.rolled_up(uid), "task {uid}");
        }
        assert_eq!(auto.rolled_up(1), Some((at(2, 8), at(6, 17))));
        // Nor does it floor anything: an outer manual summary's start does.
        let s = schedule(&march2(vec![
            msum(1, 1, at(9, 8), Some(at(13, 17))),
            Task {
                manual: true,
                ..asum(2, 2)
            },
            sub(3, 2, 3, &[]),
        ]));
        expect(&s, 3, (9, 8), (10, 17), 3, false);
    }

    #[test]
    fn leveling_keeps_manual_summary_dates_and_levels_the_rollup() {
        // Both subtasks share one resource: leveling pushes one after the other.
        let mut proj = march2(vec![
            msum(1, 1, at(9, 8), Some(at(10, 17))),
            sub(2, 2, 2, &[]),
            sub(3, 2, 2, &[]),
        ]);
        proj.resources.push(Resource {
            uid: 1,
            name: "R".into(),
            max_units: 1.0,
            ..Resource::default()
        });
        for task_uid in [2, 3] {
            proj.assignments.push(Assignment {
                uid: task_uid,
                task_uid,
                resource_uid: 1,
                units: 1.0,
                work_min: 2 * DAY,
                ..Assignment::default()
            });
        }
        let lv = level(&proj);
        assert_eq!(
            (lv.start(1), lv.finish(1)),
            (Some(at(9, 8)), Some(at(10, 17)))
        );
        // The floor survives: nothing starts before 3/9.
        assert_eq!(lv.start(2), Some(at(9, 8)));
        assert_eq!(lv.start(3), Some(at(11, 8)));
        assert_eq!(lv.rolled_up(1), Some((at(9, 8), at(12, 17))));
        assert_eq!(schedule(&proj).rolled_up(1), Some((at(9, 8), at(10, 17))));
        assert_eq!(lv.project_finish, at(12, 17));
    }
}
