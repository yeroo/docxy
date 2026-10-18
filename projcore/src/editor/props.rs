//! Single-field task properties the Task Information dialog sets (#1368):
//! % Complete, Priority, Deadline, Task type and Effort driven, and the
//! Advanced page's Calendar, Scheduling ignores resource calendars, WBS
//! code, Earned value method and Mark task as milestone (#1550). Each is one
//! undo step, validates before it snapshots, and changes nothing (`Ok(false)`)
//! when the value is already the task's.
use super::*;

/// Project's earned value methods (MSPDI `EarnedValueMethod`).
pub const EV_PERCENT_COMPLETE: i32 = 0;
pub const EV_PHYSICAL_PERCENT_COMPLETE: i32 = 1;

/// Levelling priority bounds, as Project's Priority field.
pub const PRIORITY_MAX: i32 = 1000;
/// The priority a task without one has (Project's default).
pub const PRIORITY_DEFAULT: i32 = 500;

impl Editor {
    /// The task's index, refusing a blank row, which holds no properties.
    fn property_row(&self, uid: i32) -> Result<usize, String> {
        let i = self.index(uid)?;
        if self.proj.tasks[i].is_null {
            return Err("A blank row has no task properties".into());
        }
        Ok(i)
    }

    /// Set the task's % Complete (0 to 100). A summary's rolls up from its
    /// subtasks, so it takes none.
    pub fn set_percent_complete(&mut self, uid: i32, percent: i32) -> Result<bool, String> {
        let i = self.property_row(uid)?;
        let percent = u8::try_from(percent)
            .ok()
            .filter(|p| *p <= 100)
            .ok_or("% Complete is 0 to 100")?;
        let task = &self.proj.tasks[i];
        if task.summary {
            return Err("Summary % Complete is computed from its subtasks".into());
        }
        // The task's actuals say how far it is: a lower value would contradict
        // them.
        if task.actual_finish.is_some() && percent < 100 {
            return Err("The task has an actual finish, so it is 100% complete".into());
        }
        if task.actual_start.is_some() && percent == 0 {
            return Err("The task has an actual start, so it is more than 0% complete".into());
        }
        if task.percent_complete.unwrap_or(0) == percent {
            return Ok(false);
        }
        self.snapshot();
        self.proj.tasks[i].percent_complete = Some(percent);
        self.changed();
        Ok(true)
    }

    /// Set the task's Priority (0 to 1000, 500 by default).
    pub fn set_priority(&mut self, uid: i32, priority: i32) -> Result<bool, String> {
        let i = self.property_row(uid)?;
        if !(0..=PRIORITY_MAX).contains(&priority) {
            return Err(format!("Priority is 0 to {PRIORITY_MAX}"));
        }
        if self.proj.tasks[i].priority.unwrap_or(PRIORITY_DEFAULT) == priority {
            return Ok(false);
        }
        self.snapshot();
        self.proj.tasks[i].priority = Some(priority);
        self.changed();
        Ok(true)
    }

    /// Set the task's Deadline; `None` clears it.
    pub fn set_deadline(&mut self, uid: i32, deadline: Option<DateTime>) -> Result<bool, String> {
        let i = self.property_row(uid)?;
        if self.proj.tasks[i].deadline == deadline {
            return Ok(false);
        }
        self.snapshot();
        self.proj.tasks[i].deadline = deadline;
        self.changed();
        Ok(true)
    }

    /// Set the task's type (what stays fixed when its units, work or
    /// duration change).
    pub fn set_task_type(&mut self, uid: i32, kind: TaskType) -> Result<bool, String> {
        let i = self.property_row(uid)?;
        let task = &self.proj.tasks[i];
        if task.task_type.unwrap_or(self.proj.default_task_type()) == kind {
            return Ok(false);
        }
        self.snapshot();
        self.proj.tasks[i].task_type = Some(kind);
        self.changed();
        Ok(true)
    }

    /// Set whether adding resources keeps the task's work constant.
    pub fn set_effort_driven(&mut self, uid: i32, on: bool) -> Result<bool, String> {
        let i = self.property_row(uid)?;
        let task = &self.proj.tasks[i];
        if task
            .effort_driven
            .unwrap_or(self.proj.new_tasks_effort_driven())
            == on
        {
            return Ok(false);
        }
        self.snapshot();
        self.proj.tasks[i].effort_driven = Some(on);
        self.changed();
        Ok(true)
    }

    /// Mark the task a milestone (a zero duration) or not (a day, as
    /// Project's Mark task as milestone unticked restores a duration).
    pub fn set_milestone(&mut self, uid: i32, on: bool) -> Result<bool, String> {
        let i = self.property_row(uid)?;
        let task = &self.proj.tasks[i];
        if task.summary {
            return Err("A summary task cannot be a milestone".into());
        }
        if task.is_milestone() == on {
            return Ok(false);
        }
        self.toggle_milestone(uid)?;
        Ok(true)
    }

