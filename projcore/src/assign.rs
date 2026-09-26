//! Assignment dates and costs derived from the schedule, and the refresh that
//! keeps an edited plan's stored assignment, task and resource totals in step.
//!
//! A saved plan says `ProjectExternallyEdited=0`, so Project trusts the
//! assignment `Start`/`Finish`/`Cost` and the resource and task totals it
//! reads. docxy stores them as read; after an edit, `refresh` rewrites the
//! ones whose inputs the edit changed and nothing else, so an unedited value
//! stays exactly as Project wrote it. The model is simpler than Project's:
//! assignments use their task's calendar (resource calendars are not
//! modelled), work is spread flat, and overtime is never scheduled.
//!
//! Units, as MSPDI stores them: `StandardRate`/`OvertimeRate` are currency per
//! hour (per unit for a material), `CostPerUse` and every cost are in
//! hundredths, `Delay`/`LevelingDelay` are tenths of a minute.

use crate::datetime::DateTime;
use crate::model::{
    Assignment, Calendar, LagFormat, LagKind, Project, Rate, Resource, ResourceType, Task,
    TimephasedValue, WorkCalendar,
};
use crate::schedule::{HORIZON_DAYS, Schedule, working_minutes_on};
use std::collections::{HashMap, HashSet};

/// The calendar a task is scheduled on: its own, else the project default,
/// else Standard.
pub(crate) fn task_calendar(proj: &Project, task: &Task) -> WorkCalendar {
    match proj
        .calendar(task.calendar_uid.unwrap_or(proj.default_calendar_uid))
        .or_else(|| proj.calendar(proj.default_calendar_uid))
    {
        Some(cal) => proj.resolved_calendar(cal),
        None => WorkCalendar::weekly(Calendar::standard_week()),
    }
}

/// `minutes` of working time on `cal` after `from`. As a start (`finish`
/// false) an instant at the end of a working period moves on to the next
/// period's start, as a task starting there starts the next morning; as a
/// finish it stays at the end. `None` when the horizon has no such time.
fn advance(cal: &WorkCalendar, from: DateTime, minutes: i64, finish: bool) -> Option<DateTime> {
    let from = from.minutes();
    let mut left = minutes.max(0);
    if finish && left == 0 {
        return Some(DateTime::from_minutes(from));
    }
    if !cal.has_working_time() {
        return None;
    }
    let first = from.div_euclid(1440);
    let mut slots = Vec::new();
    for day in first..first + HORIZON_DAYS {
        cal.day_into(day, &mut slots);
        for t in &slots {
            let s = (day * 1440 + i64::from(t.from)).max(from);
            let e = day * 1440 + i64::from(t.to);
            if s >= e {
                continue;
            }
            if left < e - s || (finish && left == e - s) {
                return Some(DateTime::from_minutes(s + left));
            }
            left -= e - s;
        }
    }
    None
}

/// Whether the assignment's work runs flat at its units, so its own finish is
/// its start plus work ÷ units: a work resource (or none, like Project's
/// unassigned -65535), no contour, positive units.
fn is_flat_work(proj: &Project, a: &Assignment) -> bool {
    is_work(&proj.resources, a)
        && matches!(a.work_contour, None | Some(0))
        && a.units.is_finite()
        && a.units > 0.0
}

/// Whether an assignment's work is working time: a work resource's, or one
/// without a resource (Project's unassigned -65535). A material's work is a
/// quantity and a cost resource has none, so a task's Work leaves them out.
fn is_work(resources: &[Resource], a: &Assignment) -> bool {
    resources
        .iter()
        .find(|r| r.uid == a.resource_uid)
        .is_none_or(|r| r.kind == ResourceType::Work)
}

