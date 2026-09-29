//! One assignment at a time, by its UID (#395): what an agent's `assign.add`,
//! `assign.set` and `assign.del` do. Each edit stages the resources and
//! assignments and commits them through [`Editor::commit_assignments`], so
//! the task type and effort-driven rules, the horizon check, the undo step
//! and the refresh of cost, dates and totals are the ones the Resource Names
//! cell uses. Every argument is checked before anything is staged, so a
//! rejected edit leaves the editor untouched.
use super::effort::task_type;
use super::*;
use crate::assign::contoured;

/// The resource an added assignment is for.
#[derive(Clone, Copy, Debug)]
pub enum ResourceRef<'a> {
    /// An existing resource.
    Uid(i32),
    /// A resource by name, ignoring ASCII case; a new name is staged as a
    /// work resource, as the Resource Names cell does.
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

fn non_negative(what: &str, min: i64) -> Result<i64, String> {
    if min >= 0 {
        Ok(min)
    } else {
        Err(format!("{what} must not be negative"))
    }
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
        if let Some(work) = work_min {
            non_negative("work", work)?;
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
                find_or_stage_resource(&mut resources, name)?
            }
        };
        let r = resources.iter().find(|r| r.uid == rid).expect("staged");
        check_kind(r, units.is_some(), false)?;
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
        let mut next_aid = self
            .proj
            .assignments
            .iter()
            .map(|a| a.uid)
            .max()
            .unwrap_or(0);
        let mut assignments = self.proj.assignments.clone();
        let added = new_assignment(&mut next_aid, task_uid, rid, kind, units, duration)?;
        let uid = added.uid;
        assignments.push(added);
        self.commit_assignments(i, resources, assignments)?;
        Ok(uid)
    }

    /// Change assignment `uid`'s units, work, cost rate table or delay, as
    /// one undo step, then reschedule its task by its type (see
    /// `effort::same_resources`). The changes apply in that order: the delay,
    /// then the units (a work resource works them from its delay to the task
    /// finish), then the work, so given work is what it keeps, then the rate
    /// table. A work edit clears the overtime. Values it already has are no
    /// change, and a patch that changes nothing records no undo step.
    pub fn set_assignment(&mut self, uid: i32, patch: AssignmentPatch) -> Result<(), String> {
        let a = self
            .proj
            .assignments
            .iter()
            .find(|a| a.uid == uid)
            .ok_or_else(|| format!("no assignment with uid {uid}"))?;
        if patch.is_empty() {
            return Err("nothing to set: give units, work, rate_table or delay".into());
        }
        let i = self.index(a.task_uid)?;
        let units = patch.units.map(positive_units).transpose()?;
        let work = patch
            .work_min
            .map(|w| non_negative("work", w))
            .transpose()?;
        let delay = patch
            .delay_min
            .map(|d| non_negative("delay", d))
            .transpose()?;
        if let Some(table) = patch.rate_table
            && table > 4
        {
            return Err(format!("no cost rate table {table}; use A to E"));
        }
        let r = self.proj.resources.iter().find(|r| r.uid == a.resource_uid);
        if let Some(r) = r {
            check_kind(r, units.is_some(), work.is_some())?;
        }
        let kind = r.map(|r| r.kind);
        // Only what differs from the assignment is an edit.
        let delay = delay.filter(|&d| d * 10 != a.delay.unwrap_or(0).max(0));
        let units = units.filter(|&u| !same_shown_units(kind, u, a.units));
        let work = work.filter(|&w| w != a.work_min);
        let table = patch
            .rate_table
            .filter(|&t| t != a.cost_rate_table.unwrap_or(0));
        if (delay, units, work, table) == (None, None, None, None) {
            return Ok(());
        }
        // A fixed-duration task works new work over the span from the
        // delay to its finish, which must be there.
        let task = self.row_as_edited(i);
        let delay_after = delay.unwrap_or_else(|| a.delay_min());
        if work.is_some_and(|w| w > 0)
            && task_type(&task) == TaskType::FixedDuration
            && !self.proj.is_outline_summary(i)
            && task.duration_min > 0
            && kind.is_none_or(|k| k == ResourceType::Work)
            && !contoured(a)
            && task.duration_min - delay_after <= 0
        {
            return Err("delay must be shorter than the task".into());
        }
        let edit = |ed: &mut Editor| {
            let stage = |ed: &mut Editor, change: &dyn Fn(&mut Assignment, i64)| {
                let duration = ed.row_as_edited(i).duration_min;
                let mut assignments = ed.proj.assignments.clone();
                let a = assignments
                    .iter_mut()
                    .find(|a| a.uid == uid)
                    .expect("checked");
                change(a, duration);
                ed.commit_assignments(i, ed.proj.resources.clone(), assignments)
            };
            if let Some(d) = delay {
                stage(ed, &|a, _| a.delay = Some(d * 10))?;
            }
            if let Some(u) = units {
                stage(ed, &|a, duration| {
                    let work = match kind {
                        Some(ResourceType::Work) | None => {
                            work_for((duration - a.delay_min()).max(0), u)
                        }
                        _ => assigned_work(kind, duration, u),
                    };
                    a.set_units(u, work);
                })?;
            }
            if let Some(w) = work {
                stage(ed, &|a, _| a.set_work(w))?;
            }
            if let Some(t) = table {
                stage(ed, &|a, _| a.cost_rate_table = Some(t))?;
            }
            Ok(())
        };
        if self.batching {
            edit(self)
        } else {
            self.batch(edit)
        }
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
