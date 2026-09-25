//! Atomic operations and lossless text interchange for entry-table cells.
use super::*;
use crate::model::Calendar;
use crate::schedule::{HORIZON_DAYS, HORIZON_PADDING_MIN};

impl Editor {
    /// Preflight the scheduler's aggregate working-minute horizon before snapshotting.
    pub(super) fn validate_cell_horizon(
        &self,
        uid: i32,
        duration: Option<i64>,
        predecessors: Option<&[Predecessor]>,
    ) -> Result<(), String> {
        // Timelines include up to 100 years before the anchor and 100 years
        // after it, plus the final guard day. Reserve the full index as well as
        // padding so index + duration/lag stays representable for dated starts.
        let mut total = (2 * HORIZON_DAYS + 1) * 1440 + HORIZON_PADDING_MIN;
        for task in &self.proj.tasks {
            let minutes = if task.uid == uid {
                duration.unwrap_or(task.duration_min)
            } else {
                task.duration_min
            };
            total = total
                .checked_add(minutes.max(0))
                .ok_or("Duration exceeds scheduling range")?;
            let preds = if task.uid == uid {
                predecessors.unwrap_or(&task.predecessors)
            } else {
                &task.predecessors
            };
            for pred in preds {
                total = total
                    .checked_add(
                        pred.lag_min
                            .checked_abs()
                            .ok_or("Lag exceeds scheduling range")?,
                    )
                    .ok_or("Lag exceeds scheduling range")?;
            }
        }
        Ok(())
    }

    pub fn set_constraint_typed(
        &mut self,
        uid: i32,
        constraint: ConstraintType,
        date: Option<DateTime>,
    ) -> Result<(), String> {
        let i = self.index(uid)?;
        let needs_date = !matches!(
            constraint,
            ConstraintType::AsSoonAsPossible | ConstraintType::AsLateAsPossible
        );
        if needs_date != date.is_some() {
            return Err("Dated constraints require a date; ASAP/ALAP do not".into());
        }
        let task = &self.proj.tasks[i];
        if task.constraint == constraint && task.constraint_date == date {
            return Ok(());
        }
        self.snapshot();
        self.proj.tasks[i].constraint = constraint;
        self.proj.tasks[i].constraint_date = date;
        self.changed();
        Ok(())
    }

    /// Set a task's start to a typed day. A manual task moves there, at the
    /// day's first working time, keeping its duration; an auto task gets a
    /// Start-No-Earlier-Than constraint on that day.
    pub fn set_start(&mut self, uid: i32, day: DateTime) -> Result<(), String> {
        let i = self.index(uid)?;
        let task = &self.proj.tasks[i];
        if !task.manual {
            return self.set_constraint_typed(uid, ConstraintType::StartNoEarlierThan, Some(day));
        }
        self.validate_pinned_day(day)?;
        let start = day_start(&self.proj, task, day);
        if task.manual_start == Some(start) && task.manual_finish.is_none() {
            return Ok(());
        }
        self.snapshot();
        let task = &mut self.proj.tasks[i];
        task.manual_start = Some(start);
        task.manual_finish = None;
        self.changed();
        self.stamp_pinned_dates(uid);
        Ok(())
    }

    /// Set a task's finish to a typed day. A manual task keeps its start and
    /// its duration becomes the working time up to the day's last working
    /// time; an auto task gets a Finish-No-Earlier-Than constraint.
    pub fn set_finish(&mut self, uid: i32, day: DateTime) -> Result<(), String> {
        let i = self.index(uid)?;
        let task = &self.proj.tasks[i];
        if !task.manual {
            let finish = day_finish(&self.proj, task, day)?;
            return self.set_constraint_typed(
                uid,
                ConstraintType::FinishNoEarlierThan,
                Some(finish),
            );
        }
        self.validate_pinned_day(day)?;
        // Like a typed start, a manual finish may fall on a non-working day.
        let finish = day_end(&self.proj, task, day);
        let start = match task.pinned_dates() {
            Some((start, _)) => start,
            None => self.disp_start(uid).ok_or("The task has no start")?,
        };
        if finish < start {
            return Err("Finish is before the task's start".into());
        }
        let calendar = task_calendar(&self.proj, task);
        let duration = crate::schedule::working_minutes_on(&calendar, start, finish);
        if task.manual_start == Some(start) && task.manual_finish == Some(finish) {
            return Ok(());
        }
        self.validate_cell_horizon(uid, Some(duration), None)?;
        self.snapshot();
        let task = &mut self.proj.tasks[i];
        task.manual_start = Some(start);
        task.manual_finish = Some(finish);
        task.duration_min = duration;
        task.manual_duration_min = Some(duration);
        task.milestone = duration == 0;
        self.changed();
        self.stamp_pinned_dates(uid);
        Ok(())
    }

