//! One assignment at a time, by its UID (#395): what an agent's `assign.add`,
//! `assign.set` and `assign.del` do. Each edit stages the resources and
//! assignments and commits them through [`Editor::commit_assignments`], so
//! the task type and effort-driven rules, the horizon check, the undo step
//! and the refresh of cost, dates and totals are the ones the Resource Names
//! cell uses. Arguments are checked before anything is staged. The checks
//! that need an earlier stage's result (the Fixed Duration span for new
//! work, the work new units give) run inside the edit's batch, which
//! restores the editor on a rejection. So a rejected edit leaves the editor
//! untouched; inside a caller's [`Editor::batch`], stages made before such a
//! rejection stay until that batch fails, as `batch` documents for its
//! closure.
use super::effort::work_edit_rescales_units;
use super::*;

/// The resource an added assignment is for.
#[derive(Clone, Copy, Debug)]
pub enum ResourceRef<'a> {
    /// An existing resource.
    Uid(i32),
    /// A resource by name, ignoring ASCII case; a new name is staged as a
    /// work resource. A new name that is a number or contains a comma is
    /// refused (pass a uid as a number).
    Name(&'a str),
}

/// What [`Editor::set_assignment`] changes; `None` keeps a value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssignmentPatch {
    /// A fraction (1.0 = 100%), or a material's quantity.
    pub units: Option<f64>,
    pub work_min: Option<i64>,
    /// The cost rate table: 0 = A .. 4 = E.
    pub rate_table: Option<u8>,
    /// The assignment's `Delay`, in working minutes.
    pub delay_min: Option<i64>,
    /// Overtime work, in minutes: part of the work, priced at the overtime
    /// rate. A work resource's only.
    pub overtime_work_min: Option<i64>,
    /// 0 = Flat .. 7 = Turtle; 8 (Contoured) is refused.
    pub work_contour: Option<u8>,
    /// Actual work, in minutes, never more than the work.
    pub actual_work_min: Option<i64>,
    /// 0..=100: the actual work as a share of the work. Exclusive with
    /// `actual_work_min`.
    pub percent_work_complete: Option<u8>,
}

impl AssignmentPatch {
    fn is_empty(&self) -> bool {
        *self == AssignmentPatch::default()
    }
}

/// Units an agent gives must be a positive, finite number.
fn positive_units(units: f64) -> Result<f64, String> {
    if units.is_finite() && units > 0.0 {
        Ok(units)
    } else {
        Err(format!("units must be a positive number, not {units}"))
    }
}

/// Work or a delay must be a non-negative number of minutes within the
/// scheduling horizon.
fn checked_minutes(what: &str, min: i64) -> Result<i64, String> {
    if min < 0 {
        Err(format!("{what} must not be negative"))
    } else if min > MAX_MINUTES {
        Err(format!("{what} is beyond the scheduling range"))
    } else {
        Ok(min)
    }
}

/// The work `units` give over `span` minutes ([`assigned_work`]), refused
/// when it would pass the scheduling horizon. The work saturates, so huge
/// units are refused too.
fn units_work(kind: Option<ResourceType>, span: i64, units: f64) -> Result<i64, String> {
    bounded_work(kind, span, units).ok_or_else(|| "units are beyond the scheduling range".into())
}

/// Refuse units or work a resource of this kind does not take: a cost
/// resource has neither, and a material's work is its quantity (its units).
fn check_kind(r: &Resource, units: bool, work: bool) -> Result<(), String> {
    match r.kind {
        ResourceType::Cost if units || work => Err(format!(
            "'{}' is a cost resource; it takes no units or work",
            r.name
        )),
        ResourceType::Material if work => Err(format!(
            "'{}' is a material resource; set its units",
            r.name
        )),
        _ => Ok(()),
    }
}

