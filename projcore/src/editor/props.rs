//! Single-field task properties the Task Information dialog sets (#1368):
//! % Complete, Priority, Deadline, Task type and Effort driven. Each is one
//! undo step, validates before it snapshots, and changes nothing (`Ok(false)`)
//! when the value is already the task's.
use super::*;

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
}

#[cfg(test)]
mod tests;