    /// Set the task's calendar: a base calendar's uid, or `None` for the
    /// project's. A derived or unknown calendar is refused.
    pub fn set_task_calendar(&mut self, uid: i32, calendar: Option<i32>) -> Result<bool, String> {
        let i = self.property_row(uid)?;
        if let Some(cal) = calendar {
            match self.proj.calendar(cal) {
                None => return Err(format!("no calendar with uid {cal}")),
                Some(c) if c.base_calendar_uid.is_some() => {
                    return Err(format!("'{}' is not a base calendar", c.name));
                }
                Some(_) => {}
            }
        }
        if self.proj.tasks[i].calendar_uid == calendar {
            return Ok(false);
        }
        // A calendar without working time cannot be adopted (#1605: the
        // saved plan could not be read back).
        let mut next = self.proj.clone();
        next.tasks[i].calendar_uid = calendar;
        if let Some(error) = crate::schedule::calendar_error(&next) {
            return Err(error);
        }
        self.capture_view();
        let prev = std::mem::replace(&mut self.proj, next);
        self.push_undo(prev);
        self.changed();
        Ok(true)
    }

    /// Set whether the task's own calendar overrides its resources'
    /// calendars (Scheduling ignores resource calendars); it needs a task
    /// calendar.
    pub fn set_ignore_resource_calendar(&mut self, uid: i32, on: bool) -> Result<bool, String> {
        let i = self.property_row(uid)?;
        if on && self.proj.tasks[i].calendar_uid.is_none() {
            return Err("Scheduling ignores resource calendars needs a task calendar".into());
        }
        if self.proj.tasks[i].ignore_resource_calendar.unwrap_or(false) == on {
            return Ok(false);
        }
        self.snapshot();
        self.proj.tasks[i].ignore_resource_calendar = Some(on);
        self.changed();
        Ok(true)
    }

    /// Set the task's WBS code (its text; an empty code clears it).
    pub fn set_wbs(&mut self, uid: i32, wbs: &str) -> Result<bool, String> {
        let i = self.property_row(uid)?;
        let wbs = wbs.trim();
        let next = (!wbs.is_empty()).then(|| wbs.to_string());
        if self.proj.tasks[i].wbs == next {
            return Ok(false);
        }
        self.snapshot();
        self.proj.tasks[i].wbs = next;
        self.changed();
        Ok(true)
    }

    /// Set a custom field (`Text1`, `Number3`, `Flag2`, `Date1`, `Start1`,
    /// `Finish1`, `Duration1`, `Cost1`, ...) from its text, as typed in the
    /// field's cell (#1554): the field needs a task definition in the plan,
    /// a formula field is refused, a lookup-table field takes one of its
    /// values, an empty text clears the value. `Ok(false)` when the stored
    /// value is already what the text means. Outline codes are not custom
    /// fields here.
    pub fn set_custom_field(
        &mut self,
        uid: i32,
        field: super::fields::Field,
        text: &str,
    ) -> Result<bool, String> {
        let super::fields::Field::Custom(kind, n) = field else {
            return Err(format!("{} is not a custom field", field.name()));
        };
        let i = self.property_row(uid)?;
        let (id, next) =
            super::fields::custom_value_for(&self.proj, &self.proj.tasks[i], kind, n, text)?;
        let values = &self.proj.tasks[i].extended_attributes;
        let at = values.iter().position(|v| v.field_id.trim() == id);
        match (at, &next) {
            (None, None) => return Ok(false),
            (Some(at), Some(v)) if values[at] == *v => return Ok(false),
            _ => {}
        }
        self.snapshot();
        let values = &mut self.proj.tasks[i].extended_attributes;
        match (at, next) {
            (Some(at), None) => {
                values.remove(at);
            }
            (Some(at), Some(v)) => values[at] = v,
            (None, Some(v)) => values.push(v),
            (None, None) => unreachable!(),
        }
        self.changed();
        Ok(true)
    }

    /// Set the task's earned value method ([`EV_PERCENT_COMPLETE`] or
    /// [`EV_PHYSICAL_PERCENT_COMPLETE`]).
    pub fn set_earned_value_method(&mut self, uid: i32, method: i32) -> Result<bool, String> {
        let i = self.property_row(uid)?;
        if !matches!(method, EV_PERCENT_COMPLETE | EV_PHYSICAL_PERCENT_COMPLETE) {
            return Err("Earned value method is % Complete (0) or Physical % Complete (1)".into());
        }
        if self.proj.tasks[i]
            .earned_value_method
            .unwrap_or(EV_PERCENT_COMPLETE)
            == method
        {
            return Ok(false);
        }
        self.snapshot();
        self.proj.tasks[i].earned_value_method = Some(method);
        self.changed();
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