/// Refuse what a resource of this kind does not take: a cost resource has no
/// overtime, contour or actual work, and a material no overtime.
fn check_progress_kind(
    r: &Resource,
    overtime: bool,
    contour: bool,
    actuals: bool,
) -> Result<(), String> {
    let what = match r.kind {
        ResourceType::Cost if overtime => "overtime work",
        ResourceType::Cost if contour => "work contour",
        ResourceType::Cost if actuals => "actual work",
        ResourceType::Material if overtime => {
            return Err(format!(
                "'{}' is a material resource; it takes no overtime work",
                r.name
            ));
        }
        _ => return Ok(()),
    };
    Err(format!(
        "'{}' is a cost resource; it takes no {what}",
        r.name
    ))
}

/// The work contour an agent gives: Flat (0) to Turtle (7). Project sets
/// Contoured (8) itself, from edits to the timephased work.
fn checked_contour(contour: u8) -> Result<u8, String> {
    match contour {
        0..=7 => Ok(contour),
        8 => {
            Err("Contoured comes from editing timephased work; set Flat to Turtle (0 to 7)".into())
        }
        _ => Err(format!(
            "no work contour {contour}; use 0 (Flat) to 7 (Turtle)"
        )),
    }
}

/// A percent of no work is no actual work, so only 0 means what it says.
const NO_WORK_PERCENT: &str = "the assignment has no work; percent_work_complete can only be 0";

/// The actual work a patch asks for, in minutes, on `work_min` of work:
/// its actual work, or its percent of the work.
fn asked_actual(actual: Option<i64>, percent: Option<u8>, work_min: i64) -> Option<i64> {
    actual
        .or_else(|| percent.map(|p| (work_min.max(0) as f64 * f64::from(p) / 100.0).round() as i64))
}

impl Editor {
    /// Assign a resource to task `task_uid` and return the new assignment's
    /// UID, as one undo step. Without `units`, a work resource is assigned
    /// at its Max. Units (capped at 100%) and anything else at 1; its work is
    /// the task duration times the units (a material's is its quantity).
    /// With `work_min`, that work is then set as [`Self::set_assignment`]
    /// sets it, in the same undo step. A summary, a milestone and a blank row
    /// (which becomes a task) take an assignment as the Resource Names cell
    /// gives them one. A resource already on the task is an error.
    pub fn add_assignment(
        &mut self,
        task_uid: i32,
        resource: ResourceRef,
        units: Option<f64>,
        work_min: Option<i64>,
    ) -> Result<i32, String> {
        let units = units.map(positive_units).transpose()?;
        // Refuse units or work an existing resource does not take before
        // anything is staged; a new name is a work resource.
        let existing = match resource {
            ResourceRef::Uid(rid) => self.proj.resources.iter().find(|r| r.uid == rid),
            ResourceRef::Name(name) => self
                .proj
                .resources
                .iter()
                .find(|r| r.name.eq_ignore_ascii_case(name.trim())),
        };
        if let Some(r) = existing {
            check_kind(r, units.is_some(), work_min.is_some())?;
        }
        if let Some(work) = work_min {
            checked_minutes("work", work)?;
            let add = |ed: &mut Editor| {
                let uid = ed.add_assignment(task_uid, resource, units, None)?;
                ed.set_assignment(
                    uid,
                    AssignmentPatch {
                        work_min: Some(work),
                        ..AssignmentPatch::default()
                    },
                )?;
                Ok(uid)
            };
            return if self.batching {
                add(self)
            } else {
                self.batch(add)
            };
        }
        let i = self.index(task_uid)?;
        let mut resources = self.proj.resources.clone();
        let rid = match resource {
            ResourceRef::Uid(rid) => {
                if !resources.iter().any(|r| r.uid == rid) {
                    return Err(format!("no resource with uid {rid}"));
                }
                rid
            }
            ResourceRef::Name(name) => {
                let name = name.trim();
                if name.is_empty() {
                    return Err("the resource name is empty".into());
                }
                // A new name must survive the Resource Names cell, which
                // splits its text at commas; an existing one still matches.
                let known = resources.iter().any(|r| r.name.eq_ignore_ascii_case(name));
                if !known && name.contains(',') {
                    return Err(format!("Resource name '{name}' cannot contain a comma"));
                }
                // A number is a uid spelled as text; staging it as a new
                // resource would hide the caller's mistake.
                if !known && name.parse::<i64>().is_ok() {
                    return Err(format!(
                        "no resource named '{name}'; pass a resource uid as a number"
                    ));
                }
                find_or_stage_resource(&mut resources, name)?
            }
        };
        let r = resources.iter().find(|r| r.uid == rid).expect("staged");
        if self
            .proj
            .assignments
            .iter()
            .any(|a| a.task_uid == task_uid && a.resource_uid == rid)
        {
            return Err(format!(
                "'{}' is already assigned to task {task_uid}; use assign.set",
                r.name
            ));
        }
        let units = units.unwrap_or_else(|| default_units(r));
        let kind = Some(r.kind);
        // A blank row is assigned as the task the edit makes it.
        let duration = self.row_as_edited(i).duration_min;
        units_work(kind, duration, units)?;
        let mut next_aid = self
            .proj
            .assignments
            .iter()
            .map(|a| a.uid)
            .max()
            .unwrap_or(0);
        let mut assignments = self.proj.assignments.clone();
        let added = new_assignment(&mut next_aid, task_uid, rid, kind, units, "units", duration)?;
        let uid = added.uid;
        assignments.push(added);
        self.commit_assignments(i, resources, assignments)?;
        Ok(uid)
    }

