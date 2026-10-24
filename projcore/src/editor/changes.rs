//! The cells the last change of the plan changed (#1667), for Project's
//! `Changed Cells` text style: every change (an edit, a batch step, undo,
//! redo) remembers the plan it started from, and [`Editor::changed_cells`]
//! compares display texts with it. A whole-project snapshot is what an undo
//! step is, so the difference is exactly what the step changed, the cells
//! the schedule recalculated (a successor's Start) included.
use super::*;
use std::collections::HashMap;

impl Editor {
    /// A read-only editor over `proj` with this editor's schedule and
    /// leveling overlay, as they stand before the change in flight: nothing
    /// is rescheduled.
    pub(super) fn frozen(&self, proj: Project) -> Box<Editor> {
        let mut ed = Editor::assembled(proj, self.sched.clone(), false);
        ed.leveled = self.leveled;
        ed.level = self.level.clone();
        Box::new(ed)
    }

    /// Settle the change just made: the state it started from becomes the
    /// record, unless only the formatting or the table view changed (a text
    /// style or a column width is no change of a cell, and the Text Styles
    /// OK that styles `Changed Cells` must not clear them).
    pub(super) fn settle_change(&mut self) {
        if let Some(from) = self.change_from.take() {
            self.record_change(from, None);
        }
    }

    /// Make `from` the record when the plan differs from it in more than
    /// the formatting and the table view, else keep `kept` (when given) or
    /// the record as it is.
    pub(super) fn record_change(
        &mut self,
        mut from: Box<Editor>,
        kept: Option<Option<Box<Editor>>>,
    ) {
        let formatting = std::mem::replace(&mut from.proj.formatting, self.proj.formatting.clone());
        let table_view = std::mem::replace(&mut from.proj.table_view, self.proj.table_view.clone());
        let view_only = from.proj == self.proj;
        from.proj.formatting = formatting;
        from.proj.table_view = table_view;
        if !view_only {
            self.last_change = Some(from);
        } else if let Some(kept) = kept {
            self.last_change = kept;
        }
    }

    /// Whether a change of the plan is remembered.
    pub fn has_last_change(&self) -> bool {
        self.last_change.is_some()
    }

    /// The cells the last change of the plan changed: for every task that
    /// is not blank both before it and now (by UID), each of `fields` whose
    /// display text differs, in task then `fields` order. A task the change
    /// made has none (it has no before; unmeasured against Project). Empty
    /// when no change is remembered (a new document, a replaced project).
    pub fn changed_cells(&self, fields: &[Field]) -> Vec<(i32, Field)> {
        let Some(before) = self.last_change.as_deref() else {
            return Vec::new();
        };
        let old_tasks: HashMap<i32, &Task> = before
            .proj
            .tasks
            .iter()
            .filter(|t| !t.is_null)
            .map(|t| (t.uid, t))
            .collect();
        let now = FieldReader::new(self);
        let was = FieldReader::new(before);
        let mut out = Vec::new();
        for task in self.proj.tasks.iter().filter(|t| !t.is_null) {
            let Some(old) = old_tasks.get(&task.uid) else {
                continue;
            };
            for &field in fields {
                if now.read(task, field).text != was.read(old, field).text {
                    out.push((task.uid, field));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