    /// A pinned date must lie within the scheduler's timeline around the
    /// project start, or its finish could not be derived from it.
    fn validate_pinned_day(&self, day: DateTime) -> Result<(), String> {
        let offset = day.minutes() - self.sched.project_start.minutes();
        if offset.abs() > HORIZON_DAYS * 1440 {
            return Err(
                "Date is outside the scheduling range (100 years around the project start)".into(),
            );
        }
        Ok(())
    }

    pub fn set_predecessors(
        &mut self,
        uid: i32,
        predecessors: Vec<Predecessor>,
    ) -> Result<(), String> {
        let i = self.index(uid)?;
        let mut seen = std::collections::HashSet::new();
        for p in &predecessors {
            self.index(p.uid)?;
            if p.uid == uid {
                return Err("A task cannot depend on itself".into());
            }
            if !seen.insert(p.uid) {
                return Err("Duplicate predecessor".into());
            }
        }
        if self.proj.tasks[i].predecessors == predecessors {
            return Ok(());
        }
        self.validate_cell_horizon(uid, None, Some(&predecessors))?;
        self.snapshot();
        self.proj.tasks[i].predecessors = predecessors;
        self.changed();
        Ok(())
    }

    /// Replace membership while preserving allocation data for retained resources.
    /// Prefer an already assigned namesake; otherwise duplicate names are ambiguous.
    ///
    /// Tokens are Resource Names cell text: `Name[NN%]` sets explicit units. The
    /// cell is WYSIWYG, so a retained assignment changes only when its text does:
    /// different bracketed units, or a bare work resource that was shown bracketed.
    pub fn set_resources(&mut self, uid: i32, names: &[String]) -> Result<(), String> {
        let i = self.index(uid)?;
        // Stage every allocation before touching the project or history, including ID exhaustion.
        let mut resources = self.proj.resources.clone();
        let mut next_aid = self
            .proj
            .assignments
            .iter()
            .map(|a| a.uid)
            .max()
            .unwrap_or(0);
        let mut wanted: Vec<(i32, Option<f64>, &str)> = Vec::new();
        for raw in names {
            // A whole-token name match wins, so names like `Crew [A%]` are not split.
            let (rid, units) = match self.match_resource(uid, &resources, raw)? {
                Some(found) => found,
                None => {
                    let (name, units) = parse_resource_token(raw)?;
                    let matched = match units {
                        Some(_) => self.match_resource(uid, &resources, name)?.map(|m| m.0),
                        None => None,
                    };
                    let rid = match matched {
                        Some(rid) => rid,
                        None if name.trim().is_empty() => continue,
                        None => find_or_stage_resource(&mut resources, name.trim())?,
                    };
                    (rid, units)
                }
            };
            if !wanted.iter().any(|w| w.0 == rid) {
                wanted.push((rid, units, raw));
            }
        }
        let duration = self.proj.tasks[i].duration_min;
        let kind = |rid: i32| resources.iter().find(|r| r.uid == rid).map(|r| r.kind);
        let mut changed = false;
        let mut assignments = self.proj.assignments.clone();
        assignments.retain(|a| {
            let keep = a.task_uid != uid || wanted.iter().any(|w| w.0 == a.resource_uid);
            changed |= !keep;
            keep
        });
        for a in assignments.iter_mut().filter(|a| a.task_uid == uid) {
            let &(_, explicit, raw) = wanted
                .iter()
                .find(|w| w.0 == a.resource_uid)
                .expect("retained");
            let units = match explicit {
                Some(u) if format_units(u) == format_units(a.units) => None,
                Some(u) => Some(checked_units(u, raw)?),
                // The cell showed `Name[NN%]` and the user deleted the bracket.
                None if kind(a.resource_uid) == Some(ResourceType::Work)
                    && units_bracket(a.units).is_some() =>
                {
                    Some(1.0)
                }
                None => None,
            };
            if let Some(u) = units {
                a.units = u;
                a.work_min = work_for(duration, u);
                changed = true;
            }
        }
        let old: std::collections::HashSet<_> = self
            .proj
            .assignments
            .iter()
            .filter(|a| a.task_uid == uid)
            .map(|a| a.resource_uid)
            .collect();
        for &(rid, explicit, raw) in wanted.iter().filter(|w| !old.contains(&w.0)) {
            let units = match explicit {
                Some(u) => checked_units(u, raw)?,
                None => default_units(resources.iter().find(|r| r.uid == rid).expect("staged")),
            };
            assignments.push(new_assignment(&mut next_aid, uid, rid, units, duration)?);
            changed = true;
        }
        if !changed {
            return Ok(());
        }
        self.snapshot();
        self.proj.resources = resources;
        self.proj.assignments = assignments;
        self.changed();
        Ok(())
    }