    /// Change assignment `uid`'s units, work, overtime work, work contour,
    /// cost rate table, delay or actual work, as one undo step, then
    /// reschedule its task by its type (see `effort::same_resources`). The
    /// changes apply in this order: the delay, then the units (a work
    /// resource works them from its delay to the task finish), then the
    /// work, so given work is what it keeps; so on a Fixed Duration task,
    /// units given together with work are recomputed from the work. Then
    /// the overtime (a work edit clears it, so given overtime is checked
    /// against the new work), the contour, the rate table, and last the
    /// actual work, or the percent of the final work given instead. Actual
    /// work above zero sets the actual start to the assignment's start if it
    /// has none, the whole work also the actual finish to its finish, and
    /// zero clears both; actual overtime above the new actual is cut to it,
    /// and the timephased actual and remaining work and actual cost go. The
    /// refresh then derives the actual cost by the resource's accrual
    /// ([`crate::assign::actual_cost`]), the remaining work and cost and the
    /// % work complete. Overtime is not
    /// scheduled and a contour keeps the work, units and duration (the
    /// assignment spans its task), but a contour or an actual start or
    /// finish ends a delayed assignment's extension of its task
    /// (`assign::delayed_extent_min`). Values it already has are no change
    /// (given work, overtime and actuals only when no units or work edit
    /// restages them), and a patch that changes nothing records no undo
    /// step.
    /// A rejected edit leaves the editor untouched, except inside a caller's
    /// [`Self::batch`], where stages made before a rejection that needs
    /// their result stay until that batch fails.
    pub fn set_assignment(&mut self, uid: i32, patch: AssignmentPatch) -> Result<(), String> {
        let a = self
            .proj
            .assignments
            .iter()
            .find(|a| a.uid == uid)
            .ok_or_else(|| format!("no assignment with uid {uid}"))?;
        if patch.is_empty() {
            return Err("nothing to set: give units, work, overtime_work, contour, \
                        rate_table, delay, actual_work or percent_work_complete"
                .into());
        }
        let i = self.index(a.task_uid)?;
        let units = patch.units.map(positive_units).transpose()?;
        let work = patch
            .work_min
            .map(|w| checked_minutes("work", w))
            .transpose()?;
        let delay = patch
            .delay_min
            .map(|d| checked_minutes("delay", d))
            .transpose()?;
        if let Some(table) = patch.rate_table
            && table > 4
        {
            return Err(format!("no cost rate table {table}; use A to E"));
        }
        let overtime = patch
            .overtime_work_min
            .map(|o| checked_minutes("overtime work", o))
            .transpose()?;
        let contour = patch.work_contour.map(checked_contour).transpose()?;
        let actual = patch
            .actual_work_min
            .map(|w| checked_minutes("actual work", w))
            .transpose()?;
        let percent = patch.percent_work_complete;
        if let Some(p) = percent
            && p > 100
        {
            return Err(format!("percent_work_complete must be 0 to 100, not {p}"));
        }
        if actual.is_some() && percent.is_some() {
            return Err("give actual_work or percent_work_complete, not both".into());
        }
        let actuals = actual.is_some() || percent.is_some();
        let r = self.proj.resources.iter().find(|r| r.uid == a.resource_uid);
        if let Some(r) = r {
            check_kind(r, units.is_some(), work.is_some())?;
            check_progress_kind(r, overtime.is_some(), contour.is_some(), actuals)?;
        }
        let kind = r.map(|r| r.kind);
        // Only what differs from the assignment is an edit.
        let delay = delay.filter(|&d| d * 10 != a.delay.unwrap_or(0).max(0));
        // Exactly: an agent's units are not rounded to what the cell shows.
        let units = units.filter(|&u| u != a.units);
        // A units edit restages the work, so given work is then an edit.
        let work = work.filter(|&w| units.is_some() || w != a.work_min);
        let table = patch
            .rate_table
            .filter(|&t| t != a.cost_rate_table.unwrap_or(0));
        // A units or work edit restages the work, which clears the overtime
        // and moves what a percent asks for: given overtime and actuals are
        // then edits.
        let restaged = units.is_some() || work.is_some();
        if !restaged && percent.is_some_and(|p| p > 0) && a.work_min <= 0 {
            return Err(NO_WORK_PERCENT.into());
        }
        let overtime = overtime.filter(|&o| restaged || o != a.overtime_work_min.unwrap_or(0));
        let contour = contour.filter(|&c| c != a.work_contour.unwrap_or(0));
        let actuals = actuals
            && (restaged
                || asked_actual(actual, percent, a.work_min)
                    != Some(a.actual_work_min.unwrap_or(0)));
        if (delay, units, work, table, overtime, contour) == (None, None, None, None, None, None)
            && !actuals
        {
            return Ok(());
        }
        let edit = |ed: &mut Editor| {
            let stage = |ed: &mut Editor, change: &dyn Fn(&mut Assignment)| {
                let mut assignments = ed.proj.assignments.clone();
                let a = assignments
                    .iter_mut()
                    .find(|a| a.uid == uid)
                    .expect("checked");
                change(a);
                ed.commit_assignments(i, ed.proj.resources.clone(), assignments)
            };
            if let Some(d) = delay {
                stage(ed, &|a| a.delay = Some(d * 10))?;
            }
            if let Some(u) = units {
                // A work resource works them from its delay (as the delay
                // stage left it) to the task finish.
                let duration = ed.row_as_edited(i).duration_min;
                let a = ed.proj.assignments.iter().find(|a| a.uid == uid);
                let work = units_work(kind, duration - a.expect("checked").delay_min(), u)?;
                stage(ed, &|a| a.set_units(u, work))?;
            }
            if let Some(w) = work {
                // A fixed-duration task works new work over the span from
                // the delay to its finish, as the stages before left them,
                // which must be there. The batch undoes those stages.
                let task = ed.row_as_edited(i);
                let a = ed.proj.assignments.iter().find(|a| a.uid == uid);
                let a = a.expect("checked");
                if w > 0
                    && work_edit_rescales_units(&ed.proj, i, &task, a)
                    && task.duration_min - a.delay_min() <= 0
                {
                    return Err("delay must be shorter than the task".into());
                }
                stage(ed, &|a| a.set_work(w))?;
            }
            if let Some(o) = overtime {
                let a = ed.proj.assignments.iter().find(|a| a.uid == uid);
                if o > a.expect("checked").work_min {
                    return Err("overtime work must not be more than the work".into());
                }
                stage(ed, &|a| {
                    // What described the old overtime goes; the refresh
                    // prices the new one.
                    a.overtime_work_min = Some(o);
                    a.overtime_cost = None;
                    a.remaining_overtime_work_min = None;
                    a.remaining_overtime_cost = None;
                    a.timephased_data
                        .retain(|t| t.kind != TimephasedValue::REMAINING_WORK);
                })?;
            }
            if let Some(c) = contour {
                stage(ed, &|a| {
                    // The planned spread had the old shape, and a contour's
                    // units are its peak.
                    a.work_contour = Some(c);
                    a.peak_units = None;
                    a.timephased_data
                        .retain(|t| t.kind != TimephasedValue::REMAINING_WORK);
                })?;
            }
            if let Some(t) = table {
                stage(ed, &|a| a.cost_rate_table = Some(t))?;
            }
            if actuals {
                ed.stage_actuals(i, uid, actual, percent)?;
            }
            Ok(())
        };
        if self.batching {
            edit(self)
        } else {
            self.batch(edit)
        }
    }

