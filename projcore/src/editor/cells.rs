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
        let start = day_start(&self.proj, task, day);
        if task.manual_start == Some(start) && task.manual_finish.is_none() {
            return Ok(());
        }
        self.snapshot();
        let task = &mut self.proj.tasks[i];
        task.manual_start = Some(start);
        task.manual_finish = None;
        self.changed();
        Ok(())
    }

    /// Set a task's finish to a typed day. A manual task keeps its start and
    /// its duration becomes the working time up to the day's last working
    /// time; an auto task gets a Finish-No-Earlier-Than constraint.
    pub fn set_finish(&mut self, uid: i32, day: DateTime) -> Result<(), String> {
        let i = self.index(uid)?;
        let task = &self.proj.tasks[i];
        let finish = day_finish(&self.proj, task, day)?;
        if !task.manual {
            return self.set_constraint_typed(
                uid,
                ConstraintType::FinishNoEarlierThan,
                Some(finish),
            );
        }
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
        let mut wanted = Vec::new();
        for raw in names {
            // Imported names may contain significant whitespace. Match the raw
            // token against retained assignments before normalizing user input.
            let assigned_resources: Vec<_> = resources
                .iter()
                .filter(|r| {
                    self.proj
                        .assignments
                        .iter()
                        .any(|a| a.task_uid == uid && a.resource_uid == r.uid)
                })
                .collect();
            let mut retained: Vec<_> = assigned_resources
                .iter()
                .filter(|r| r.name == *raw)
                .map(|r| r.uid)
                .collect();
            if retained.is_empty() {
                retained = assigned_resources
                    .iter()
                    .filter(|r| r.name.eq_ignore_ascii_case(raw))
                    .map(|r| r.uid)
                    .collect();
            }
            match retained.as_slice() {
                [rid] => {
                    if !wanted.contains(rid) {
                        wanted.push(*rid);
                    }
                    continue;
                }
                [] => {}
                _ => return Err(format!("Resource name '{raw}' is ambiguous")),
            }
            let name = raw.trim();
            if name.is_empty() {
                continue;
            }
            let matches: Vec<_> = resources
                .iter()
                .filter(|r| r.name.eq_ignore_ascii_case(name))
                .map(|r| r.uid)
                .collect();
            let assigned: Vec<_> = matches
                .iter()
                .copied()
                .filter(|rid| {
                    self.proj
                        .assignments
                        .iter()
                        .any(|a| a.task_uid == uid && a.resource_uid == *rid)
                })
                .collect();
            let rid = match assigned.as_slice() {
                [rid] => *rid,
                [] if matches.len() <= 1 => find_or_stage_resource(&mut resources, name)?,
                _ => return Err(format!("Resource name '{name}' is ambiguous")),
            };
            if !wanted.contains(&rid) {
                wanted.push(rid);
            }
        }
        let old: std::collections::HashSet<_> = self
            .proj
            .assignments
            .iter()
            .filter(|a| a.task_uid == uid)
            .map(|a| a.resource_uid)
            .collect();
        if wanted.len() == old.len() && wanted.iter().all(|r| old.contains(r)) {
            return Ok(());
        }
        let mut assignments = self.proj.assignments.clone();
        assignments.retain(|a| a.task_uid != uid || wanted.contains(&a.resource_uid));
        for rid in wanted.into_iter().filter(|rid| !old.contains(rid)) {
            assignments.push(new_assignment(
                &mut next_aid,
                uid,
                rid,
                self.proj.tasks[i].duration_min,
            )?);
        }
        self.snapshot();
        self.proj.resources = resources;
        self.proj.assignments = assignments;
        self.changed();
        Ok(())
    }
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