    /// Resolve a whole token to an existing resource: a raw, then case-insensitive,
    /// match among the task's assigned resources, then their shown cell text
    /// (which carries their current units), then a trimmed match among all.
    fn match_resource(
        &self,
        uid: i32,
        resources: &[Resource],
        raw: &str,
    ) -> Result<Option<(i32, Option<f64>)>, String> {
        let is_assigned = |rid: i32| {
            self.proj
                .assignments
                .iter()
                .any(|a| a.task_uid == uid && a.resource_uid == rid)
        };
        // Imported names may contain significant whitespace. Match the raw
        // token against retained assignments before normalizing user input.
        let assigned_resources: Vec<_> = resources.iter().filter(|r| is_assigned(r.uid)).collect();
        let mut retained: Vec<_> = assigned_resources
            .iter()
            .filter(|r| r.name == raw)
            .map(|r| r.uid)
            .collect();
        if retained.is_empty() {
            retained = assigned_resources
                .iter()
                .filter(|r| r.name.eq_ignore_ascii_case(raw))
                .map(|r| r.uid)
                .collect();
        }
        let name = raw.trim();
        // `Bob[50%]` is also the shown text of an assigned Bob. An unassigned
        // resource literally named so never takes it; an assigned one is ambiguous.
        let shown_matching = |exact: bool| -> Vec<_> {
            self.proj
                .assignments
                .iter()
                .filter(|a| a.task_uid == uid && !name.is_empty())
                .filter_map(|a| {
                    let r = resources.iter().find(|r| r.uid == a.resource_uid)?;
                    let text = cell_text(r, a);
                    let text = text.trim();
                    (if exact {
                        text == name
                    } else {
                        text.eq_ignore_ascii_case(name)
                    })
                    .then_some((a.resource_uid, Some(a.units)))
                })
                .collect()
        };
        // An exact spelling beats a case-insensitive one, as for names above.
        let mut shown = shown_matching(true);
        if shown.is_empty() {
            shown = shown_matching(false);
        }
        match (retained.as_slice(), shown.as_slice()) {
            ([rid], shown) if shown.iter().all(|s| s.0 == *rid) => return Ok(Some((*rid, None))),
            ([], [found]) => return Ok(Some(*found)),
            ([], []) => {}
            _ => return Err(format!("Resource name '{name}' is ambiguous")),
        }
        if name.is_empty() {
            return Ok(None);
        }
        let matches: Vec<_> = resources
            .iter()
            .filter(|r| r.name.eq_ignore_ascii_case(name))
            .map(|r| r.uid)
            .collect();
        let assigned: Vec<_> = matches
            .iter()
            .copied()
            .filter(|&rid| is_assigned(rid))
            .collect();
        match (assigned.as_slice(), matches.as_slice()) {
            ([rid], _) | ([], [rid]) => Ok(Some((*rid, None))),
            ([], []) => Ok(None),
            _ => Err(format!("Resource name '{name}' is ambiguous")),
        }
    }
}