    /// Record `actual` minutes of actual work on assignment `uid` (or
    /// `percent` of its work), with the actual dates and cost that follow:
    /// [`Self::set_assignment`]'s last stage, when its work and dates are
    /// final.
    fn stage_actuals(
        &mut self,
        i: usize,
        uid: i32,
        actual: Option<i64>,
        percent: Option<u8>,
    ) -> Result<(), String> {
        let a = self.proj.assignments.iter().find(|a| a.uid == uid);
        let a = a.expect("checked");
        let done = asked_actual(actual, percent, a.work_min).expect("asked");
        if done > a.work_min {
            return Err("actual work must not be more than the work; raise the work first".into());
        }
        if percent.is_some_and(|p| p > 0) && a.work_min <= 0 {
            return Err(NO_WORK_PERCENT.into());
        }
        let (start, finish) = if done > 0 {
            let (start, finish) = a
                .start
                .zip(a.finish)
                .or_else(|| crate::assign::assignment_span(&self.proj, self.schedule(), a))
                .ok_or_else(|| format!("task {} is not scheduled", a.task_uid))?;
            let start = a.actual_start.unwrap_or(start);
            let actual_finish = (done >= a.work_min).then(|| a.actual_finish.unwrap_or(finish));
            (Some(start), actual_finish)
        } else {
            (None, None)
        };
        // Actual overtime is part of the actual work.
        let overtime = a.actual_overtime_work_min.map(|o| o.min(done));
        let mut assignments = self.proj.assignments.clone();
        let a = assignments
            .iter_mut()
            .find(|a| a.uid == uid)
            .expect("checked");
        // The spread of the old actuals and of the work left described them;
        // the baselines stay.
        let stale = |kind: u8| {
            matches!(
                kind,
                TimephasedValue::REMAINING_WORK
                    | TimephasedValue::ACTUAL_WORK
                    | TimephasedValue::ACTUAL_COST
            ) || (kind == TimephasedValue::ACTUAL_OVERTIME_WORK
                && overtime != a.actual_overtime_work_min)
        };
        a.timephased_data.retain(|t| !stale(t.kind));
        a.actual_work_min = Some(done);
        a.actual_start = start;
        a.actual_finish = finish;
        a.actual_overtime_work_min = overtime;
        // The refresh prices the actual cost, and the actual overtime's.
        self.commit_assignments(i, self.proj.resources.clone(), assignments)
    }

    /// Remove assignment `uid` as one undo step and return its task's UID.
    /// An effort-driven task keeps its work across the assignments left, as
    /// in Project (see `effort::redistribute`).
    pub fn delete_assignment(&mut self, uid: i32) -> Result<i32, String> {
        let k = self
            .proj
            .assignments
            .iter()
            .position(|a| a.uid == uid)
            .ok_or_else(|| format!("no assignment with uid {uid}"))?;
        let task_uid = self.proj.assignments[k].task_uid;
        let i = self.index(task_uid)?;
        let mut assignments = self.proj.assignments.clone();
        assignments.remove(k);
        self.commit_assignments(i, self.proj.resources.clone(), assignments)?;
        Ok(task_uid)
    }
}

#[cfg(test)]
mod tests;
