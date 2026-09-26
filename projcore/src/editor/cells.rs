//! Atomic operations and lossless text interchange for entry-table cells.
use super::*;
#[cfg(test)]
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
                // A percent lag is a share of its predecessor's duration,
                // including the one this edit gives it.
                let pred_duration = match self.proj.task(pred.uid) {
                    Some(p) if p.uid == uid => duration.unwrap_or(p.duration_min),
                    Some(p) => p.duration_min,
                    None => 0,
                };
                total = total
                    .checked_add(
                        pred.lag_minutes(pred_duration)
                            .and_then(i64::checked_abs)
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
        self.edit_row(i, |proj, _| {
            proj.tasks[i].constraint = constraint;
            proj.tasks[i].constraint_date = date;
        })
    }

    /// Set a task's start to a typed day. A manual task moves there, at the
    /// day's first working time, keeping its duration; an auto task gets a
    /// Start-No-Earlier-Than constraint on that day. A blank row is judged as
    /// the task the edit makes it (manual in a plan whose new tasks are).
    pub fn set_start(&mut self, uid: i32, day: DateTime) -> Result<(), String> {
        let i = self.index(uid)?;
        let task = &self.row_as_edited(i);
        if !task.manual {
            return self.set_start_at(uid, day);
        }
        self.validate_pinned_day(day)?;
        let start = day_start(&self.proj, task, day);
        self.set_start_at(uid, start)
    }

    /// Set a task's start to an exact instant, as [`Self::set_start`] does
    /// for a day: a manual task's pinned start moves there, keeping its
    /// duration; an auto task gets a Start-No-Earlier-Than constraint there,
    /// replacing any other constraint. A manual summary keeps the span it
    /// shows as its manual duration.
    pub fn set_start_at(&mut self, uid: i32, start: DateTime) -> Result<(), String> {
        let i = self.index(uid)?;
        let blank = self.proj.tasks[i].is_null;
        let task = &self.row_as_edited(i);
        if !task.manual {
            return self.set_constraint_typed(uid, ConstraintType::StartNoEarlierThan, Some(start));
        }
        self.validate_pinned_day(start)?;
        let span = if task.summary {
            self.disp_duration_min(uid).or(task.manual_duration_min)
        } else {
            task.manual_duration_min
        };
        if !blank
            && task.manual_start == Some(start)
            && task.manual_finish.is_none()
            && task.manual_duration_min == span
        {
            return Ok(());
        }
        let summary = task.summary;
        self.edit_row(i, |proj, _| {
            let task = &mut proj.tasks[i];
            task.manual_start = Some(start);
            task.manual_finish = None;
            if summary {
                task.manual_duration_min = span;
            }
        })?;
        self.stamp_pinned_dates(uid);
        Ok(())
    }

    /// Set a task's finish to a typed day. A manual task keeps its start and
    /// its duration becomes the working time up to the day's last working
    /// time; an auto task gets a Finish-No-Earlier-Than constraint. A blank
    /// row is judged as the task the edit makes it, as in [`Self::set_start`].
    pub fn set_finish(&mut self, uid: i32, day: DateTime) -> Result<(), String> {
        let i = self.index(uid)?;
        let blank = self.proj.tasks[i].is_null;
        let task = &self.row_as_edited(i);
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
        let start = match task.pinned_dates().or_else(|| task.manual_summary_dates()) {
            Some((start, _)) => start,
            None => self.disp_start(uid).ok_or("The task has no start")?,
        };
        if finish < start {
            return Err("Finish is before the task's start".into());
        }
        if task.summary {
            return self.set_manual_summary_span(i, start, finish);
        }
        let calendar = task_calendar(&self.proj, task);
        let duration = crate::schedule::working_minutes_on(&calendar, start, finish);
        if !blank && task.manual_start == Some(start) && task.manual_finish == Some(finish) {
            return Ok(());
        }
        self.validate_cell_horizon(uid, Some(duration), None)?;
        self.edit_row(i, |proj, was_blank| {
            // As in update_task, a blank row's default duration is not typed.
            let task = &mut proj.tasks[i];
            let old = task.duration_min;
            let changed = was_blank || duration != old;
            if changed {
                commit_estimate(task);
            }
            task.manual_start = Some(start);
            task.manual_finish = Some(finish);
            task.duration_min = duration;
            task.manual_duration_min = Some(duration);
            task.milestone = duration == 0;
            if changed {
                rescale_work(proj, i, old, duration);
            }
        })?;
        self.stamp_pinned_dates(uid);
        Ok(())
    }

    /// Pin manual summary `i` to `start`..`finish`; its manual duration is
    /// the working time between them on the summary calendar. Its stored
    /// duration, milestone flag and work belong to the rollup.
    fn set_manual_summary_span(
        &mut self,
        i: usize,
        start: DateTime,
        finish: DateTime,
    ) -> Result<(), String> {
        let task = &self.proj.tasks[i];
        let span = crate::schedule::summary_or_leaf_min(&self.proj, task, start, finish);
        if task.manual_start == Some(start)
            && task.manual_finish == Some(finish)
            && task.manual_duration_min == Some(span)
        {
            return Ok(());
        }
        let uid = task.uid;
        self.edit_row(i, |proj, _| {
            let task = &mut proj.tasks[i];
            task.manual_start = Some(start);
            task.manual_finish = Some(finish);
            task.manual_duration_min = Some(span);
        })?;
        self.stamp_pinned_dates(uid);
        Ok(())
    }

    /// A pinned date must lie within the scheduler's timeline around the
    /// project start, or its finish could not be derived from it.
    pub(super) fn validate_pinned_day(&self, day: DateTime) -> Result<(), String> {
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
        let current = &self.proj.tasks[i].predecessors;
        for p in &predecessors {
            self.index(p.uid)?;
            // A link to a blank row the task already has is kept (it shows in
            // the cell); only a new one is refused.
            if self.is_blank(p.uid) && !current.iter().any(|c| c.uid == p.uid) {
                let id = self.proj.task(p.uid).map_or(p.uid, |t| t.id);
                return Err(format!("No task with ID {id}"));
            }
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
        self.edit_row(i, |proj, _| {
            proj.tasks[i].predecessors = predecessors;
        })
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
        // A blank row is assigned as the task the edit makes it.
        let duration = self.row_as_edited(i).duration_min;
        let kind = |rid: i32| resources.iter().find(|r| r.uid == rid).map(|r| r.kind);
        let mut changed = false;
        let mut assignments = self.proj.assignments.clone();
        assignments.retain(|a| {
            let keep = a.task_uid != uid || wanted.iter().any(|w| w.0 == a.resource_uid);
            changed |= !keep;
            keep
        });
        // A token speaks for the first assignment of its resource; imported
        // duplicates on the same task are left as they are.
        let mut seen = std::collections::HashSet::new();
        for a in assignments
            .iter_mut()
            .filter(|a| a.task_uid == uid && seen.insert(a.resource_uid))
        {
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
                a.set_units(u, work_for(duration, u));
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
        self.edit_row(i, |proj, _| {
            proj.resources = resources;
            proj.assignments = assignments;
        })?;
        Ok(())
    }

    /// Resolve a whole token to an existing resource. The task's assignments
    /// match by name, or by their shown cell text (`Bob[50%]`, which carries the
    /// assignment's current units), on one exactness ladder: raw, then trimmed,
    /// then case-insensitive. The first tier with a hit decides; hits on two
    /// resources are ambiguous. Otherwise a trimmed match among all resources.
    fn match_resource(
        &self,
        uid: i32,
        resources: &[Resource],
        raw: &str,
    ) -> Result<Option<(i32, Option<f64>)>, String> {
        let name = raw.trim();
        let ambiguous = || Err(format!("Resource name '{name}' is ambiguous"));
        let on_task: Vec<_> = self
            .proj
            .assignments
            .iter()
            .filter(|a| a.task_uid == uid)
            .filter_map(|a| Some((resources.iter().find(|r| r.uid == a.resource_uid)?, a)))
            .collect();
        // Imported names may contain significant whitespace, so the raw token
        // is tried before it is normalized.
        for tier in 0..3 {
            if tier > 0 && name.is_empty() {
                return Ok(None);
            }
            let mut hits: Vec<(i32, Option<f64>)> = Vec::new();
            for &(r, a) in &on_task {
                let shown = cell_text(r, a);
                let (by_name, by_shown) = match tier {
                    0 => (r.name == raw, shown == raw),
                    1 => (r.name == name, shown.trim() == name),
                    _ => (
                        r.name.eq_ignore_ascii_case(raw) || r.name.eq_ignore_ascii_case(name),
                        shown.trim().eq_ignore_ascii_case(name),
                    ),
                };
                if !(by_name || by_shown) {
                    continue;
                }
                // A name hit keeps no units; shown text carries the current ones.
                let units = (!by_name).then_some(a.units);
                match hits.iter_mut().find(|h| h.0 == r.uid) {
                    Some(hit) => hit.1 = hit.1.and(units),
                    None => hits.push((r.uid, units)),
                }
            }
            match hits.as_slice() {
                [] => {}
                [hit] => return Ok(Some(*hit)),
                _ => return ambiguous(),
            }
        }
        let matches: Vec<_> = resources
            .iter()
            .filter(|r| r.name.eq_ignore_ascii_case(name))
            .map(|r| r.uid)
            .collect();
        match matches.as_slice() {
            [] => Ok(None),
            [rid] => Ok(Some((*rid, None))),
            _ => ambiguous(),
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

/// Calendar minutes in one elapsed unit; `None` for a percentage.
fn elapsed_unit_min(unit: LagUnit) -> Option<f64> {
    Some(match unit {
        LagUnit::Minute => 1.0,
        LagUnit::Hour => 60.0,
        LagUnit::Day => 1440.0,
        LagUnit::Week => 7.0 * 1440.0,
        // Project's elapsed month is 30 elapsed days.
        LagUnit::Month => 30.0 * 1440.0,
        LagUnit::Percent => return None,
    })
}

/// Working minutes in one unit, as `parse_duration` counts them. A working
/// month needs the project's days per month, which the model lacks.
fn working_unit_min(unit: LagUnit, proj: &Project) -> Option<f64> {
    match unit {
        LagUnit::Minute => Some(1.0),
        LagUnit::Hour => Some(60.0),
        LagUnit::Day => Some(proj.hours_per_day * 60.0),
        LagUnit::Week => Some(proj.hours_per_week * 60.0),
        LagUnit::Month | LagUnit::Percent => None,
    }
}

fn unit_suffix(unit: LagUnit) -> &'static str {
    match unit {
        LagUnit::Minute => "m",
        LagUnit::Hour => "h",
        LagUnit::Day => "d",
        LagUnit::Week => "w",
        LagUnit::Month => "mo",
        LagUnit::Percent => "%",
    }
}

/// Parse a signed lag as the Predecessors cell spells it: a number and a unit
/// (`m`, `h`, `d`, `w` working; `em`, `eh`, `ed`, `ew`, `emo` elapsed; `%` of
/// the predecessor's duration), optionally marked estimated with `?`. The
/// unit sets the lag's format. Working months are refused: the model has no
/// days per month to count them with.
pub fn parse_lag(text: &str, proj: &Project) -> Option<(i64, LagFormat)> {
    let t = text.trim().to_ascii_lowercase();
    let (t, estimated) = match t.strip_suffix('?') {
        Some(rest) => (rest.trim_end(), true),
        None => (t.as_str(), false),
    };
    // Longer suffixes first: `emo` before `mo`/`m`, `em` before `m`.
    let (num, unit, elapsed) = [
        ("emo", LagUnit::Month, true),
        ("em", LagUnit::Minute, true),
        ("eh", LagUnit::Hour, true),
        ("ed", LagUnit::Day, true),
        ("ew", LagUnit::Week, true),
        ("%", LagUnit::Percent, false),
        ("m", LagUnit::Minute, false),
        ("h", LagUnit::Hour, false),
        ("d", LagUnit::Day, false),
        ("w", LagUnit::Week, false),
    ]
    .into_iter()
    .find_map(|(suffix, unit, elapsed)| {
        t.strip_suffix(suffix).map(|n| (n.trim(), unit, elapsed))
    })?;
    let format = LagFormat::new(unit, elapsed, estimated)?;
    let value = if unit == LagUnit::Percent {
        num.parse::<i64>().ok()?
    } else if !elapsed {
        parse_duration(&format!("{num}{}", unit_suffix(unit)), proj)?
    } else if let Ok(exact) = num.parse::<i64>() {
        exact.checked_mul(elapsed_unit_min(unit)? as i64)?
    } else {
        let minutes = (num.parse::<f64>().ok()? * elapsed_unit_min(unit)?).round();
        (minutes.is_finite() && minutes > i64::MIN as f64 && minutes < i64::MAX as f64)
            .then_some(minutes as i64)?
    };
    Some((value, format))
}

/// A predecessor's lag as the cell shows it, signed, in its own unit when
/// [`parse_lag`] reads that text back to the same lag and format. Otherwise
/// it falls back to minutes of its kind (a working month shows in days), and
/// re-entering the text changes only the format, never the time.
pub fn format_lag(p: &Predecessor, proj: &Project) -> String {
    let format = p.lag_format;
    let sign = if p.lag >= 0 { "+" } else { "" };
    let mark = if format.estimated() { "?" } else { "" };
    let elapsed = format.kind() == LagKind::Elapsed;
    let text = |value: String, unit: LagUnit| {
        let e = if elapsed { "e" } else { "" };
        format!("{sign}{value}{e}{}{mark}", unit_suffix(unit))
    };
    if format.kind() == LagKind::Percent {
        return text(p.lag.to_string(), LagUnit::Percent);
    }
    let unit_min = |unit| {
        if elapsed {
            elapsed_unit_min(unit)
        } else {
            working_unit_min(unit, proj)
        }
    };
    let unit = match format.unit() {
        LagUnit::Month if !elapsed => LagUnit::Day,
        unit => unit,
    };
    if let Some(per) = unit_min(unit).filter(|per| per.is_finite() && *per > 0.) {
        let value = p.lag as f64 / per;
        // At most two decimals, as Project shows a lag.
        let shown = format!("{:.2}", value)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string();
        let candidate = text(shown, unit);
        if parse_lag(&candidate, proj) == Some((p.lag, format)) {
            return candidate;
        }
        // A working month shows in days: the same time, in format days.
        if unit != format.unit() && parse_lag(&candidate, proj).map(|(v, _)| v) == Some(p.lag) {
            return candidate;
        }
    }
    text(p.lag.to_string(), LagUnit::Minute)
}

/// One link as the Predecessors cell shows it.
fn format_link(p: &Predecessor, proj: &Project) -> String {
    let id = proj
        .task(p.uid)
        .map(|t| t.id.to_string())
        .unwrap_or_else(|| format!("?{}", p.uid));
    // A zero lag in days is the default and shows nothing; any other
    // format shows, so re-entering the cell keeps it.
    let plain = p.lag == 0 && p.lag_format == LagFormat::DAYS;
    let kind = match p.link {
        LinkType::FinishStart if plain => "",
        LinkType::FinishStart => "FS",
        LinkType::StartStart => "SS",
        LinkType::FinishFinish => "FF",
        LinkType::StartFinish => "SF",
    };
    let lag = if plain {
        String::new()
    } else {
        format_lag(p, proj)
    };
    format!("{id}{kind}{lag}")
}

pub fn format_predecessors(task: &Task, proj: &Project) -> String {
    task.predecessors
        .iter()
        .map(|p| format_link(p, proj))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse Predecessors cell text on its own, for tests. Re-entering a task's
/// cell uses [`parse_task_predecessors`], which keeps fallback formats.
#[cfg(test)]
pub(crate) fn parse_predecessors(text: &str, proj: &Project) -> Result<Vec<Predecessor>, String> {
    parse_predecessors_keeping(text, proj, &[])
}

/// Parse text typed into `task`'s Predecessors cell (`2FS+2ed, 3SS`). An
/// entry spelled exactly as the cell shows one of the task's links keeps
/// that link as it is. A lag shown in a fallback unit (a working month in
/// days, a fraction of a day in minutes) keeps its format unless it is
/// edited.
pub fn parse_task_predecessors(
    text: &str,
    task: &Task,
    proj: &Project,
) -> Result<Vec<Predecessor>, String> {
    parse_predecessors_keeping(text, proj, &task.predecessors)
}

fn parse_predecessors_keeping(
    text: &str,
    proj: &Project,
    existing: &[Predecessor],
) -> Result<Vec<Predecessor>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in text.split(',') {
        let entry = entry.trim().to_ascii_uppercase();
        let shown = existing
            .iter()
            .find(|p| format_link(p, proj).to_ascii_uppercase() == entry);
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
        let (lag, lag_format) = if rest.is_empty() {
            (0, LagFormat::DAYS)
        } else {
            if !rest.starts_with(['+', '-']) {
                return Err("Expected FS/SS/FF/SF and signed lag (e.g. +2h, +2ed, +50%)".into());
            }
            parse_lag(rest, proj).ok_or("Invalid predecessor lag")?
        };
        let parsed = Predecessor {
            uid,
            link,
            lag,
            lag_format,
        };
        // The shown text parses to the same task, link and lag; only a
        // fallback display can differ in format.
        out.push(match shown {
            Some(p) if (p.uid, p.link, p.lag) == (uid, link, lag) => *p,
            _ => parsed,
        });
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

/// A task's working time by date, exceptions included, with exactly the
/// scheduler's fallback and base-chain resolution.
pub(super) use crate::assign::task_calendar;

/// Start of a typed date: its first working time, or 08:00 (Project's default
/// start time) on a non-working day or holiday, where a manual task may still
/// start.
fn day_start(proj: &Project, task: &Task, date: DateTime) -> DateTime {
    let from = task_calendar(proj, task)
        .day(date.day_number())
        .first()
        .map_or(8 * 60, |s| s.from);
    date.start_of_day().add_minutes(i64::from(from))
}

/// Finish of a typed date: its last working time, or 17:00 on a non-working
/// day or holiday, where a manual task may still finish.
fn day_end(proj: &Project, task: &Task, date: DateTime) -> DateTime {
    let to = task_calendar(proj, task)
        .day(date.day_number())
        .last()
        .map_or(17 * 60, |s| s.to);
    date.start_of_day().add_minutes(i64::from(to))
}

/// Finish boundary for a typed date, with exactly the scheduler's calendar fallback.
pub fn day_finish(proj: &Project, task: &Task, date: DateTime) -> Result<DateTime, String> {
    let end = task_calendar(proj, task)
        .day(date.day_number())
        .last()
        .map(|s| s.to)
        .ok_or("Finish date is a non-working day")?;
    Ok(date.start_of_day().add_minutes(i64::from(end)))
}

#[cfg(test)]
mod tests;