/// Where the assignment starts on its task: the task's start advanced by the
/// `Delay` (working time) and then the `LevelingDelay` (elapsed or working, as
/// its format says).
fn delayed_start(cal: &WorkCalendar, task_start: DateTime, a: &Assignment) -> Option<DateTime> {
    let mut start = task_start;
    let delay = a.delay_min();
    if delay > 0 {
        start = advance(cal, start, delay, false)?;
    }
    let leveling = a.leveling_delay_min();
    if leveling > 0 {
        let elapsed = a
            .leveling_delay_format
            .and_then(|code| LagFormat::from_code(i64::from(code)))
            .is_some_and(|f| f.kind() == LagKind::Elapsed);
        start = if elapsed {
            advance(cal, start.add_minutes(leveling), 0, false)?
        } else {
            advance(cal, start, leveling, false)?
        };
    }
    Some(start)
}

/// An assignment's own start and finish on its task's schedule. The start is
/// its `ActualStart`, else the task's scheduled start moved by its delays; the
/// finish is its `ActualFinish`, else, for flat work, the start plus work ÷
/// units of working time (so assignments on one task finish apart), else the
/// task's finish. Never finishes before it starts. `None` when its task is
/// not scheduled.
pub fn assignment_span(
    proj: &Project,
    sched: &Schedule,
    a: &Assignment,
) -> Option<(DateTime, DateTime)> {
    let task = proj.task(a.task_uid)?;
    let r = sched.get(task.uid)?;
    let cal = task_calendar(proj, task);
    let start = match a.actual_start {
        Some(start) => start,
        None => delayed_start(&cal, r.early_start, a)?,
    };
    let finish = match a.actual_finish {
        Some(finish) => finish,
        None if is_flat_work(proj, a) => {
            let span = (a.work_min.max(0) as f64 / a.units).round() as i64;
            advance(&cal, start, span, true)?
        }
        None => r.early_finish,
    };
    Some((start, finish.max(start)))
}

/// A stored decimal as a number; absent or unreadable is 0.
pub(crate) fn value(rate: Option<&Rate>) -> f64 {
    rate.and_then(Rate::to_f64).unwrap_or(0.0)
}

/// A cost in whole hundredths, as the text a save writes.
pub(crate) fn money(hundredths: f64) -> Option<Rate> {
    // `+ 0.0` turns a rounded -0 into 0.
    Rate::from_f64(hundredths.round() + 0.0)
}

/// One effective-dated row of a cost rate table.
struct RateRow<'a> {
    from: Option<DateTime>,
    standard: Option<&'a Rate>,
    overtime: Option<&'a Rate>,
    per_use: Option<&'a Rate>,
}

/// Table `table`'s rows by start date. Each row holds from its `RatesFrom`
/// until the next row's, the first from the beginning of time and the last
/// forever, so rows never leave a gap or overlap: of several rows without a
/// `RatesFrom` (a malformed table) only the first counts. A resource without
/// rows for the table prices from its own rate fields.
fn rate_rows(r: &Resource, table: u8) -> Vec<RateRow<'_>> {
    let mut open = false;
    let mut rows: Vec<RateRow<'_>> = r
        .rates
        .iter()
        .filter(|e| e.rate_table.unwrap_or(0) == table)
        .filter(|e| e.rates_from.is_some() || !std::mem::replace(&mut open, true))
        .map(|e| RateRow {
            from: e.rates_from,
            standard: e.standard_rate.as_ref(),
            overtime: e.overtime_rate.as_ref(),
            per_use: e.cost_per_use.as_ref(),
        })
        .collect();
    if rows.is_empty() {
        rows.push(RateRow {
            from: None,
            standard: r.standard_rate.as_ref(),
            overtime: r.overtime_rate.as_ref(),
            per_use: r.cost_per_use.as_ref(),
        });
    }
    rows.sort_by_key(|row| row.from);
    rows
}

/// The row in effect at `at`: the last one starting by then, else the first.
fn row_at<'r, 'a>(rows: &'r [RateRow<'a>], at: DateTime) -> &'r RateRow<'a> {
    rows.iter()
        .rev()
        .find(|row| row.from.is_none_or(|from| from <= at))
        .unwrap_or(&rows[0])
}