/// Split a Resource Names token `Name[NN%]` into its name and units (`NN/100`).
/// A token without a trailing bracket is all name. The name keeps its raw
/// spelling so it resolves like a bare token.
pub(super) fn parse_resource_token(raw: &str) -> Result<(&str, Option<f64>), String> {
    let Some((name, inner)) = raw
        .trim_end()
        .strip_suffix(']')
        .and_then(|body| body.rsplit_once('['))
    else {
        return Ok((raw, None));
    };
    let percent = inner
        .trim()
        .strip_suffix('%')
        .and_then(|n| n.trim_end().parse::<f64>().ok())
        .filter(|n| n.is_finite());
    match percent {
        Some(n) if !name.trim().is_empty() => Ok((name, Some(n / 100.))),
        _ => Err(format!("Invalid units in '{}'", raw.trim())),
    }
}

/// Assignment units as percent text: two decimals, trailing zeros trimmed (`50%`, `33.33%`).
pub(super) fn format_units(units: f64) -> String {
    let text = format!("{:.2}", units * 100.);
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{}%", if text == "-0" { "0" } else { text })
}

/// The bracket a work assignment shows after its name, or `None` at 100%.
fn units_bracket(units: f64) -> Option<String> {
    (units.is_finite() && (units - 1.).abs() > 1e-9).then(|| format!("[{}]", format_units(units)))
}