/// What an assignment costs over `span`, in hundredths, from its resource's
/// rate table `CostRateTable` (A by default). A work resource's regular work
/// (work less overtime) is spread flat over the span's working time and priced
/// at each dated row's standard rate for the part it covers; its overtime is
/// priced at the overtime rate in effect at the start, and the cost per use of
/// that row is added once. A material's quantity (its work, in hours) is
/// priced at the standard rate in effect at the start, plus the cost per use.
/// `None`, leaving the stored cost, for a cost resource, which carries its
/// cost as entered, and when there is no such resource.
pub fn assignment_cost(
    proj: &Project,
    a: &Assignment,
    (start, finish): (DateTime, DateTime),
) -> Option<Rate> {
    let r = proj.resources.iter().find(|r| r.uid == a.resource_uid)?;
    let rows = rate_rows(r, a.cost_rate_table.unwrap_or(0));
    let first = row_at(&rows, start);
    let per_use = value(first.per_use);
    let hundredths = match r.kind {
        ResourceType::Cost => return None,
        ResourceType::Material => {
            a.work_min.max(0) as f64 / 60.0 * value(first.standard) * 100.0 + per_use
        }
        ResourceType::Work => {
            let overtime = a.overtime_work_min.unwrap_or(0).max(0);
            let regular_h = (a.work_min - overtime).max(0) as f64 / 60.0;
            let cal = match proj.task(a.task_uid) {
                Some(task) => task_calendar(proj, task),
                None => WorkCalendar::weekly(Calendar::standard_week()),
            };
            let total = working_minutes_on(&cal, start, finish);
            let regular = if total <= 0 || rows.len() == 1 {
                regular_h * value(first.standard)
            } else {
                (0..rows.len())
                    .map(|i| {
                        let lo = if i == 0 {
                            start
                        } else {
                            rows[i].from.unwrap_or(start)
                        };
                        let hi = rows.get(i + 1).and_then(|next| next.from).unwrap_or(finish);
                        let (lo, hi) = (lo.max(start), hi.min(finish));
                        if lo >= hi {
                            return 0.0;
                        }
                        let share = working_minutes_on(&cal, lo, hi) as f64 / total as f64;
                        regular_h * share * value(rows[i].standard)
                    })
                    .sum()
            };
            let overtime_h = overtime as f64 / 60.0;
            (regular + overtime_h * value(first.overtime)) * 100.0 + per_use
        }
    };
    money(hundredths)
}

// ---- refresh after an edit ----------------------------------------------------

/// Whether anything an assignment's derived fields are computed from differs
/// between `before` (in `prev`, scheduled as `prev_sched`) and `a` (in `proj`,
/// scheduled as `sched`): its own inputs, its task's scheduled dates or
/// calendar, or its resource's type and rates.
fn inputs_changed(
    (prev, prev_sched): (&Project, &Schedule),
    (proj, sched): (&Project, &Schedule),
    before: &Assignment,
    a: &Assignment,
    calendar_changed: &HashSet<i32>,
) -> bool {
    let own = |a: &Assignment| {
        (
            a.task_uid,
            a.resource_uid,
            a.units.to_bits(),
            a.work_min,
            a.overtime_work_min,
            (a.delay, a.leveling_delay, a.leveling_delay_format),
            a.cost_rate_table,
            a.work_contour,
            (a.actual_start, a.actual_finish, a.actual_work_min),
            a.actual_cost.clone(),
        )
    };
    let dates = |sched: &Schedule, uid| sched.get(uid).map(|r| (r.early_start, r.early_finish));
    let pricing = |proj: &Project, uid| {
        proj.resources.iter().find(|r| r.uid == uid).map(|r| {
            (
                r.kind,
                r.standard_rate.clone(),
                r.overtime_rate.clone(),
                r.cost_per_use.clone(),
                r.rates.clone(),
            )
        })
    };
    own(before) != own(a)
        || dates(prev_sched, before.task_uid) != dates(sched, a.task_uid)
        || calendar_changed.contains(&a.task_uid)
        || pricing(prev, before.resource_uid) != pricing(proj, a.resource_uid)
}

/// The remaining work an assignment counts for in a total: its stored value,
/// else its work less its actual work.
fn remaining_work(a: &Assignment) -> i64 {
    a.remaining_work_min
        .unwrap_or_else(|| (a.work_min - a.actual_work_min.unwrap_or(0)).max(0))
}

/// The remaining cost an assignment counts for in a total: its stored value,
/// else its cost less its actual cost, else 0.
fn remaining_cost(a: &Assignment) -> f64 {
    match (&a.remaining_cost, &a.cost) {
        (Some(rc), _) => value(Some(rc)),
        (None, Some(c)) => (value(Some(c)) - value(a.actual_cost.as_ref())).max(0.0),
        (None, None) => 0.0,
    }
}

/// Work, cost, remaining work and remaining cost of a group of assignments.
type Totals = (i64, f64, i64, f64);

fn totals<'a>(assignments: impl Iterator<Item = &'a Assignment>) -> Totals {
    assignments.fold((0, 0.0, 0, 0.0), |(w, c, rw, rc), a| {
        (
            w + a.work_min,
            c + value(a.cost.as_ref()),
            rw + remaining_work(a),
            rc + remaining_cost(a),
        )
    })
}

/// The totals a task's stored fields count of its own assignments: the cost
/// of all of them, the work of those whose work is time ([`is_work`]).
fn task_totals(p: &Project, uid: i32) -> Totals {
    let mine = p.assignments.iter().filter(|a| a.task_uid == uid);
    let (_, cost, _, remaining_cost) = totals(mine.clone());
    let (work, _, remaining, _) = totals(mine.filter(|a| is_work(&p.resources, a)));
    (work, cost, remaining, remaining_cost)
}

/// The UIDs of a task's outline ancestors, nearest first, up to and including
/// the project summary (outline level 0) when the plan holds one.
fn ancestors(p: &Project, uid: i32) -> Vec<i32> {
    let Some(i) = p.tasks.iter().position(|t| t.uid == uid) else {
        return Vec::new();
    };
    let mut level = p.tasks[i].outline_level;
    let mut out = Vec::new();
    for t in p.tasks[..i].iter().rev().filter(|t| !t.is_null) {
        if level == 0 {
            break;
        }
        if t.outline_level < level {
            out.push(t.uid);
            level = t.outline_level;
        }
    }
    out
}