/// The task's Resource Names cell, in assignment order: `Bob[50%], Alice`.
/// Only work resources show their units, as in Project.
pub fn format_resource_names(proj: &Project, task_uid: i32) -> String {
    proj.assignments
        .iter()
        .filter(|a| a.task_uid == task_uid)
        .filter_map(|a| {
            let r = proj.resources.iter().find(|r| r.uid == a.resource_uid)?;
            Some(cell_text(r, a))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One assignment's Resource Names text; only work resources show their units.
fn cell_text(r: &Resource, a: &Assignment) -> String {
    let units = match r.kind {
        ResourceType::Work => units_bracket(a.units),
        _ => None,
    };
    format!("{}{}", r.name, units.unwrap_or_default())
}

/// Prefer whole days/hours, falling back to exact minutes (including signed lag).
pub fn format_duration_exact(min: i64, proj: &Project) -> String {
    let day = proj.hours_per_day * 60.;
    if day.is_finite() && day > 0. {
        let days = min as f64 / day;
        let text = format!("{days:.0}d");
        if days.fract() == 0. && parse_duration(&text, proj) == Some(min) {
            return text;
        }
    }
    if min % 60 == 0 {
        let hours = format!("{}h", min / 60);
        if parse_duration(&hours, proj) == Some(min) {
            return hours;
        }
    }
    format!("{min}m")
}

pub fn format_predecessors(task: &Task, proj: &Project) -> String {
    task.predecessors
        .iter()
        .map(|p| {
            let id = proj
                .task(p.uid)
                .map(|t| t.id.to_string())
                .unwrap_or_else(|| format!("?{}", p.uid));
            let kind = match p.link {
                LinkType::FinishStart if p.lag_min == 0 => "",
                LinkType::FinishStart => "FS",
                LinkType::StartStart => "SS",
                LinkType::FinishFinish => "FF",
                LinkType::StartFinish => "SF",
            };
            let lag = if p.lag_min == 0 {
                String::new()
            } else {
                format!(
                    "{}{}",
                    if p.lag_min > 0 { "+" } else { "" },
                    format_duration_exact(p.lag_min, proj)
                )
            };
            format!("{id}{kind}{lag}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn parse_predecessors(text: &str, proj: &Project) -> Result<Vec<Predecessor>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in text.split(',') {
        let entry = entry.trim().to_ascii_uppercase();
        let end = entry.bytes().take_while(u8::is_ascii_digit).count();
        let id: i32 = entry[..end]
            .parse()
            .map_err(|_| "Expected predecessor task ID")?;
        let mut matches = proj.tasks.iter().filter(|t| t.id == id);
        let uid = matches
            .next()
            .ok_or_else(|| format!("No task with ID {id}"))?
            .uid;
        if matches.next().is_some() {
            return Err(format!("Ambiguous task ID {id}"));
        }
        if out.iter().any(|p: &Predecessor| p.uid == uid) {
            return Err("Duplicate predecessor".into());
        }
        let mut rest = &entry[end..];
        let mut link = LinkType::FinishStart;
        for (code, kind) in [
            ("FS", LinkType::FinishStart),
            ("SS", LinkType::StartStart),
            ("FF", LinkType::FinishFinish),
            ("SF", LinkType::StartFinish),
        ] {
            if let Some(tail) = rest.strip_prefix(code) {
                link = kind;
                rest = tail;
                break;
            }
        }
        let lag_min = if rest.is_empty() {
            0
        } else {
            if !rest.starts_with(['+', '-']) || !rest.ends_with(['D', 'H', 'M', 'W']) {
                return Err("Expected FS/SS/FF/SF and signed lag (e.g. +2h)".into());
            }
            parse_duration(rest, proj).ok_or("Invalid predecessor lag")?
        };
        out.push(Predecessor { uid, link, lag_min });
    }
    Ok(out)
}

/// Strict civil date input, independent of the permissive import parser.
pub fn parse_cell_date(text: &str) -> Result<DateTime, String> {
    let s = text.trim();
    let b = s.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || b.iter()
            .enumerate()
            .any(|(i, c)| i != 4 && i != 7 && !c.is_ascii_digit())
    {
        return Err("Expected a valid date: YYYY-MM-DD".into());
    }
    let d = DateTime::parse_mspdi(s).ok_or("Expected a valid date: YYYY-MM-DD")?;
    let p = d.parts();
    if format!("{:04}-{:02}-{:02}", p.year, p.month, p.day) != s {
        return Err("Invalid calendar date".into());
    }
    Ok(d)
}

/// A task's calendar, with exactly the scheduler's fallback.
fn task_calendar(proj: &Project, task: &Task) -> Calendar {
    proj.calendars
        .iter()
        .find(|c| c.uid == task.calendar_uid.unwrap_or(proj.default_calendar_uid))
        .or_else(|| {
            proj.calendars
                .iter()
                .find(|c| c.uid == proj.default_calendar_uid)
        })
        .cloned()
        .unwrap_or_else(|| Calendar::standard(proj.default_calendar_uid))
}

/// Start of a typed date: its first working time, or 08:00 (Project's default
/// start time) on a non-working day, where a manual task may still start.
fn day_start(proj: &Project, task: &Task, date: DateTime) -> DateTime {
    let from = task_calendar(proj, task).week[date.weekday() as usize]
        .times
        .iter()
        .filter(|s| s.to > s.from)
        .map(|s| s.from)
        .min()
        .unwrap_or(8 * 60);
    date.start_of_day().add_minutes(i64::from(from))
}

/// Finish of a typed date: its last working time, or 17:00 on a non-working
/// day, where a manual task may still finish.
fn day_end(proj: &Project, task: &Task, date: DateTime) -> DateTime {
    let to = task_calendar(proj, task).week[date.weekday() as usize]
        .times
        .iter()
        .filter(|s| s.to > s.from)
        .map(|s| s.to)
        .max()
        .unwrap_or(17 * 60);
    date.start_of_day().add_minutes(i64::from(to))
}

/// Finish boundary for a typed date, with exactly the scheduler's calendar fallback.
pub fn day_finish(proj: &Project, task: &Task, date: DateTime) -> Result<DateTime, String> {
    let calendar = task_calendar(proj, task);
    let end = calendar.week[date.weekday() as usize]
        .times
        .iter()
        .filter(|s| s.to > s.from)
        .map(|s| s.to)
        .max()
        .ok_or("Finish date is a non-working day")?;
    Ok(date.start_of_day().add_minutes(i64::from(end)))
}

#[cfg(test)]
mod tests;