/// Refresh what an edit made stale. `prev` and `prev_sched` are the model and
/// schedule before the edit; `proj` is the edited model and `sched` its
/// schedule. The refresh reads only inputs, never the fields it writes, so
/// running it again on the same edit changes nothing.
///
/// - An assignment that is new, or whose inputs changed (see
///   [`inputs_changed`]), gets its `Start`/`Finish` from [`assignment_span`]
///   and its `Cost` from [`assignment_cost`] (a cost it cannot price stays),
///   `RegularWork` = work − overtime, `RemainingWork` = work − actual work and
///   `RemainingCost` = cost − actual cost. Its planned-work spread (Type 1)
///   is dropped when its dates or work moved.
/// - A task keeps its stored `Work`, `Cost`, `RemainingWork` and
///   `RemainingCost` but moves each by how much its assignments' total moved
///   (work counting work resources only), and so do its outline summaries; a
///   task moved in the outline takes its assignments' totals from its old
///   summaries to its new ones. A fixed cost inside a stored task cost
///   survives, and an absent total stays absent.
/// - A resource whose assignments changed (added, removed or refreshed) gets
///   its work, cost and remaining totals and its `Start`/`Finish` from them.
pub(crate) fn refresh(prev: &Project, prev_sched: &Schedule, proj: &mut Project, sched: &Schedule) {
    let calendar_changed: HashSet<i32> = proj
        .assignments
        .iter()
        .map(|a| a.task_uid)
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|&uid| {
            let cal = |p: &Project| p.task(uid).map(|t| task_calendar(p, t));
            cal(prev) != cal(proj)
        })
        .collect();
    let before: HashMap<i32, &Assignment> = prev.assignments.iter().map(|a| (a.uid, a)).collect();

    let mut refreshed = HashSet::new();
    for k in 0..proj.assignments.len() {
        let a = &proj.assignments[k];
        let was = before.get(&a.uid).copied();
        let stale = was.is_none_or(|b| {
            inputs_changed((prev, prev_sched), (proj, sched), b, a, &calendar_changed)
        });
        if !stale {
            continue;
        }
        let span = assignment_span(proj, sched, a);
        let cost = span.and_then(|span| assignment_cost(proj, a, span));
        let a = &mut proj.assignments[k];
        refreshed.insert(a.uid);
        if let Some((start, finish)) = span {
            a.start = Some(start);
            a.finish = Some(finish);
        }
        let moved = was.is_some_and(|b| {
            b.work_min != a.work_min
                || (span.is_some() && (b.start, b.finish) != (a.start, a.finish))
        });
        if moved {
            a.timephased_data
                .retain(|t| t.kind != TimephasedValue::REMAINING_WORK);
        }
        // A cost it cannot price (a cost resource's, entered by hand, or one
        // without a resource) stays as it was, even where a work edit
        // cleared it.
        a.cost = cost
            .or_else(|| a.cost.clone())
            .or_else(|| was.and_then(|b| b.cost.clone()));
        let overtime = a.overtime_work_min.unwrap_or(0).max(0);
        a.regular_work_min = Some((a.work_min - overtime).max(0));
        a.remaining_work_min = Some((a.work_min - a.actual_work_min.unwrap_or(0)).max(0));
        if let Some(cost) = &a.cost {
            let left = value(Some(cost)) - value(a.actual_cost.as_ref());
            a.remaining_cost = money(left.max(0.0));
        }
    }

    // Tasks: move the stored totals by the change in their assignments', and
    // every outline summary above them by the same. A task the edit moved in
    // the outline (or deleted, or added) leaves its old summaries with all it
    // had and joins its new ones with all it has, so each summary keeps its
    // stored total's relation to its subtasks.
    let changed: HashSet<i32> = prev
        .assignments
        .iter()
        .chain(&proj.assignments)
        .map(|a| a.task_uid)
        .collect();
    let mut moved: HashMap<i32, Totals> = HashMap::new();
    let mut add = |uid: i32, (w, c, rw, rc): Totals, sign: i64| {
        let sum = moved.entry(uid).or_insert((0, 0.0, 0, 0.0));
        let f = sign as f64;
        *sum = (
            sum.0 + sign * w,
            sum.1 + f * c,
            sum.2 + sign * rw,
            sum.3 + f * rc,
        );
    };
    for uid in changed {
        let (w0, c0, rw0, rc0) = task_totals(prev, uid);
        let (w1, c1, rw1, rc1) = task_totals(proj, uid);
        let delta = (w1 - w0, c1 - c0, rw1 - rw0, rc1 - rc0);
        let (was_under, now_under) = (ancestors(prev, uid), ancestors(proj, uid));
        if was_under == now_under {
            if delta == (0, 0.0, 0, 0.0) {
                continue;
            }
            for target in std::iter::once(uid).chain(now_under) {
                add(target, delta, 1);
            }
        } else {
            add(uid, delta, 1);
            for target in was_under {
                add(target, (w0, c0, rw0, rc0), -1);
            }
            for target in now_under {
                add(target, (w1, c1, rw1, rc1), 1);
            }
        }
    }
    for t in &mut proj.tasks {
        let (Some(&(dw, dc, drw, drc)), Some(was)) = (moved.get(&t.uid), prev.task(t.uid)) else {
            continue;
        };
        if dw != 0 {
            t.work_min = was.work_min.map(|w| w + dw);
        }
        if drw != 0 {
            t.remaining_work_min = was.remaining_work_min.map(|w| w + drw);
        }
        if dc != 0.0 {
            t.cost = was.cost.as_ref().and_then(|c| money(value(Some(c)) + dc));
        }
        if drc != 0.0 {
            t.remaining_cost = was
                .remaining_cost
                .as_ref()
                .and_then(|c| money(value(Some(c)) + drc));
        }
    }

    // Resources: totals over their assignments, when those changed.
    let Project {
        resources,
        assignments,
        ..
    } = proj;
    for r in resources {
        let uids = |assignments: &[Assignment]| {
            let mut uids: Vec<i32> = assignments
                .iter()
                .filter(|a| a.resource_uid == r.uid)
                .map(|a| a.uid)
                .collect();
            uids.sort_unstable();
            uids
        };
        let now = uids(assignments);
        if now == uids(&prev.assignments) && !now.iter().any(|uid| refreshed.contains(uid)) {
            continue;
        }
        let mine: Vec<&Assignment> = assignments
            .iter()
            .filter(|a| a.resource_uid == r.uid)
            .collect();
        let (work, cost, remaining, remaining_c) = totals(mine.iter().copied());
        let overtime: i64 = mine
            .iter()
            .map(|a| a.overtime_work_min.unwrap_or(0).max(0))
            .sum();
        r.work_min = Some(work);
        r.overtime_work_min = Some(overtime);
        r.regular_work_min = Some((work - overtime).max(0));
        r.remaining_work_min = Some(remaining);
        // A total over a cost nobody knows would be invented.
        if mine.iter().all(|a| a.cost.is_some()) {
            r.cost = money(cost);
            r.remaining_cost = money(remaining_c);
        }
        r.start = mine.iter().filter_map(|a| a.start).min();
        r.finish = mine.iter().filter_map(|a| a.finish).max();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RateEntry;
    use crate::schedule::schedule;

    fn at(day: u32, hour: u32) -> DateTime {
        DateTime::from_ymd_hm(2026, 3, day, hour, 0)
    }

    /// One task of `days` working days from `start` on the Standard calendar.
    fn plan(start: DateTime, days: i64) -> Project {
        Project {
            start_date: Some(start),
            tasks: vec![Task {
                uid: 1,
                id: 1,
                name: "T".into(),
                duration_min: days * 480,
                ..Task::default()
            }],
            ..Project::default()
        }
    }

    fn resource(uid: i32, kind: ResourceType, standard: &str, per_use: &str) -> Resource {
        Resource {
            uid,
            id: uid,
            name: format!("R{uid}"),
            kind,
            max_units: 1.0,
            standard_rate: Rate::parse(standard),
            cost_per_use: Rate::parse(per_use),
            ..Resource::default()
        }
    }

    fn assignment(uid: i32, resource: i32, units: f64, hours: i64) -> Assignment {
        Assignment {
            uid,
            task_uid: 1,
            resource_uid: resource,
            units,
            work_min: hours * 60,
            ..Assignment::default()
        }
    }

    fn span(proj: &Project, a: &Assignment) -> (DateTime, DateTime) {
        assignment_span(proj, &schedule(proj), a).unwrap()
    }

    fn cost(proj: &Project, a: &Assignment) -> Option<String> {
        assignment_cost(proj, a, span(proj, a)).map(|r| r.as_str().to_string())
    }

    #[test]
    fn flat_assignments_on_one_task_finish_apart() {
        // Project 2024, paired corpus 19: 32h at 100% and at 50% from Monday.
        let mut proj = plan(at(2, 8), 8);
        proj.resources = vec![
            resource(1, ResourceType::Work, "0", "0"),
            resource(2, ResourceType::Work, "0", "0"),
        ];
        let alice = assignment(1, 1, 1.0, 32);
        let bob = assignment(2, 2, 0.5, 32);
        assert_eq!(span(&proj, &alice), (at(2, 8), at(5, 17)));
        assert_eq!(span(&proj, &bob), (at(2, 8), at(11, 17)));
        // Project's unassigned resource and a missing one are work too.
        assert_eq!(
            span(&proj, &assignment(3, -65535, 1.0, 8)),
            (at(2, 8), at(2, 17))
        );
    }

    #[test]
    fn a_delay_is_working_time_from_the_task_start() {
        // RES-CASE-058: a 3d delay on a Thursday task skips the weekend.
        let proj = plan(at(5, 8), 6);
        let bob = Assignment {
            delay: Some(3 * 480 * 10),
            ..assignment(1, -65535, 1.0, 24)
        };
        assert_eq!(span(&proj, &bob), (at(10, 8), at(12, 17)));
    }

    #[test]
    fn a_leveling_delay_counts_elapsed_or_working_time_by_its_format() {
        let proj = plan(at(2, 8), 5);
        // 12 hours: elapsed lands Monday 20:00, so the next working start;
        // working takes Monday's 8h and 4h of Tuesday, then lunch.
        let delayed = |format| Assignment {
            leveling_delay: Some(720 * 10),
            leveling_delay_format: Some(format),
            ..assignment(1, -65535, 1.0, 8)
        };
        assert_eq!(span(&proj, &delayed(6)).0, at(3, 8));
        assert_eq!(span(&proj, &delayed(5)).0, at(3, 13));
        // Delay first, then the leveling delay on top.
        let both = Assignment {
            delay: Some(480 * 10),
            ..delayed(5)
        };
        assert_eq!(span(&proj, &both).0, at(4, 13));
    }

    #[test]
    fn actuals_contours_and_non_work_resources_take_their_own_dates() {
        let mut proj = plan(at(2, 8), 5);
        proj.resources = vec![
            resource(1, ResourceType::Work, "0", "0"),
            resource(2, ResourceType::Material, "0", "0"),
        ];
        let started = Assignment {
            actual_start: Some(at(3, 9)),
            ..assignment(1, 1, 1.0, 8)
        };
        assert_eq!(span(&proj, &started), (at(3, 9), at(4, 9)));
        let done = Assignment {
            actual_finish: Some(at(3, 12)),
            ..started.clone()
        };
        assert_eq!(span(&proj, &done), (at(3, 9), at(3, 12)));
        // A contour's units are its peak, and a material's work is a
        // quantity: both last as long as the task.
        let bell = Assignment {
            work_contour: Some(3),
            ..assignment(1, 1, 1.0, 8)
        };
        assert_eq!(span(&proj, &bell), (at(2, 8), at(6, 17)));
        assert_eq!(
            span(&proj, &assignment(2, 2, 1.0, 3)),
            (at(2, 8), at(6, 17))
        );
        // A delay past the task's finish never ends before it starts.
        let late = Assignment {
            delay: Some(10 * 480 * 10),
            ..bell
        };
        assert_eq!(span(&proj, &late), (at(16, 8), at(16, 8)));
    }

    #[test]
    fn a_work_assignment_costs_its_hours_at_the_rate_plus_its_per_use_cost() {
        // Project 2024, paired corpus 22: $50/h and $100 per use, 40h.
        let mut proj = plan(at(2, 8), 5);
        proj.resources = vec![resource(1, ResourceType::Work, "50", "10000")];
        let a = assignment(1, 1, 1.0, 40);
        assert_eq!(cost(&proj, &a).as_deref(), Some("210000"));
        // Overtime at the overtime rate: 36h at $50 + 4h at $75.
        proj.resources[0].overtime_rate = Rate::parse("75");
        proj.resources[0].cost_per_use = None;
        let overtime = Assignment {
            overtime_work_min: Some(240),
            ..a
        };
        assert_eq!(cost(&proj, &overtime).as_deref(), Some("210000"));
    }

    fn row(table: u8, from: Option<DateTime>, standard: &str) -> RateEntry {
        RateEntry {
            rates_from: from,
            rates_to: None,
            rate_table: Some(table),
            standard_rate: Rate::parse(standard),
            overtime_rate: Rate::parse("0"),
            cost_per_use: Rate::parse("0"),
            ..RateEntry::default()
        }
    }

    #[test]
    fn a_rate_change_prices_the_work_on_each_side_of_its_date() {
        // CST-027: 40h at $150, then 8h at $165 from the change date.
        let mut proj = plan(at(2, 8), 6);
        let mut r = resource(1, ResourceType::Work, "150", "0");
        r.rates = vec![row(0, Some(at(9, 8)), "165"), row(0, None, "150")];
        proj.resources = vec![r];
        let a = assignment(1, 1, 1.0, 48);
        assert_eq!(cost(&proj, &a).as_deref(), Some("732000"));
    }

    #[test]
    fn only_the_first_row_without_a_start_date_counts() {
        // A malformed table with two open-start rows must not price the work
        // at the sum of both.
        let mut proj = plan(at(2, 8), 5);
        let mut r = resource(1, ResourceType::Work, "0", "0");
        r.rates = vec![row(0, None, "50"), row(0, None, "70")];
        proj.resources = vec![r];
        let a = assignment(1, 1, 1.0, 40);
        assert_eq!(cost(&proj, &a).as_deref(), Some("200000"));
        // A dated row still takes over from its date.
        proj.resources[0].rates.push(row(0, Some(at(5, 8)), "80"));
        // 24h at $50, then 16h at $80.
        assert_eq!(cost(&proj, &a).as_deref(), Some("248000"));
    }

    #[test]
    fn the_cost_rate_table_picks_its_rows() {
        let mut proj = plan(at(2, 8), 5);
        let mut r = resource(1, ResourceType::Work, "50", "0");
        r.rates = vec![row(0, None, "50"), row(2, None, "80")];
        proj.resources = vec![r];
        let a = assignment(1, 1, 1.0, 40);
        assert_eq!(cost(&proj, &a).as_deref(), Some("200000"));
        let table_c = Assignment {
            cost_rate_table: Some(2),
            ..a.clone()
        };
        assert_eq!(cost(&proj, &table_c).as_deref(), Some("320000"));
        // A table without rows prices at the resource's own rates.
        let table_e = Assignment {
            cost_rate_table: Some(4),
            ..a
        };
        assert_eq!(cost(&proj, &table_e).as_deref(), Some("200000"));
    }

    #[test]
    fn materials_cost_their_quantity_and_cost_resources_keep_theirs() {
        let mut proj = plan(at(2, 8), 5);
        proj.resources = vec![
            resource(1, ResourceType::Material, "5", "1000"),
            resource(2, ResourceType::Cost, "0", "0"),
        ];
        // 10 units at $5 plus $10 per use.
        assert_eq!(
            cost(&proj, &assignment(1, 1, 1.0, 10)).as_deref(),
            Some("6000")
        );
        assert_eq!(cost(&proj, &assignment(2, 2, 1.0, 0)), None);
        assert_eq!(cost(&proj, &assignment(3, -65535, 1.0, 8)), None);
    }

    #[test]
    fn costs_are_whole_hundredths() {
        let mut proj = plan(at(2, 8), 5);
        proj.resources = vec![resource(1, ResourceType::Work, "33.333333", "0")];
        // 1h at $33.333333 = 3333.3333 hundredths.
        assert_eq!(
            cost(&proj, &assignment(1, 1, 1.0, 1)).as_deref(),
            Some("3333")
        );
    }
}
